use anyhow::Result;
use enum_map::{Enum, EnumMap, enum_map};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use itertools::Itertools;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use regex::Regex;
use std::collections::HashSet;
use std::sync::Arc;

use crate::{
    config::keys::CommonAction,
    ctx::Ctx,
    mpd::mpd_client::MpdClient,
    shared::{
        keys::ActionEvent,
        macros::{modal, status_error, status_info},
        mouse_event::{MouseEvent, MouseEventKind},
        mpd_client_ext::MpdClientExt,
    },
    ui::{
        UiEvent,
        input::{BufferId, InputResultEvent},
        modals::confirm_modal::{Action, ConfirmModal},
        panes::Pane,
    },
    ui::{
        modals::input_modal::InputModal, modals::menu::add_to_playlist_or_show_modal,
        modals::menu::modal::MenuModal, modals::select_modal::SelectModal,
    },
    youtube::events::YouTubeEvent,
    youtube::models::{FilterMode, SearchResult, YouTubeId, YouTubeTrack},
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SelectableItem {
    SearchResult(YouTubeId),
    Artist(String),
    Track(YouTubeId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum YoutubeFocus {
    Input,
    SearchTable,
    LibraryArtists,
    LibraryTracks,
}

#[derive(Debug, Enum)]
enum Areas {
    SearchInput,
    SearchTable,
    Artists,
    Tracks,
    Preview,
}

pub struct YoutubePane {
    search_results: Vec<SearchResult>,
    filtered_results: Vec<SearchResult>,
    library_artists: Arc<[String]>,
    library_tracks: Arc<[YouTubeTrack]>,
    all_library_tracks: Arc<[YouTubeTrack]>, // All tracks in library for selection logic
    tracks_cache: std::collections::HashMap<String, Arc<[YouTubeTrack]>>,

    search_list_state: ListState,
    artists_list_state: ListState,
    tracks_list_state: ListState,

    marked_items: HashSet<SelectableItem>,

    buffer_id: BufferId,
    areas: EnumMap<Areas, Rect>,

    filter_mode: FilterMode,
    matcher: SkimMatcherV2,
    focus: YoutubeFocus,
    is_active_pane: bool,
    last_submitted_query: String,
    preview_visible: bool,
    import_status: Option<String>,
    import_progress: (usize, usize),
    compiled_filter_regex: Option<Regex>,
    compiled_highlight_regex: Option<Regex>,
}

impl std::fmt::Debug for YoutubePane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YoutubePane")
            .field("search_results_count", &self.search_results.len())
            .field("filtered_results_count", &self.filtered_results.len())
            .field("library_artists_count", &self.library_artists.len())
            .field("library_tracks_count", &self.library_tracks.len())
            .field("focus", &self.focus)
            .field("is_active_pane", &self.is_active_pane)
            .finish_non_exhaustive()
    }
}

fn move_list_selection(state: &mut ListState, count: usize, delta: isize) {
    if count == 0 {
        return;
    }
    let current = state.selected().unwrap_or(0);
    let next = if delta < 0 {
        let abs_delta = delta.unsigned_abs();
        if current == 0 {
            count.saturating_sub(1)
        } else {
            current.saturating_sub(abs_delta)
        }
    } else {
        let abs_delta = delta.unsigned_abs();
        if current >= count.saturating_sub(1) {
            0
        } else {
            current.saturating_add(abs_delta)
        }
    };
    state.select(Some(next));
}

fn make_list_item(text: String, is_marked: bool, ctx: &Ctx) -> ListItem<'_> {
    let mut spans = Vec::new();
    if is_marked {
        spans.push(ratatui::text::Span::styled(
            &ctx.config.theme.symbols.marker,
            ctx.config.theme.highlighted_item_style,
        ));
    } else {
        spans.push(ratatui::text::Span::from(" ".repeat(ctx.config.theme.symbols.marker.chars().count())));
    }
    spans.push(ratatui::text::Span::raw(text));
    ListItem::new(Line::from(spans))
}

impl YoutubePane {
    pub fn new(ctx: &Ctx) -> Self {
        let buffer_id = BufferId::new();
        ctx.input.create_buffer(buffer_id, None);
        Self {
            search_results: Vec::new(),
            filtered_results: Vec::new(),
            library_artists: Arc::from([]),
            library_tracks: Arc::from([]),
            all_library_tracks: Arc::from([]),
            tracks_cache: std::collections::HashMap::new(),

            search_list_state: ListState::default(),
            artists_list_state: ListState::default(),
            tracks_list_state: ListState::default(),

            marked_items: HashSet::new(),

            buffer_id,
            areas: enum_map! { _ => Rect::default() },

            filter_mode: FilterMode::default(),
            matcher: SkimMatcherV2::default(),
            focus: YoutubeFocus::SearchTable,
            is_active_pane: false,
            last_submitted_query: String::new(),
            preview_visible: false,
            import_status: None,
            import_progress: (0, 0),
            compiled_filter_regex: None,
            compiled_highlight_regex: None,
        }
    }

    fn cycle_filter_mode(&mut self, ctx: &Ctx, forward: bool) {
        self.filter_mode = match (forward, self.filter_mode) {
            (true, FilterMode::Fuzzy) => FilterMode::Contains,
            (true, FilterMode::Contains) => FilterMode::StartsWith,
            (true, FilterMode::StartsWith) => FilterMode::Exact,
            (true, FilterMode::Exact) => FilterMode::Regex,
            (true, FilterMode::Regex) => FilterMode::Fuzzy,
            (false, FilterMode::Fuzzy) => FilterMode::Regex,
            (false, FilterMode::Contains) => FilterMode::Fuzzy,
            (false, FilterMode::StartsWith) => FilterMode::Contains,
            (false, FilterMode::Exact) => FilterMode::StartsWith,
            (false, FilterMode::Regex) => FilterMode::Exact,
        };
        self.update_filter(ctx);
    }

