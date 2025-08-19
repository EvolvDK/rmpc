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
    core::data_store::models::{PlaylistItem, YouTubeVideo},
    shared::id,
    ctx::Ctx,
    youtube::YtDlpVideoInfo,
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
    LibraryChannels,
    LibraryVideos,
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
enum LibraryVideoFocus {
    #[default]
    List,
    Preview,
}

pub struct YouTubePane {
    focus: Focus,
    library_video_focus: LibraryVideoFocus,
    // Search state
    search_mode: SearchMode,
    search_generation: u64,
    search_input: Input,
    raw_search_results: Vec<YtDlpVideoInfo>,
    filtered_search_results: Vec<(i64, YtDlpVideoInfo, Vec<usize>)>,
    search_list_state: ListState,
    is_loading_search: bool,
    matcher: SkimMatcherV2,
    // Library state
    pub videos_by_channel: BTreeMap<String, Vec<YouTubeVideo>>,
    channels: Vec<String>,
    channel_list_state: ListState,
    video_list_state: ListState,
    selected_channels: BTreeSet<usize>,
    selected_videos: BTreeMap<String, BTreeSet<usize>>,
    // Area cache for mouse events
    search_input_area: Rect,
    search_results_area: Rect,
    library_channels_area: Rect,
    library_videos_area: Rect,
}

impl std::fmt::Debug for YouTubePane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YouTubePane").field("focus", &self.focus).finish_non_exhaustive()
    }
}

const CTRL_ALT: KeyModifiers = KeyModifiers::CONTROL.union(KeyModifiers::ALT);

impl YouTubePane {
    pub fn new(ctx: &Ctx) -> Result<Self> {
        let videos = ctx.data_store.get_all_library_videos()?;
        let mut videos_by_channel: BTreeMap<String, Vec<YouTubeVideo>> = BTreeMap::new();
        for video in videos {
            videos_by_channel.entry(video.channel.clone()).or_default().push(video);
        }
        let channels = videos_by_channel.keys().cloned().collect();
        Ok(Self {
            focus: Focus::default(),
            library_video_focus: LibraryVideoFocus::default(),
            search_mode: SearchMode::default(),
            search_generation: 0,
            search_input: Input::default(),
            raw_search_results: Vec::new(),
            filtered_search_results: Vec::new(),
            search_list_state: ListState::default(),
            is_loading_search: false,
            matcher: SkimMatcherV2::default(),
            videos_by_channel,
            channels,
            channel_list_state: ListState::default(),
            video_list_state: ListState::default(),
            selected_channels: BTreeSet::new(),
            selected_videos: BTreeMap::new(),
            search_input_area: Rect::default(),
            search_results_area: Rect::default(),
            library_channels_area: Rect::default(),
            library_videos_area: Rect::default(),
        })
    }

