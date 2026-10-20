# Changelog

What changed in each release of kuulo, newest first. A release's page on GitHub shows the same lines above its downloads, and kuulo shows them the first time it starts after an update. kuulo was called hark before 0.5.3.

## 0.6.0

- **kuulo updates itself.** Settings → About → Check for updates offers to install a newer release, and installing it restarts kuulo; `kuulo-cli update` does the same from a terminal. kuulo installs only a download that carries kuulo's release signature, and only where its own installer put it: `install.sh`, the Windows setup or the macOS app.
- **What's new after an update.** The first start of a new version shows what changed since the one you had, and Settings → About → What's new shows it again.
- **macOS gets a real app:** `kuulo.app` in a disk image, with its icon in the Dock and Finder. Drag it to Applications.
- **Windows gets an installer.** It installs kuulo for your account without asking for admin rights, adds it to the Start menu, puts `kuulo-cli` on your PATH and adds an uninstall entry. The zip stays, for running kuulo from any folder.
- **Linux:** kuulo's launcher entry starts it even when `~/.local/bin` is not on your session's PATH, `install.sh` says when a terminal will not find `kuulo-cli`, and `install.sh --uninstall` removes kuulo again.

## 0.5.4

- **A new logo and app icon**: kuulo's name drawn in one even stroke with its uu as a square wave, and an icon of that wave worn from copper to verdigris. The top bar shows it a little larger, still in your palette's colours.
- **Fixed:** the playhead stepped in jumps, ran ahead of what was heard (by about 180 ms over Bluetooth), and on a short clip showed at one spot or not at all. It now moves on every frame at the screen's refresh rate and follows the sound as it reaches the speaker.

## 0.5.3

- **hark is now kuulo**, Finnish for "hearing": the name hark was already used by several audio products. The programs are `kuulo` and `kuulo-cli`.
- **To keep your library, tags, favorites and presets**, rename the folders named `hark` in your config, data and cache locations to `kuulo` before the first start (on Linux `~/.config/hark`, `~/.local/share/hark` and `~/.cache/hark`); `kuulo-cli config --path` shows where the config now lives.
- **Project files are `.kuulo`**: rename a `.hark` file to keep its project.
- **Exports carry their origin under the id `kuul`**; files exported by hark no longer show where they came from.

## 0.5.2

- **The transport moves a group at a time.** Playback, the selection and the fades each drop to the next row whole when the wave card gets narrow, and the waveform gives up the height they take.
- **Fixed:** dragging the divider between the wave and export cards could make the bottom panel grow on its own.
- **Fixed:** the transport cut buttons off before it wrapped, and the export card could be dragged wide enough to cut them off for good.
- **Fixed:** Opus files in a library scanned with 0.5.0 were still listed and would not play; they now drop out at the next start, keeping their tags and favorites.

## 0.5.1

- **The filter's tags are check boxes** in their colours, the same control as the file menu's, so which tags are on reads at a glance. They fill columns as tall as the file type list and add columns as you add tags.
- **Fixed:** hark crashed as it quit on Wayland, after its window had closed.
- **Fixed:** dragging the end of a playing loop quickly made it crackle, and pulling the end back past the playhead clicked.
- **Fixed:** a tag, preset or library folder changed with `hark-cli` while the window was open came back when the window next saved.
- **Fixed:** Opus files, and any file whose codec hark cannot decode, were listed but would not play; they now stay out of the library. A damaged file is still listed in red with its error, and a file that failed is checked again on every scan.

## 0.5.0

- **Tabs in the file browser.** Keep a few standing views one key apart, each with its own folder or Favorites, search, filter, selected file and scroll. Ctrl+T opens a tab on the list as it is, Ctrl+W closes one, Ctrl+Tab and Ctrl+Shift+Tab step through them, and a folder's menu opens it in a new tab. A project reopens with the tabs it was left with.
- **Pick the channels of a file.** A strip beside the wave mutes or keeps each channel, L and R or one per channel; Alt-click hears one alone. What is kept is heard, drawn and exported, comes first in the export, and is recorded in the origin. `hark-cli export`, `play` and `takes` take `--channels 1,3`.
- **Reverse a file.** R, the button beside Preview or the wave's menu turns the file around, heard, drawn and exported backwards; the selection stays on the same sound and the fades sit where they are heard. `hark-cli export --reverse` and `play --reverse` do the same.
- **A wider filter.** Favorites, your tags as coloured chips, file types and channel counts side by side. Search and the filter sit above the list, as each tab's own.
- **Make a tag where you use it.** The file menu's "New tag…" row turns into a field: type a name and press Enter to create the tag and put it on the file.
- **Tag colours from twelve swatches.** A new tag takes one no tag uses yet; Settings → Tags changes it with a click, or to any colour with Custom. Tags that had no colour of their own take a swatch once and keep it. `hark-cli tags set <tag> --color indigo` takes a swatch name.
- **The wave card shows the file's whole state:** its favorite star beside the name, its tags in their colours, and its menu under "⋯".
- **Escape clears the selection and its fades.** Space stops playback, so the separate Stop key is gone.
- **The export card picks its channels from one list:** the source's, or mono by average or by a −3 dB sum. Presets that kept only the left or right channel now fold by average; pick the channel beside the wave instead. Exports made before keep re-exporting the same audio.
- **Opening hark is silent.** A file restored with a tab or opened with `hark --open` no longer plays until you play it.
- **The tag number keys 1–9 are gone.** Tags are tagged from the file menu.
- **Fixed:** Escape in the search box left the search in place.
- **Fixed:** dragging a row with the right mouse button reported "Drag: no button held".
- **Fixed:** tag chips in the list drew their names in white instead of their tag's colour.

