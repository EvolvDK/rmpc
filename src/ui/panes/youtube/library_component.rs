// src/ui/panes/youtube/library_component.rs

use super::{ActionOutcome, Component, Focus};
use crate::{
    config::keys::CommonAction,
    core::data_store::models::YouTubeSong,
    ctx::Ctx,
    shared::{
        events::AppEvent,
        key_event::KeyEvent,
        macros::status_info,
        mouse_event::{MouseEvent, MouseEventKind},
    },
    ui::UiAppEvent,
    youtube::YouTubeLibraryController,
};
use anyhow::Result;
use crossterm::event::KeyCode;
use ratatui::{
    layout::{Constraint, Layout, Position, Rect},
    style::{Stylize},
    text::{Line, Span, Text},
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
enum InternalFocus {
    #[default]
    Artists,
    Songs,
}

#[derive(Debug, Default)]
enum SongFocus {
    #[default]
    List,
    Preview,
}

pub struct LibraryComponent {
    focus: InternalFocus,
    song_focus: SongFocus,
    pub controller: YouTubeLibraryController,
    artist_list_state: ListState,
    song_list_state: ListState,
    artists_area: Rect,
    songs_area: Rect,
}

impl LibraryComponent {
    pub fn new(ctx: &Ctx) -> Result<Self> {
        Ok(Self {
            focus: InternalFocus::default(),
            song_focus: SongFocus::default(),
            controller: YouTubeLibraryController::load_from_data_store(ctx)?,
            artist_list_state: ListState::default(),
            song_list_state: ListState::default(),
            artists_area: Rect::default(),
            songs_area: Rect::default(),
        })
    }

    // === Public API for parent communication ===
    pub fn add_song(&mut self, song: YouTubeSong) {
        if self.controller.add_song(song) {
            self.sync_list_selections();
        }
    }

    pub fn remove_song(&mut self, youtube_id: &str) {
        if self.controller.remove_song(youtube_id) {
            self.sync_list_selections();
        }
    }

    // === Internal Logic ===
    fn sync_list_selections(&mut self) {
        self.artist_list_state.select(self.controller.navigation_state().selected_artist_index);
        self.song_list_state.select(self.controller.navigation_state().selected_song_index);
    }
    
    // Helper to calculate a list item's index from a mouse position.
    fn get_list_index_from_pos(&self, area: Rect, state: &ListState, pos: Position) -> Option<usize> {
        if area.contains(pos) {
            // Subtract 1 for the top border.
            let y_offset = pos.y.saturating_sub(area.y + 1) as usize;
            Some(y_offset + state.offset())
        } else {
            None
        }
    }

    // --- Action Handlers ---
    fn handle_artists_key_event(&mut self, event: &mut KeyEvent, _ctx: &mut Ctx) -> Result<ActionOutcome> {
        match event.as_common_action(_ctx) {
            Some(CommonAction::Up) => self.controller.move_artist_selection(-1),
            Some(CommonAction::Down) => self.controller.move_artist_selection(1),
            _ => match event.code() {
                KeyCode::Left => return Ok(ActionOutcome::FocusChanged(Focus::Search)),
                KeyCode::Right => self.focus = InternalFocus::Songs,
                KeyCode::Char(' ') => {
                    if let Some(index) = self.controller.navigation_state().selected_artist_index {
                        self.controller.toggle_artist_selection(index);
                    }
                }
                KeyCode::Esc => self.controller.clear_selections(),
                _ => return Ok(ActionOutcome::Ignored),
            },
        }
        self.sync_list_selections();
        Ok(ActionOutcome::Handled)
    }

    fn handle_songs_key_event(&mut self, event: &mut KeyEvent, ctx: &mut Ctx) -> Result<ActionOutcome> {
        match self.song_focus {
            SongFocus::List => match event.as_common_action(ctx) {
                Some(CommonAction::Up) => self.controller.move_song_selection(-1),
                Some(CommonAction::Down) => self.controller.move_song_selection(1),
                Some(CommonAction::Confirm) => {
                    if let Some(song) = self.controller.selected_song().cloned() {
                        return Ok(ActionOutcome::QueueSong(song));
                    }
                    status_info!("No song selected.");
                    return Ok(ActionOutcome::Handled);
                }
                _ => match event.code() {
                    KeyCode::Right => self.song_focus = SongFocus::Preview,
                    KeyCode::Left => self.focus = InternalFocus::Artists,
                    KeyCode::Char(' ') => {
                        if let Some(index) = self.song_list_state.selected() {
                            self.controller.toggle_song_selection(index);
                        }
                    }
                    KeyCode::Delete => {
                        if let Some(song) = self.controller.selected_song() {
                            ctx.app_event_sender.send(AppEvent::UiEvent(
                                UiAppEvent::YouTubeLibraryRemoveSong(song.youtube_id.clone()),
                            ))?;
                        }
                    }
                    _ => return Ok(ActionOutcome::Ignored),
                },
            },
            SongFocus::Preview => match event.as_common_action(ctx) {
                Some(CommonAction::Up) => self.controller.move_song_selection(-1),
                Some(CommonAction::Down) => self.controller.move_song_selection(1),
                _ => match event.code() {
                    KeyCode::Left => self.song_focus = SongFocus::List,
                    _ => return Ok(ActionOutcome::Ignored),
                },
            },
        }
        self.sync_list_selections();
        Ok(ActionOutcome::Handled)
    }
}

impl Component for LibraryComponent {
    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Result<()> {
        self.sync_list_selections();
        let columns = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);

        self.artists_area = columns[0];
        self.songs_area = columns[1];

        // Render Artists
        self.render_artists_list(frame, self.artists_area, ctx);

        // Render Songs or Preview
        match self.song_focus {
            SongFocus::List => self.render_songs_list(frame, self.songs_area, ctx),
            SongFocus::Preview => self.render_song_preview(frame, self.songs_area, ctx),
        }

        Ok(())
    }

    fn handle_key_event(
        &mut self,
        event: &mut KeyEvent,
        ctx: &mut Ctx,
        is_focused: bool,
    ) -> Result<ActionOutcome> {
        if !is_focused {
            // Allow focus shift even when not focused
            if let KeyCode::Right = event.code() {
                if self.focus == InternalFocus::Artists {
                     return Ok(ActionOutcome::FocusChanged(Focus::Library));
                }
            }
            return Ok(ActionOutcome::Ignored);
        }

        let outcome = match self.focus {
            InternalFocus::Artists => self.handle_artists_key_event(event, ctx)?,
            InternalFocus::Songs => self.handle_songs_key_event(event, ctx)?,
        };

        if outcome != ActionOutcome::Ignored {
            event.stop_propagation();
        }
        Ok(outcome)
    }

    fn handle_mouse_event(
        &mut self,
        event: MouseEvent,
        ctx: &Ctx,
        _is_focused: bool,
    ) -> Result<ActionOutcome> {
        let pos: Position = event.into();
        if event.kind == MouseEventKind::ScrollUp {
            self.controller.move_artist_selection(-1);
            self.sync_list_selections();
            return Ok(ActionOutcome::Handled);
        }
        if event.kind == MouseEventKind::ScrollDown {
            self.controller.move_artist_selection(1);
            self.sync_list_selections();
            return Ok(ActionOutcome::Handled);
        }
        if let Some(index) = self.get_list_index_from_pos(self.artists_area, &self.artist_list_state, pos) {
            if index < self.controller.artists().len() {
                self.focus = InternalFocus::Artists;
                self.controller.select_artist(index);
                self.sync_list_selections();
                if event.kind == MouseEventKind::RightClick {
                    return Ok(ActionOutcome::ShowContextMenu);
                }
                return Ok(ActionOutcome::Handled);
            }
        }
        if self.songs_area.contains(pos) {
            self.focus = InternalFocus::Songs;
            if let Some(song_count) = self.controller.selected_artist().and_then(|a| self.controller.songs_for_artist(a)).map(|s| s.len()) {
                if let Some(index) = self.get_list_index_from_pos(self.songs_area, &self.song_list_state, pos) {
                    if index < song_count {
                        self.controller.select_song(index);
                        self.sync_list_selections();
                        if event.kind == MouseEventKind::RightClick {
                            return Ok(ActionOutcome::ShowContextMenu);
                        }
                        return Ok(ActionOutcome::Handled);
                    }
                }
            }
            return Ok(ActionOutcome::Handled);
        }
        Ok(ActionOutcome::Ignored)
    }
}