    fn update_filter(&mut self, ctx: &Ctx) {
        let query = ctx.input.value(self.buffer_id);
        if query == self.last_submitted_query || query.is_empty() {
            self.filtered_results = self.search_results.clone();
            self.compiled_filter_regex = None;
            self.compiled_highlight_regex = None;
        } else {
            if self.filter_mode == FilterMode::Regex {
                self.compiled_filter_regex = Regex::new(&query).ok();
                self.compiled_highlight_regex = Regex::new(&format!("(?i){query}")).ok();
            } else if self.filter_mode == FilterMode::Fuzzy {
                self.compiled_filter_regex = None;
                self.compiled_highlight_regex = None;
            } else {
                self.compiled_filter_regex = None;
                let pattern = match self.filter_mode {
                    FilterMode::Contains => {
                        format!(
                            "(?i){}",
                            crate::shared::string_util::StringExt::escape_regex_chars(&query)
                        )
                    }
                    FilterMode::StartsWith => {
                        format!(
                            "(?i)^{}",
                            crate::shared::string_util::StringExt::escape_regex_chars(&query)
                        )
                    }
                    FilterMode::Exact => {
                        format!(
                            "(?i)^{}$",
                            crate::shared::string_util::StringExt::escape_regex_chars(&query)
                        )
                    }
                    FilterMode::Regex | FilterMode::Fuzzy => unreachable!(),
                };
                self.compiled_highlight_regex = Regex::new(&pattern).ok();
            }

            self.filtered_results = self
                .search_results
                .iter()
                .filter(|res| {
                    let text = format!("{} - {}", res.title, res.creator);
                    match self.filter_mode {
                        FilterMode::Fuzzy => self.matcher.fuzzy_match(&text, &query).is_some(),
                        FilterMode::Contains => text.to_lowercase().contains(&query.to_lowercase()),
                        FilterMode::StartsWith => {
                            text.to_lowercase().starts_with(&query.to_lowercase())
                        }
                        FilterMode::Exact => text.eq_ignore_ascii_case(&query),
                        FilterMode::Regex => {
                            self.compiled_filter_regex.as_ref().is_none_or(|re| re.is_match(&text))
                        }
                    }
                })
                .cloned()
                .collect();
        }

        if let Some(selected) = self.search_list_state.selected() {
            if selected >= self.filtered_results.len() {
                self.search_list_state.select(if self.filtered_results.is_empty() {
                    None
                } else {
                    Some(0)
                });
            }
        } else if !self.filtered_results.is_empty() {
            self.search_list_state.select(Some(0));
        }
    }

    fn refresh_library(&self, ctx: &Ctx) {
        let sender = ctx.youtube_manager.sender();
        smol::spawn(async move {
            let _ = sender.send(YouTubeEvent::GetLibraryArtists).await;
        })
        .detach();
    }

    fn update_artist_tracks(&mut self, ctx: &Ctx, artist: String) {
        if let Some(tracks) = self.tracks_cache.get(&artist) {
            self.library_tracks = Arc::clone(tracks);
            let _ = ctx.render();
            return;
        }

        let sender = ctx.youtube_manager.sender();
        smol::spawn(async move {
            let _ = sender.send(YouTubeEvent::GetLibraryTracks(artist)).await;
        })
        .detach();
    }

    fn fetch_all_library_tracks(&self, ctx: &Ctx) {
        let sender = ctx.youtube_manager.sender();
        smol::spawn(async move {
            let _ = sender.send(YouTubeEvent::GetLibraryTracksAll).await;
        })
        .detach();
    }

    fn has_marked_search_results(&self) -> bool {
        self.marked_items.iter().any(|item| matches!(item, SelectableItem::SearchResult(_)))
    }

    fn has_marked_artists(&self) -> bool {
        self.marked_items.iter().any(|item| matches!(item, SelectableItem::Artist(_)))
    }

    fn has_marked_tracks(&self) -> bool {
        self.marked_items.iter().any(|item| matches!(item, SelectableItem::Track(_)))
    }

    fn collect_marked_search_tracks(&self) -> Vec<YouTubeTrack> {
        self.filtered_results
            .iter()
            .filter(|r| {
                self.marked_items.contains(&SelectableItem::SearchResult(r.youtube_id.clone()))
            })
            .map(|r| YouTubeTrack::from(r.clone()))
            .collect()
    }

    fn collect_tracks_for_marked_artists(&self) -> Vec<YouTubeTrack> {
        let marked_artists: HashSet<_> = self
            .marked_items
            .iter()
            .filter_map(|item| match item {
                SelectableItem::Artist(name) => Some(name),
                _ => None,
            })
            .collect();

        self.all_library_tracks
            .iter()
            .filter(|t| marked_artists.contains(&t.artist))
            .cloned()
            .collect()
    }

    fn collect_marked_tracks(&self) -> Vec<YouTubeTrack> {
        self.all_library_tracks
            .iter()
            .filter(|t| self.marked_items.contains(&SelectableItem::Track(t.youtube_id.clone())))
            .cloned()
            .collect()
    }

    fn current_search_selection(&self) -> Option<YouTubeTrack> {
        self.search_list_state
            .selected()
            .and_then(|idx| self.filtered_results.get(idx).map(|r| r.clone().into()))
    }

    fn current_track_selection(&self) -> Option<YouTubeTrack> {
        self.tracks_list_state.selected().and_then(|idx| self.library_tracks.get(idx).cloned())
    }

    fn is_in_queue(&self, track: &YouTubeTrack, ctx: &Ctx) -> bool {
        ctx.queue.iter().any(|s| s.youtube_id().as_ref() == Some(&track.youtube_id))
    }

    fn add_tracks_to_queue(&self, tracks: Vec<YouTubeTrack>, ctx: &Ctx) {
        if tracks.is_empty() {
            return;
        }
        let sender = ctx.youtube_manager.sender();
        let tracks = Arc::from(tracks);
        smol::spawn(async move {
            let _ = sender.send(YouTubeEvent::AddManyToQueue(tracks)).await;
        })
        .detach();
    }

    fn clear_marked_search_results(&mut self) {
        self.marked_items.retain(|item| !matches!(item, SelectableItem::SearchResult(_)));
    }

    fn clear_marked_artists(&mut self) {
        self.marked_items.retain(|item| !matches!(item, SelectableItem::Artist(_)));
    }

    fn clear_marked_tracks(&mut self) {
        self.marked_items.retain(|item| !matches!(item, SelectableItem::Track(_)));
    }