## 0.4.3

- **Selections drag out on macOS.** Option-drag a selection out of the waveform, or copy it with Cmd+C, as Ctrl-drag and Ctrl+C work on Linux and Windows; the hints name the platform's keys.
- **A drag waits for its file.** A selection still being exported shows on the pointer as a grey, striped card while hark exports, and the drag starts with the finished file the moment it is written, so the program you drop on never waits on hark. Letting go before then, or a drop that lands nowhere, says so in the status line, and dragging again takes the file at once.
- **The wave shows an export under way.** The span dims with "Exporting" while a drag, copy or export of it is written, and flashes when the file is done.
- **Faster drags, copies and exports of a normalised selection.** They take the level Preview already measured instead of measuring the same audio again, which more than halves the wait on a long selection. Coming back to a selection or file reuses its level at once.
- **Preview plays a selection at exactly its file's level.** The measurement runs through the preset's own resampler, so a true-peak ceiling holds on the file as written.
- **Fixed:** on Windows, dragging a selection out froze the window for two minutes and then failed, unless the same copy had been made before.
- **Fixed:** a copy made while another of the same selection was still being written could take its name and replace it, so a DAW holding the first file got the second's audio.
- **Fixed:** Ctrl+C during a drag of the same selection wrote a second, identical copy.
- **Known on Hyprland:** a drag that starts while the pointer rests over the target drops nowhere unless the pointer moves first, and the grey card shows only over hark. Both are Hyprland's handling of a held pointer; hark reports the missed drop.

## 0.4.2

- **Preview follows the selection.** Under a normalising preset, a selection plays and draws at the level its export would get, fades included, while the file around it keeps its own level. A selection on a long file reads "Measuring level" until its pass is done.
- **Under Preview the waveform draws under its fades**, as the export sounds. With Preview off it shows the file as it is on disk.
- **An edit lands when you let go.** Dragging a selection edge, a fade handle or a number field changes nothing until release; the level and the picture then change once.
- **Fixed:** selecting another file did not stop the one playing, so the new file took two presses of Space, and moving quickly between files could crash hark.
- `hark-cli play --in --out --preset` plays at the level of that span rather than the whole file's.

## 0.4.1

- **hark opens the way you left it.** Auto-play and Loop are remembered, and each project remembers its export card: the preset, or your edited values as Custom, and Preview and Overwrite.
- **The waveform shows what it is waiting on.** A long file fills in as it is read, dimmed, with Loading, Analysing, Building preview or Measuring level written across it.
- **Instant level under Preview.** Changing the target or the ceiling on the export card applies at once, and a long file is read once for its preview instead of twice.
- **Phone recordings open.** Raw `.aac` files, such as Nothing Recorder's, are scanned and play.
- **Fixed:** a WAV, FLAC, Ogg or AAC file whose recording was cut short failed to open; it now plays up to the cut, and a damaged packet is skipped instead of failing the file.
- **Fixed:** returning to a long file while it was still being analysed started the analysis over.
- **Fixed:** `hark-cli roots remove`, `fav` and `keys set` printed text under `--json`, and `hark-cli presets list` printed a preset's level as internal debug text.
- **`hark-cli export` needs `--preset`**, so a script always says what it exports. `hark-cli config --autoplay` and `--loop` set the window's toggles.
- Long files analysed by an earlier version are read once more, the first time you open them, to record their sample peak.

## 0.4.0

- **Preview the export.** With Preview on (the ear on the transport, or P), playback and the waveform follow the export card: a mono preset is heard and drawn folded to one channel, so phase cancellation shows before you export, and a normalising preset plays the file at the level it would export at, with the fader showing that level. Off, the fader is your volume as before. `hark-cli play <file> --preset <name>` hears the same.
- **About.** A new settings page shows the version, links to the repository and its releases, the license, and checks GitHub for a newer release on request. `hark-cli version --check` does the same. It is hark's only network request, and it is only made when asked.
- **Faster loading.** The file you select loads on a thread of its own and never waits behind the neighbours being loaded ahead of it.
- **Fixed:** the Favorites count no longer includes favorites from folders removed from the project.
- **An empty folder panel** shows a centred placeholder that points at Add folder.

