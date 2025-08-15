use std::collections::{BTreeSet, HashMap};

use anyhow::Result;
use crossterm::event::KeyCode;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Stylize,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use crate::{
    config::{keys::CommonAction, tabs::PaneType},
    core::data_store::models::{PlaylistItem, Playlist, YouTubeVideo},
    ctx::Ctx,
    mpd::mpd_client::{Filter, MpdClient, Tag},
    shared::{
        events::AppEvent,
        key_event::KeyEvent,
        macros::{status_error, status_info},
        mouse_event::{MouseEvent, MouseEventKind},
        mpd_query::PreviewGroup,
    },
    ui::{
        modals::{input_modal::InputModal, menu},
        panes::{render_preview_data, Pane, ToPreview},
        UiAppEvent,
    },
    MpdQueryResult,
};

#[derive(Debug, PartialEq, Eq)]
enum Focus {
    Playlists,
    Content,
}

impl Default for Focus {
    fn default() -> Self {
        Self::Playlists
    }
}

#[derive(Debug, Default)]
pub struct RmpcPlaylistsPane {
    playlists: Vec<crate::core::data_store::models::Playlist>,
    youtube_library: HashMap<String, YouTubeVideo>,
    playlist_list_state: ListState,
    content_list_state: ListState,
    selected_content_indices: BTreeSet<usize>,
    focus: Focus,
    preview_data: Option<Vec<PreviewGroup>>,
    playlist_list_area: Rect,
    content_list_area: Rect,
}

const PREVIEW: &str = "rmpc_playlist_preview";

impl RmpcPlaylistsPane {
    pub fn new(ctx: &Ctx) -> Result<Self> {
        let playlists = ctx.data_store.get_all_playlists()?;
        let youtube_library_vec = ctx.data_store.get_all_library_videos()?;
        let youtube_library: HashMap<String, YouTubeVideo> = youtube_library_vec
            .into_iter()
            .map(|v| (v.youtube_id.clone(), v))
            .collect();

        let mut pane = Self {
            playlists,
            youtube_library,
            playlist_list_area: Rect::default(),
            content_list_area: Rect::default(),
            preview_data: None,
            ..Self::default()
        };
        if !pane.playlists.is_empty() {
            pane.playlist_list_state.select(Some(0));
            if !pane.playlists[0].items.is_empty() {
                pane.content_list_state.select(Some(0));
            }
        }
        Ok(pane)
    }

    pub fn add_video(&mut self, video: &YouTubeVideo) {
        self.youtube_library.insert(video.youtube_id.clone(), video.clone());
    }

    pub fn remove_video(&mut self, video_id: &str) {
        self.youtube_library.remove(video_id);
    }


    fn prepare_preview(&mut self, ctx: &Ctx) -> Result<()> {
        let key_style = ctx.config.theme.preview_label_style;
        let group_style = ctx.config.theme.preview_metadata_group_style;

        self.preview_data = None;

        if let Some(item) = self.get_highlighted_content_item() {
            match item {
                PlaylistItem::YouTube(video) => {
                    if let Some(video) = self.youtube_library.get(&video.youtube_id) {
                        self.preview_data = Some(video.to_preview(key_style, group_style));
                    }
                }
                PlaylistItem::Local(path) => {
                    let file = path.clone();
                    ctx.query()
                        .id(PREVIEW)
                        .replace_id("rmpc_playlist_preview_id")
                        .target(PaneType::RmpcPlaylists)
                        .query(move |client| {
                            Ok(MpdQueryResult::Preview {
                                data: client
                                    .find(&[Filter::new(Tag::File, &file)])?
                                    .pop()
                                    .map(|v| v.to_preview(key_style, group_style)),
                                origin_path: None,
                            })
                        });
                }
            }
        }
        ctx.render()?;
        Ok(())
    }

    fn get_selected_playlist(&self) -> Option<&crate::core::data_store::models::Playlist> {
        self.playlist_list_state.selected().and_then(|i| self.playlists.get(i))
    }

    fn get_selected_playlist_name(&self) -> Option<&str> {
        self.get_selected_playlist().map(|p| p.name.as_str())
    }

