use anyhow::Result;
use itertools::Itertools;
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    prelude::Rect,
    widgets::{List, ListState},
};

use super::Pane;
use crate::{
    config::keys::{CommonAction, LogsActions},
    ctx::Ctx,
    shared::{
        keys::ActionEvent,
        mouse_event::{MouseEvent, MouseEventKind},
        ring_vec::RingVec,
    },
    ui::{UiEvent, dirstack::DirState},
};

#[derive(Debug)]
pub struct LogsPane {
    logs: RingVec<1000, Vec<u8>>,
    scrolling_state: DirState<ListState>,
    logs_area: Rect,
    should_scroll_to_last: bool,
    scroll_enabled: bool,
    selection_start: Option<usize>,
    selection_end: Option<usize>,
    is_selecting: bool,
    unwrapped_lines: Vec<String>,
}

impl LogsPane {
    pub fn new() -> Self {
        Self {
            scroll_enabled: true,
            logs: RingVec::default(),
            scrolling_state: DirState::default(),
            logs_area: Rect::default(),
            should_scroll_to_last: false,
            selection_start: None,
            selection_end: None,
            is_selecting: false,
            unwrapped_lines: Vec::new(),
        }
    }
}

const INDENT_LEN: usize = 4;
const INDENT: &str = "    ";

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

        let max_line_width = (logs_area.width as usize).saturating_sub(INDENT_LEN + 3);
        let lines: Vec<_> = self.logs.iter().map(|l| String::from_utf8_lossy(l)).collect_vec();
        let lines: Vec<_> = lines
            .iter()
            .flat_map(|l| {
                let mut lines = textwrap::wrap(l, textwrap::Options::new(max_line_width));
                lines
                    .iter_mut()
                    .skip(1)
                    .for_each(|v| *v = std::borrow::Cow::Owned(textwrap::indent(v, INDENT)));
                lines
            })
            .collect();

        self.unwrapped_lines = lines.iter().map(|l| l.to_string()).collect();
        let content_len = lines.len();
        self.scrolling_state.set_content_and_viewport_len(content_len, logs_area.height.into());
        if self.scroll_enabled
            && (self.scrolling_state.get_selected().is_none() || self.should_scroll_to_last)
        {
            self.should_scroll_to_last = false;
            self.scrolling_state.last();
        }

        let selection_range =
            if let (Some(start), Some(end)) = (self.selection_start, self.selection_end) {
                let (s, e) = if start <= end { (start, end) } else { (end, start) };
                Some(s..=e)
            } else {
                None
            };

        let items: Vec<_> = lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let mut item = ratatui::widgets::ListItem::new(l.clone());
                if let Some(ref range) = selection_range {
                    if range.contains(&i) {
                        item = item.style(config.theme.highlighted_item_style);
                    }
                }
                item
            })
            .collect();

        let logs_wg = List::new(items).style(config.as_text_style());
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
            if matches!(event.kind, MouseEventKind::LeftClick) {
                self.selection_start = None;
                self.selection_end = None;
                ctx.render()?;
            }
            return Ok(());
        }

        match event.kind {
            MouseEventKind::ScrollUp => {
                self.scrolling_state.scroll_up(ctx.config.scroll_amount, ctx.config.scrolloff);

                ctx.render()?;
            }
            MouseEventKind::ScrollDown => {
                self.scrolling_state.scroll_down(ctx.config.scroll_amount, ctx.config.scrolloff);

                ctx.render()?;
            }
            MouseEventKind::LeftClick => {
                let clicked_row = (event.y - self.logs_area.y) as usize;
                let offset = self.scrolling_state.as_render_state_ref().offset();
                let selected_idx = offset + clicked_row;
                if selected_idx < self.unwrapped_lines.len() {
                    self.selection_start = Some(selected_idx);
                    self.selection_end = Some(selected_idx);
                    self.is_selecting = true;
                    ctx.render()?;
                } else {
                    self.selection_start = None;
                    self.selection_end = None;
                    ctx.render()?;
                }
            }
            MouseEventKind::Drag { .. } if self.is_selecting => {
                let clicked_row = (event.y - self.logs_area.y) as usize;
                let offset = self.scrolling_state.as_render_state_ref().offset();
                let selected_idx = offset + clicked_row;
                self.selection_end =
                    Some(selected_idx.min(self.unwrapped_lines.len().saturating_sub(1)));
                ctx.render()?;
            }
            MouseEventKind::Release => {
                self.is_selecting = false;
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_action(&mut self, event: &mut ActionEvent, ctx: &mut Ctx) -> Result<()> {
        let config = &ctx.config;
        if let Some(action) = event.claim_logs() {
            match action {
                LogsActions::Clear => {
                    self.logs.clear();

                    ctx.render()?;
                }
                LogsActions::ToggleScroll => {
                    self.scroll_enabled ^= true;
                }
                LogsActions::Copy => {
                    if let (Some(start), Some(end)) = (self.selection_start, self.selection_end) {
                        let (s, e) = if start <= end { (start, end) } else { (end, start) };
                        let selected_text = self.unwrapped_lines[s..=e].join("\n");

                        let child = std::process::Command::new("xclip")
                            .args(["-selection", "clipboard"])
                            .stdin(std::process::Stdio::piped())
                            .spawn();

                        match child {
                            Ok(mut child) => {
                                use std::io::Write;
                                if let Some(mut stdin) = child.stdin.take() {
                                    if let Err(err) = stdin.write_all(selected_text.as_bytes()) {
                                        log::error!("Failed to write to xclip stdin: {err}");
                                    }
                                }
                                let _ = child.wait();
                                let _ = ctx.app_event_sender.send(
                                    crate::shared::events::AppEvent::Status(
                                        "Copied selection to clipboard".to_string(),
                                        crate::shared::events::Level::Info,
                                        std::time::Duration::from_secs(3),
                                    ),
                                );
                            }
                            Err(err) => {
                                log::error!("Failed to spawn xclip: {err}");
                                let _ = ctx.app_event_sender.send(
                                    crate::shared::events::AppEvent::Status(
                                        "Failed to copy: xclip not found?".to_string(),
                                        crate::shared::events::Level::Error,
                                        std::time::Duration::from_secs(3),
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        } else if let Some(action) = event.claim_common() {
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