    fn open_context_menu(&mut self, ctx: &Ctx) {
        let sender = ctx.youtube_manager.sender();
        let focused_tracks = match self.focus {
            YoutubeFocus::SearchTable => {
                let marked: Vec<_> = self
                    .filtered_results
                    .iter()
                    .filter(|r| {
                        self.marked_items
                            .contains(&SelectableItem::SearchResult(r.youtube_id.clone()))
                    })
                    .map(|r| YouTubeTrack::from(r.clone()))
                    .collect();

                if marked.is_empty() {
                    self.search_list_state
                        .selected()
                        .and_then(|idx| self.filtered_results.get(idx))
                        .map(|r| vec![YouTubeTrack::from(r.clone())])
                        .unwrap_or_default()
                } else {
                    marked
                }
            }
            YoutubeFocus::LibraryArtists => {
                let marked: Vec<_> = self
                    .all_library_tracks
                    .iter()
                    .filter(|t| {
                        self.marked_items.contains(&SelectableItem::Artist(t.artist.clone()))
                    })
                    .cloned()
                    .collect();

                if marked.is_empty() { self.library_tracks.to_vec() } else { marked }
            }
            YoutubeFocus::LibraryTracks => {
                let marked: Vec<_> = self
                    .all_library_tracks
                    .iter()
                    .filter(|t| {
                        self.marked_items.contains(&SelectableItem::Track(t.youtube_id.clone()))
                    })
                    .cloned()
                    .collect();

                if marked.is_empty() {
                    self.tracks_list_state
                        .selected()
                        .and_then(|idx| self.library_tracks.get(idx).cloned())
                        .map(|t| vec![t])
                        .unwrap_or_default()
                } else {
                    marked
                }
            }
            YoutubeFocus::Input => Vec::new(),
        };

        if focused_tracks.is_empty() {
            return;
        }

        let tracks_clone = focused_tracks.clone();
        let sender_clone = sender.clone();
        let queue_ids: std::collections::HashSet<YouTubeId> =
            ctx.queue.iter().filter_map(|s| s.youtube_id()).collect();

        let modal = MenuModal::new(ctx)
            .list_section(ctx, |mut section| {
                let tracks = tracks_clone.clone();
                let s = sender_clone.clone();
                let queue_ids = queue_ids.clone();
                section.add_item("Add to queue", move |_ctx| {
                    let s = s.clone();
                    let tracks = tracks
                        .iter()
                        .filter(|t| !queue_ids.contains(&t.youtube_id))
                        .cloned()
                        .collect_vec();

                    if tracks.is_empty() {
                        status_info!("All selected tracks are already in queue.");
                        return Ok(());
                    }

                    let tracks = Arc::from(tracks);
                    smol::spawn(async move {
                        let _ = s.send(YouTubeEvent::AddManyToQueue(tracks)).await;
                    })
                    .detach();
                    Ok(())
                });
                let tracks = tracks_clone.clone();
                let s = sender_clone.clone();
                section.add_item("Replace queue", move |_ctx| {
                    let s = s.clone();
                    let tracks: Arc<[YouTubeTrack]> = Arc::from(tracks.clone());
                    smol::spawn(async move {
                        let _ = s.send(YouTubeEvent::ReplaceQueue(tracks)).await;
                    })
                    .detach();
                    Ok(())
                });
                Some(section)
            })
            .list_section(ctx, |mut section| {
                let tracks = tracks_clone.clone();
                let s = sender_clone.clone();
                section.add_item("Add to playlist...", move |ctx| {
                    let tracks: Arc<[YouTubeTrack]> = Arc::from(tracks.clone());
                    let s = s.clone();
                    let playlists_res = ctx.query_sync(|client| {
                        Ok(client.list_playlists()?.into_iter().map(|p| p.name).collect_vec())
                    });
                    match playlists_res {
                        Ok(playlists) => {
                            modal!(
                                ctx,
                                SelectModal::builder()
                                    .ctx(ctx)
                                    .options(playlists)
                                    .title("Select Playlist")
                                    .on_confirm(move |_ctx, name, _idx| {
                                        let s = s.clone();
                                        let tracks = tracks.clone();
                                        let playlist = name.to_string();
                                        smol::spawn(async move {
                                            let _ = s
                                                .send(YouTubeEvent::AddToPlaylist {
                                                    playlist,
                                                    tracks,
                                                })
                                                .await;
                                        })
                                        .detach();
                                        Ok(())
                                    })
                                    .build()
                            );
                        }
                        Err(e) => status_error!("Failed to get playlists: {}", e),
                    }
                    Ok(())
                });
                let tracks = tracks_clone.clone();
                let s = sender_clone.clone();
                section.add_item("Create playlist from selection...", move |ctx| {
                    let tracks: Arc<[YouTubeTrack]> = Arc::from(tracks.clone());
                    let s = s.clone();
                    modal!(
                        ctx,
                        InputModal::new(ctx)
                            .title("Create Playlist")
                            .input_label("Name:")
                            .on_confirm(move |_ctx, name| {
                                let name = name.to_string();
                                let s = s.clone();
                                let tracks = tracks.clone();
                                smol::spawn(async move {
                                    let _ =
                                        s.send(YouTubeEvent::CreatePlaylist { name, tracks }).await;
                                })
                                .detach();
                                Ok(())
                            })
                    );
                    Ok(())
                });
                Some(section)
            })
            .list_section(ctx, |mut section| {
                let tracks = tracks_clone.clone();
                let s = sender_clone.clone();
                section.add_item("Remove from library", move |ctx| {
                    let tracks = tracks.clone();
                    let s = s.clone();
                    modal!(
                        ctx,
                        crate::ui::modals::confirm_modal::ConfirmModal::builder()
                            .ctx(ctx)
                            .message(vec![format!("Remove {} tracks from library?", tracks.len())])
                            .action(crate::ui::modals::confirm_modal::Action::Single {
                                on_confirm: Box::new(move |_ctx| {
                                    for track in tracks {
                                        let s = s.clone();
                                        let id = track.youtube_id.clone().to_string();
                                        smol::spawn(async move {
                                            let _ =
                                                s.send(YouTubeEvent::RemoveFromLibrary(id)).await;
                                        })
                                        .detach();
                                    }
                                    Ok(())
                                }),
                                confirm_label: Some("Remove"),
                                cancel_label: None,
                            })
                            .build()
                    );
                    Ok(())
                });
                let tracks = tracks_clone.clone();
                section.add_item("Download", move |ctx| {
                    let songs: Vec<crate::mpd::commands::Song> =
                        tracks.clone().into_iter().map(crate::mpd::commands::Song::from).collect();
                    modal!(ctx, crate::ui::modals::menu::create_download_modal(&songs, ctx));
                    Ok(())
                });
                Some(section)
            })
            .list_section(ctx, |mut section| {
                section.add_item("Cancel", |_ctx| Ok(()));
                Some(section)
            })
            .build();

        modal!(ctx, modal);
    }

    fn handle_confirm(&mut self, ctx: &mut Ctx) {
        match self.focus {
            YoutubeFocus::Input => self.confirm_search(ctx),
            YoutubeFocus::SearchTable => self.confirm_search_selection(ctx),
            YoutubeFocus::LibraryArtists => self.confirm_artist_selection(ctx),
            YoutubeFocus::LibraryTracks => self.confirm_track_selection(ctx),
        }
    }

    fn confirm_search(&mut self, ctx: &mut Ctx) {
        let sender = ctx.youtube_manager.sender();
        let query = ctx.input.value(self.buffer_id).clone();
        self.last_submitted_query.clone_from(&query);
        let filter_mode = self.filter_mode;
        let limit = ctx.config.youtube.search_results_limit;
        smol::spawn(async move {
            let _ = sender.send(YouTubeEvent::SearchRequest { query, filter_mode, limit }).await;
        })
        .detach();
        self.focus = YoutubeFocus::SearchTable;
    }

    fn process_track_selection(&mut self, ctx: &Ctx, tracks: Vec<YouTubeTrack>) {
        if tracks.is_empty() {
            return;
        }

        let new_tracks: Vec<_> =
            tracks.into_iter().filter(|t| !self.is_in_queue(t, ctx)).collect();

        if new_tracks.is_empty() {
            status_info!("All selected tracks already in queue");
            return;
        }

        self.add_tracks_to_queue(new_tracks, ctx);
    }

