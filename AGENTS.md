# AGENTS.md

Guidance for coding agents working in this repository.

## What this is

hark: a local audio asset browser for anyone who works from a library of sounds, game audio first but film, apps, theatre and podcasts alike. Fast audition of a folder-based library, favorites and user-defined tags, waveform selection with fades, preset-driven export into a project, and an origin record embedded in every exported file. Linux and Wayland first, Windows from the same code. Public repository, git, GPL-3.0-or-later.

The design note that started it lives in the user's Obsidian vault (`Audio Asset Browser - idea.md`); `README.md` is the current description for users.

## Shape

A Cargo workspace with one core and two thin frontends. The frontends never reimplement core logic.

- `crates/hark-core`: everything that is not UI. `decode` (symphonia, all formats), `peaks` (min/max envelope with a disk cache format), `loudness` (EBU R128 via ebur128, with a momentary fallback for clips shorter than the gating block), `resample` (rubato Fft), `wav` and `flac` (writers that embed the origin; readers for the chunks), `origin` (what an export carries), `export` (the pipeline and the path template; `preview_level`, the gain a normalising preset gives the whole file by the same rule, streamed), `copies` (a selection written through the pipeline with the export card's preset into the copy folder for a drag or a paste, so what lands is what an export would be; the folder's listing and clearing), `library` (SQLite: file rows keyed by path, marks keyed by content hash), `config` (the default state's folders and output, tags, presets, project files, the keys that differ from the defaults), `update` (the latest GitHub release against this build, asked only on request: hark's one network call), `keys` (the window's rebindable actions, their default chords and the chord text format; the window resolves chords against egui's key names, the CLI only carries them), `audition` (one cpal stream, a lock-free command channel, PipeWire native on Linux; a clip's source is either a whole decoded `Pcm` or a file streamed by a decoder thread through a lock-free SPSC ring; a clip can be folded to mono by `DownmixMode::fold`, the export's own downmix). `decode::Reader` decodes one packet at a time and seeks by frame; `peaks::PeaksBuilder` and `loudness::Meter` take audio in any chunking, so a long file is analysed in one pass without being held in memory.
- `crates/hark-cli`: clap commands over the core. Every command supports `--json`.
- `crates/hark`: eframe/egui window. `worker.rs` runs preloads, exports and scans on a pool, and on threads of their own where a newer request replaces a waiting one: the selected file's load, so it never queues behind preloads, and the whole-file passes (a streamed file's analysis, the preview's level and fold), which a newer request also aborts; `waveform.rs` is the selection and fade widget and draws the drag ghost; `dnd.rs` hands files to other programs, a drag out of the window (Wayland through a second client on winit's own connection, Windows through OLE, none on X11) and a file copy to the clipboard; `theme.rs` is the palette, fonts and egui styling; `main.rs` holds the app state, panels, settings window and keyboard map.
- Icons come from Phosphor through `theme::icon` and `theme::icon_text`, never through a plain string in a label: Inter maps its stylistic alternates into the private-use area and would render them as stray letters.

## Invariants

- **Originals are never modified.** The library reads and hashes source files; only exports are written.
- **Exports carry their origin inside the file.** WAV: a `hark` RIFF chunk with the JSON and a BWF `bext` description. FLAC: an APPLICATION block with id `hark` and a `HARK_ORIGIN` Vorbis comment. A sidecar is never the only copy.
- **Marks key on the content hash**, so a renamed or moved file keeps its tags and favorite once re-scanned: a scan ends by hashing unhashed files of the same size as a mark's missing file until every such mark has found it again, and does nothing when no mark is orphaned. The `hashes` table keeps each hash's size for that. A root that cannot be reached, such as an unplugged drive, fails its scan and keeps its rows; only a reachable folder's missing files are dropped. Tags are global; favorites belong to a project, and `Db` reads and writes them for the project set with `set_project`.
- **A project is the target**: a `.hark` file beside the game, edit or show holding an id, its library folders, and its output folder; favorites key on its id and its name is the file's stem. No project open is the default state: the config's own folders and output folder, favorites under `default`. The database is global and keyed by path, so projects that share a folder share its rows. Presets and tags are not per project; move them only when a real case demands it.
- **The audio callback allocates nothing and locks nothing.** Short clips arrive pre-decoded at the device rate through a channel; large files (decoded size over `stream_above_mb`) arrive as a lock-free ring that a decoder thread keeps a few seconds ahead at the device rate, and the callback plays counted silence if it ever runs dry. The callback never frees a stream either: the decoder thread outlives the callback's reference and releases the ring. The GUI reads the playhead and the underrun count from atomics.
- **hark never removes a copy.** The copy folder's files may be referenced in place by a DAW session, and the drag protocol does not say which program took a drop, so only the user clears the folder.
- **The CLI and the window are peers over the core.** A feature that exists only in the window is a bug; add the command. The one exception is a copy: it is a mouse gesture whose file is an export into the copy folder, which `hark-cli export --to` writes; the CLI keeps `copies` for the folder.

## Working here

- `cargo build` builds everything; `cargo check --target x86_64-pc-windows-gnu` checks the Windows build (mingw-w64 is installed on majestic-prime).
- Test audio is generated, not committed: see the sox and ffmpeg recipe in the session that built the first version, or make a few files with `sox -n -r 48000 -c 2 out.wav synth 0.4 sine 440 fade 0 0.4 0.3`.
- `hark-cli roots add <folder>` then `hark-cli export <file> --preset sfx --output-root /tmp/out` exercises the whole core; `hark --open <file>` opens the window on a file for a visual check.
- Config and database live under `~/.config/hark` and `~/.local/share/hark` on Linux; delete them to start fresh.
- `NOTES.md` at the root is git-ignored, a working copy's own file: follow-ups and observations with their reasons, between a passing thought and a ROADMAP.md entry. Read it when a task starts if it is there; add to it when work turns up something worth keeping that is not the task.
- `docs/screenshots/stage.sh all` synthesizes a sample library with sox, stages a project over it under `/tmp/hark-stage` with its own XDG folders, and captures the README screenshots through Hyprland and grim; `stage.sh social` composes the repository's social card, `docs/brand/social-preview.png`, from the running window. The capture floats and focuses windows on the user's desktop, so after a visible change, say the screenshots need retaking and run it only when the user gives the go-ahead.
