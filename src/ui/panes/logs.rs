use anyhow::Result;
use itertools::Itertools;
use log::debug;
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    prelude::Rect,
    widgets::{List, ListItem, ListState},
};

use super::Pane;
use crate::{
    config::keys::{CommonAction, LogsActions},
    ctx::Ctx,
    shared::{
        clipboard,
        key_event::KeyEvent,
        mouse_event::{MouseEvent, MouseEventKind},
        ring_vec::RingVec,
    },
    ui::{UiEvent, dirstack::DirState},
};

/// Handles selection state for multi-line selection
#[derive(Debug, Default)]
pub struct SelectionState {
    start: Option<usize>,
    end: Option<usize>,
}

impl SelectionState {
    pub fn begin(&mut self, idx: usize) {
        self.start = Some(idx);
        self.end = Some(idx);
    }

    pub fn update(&mut self, idx: usize) {
        if self.start.is_some() {
            self.end = Some(idx);
        }
    }

    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
    }

    pub fn selected_range(&self) -> Option<(usize, usize)> {
        match (self.start, self.end) {
            (Some(s), Some(e)) => Some((s.min(e), s.max(e))),
            _ => None,
        }
    }

    pub fn contains(&self, idx: usize) -> bool {
        if let Some((start, end)) = self.selected_range() {
            (start..=end).contains(&idx)
        } else {
            false
        }
    }
}

/// Utility: formats logs into wrapped, indented lines
pub struct LogFormatter;

impl LogFormatter {
    const INDENT_LEN: usize = 4;
    const INDENT: &'static str = "    ";

    pub fn format_lines<'a>(
        logs: &'a RingVec<1000, Vec<u8>>,
        max_width: usize,
    ) -> Vec<String> {
        logs.iter()
            .map(|l| String::from_utf8_lossy(l))
            .flat_map(|l| {
                let mut wrapped =
                    textwrap::wrap(&l, textwrap::Options::new(max_width));
                wrapped
                    .iter_mut()
                    .skip(1)
                    .for_each(|v| *v = std::borrow::Cow::Owned(textwrap::indent(v, Self::INDENT)));
                wrapped.into_iter().map(|cow| cow.into_owned()).collect_vec()
            })
            .collect()
    }
}

#[derive(Debug)]
pub struct LogsPane {
    logs: RingVec<1000, Vec<u8>>,
    scrolling_state: DirState<ListState>,
    logs_area: Rect,
    should_scroll_to_last: bool,
    scroll_enabled: bool,
    selection_state: SelectionState,
}

impl LogsPane {
    pub fn new() -> Self {
        Self {
            logs: RingVec::default(),
            scrolling_state: DirState::default(),
            logs_area: Rect::default(),
            should_scroll_to_last: false,
            scroll_enabled: true,
            selection_state: SelectionState::default(),
        }
    }

    fn line_index_from_mouse(&self, event: MouseEvent) -> Option<usize> {
        if !self.logs_area.contains(event.into()) {
            return None;
        }
        let y_offset = event.y.saturating_sub(self.logs_area.top());
        // DirState probably has a "offset" or "scroll" index instead of first_visible()
        let viewport_start = self.scrolling_state.offset();
        Some(viewport_start + y_offset as usize)
    }