    fn confirm_search_selection(&mut self, ctx: &mut Ctx) {
        let tracks = if self.has_marked_search_results() {
            self.collect_marked_search_tracks()
        } else {
            self.current_search_selection().map(|t| vec![t]).unwrap_or_default()
        };

        self.process_track_selection(ctx, tracks);
        self.clear_marked_search_results();
    }

    fn confirm_artist_selection(&mut self, ctx: &mut Ctx) {
        let tracks = if self.has_marked_artists() {
            self.collect_tracks_for_marked_artists()
        } else {
            self.library_tracks.to_vec()
        };

        self.process_track_selection(ctx, tracks);
        self.clear_marked_artists();
    }

    fn confirm_track_selection(&mut self, ctx: &mut Ctx) {
        let tracks = if self.has_marked_tracks() {
            self.collect_marked_tracks()
        } else {
            self.current_track_selection().map(|t| vec![t]).unwrap_or_default()
        };

        self.process_track_selection(ctx, tracks);
        self.clear_marked_tracks();
    }

    fn handle_up(&mut self, ctx: &Ctx) -> Result<()> {
        match self.focus {
            YoutubeFocus::SearchTable => {
                move_list_selection(&mut self.search_list_state, self.filtered_results.len(), -1);
            }
            YoutubeFocus::LibraryArtists => {
                move_list_selection(&mut self.artists_list_state, self.library_artists.len(), -1);
                if let Some(idx) = self.artists_list_state.selected() {
                    if let Some(artist) = self.library_artists.get(idx) {
                        self.update_artist_tracks(ctx, artist.clone());
                    }
                }
            }
            YoutubeFocus::LibraryTracks => {
                move_list_selection(&mut self.tracks_list_state, self.library_tracks.len(), -1);
            }
            YoutubeFocus::Input => {}
        }
        ctx.render()?;
        Ok(())
    }

    fn handle_down(&mut self, ctx: &Ctx) -> Result<()> {
        match self.focus {
            YoutubeFocus::Input => {
                self.focus = YoutubeFocus::SearchTable;
                ctx.input.normal_mode();
                if !self.filtered_results.is_empty() {
                    self.search_list_state.select(Some(0));
                }
            }
            YoutubeFocus::SearchTable => {
                move_list_selection(&mut self.search_list_state, self.filtered_results.len(), 1);
            }
            YoutubeFocus::LibraryArtists => {
                move_list_selection(&mut self.artists_list_state, self.library_artists.len(), 1);
                if let Some(idx) = self.artists_list_state.selected() {
                    if let Some(artist) = self.library_artists.get(idx) {
                        self.update_artist_tracks(ctx, artist.clone());
                    }
                }
            }
            YoutubeFocus::LibraryTracks => {
                move_list_selection(&mut self.tracks_list_state, self.library_tracks.len(), 1);
            }
        }
        ctx.render()?;
        Ok(())
    }

    fn handle_right(&mut self, event: &mut ActionEvent, ctx: &Ctx) -> Result<()> {
        match self.focus {
            YoutubeFocus::SearchTable => self.focus = YoutubeFocus::LibraryArtists,
            YoutubeFocus::LibraryArtists => self.focus = YoutubeFocus::LibraryTracks,
            YoutubeFocus::LibraryTracks => {
                if !self.library_tracks.is_empty() {
                    self.preview_visible = true;
                }
            }
            YoutubeFocus::Input => {
                event.abandon();
            }
        }
        ctx.render()?;
        Ok(())
    }

    fn handle_left(&mut self, event: &mut ActionEvent, ctx: &Ctx) -> Result<()> {
        match self.focus {
            YoutubeFocus::LibraryTracks => {
                if self.preview_visible {
                    self.preview_visible = false;
                } else {
                    self.focus = YoutubeFocus::LibraryArtists;
                }
            }
            YoutubeFocus::LibraryArtists => self.focus = YoutubeFocus::SearchTable,
            YoutubeFocus::Input | YoutubeFocus::SearchTable => {
                event.abandon();
            }
        }
        ctx.render()?;
        Ok(())
    }

    fn handle_select(&mut self, event: &mut ActionEvent, ctx: &Ctx) -> Result<()> {
        match self.focus {
            YoutubeFocus::SearchTable => {
                if let Some(i) = self.search_list_state.selected() {
                    if let Some(res) = self.filtered_results.get(i) {
                        let id = SelectableItem::SearchResult(res.youtube_id.clone());
                        if self.marked_items.contains(&id) {
                            self.marked_items.remove(&id);
                        } else {
                            self.marked_items.insert(id);
                        }
                    }
                    move_list_selection(&mut self.search_list_state, self.filtered_results.len(), 1);
                }
            }
            YoutubeFocus::LibraryArtists => {
                if let Some(i) = self.artists_list_state.selected() {
                    if let Some(artist_name) = self.library_artists.get(i) {
                        let artist_item = SelectableItem::Artist(artist_name.clone());
                        let artist = artist_name.clone();

                        if self.marked_items.contains(&artist_item) {
                            self.marked_items.remove(&artist_item);
                            // Deselect all tracks by this artist
                            let tracks_to_remove: Vec<SelectableItem> = self
                                .all_library_tracks
                                .iter()
                                .filter(|t| t.artist == artist)
                                .map(|t| SelectableItem::Track(t.youtube_id.clone()))
                                .collect();
                            for item in tracks_to_remove {
                                self.marked_items.remove(&item);
                            }
                        } else {
                            self.marked_items.insert(artist_item);
                            // Select all tracks by this artist
                            let tracks_to_add: Vec<SelectableItem> = self
                                .all_library_tracks
                                .iter()
                                .filter(|t| t.artist == artist)
                                .map(|t| SelectableItem::Track(t.youtube_id.clone()))
                                .collect();
                            for item in tracks_to_add {
                                self.marked_items.insert(item);
                            }
                        }
                    }
                    
                    move_list_selection(&mut self.artists_list_state, self.library_artists.len(), 1);
                    if let Some(next) = self.artists_list_state.selected() {
                        if let Some(artist) = self.library_artists.get(next) {
                            self.update_artist_tracks(ctx, artist.clone());
                        }
                    }
                }
            }
            YoutubeFocus::LibraryTracks => {
                if let Some(i) = self.tracks_list_state.selected() {
                    if let Some(track) = self.library_tracks.get(i) {
                        let track_item = SelectableItem::Track(track.youtube_id.clone());
                        let artist_name = track.artist.clone();
                        let artist_item = SelectableItem::Artist(artist_name.clone());

                        if self.marked_items.contains(&track_item) {
                            self.marked_items.remove(&track_item);
                            // Deselect artist since not all tracks are selected anymore
                            self.marked_items.remove(&artist_item);
                        } else {
                            self.marked_items.insert(track_item);
                            // Check if all tracks of this artist are now selected
                            let all_artist_tracks: Vec<&YouTubeTrack> = self
                                .all_library_tracks
                                .iter()
                                .filter(|t| t.artist == artist_name)
                                .collect();
                            let all_selected = all_artist_tracks.iter().all(|t| {
                                self.marked_items
                                    .contains(&SelectableItem::Track(t.youtube_id.clone()))
                            });
                            if all_selected {
                                self.marked_items.insert(artist_item);
                            }
                        }
                    }
                    move_list_selection(&mut self.tracks_list_state, self.library_tracks.len(), 1);
                }
            }
            YoutubeFocus::Input => {
                event.abandon();
            }
        }
        ctx.render()?;
        Ok(())
    }

