use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use itertools::Itertools;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Stylize,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use regex::Regex;
use tui_input::{backend::crossterm::EventHandler, Input};

use std::time::Duration;

use once_cell::sync::Lazy;

use crate::{
    config::keys::CommonAction,
    core::data_store::models::{PlaylistItem, YouTubeSong},
    shared::id,
    ctx::Ctx,
    youtube::ResolvedYouTubeSong,
    shared::{
        events::{AppEvent, WorkRequest},
        key_event::KeyEvent,
        macros::status_info,
        mouse_event::{MouseEvent, MouseEventKind},
    },
    ui::{
        modals::menu,
        panes::{render_preview_data, Pane},
        UiAppEvent,
    },
};

static CLEAR_STATUS_JOB_ID: Lazy<id::Id> = Lazy::new(id::new);

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Focus {
    SearchInput,
    SearchResults,
    LibraryArtists,
    LibrarySongs,
}

impl Default for Focus {
    fn default() -> Self {
        Self::SearchInput
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    #[default]
    Fuzzy,
    Contains,
    StartsWith,
    Exact,
    Regex,
}

impl SearchMode {
    const MODES: &'static [Self] = &[
        Self::Fuzzy,
        Self::Contains,
        Self::StartsWith,
        Self::Exact,
        Self::Regex,
    ];

    pub fn next(&self) -> Self {
        let current_index = Self::MODES.iter().position(|&m| m == *self).unwrap_or(0);
        let next_index = (current_index + 1) % Self::MODES.len();
        Self::MODES[next_index]
    }

    pub fn previous(&self) -> Self {
        let current_index = Self::MODES.iter().position(|&m| m == *self).unwrap_or(0);
        let prev_index = (current_index + Self::MODES.len() - 1) % Self::MODES.len();
        Self::MODES[prev_index]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Fuzzy => "Fuzzy",
            Self::Contains => "Contains",
            Self::StartsWith => "Starts With",
            Self::Exact => "Exact Match",
            Self::Regex => "Regex",
        }
    }
}

#[derive(Debug, Default)]
enum LibrarySongFocus {
    #[default]
    List,
    Preview,
}

pub struct YouTubePane {
    focus: Focus,
    library_song_focus: LibrarySongFocus,
    // Search state
    search_mode: SearchMode,
    search_generation: u64,
    search_input: Input,
    raw_search_results: Vec<ResolvedYouTubeSong>,
    filtered_search_results: Vec<(i64, ResolvedYouTubeSong, Vec<usize>)>,
    search_list_state: ListState,
    is_loading_search: bool,
    matcher: SkimMatcherV2,
    // Library state
    pub songs_by_artist: BTreeMap<String, Vec<YouTubeSong>>,
    artists: Vec<String>,
    artist_list_state: ListState,
    song_list_state: ListState,
    selected_artists: BTreeSet<usize>,
    selected_songs: BTreeMap<String, BTreeSet<usize>>,
    // Area cache for mouse events
    search_input_area: Rect,
    search_results_area: Rect,
    library_artists_area: Rect,
    library_songs_area: Rect,
}

impl std::fmt::Debug for YouTubePane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YouTubePane").field("focus", &self.focus).finish_non_exhaustive()
    }
}

const CTRL_ALT: KeyModifiers = KeyModifiers::CONTROL.union(KeyModifiers::ALT);

impl YouTubePane {
    pub fn new(ctx: &Ctx) -> Result<Self> {
        let songs = ctx.data_store.get_all_library_songs()?;
        let mut songs_by_artist: BTreeMap<String, Vec<YouTubeSong>> = BTreeMap::new();
        for song in songs {
            songs_by_artist.entry(song.artist.clone()).or_default().push(song);
        }
        let artists = songs_by_artist.keys().cloned().collect();
        Ok(Self {
            focus: Focus::default(),
            library_song_focus: LibrarySongFocus::default(),
            search_mode: SearchMode::default(),
            search_generation: 0,
            search_input: Input::default(),
            raw_search_results: Vec::new(),
            filtered_search_results: Vec::new(),
            search_list_state: ListState::default(),
            is_loading_search: false,
            matcher: SkimMatcherV2::default(),
            songs_by_artist,
            artists,
            artist_list_state: ListState::default(),
            song_list_state: ListState::default(),
            selected_artists: BTreeSet::new(),
            selected_songs: BTreeMap::new(),
            search_input_area: Rect::default(),
            search_results_area: Rect::default(),
            library_artists_area: Rect::default(),
            library_songs_area: Rect::default(),
        })
    }

