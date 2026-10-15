# hark

**Listen fast, export clean.** A local audio asset browser for game development.

![hark auditioning a sword take, one hit selected with fades, the export card set to the sfx preset](docs/screenshots/hero.png)

hark sits between your sound libraries and your game project. Point it at the folders the libraries live in, arrow through them at keyboard speed, star and tag what works, select the part you want on the waveform, and export it through a preset into the project's audio folder with a consistent name, format and loudness. Every exported file carries where it came from and how it was made, embedded in the file itself, so months later `hark-cli inspect` still knows.

Linux (Wayland) first, Windows from the same code. No accounts, no subscriptions, no cloud. MIT.

## Why

Auditioning a big library in a DAW means importing first, and in a file manager means waiting for a player to open; neither remembers what you liked. Exporting by hand gives a different level and format every time, and leaves a file in the project with no link back to the pack it came from. hark does the whole loop in one window, at the speed of the arrow keys, and writes the link into the file.

## Features

- **Instant audition.** Arrowing through the list plays each file as it lands: one always-open audio stream, pre-decoded neighbours, cached waveform peaks. Hour-long recordings stream from disk and start as fast as a one-shot.
- **Library, not import.** Folders stay where they are and are never modified. The list shows duration, channels, rate, bit depth, codec and loudness; search covers everything, and a filter narrows by favorites, tags, type and channels.
- **Favorites and tags** from the keyboard. Tags are yours to define, with a colour each. Marks key on the file's content, so a renamed or moved file keeps them.
- **Projects.** A `.hark` file beside the game holds its library folders, its output folder and its own favorites. Switch between games from the top bar.
- **Waveform selection with fades.** Drag a region, set fade in and out, zoom, loop, seek by clicking, and scale the level with the fader beside the waveform.
- **Preset-driven export.** WAV or FLAC, bit depth with dither, resampling, mono downmix, silence trim, peak or EBU R128 loudness normalisation under a true-peak ceiling, and a file name template. Override any of it for one export.
- **An origin inside every export.** A `hark` RIFF chunk and a BWF `bext` description in WAV, an APPLICATION block and a Vorbis comment in FLAC: source path and hash, pack, selection, fades, preset and measured gain. `hark-cli reexport` runs that recipe again with changes.
- **A CLI for all of it.** `hark-cli` exposes the same core with `--json` on every command, so a script or an agent can search, mark, analyse and export without a window.

![The presets page of the settings window](docs/screenshots/presets.png)

## Install

Downloads for Linux and Windows are on the [Releases](../../releases) page. To build from source:

```
cargo build --release
```

Binaries land in `target/release/hark` (the window) and `target/release/hark-cli`. `packaging/install.sh` installs the desktop entry and the icon for the current user, so launchers and the compositor show hark by its app id; from a release archive it also puts the binaries in `~/.local/bin`. Rust stable; on Linux the PipeWire headers (`pipewire` package) are needed for the native backend, with ALSA as the fallback. Windows builds with the MSVC or GNU toolchain and uses WASAPI.

## Use

Start `hark`, add a library folder with the button under the tree, pick a folder, and arrow through the files. `hark --open <file>` starts on a file, with `--in` and `--out` seconds for a selection and `--fade-in` and `--fade-out` for its fades; `hark --project <file.hark>` opens a project; `hark --settings [general|tags|presets]` opens the settings window, a window of its own with those three pages. A project has no window of its own: its folders are the tree, its output folder is on the export card, and its name is its file's.

| Key | Action |
|---|---|
| Up / Down, PageUp / PageDown, Home / End | Move through the list, playing each file when autoplay is on |
| Space | Play the selection or the whole file; stop when playing |
| Enter | Play from the start |
| Escape | Stop |
| F | Toggle favorite |
| 1 to 9 | Toggle the first nine tags |
| L | Loop |
| A | Autoplay |
| `[` and `]` | Set the selection start or end at the playhead |
| Z | Zoom to the selection |
| Delete | Clear the selection and fades |
| E | Export the selection with the chosen preset |

On the waveform: drag to select, drag a selection edge to adjust it, shift-drag inside to move it, drag the triangles in the strip above to set fades, click to play from there, wheel to zoom, shift-wheel or middle-drag to pan, double-click to zoom out. The window scale is in settings; Ctrl+= and Ctrl+- change it too.

### CLI

```
hark-cli roots add ~/Audio/Libraries --name libraries
hark-cli scan
hark-cli search footstep gravel --tag sfx --tag foley --type wav --channels mono
hark-cli tag "Footsteps/gravel_03.wav" +sfx +foley
hark-cli fav "Footsteps/gravel_03.wav"
hark-cli analyze "Footsteps/gravel_03.wav" --store
hark-cli projects new ~/Projects/game/MyGame.hark --output ~/Projects/game/Assets/Audio
hark-cli roots add ~/Audio/Libraries/Footsteps
hark-cli tags add footstep --color '#79b86a'
hark-cli presets add sfx-quiet --from sfx
hark-cli presets set sfx-quiet --normalize lufs --target=-20 --ceiling=-1 --rate 48000
hark-cli export "Footsteps/gravel_03.wav" --preset sfx --into sfx/footsteps --in 0.2 --out 1.4 --fade-out 0.1
hark-cli inspect ~/Projects/game/Assets/Audio/sfx/gravel_03.wav
hark-cli reexport ~/Projects/game/Assets/Audio/sfx/*.wav --preset sfx
```

`hark-cli --help` lists everything; `--json` makes any command machine-readable.

### What an export carries

```
$ hark-cli inspect Assets/Audio/sfx/combat/impact_metal_heavy_02.wav
  wave pcm_s24le · 48000 Hz · 1 ch · 24 bit · 1.600s
  bext: hark: Hollowmere SFX:Impacts/impact_metal_heavy_02.wav
  origin: hark: Hollowmere SFX:Impacts/impact_metal_heavy_02.wav
    exported 2026-10-15T20:08:46+02:00 by hark 0.1.0 with preset sfx
    source ~/Audio/Hollowmere/Impacts/impact_metal_heavy_02.wav (230480 bytes, blake3 a1e93ed24193076e)
    fades 0.000s / 0.000s · gain -1.33 dB (true-peak limited)
```

The same record is in the file as JSON, readable by anything that walks RIFF chunks or FLAC metadata blocks. A sidecar is never the only copy.

## Configuration

`~/.config/hark/config.json` (or the platform equivalent) holds the default state's folders and output folder, the known project files and the open one, the tags, the export presets, the default preset, the window zoom, the audition sample rate and the streaming threshold (`stream_above_mb`, 256 MB of decoded audio by default, about ten minutes of 48 kHz stereo). The defaults define `clean`, `sfx`, `ambient`, `music` and `voice` presets and a matching set of tags. Edit the file, use the settings window, or `hark-cli config`. A project file is JSON too: an id (favorites key on it), its folders, the output folder and the last export folder; `hark-cli --project <file or name>` acts in one without opening it.

The library database lives in the data directory and only indexes what the files already carry; deleting it loses nothing but the marks, which `hark-cli` can re-apply. Waveform peaks are cached in the cache directory.

## Layout

- `crates/hark-core`: decoding (symphonia), peaks, loudness (ebur128), resampling (rubato), WAV and FLAC writing with embedded origin, the export pipeline, the library database (SQLite), config, and the audition engine (cpal).
- `crates/hark-cli`: the command line, a thin layer over the core.
- `crates/hark`: the window (egui), a thin layer over the core.
- `docs/screenshots/stage.sh`: synthesizes a sample library, stages a project over it and captures the screenshots above, so they can be retaken after a change.

## Licence

MIT.