    fn render_search_input(&mut self, frame: &mut Frame, ctx: &Ctx) {
        let is_insert_mode = ctx.input.is_active(self.buffer_id);
        let is_really_focused = self.is_active_pane || is_insert_mode;

        let search_border_style =
            if is_really_focused && (self.focus == YoutubeFocus::Input || is_insert_mode) {
                Style::default().fg(Color::White)
            } else {
                ctx.config.as_border_style()
            };

        let mode_label = format!("[{mode_label:?}]", mode_label = self.filter_mode);
        let title = if let Some(status) = &self.import_status {
            format!(" Search YouTube {mode_label} - {status} ")
        } else {
            format!(" Search YouTube {mode_label} (Press '/' to type, CTRL(+ALT)+f to cycle mode) ")
        };

        let search_block = Block::default()
            .borders(Borders::ALL)
            .border_style(search_border_style)
            .title(title);

        let area = self.areas[Areas::SearchInput];
        let inner_search_area = search_block.inner(area);
        frame.render_widget(search_block, area);

        let spans = ctx.input.as_spans(
            self.buffer_id,
            inner_search_area.width,
            ctx.config.as_text_style(),
            is_insert_mode,
        );
        frame.render_widget(Paragraph::new(Line::from(spans)), inner_search_area);
    }

    fn render_search_results(&mut self, frame: &mut Frame, ctx: &Ctx) {
        let is_insert_mode = ctx.input.is_active(self.buffer_id);
        let is_really_focused = self.is_active_pane || is_insert_mode;

        let search_results_border_style =
            if is_really_focused && self.focus == YoutubeFocus::SearchTable && !is_insert_mode {
                Style::default().fg(Color::White)
            } else {
                ctx.config.as_border_style()
            };

        let search_query = ctx.input.value(self.buffer_id);
        let highlight_style = ctx.config.theme.highlighted_item_style.add_modifier(Modifier::BOLD);

        let items = self.filtered_results.iter().map(|res| {
            let duration = if res.duration.as_secs() > 0 {
                format!(
                    " [{}]",
                    crate::shared::ext::duration::DurationExt::to_string(&res.duration)
                )
            } else {
                String::new()
            };
            let text = format!("{} - {}{}", res.title, res.creator, duration);
            let mut spans = Vec::with_capacity(10);

            let is_marked =
                self.marked_items.contains(&SelectableItem::SearchResult(res.youtube_id.clone()));

            if is_marked {
                spans.push(ratatui::text::Span::styled(
                    &ctx.config.theme.symbols.marker,
                    ctx.config.theme.highlighted_item_style,
                ));
            } else {
                spans.push(ratatui::text::Span::from(" ".repeat(ctx.config.theme.symbols.marker.chars().count())));
            }

            if !search_query.is_empty() && search_query != self.last_submitted_query {
                if self.filter_mode == FilterMode::Fuzzy {
                    if let Some((_, indices)) = self.matcher.fuzzy_indices(&text, &search_query) {
                        let mut last_end = 0;
                        let mut indices_iter = indices.iter().peekable();

                        for (idx, c) in text.char_indices() {
                            if let Some(&&match_idx) = indices_iter.peek() {
                                if idx == match_idx {
                                    if idx > last_end {
                                        spans.push(ratatui::text::Span::raw(
                                            text[last_end..idx].to_string(),
                                        ));
                                    }
                                    spans.push(ratatui::text::Span::styled(
                                        c.to_string(),
                                        highlight_style,
                                    ));
                                    last_end = idx + c.len_utf8();
                                    indices_iter.next();
                                }
                            }
                        }
                        if last_end < text.len() {
                            spans.push(ratatui::text::Span::raw(text[last_end..].to_string()));
                        }
                    } else {
                        spans.push(ratatui::text::Span::raw(text));
                    }
                } else if let Some(re) = &self.compiled_highlight_regex {
                    let mut last_end = 0;
                    for m in re.find_iter(&text) {
                        if m.start() > last_end {
                            spans.push(ratatui::text::Span::raw(
                                text[last_end..m.start()].to_string(),
                            ));
                        }
                        spans.push(ratatui::text::Span::styled(
                            m.as_str().to_string(),
                            highlight_style,
                        ));
                        last_end = m.end();
                    }

                    if last_end < text.len() {
                        spans.push(ratatui::text::Span::raw(text[last_end..].to_string()));
                    }
                } else {
                    spans.push(ratatui::text::Span::raw(text));
                }
            } else {
                spans.push(ratatui::text::Span::raw(text));
            }

            ListItem::new(ratatui::text::Line::from(spans))
        });

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(search_results_border_style)
                    .title(" Results "),
            )
            .highlight_style(ctx.config.theme.current_item_style)
            .highlight_symbol("> ");