    // Search methods
    pub fn on_search_result(&mut self, song_info: ResolvedYouTubeSong, generation: u64) {
        if generation == self.search_generation {
            self.raw_search_results.push(song_info);
            self.filter_search_results();
        }
    }

    pub fn on_search_complete(&mut self, generation: u64) {
        if generation == self.search_generation {
            self.is_loading_search = false;
            if !self.filtered_search_results.is_empty() {
                self.search_list_state.select(Some(0));
            }
        }
    }

    fn filter_search_results(&mut self) {
        let query = self.search_input.value();
        if query.is_empty() {
            self.filtered_search_results = self
                .raw_search_results
                .clone()
                .into_iter()
                .map(|v| (100, v, vec![]))
                .collect();
        } else {
            match self.search_mode {
                SearchMode::Fuzzy => {
                    self.filtered_search_results = self
                        .raw_search_results
                        .iter()
                        .filter_map(|song_info| {
                            self.matcher
                                .fuzzy_indices(&song_info.title, query)
                                .map(|(score, indices)| (score, song_info.clone(), indices))
                        })
                        .sorted_by_key(|(score, _, _)| -*score)
                        .collect();
                }
                SearchMode::Contains => {
                    let lower_query = query.to_lowercase();
                    self.filtered_search_results = self
                        .raw_search_results
                        .iter()
                        .filter(|v| v.title.to_lowercase().contains(&lower_query))
                        .map(|v| (100, v.clone(), vec![]))
                        .collect();
                }
                SearchMode::StartsWith => {
                    let lower_query = query.to_lowercase();
                    self.filtered_search_results = self
                        .raw_search_results
                        .iter()
                        .filter(|v| v.title.to_lowercase().starts_with(&lower_query))
                        .map(|v| (100, v.clone(), vec![]))
                        .collect();
                }
                SearchMode::Exact => {
                    self.filtered_search_results = self
                        .raw_search_results
                        .iter()
                        .filter(|v| v.title.eq_ignore_ascii_case(query))
                        .map(|v| (100, v.clone(), vec![]))
                        .collect();
                }
                SearchMode::Regex => {
                    if let Ok(re) = Regex::new(query) {
                        self.filtered_search_results = self
                            .raw_search_results
                            .iter()
                            .filter(|v| re.is_match(&v.title))
                            .map(|v| (100, v.clone(), vec![]))
                            .collect();
                    } else {
                        // Invalid regex, show no results
                        self.filtered_search_results.clear();
                    }
                }
            }
        }

        if self.filtered_search_results.is_empty() {
            self.search_list_state.select(None);
        } else if self.search_list_state.selected().is_none() {
            self.search_list_state.select(Some(0));
        }
    }

    // Library methods
    pub fn add_song(&mut self, song: YouTubeSong) {
        let songs = self.songs_by_artist.entry(song.artist.clone()).or_default();
        if !songs.iter().any(|v| v.youtube_id == song.youtube_id) {
            songs.push(song);
            self.update_artists();
        }
    }

    pub fn remove_song(&mut self, song_id: &str) {
        let mut empty_artist = None;
        for (artist, songs) in self.songs_by_artist.iter_mut() {
            let song_count = songs.len();
            songs.retain(|v| v.youtube_id != song_id);
            if songs.len() < song_count {
                // A song was removed, so invalidate selections for this artist
                self.selected_songs.remove(artist);
            }
            if songs.is_empty() {
                empty_artist = Some(artist.clone());
            }
        }
        if let Some(artist) = empty_artist {
            self.songs_by_artist.remove(&artist);
            self.update_artists();
            self.artist_list_state.select(Some(0));
            self.song_list_state.select(Some(0));
        }
    }

    fn update_artists(&mut self) {
        self.artists = self.songs_by_artist.keys().cloned().collect();
        if self.artist_list_state.selected().is_none() && !self.artists.is_empty() {
            self.artist_list_state.select(Some(0));
            self.song_list_state.select(Some(0));
        }
    }

    fn get_selected_artist(&self) -> Option<&str> {
        self.artist_list_state.selected().and_then(|i| self.artists.get(i)).map(|s| s.as_str())
    }