    // Search methods
    pub fn on_search_result(&mut self, video_info: YtDlpVideoInfo, generation: u64) {
        if generation == self.search_generation {
            self.raw_search_results.push(video_info);
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
                        .filter_map(|video_info| {
                            self.matcher
                                .fuzzy_indices(&video_info.title, query)
                                .map(|(score, indices)| (score, video_info.clone(), indices))
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
    pub fn add_video(&mut self, video: YouTubeVideo) {
        let videos = self.videos_by_channel.entry(video.channel.clone()).or_default();
        if !videos.iter().any(|v| v.youtube_id == video.youtube_id) {
            videos.push(video);
            self.update_channels();
        }
    }

    pub fn remove_video(&mut self, video_id: &str) {
        let mut empty_channel = None;
        for (channel, videos) in self.videos_by_channel.iter_mut() {
            let video_count = videos.len();
            videos.retain(|v| v.youtube_id != video_id);
            if videos.len() < video_count {
                // A video was removed, so invalidate selections for this channel
                self.selected_videos.remove(channel);
            }
            if videos.is_empty() {
                empty_channel = Some(channel.clone());
            }
        }
        if let Some(channel) = empty_channel {
            self.videos_by_channel.remove(&channel);
            self.update_channels();
            self.channel_list_state.select(Some(0));
            self.video_list_state.select(Some(0));
        }
    }

    fn update_channels(&mut self) {
        self.channels = self.videos_by_channel.keys().cloned().collect();
        if self.channel_list_state.selected().is_none() && !self.channels.is_empty() {
            self.channel_list_state.select(Some(0));
            self.video_list_state.select(Some(0));
        }
    }

    fn get_selected_channel(&self) -> Option<&str> {
        self.channel_list_state.selected().and_then(|i| self.channels.get(i)).map(|s| s.as_str())
    }

    fn get_selected_video(&self) -> Option<&YouTubeVideo> {
        let channel = self.get_selected_channel()?;
        let videos = self.videos_by_channel.get(channel)?;
        self.video_list_state.selected().and_then(|i| videos.get(i))
    }

    // Navigation methods
    fn move_selection(list_state: &mut ListState, count: usize, change: isize) {
        let current = list_state.selected().unwrap_or(0);
        let next = (current as isize + change).max(0).min(count.saturating_sub(1) as isize);
        list_state.select(Some(next as usize));
    }

    fn add_youtube_video_to_queue(&self, video: &YouTubeVideo, ctx: &mut Ctx) -> Result<()> {
        let queue_yt_ids = ctx.data_store.get_all_queue_youtube_ids()?;
        if queue_yt_ids.contains(&video.youtube_id) {
            status_info!("'{}' is already in the queue.", video.title);
            return Ok(());
        }

        status_info!("Fetching stream URL for '{}'...", video.title);
        ctx.work_sender.send(WorkRequest::GetYouTubeStreamUrl {
            video: video.clone(),
            context: None,
        })?;
        ctx.render()?;
        Ok(())
    }

    fn add_selected_video_to_queue(&self, ctx: &mut Ctx) -> Result<()> {
        if let Some(video) = self.get_selected_video() {
            self.add_youtube_video_to_queue(video, ctx)?;
        }
        Ok(())
    }

    fn show_youtube_context_menu(&self, selected_videos: Vec<YouTubeVideo>, ctx: &Ctx) -> Result<()> {
        if selected_videos.is_empty() {
            return Ok(());
        }

        let items: Vec<_> =
            selected_videos.iter().map(|v| PlaylistItem::YouTube(v.clone())).collect();
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
            .list_section(ctx, menu::youtube_library_actions(selected_videos))
            .list_section(ctx, |s| Some(s.item("Cancel", |_| Ok(()))))
            .build();

        Ok(ctx.app_event_sender.send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?)
    }

    fn handle_search_mode_cycle(&mut self, key_event: &crossterm::event::KeyEvent) -> bool {
        match (key_event.code, key_event.modifiers) {
            // Ctrl+Alt+F (both cases handled in one pattern with exact match)
            (KeyCode::Char('F' | 'f'), mods) if mods == CTRL_ALT => {
                self.search_mode = self.search_mode.previous();
                true
            }
            // Ctrl+F (uppercase - some terminal behavior)
            (KeyCode::Char('F'), KeyModifiers::CONTROL) => {
                self.search_mode = self.search_mode.previous();
                true
            }
            // Ctrl+f (lowercase)
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
                // Avoid unnecessary string allocation if empty
                let input_value = self.search_input.value();
                if !input_value.is_empty() {
                    status_info!("Searching YouTube for: {}", input_value);
                    self.search_generation += 1;
                    self.is_loading_search = true;
                    self.raw_search_results.clear();
                    self.filter_search_results();

                    // Only clone/allocate string when actually needed
                    ctx.work_sender.send(WorkRequest::YouTubeSearch {
                        query: input_value.to_string(),
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
                    self.focus = Focus::LibraryChannels;
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
                        if let Some((_, video_info, _)) = self.filtered_search_results.get(index) {
                            let video: YouTubeVideo = video_info.clone().into();
                            self.add_youtube_video_to_queue(&video, ctx)?;
                        }
                    }
                    event.stop_propagation();
                }
                _ => {}
            }
        }

        if old_selection != self.search_list_state.selected() {
            if let Some(index) = self.search_list_state.selected() {
                if let Some((_, video_info, _)) = self.filtered_search_results.get(index) {
                    status_info!("Selected: {} - {}", video_info.title, video_info.channel);
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
            self.focus = Focus::LibraryChannels;
        }

        Ok(())
    }

    fn handle_library_channels_action(&mut self, event: &mut KeyEvent, ctx: &mut Ctx) -> Result<()> {
        if let Some(action) = event.as_common_action(ctx) {
            match action {
                CommonAction::Down => {
                    Self::move_selection(
                        &mut self.channel_list_state,
                        self.channels.len(),
                        1,
                    );
                    self.video_list_state.select(Some(0));
                }
                CommonAction::Up => {
                    Self::move_selection(
                        &mut self.channel_list_state,
                        self.channels.len(),
                        -1,
                    );
                    self.video_list_state.select(Some(0));
                }
                _ => {}
            }
        }
        match event.code() {
            KeyCode::Char(' ') => {
                if let Some(selected_idx) = self.channel_list_state.selected() {
                    if let Some(channel_name) = self.channels.get(selected_idx).cloned() {
                        if self.selected_channels.remove(&selected_idx) {
                            // Deselect channel, remove video selections for it
                            self.selected_videos.remove(&channel_name);
                        } else {
                            // Select channel, select all its videos
                            self.selected_channels.insert(selected_idx);
                            if let Some(videos) =
                                self.videos_by_channel.get(&channel_name)
                            {
                                let all_indices = (0..videos.len()).collect();
                                self.selected_videos.insert(channel_name, all_indices);
                            }
                        }
                    }
                }
                event.stop_propagation();
            }
            KeyCode::Esc => {
                if !self.selected_channels.is_empty() {
                    self.selected_channels.clear();
                    self.selected_videos.clear();
                    event.stop_propagation();
                }
            }
            KeyCode::Left => self.focus = Focus::SearchInput,
            KeyCode::Right => self.focus = Focus::LibraryVideos,
            KeyCode::Enter => {
                self.add_selected_video_to_queue(ctx)?;
                event.stop_propagation();
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_library_videos_action(&mut self, event: &mut KeyEvent, ctx: &mut Ctx) -> Result<()> {
        let old_selection = self.video_list_state.selected();

        match self.library_video_focus {
            LibraryVideoFocus::List => {
                if let Some(action) = event.as_common_action(ctx) {
                    match action {
                        CommonAction::Down => {
                            let videos = self
                                .get_selected_channel()
                                .and_then(|c| self.videos_by_channel.get(c));
                            if let Some(videos) = videos {
                                Self::move_selection(
                                    &mut self.video_list_state,
                                    videos.len(),
                                    1,
                                );
                            }
                        }
                        CommonAction::Up => {
                            let videos = self
                                .get_selected_channel()
                                .and_then(|c| self.videos_by_channel.get(c));
                            if let Some(videos) = videos {
                                Self::move_selection(
                                    &mut self.video_list_state,
                                    videos.len(),
                                    -1,
                                );
                            }
                        }
                        CommonAction::Confirm => {
                            self.add_selected_video_to_queue(ctx)?;
                            event.stop_propagation();
                        }
                        _ => {}
                    }
                }
                match event.code() {
                    KeyCode::Char(' ') => {
                        if let Some(channel_name) =
                            self.get_selected_channel().map(|s| s.to_string())
                        {
                            if let Some(selected_idx) = self.video_list_state.selected() {
                                let selections =
                                    self.selected_videos.entry(channel_name).or_default();
                                if !selections.remove(&selected_idx) {
                                    selections.insert(selected_idx);
                                }
                            }
                        }
                        event.stop_propagation();
                    }
                    KeyCode::Esc => {
                        if let Some(channel_name) =
                            self.get_selected_channel().map(|s| s.to_string())
                        {
                            if let Some(selections) =
                                self.selected_videos.get_mut(&channel_name)
                            {
                                if !selections.is_empty() {
                                    selections.clear();
                                    event.stop_propagation();
                                }
                            }
                        }
                    }
                    KeyCode::Left => self.focus = Focus::LibraryChannels,
                    KeyCode::Right => self.library_video_focus = LibraryVideoFocus::Preview,
                    KeyCode::Delete => {
                        if let Some(video) = self.get_selected_video() {
                            ctx.app_event_sender.send(AppEvent::UiEvent(
                                UiAppEvent::YouTubeLibraryRemoveVideo(video.youtube_id.clone()),
                            ))?;
                        }
                    }
                    _ => {}
                }
            }
            LibraryVideoFocus::Preview => {
                if let KeyCode::Left = event.code() {
                    self.library_video_focus = LibraryVideoFocus::List;
                }
            }
        }

        if old_selection != self.video_list_state.selected() {
            if let Some(video) = self.get_selected_video() {
                status_info!("Selected: {} - {}", video.title, video.channel);
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
        video_info: &'a YtDlpVideoInfo,
        indices: &'a [usize],
        ctx: &'a Ctx,
    ) -> ListItem<'a> {
        let highlight_style = ctx.config.theme.highlighted_item_style.bold();
        let highlighted_indices: HashSet<usize> = indices.iter().cloned().collect();

        let mut spans = Vec::new();
        let mut current_chars = String::new();
        let mut is_currently_highlighted = false;

        // Nous ne pouvons surligner que le titre, car les indices ne s'appliquent qu'à lui.
        for (i, char) in video_info.title.char_indices() {
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
        spans.push(Span::raw(format!(" - {}", video_info.channel)));

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
        self.library_channels_area = columns[1];
        self.library_videos_area = columns[2];

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
            frame.set_cursor(
                search_layout[0].x + (self.search_input.visual_cursor() - scroll) as u16 + 1,
                search_layout[0].y + 1,
            );
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
        let channels_block = Block::default()
            .borders(Borders::ALL)
            .title("Library: Artists")
            .border_style(if self.focus == Focus::LibraryChannels {
                ctx.config.as_focused_border_style()
            } else {
                ctx.config.as_border_style()
            });
        let channel_items: Vec<ListItem> = self
            .channels
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut item = ListItem::new(c.as_str());
                if self.selected_channels.contains(&i) {
                    item = item.style(ctx.config.theme.highlighted_item_style.reversed());
                }
                item
            })
            .collect();
        let channel_list = List::new(channel_items)
            .block(channels_block)
            .highlight_style(ctx.config.theme.highlighted_item_style);
        frame.render_stateful_widget(channel_list, columns[1], &mut self.channel_list_state);

        // --- Column 3: Library Videos / Preview ---
        match self.library_video_focus {
            LibraryVideoFocus::List => {
                let videos_block = Block::default()
                    .borders(Borders::ALL)
                    .title("Library: Tracks")
                    .border_style(if self.focus == Focus::LibraryVideos {
                        ctx.config.as_focused_border_style()
                    } else {
                        ctx.config.as_border_style()
                    });
                let video_items: Vec<ListItem> = self
                    .get_selected_channel()
                    .and_then(|c| self.videos_by_channel.get(c))
                    .map(|videos| {
                        videos
                            .iter()
                            .enumerate()
                            .map(|(i, v)| {
                                let mut item = ListItem::new(v.title.as_str());
                                let is_selected = self
                                    .get_selected_channel()
                                    .and_then(|c| self.selected_videos.get(c))
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
                let video_list = List::new(video_items)
                    .block(videos_block)
                    .highlight_style(ctx.config.theme.highlighted_item_style);
                frame.render_stateful_widget(video_list, columns[2], &mut self.video_list_state);
            }
            LibraryVideoFocus::Preview => {
                let preview_block = Block::default()
                    .borders(Borders::ALL)
                    .title("Preview")
                    .border_style(ctx.config.as_focused_border_style());

                let preview_data = self.get_selected_video().map(|video| {
                    let key_style = ctx.config.theme.preview_label_style;
                    let group_style = ctx.config.theme.preview_metadata_group_style;
                    video.to_song_for_preview().to_preview(key_style, group_style)
                });

                if let Some(data) = preview_data {
                    render_preview_data(frame, columns[2], &data, preview_block);
                } else {
                    let preview_widget = Paragraph::new("No video selected")
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
            Focus::LibraryChannels => self.handle_library_channels_action(event, ctx)?,
            Focus::LibraryVideos => self.handle_library_videos_action(event, ctx)?,
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
                } else if self.library_channels_area.contains(pos) {
                    self.focus = Focus::LibraryChannels;
                } else if self.library_videos_area.contains(pos) {
                    self.focus = Focus::LibraryVideos;
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
                if self.library_videos_area.contains(pos) {
                    self.focus = Focus::LibraryVideos;
                    if let Some(channel_name) = self.get_selected_channel().map(|c| c.to_owned()) {
                        let clicked_row = pos.y.saturating_sub(self.library_videos_area.y + 1);
                        let clicked_idx = self.video_list_state.offset() + clicked_row as usize;

                        let selections = self.selected_videos.entry(channel_name.clone()).or_default();
                        if !selections.contains(&clicked_idx) {
                            selections.clear();
                        }
                        selections.insert(clicked_idx);
                        self.video_list_state.select(Some(clicked_idx));

                        let videos = self.videos_by_channel.get(&channel_name).unwrap();
                        let selected_videos: Vec<_> = selections.iter().filter_map(|i| videos.get(*i).cloned()).collect();

                        self.show_youtube_context_menu(selected_videos, ctx)?;
                    }
                } else if self.library_channels_area.contains(pos) {
                    self.focus = Focus::LibraryChannels;
                    let clicked_row = pos.y.saturating_sub(self.library_channels_area.y + 1);
                    let clicked_idx = self.channel_list_state.offset() + clicked_row as usize;

                    if !self.selected_channels.contains(&clicked_idx) {
                        self.selected_channels.clear();
                        self.selected_videos.clear();
                    }
                    self.selected_channels.insert(clicked_idx);
                    self.channel_list_state.select(Some(clicked_idx));

                    let selected_videos: Vec<_> = self.selected_channels.iter()
                        .filter_map(|i| self.channels.get(*i))
                        .filter_map(|c| self.videos_by_channel.get(c))
                        .flatten()
                        .cloned()
                        .collect();
                    
                    self.show_youtube_context_menu(selected_videos, ctx)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
