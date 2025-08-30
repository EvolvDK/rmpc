// src/ui/panes/youtube/search_component.rs

use super::{ActionOutcome, Component, Focus};
use crate::{
    config::keys::CommonAction,
    core::data_store::models::YouTubeSong,
    ctx::Ctx,
    shared::{
        events::{AppEvent, WorkRequest},
        key_event::KeyEvent,
        macros::status_info,
        mouse_event::{MouseEvent, MouseEventKind},
    },
    youtube::{
        ResolvedYouTubeSong,
        controllers::YouTubeSearchController,
        controllers::search_controller::FilteredSearchResult,
    },
};
use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Position, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, List, ListItem, ListState, Paragraph},
    Frame,
};
use std::collections::HashSet;
use tui_input::{backend::crossterm::EventHandler, Input};

#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
enum InternalFocus {
    #[default]
    SearchInput,
    SearchResults,
}

pub struct SearchComponent {
    focus: InternalFocus,
    input: Input,
    list_state: ListState,
    controller: YouTubeSearchController,
    input_area: Rect,
    results_area: Rect,
}

impl Default for SearchComponent {
    fn default() -> Self {
        Self {
            focus: InternalFocus::default(),
            input: Input::default(),
            list_state: ListState::default(),
            controller: YouTubeSearchController::new(),
            input_area: Rect::default(),
            results_area: Rect::default(),
        }
    }
}

impl SearchComponent {
    // === Public API for parent communication ===
    pub fn on_search_result(&mut self, song: ResolvedYouTubeSong, generation: u64) {
        self.controller.add_search_result(song, generation);
        self.sync_list_selection();
    }

    pub fn on_search_complete(&mut self, generation: u64) {
        self.controller.complete_search(generation);
    }

    // === Internal Logic ===
    fn sync_list_selection(&mut self) {
        let count = self.controller.filtered_results().len();
        if count == 0 {
            self.list_state.select(None);
        } else if self.list_state.selected().is_none() {
            self.list_state.select(Some(0));
        }
    }

    fn move_list_selection(&mut self, count: usize, change: isize) {
        if count == 0 {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0);
        let next = (current as isize + change).max(0).min(count as isize - 1);
        self.list_state.select(Some(next as usize));
    }

    // --- Action Handlers ---
    fn handle_input_key_event(&mut self, event: &mut KeyEvent, ctx: &mut Ctx) -> Result<ActionOutcome> {
        if self.handle_search_mode_cycle(&event.inner()) {
            return Ok(ActionOutcome::Handled);
        }
        match event.code() {
            KeyCode::Enter => {
                let query = self.input.value().to_string();
                if query.trim().is_empty() {
                    return Ok(ActionOutcome::Handled);
                }
                let generation = self.controller.start_new_search(query.clone());
                status_info!("Searching YouTube for: '{}'", &query);
                let request = WorkRequest::YouTubeSearch { query, generation };
                ctx.app_event_sender.send(AppEvent::WorkRequest(request))?;
                Ok(ActionOutcome::Handled)
            }
            KeyCode::Down | KeyCode::Tab => {
                if !self.controller.filtered_results().is_empty() {
                    self.focus = InternalFocus::SearchResults;
                    self.list_state.select(Some(0));
                }
                Ok(ActionOutcome::Handled)
            }
            KeyCode::Right => Ok(ActionOutcome::FocusChanged(Focus::Library)),
            _ => {
                if self.input.handle_event(&Event::Key(event.inner())).is_some() {
                    self.controller.filter_results(self.input.value());
                    self.sync_list_selection();
                    Ok(ActionOutcome::Handled)
                } else {
                    Ok(ActionOutcome::Ignored)
                }
            }
        }
    }

    fn handle_results_key_event(&mut self, event: &mut KeyEvent, ctx: &mut Ctx) -> Result<ActionOutcome> {
        let count = self.controller.filtered_results().len();
        
        // Handle common actions first
        match event.as_common_action(ctx) {
            Some(CommonAction::Up) => {
                if self.list_state.selected() == Some(0) {
                    self.focus = InternalFocus::SearchInput;
                } else {
                    self.move_list_selection(count, -1);
                }
                return Ok(ActionOutcome::Handled);
            }
            Some(CommonAction::Down) => {
                self.move_list_selection(count, 1);
                return Ok(ActionOutcome::Handled);
            }
            Some(CommonAction::Confirm) => {
                if let Some(index) = self.list_state.selected() {
                    if let Some(result) = self.controller.filtered_results().get(index) {
                        let song: YouTubeSong = result.song.clone().into();
                        return Ok(ActionOutcome::QueueSong(song));
                    }
                }
                return Ok(ActionOutcome::Handled);
            }
            _ => {}
        }

        // Handle specific key codes
        match event.code() {
            KeyCode::Char('a') => {
                if let Some(index) = self.list_state.selected() {
                    if let Some(result) = self.controller.filtered_results().get(index) {
                        let song: YouTubeSong = result.song.clone().into();
                        return Ok(ActionOutcome::AddSongsToLibrary(vec![song]));
                    }
                }
                Ok(ActionOutcome::Handled)
            }
            KeyCode::Left => {
                self.focus = InternalFocus::SearchInput;
                Ok(ActionOutcome::Handled)
            }
            KeyCode::Right => Ok(ActionOutcome::FocusChanged(Focus::Library)),
            _ => Ok(ActionOutcome::Ignored),
        }
    }

