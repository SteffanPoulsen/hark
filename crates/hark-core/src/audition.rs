//! One always-open output stream. Clips are handed over already at the device rate and start
//! on the next callback, so retriggering a hundred one-shots in a row costs nothing.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

use crate::pcm::Pcm;

#[derive(Clone)]
pub struct Clip {
    pub pcm: Arc<Pcm>,
    pub start: usize,
    pub end: usize,
    pub looped: bool,
}

enum Cmd {
    Play(Clip),
    Stop,
    Seek(usize),
}

struct Shared {
    position: AtomicUsize,
    playing: AtomicBool,
    gain: AtomicU32,
}

pub struct Player {
    _stream: cpal::Stream,
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    pub sample_rate: u32,
    pub channels: u16,
    pub device_name: String,
}

struct Voice {
    clip: Clip,
    pos: usize,
}

struct Mixer {
    rx: Receiver<Cmd>,
    shared: Arc<Shared>,
    voice: Option<Voice>,
    channels: usize,
}

impl Mixer {
    fn fill(&mut self, out: &mut [f32]) {
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                Cmd::Play(clip) => {
                    let pos = clip.start;
                    self.voice = Some(Voice { clip, pos });
                    self.shared.playing.store(true, Ordering::Relaxed);
                }
                Cmd::Stop => {
                    self.voice = None;
                    self.shared.playing.store(false, Ordering::Relaxed);
                }
                Cmd::Seek(frame) => {
                    if let Some(v) = &mut self.voice {
                        v.pos = frame.clamp(v.clip.start, v.clip.end);
                    }
                }
            }
        }
        out.fill(0.0);
        let gain = f32::from_bits(self.shared.gain.load(Ordering::Relaxed));
        let oc = self.channels;
        let Some(v) = &mut self.voice else { return };
        let ic = v.clip.pcm.channels as usize;
        let samples = &v.clip.pcm.samples;
        let mut done = false;
        for frame in out.chunks_mut(oc) {
            if v.pos >= v.clip.end {
                if v.clip.looped && v.clip.end > v.clip.start {
                    v.pos = v.clip.start;
                } else {
                    done = true;
                    break;
                }
            }
            let src = &samples[v.pos * ic..(v.pos + 1) * ic];
            match ic {
                1 => frame.iter_mut().for_each(|o| *o = src[0] * gain),
                _ => {
                    for (i, o) in frame.iter_mut().enumerate() {
                        *o = src[i.min(ic - 1)] * gain;
                    }
                }
            }
            v.pos += 1;
        }
        self.shared.position.store(v.pos, Ordering::Relaxed);
        if done {
            self.voice = None;
            self.shared.playing.store(false, Ordering::Relaxed);
        }
    }
}

impl Player {
    pub fn open(preferred_rate: Option<u32>) -> Result<Player> {
        let (device, host_name) = pick_device()?;
        let device_name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "output".into());
        let default = device.default_output_config()?;
        let mut config: cpal::StreamConfig = default.clone().into();
        if let Some(rate) = preferred_rate {
            if device.supported_output_configs().map(|mut c| c.any(|r| r.min_sample_rate() <= rate && rate <= r.max_sample_rate())).unwrap_or(false) {
                config.sample_rate = rate;
            }
        }
        let shared = Arc::new(Shared {
            position: AtomicUsize::new(0),
            playing: AtomicBool::new(false),
            gain: AtomicU32::new(1.0f32.to_bits()),
        });
        let (tx, rx) = channel();
        let mixer = Mixer { rx, shared: shared.clone(), voice: None, channels: config.channels as usize };
        let stream = match default.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, mixer)?,
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, mixer)?,
            cpal::SampleFormat::I32 => build::<i32>(&device, &config, mixer)?,
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, mixer)?,
            f => return Err(anyhow!("unsupported output sample format {f}")),
        };
        stream.play()?;
        Ok(Player {
            _stream: stream,
            tx,
            shared,
            sample_rate: config.sample_rate,
            channels: config.channels,
            device_name: format!("{device_name} ({host_name})"),
        })
    }

    pub fn play(&self, clip: Clip) {
        let _ = self.tx.send(Cmd::Play(clip));
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }

    pub fn seek(&self, frame: usize) {
        let _ = self.tx.send(Cmd::Seek(frame));
    }

    pub fn set_gain(&self, gain: f32) {
        self.shared.gain.store(gain.max(0.0).to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.shared.gain.load(Ordering::Relaxed))
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Frame of the current clip the callback last reached.
    pub fn position(&self) -> usize {
        self.shared.position.load(Ordering::Relaxed)
    }
}

fn build<T>(device: &cpal::Device, config: &cpal::StreamConfig, mut mixer: Mixer) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let mut scratch: Vec<f32> = Vec::new();
    let stream = device.build_output_stream(
        config.clone(),
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            scratch.resize(data.len(), 0.0);
            mixer.fill(&mut scratch);
            for (o, s) in data.iter_mut().zip(&scratch) {
                *o = T::from_sample(*s);
            }
        },
        |err| eprintln!("audio stream error: {err}"),
        None,
    )?;
    Ok(stream)
}

fn pick_device() -> Result<(cpal::Device, &'static str)> {
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    {
        if let Ok(host) = cpal::host_from_id(cpal::HostId::PipeWire) {
            if let Some(d) = host.default_output_device() {
                return Ok((d, "pipewire"));
            }
        }
    }
    let host = cpal::default_host();
    #[cfg(target_os = "linux")]
    {
        let named = |want: &str| {
            host.output_devices().ok().and_then(|mut it| it.find(|d| d.description().map(|desc| desc.name() == want).unwrap_or(false)))
        };
        if let Some(d) = named("pipewire").or_else(|| named("pulse")) {
            return Ok((d, "alsa"));
        }
    }
    host.default_output_device().map(|d| (d, "default")).ok_or_else(|| anyhow!("no audio output device"))
}