    fn get_selected_song(&self) -> Option<&YouTubeSong> {
        let artist = self.get_selected_artist()?;
        let songs = self.songs_by_artist.get(artist)?;
        self.song_list_state.selected().and_then(|i| songs.get(i))
    }

    // Navigation methods
    fn move_selection(list_state: &mut ListState, count: usize, change: isize) {
        let current = list_state.selected().unwrap_or(0);
        let next = (current as isize + change).max(0).min(count.saturating_sub(1) as isize);
        list_state.select(Some(next as usize));
    }

    fn add_youtube_song_to_queue(&self, song: &YouTubeSong, ctx: &mut Ctx) -> Result<()> {
        let queue_yt_ids = ctx.data_store.get_all_queue_youtube_ids()?;
        if queue_yt_ids.contains(&song.youtube_id) {
            status_info!("'{}' is already in the queue.", song.title);
            return Ok(());
        }

        status_info!("Fetching stream URL for '{}'...", song.title);
        ctx.work_sender.send(WorkRequest::GetYouTubeStreamUrl {
            song: song.clone(),
            context: None,
        })?;
        ctx.render()?;
        Ok(())
    }

    fn add_selected_song_to_queue(&self, ctx: &mut Ctx) -> Result<()> {
        if let Some(song) = self.get_selected_song() {
            self.add_youtube_song_to_queue(song, ctx)?;
        }
        Ok(())
    }

    fn show_youtube_context_menu(&self, selected_songs: Vec<YouTubeSong>, ctx: &Ctx) -> Result<()> {
        if selected_songs.is_empty() {
            return Ok(());
        }

        let items: Vec<_> =
            selected_songs.iter().map(|v| PlaylistItem::YouTube(v.clone())).collect();
        let playlists: Vec<_> = ctx
            .data_store
            .get_all_playlists()?
            .into_iter()
            .map(|p| p.name)
            .collect();

        let modal = menu::modal::MenuModal::new(ctx)
            .list_section(ctx, menu::queue_actions(items.clone()))
            .list_section(ctx, menu::add_to_playlist_actions(items.clone(), playlists))
            .list_section(ctx, menu::create_playlist_action(items))
            .list_section(ctx, menu::youtube_library_actions(selected_songs))
            .list_section(ctx, |s| Some(s.item("Cancel", |_| Ok(()))))
            .build();

        Ok(ctx.app_event_sender.send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?)
    }

    fn handle_search_mode_cycle(&mut self, key_event: &crossterm::event::KeyEvent) -> bool {
        match (key_event.code, key_event.modifiers) {
            // `Ctrl+Shift+F` cycles backwards.
            // Accommodates terminals that send `F` or `f` with `SHIFT`.
            (KeyCode::Char('F' | 'f'), mods) if mods == CTRL_ALT => {
                self.search_mode = self.search_mode.previous();
                true
            }
            // Accommodates terminals that send `Ctrl+Shift+...` as an uppercase char with only `CONTROL`.
            (KeyCode::Char('F'), KeyModifiers::CONTROL) => {
                self.search_mode = self.search_mode.previous();
                true
            }
            // `Ctrl+f` cycles forwards.
            (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                self.search_mode = self.search_mode.next();
                true
            }
            _ => false,
        }
    }

    fn handle_search_input_action(&mut self, event: &mut KeyEvent, ctx: &mut Ctx) -> Result<(), anyhow::Error> {
        if self.handle_search_mode_cycle(&event.inner()) {
            self.filter_search_results();
            event.stop_propagation();
            return Ok(ctx.render()?);
        }

        // Cache the key code to avoid repeated method calls
        let key_code = event.code();

        match key_code {
            KeyCode::Enter => {
                let input_value = self.search_input.value().to_string();
                if !input_value.is_empty() {
                    status_info!("Searching YouTube for: {}", &input_value);
                    self.search_generation += 1;
                    self.is_loading_search = true;
                    self.raw_search_results.clear();
                    self.filter_search_results();

                    ctx.work_sender.send(WorkRequest::YouTubeSearch {
                        query: input_value,
                        generation: self.search_generation,
                    })?;
                }
            }
            KeyCode::Down | KeyCode::Tab => {
                if !self.filtered_search_results.is_empty() {
                    self.focus = Focus::SearchResults;
                }
            }
            _ => {
                // Cache the event inner to avoid repeated calls
                let inner_event = event.inner();

                if self.search_input.handle_event(&Event::Key(inner_event)).is_some() {
                    self.filter_search_results();
                    event.stop_propagation();
                    ctx.render()?;
                } else if matches!(key_code, KeyCode::Right) {
                    self.focus = Focus::LibraryArtists;
                }
            }
        }
        Ok(())
    }