    fn handle_search_mode_cycle(&mut self, key_event: &crossterm::event::KeyEvent) -> bool {
        match (key_event.code, key_event.modifiers) {
            (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                self.controller.next_search_mode();
                true
            }
            (KeyCode::Char('f'), mods) if mods.contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
                self.controller.previous_search_mode();
                true
            }
            _ => false,
        }
    }
}

impl Component for SearchComponent {
    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Result<()> {
        let layout = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(area);
        self.input_area = layout[0];
        self.results_area = layout[1];

        // Search Input
        let mut title = format!("Search [{}]", self.controller.search_mode().display_name());
        if let Some(error_msg) = &self.controller.regex_error {
            title = format!("{} (Invalid Regex: {})", title, error_msg);
        }
        let border_style = if self.focus == InternalFocus::SearchInput {
            ctx.config.as_focused_border_style()
        } else {
            ctx.config.as_border_style()
        };
        let input_widget = Paragraph::new(self.input.value())
            .block(Block::bordered().title(title).border_style(border_style));
        frame.render_widget(input_widget, self.input_area);
        if self.focus == InternalFocus::SearchInput {
            frame.set_cursor(
                self.input_area.x + self.input.visual_cursor() as u16 + 1,
                self.input_area.y + 1,
            );
        }

        // Search Results
        let title = if self.controller.is_loading() { "Results (Loading...)" } else { "Results" };
        let border_style = if self.focus == InternalFocus::SearchResults {
            ctx.config.as_focused_border_style()
        } else {
            ctx.config.as_border_style()
        };
        let items: Vec<ListItem> = self
            .controller
            .filtered_results()
            .iter()
            .map(|r| {
                let highlight = ctx.config.theme.highlighted_item_style.bold();
                let indices: HashSet<usize> = r.highlighted_indices.iter().cloned().collect();
                let title_spans: Vec<Span> = r
                    .song
                    .title
                    .chars()
                    .enumerate()
                    .map(|(i, c)| {
                        Span::styled(
                            c.to_string(),
                            if indices.contains(&i) {
                                highlight
                            } else {
                                Style::default()
                            },
                        )
                    })
                    .collect();
                ListItem::new(Line::from(vec![
                    Line::from(title_spans).into(),
                    Span::raw(format!(" - {}", r.song.artist)),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(Block::bordered().title(title).border_style(border_style))
            .highlight_style(ctx.config.theme.highlighted_item_style);
        frame.render_stateful_widget(list, self.results_area, &mut self.list_state);
        Ok(())
    }

    fn handle_key_event(
        &mut self,
        event: &mut KeyEvent,
        ctx: &mut Ctx,
        is_focused: bool,
    ) -> Result<ActionOutcome> {
        if !is_focused {
            return Ok(ActionOutcome::Ignored);
        }
        let outcome = match self.focus {
            InternalFocus::SearchInput => self.handle_input_key_event(event, ctx)?,
            InternalFocus::SearchResults => self.handle_results_key_event(event, ctx)?,
        };
        if outcome != ActionOutcome::Ignored {
            event.stop_propagation();
        }
        Ok(outcome)
    }

    fn handle_mouse_event(
        &mut self,
        event: MouseEvent,
        _ctx: &mut Ctx,
        _is_focused: bool,
    ) -> Result<ActionOutcome> {
        let pos = Position::from(event);
        if let MouseEventKind::LeftClick = event.kind {
            if self.input_area.contains(pos) {
                self.focus = InternalFocus::SearchInput;
                return Ok(ActionOutcome::Handled);
            }
            if self.results_area.contains(pos) {
                self.focus = InternalFocus::SearchResults;
                let index = pos.y.saturating_sub(self.results_area.y + 1) as usize
                    + self.list_state.offset();
                if index < self.controller.filtered_results().len() {
                    self.list_state.select(Some(index));
                }
                return Ok(ActionOutcome::Handled);
            }
        }
        Ok(ActionOutcome::Ignored)
    }
}
