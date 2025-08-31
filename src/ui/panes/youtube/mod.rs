// src/ui/panes/youtube/mod.rs
pub use super::{ActionOutcome, Component, Focus};
pub(crate) use super::Pane;

pub mod library_component;
pub mod search_component;

use self::{
    library_component::LibraryComponent,
    search_component::SearchComponent,
};

use crate::{
    core::data_store::models::YouTubeSong,
    ctx::Ctx,
    shared::{
        events::AppEvent,
        key_event::KeyEvent,
        mouse_event::MouseEvent,
    },
    ui::UiAppEvent,
    youtube::ResolvedYouTubeSong,
};
use anyhow::Result;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    Frame,
};

pub struct YouTubePane {
    focus: Focus,
    search: SearchComponent,
    library: LibraryComponent,
}

impl std::fmt::Debug for YouTubePane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YouTubePane").field("focus", &self.focus).finish()
    }
}

impl YouTubePane {
    pub fn new(ctx: &Ctx) -> Result<Self> {
        Ok(Self {
            focus: Focus::default(),
            search: SearchComponent::default(),
            library: LibraryComponent::new(ctx)?,
        })
    }

    // === Public API for event handling from the main event loop ===
    pub fn on_search_result(&mut self, song: ResolvedYouTubeSong, generation: u64) {
        self.search.on_search_result(song, generation);
    }

    pub fn on_search_complete(&mut self, generation: u64) {
        self.search.on_search_complete(generation);
    }

    pub fn add_song_to_library(&mut self, song: YouTubeSong) {
        self.library.add_song(song);
    }

    pub fn remove_song_from_library(&mut self, youtube_id: &str) {
        self.library.remove_song(youtube_id);
    }

    fn handle_outcome(&mut self, outcome: ActionOutcome, ctx: &mut Ctx) -> Result<()> {
        match outcome {
            ActionOutcome::FocusChanged(new_focus) => self.focus = new_focus,
            ActionOutcome::QueueSong(song) => {
                ctx.app_event_sender
                    .send(AppEvent::UiEvent(UiAppEvent::QueueYouTubeSong(song)))?;
            }
            ActionOutcome::AddSongsToLibrary(songs) => {
                ctx.app_event_sender
                    .send(AppEvent::UiEvent(UiAppEvent::YouTubeLibraryAddSongs(songs)))?;
            }
            ActionOutcome::ShowContextMenu => {
                self.library.controller.show_context_menu(ctx)?;
            }
            _ => {} // Handled and Ignored do nothing here.
        }
        Ok(())
    }
}

impl Pane for YouTubePane {
    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Result<()> {
        let columns =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).split(area);

        self.search.render(frame, columns[0], ctx)?;
        self.library.render(frame, columns[1], ctx)?;

        Ok(())
    }

    fn handle_action(&mut self, event: &mut KeyEvent, ctx: &mut Ctx) -> Result<()> {
        let outcome = match self.focus {
            Focus::Search => self.search.handle_key_event(event, ctx, true)?,
            Focus::Library => self.library.handle_key_event(event, ctx, true)?,
        };

        self.handle_outcome(outcome, ctx)?;

        ctx.render()?;
        Ok(())
    }

    fn handle_mouse_event(&mut self, event: MouseEvent, ctx: &Ctx) -> Result<()> {
        let search_outcome = self.search.handle_mouse_event(event.clone(), ctx, self.focus == Focus::Search)?;
        if search_outcome != ActionOutcome::Ignored {
            self.focus = Focus::Search;
            self.handle_outcome(search_outcome, ctx)?;
            return Ok(());
        }

        let library_outcome = self.library.handle_mouse_event(event, ctx, self.focus == Focus::Library)?;
        if library_outcome != ActionOutcome::Ignored {
            self.focus = Focus::Library;
            self.handle_outcome(library_outcome, ctx)?;
            return Ok(());
        }

        Ok(())
    }
}