    fn handle_search_results_action(&mut self, event: &mut KeyEvent, ctx: &mut Ctx) -> Result<()> {
        if event.code() == KeyCode::Up && self.search_list_state.selected().is_some_and(|i| i == 0) {
            self.focus = Focus::SearchInput;
            return Ok(());
        }

        let old_selection = self.search_list_state.selected();

        if let Some(action) = event.as_common_action(ctx) {
            match action {
                CommonAction::Down => Self::move_selection(
                    &mut self.search_list_state,
                    self.filtered_search_results.len(),
                    1,
                ),
                CommonAction::Up => Self::move_selection(
                    &mut self.search_list_state,
                    self.filtered_search_results.len(),
                    -1,
                ),
                CommonAction::Confirm => {
                    if let Some(index) = self.search_list_state.selected() {
                        if let Some((_, song_info, _)) = self.filtered_search_results.get(index) {
                            let song: YouTubeSong = song_info.clone().into();
                            self.add_youtube_song_to_queue(&song, ctx)?;
                        }
                    }
                    event.stop_propagation();
                }
                _ => {}
            }
        }

        if old_selection != self.search_list_state.selected() {
            if let Some(index) = self.search_list_state.selected() {
                if let Some((_, song_info, _)) = self.filtered_search_results.get(index) {
                    status_info!("Selected: {} - {}", song_info.title, song_info.artist);
                    ctx.scheduler.schedule_replace(
                        *CLEAR_STATUS_JOB_ID,
                        Duration::from_secs(3),
                        |(event_tx, _)| {
                            event_tx.send(AppEvent::UiEvent(UiAppEvent::ClearStatusMessage))?;
                            Ok(())
                        },
                    );
                }
            }
        } else if let KeyCode::Right = event.code() {
            self.focus = Focus::LibraryArtists;
        }

        Ok(())
    }

