use crate::{
    core::data_store::models::{PlaylistItem, YouTubeSong},
    ctx::Ctx,
    shared::{events::AppEvent, macros::status_info},
    ui::{modals::menu, UiAppEvent},
};
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet, HashSet};


/// Selection state for multi-selection operations
#[derive(Debug, Default, Clone)]
pub struct SelectionState {
    /// Selected artist indices
    pub selected_artists: BTreeSet<usize>,
    /// Selected song indices per artist
    pub selected_songs: BTreeMap<String, BTreeSet<usize>>,
}

impl SelectionState {
    pub fn new() -> Self {
        Self::default()
    }
    
    pub fn clear(&mut self) {
        self.selected_artists.clear();
        self.selected_songs.clear();
    }
    
    pub fn is_artist_selected(&self, artist_index: usize) -> bool {
        self.selected_artists.contains(&artist_index)
    }
    
    pub fn is_song_selected(&self, artist: &str, song_index: usize) -> bool {
        self.selected_songs
            .get(artist)
            .map_or(false, |indices| indices.contains(&song_index))
    }
    
    pub fn toggle_artist(&mut self, artist_index: usize, artist_name: &str, songs: &[YouTubeSong]) {
        if self.selected_artists.remove(&artist_index) {
            // Deselect artist, remove all song selections for it
            self.selected_songs.remove(artist_name);
        } else {
            // Select artist, select all its songs
            self.selected_artists.insert(artist_index);
            let all_indices: BTreeSet<usize> = (0..songs.len()).collect();
            self.selected_songs.insert(artist_name.to_string(), all_indices);
        }
    }
    
    pub fn toggle_song(&mut self, artist_name: &str, song_index: usize) {
        let selections = self.selected_songs.entry(artist_name.to_string()).or_default();
        if !selections.remove(&song_index) { selections.insert(song_index); }
    }
    
    pub fn clear_artist_songs(&mut self, artist_name: &str) {
        if let Some(selections) = self.selected_songs.get_mut(artist_name) {
            selections.clear();
        }
    }
    
    pub fn get_selected_songs(&self, songs_by_artist: &BTreeMap<String, Vec<YouTubeSong>>) -> Vec<YouTubeSong> {
        self.selected_songs
            .iter()
            .flat_map(|(artist, song_indices)| {
                songs_by_artist.get(artist).map_or_else(
                    Vec::new,
                    |songs| song_indices.iter().filter_map(|&index| songs.get(index).cloned()).collect(),
                )
            })
            .collect()
    }
}

/// Navigation state for library browsing
#[derive(Debug, Default, Clone)]
pub struct NavigationState {
    /// Current selected artist index
    pub selected_artist_index: Option<usize>,
    /// Current selected song index within the selected artist
    pub selected_song_index: Option<usize>,
}

impl NavigationState {
    pub fn new() -> Self {
        Self::default()
    }
    
    pub fn select_artist(&mut self, index: usize) {
        self.selected_artist_index = Some(index);
        self.selected_song_index = Some(0);
    }
    
    pub fn select_song(&mut self, index: usize) { self.selected_song_index = Some(index); }
    
    pub fn move_artist_selection(&mut self, artist_count: usize, change: isize) {
        if artist_count == 0 {
            self.selected_artist_index = None;
            return;
        }
        let current = self.selected_artist_index.unwrap_or(0);
        let next = (current as isize + change).max(0).min(artist_count.saturating_sub(1) as isize);
        self.select_artist(next as usize);
    }
    
    pub fn move_song_selection(&mut self, song_count: usize, change: isize) {
        if song_count == 0 {
            self.selected_song_index = None;
            return;
        }
        let current = self.selected_song_index.unwrap_or(0);
        let next = (current as isize + change).max(0).min(song_count.saturating_sub(1) as isize);
        self.select_song(next as usize);
    }
}

/// A data model representing the user's YouTube library.
#[derive(Debug, Default)]
pub struct YouTubeLibrary {
    pub songs_by_artist: BTreeMap<String, Vec<YouTubeSong>>,
    artists: Vec<String>,
    song_id_to_artist: BTreeMap<String, String>,
}

