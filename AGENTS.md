# AGENTS.md

Guidance for coding agents working in this repository.

## What this is

hark: a local audio asset browser for game development. Fast audition of a folder-based library, favorites and user-defined tags, waveform selection with fades, preset-driven export into a project, and an origin record embedded in every exported file. Linux and Wayland first, Windows from the same code. Public repository, git, MIT.

The design note that started it lives in the user's Obsidian vault (`Audio Asset Browser - idea.md`); `README.md` is the current description for users.

## Shape

A Cargo workspace with one core and two thin frontends. The frontends never reimplement core logic.

- `crates/hark-core`: everything that is not UI. `decode` (symphonia, all formats), `peaks` (min/max envelope with a disk cache format), `loudness` (EBU R128 via ebur128, with a momentary fallback for clips shorter than the gating block), `resample` (rubato Fft), `wav` and `flac` (writers that embed the origin; readers for the chunks), `origin` (what an export carries), `export` (the pipeline and the path template), `library` (SQLite: file rows keyed by path, marks keyed by content hash), `config` (roots, tags, presets, output root), `audition` (one cpal stream, a lock-free command channel, PipeWire native on Linux; a clip's source is either a whole decoded `Pcm` or a file streamed by a decoder thread through a lock-free SPSC ring). `decode::Reader` decodes one packet at a time and seeks by frame; `peaks::PeaksBuilder` and `loudness::Meter` take audio in any chunking, so a long file is analysed in one pass without being held in memory.
- `crates/hark-cli`: clap commands over the core. Every command supports `--json`.
- `crates/hark`: eframe/egui window. `worker.rs` runs decode, export and scan jobs on background threads; `waveform.rs` is the selection and fade widget; `theme.rs` is the palette, fonts and egui styling; `main.rs` holds the app state, panels, settings window and keyboard map.
- Icons come from Phosphor through `theme::icon` and `theme::icon_text`, never through a plain string in a label: Inter maps its stylistic alternates into the private-use area and would render them as stray letters.

## Invariants

- **Originals are never modified.** The library reads and hashes source files; only exports are written.
- **Exports carry their origin inside the file.** WAV: a `hark` RIFF chunk with the JSON and a BWF `bext` description. FLAC: an APPLICATION block with id `hark` and a `HARK_ORIGIN` Vorbis comment. A sidecar is never the only copy.
- **Marks key on the content hash**, so a renamed or moved file keeps its tags and favorite once re-scanned and re-hashed. Tags are global; favorites belong to a project, and `Db` reads and writes them for the project set with `set_project`.
- **A project is the target**: a `.hark` file beside the game holding an id, output folder and library scope; favorites key on its id. No project open is the default state: whole library, the config's output folder, favorites under `default`. Presets and tags are not per project; move them only when a real case demands it.
- **The audio callback allocates nothing and locks nothing.** Short clips arrive pre-decoded at the device rate through a channel; long files (over `stream_above_secs`) arrive as a lock-free ring that a decoder thread keeps a few seconds ahead at the device rate, and the callback plays counted silence if it ever runs dry. The callback never frees a stream either: the decoder thread outlives the callback's reference and releases the ring. The GUI reads the playhead and the underrun count from atomics.
- **The CLI and the window are peers over the core.** A feature that exists only in the window is a bug; add the command.

## Working here

- `cargo build` builds everything; `cargo check --target x86_64-pc-windows-gnu` checks the Windows build (mingw-w64 is installed on majestic-prime).
- Test audio is generated, not committed: see the sox and ffmpeg recipe in the session that built the first version, or make a few files with `sox -n -r 48000 -c 2 out.wav synth 0.4 sine 440 fade 0 0.4 0.3`.
- `hark-cli roots add <folder>` then `hark-cli export <file> --preset sfx --output-root /tmp/out` exercises the whole core; `hark --open <file>` opens the window on a file for a visual check.
- Config and database live under `~/.config/hark` and `~/.local/share/hark` on Linux; delete them to start fresh.