    fn handle_library_artists_action(&mut self, event: &mut KeyEvent, ctx: &mut Ctx) -> Result<()> {
        if let Some(action) = event.as_common_action(ctx) {
            match action {
                CommonAction::Down => {
                    Self::move_selection(
                        &mut self.artist_list_state,
                        self.artists.len(),
                        1,
                    );
                    self.song_list_state.select(Some(0));
                }
                CommonAction::Up => {
                    Self::move_selection(
                        &mut self.artist_list_state,
                        self.artists.len(),
                        -1,
                    );
                    self.song_list_state.select(Some(0));
                }
                _ => {}
            }
        }
        match event.code() {
            KeyCode::Char(' ') => {
                if let Some(selected_idx) = self.artist_list_state.selected() {
                    if let Some(artist_name) = self.artists.get(selected_idx).cloned() {
                        if self.selected_artists.remove(&selected_idx) {
                            // Deselect artist, remove song selections for it
                            self.selected_songs.remove(&artist_name);
                        } else {
                            // Select artist, select all its songs
                            self.selected_artists.insert(selected_idx);
                            if let Some(songs) =
                                self.songs_by_artist.get(&artist_name)
                            {
                                let all_indices = (0..songs.len()).collect();
                                self.selected_songs.insert(artist_name, all_indices);
                            }
                        }
                    }
                }
                event.stop_propagation();
            }
            KeyCode::Esc => {
                if !self.selected_artists.is_empty() {
                    self.selected_artists.clear();
                    self.selected_songs.clear();
                    event.stop_propagation();
                }
            }
            KeyCode::Left => self.focus = Focus::SearchInput,
            KeyCode::Right => self.focus = Focus::LibrarySongs,
            KeyCode::Enter => {
                self.add_selected_song_to_queue(ctx)?;
                event.stop_propagation();
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_library_songs_action(&mut self, event: &mut KeyEvent, ctx: &mut Ctx) -> Result<()> {
        let old_selection = self.song_list_state.selected();

        match self.library_song_focus {
            LibrarySongFocus::List => {
                if let Some(action) = event.as_common_action(ctx) {
                    match action {
                        CommonAction::Down => {
                            let songs = self
                                .get_selected_artist()
                                .and_then(|c| self.songs_by_artist.get(c));
                            if let Some(songs) = songs {
                                Self::move_selection(
                                    &mut self.song_list_state,
                                    songs.len(),
                                    1,
                                );
                            }
                        }
                        CommonAction::Up => {
                            let songs = self
                                .get_selected_artist()
                                .and_then(|c| self.songs_by_artist.get(c));
                            if let Some(songs) = songs {
                                Self::move_selection(
                                    &mut self.song_list_state,
                                    songs.len(),
                                    -1,
                                );
                            }
                        }
                        CommonAction::Confirm => {
                            self.add_selected_song_to_queue(ctx)?;
                            event.stop_propagation();
                        }
                        _ => {}
                    }
                }
                match event.code() {
                    KeyCode::Char(' ') => {
                        if let Some(artist_name) =
                            self.get_selected_artist().map(|s| s.to_string())
                        {
                            if let Some(selected_idx) = self.song_list_state.selected() {
                                let selections =
                                    self.selected_songs.entry(artist_name).or_default();
                                if !selections.remove(&selected_idx) {
                                    selections.insert(selected_idx);
                                }
                            }
                        }
                        event.stop_propagation();
                    }
                    KeyCode::Esc => {
                        if let Some(artist_name) =
                            self.get_selected_artist().map(|s| s.to_string())
                        {
                            if let Some(selections) =
                                self.selected_songs.get_mut(&artist_name)
                            {
                                if !selections.is_empty() {
                                    selections.clear();
                                    event.stop_propagation();
                                }
                            }
                        }
                    }
                    KeyCode::Left => self.focus = Focus::LibraryArtists,
                    KeyCode::Right => self.library_song_focus = LibrarySongFocus::Preview,
                    KeyCode::Delete => {
                        if let Some(song) = self.get_selected_song() {
                            ctx.app_event_sender.send(AppEvent::UiEvent(
                                UiAppEvent::YouTubeLibraryRemoveSong(song.youtube_id.clone()),
                            ))?;
                        }
                    }
                    _ => {}
                }
            }
            LibrarySongFocus::Preview => {
                if let KeyCode::Left = event.code() {
                    self.library_song_focus = LibrarySongFocus::List;
                }
            }
        }

        if old_selection != self.song_list_state.selected() {
            if let Some(song) = self.get_selected_song() {
                status_info!("Selected: {} - {}", song.title, song.artist);
                ctx.scheduler.schedule_replace(
                    *CLEAR_STATUS_JOB_ID,
                    Duration::from_secs(3),
                    |(event_tx, _)| {
                        event_tx.send(AppEvent::UiEvent(UiAppEvent::ClearStatusMessage))?;
                        Ok(())
                    },
                );
            }
        }

        Ok(())
    }

    fn render_search_result_item<'a>(
        song_info: &'a ResolvedYouTubeSong,
        indices: &'a [usize],
        ctx: &'a Ctx,
    ) -> ListItem<'a> {
        let highlight_style = ctx.config.theme.highlighted_item_style.bold();
        let highlighted_indices: HashSet<usize> = indices.iter().cloned().collect();

        let mut spans = Vec::new();
        let mut current_chars = String::new();
        let mut is_currently_highlighted = false;

        // Nous ne pouvons surligner que le titre, car les indices ne s'appliquent qu'à lui.
        for (i, char) in song_info.title.char_indices() {
            let should_be_highlighted = highlighted_indices.contains(&i);

            if i == 0 {
                is_currently_highlighted = should_be_highlighted;
            } else if should_be_highlighted != is_currently_highlighted {
                let style =
                    if is_currently_highlighted { highlight_style } else { Default::default() };
                spans.push(Span::styled(current_chars.clone(), style));
                current_chars.clear();
                is_currently_highlighted = should_be_highlighted;
            }
            current_chars.push(char);
        }

        if !current_chars.is_empty() {
            let style = if is_currently_highlighted { highlight_style } else { Default::default() };
            spans.push(Span::styled(current_chars, style));
        }

        // Ajouter le reste de la chaîne (artiste) sans style particulier.
        spans.push(Span::raw(format!(" - {}", song_info.artist)));

        ListItem::new(Line::from(spans))
    }
}

impl Pane for YouTubePane {
    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Result<()> {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(40),
                Constraint::Percentage(30),
                Constraint::Percentage(30),
            ])
            .split(area);
        self.library_artists_area = columns[1];
        self.library_songs_area = columns[2];