impl YouTubeLibrary {
    /// Adds a song to the library model. Returns true if the song was new.
    pub fn add_song(&mut self, song: YouTubeSong) -> bool {
        if self.song_id_to_artist.contains_key(&song.youtube_id) {
            return false;
        }
        self.song_id_to_artist.insert(song.youtube_id.clone(), song.artist.clone());
        let songs = self.songs_by_artist.entry(song.artist.clone()).or_default();
        if songs.is_empty() {
            // New artist, so the cache needs updating.
            songs.push(song);
            self.update_artists_cache();
        } else {
            songs.push(song);
        }
        true
    }

    /// Removes a song from the library model. Returns true if the song existed.
    pub fn remove_song(&mut self, youtube_id: &str) -> bool {
        // [FIXED] Replaced invalid `guard let` with `if let ... else`.
        if let Some(artist_name) = self.song_id_to_artist.remove(youtube_id) {
            let mut artist_is_empty = false;
            if let Some(songs) = self.songs_by_artist.get_mut(&artist_name) {
                songs.retain(|s| s.youtube_id != youtube_id);
                artist_is_empty = songs.is_empty();
            }
            if artist_is_empty {
                self.songs_by_artist.remove(&artist_name);
                self.update_artists_cache();
            }
            true
        } else {
            false
        }
    }

    pub fn artists(&self) -> &[String] { &self.artists }
    pub fn songs_for_artist(&self, artist: &str) -> Option<&[YouTubeSong]> {
        self.songs_by_artist.get(artist).map(|s| s.as_slice())
    }

    fn update_artists_cache(&mut self) {
        self.artists = self.songs_by_artist.keys().cloned().collect();
        self.artists.sort_by_key(|a| a.to_lowercase());
    }
}

/// Manages the UI state (navigation, selection) for the YouTube library.
#[derive(Debug)]
pub struct YouTubeLibraryController {
    library: YouTubeLibrary,
    selection_state: SelectionState,
    navigation_state: NavigationState,
}


impl YouTubeLibraryController {
    pub fn new() -> Self {
        Self {
            library: YouTubeLibrary::default(),
            selection_state: SelectionState::default(),
            navigation_state: NavigationState::default(),
        }
    }

    pub fn load_from_data_store(ctx: &Ctx) -> Result<Self> {
        let songs = ctx.data_store.get_all_library_songs()?;
        let mut controller = Self::new();
        // Load songs into the model, not the controller directly.
        for song in songs {
            controller.library.add_song(song);
        }
        controller.reset_navigation_if_needed();
        Ok(controller)
    }
    
    // === Public API ===
    
    /// Get all artists
    // === Public API (now delegates data queries to the model) ===
    pub fn artists(&self) -> &[String] { self.library.artists() }
    
    /// Get songs for a specific artist
    pub fn songs_for_artist(&self, artist: &str) -> Option<&[YouTubeSong]> {
        self.library.songs_for_artist(artist)
    }
    
    /// Get currently selected artist
    pub fn selected_artist(&self) -> Option<&str> {
        self.navigation_state.selected_artist_index
            .and_then(|i| self.library.artists().get(i))
            .map(AsRef::as_ref)
    }
    
    /// Get currently selected song
    pub fn selected_song(&self) -> Option<&YouTubeSong> {
        self.navigation_state.selected_song_index.and_then(|song_idx| {
            self.selected_artist()
                .and_then(|artist| self.library.songs_for_artist(artist))
                .and_then(|songs| songs.get(song_idx))
        })
    }
    
    // === Read-only accessors for state ===
    pub fn selection_state(&self) -> &SelectionState { &self.selection_state }
    pub fn navigation_state(&self) -> &NavigationState { &self.navigation_state }
    pub fn songs_by_artist(&self) -> &BTreeMap<String, Vec<YouTubeSong>> { &self.library.songs_by_artist }
    
    // === Library Management ===
    
    /// Add a song to the library
    pub fn add_song(&mut self, song: YouTubeSong) -> bool {
        if self.library.add_song(song) {
            self.reset_navigation_if_needed();
            true
        } else { false }
    }
    
    /// Remove a song by YouTube ID
    pub fn remove_song(&mut self, youtube_id: &str) -> bool {
        if self.library.remove_song(youtube_id) {
            self.selection_state.clear(); // Selections are now invalid.
            self.reset_navigation_if_needed();
            true
        } else { false }
    }
    
    // === Navigation Operations ===
    
    /// Move artist selection up/down
    pub fn move_artist_selection(&mut self, change: isize) {
        self.navigation_state.move_artist_selection(self.library.artists().len(), change);
    }
    
