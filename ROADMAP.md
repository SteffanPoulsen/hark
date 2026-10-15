# Roadmap

Ideas that have been weighed and kept, with the case that would make each one worth building. None of these is scheduled.

- **Sort order for the list.** The list sorts by name, which suits a filtered view and interleaves folders in a recursive one. A sort control (name, folder, length, loudness) would earn its place once browsing a folder tree by name proves to be the wrong default in use.
- **List queries off the UI thread.** The folder list is one SQLite query on the window's thread; a root folder of 50,000 files takes about a fifth of a second to materialise, and a filtered view far less. The worker already opens its own database for scans, so a list job with a generation counter to drop stale results is the shape when a real library makes the root click hitch. That would also let the 2,000-row cap on text search go.
