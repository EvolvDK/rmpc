# rmpc (Rusty Music Player Client) Overview

This document provides a comprehensive overview of the functionality and architecture of `rmpc`, a modern, configurable MPD client built with Rust.

## 1. Features

`rmpc` is designed to be a feature-rich, high-performance TUI client for MPD.

- **MPD Integration:** Full support for the Music Player Daemon protocol, including playback control, queue management, library browsing, and partition support.
- **TUI Interface:** Built using `ratatui`, providing a responsive and customizable terminal user interface with mouse support and high-performance rendering.
- **Album Art Rendering:** Integrated album art support using various protocols (Kitty, Sixel, iTerm2, and UeberzugPP) to display covers directly in the terminal.
- **YouTube Playback Support:** Seamlessly search and play music from YouTube. It uses `yt-dlp` to fetch stream URLs and maintains a local metadata cache. It also supports importing libraries and playlists from YouTube Takeout CSV files.
- **Proactive Stream Refreshing:** Automatically detects expired YouTube URLs (e.g., HTTP 403) and refreshes them on the fly during playback to ensure a smooth experience.
- **Configuration System (RON):** Extensive configuration via Rusty Object Notation (RON) files, allowing for deep customization of keybinds, themes, and layouts.
- **Hot-Reloading:** Supports live reloading of configuration and themes without restarting the application.
- **Lyrics Integration:** Built-in lyrics viewer with support for local `.lrc` or `.txt` files and background indexing for fast retrieval.
- **Visualizer Integration:** Support for `cava` (Console-based Audio Visualizer) rendered directly within the TUI.
- **Data Persistence:** Uses a local SQLite database (`DataStore`) for caching YouTube metadata, managing local playlists, and synchronizing queue state.

## 2. Pane Disposition

The UI is divided into modular panes that can be arranged in a flexible layout defined in the configuration. Users can organize these panes into multiple tabs.

### Major UI Panes

- **Queue:** Displays the current MPD playback queue. Shows song titles, artists, durations, and status (playing/paused).
- **Browser Panes:**
    - **Directories:** Browse the music library through the filesystem structure.
    - **Tag Browsers:** Generic panes used to navigate the library using MPD metadata tags. This includes the default **Artists** and **Album Artists** views, but can be configured for any tag (e.g., Genre, Composer).
    - **Albums:** A specialized browser for viewing and searching albums.
- **Playlists Panes:**
    - **Playlists (MPD):** Manage standard MPD playlists.
    - **Rmpc Playlists:** Manage local playlists stored in the `rmpc` database, which can include both local files and YouTube tracks.
- **YouTube:** Provides a search interface for YouTube and a view of the local YouTube library.
- **Lyrics:** Displays the lyrics of the currently playing song.
- **Album Art:** Renders the album cover of the current track.
- **Cava:** Displays an audio visualizer (requires `cava` to be installed and configured).
- **Informational Panes:**
    - **Header:** Shows current song info and playback status at the top.
    - **Progress Bar:** A dedicated pane for the playback progress and time.
    - **Volume:** Displays and controls the current volume level.
    - **Tabs:** Navigation bar for switching between different UI tabs.
    - **Logs:** (Debug mode) Real-time view of application logs.

## 3. Pane Actions

Actions in `rmpc` are divided into global actions and pane-specific actions.

### Global Actions (Available Anywhere)
- **Navigation:** Switching between tabs (`NextTab`, `PreviousTab`, `SwitchToTab`).
- **Playback Control:** Play/Pause, Stop, Next/Previous Track, Toggle Random/Repeat/Consume/Single modes.
- **Volume:** Increase or decrease volume.
- **Seeking:** Jump forward or backward in the current track.
- **System:** Quit, Show Help (Keybinds), Enter Command Mode, Update/Rescan MPD database.
- **Modals:** Open MPD Outputs or Decoders management, Show Current Song Info.

### Common Pane Actions (Standardized Navigation)
- **Movement:** Up, Down, Left, Right (standard or half-page/full-page jumps).
- **Focus:** Move focus between panes (`PaneUp`, `PaneDown`, etc.).
- **Selection:** Select individual items or invert selection for batch operations.
- **Search:** Enter search/filter mode within a pane.
- **Confirmation:** `Confirm` (usually Enter) to play a song or enter a directory.

### Pane-Specific Actions
- **Queue:** Delete items, clear queue, save queue as a playlist, shuffle queue, jump to the currently playing song.
- **Browser:** Add selected items to the queue (at the end, next, or replace), add to a playlist.
- **Playlists:** Create, rename, or delete playlists; add/remove items from playlists.
- **YouTube:** Search, add results to library/queue, import library/playlists.

## 4. Application Behaviors

### Event Loop and Update Cycle
The application centers around a main event loop in `src/core/event_loop.rs`. It processes events from several sources:
- **User Input:** Keyboard and mouse events captured by `ratatui`.
- **MPD Idle Events:** Real-time updates from MPD (player state, database changes, volume).
- **Worker Results:** Completion signals from background tasks.
- **Timers:** Scheduled tasks like status updates (every ~500ms during playback) and UI message timeouts.

### Background and Asynchronous Tasks
Heavy operations are offloaded to a worker thread (`src/core/work.rs`) to keep the UI responsive:
- Fetching lyrics from the filesystem.
- Querying YouTube metadata and stream URLs.
- Heavy MPD queries (e.g., listing a massive library).
- Indexing local lyrics files.

### State Management
- **Ctx (Context):** A shared structure containing the current application state, including MPD status, configuration, and references to communication channels.
- **DataStore:** A SQLite-backed persistent store for:
    - YouTube metadata (to avoid redundant API calls).
    - Local `rmpc` playlists.
    - Queue synchronization (mapping MPD IDs to YouTube IDs).
- **In-Memory Caches:** Fast access to YouTube metadata and lyrics indexes during a session.

### Error Handling and Recovery
- **MPD Reconnection:** Automatically attempts to reconnect if the connection to the daemon is lost.
- **Status Messages:** Informative messages displayed to the user for errors, warnings, and info (e.g., "Added to queue").
- **YouTube Fallbacks:** Handles unavailable videos or network errors gracefully, prompting the user if a video needs to be removed from the library.

### Metadata and State Persistence
`rmpc` uses a local SQLite database (DataStore) to enhance the user experience and provide features beyond standard MPD capabilities:
- **YouTube Library:** Stores metadata for YouTube tracks, allowing them to be browsed and searched as a local library.
- **Rmpc Playlists:** Local playlists that can mix MPD-hosted files and YouTube streams.
- **Queue Mapping:** Maintains a mapping between MPD's volatile song IDs and permanent YouTube IDs, enabling features like automatic stream refreshing.
- **Lyrics Cache:** While lyrics are primarily file-based, the application maintains an index for fast lookup.

### User Input Handling
Input is processed hierarchically:
1. **Modals:** If a modal is open, it captures all input.
2. **Active Tab:** The current tab's panes handle actions first.
3. **Global Actions:** If not handled by the active tab, the key is checked against global keybindings.