    /// Move song selection up/down
    pub fn move_song_selection(&mut self, change: isize) {
        if let Some(artist) = self.selected_artist() {
            let song_count = self.library.songs_for_artist(artist).map_or(0, |s| s.len());
            self.navigation_state.move_song_selection(song_count, change);
        }
    }
    
    /// Select specific artist by index
    pub fn select_artist(&mut self, index: usize) {
        if index < self.library.artists().len() {
            self.navigation_state.select_artist(index);
        }
    }
    
    /// Select specific song by index
    pub fn select_song(&mut self, index: usize) {
        if let Some(artist) = self.selected_artist() {
            let song_count = self.library.songs_for_artist(artist).map_or(0, |s| s.len());
            if index < song_count {
                self.navigation_state.select_song(index);
            }
        }
    }
    
    // === Selection Operations ===
    
    /// Toggle artist selection
    pub fn toggle_artist_selection(&mut self, artist_index: usize) {
        if let Some(artist_name) = self.library.artists().get(artist_index) {
            if let Some(songs) = self.library.songs_for_artist(artist_name) {
                self.selection_state.toggle_artist(artist_index, artist_name, songs);
            }
        }
    }
    
    /// Toggle song selection
    pub fn toggle_song_selection(&mut self, song_index: usize) {
        if let Some(artist) = self.selected_artist().map(|s| s.to_owned()) {
            self.selection_state.toggle_song(&artist, song_index);
        }
    }
    
    /// Clear all selections
    pub fn clear_selections(&mut self) {
        self.selection_state.clear();
    }
    
    /// Clear song selections for current artist
    pub fn clear_current_artist_selections(&mut self) {
        if let Some(artist) = self.selected_artist().map(|s| s.to_owned()) {
            self.selection_state.clear_artist_songs(&artist);
        }
    }
    
    /// Get all currently selected songs
    pub fn get_selected_songs(&self) -> Vec<YouTubeSong> {
        self.selection_state.get_selected_songs(&self.library.songs_by_artist)
    }
    
    // === Queue Operations ===
    
    pub fn get_selected_song_for_queueing(&self) -> Result<YouTubeSong, &'static str> {
        self.selected_song().cloned().ok_or("No song selected to add to the queue.")
    }
    
    // === Context Menu Operations ===
    
    /// Show context menu for selected songs
    pub fn show_context_menu(&self, ctx: &Ctx) -> Result<()> {
        let selected_songs = self.get_selected_songs();
        if selected_songs.is_empty() {
            if let Some(current_song) = self.selected_song() {
                self.show_context_menu_for_songs(vec![current_song.clone()], ctx)
            } else {
                status_info!("No songs selected");
                Ok(())
            }
        } else {
            self.show_context_menu_for_songs(selected_songs, ctx)
        }
    }
    
    /// Show context menu for specific songs
    pub fn show_context_menu_for_songs(&self, songs: Vec<YouTubeSong>, ctx: &Ctx) -> Result<()> {
        if songs.is_empty() { return Ok(()); }
        let items: Vec<_> = songs.iter().map(|song| PlaylistItem::YouTube(song.clone())).collect();
        let playlists = ctx.data_store.get_playlist_names()?;
        let modal = menu::modal::MenuModal::new(ctx)
            .list_section(ctx, menu::queue_actions(items.clone()))
            .list_section(ctx, menu::add_to_playlist_actions(items.clone(), playlists))
            .list_section(ctx, menu::create_playlist_action(items))
            .list_section(ctx, menu::youtube_library_actions(songs))
            .list_section(ctx, |s| Some(s.item("Cancel", |_| Ok(()))))
            .build();
        ctx.app_event_sender.send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?;
        Ok(())
    }
    
    // === Private Implementation ===
    
    /// Reset navigation if current selections are invalid
    fn reset_navigation_if_needed(&mut self) {
        let artist_count = self.library.artists().len();
        if artist_count == 0 {
            self.navigation_state.selected_artist_index = None;
            self.navigation_state.selected_song_index = None;
            return;
        }

        let mut artist_changed = false;
        if let Some(selected_index) = self.navigation_state.selected_artist_index {
            if selected_index >= artist_count {
                self.navigation_state.selected_artist_index = Some(artist_count - 1);
                artist_changed = true;
            }
        } else {
            self.navigation_state.select_artist(0);
            artist_changed = true;
        }

        if artist_changed {
            self.navigation_state.selected_song_index = Some(0);
        }

        if let Some(artist_name) = self.selected_artist() {
            let song_count = self.library.songs_for_artist(artist_name).map_or(0, |s| s.len());
            if song_count == 0 {
                self.navigation_state.selected_song_index = None;
                return;
            }
            if let Some(song_idx) = self.navigation_state.selected_song_index {
                if song_idx >= song_count {
                    self.navigation_state.selected_song_index = Some(song_count - 1);
                }
            } else {
                self.navigation_state.selected_song_index = Some(0);
            }
        }
    }
}