## 0.3.1

- **Output device.** The Audio output row in settings picks the sink; the system default is first and the fallback when the chosen one is absent at open. `hark-cli devices` lists them, `hark-cli config --output-device <id|default>` sets one.
- **A playing loop follows its edits.** Drag the edges or the fades while it plays: no restart, the playhead runs on and wraps at the new start. Files large enough to stream from disk restart from their position when the drag ends.
- **A device change keeps the selection and fades.**
- **Names.** Settings, CLI and config say output where they said audition; an older config's `audition_sample_rate` still loads. The footer hint reads Browse, the fader Volume.
- **The folder panel opens wider.** A remembered width stays until the Layout reset in settings.
- **PipeWire.** A stream pinned to a chosen sink no longer logs a disconnect when hark closes it.
- **Tests.** A WAV written by hark is read back through a strict reader, as FLAC already was.

## 0.3.0

A selection copies and drags out of the window. Ctrl-drag the waveform into Reaper, Pro Tools, a folder or a chat and the selection lands there, its waveform under the pointer on the way; Ctrl+C and the wave's menu put the same file on the clipboard for a paste, and a row dragged out of the list carries the original. Copies are written to hark's own folder, one file per recipe, and stay there until you clear it from settings, because Pro Tools and Reaper at factory settings reference a dropped file where it is; a project can send its copies to its session's audio folder instead. The pipeline decodes only the selection, so a clip from an hour-long recording is as quick as the clip. `hark-cli copies` lists and clears the folder. Dragging out needs Wayland on Linux; on X11, copy and paste.

The keys are yours to change. A Keys page in settings lists every action with its chord: click one, press the new key with any modifiers held, and the footer, the hover texts and the menus follow. Each row resets on its own and the page resets as a whole; a chord bound twice is shown on both rows. `hark-cli keys` lists and sets the same bindings. The list's arrows, the tag digits and Ctrl+C stay as they are.

The window gives more to the hand. Column headers sort the list and a second click reverses it; the name, folder and tag columns resize by the dividers in the header row, and the folder tree, the bottom panel and the export card by their separators, with a double-click putting one back and a row in settings resetting them all. The waveform has a time ruler, a drawn fader beside it that scales what you see with what you hear, and buttons for the takes. The export button answers by colour, green or red for a moment, while the exported span flashes on the wave; the footer keeps the words. The default window scale is 1.0.

FLAC exports from earlier versions could not be read back by hark, or by any reader that holds the stream to the letter of the format: the encoder wrote the last, shorter block's size as the stream's minimum block size, which marks a fixed-block stream as variable. ffmpeg and most players read them regardless; hark's own list, inspect and reexport did not. The stream info is now written as the format asks, and a FLAC of full-scale noise no longer comes out bloated or, at a full block, corrupt: a frame the encoder misjudges is redone without prediction, the rest of the file compresses as before. An earlier FLAC export is fixed by running it through `hark-cli reexport`, which reads the recipe from the file and writes it again from the source.

Smaller changes: the fade handles push each other in both directions, and the one set last keeps its length and takes the grab when they meet; zoom to selection stays inside the file instead of starting before zero; a drag out of the window no longer leaves the mouse button held in the window, so the next drag needs no click first; Backspace no longer clears the selection, Delete does and can be rebound.

## 0.2.0

A folder now lists everything under it. Pick a library root and the whole tree is one list; pick deeper to see less. A Folder column shows each file's subfolder, and in search and favorites views the folder under its root; when the column is narrow the hierarchy gives way before the filename, with the full path on hover. The list stays light at any size: fifty thousand files scroll at a few percent of one core.

Marks are safe across unplugged drives and moves. Opening hark with a library drive disconnected used to walk the root as empty and drop every file under it, and the tags and favorites looked gone until each file was marked again. A root that cannot be reached now fails its scan, keeps its files as last seen, and is named in the footer. After every scan, marks whose file is unknown are matched back to moved or renamed files by size and content, so a move costs reading the moved file and nothing else, and a library without moves reads nothing. Marks lost to the old behaviour come back on the first scan with this version.

Smaller changes: folder queries seek the path index instead of scanning it, which also fixes `_` and `%` in a folder name acting as wildcards; the footer counts files while a scan runs and hashes while marks are matched; `hark-cli scan --json` now returns an object with `roots`, `failed` and `hashed` instead of a bare list of root reports.

## 0.1.0

The first release of hark, free software under the GPL: a local audio asset browser for anyone who works from a library of sounds, in games, film, apps, theatre or podcasts. audition a library at keyboard speed, mark favorites and tags, select a region of the waveform with fades or trim it to the sound, and export it through a preset into a project with the origin embedded in the file. The command line, `hark-cli`, exposes the same core with `--json` on every command.