    pub fn refresh_playlists(&mut self, ctx: &Ctx) -> Result<()> {
        let prev_selected_id = self.get_selected_playlist().map(|p| p.id);
        self.playlists = ctx.data_store.get_all_playlists()?;

        let mut new_selection_idx = None;
        if let Some(prev_id) = prev_selected_id {
            new_selection_idx = self.playlists.iter().position(|p| p.id == prev_id);
        }

        if new_selection_idx.is_none() && !self.playlists.is_empty() {
            new_selection_idx = Some(0);
        }

        self.playlist_list_state.select(new_selection_idx);

        self.selected_content_indices.clear();
        if self.get_selected_playlist().is_some_and(|p| !p.items.is_empty()) {
            self.content_list_state.select(Some(0));
        } else {
            self.content_list_state.select(None);
        }

        Ok(())
    }

    fn get_selected_content_items(&self) -> Vec<PlaylistItem> {
        let Some(playlist) = self.get_selected_playlist() else {
            return Vec::new();
        };
        if self.selected_content_indices.is_empty() {
            self.content_list_state
                .selected()
                .and_then(|i| playlist.items.get(i).cloned())
                .into_iter()
                .collect()
        } else {
            self.selected_content_indices
                .iter()
                .filter_map(|i| playlist.items.get(*i).cloned())
                .collect()
        }
    }

    fn get_highlighted_content_item(&self) -> Option<&PlaylistItem> {
        self.get_selected_playlist()
            .and_then(|p| self.content_list_state.selected().and_then(|i| p.items.get(i)))
    }

    fn move_selection(list_state: &mut ListState, count: usize, change: isize) {
        let current = list_state.selected().unwrap_or(0);
        let next = (current as isize + change).max(0).min(count.saturating_sub(1) as isize);
        list_state.select(Some(next as usize));
    }
}

impl Pane for RmpcPlaylistsPane {
    fn on_query_finished(
        &mut self,
        id: &'static str,
        data: MpdQueryResult,
        _is_visible: bool,
        ctx: &Ctx,
    ) -> Result<()> {
        if id == PREVIEW {
            if let MpdQueryResult::Preview { data, .. } = data {
                self.preview_data = data;
                ctx.render()?;
            }
        }
        Ok(())
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Result<()> {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(30),
                Constraint::Percentage(40),
                Constraint::Percentage(30),
            ])
            .split(area);

        self.playlist_list_area = columns[0];
        self.content_list_area = columns[1];

        // Column 1: Playlists
        let playlists_block = Block::default()
            .borders(Borders::ALL)
            .title("Playlists")
            .border_style(if self.focus == Focus::Playlists {
                ctx.config.as_focused_border_style()
            } else {
                ctx.config.as_border_style()
            });
        let playlist_items: Vec<ListItem> =
            self.playlists.iter().map(|p| ListItem::new(p.name.as_str())).collect();
        let playlist_list = List::new(playlist_items)
            .block(playlists_block)
            .highlight_style(ctx.config.theme.highlighted_item_style);
        frame.render_stateful_widget(playlist_list, columns[0], &mut self.playlist_list_state);

        // Column 2: Content
        let content_block = Block::default()
            .borders(Borders::ALL)
            .title("Content")
            .border_style(if self.focus == Focus::Content {
                ctx.config.as_focused_border_style()
            } else {
                ctx.config.as_border_style()
            });

        let library_videos: std::collections::HashMap<String, &crate::youtube::YouTubeVideo> =
            self.youtube_library.iter().map(|(id, v)| (id.clone(), v)).collect();

        let content_items: Vec<ListItem> = self
            .get_selected_playlist()
            .map(|p| p.items.as_slice())
            .unwrap_or(&[])
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let line = match item {
                    PlaylistItem::YouTube(video) => {
                        let title = library_videos
                            .get(&video.youtube_id)
                            .map_or("Unknown YouTube Video", |v| &v.title);
                        format!("[YT] {}", title)
                    }
                    PlaylistItem::Local(path) => {
                        let filename = std::path::Path::new(path)
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or(path);
                        format!("[File] {}", filename)
                    }
                };