// === Rendering Helpers ===
impl LibraryComponent {
    fn render_artists_list(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) {
        let border_style = if self.focus == InternalFocus::Artists {
            ctx.config.as_focused_border_style()
        } else {
            ctx.config.as_border_style()
        };
        let items: Vec<ListItem> = self
            .controller
            .artists()
            .iter()
            .enumerate()
            .map(|(i, artist)| {
                let mut item = ListItem::new(artist.as_str());
                if self.controller.selection_state().is_artist_selected(i) {
                    item = item.style(ctx.config.theme.highlighted_item_style.reversed());
                }
                item
            })
            .collect();
        let list = List::new(items)
            .block(Block::bordered().title("Library: Artists").border_style(border_style))
            .highlight_style(ctx.config.theme.highlighted_item_style);
        frame.render_stateful_widget(list, area, &mut self.artist_list_state);
    }
    
    fn render_songs_list(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) {
        let border_style = if self.focus == InternalFocus::Songs {
            ctx.config.as_focused_border_style()
        } else {
            ctx.config.as_border_style()
        };

        let artist_name = self.controller.selected_artist().unwrap_or_default();
        let songs = self.controller.songs_for_artist(artist_name).unwrap_or(&[]);
        
        let items: Vec<ListItem> = songs.iter().enumerate().map(|(i, song)| {
            let mut item = ListItem::new(song.title.as_str());
            if self.controller.selection_state().is_song_selected(artist_name, i) {
                item = item.style(ctx.config.theme.highlighted_item_style.reversed());
            }
            item
        }).collect();

        let list = List::new(items)
            .block(Block::bordered().title("Library: Tracks").border_style(border_style))
            .highlight_style(ctx.config.theme.highlighted_item_style);
            
        frame.render_stateful_widget(list, area, &mut self.song_list_state);
    }