impl Default for YouTubeLibraryController {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_song(id: &str, title: &str, artist: &str) -> YouTubeSong {
        YouTubeSong {
            youtube_id: id.to_string(),
            title: title.to_string(),
            artist: artist.to_string(),
            album: None,
            duration_secs: 180,
        }
    }

    // --- YouTubeLibrary Tests ---
    #[test]
    fn test_library_add_song() {
        let mut library = YouTubeLibrary::default();
        let song1 = create_song("1", "Song A", "Artist X");
        let song2 = create_song("2", "Song B", "Artist Y");
        let song3 = create_song("3", "Song C", "Artist X");

        assert!(library.add_song(song1.clone()));
        assert!(library.add_song(song2));
        assert!(library.add_song(song3));
        assert!(!library.add_song(song1)); // Duplicate

        assert_eq!(library.artists(), &["Artist X", "Artist Y"]);
        assert_eq!(library.songs_for_artist("Artist X").unwrap().len(), 2);
        assert_eq!(library.songs_for_artist("Artist Y").unwrap().len(), 1);
    }

    #[test]
    fn test_library_remove_song() {
        let mut library = YouTubeLibrary::default();
        let song1 = create_song("1", "Song A", "Artist X");
        let song2 = create_song("2", "Song B", "Artist Y");
        library.add_song(song1);
        library.add_song(song2);

        assert!(library.remove_song("1")); // Remove song from Artist X
        assert_eq!(library.artists(), &["Artist Y"]); // Artist X should be gone
        assert!(library.songs_for_artist("Artist X").is_none());
        assert!(!library.remove_song("99")); // Non-existent
    }

    // --- NavigationState Tests ---
    #[test]
    fn test_nav_move_artist_selection() {
        let mut nav = NavigationState::new();
        nav.move_artist_selection(5, 1); // From 0 to 1
        assert_eq!(nav.selected_artist_index, Some(1));
        nav.move_artist_selection(5, -2); // From 1 to 0 (clamped)
        assert_eq!(nav.selected_artist_index, Some(0));
        nav.move_artist_selection(5, 10); // From 0 to 4 (clamped)
        assert_eq!(nav.selected_artist_index, Some(4));
    }

    // --- SelectionState Tests ---
    #[test]
    fn test_selection_toggle_artist() {
        let mut selection = SelectionState::new();
        let songs = vec![create_song("1", "A", "Z"), create_song("2", "B", "Z")];
        
        // Select artist
        selection.toggle_artist(0, "Artist Z", &songs);
        assert!(selection.is_artist_selected(0));
        assert!(selection.is_song_selected("Artist Z", 0));
        assert!(selection.is_song_selected("Artist Z", 1));

        // Deselect artist
        selection.toggle_artist(0, "Artist Z", &songs);
        assert!(!selection.is_artist_selected(0));
        assert!(!selection.is_song_selected("Artist Z", 0));
    }

    #[test]
    fn test_get_selected_songs() {
        let mut library = YouTubeLibrary::default();
        let mut selection = SelectionState::new();
        
        let s1 = create_song("1", "A", "Artist X");
        let s2 = create_song("2", "B", "Artist X");
        let s3 = create_song("3", "C", "Artist Y");
        library.add_song(s1.clone());
        library.add_song(s2.clone());
        library.add_song(s3.clone());

        selection.toggle_song("Artist X", 1); // Select song B
        selection.toggle_song("Artist Y", 0); // Select song C

        let selected = selection.get_selected_songs(&library.songs_by_artist);
        assert_eq!(selected.len(), 2);
        assert!(selected.contains(&s2));
        assert!(selected.contains(&s3));
    }
}