        frame.render_stateful_widget(
            list,
            self.areas[Areas::SearchTable],
            &mut self.search_list_state,
        );
    }

    fn render_artists(&mut self, frame: &mut Frame, ctx: &Ctx) {
        let is_insert_mode = ctx.input.is_active(self.buffer_id);
        let is_really_focused = self.is_active_pane || is_insert_mode;

        let artists_border_style =
            if is_really_focused && self.focus == YoutubeFocus::LibraryArtists && !is_insert_mode {
                Style::default().fg(Color::White)
            } else {
                ctx.config.as_border_style()
            };

        let artist_items = self.library_artists.iter().map(|a| {
            let is_marked = self.marked_items.contains(&SelectableItem::Artist(a.clone()));
            make_list_item(a.to_string(), is_marked, ctx)
        });

        let artists_list = List::new(artist_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(artists_border_style)
                    .title(" Artists "),
            )
            .highlight_style(ctx.config.theme.current_item_style)
            .highlight_symbol("> ");

        frame.render_stateful_widget(
            artists_list,
            self.areas[Areas::Artists],
            &mut self.artists_list_state,
        );
    }

    fn render_tracks(&mut self, frame: &mut Frame, ctx: &Ctx) {
        if self.preview_visible {
            return;
        }

        let is_insert_mode = ctx.input.is_active(self.buffer_id);
        let is_really_focused = self.is_active_pane || is_insert_mode;

        let tracks_border_style =
            if is_really_focused && self.focus == YoutubeFocus::LibraryTracks && !is_insert_mode {
                Style::default().fg(Color::White)
            } else {
                ctx.config.as_border_style()
            };

        let track_items = self.library_tracks.iter().map(|t| {
            let is_marked =
                self.marked_items.contains(&SelectableItem::Track(t.youtube_id.clone()));
            make_list_item(t.title.to_string(), is_marked, ctx)
        });

        let tracks_list = List::new(track_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(tracks_border_style)
                    .title(" Tracks "),
            )
            .highlight_style(ctx.config.theme.current_item_style)
            .highlight_symbol("> ");

        frame.render_stateful_widget(
            tracks_list,
            self.areas[Areas::Tracks],
            &mut self.tracks_list_state,
        );
    }

    fn render_preview(&mut self, frame: &mut Frame, ctx: &Ctx) {
        if !self.preview_visible {
            return;
        }

        let is_insert_mode = ctx.input.is_active(self.buffer_id);
        let is_really_focused = self.is_active_pane || is_insert_mode;

        let preview_border_style =
            if is_really_focused && self.focus == YoutubeFocus::LibraryTracks && !is_insert_mode {
                Style::default().fg(Color::White)
            } else {
                ctx.config.as_border_style()
            };

        let selected_idx = self.tracks_list_state.selected().unwrap_or(0);
        if let Some(track) = self.library_tracks.get(selected_idx) {
            let mut lines = Vec::new();
            let key_style = ctx.config.theme.preview_label_style;
            let val_style = ctx.config.as_text_style();

            lines.push(Line::from(vec![
                ratatui::text::Span::styled("Title: ", key_style),
                ratatui::text::Span::styled(&track.title, val_style),
            ]));
            lines.push(Line::from(vec![
                ratatui::text::Span::styled("Artist: ", key_style),
                ratatui::text::Span::styled(&track.artist, val_style),
            ]));
            if let Some(album) = &track.album {
                lines.push(Line::from(vec![
                    ratatui::text::Span::styled("Album: ", key_style),
                    ratatui::text::Span::styled(album, val_style),
                ]));
            }
            lines.push(Line::from(vec![
                ratatui::text::Span::styled("Duration: ", key_style),
                ratatui::text::Span::styled(
                    crate::shared::ext::duration::DurationExt::to_string(&track.duration),
                    val_style,
                ),
            ]));
            lines.push(Line::from(vec![
                ratatui::text::Span::styled("YouTube ID: ", key_style),
                ratatui::text::Span::styled(track.youtube_id.as_str(), val_style),
            ]));
            lines.push(Line::from(vec![
                ratatui::text::Span::styled("Link: ", key_style),
                ratatui::text::Span::styled(&track.link, val_style),
            ]));

            let preview_block = Block::default()
                .borders(Borders::ALL)
                .border_style(preview_border_style)
                .title(" Preview ");

            let inner_area = preview_block.inner(self.areas[Areas::Preview]);
            frame.render_widget(preview_block, self.areas[Areas::Preview]);

            let paragraph = Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true });
            frame.render_widget(paragraph, inner_area);
        }
    }

    fn get_clicked_row(&self, area: Rect, y: u16, state: &ListState) -> Option<usize> {
        let clicked_row = y.saturating_sub(area.y + 1) as usize;
        state.offset().checked_add(clicked_row)
    }
}