    fn render_song_preview(&self, frame: &mut Frame, area: Rect, ctx: &Ctx) {
        let border_style = if self.focus == InternalFocus::Songs {
             ctx.config.as_focused_border_style()
        } else {
            ctx.config.as_border_style()
        };
        let block = Block::bordered().title("Preview").border_style(border_style);
        
        let content = if let Some(song) = self.controller.selected_song() {
            let key_style = ctx.config.theme.preview_label_style;
            let mut lines = vec![
                Line::from(vec![Span::styled("Title: ", key_style), Span::raw(&song.title)]),
                Line::from(vec![Span::styled("Artist: ", key_style), Span::raw(&song.artist)]),
                Line::from(vec![Span::styled("Album: ", key_style), Span::raw(song.album.as_deref().unwrap_or("N/A"))]),
                Line::from(vec![Span::styled("Duration: ", key_style), Span::raw(format!("{}s", song.duration_secs))]),
                Line::from(vec![Span::styled("ID: ", key_style), Span::raw(&song.youtube_id)]),
            ];
            let url = format!("https://www.youtube.com/watch?v={}", song.youtube_id);
            lines.push(Line::from(vec![Span::styled("Link: ", key_style), Span::raw(url)]));
            Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false })
        } else {
            Paragraph::new("No song selected")
        };
        
        frame.render_widget(content.block(block), area);
    }
}