    fn copy_selection_to_clipboard(&self) -> Result<()> {
        if self.selection_state.selected_range().is_some() {
            let max_line_width = (self.logs_area.width as usize)
                .saturating_sub(LogFormatter::INDENT_LEN + 3);
            let formatted_lines = LogFormatter::format_lines(&self.logs, max_line_width);

            let selected_text = formatted_lines
                .iter()
                .enumerate()
                .filter_map(|(idx, line)| {
                    if self.selection_state.contains(idx) {
                        Some(line.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");

            if selected_text.is_empty() {
                debug!("No text selected for clipboard copy");
                return Ok(());
            }

            clipboard::copy_to_clipboard(selected_text);
        } else {
            debug!("No selection to copy");
        }

        Ok(())
    }
}

impl Pane for LogsPane {
    fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        Ctx { config, .. }: &Ctx,
    ) -> anyhow::Result<()> {
        let scrollbar_area_width: u16 = config.theme.scrollbar.is_some().into();
        let [logs_area, scrollbar_area] = Layout::horizontal([
            Constraint::Percentage(100),
            Constraint::Min(scrollbar_area_width),
        ])
        .areas(area);
        self.logs_area = logs_area;

        let max_line_width = (logs_area.width as usize)
            .saturating_sub(LogFormatter::INDENT_LEN + 3);
        let lines = LogFormatter::format_lines(&self.logs, max_line_width);

        let content_len = lines.len();
        self.scrolling_state
            .set_content_and_viewport_len(content_len, logs_area.height.into());

        if self.scroll_enabled
            && (self.scrolling_state.get_selected().is_none() || self.should_scroll_to_last)
        {
            self.should_scroll_to_last = false;
            self.scrolling_state.last();
        }

        // Build styled list
        let items: Vec<ListItem> = lines
            .into_iter()
            .enumerate()
            .map(|(idx, line)| {
                let style = if self.selection_state.contains(idx) {
                    config.theme.current_item_style
                } else {
                    config.as_text_style()
                };
                ListItem::new(line).style(style)
            })
            .collect();

        let logs_wg = List::new(items)
            .highlight_style(config.theme.current_item_style);

        if let Some(scrollbar) = config.as_styled_scrollbar() {
            frame.render_stateful_widget(
                scrollbar,
                scrollbar_area,
                self.scrolling_state.as_scrollbar_state_ref(),
            );
        }
        frame.render_stateful_widget(
            logs_wg,
            logs_area,
            self.scrolling_state.as_render_state_ref(),
        );

        Ok(())
    }

    fn before_show(&mut self, _ctx: &Ctx) -> Result<()> {
        self.scrolling_state.last();
        Ok(())
    }

    fn on_event(&mut self, event: &mut UiEvent, is_visible: bool, ctx: &Ctx) -> Result<()> {
        if let UiEvent::LogAdded(msg) = event {
            self.logs.push(std::mem::take(msg));
            self.should_scroll_to_last = true;
            if is_visible {
                ctx.render()?;
            }
        }
        Ok(())
    }

    fn handle_mouse_event(&mut self, event: MouseEvent, ctx: &Ctx) -> Result<()> {
        if !self.logs_area.contains(event.into()) {
            return Ok(());
        }

        match event.kind {
            MouseEventKind::ScrollUp => {
                self.scrolling_state.scroll_up(1, ctx.config.scrolloff);
                ctx.render()?;
            }
            MouseEventKind::ScrollDown => {
                self.scrolling_state.scroll_down(1, ctx.config.scrolloff);
                ctx.render()?;
            }
            MouseEventKind::LeftClick => {
                if let Some(idx) = self.line_index_from_mouse(event) {
                    // start a new selection
                    self.selection_state.begin(idx);
                    ctx.render()?;
                }
            }
            MouseEventKind::Drag { .. } => {
                if let Some(idx) = self.line_index_from_mouse(event) {
                    // extend selection
                    self.selection_state.update(idx);
                    ctx.render()?;
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_action(&mut self, event: &mut KeyEvent, ctx: &mut Ctx) -> Result<()> {
        let config = &ctx.config;

        if let Some(action) = event.as_logs_action(ctx) {
            match action {
                LogsActions::Clear => {
                    self.logs.clear();
                    self.selection_state.clear();
                    ctx.render()?;
                }
                LogsActions::ToggleScroll => {
                    self.scroll_enabled ^= true;
                }
                LogsActions::Copy => {
                    self.copy_selection_to_clipboard()?;
                }
            }
        } else if let Some(action) = event.as_common_action(ctx) {
            match action {
                CommonAction::DownHalf => {
                    self.scrolling_state.next_half_viewport(ctx.config.scrolloff);
                    ctx.render()?;
                }
                CommonAction::UpHalf => {
                    self.scrolling_state.prev_half_viewport(ctx.config.scrolloff);
                    ctx.render()?;
                }
                CommonAction::Up => {
                    self.scrolling_state.prev(ctx.config.scrolloff, config.wrap_navigation);
                    ctx.render()?;
                }
                CommonAction::Down => {
                    self.scrolling_state.next(ctx.config.scrolloff, config.wrap_navigation);
                    ctx.render()?;
                }
                CommonAction::Bottom => {
                    self.scrolling_state.last();
                    ctx.render()?;
                }
                CommonAction::Top => {
                    self.scrolling_state.first();
                    ctx.render()?;
                }
                _ => {}
            }
        }

        Ok(())
    }
}