impl Pane for YoutubePane {
    fn calculate_areas(&mut self, area: Rect, ctx: &Ctx) -> Result<()> {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(area);

        self.areas[Areas::SearchInput] = chunks[0];

        let widths = &ctx.config.youtube.column_widths;
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(widths[0]),
                Constraint::Percentage(widths[1]),
                Constraint::Percentage(widths[2]),
            ])
            .split(chunks[1]);

        self.areas[Areas::SearchTable] = cols[0];
        self.areas[Areas::Artists] = cols[1];
        self.areas[Areas::Tracks] = cols[2];
        self.areas[Areas::Preview] = cols[2];
        Ok(())
    }

    fn before_show(&mut self, ctx: &Ctx) -> Result<()> {
        self.refresh_library(ctx);
        self.fetch_all_library_tracks(ctx);
        Ok(())
    }

    fn render(&mut self, frame: &mut Frame, _area: Rect, ctx: &Ctx) -> Result<()> {
        self.render_search_input(frame, ctx);
        self.render_search_results(frame, ctx);
        self.render_artists(frame, ctx);
        self.render_tracks(frame, ctx);
        self.render_preview(frame, ctx);
        Ok(())
    }

    fn on_hide(&mut self, ctx: &Ctx) -> Result<()> {
        self.is_active_pane = false;
        ctx.input.normal_mode();
        Ok(())
    }

    fn on_event(&mut self, event: &mut UiEvent, is_visible: bool, ctx: &Ctx) -> Result<()> {
        match event {
            UiEvent::TabChanged(_) => {
                if !is_visible {
                    self.is_active_pane = false;
                    ctx.input.normal_mode();
                }
            }
            UiEvent::YouTube(yt_event) => {
                match yt_event {
                    YouTubeEvent::SearchStarted => {
                        self.search_results.clear();
                        self.filtered_results.clear();
                        self.marked_items
                            .retain(|item| !matches!(item, SelectableItem::SearchResult(_)));
                        self.search_list_state.select(None);
                        ctx.render()?;
                    }
                    YouTubeEvent::SearchResult(result) => {
                        self.search_results.push(result.clone());
                        let is_first = self.search_results.len() == 1;
                        self.update_filter(ctx);

                        if is_first && !self.filtered_results.is_empty() {
                            self.search_list_state.select(Some(0));
                            if is_visible && self.is_active_pane {
                                self.focus = YoutubeFocus::SearchTable;
                            }
                        }
                        ctx.render()?;
                    }
                    YouTubeEvent::SearchError(err) => {
                        let _ = ctx.app_event_sender.send(crate::shared::events::AppEvent::Status(
                            format!("YouTube Search Error: {err}"),
                            crate::shared::events::Level::Error,
                            std::time::Duration::from_secs(5),
                        ));
                        ctx.render()?;
                    }
                    YouTubeEvent::LibraryUpdated => {
                        self.tracks_cache.clear();
                        self.refresh_library(ctx);
                        self.fetch_all_library_tracks(ctx);
                    }
                    YouTubeEvent::LibraryArtistsAvailable(artists) => {
                        self.library_artists = Arc::clone(artists);
                        self.marked_items.retain(|item| !matches!(item, SelectableItem::Artist(_)));

                        let selected = self.artists_list_state.selected();
                        if let Some(idx) = selected {
                            if idx >= self.library_artists.len() {
                                self.artists_list_state.select(
                                    if self.library_artists.is_empty() {
                                        None
                                    } else {
                                        Some(self.library_artists.len().saturating_sub(1))
                                    },
                                );
                            }
                        } else if !self.library_artists.is_empty() {
                            self.artists_list_state.select(Some(0));
                        }

                        if let Some(idx) = self.artists_list_state.selected() {
                            if let Some(artist) = self.library_artists.get(idx) {
                                self.update_artist_tracks(ctx, artist.clone());
                            }
                        } else {
                            self.library_tracks = Arc::from([]);
                            self.tracks_list_state.select(None);
                        }
                        ctx.render()?;
                    }
                    YouTubeEvent::LibraryTracksAvailable { artist, tracks } => {
                        self.tracks_cache.insert(artist.clone(), Arc::clone(tracks));

                        // Only update view if it matches currently selected artist
                        let current_artist = self
                            .artists_list_state
                            .selected()
                            .and_then(|idx| self.library_artists.get(idx));

                        if current_artist == Some(artist) {
                            self.library_tracks = Arc::clone(tracks);

                            let selected = self.tracks_list_state.selected();
                            if let Some(idx) = selected {
                                if idx >= self.library_tracks.len() {
                                    self.tracks_list_state.select(
                                        if self.library_tracks.is_empty() {
                                            None
                                        } else {
                                            Some(self.library_tracks.len().saturating_sub(1))
                                        },
                                    );
                                }
                            } else if !self.library_tracks.is_empty() {
                                self.tracks_list_state.select(Some(0));
                            }
                        }
                        ctx.render()?;
                    }
                    YouTubeEvent::AllLibraryTracksAvailable(tracks) => {
                        self.all_library_tracks = Arc::clone(tracks);
                        ctx.render()?;
                    }
                    YouTubeEvent::ImportProgress { current, total, message } => {
                        self.import_status = Some(message.clone());
                        self.import_progress = (*current, *total);
                        ctx.render()?;
                    }
                    YouTubeEvent::ImportFinished { success, failed } => {
                        self.import_status =
                            Some(format!("Import finished: {success} success, {failed} failed"));
                        // Clear after some time or on next search/action
                        ctx.render()?;
                    }
                    YouTubeEvent::MetadataAvailable(_) => {
                        ctx.render()?;
                    }
                    YouTubeEvent::AddToPlaylistResult { playlist, urls } => {
                        add_to_playlist_or_show_modal(
                            playlist.clone(),
                            urls.to_vec(),
                            ctx.config.playlist_duplicate_strategy,
                            ctx,
                        );
                    }
                    YouTubeEvent::CreatePlaylistResult { name, urls } => {
                        let name = name.clone();
                        let urls_vec = urls.to_vec();
                        ctx.command(move |client| {
                            client.create_playlist(&name, urls_vec)?;
                            status_info!("Created playlist '{}'", name);
                            Ok(())
                        });
                    }
                    YouTubeEvent::YouTubeError(err) => {
                        let _ = ctx.app_event_sender.send(crate::shared::events::AppEvent::Status(
                            format!("YouTube Error: {err}"),
                            crate::shared::events::Level::Error,
                            std::time::Duration::from_secs(5),
                        ));
                        ctx.render()?;
                    }
                    _ => {}
                }
            }
            UiEvent::UserKeyInput(key) => {
                if !is_visible {
                    return Ok(());
                }

                if !self.is_active_pane {
                    return Ok(());
                }

                let is_insert_mode = ctx.input.is_active(self.buffer_id);

                if is_insert_mode {
                    if let crossterm::event::KeyCode::Down = key.code {
                        ctx.input.normal_mode();
                        self.focus = YoutubeFocus::SearchTable;
                        if self.search_list_state.selected().is_none()
                            && !self.filtered_results.is_empty()
                        {
                            self.search_list_state.select(Some(0));
                        }
                        ctx.render()?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_mouse_event(&mut self, event: MouseEvent, ctx: &Ctx) -> Result<()> {
        if matches!(event.kind, MouseEventKind::LeftClick | MouseEventKind::RightClick) {
            if self.areas[Areas::SearchInput].contains(event.into()) {
                self.is_active_pane = true;
                self.focus = YoutubeFocus::Input;
                ctx.input.insert_mode(self.buffer_id);
                ctx.render()?;
            } else if self.areas[Areas::SearchTable].contains(event.into()) {
                self.is_active_pane = true;
                self.focus = YoutubeFocus::SearchTable;
                if let Some(idx) = self.get_clicked_row(self.areas[Areas::SearchTable], event.y, &self.search_list_state) {
                    if idx < self.filtered_results.len() {
                        self.search_list_state.select(Some(idx));
                        ctx.render()?;
                    }
                }
                if matches!(event.kind, MouseEventKind::RightClick) {
                    self.open_context_menu(ctx);
                }
            } else if self.areas[Areas::Artists].contains(event.into()) {
                self.is_active_pane = true;
                self.focus = YoutubeFocus::LibraryArtists;
                if let Some(idx) = self.get_clicked_row(self.areas[Areas::Artists], event.y, &self.artists_list_state) {
                    if idx < self.library_artists.len() {
                        self.artists_list_state.select(Some(idx));
                        if let Some(artist) = self.library_artists.get(idx) {
                            self.update_artist_tracks(ctx, artist.clone());
                        }
                        ctx.render()?;
                    }
                }
                if matches!(event.kind, MouseEventKind::RightClick) {
                    self.open_context_menu(ctx);
                }
            } else if self.areas[Areas::Tracks].contains(event.into()) {
                self.is_active_pane = true;
                self.focus = YoutubeFocus::LibraryTracks;
                if let Some(idx) = self.get_clicked_row(self.areas[Areas::Tracks], event.y, &self.tracks_list_state) {
                    if idx < self.library_tracks.len() {
                        self.tracks_list_state.select(Some(idx));
                        ctx.render()?;
                    }
                }
                if matches!(event.kind, MouseEventKind::RightClick) {
                    self.open_context_menu(ctx);
                }
            } else {
                self.is_active_pane = false;
                ctx.render()?;
            }
        }
        Ok(())
    }

    fn handle_insert_mode(&mut self, kind: InputResultEvent, ctx: &mut Ctx) -> Result<()> {
        self.is_active_pane = true;
        match kind {
            InputResultEvent::Confirm => {
                let sender = ctx.youtube_manager.sender();
                let query = ctx.input.value(self.buffer_id).clone();
                self.last_submitted_query.clone_from(&query);
                let filter_mode = self.filter_mode;
                let limit = ctx.config.youtube.search_results_limit;
                smol::spawn(async move {
                    let _ = sender
                        .send(YouTubeEvent::SearchRequest { query, filter_mode, limit })
                        .await;
                })
                .detach();
                ctx.input.normal_mode();
                self.focus = YoutubeFocus::SearchTable;
            }
            InputResultEvent::Cancel => {
                ctx.input.normal_mode();
                self.focus = YoutubeFocus::SearchTable;
                self.update_filter(ctx);
            }
            InputResultEvent::Push | InputResultEvent::Pop => {
                self.update_filter(ctx);
            }
            InputResultEvent::NoChange => {}
        }
        ctx.render()?;
        Ok(())
    }

    fn handle_action(&mut self, event: &mut ActionEvent, ctx: &mut Ctx) -> Result<()> {
        self.is_active_pane = true;
        if let Some(action) = event.claim_youtube() {
            match action {
                crate::config::keys::actions::YoutubeActions::CycleFilterMode { forward } => {
                    self.cycle_filter_mode(ctx, *forward);
                    ctx.render()?;
                }
            }
        } else if let Some(action) = event.claim_common() {
            match action {
                CommonAction::Confirm => self.handle_confirm(ctx),
                CommonAction::EnterSearch => {
                    self.focus = YoutubeFocus::Input;
                    ctx.input.insert_mode(self.buffer_id);
                    ctx.render()?;
                }
                CommonAction::FocusInput => {
                    self.focus = YoutubeFocus::Input;
                    ctx.input.insert_mode(self.buffer_id);
                    ctx.render()?;
                }
                CommonAction::Up => self.handle_up(ctx)?,
                CommonAction::Down => self.handle_down(ctx)?,
                CommonAction::Right => self.handle_right(event, ctx)?,
                CommonAction::Left => self.handle_left(event, ctx)?,
                CommonAction::Select => self.handle_select(event, ctx)?,
                CommonAction::ContextMenu => {
                    self.open_context_menu(ctx);
                }
                CommonAction::AddOptions { .. } => {
                    // Add to library
                    match self.focus {
                        YoutubeFocus::SearchTable => {
                            if let Some(idx) = self.search_list_state.selected() {
                                if let Some(res) = self.filtered_results.get(idx) {
                                    let track: YouTubeTrack = res.clone().into();
                                    let sender = ctx.youtube_manager.sender();
                                    smol::spawn(async move {
                                        let _ =
                                            sender.send(YouTubeEvent::AddToLibrary(track)).await;
                                    })
                                    .detach();
                                }
                            }
                        }
                        YoutubeFocus::Input
                        | YoutubeFocus::LibraryArtists
                        | YoutubeFocus::LibraryTracks => {
                            event.abandon();
                        }
                    }
                }
                CommonAction::Delete => {
                    // Remove from library with confirmation
                    match self.focus {
                        YoutubeFocus::LibraryArtists => {
                            let marked_artists: Vec<String> = self
                                .marked_items
                                .iter()
                                .filter_map(|item| match item {
                                    SelectableItem::Artist(name) => Some(name.clone()),
                                    _ => None,
                                })
                                .collect();

                            if !marked_artists.is_empty() {
                                let sender = ctx.youtube_manager.sender();
                                let count = marked_artists.len();
                                modal!(ctx, ConfirmModal::builder()
                                    .ctx(ctx)
                                    .message(vec![
                                        format!("Are you sure you want to remove ALL tracks by {} selected artists from your library?", count),
                                    ])
                                    .action(Action::Single {
                                        on_confirm: Box::new(move |_ctx| {
                                            for artist in marked_artists {
                                                let sender = sender.clone();
                                                smol::spawn(async move {
                                                    let _ = sender.send(YouTubeEvent::RemoveArtistFromLibrary(artist)).await;
                                                }).detach();
                                            }
                                            Ok(())
                                        }),
                                        confirm_label: Some("Remove All Selected"),
                                        cancel_label: None,
                                    })
                                    .size((60, 6))
                                    .build()
                                );
                                self.marked_items
                                    .retain(|item| !matches!(item, SelectableItem::Artist(_)));
                            } else if let Some(idx) = self.artists_list_state.selected() {
                                let artist = self.library_artists[idx].clone();
                                let sender = ctx.youtube_manager.sender();
                                modal!(ctx, ConfirmModal::builder()
                                    .ctx(ctx)
                                    .message(vec![
                                        format!("Are you sure you want to remove ALL tracks by '{}' from your library?", artist),
                                    ])
                                    .action(Action::Single {
                                        on_confirm: Box::new(move |_ctx| {
                                            let sender = sender.clone();
                                            let artist = artist.clone();
                                            smol::spawn(async move {
                                                let _ = sender.send(YouTubeEvent::RemoveArtistFromLibrary(artist)).await;
                                            }).detach();
                                            Ok(())
                                        }),
                                        confirm_label: Some("Remove All"),
                                        cancel_label: None,
                                    })
                                    .size((60, 6))
                                    .build()
                                );
                            }
                        }
                        YoutubeFocus::LibraryTracks => {
                            let marked_tracks: Vec<YouTubeId> = self
                                .marked_items
                                .iter()
                                .filter_map(|item| match item {
                                    SelectableItem::Track(id) => Some(id.clone()),
                                    _ => None,
                                })
                                .collect();

                            if !marked_tracks.is_empty() {
                                let sender = ctx.youtube_manager.sender();
                                let count = marked_tracks.len();
                                modal!(ctx, ConfirmModal::builder()
                                    .ctx(ctx)
                                    .message(vec![
                                        format!("Are you sure you want to remove {} selected tracks from your library?", count),
                                    ])
                                    .action(Action::Single {
                                        on_confirm: Box::new(move |_ctx| {
                                            for id in marked_tracks {
                                                let sender = sender.clone();
                                                let id_str = id.to_string();
                                                smol::spawn(async move {
                                                    let _ = sender.send(YouTubeEvent::RemoveFromLibrary(id_str)).await;
                                                }).detach();
                                            }
                                            Ok(())
                                        }),
                                        confirm_label: Some("Remove Selected"),
                                        cancel_label: None,
                                    })
                                    .size((60, 6))
                                    .build()
                                );
                                self.marked_items
                                    .retain(|item| !matches!(item, SelectableItem::Track(_)));
                            } else if let Some(idx) = self.tracks_list_state.selected() {
                                if let Some(track) = self.library_tracks.get(idx) {
                                    let id = track.youtube_id.clone();
                                    let title = track.title.clone();
                                    let sender = ctx.youtube_manager.sender();

                                    modal!(ctx, ConfirmModal::builder()
                                        .ctx(ctx)
                                        .message(vec![
                                            format!("Are you sure you want to remove '{}' from your library?", title),
                                        ])
                                        .action(Action::Single {
                                            on_confirm: Box::new(move |_ctx| {
                                                let sender = sender.clone();
                                                let id_str = id.to_string();
                                                smol::spawn(async move {
                                                    let _ = sender.send(YouTubeEvent::RemoveFromLibrary(id_str)).await;
                                                }).detach();
                                                Ok(())
                                            }),
                                            confirm_label: Some("Remove"),
                                            cancel_label: None,
                                        })
                                        .size((60, 6))
                                        .build()
                                    );
                                }
                            }
                        }
                        YoutubeFocus::Input | YoutubeFocus::SearchTable => {
                            event.abandon();
                        }
                    }
                }
                _ => {
                    event.abandon();
                }
            }
        }
        Ok(())
    }
}