                let mut item = ListItem::new(line);
                if self.selected_content_indices.contains(&i) {
                    item = item.style(ctx.config.theme.highlighted_item_style.reversed());
                }
                item
            })
            .collect();

        let content_list = List::new(content_items)
            .block(content_block)
            .highlight_style(ctx.config.theme.highlighted_item_style);
        frame.render_stateful_widget(content_list, columns[1], &mut self.content_list_state);

        // Column 3: Preview
        let preview_block = Block::default().borders(Borders::ALL).title("Preview");

        if let Some(data) = &self.preview_data {
            render_preview_data(frame, columns[2], data, preview_block);
        } else {
            let preview_text = if let Some(item) = self.get_highlighted_content_item() {
                match item {
                    PlaylistItem::YouTube(video) => {
                        if self.youtube_library.contains_key(&video.youtube_id) {
                            "Loading preview...".to_string()
                        } else {
                            "Could not find video info in library.".to_string()
                        }
                    }
                    PlaylistItem::Local(_) => "Loading preview...".to_string(),
                }
            } else {
                "No item selected".to_string()
            };
            let preview_widget =
                Paragraph::new(preview_text).block(preview_block).wrap(Wrap { trim: false });
            frame.render_widget(preview_widget, columns[2]);
        }

        Ok(())
    }

    fn handle_action(&mut self, event: &mut KeyEvent, ctx: &mut Ctx) -> Result<()> {
        match self.focus {
            Focus::Playlists => {
                let prev_selected = self.playlist_list_state.selected();
                if let Some(action) = event.as_common_action(ctx) {
                    match action {
                        CommonAction::Down => {
                            Self::move_selection(&mut self.playlist_list_state, self.playlists.len(), 1)
                        }
                        CommonAction::Up => {
                            Self::move_selection(&mut self.playlist_list_state, self.playlists.len(), -1)
                        }
                        _ => {}
                    }
                }
                if prev_selected != self.playlist_list_state.selected() {
                    self.selected_content_indices.clear();
                    if self.get_selected_playlist().is_some_and(|p| !p.items.is_empty()) {
                        self.content_list_state.select(Some(0));
                    } else {
                        self.content_list_state.select(None);
                    }
                    self.prepare_preview(ctx)?;
                }
                match event.code() {
                    KeyCode::Right | KeyCode::Enter => {
                        if self.get_selected_playlist().is_some_and(|p| !p.items.is_empty()) {
                            self.focus = Focus::Content;
                        }
                    }
                    KeyCode::Char('c') => {
                        let modal = InputModal::new(ctx)
                            .title("Create new playlist")
                            .on_confirm(|ctx, name| {
                                if !name.is_empty() {
                                    match ctx.data_store.create_playlist(name) {
                                        Ok(_) => {
                                            ctx.app_event_sender
                                                .send(AppEvent::UiEvent(UiAppEvent::RefreshRmpcPlaylists))?;
                                            status_info!("Created playlist '{}'", name);
                                        }
                                        Err(e) => status_error!("Failed to create playlist: {}", e),
                                    }
                                }
                                Ok(())
                            });
                        ctx.app_event_sender.send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?;
                    }
                    _ => {}
                }
            }
            Focus::Content => {
                if let Some(action) = event.as_common_action(ctx) {
                    match action {
                        CommonAction::Down => {
                            if let Some(p) = self.get_selected_playlist() {
                                Self::move_selection(&mut self.content_list_state, p.items.len(), 1);
                            }
                            self.prepare_preview(ctx)?;
                        }
                        CommonAction::Up => {
                            if let Some(p) = self.get_selected_playlist() {
                                Self::move_selection(&mut self.content_list_state, p.items.len(), -1);
                            }
                            self.prepare_preview(ctx)?;
                        }
                        CommonAction::Confirm => {
                            let items = self.get_selected_content_items();
                            ctx.app_event_sender
                                .send(AppEvent::UiEvent(UiAppEvent::AddPlaylistItemsToQueue(items)))?;
                        }
                        _ => {}
                    }
                }
                match event.code() {
                    KeyCode::Left => self.focus = Focus::Playlists,
                    KeyCode::Char(' ') => {
                        if let Some(index) = self.content_list_state.selected() {
                            if !self.selected_content_indices.remove(&index) {
                                self.selected_content_indices.insert(index);
                            }
                        }
                    }
                    KeyCode::Esc => self.selected_content_indices.clear(),
                    _ => {}
                }
            }
        }
        ctx.render()?;
        Ok(())
    }

    fn handle_mouse_event(&mut self, event: MouseEvent, ctx: &Ctx) -> Result<()> {
        let pos = event.into();
        match event.kind {
            MouseEventKind::RightClick => {
                if self.playlist_list_area.contains(pos) {
                    self.focus = Focus::Playlists;
                    let clicked_row = pos.y.saturating_sub(self.playlist_list_area.y + 1);
                    let selected_index = self.playlist_list_state.offset() + clicked_row as usize;
                    if selected_index < self.playlists.len() {
                        self.playlist_list_state.select(Some(selected_index));
                        self.selected_content_indices.clear();
                        if self.get_selected_playlist().is_some_and(|p| !p.items.is_empty()) {
                            self.content_list_state.select(Some(0));
                        } else {
                            self.content_list_state.select(None);
                        }
                        self.prepare_preview(ctx)?;
                    }

                    if let Some(playlist) = self.get_selected_playlist().cloned() {
                        let other_playlists: Vec<Playlist> = self
                            .playlists
                            .iter()
                            .filter(|p| p.id != playlist.id)
                            .cloned()
                            .collect();

                        let mut modal = menu::modal::MenuModal::new(ctx)
                            .list_section(ctx, menu::playlist_queue_actions(playlist.name.clone()))
                            .list_section(ctx, menu::playlist_cloning_actions(playlist.name.clone()));

                        if !other_playlists.is_empty() {
                            let other_playlist_names =
                                other_playlists.into_iter().map(|p| p.name).collect();
                            modal = modal.list_section(
                                ctx,
                                menu::add_playlist_to_playlist_actions(
                                    playlist.name.clone(),
                                    other_playlist_names,
                                ),
                            );
                        }

                        let final_modal = modal
                            .list_section(ctx, move |section| {
                                let p = playlist.clone();
                                Some(
                                    section
                                        .item("Rename playlist", move |ctx| {
                                            let p_clone = p.clone();
                                            let modal = InputModal::new(ctx)
                                                .title("Rename playlist")
                                                .initial_value(p_clone.name.clone())
                                                .on_confirm(move |ctx, new_name| {
                                                    if !new_name.is_empty() && new_name != &p_clone.name {
                                                        if let Err(e) = ctx.data_store.rename_playlist(p_clone.id, new_name) {
                                                            status_error!("Failed to rename playlist: {}", e);
                                                        } else {
                                                            ctx.app_event_sender
                                                                .send(AppEvent::UiEvent(UiAppEvent::RefreshRmpcPlaylists))?;
                                                        }
                                                    }
                                                    Ok(())
                                                });
                                            ctx.app_event_sender.send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?;
                                            Ok(())
                                        })
                                        .item("Delete playlist", move |ctx| {
                                            let p_clone = p.clone();
                                            let modal = menu::modal::MenuModal::new(ctx)
                                                .list_section(ctx, |section| {
                                                    Some(
                                                        section
                                                            .item(format!("Yes, delete '{}'", p_clone.name), move |ctx| {
                                                                if let Err(e) = ctx.data_store.delete_playlist(p_clone.id) {
                                                                    status_error!("Failed to delete playlist: {}", e);
                                                                } else {
                                                                    ctx.app_event_sender
                                                                        .send(AppEvent::UiEvent(UiAppEvent::RefreshRmpcPlaylists))?;
                                                                }
                                                                Ok(())
                                                            })
                                                            .item("No, cancel", |_| Ok(())),
                                                    )
                                                })
                                                .build();
                                            ctx.app_event_sender.send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?;
                                            Ok(())
                                        }),
                                )
                            })
                            .build();
                        ctx.app_event_sender.send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(final_modal))))?;
                    }
                } else if self.content_list_area.contains(pos) {
                    self.focus = Focus::Content;
                    let clicked_row = pos.y.saturating_sub(self.content_list_area.y + 1);
                    let selected_index = self.content_list_state.offset() + clicked_row as usize;
                    if self
                        .get_selected_playlist()
                        .is_some_and(|p| selected_index < p.items.len())
                    {
                        self.content_list_state.select(Some(selected_index));
                        self.prepare_preview(ctx)?;
                    }

                    let items = self.get_selected_content_items();
                    if !items.is_empty() {
                        if let Some(playlist) = self.get_selected_playlist() {
                            let other_playlists: Vec<_> = self
                                .playlists
                                .iter()
                                .filter(|p| p.id != playlist.id)
                                .map(|p| p.name.clone())
                                .collect();
                            let modal = menu::modal::MenuModal::new(ctx)
                                .list_section(ctx, menu::queue_actions(items.clone()))
                                .list_section(
                                    ctx,
                                    menu::playlist_management_actions(
                                        items.clone(),
                                        playlist.name.clone(),
                                    ),
                                )
                                .list_section(
                                    ctx,
                                    menu::add_to_playlist_actions(items, other_playlists),
                                )
                                .build();
                            ctx.app_event_sender
                                .send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
