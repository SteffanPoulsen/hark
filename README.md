# hark

Listen fast, export clean. A local, folder-based audio asset browser for game development: audition a library at keyboard speed, mark favorites and tags, select a region of the waveform with fades, and export it through a preset into your project with a consistent folder structure. Every exported file carries where it came from and how it was made, embedded in the file itself.

Linux (Wayland) first, Windows supported from the same codebase. No accounts, no subscriptions, no cloud.

## What it does

- **Browse** the library folders of the open project in a tree. The list shows duration, channels, sample rate, bit depth, codec and loudness for every file, and search covers the whole library. A filter narrows any view by favorites, tags, file type and channels, and a Favorites row above the tree lists the project's stars.
- **Audition** at speed: arrowing through the list plays each file instantly. One always-open audio stream, pre-decoded neighbours, cached waveform peaks. Loop, seek by clicking the waveform, a gain fader beside the waveform that scales what you see and hear and carries into the export unless the preset normalises. Large files stream from disk while they play instead of being decoded whole, so an hour-long recording starts as fast as a one-shot; the threshold is the decoded size, `stream_above_mb` in the config (256 MB by default, about ten minutes of 48 kHz stereo, also in the settings window).
- **Mark** favorites and tags from the keyboard or a right-click menu on any file, which also shows the file in its folder or copies its path. Tags are yours to define, each with a color, edited in the settings window. Marks follow a file through renames because they key on content, not path. The library itself is read-only: hark never renames, moves or deletes a source file.
- **Projects** are `.hark` files you keep beside the game: the library folders this game draws from, an output folder (stored relative to the file), and favorites of their own. A new project starts empty; add folders with the button under the tree and take one out with a right-click on it. With no project open, hark has a default set of folders and a default output folder of its own. Switch, open or create projects from the top bar; the name is the file's name; tags and presets stay global.
- **Select** a region on the waveform, drag fade handles for fade in and out, zoom with the wheel.
- **Export** through a preset: WAV or FLAC, bit depth with dither, resampling, mono downmix, trim silence, peak or EBU R128 loudness normalisation with a true-peak ceiling, and a file name template such as `{name}` or `{relpath}/{name}`. The card starts from a preset and lets you change format, rate, channels and level for this export (the dropdown then reads "Custom", with a way to keep the values as a new preset). The file goes into a folder you pick under the output folder, from a list of the folders that exist there or the native dialog to create one; the pick is remembered per project. Tags never decide where a file goes. Presets are edited on their own settings page.
- **Remember**: every export embeds its origin, in a `hark` RIFF chunk plus a BWF `bext` description for WAV and an APPLICATION block plus Vorbis comment for FLAC. Months later, `hark-cli inspect` on the file tells you the source path, the pack, the selection, the fades, the preset and the measured gain. `hark-cli reexport` runs that recipe again with changes.
- **Script it**: `hark-cli` exposes the same core with no window, with `--json` on everything, so an agent or a build step can search, mark, analyse and export.

## Build

```
cargo build --release
```

Binaries land in `target/release/hark` (the window) and `target/release/hark-cli`. `packaging/install-desktop.sh` installs a desktop entry and the icon for the current user, so launchers and the compositor show hark by its app id. Rust stable; on Linux the PipeWire headers (`pipewire` package) are needed for the native backend, with ALSA as the fallback. Windows builds with the MSVC or GNU toolchain and uses WASAPI.

## Use

Start `hark`, add a library folder with the button under the tree, pick a folder, and arrow through the files. `hark --open <file>` starts on a file; `hark --project <file.hark>` opens a project; `hark --settings [general|tags|presets]` opens the settings window, a window of its own with those three pages. A project has no window of its own: its folders are the tree, its output folder is on the export card, and its name is its file's.

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

## Configuration

`~/.config/hark/config.json` (or the platform equivalent) holds the default state's folders and output folder, the known project files and the open one, the tags, the export presets, the audition sample rate and the streaming threshold (`stream_above_mb`). The defaults define `clean`, `sfx`, `ambient`, `music` and `voice` presets and a matching set of tags. Edit the file or use `hark-cli config`. A project file is JSON too: an id (favorites key on it), its folders, the output folder and the last export folder; `hark-cli --project <file or name>` acts in one without opening it.

The library database lives in the data directory and only indexes what the files already carry; deleting it loses nothing but the marks, which `hark-cli` can re-apply. Waveform peaks are cached in the cache directory.

## Layout

- `crates/hark-core`: decoding (symphonia), peaks, loudness (ebur128), resampling (rubato), WAV and FLAC writing with embedded origin, the export pipeline, the library database (SQLite), config, and the audition engine (cpal).
- `crates/hark-cli`: the command line, a thin layer over the core.
- `crates/hark`: the window (egui), a thin layer over the core.

## Licence

MIT.
