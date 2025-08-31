use std::collections::{BTreeSet};

use anyhow::Result;
use crossterm::event::KeyCode;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::Stylize,
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use crate::{
    config::tabs::PaneType,
    core::data_store::models::{PlaylistItem, Playlist},
    ctx::Ctx,
    mpd::mpd_client::{Filter, MpdClient, Tag},
    shared::{
        events::AppEvent,
        key_event::KeyEvent,
        mouse_event::{MouseEvent, MouseEventKind},
        mpd_query::PreviewGroup,
    },
    ui::{
        modals::{menu},
        panes::{render_preview_data, Pane, CommonAction},
        UiAppEvent,
    },
    MpdQueryResult,
};

#[derive(Debug, PartialEq, Eq, Default)]
enum Focus {
    #[default]
    Playlists,
    Content,
}

#[derive(Debug, Default)]
pub struct RmpcPlaylistsPane {
    playlists: Vec<Playlist>,
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

        let mut pane = Self {
            playlists,
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

    fn prepare_preview(&mut self, ctx: &Ctx) -> Result<()> {
        let key_style = ctx.config.theme.preview_label_style;
        let group_style = ctx.config.theme.preview_metadata_group_style;
        self.preview_data = None;

        let Some(item) = self.get_highlighted_content_item() else {
            return Ok(());
        };

        match item {
            PlaylistItem::YouTube(song) => {
                if let Some(song_info) = ctx.youtube_library.get(&song.youtube_id) {
                    self.preview_data = Some(song_info.to_song_for_preview().to_preview(key_style, group_style));
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
        ctx.render()?;
        Ok(())
    }

    fn get_selected_playlist(&self) -> Option<&Playlist> {
        self.playlist_list_state.selected().and_then(|i| self.playlists.get(i))
    }

   pub fn refresh_playlists(&mut self, ctx: &Ctx) -> Result<()> {
        let prev_selected_id = self.get_selected_playlist().map(|p| p.id);
        self.playlists = ctx.data_store.get_all_playlists()?;
        let new_selection_idx = prev_selected_id
            .and_then(|id| self.playlists.iter().position(|p| p.id == id))
            .or_else(|| if self.playlists.is_empty() { None } else { Some(0) });
        self.playlist_list_state.select(new_selection_idx);
        self.content_list_state.select(if self.get_selected_playlist().is_some_and(|p| !p.items.is_empty()) { Some(0) } else { None });
        self.selected_content_indices.clear();
        Ok(())
    }

    fn get_selected_content_items(&self) -> Vec<PlaylistItem> {
        let Some(playlist) = self.get_selected_playlist() else { return Vec::new(); };
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
        if count == 0 {
            list_state.select(None);
            return;
        }
        let current = list_state.selected().unwrap_or(0);
        let next = (current as isize + change).max(0).min(count.saturating_sub(1) as isize);
        list_state.select(Some(next as usize));
    }
    
    /// Builds the context menu for a selected playlist.
    fn build_playlist_context_menu(&self, playlist: Playlist, ctx: &Ctx) -> Result<()> {
        let other_playlists: Vec<_> = self
            .playlists
            .iter()
            .filter(|p| p.id != playlist.id)
            .map(|p| p.name.clone())
            .collect();
 
        // Pass the playlist items directly
        let mut modal = menu::modal::MenuModal::new(ctx)
            .list_section(ctx, menu::playlist_queue_actions(playlist.items.clone()))
            .list_section(ctx, menu::playlist_cloning_actions(playlist.name.clone()));
 
        if !other_playlists.is_empty() {
            // Pass the source items directly
            modal = modal.list_section(
                ctx,
                menu::add_playlist_to_playlist_actions(playlist.items.clone(), other_playlists),
            );
        }
 
        let final_modal = modal
            .list_section(ctx, menu::playlist_editing_actions(playlist))
            .build();
 
        ctx.app_event_sender
            .send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(final_modal))))?;
        Ok(())
    }
 
    /// Builds the context menu for selected playlist items.

    fn build_content_context_menu(&self, items: Vec<PlaylistItem>, playlist: &Playlist, ctx: &Ctx) -> Result<()> {
        let other_playlists: Vec<_> = self.playlists.iter().filter(|p| p.id != playlist.id).map(|p| p.name.clone()).collect();
        let modal = menu::modal::MenuModal::new(ctx)
            .list_section(ctx, menu::queue_actions(items.clone()))
            .list_section(ctx, menu::playlist_management_actions(items.clone(), playlist.name.clone()))
            .list_section(ctx, menu::add_to_playlist_actions(items, other_playlists))
            .build();
        ctx.app_event_sender.send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?;
        Ok(())
    }
}

impl Pane for RmpcPlaylistsPane {
    fn on_query_finished(&mut self, id: &'static str, data: MpdQueryResult, _is_visible: bool, ctx: &Ctx) -> Result<()> {
        if id == PREVIEW {
            if let MpdQueryResult::Preview { data, .. } = data {
                self.preview_data = data;
                ctx.render()?;
            }
        }
        Ok(())
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Result<()> {
        let columns = Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(40), Constraint::Percentage(30)]).split(area);
        self.playlist_list_area = columns[0];
        self.content_list_area = columns[1];

        // Column 1: Playlists
        let playlists_block = Block::bordered().title("Playlists").border_style(if self.focus == Focus::Playlists { ctx.config.as_focused_border_style() } else { ctx.config.as_border_style() });
        let playlist_items: Vec<ListItem> = self.playlists.iter().map(|p| ListItem::new(p.name.as_str())).collect();
        let playlist_list = List::new(playlist_items).block(playlists_block).highlight_style(ctx.config.theme.highlighted_item_style);
        frame.render_stateful_widget(playlist_list, columns[0], &mut self.playlist_list_state);