        // --- Column 1: Search ---
        let search_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(columns[0]);
        self.search_input_area = search_layout[0];
        self.search_results_area = search_layout[1];

        let title = format!("Search Query [{}]", self.search_mode.display_name());
        let input_block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(if self.focus == Focus::SearchInput {
                ctx.config.as_focused_border_style()
            } else {
                ctx.config.as_border_style()
            });

        let scroll =
            self.search_input.visual_scroll(search_layout[0].width.saturating_sub(3) as usize);
        let input_widget =
            Paragraph::new(self.search_input.value()).scroll((0, scroll as u16)).block(input_block);

        frame.render_widget(input_widget, search_layout[0]);
        if self.focus == Focus::SearchInput {
            frame.set_cursor_position(Rect {
                x: search_layout[0].x + (self.search_input.visual_cursor() - scroll) as u16 + 1,
                y: search_layout[0].y + 1,
                width: 1,
                height: 1,
            });
        }

        let results_title =
            if self.is_loading_search { "Results (Loading...)" } else { "Results" };
        let results_block = Block::default()
            .borders(Borders::ALL)
            .title(results_title)
            .border_style(if self.focus == Focus::SearchResults {
                ctx.config.as_focused_border_style()
            } else {
                ctx.config.as_border_style()
            });
        let search_items: Vec<ListItem> = self
            .filtered_search_results
            .iter()
            .map(|(_, v_info, indices)| Self::render_search_result_item(v_info, indices, ctx))
            .collect();
        let search_list = List::new(search_items)
            .block(results_block)
            .highlight_style(ctx.config.theme.highlighted_item_style);
        frame.render_stateful_widget(
            search_list,
            search_layout[1],
            &mut self.search_list_state,
        );