        // Column 2: Content
        let content_block = Block::bordered().title("Content").border_style(if self.focus == Focus::Content { ctx.config.as_focused_border_style() } else { ctx.config.as_border_style() });
        let content_items: Vec<ListItem> = self.get_selected_playlist().map(|p| p.items.as_slice()).unwrap_or(&[]).iter().enumerate().map(|(i, item)| {
            let line = match item {
                PlaylistItem::YouTube(song) => {
                    let title = ctx.youtube_library.get(&song.youtube_id).map_or("Unknown YouTube Song", |v| &v.title);
                    format!("[YT] {}", title)
                }
                PlaylistItem::Local(path) => {
                    let filename = std::path::Path::new(path).file_name().and_then(|s| s.to_str()).unwrap_or(path);
                    format!("[File] {}", filename)
                }
            };
            let mut item = ListItem::new(line);
            if self.selected_content_indices.contains(&i) {
                item = item.style(ctx.config.theme.highlighted_item_style.reversed());
            }
            item
        }).collect();
        let content_list = List::new(content_items).block(content_block).highlight_style(ctx.config.theme.highlighted_item_style);
        frame.render_stateful_widget(content_list, columns[1], &mut self.content_list_state);

        // Column 3: Preview
        let preview_block = Block::bordered().title("Preview");
        if let Some(data) = &self.preview_data {
            render_preview_data(frame, columns[2], data, preview_block);
        } else {
            let preview_text = self.get_highlighted_content_item().map_or("No item selected", |item| {
                match item {
                    PlaylistItem::YouTube(song) if ctx.youtube_library.contains_key(&song.youtube_id) => "Loading preview...",
                    PlaylistItem::YouTube(_) => "Could not find song info in library.",
                    PlaylistItem::Local(_) => "Loading preview...",
                }
            });
            let preview_widget = Paragraph::new(preview_text).block(preview_block).wrap(Wrap { trim: false });
            frame.render_widget(preview_widget, columns[2]);
        }
        Ok(())
    }

    fn handle_action(&mut self, event: &mut KeyEvent, ctx: &mut Ctx) -> Result<()> {
        match self.focus {
            Focus::Playlists => {
                let prev_selected = self.playlist_list_state.selected();
                match event.as_common_action(ctx) {
                    Some(CommonAction::Down) => Self::move_selection(&mut self.playlist_list_state, self.playlists.len(), 1),
                    Some(CommonAction::Up) => Self::move_selection(&mut self.playlist_list_state, self.playlists.len(), -1),
                    _ => {}
                }
                if prev_selected != self.playlist_list_state.selected() {
                    self.content_list_state.select(if self.get_selected_playlist().is_some_and(|p| !p.items.is_empty()) { Some(0) } else { None });
                    self.selected_content_indices.clear();
                    self.prepare_preview(ctx)?;
                }
                match event.code() {
                    KeyCode::Right | KeyCode::Enter => {
                        if self.get_selected_playlist().is_some_and(|p| !p.items.is_empty()) {
                            self.focus = Focus::Content;
                        }
                    }
                    KeyCode::Char('c') => {
                        // [FIXED] Call the new, correctly named `create_playlist_modal` function.
                        let modal = menu::create_playlist_modal(ctx);
                        ctx.app_event_sender.send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?;
                    }
                    _ => {}
                }
            }
            Focus::Content => {
                if let Some(playlist) = self.get_selected_playlist() {
                    match event.as_common_action(ctx) {
                        Some(CommonAction::Down) => {
                            Self::move_selection(&mut self.content_list_state, playlist.items.len(), 1);
                            self.prepare_preview(ctx)?;
                        }
                        Some(CommonAction::Up) => {
                            Self::move_selection(&mut self.content_list_state, playlist.items.len(), -1);
                            self.prepare_preview(ctx)?;
                        }
                        Some(CommonAction::Confirm) => {
                            let items = self.get_selected_content_items();
                            ctx.app_event_sender.send(AppEvent::UiEvent(UiAppEvent::AddPlaylistItemsToQueue(items)))?;
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
        if let MouseEventKind::RightClick = event.kind {
            if self.playlist_list_area.contains(pos) {
                self.focus = Focus::Playlists;
                let clicked_row = pos.y.saturating_sub(self.playlist_list_area.y + 1) as usize;
                let selected_index = self.playlist_list_state.offset() + clicked_row;
                if selected_index < self.playlists.len() {
                    self.playlist_list_state.select(Some(selected_index));
                    self.content_list_state.select(if self.get_selected_playlist().is_some_and(|p| !p.items.is_empty()) { Some(0) } else { None });
                    self.selected_content_indices.clear();
                    self.prepare_preview(ctx)?;
                }
                if let Some(playlist) = self.get_selected_playlist().cloned() {
                    self.build_playlist_context_menu(playlist, ctx)?;
                }
            } else if self.content_list_area.contains(pos) {
                self.focus = Focus::Content;
                let clicked_row = pos.y.saturating_sub(self.content_list_area.y + 1) as usize;
                let selected_index = self.content_list_state.offset() + clicked_row;
                if let Some(playlist) = self.get_selected_playlist() {
                    if selected_index < playlist.items.len() {
                        self.content_list_state.select(Some(selected_index));
                        self.prepare_preview(ctx)?;
                    }
                    let items = self.get_selected_content_items();
                    if !items.is_empty() {
                        self.build_content_context_menu(items, playlist, ctx)?;
                    }
                }
            }
        }
        Ok(())
    }
}