        // --- Column 2: Library Artists / Tracks ---
        let artists_block = Block::default()
            .borders(Borders::ALL)
            .title("Library: Artists")
            .border_style(if self.focus == Focus::LibraryArtists {
                ctx.config.as_focused_border_style()
            } else {
                ctx.config.as_border_style()
            });
        let artist_items: Vec<ListItem> = self
            .artists
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut item = ListItem::new(c.as_str());
                if self.selected_artists.contains(&i) {
                    item = item.style(ctx.config.theme.highlighted_item_style.reversed());
                }
                item
            })
            .collect();
        let artist_list = List::new(artist_items)
            .block(artists_block)
            .highlight_style(ctx.config.theme.highlighted_item_style);
        frame.render_stateful_widget(artist_list, columns[1], &mut self.artist_list_state);

        // --- Column 3: Library Songs / Preview ---
        match self.library_song_focus {
            LibrarySongFocus::List => {
                let songs_block = Block::default()
                    .borders(Borders::ALL)
                    .title("Library: Tracks")
                    .border_style(if self.focus == Focus::LibrarySongs {
                        ctx.config.as_focused_border_style()
                    } else {
                        ctx.config.as_border_style()
                    });
                let song_items: Vec<ListItem> = self
                    .get_selected_artist()
                    .and_then(|c| self.songs_by_artist.get(c))
                    .map(|songs| {
                        songs
                            .iter()
                            .enumerate()
                            .map(|(i, v)| {
                                let mut item = ListItem::new(v.title.as_str());
                                let is_selected = self
                                    .get_selected_artist()
                                    .and_then(|c| self.selected_songs.get(c))
                                    .map_or(false, |s| s.contains(&i));
                                if is_selected {
                                    item = item.style(
                                        ctx.config.theme.highlighted_item_style.reversed(),
                                    );
                                }
                                item
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let song_list = List::new(song_items)
                    .block(songs_block)
                    .highlight_style(ctx.config.theme.highlighted_item_style);
                frame.render_stateful_widget(song_list, columns[2], &mut self.song_list_state);
            }
            LibrarySongFocus::Preview => {
                let preview_block = Block::default()
                    .borders(Borders::ALL)
                    .title("Preview")
                    .border_style(ctx.config.as_focused_border_style());

                let preview_data = self.get_selected_song().map(|song| {
                    let key_style = ctx.config.theme.preview_label_style;
                    let group_style = ctx.config.theme.preview_metadata_group_style;
                    song.to_song_for_preview().to_preview(key_style, group_style)
                });

                if let Some(data) = preview_data {
                    render_preview_data(frame, columns[2], &data, preview_block);
                } else {
                    let preview_widget = Paragraph::new("No song selected")
                        .block(preview_block)
                        .wrap(Wrap { trim: false });
                    frame.render_widget(preview_widget, columns[2]);
                }
            }
        }

        Ok(())
    }

    fn handle_action(&mut self, event: &mut KeyEvent, ctx: &mut Ctx) -> Result<()> {
        match self.focus {
            Focus::SearchInput => self.handle_search_input_action(event, ctx)?,
            Focus::SearchResults => self.handle_search_results_action(event, ctx)?,
            Focus::LibraryArtists => self.handle_library_artists_action(event, ctx)?,
            Focus::LibrarySongs => self.handle_library_songs_action(event, ctx)?,
        }
        ctx.render()?;
        Ok(())
    }

    fn handle_mouse_event(&mut self, event: MouseEvent, ctx: &Ctx) -> Result<()> {
        let pos = event.into();
        match event.kind {
            MouseEventKind::LeftClick => {
                if self.search_input_area.contains(pos) && pos.y == self.search_input_area.y {
                    self.search_mode = self.search_mode.next();
                    self.filter_search_results();
                    self.focus = Focus::SearchInput;
                    ctx.app_event_sender.send(AppEvent::RequestRender)?;
                    return Ok(());
                }
                if self.search_input_area.contains(pos) {
                    self.focus = Focus::SearchInput;
                } else if self.search_results_area.contains(pos) {
                    self.focus = Focus::SearchResults;
                } else if self.library_artists_area.contains(pos) {
                    self.focus = Focus::LibraryArtists;
                } else if self.library_songs_area.contains(pos) {
                    self.focus = Focus::LibrarySongs;
                }
                ctx.app_event_sender.send(AppEvent::RequestRender)?;
            }
            MouseEventKind::MiddleClick => {
                if self.search_input_area.contains(pos) {
                    self.focus = Focus::SearchInput;
                    ctx.app_event_sender.send(AppEvent::RequestRender)?;
                }
                // Ne rien faire si le clic est en dehors de la zone de recherche
            }
            MouseEventKind::RightClick => {
                if self.library_songs_area.contains(pos) {
                    self.focus = Focus::LibrarySongs;
                    if let Some(artist_name) = self.get_selected_artist().map(|c| c.to_owned()) {
                        let clicked_row = pos.y.saturating_sub(self.library_songs_area.y + 1);
                        let clicked_idx = self.song_list_state.offset() + clicked_row as usize;

                        let selections = self.selected_songs.entry(artist_name.clone()).or_default();
                        if !selections.contains(&clicked_idx) {
                            selections.clear();
                        }
                        selections.insert(clicked_idx);
                        self.song_list_state.select(Some(clicked_idx));

                        let songs = self.songs_by_artist.get(&artist_name).unwrap();
                        let selected_songs: Vec<_> = selections.iter().filter_map(|i| songs.get(*i).cloned()).collect();

                        self.show_youtube_context_menu(selected_songs, ctx)?;
                    }
                } else if self.library_artists_area.contains(pos) {
                    self.focus = Focus::LibraryArtists;
                    let clicked_row = pos.y.saturating_sub(self.library_artists_area.y + 1);
                    let clicked_idx = self.artist_list_state.offset() + clicked_row as usize;

                    if !self.selected_artists.contains(&clicked_idx) {
                        self.selected_artists.clear();
                        self.selected_songs.clear();
                    }
                    self.selected_artists.insert(clicked_idx);
                    self.artist_list_state.select(Some(clicked_idx));

                    let selected_songs: Vec<_> = self.selected_artists.iter()
                        .filter_map(|i| self.artists.get(*i))
                        .filter_map(|c| self.songs_by_artist.get(c))
                        .flatten()
                        .cloned()
                        .collect();
                    
                    self.show_youtube_context_menu(selected_songs, ctx)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
