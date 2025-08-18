use std::{collections::HashMap, path::PathBuf};

use anyhow::{Context, Result, anyhow};
use itertools::Itertools;
use serde::Deserialize;
use modals::{
    add_random_modal::AddRandomModal,
    decoders::DecodersModal,
    info_list_modal::InfoListModal,
    input_modal::InputModal,
    keybinds::KeybindsModal,
    menu::modal::MenuModal,
    outputs::OutputsModal,
};
use panes::{PaneContainer, Panes, pane_call};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    symbols::border,
    widgets::{Block, Borders},
};
use tab_screen::TabScreen;

use self::{
    modals::{menu, Modal},
    panes::Pane,
};
use crate::{
    core::data_store::models::{PlaylistItem, YouTubeVideo},
    MpdQueryResult,
    shared::events::AppEvent,
    config::{
        Config,
        cli::Args,
        keys::GlobalAction,
        tabs::{PaneType, SizedPaneOrSplit, TabName},
        theme::level_styles::LevelStyles,
    },
    core::{
        command::{create_env, run_external},
        config_watcher::ERROR_CONFIG_MODAL_ID,
    },
    ctx::Ctx,
    mpd::{
        commands::{State, idle::IdleEvent},
        errors::{ErrorCode, MpdError, MpdFailureResponse},
        mpd_client::{FilterKind, MpdClient, MpdCommand, Tag, ValueChange},
        proto_client::ProtoClient,
        version::Version,
        QueuePosition,
    },
    shared::{
        events::{Level, WorkRequest},
        id::Id,
        key_event::KeyEvent,
        macros::{modal, status_error, status_info, status_warn},
        mouse_event::MouseEvent,
        mpd_client_ext::MpdClientExt,
    },
};

pub mod browser;
pub mod dir_or_song;
pub mod dirstack;
pub mod image;
pub mod modals;
pub mod panes;
pub mod tab_screen;
pub mod widgets;

#[derive(Debug)]
pub struct StatusMessage {
    pub message: String,
    pub level: Level,
    pub created: std::time::Instant,
    pub timeout: std::time::Duration,
}

#[derive(Debug)]
struct PendingPlaylistImport {
    name: String,
    video_ids: Vec<String>,
}

#[derive(Debug)]
pub struct Ui<'ui> {
    panes: PaneContainer<'ui>,
    modals: Vec<Box<dyn Modal>>,
    tabs: HashMap<TabName, TabScreen>,
    layout: SizedPaneOrSplit,
    area: Rect,
    pending_youtube_imports: usize,
    pending_playlist_import: Vec<PendingPlaylistImport>,
}

const OPEN_DECODERS_MODAL: &str = "open_decoders_modal";
const OPEN_OUTPUTS_MODAL: &str = "open_outputs_modal";

macro_rules! active_tab_call {
    ($self:ident, $ctx:ident, $fn:ident($($param:expr),+)) => {
        $self.tabs
            .get_mut(&$ctx.active_tab)
            .context(anyhow!("Expected tab '{}' to be defined. Please report this along with your config.", $ctx.active_tab))?
            .$fn(&mut $self.panes, $($param),+)
    }
}

impl<'ui> Ui<'ui> {
    fn get_youtube_master_library(&mut self, ctx: &mut Ctx) -> Result<HashMap<String, YouTubeVideo>> {
        Ok(ctx
            .data_store
            .get_all_library_videos()?
            .into_iter()
            .map(|v| (v.youtube_id.clone(), v))
            .collect())
    }

    fn sync_videos_to_playlists_pane(
        &mut self,
        videos_to_sync: &[YouTubeVideo],
        ctx: &mut Ctx,
    ) -> Result<()> {
        if !videos_to_sync.is_empty() {
            if let Panes::RmpcPlaylists(p) = self.panes.get_mut(&PaneType::RmpcPlaylists, ctx)? {
                for video in videos_to_sync {
                    p.add_video(video);
                }
            }
        }
        Ok(())
    }


    pub fn new(ctx: &Ctx) -> Result<Ui<'ui>> {
        Ok(Self {
            panes: PaneContainer::new(ctx)?,
            layout: ctx.config.theme.layout.clone(),
            modals: Vec::default(),
            area: Rect::default(),
            tabs: Self::init_tabs(ctx)?,
            pending_youtube_imports: 0,
            pending_playlist_import: Vec::new(),
        })
    }

    fn init_tabs(ctx: &Ctx) -> Result<HashMap<TabName, TabScreen>> {
        ctx.config
            .tabs
            .tabs
            .iter()
            .map(|(name, screen)| -> Result<_> {
                Ok((name.clone(), TabScreen::new(screen.panes.clone())?))
            })
            .try_collect()
    }

    fn calc_areas(&mut self, area: Rect, _ctx: &Ctx) {
        self.area = area;
    }

    pub fn change_tab(&mut self, new_tab: TabName, ctx: &mut Ctx) -> Result<()> {
        self.layout.for_each_pane(self.area, &mut |pane, _, _, _| {
            match self.panes.get_mut(&pane.pane, ctx)? {
                Panes::TabContent => {
                    active_tab_call!(self, ctx, on_hide(ctx))?;
                }
                _ => {}
            }
            Ok(())
        })?;

        ctx.active_tab = new_tab.clone();
        self.on_event(UiEvent::TabChanged(new_tab), ctx)?;

        self.layout.for_each_pane(self.area, &mut |pane, pane_area, _, _| {
            match self.panes.get_mut(&pane.pane, ctx)? {
                Panes::TabContent => {
                    active_tab_call!(self, ctx, before_show(pane_area, ctx))?;
                }
                _ => {}
            }
            Ok(())
        })
    }

    pub fn render(&mut self, frame: &mut Frame, ctx: &mut Ctx) -> Result<()> {
        self.area = frame.area();
        if let Some(bg_color) = ctx.config.theme.background_color {
            frame
                .render_widget(Block::default().style(Style::default().bg(bg_color)), frame.area());
        }

        self.layout.for_each_pane_custom_data(
            self.area,
            &mut *frame,
            &mut |pane, pane_area, block, block_area, frame| {
                match self.panes.get_mut(&pane.pane, ctx)? {
                    Panes::TabContent => {
                        active_tab_call!(self, ctx, render(frame, pane_area, ctx))?;
                    }
                    mut pane_instance => {
                        pane_call!(pane_instance, render(frame, pane_area, ctx))?;
                    }
                }
                frame.render_widget(block.border_style(ctx.config.as_border_style()), block_area);
                Ok(())
            },
            &mut |block, block_area, frame| {
                frame.render_widget(block.border_style(ctx.config.as_border_style()), block_area);
                Ok(())
            },
        )?;

        if ctx.config.theme.modal_backdrop && !self.modals.is_empty() {
            let buffer = frame.buffer_mut();
            buffer.set_style(*buffer.area(), Style::default().fg(Color::DarkGray));
        }

        for modal in &mut self.modals {
            modal.render(frame, ctx)?;
        }

        Ok(())
    }

    pub fn handle_mouse_event(&mut self, event: MouseEvent, ctx: &mut Ctx) -> Result<()> {
        if let Some(ref mut modal) = self.modals.last_mut() {
            modal.handle_mouse_event(event, ctx)?;
            return Ok(());
        }

        self.layout.for_each_pane(self.area, &mut |pane, _, _, _| {
            match self.panes.get_mut(&pane.pane, ctx)? {
                Panes::TabContent => {
                    active_tab_call!(self, ctx, handle_mouse_event(event, ctx))?;
                }
                mut pane_instance => {
                    pane_call!(pane_instance, handle_mouse_event(event, ctx))?;
                }
            }
            Ok(())
        })
    }

    pub fn handle_key(&mut self, key: &mut KeyEvent, ctx: &mut Ctx) -> Result<KeyHandleResult> {
        if let Some(ref mut modal) = self.modals.last_mut() {
            modal.handle_key(key, ctx)?;
            return Ok(KeyHandleResult::None);
        }

        active_tab_call!(self, ctx, handle_action(key, ctx))?;

        if let Some(action) = key.as_global_action(ctx) {
            match action {
                GlobalAction::Partition { name: Some(name), autocreate } => {
                    let name = name.clone();
                    let autocreate = *autocreate;
                    ctx.command(move |client| {
                        match client.switch_to_partition(&name) {
                            Ok(()) => {}
                            Err(MpdError::Mpd(MpdFailureResponse {
                                code: ErrorCode::NoExist,
                                ..
                            })) if autocreate => {
                                client.new_partition(&name)?;
                                client.switch_to_partition(&name)?;
                            }
                            err @ Err(_) => err?,
                        }
                        Ok(())
                    });
                }
                GlobalAction::Partition { name: None, .. } => {
                    let result = ctx.query_sync(move |client| {
                        let partitions = client.list_partitions()?;
                        Ok(partitions.0)
                    })?;
                    let modal = MenuModal::new(ctx)
                        .width(60)
                        .list_section(ctx, |section| {
                            if ctx.status.partition == "default" {
                                None
                            } else {
                                let section = section.item("Switch to default partition", |ctx| {
                                    ctx.command(move |client| {
                                        client.switch_to_partition("default")?;
                                        Ok(())
                                    });
                                    Ok(())
                                });

                                Some(section)
                            }
                        })
                        .multi_section(ctx, |section| {
                            let mut section = section
                                .add_action("Switch", |ctx, label| {
                                    ctx.command(move |client| {
                                        client.switch_to_partition(&label)?;
                                        Ok(())
                                    });
                                })
                                .add_action("Delete", |ctx, label| {
                                    ctx.command(move |client| {
                                        client.delete_partition(&label)?;
                                        Ok(())
                                    });
                                });
                            let mut any_non_default = false;
                            for partition in result
                                .iter()
                                .filter(|p| *p != "default" && **p != ctx.status.partition)
                            {
                                section = section.add_item(partition);
                                any_non_default = true;
                            }

                            if any_non_default { Some(section) } else { None }
                        })
                        .input_section(ctx, "New partition:", |section| {
                            section.action(|ctx, value| {
                                if !value.is_empty() {
                                    ctx.command(move |client| {
                                        client.send_start_cmd_list()?;
                                        client.send_new_partition(&value)?;
                                        client.send_switch_to_partition(&value)?;
                                        client.send_execute_cmd_list()?;
                                        client.read_ok()?;
                                        Ok(())
                                    });
                                }
                            })
                        })
                        .list_section(ctx, |section| Some(section.item("Cancel", |_ctx| Ok(()))))
                        .build();

                    modal!(ctx, modal);
                }
                GlobalAction::Command { command, .. } => {
                    let cmd = command.parse();
                    log::debug!("executing {cmd:?}");

                    if let Ok(Args { command: Some(cmd), .. }) = cmd {
                        if ctx.work_sender.send(WorkRequest::Command(cmd)).is_err() {
                            log::error!("Failed to send command");
                        }
                    }
                }
                GlobalAction::CommandMode => {
                    modal!(
                        ctx,
                        InputModal::new(ctx)
                            .title("Execute a command")
                            .confirm_label("Execute")
                            .on_confirm(|ctx, value| {
                                ctx.app_event_sender.send(AppEvent::UiEvent(
                                    UiAppEvent::ExecuteCommand(value.to_string()),
                                ))?;
                                Ok(())
                            })
                    );
                }
                GlobalAction::NextTrack if ctx.status.state != State::Stop => {
                    ctx.command(move |client| {
                        client.next()?;
                        Ok(())
                    });
                }
                GlobalAction::PreviousTrack if ctx.status.state != State::Stop => {
                    let rewind_to_start = ctx.config.rewind_to_start_sec;
                    let elapsed_sec = ctx.status.elapsed.as_secs();
                    ctx.command(move |client| {
                        match rewind_to_start {
                            Some(value) => {
                                if elapsed_sec >= value {
                                    client.seek_current(ValueChange::Set(0))?;
                                } else {
                                    client.prev()?;
                                }
                            }
                            None => {
                                client.prev()?;
                            }
                        }
                        Ok(())
                    });
                }
                GlobalAction::Stop if matches!(ctx.status.state, State::Play | State::Pause) => {
                    ctx.command(move |client| {
                        client.stop()?;
                        Ok(())
                    });
                }
                GlobalAction::ToggleRepeat => {
                    let repeat = !ctx.status.repeat;
                    ctx.command(move |client| {
                        client.repeat(repeat)?;
                        Ok(())
                    });
                }
                GlobalAction::ToggleRandom => {
                    let random = !ctx.status.random;
                    ctx.command(move |client| {
                        client.random(random)?;
                        Ok(())
                    });
                }
                GlobalAction::ToggleSingle => {
                    let single = ctx.status.single;
                    ctx.command(move |client| {
                        if client.version() < Version::new(0, 21, 0) {
                            client.single(single.cycle_skip_oneshot())?;
                        } else {
                            client.single(single.cycle())?;
                        }
                        Ok(())
                    });
                }
                GlobalAction::ToggleConsume => {
                    let consume = ctx.status.consume;
                    ctx.command(move |client| {
                        if client.version() < Version::new(0, 24, 0) {
                            client.consume(consume.cycle_skip_oneshot())?;
                        } else {
                            client.consume(consume.cycle())?;
                        }
                        Ok(())
                    });
                }
                GlobalAction::ToggleSingleOnOff => {
                    let single = ctx.status.single;
                    ctx.command(move |client| {
                        client.single(single.cycle_skip_oneshot())?;
                        Ok(())
                    });
                }
                GlobalAction::ToggleConsumeOnOff => {
                    let consume = ctx.status.consume;
                    ctx.command(move |client| {
                        client.consume(consume.cycle_skip_oneshot())?;
                        Ok(())
                    });
                }
                GlobalAction::TogglePause => {
                    if matches!(ctx.status.state, State::Play | State::Pause) {
                        ctx.command(move |client| {
                            client.pause_toggle()?;
                            Ok(())
                        });
                    } else {
                        ctx.command(move |client| {
                            client.play()?;
                            Ok(())
                        });
                    }
                }
                GlobalAction::VolumeUp => {
                    let step = ctx.config.volume_step;
                    ctx.command(move |client| {
                        client.volume(ValueChange::Increase(step.into()))?;
                        Ok(())
                    });
                }
                GlobalAction::VolumeDown => {
                    let step = ctx.config.volume_step;
                    ctx.command(move |client| {
                        client.volume(ValueChange::Decrease(step.into()))?;
                        Ok(())
                    });
                }
                GlobalAction::SeekForward
                    if matches!(ctx.status.state, State::Play | State::Pause) =>
                {
                    ctx.command(move |client| {
                        client.seek_current(ValueChange::Increase(5))?;
                        Ok(())
                    });
                }
                GlobalAction::SeekBack
                    if matches!(ctx.status.state, State::Play | State::Pause) =>
                {
                    ctx.command(move |client| {
                        client.seek_current(ValueChange::Decrease(5))?;
                        Ok(())
                    });
                }
                GlobalAction::Update => {
                    ctx.command(move |client| {
                        client.update(None)?;
                        Ok(())
                    });
                }
                GlobalAction::Rescan => {
                    ctx.command(move |client| {
                        client.rescan(None)?;
                        Ok(())
                    });
                }
                GlobalAction::NextTab => {
                    self.change_tab(ctx.config.next_screen(&ctx.active_tab), ctx)?;
                    ctx.render()?;
                }
                GlobalAction::PreviousTab => {
                    self.change_tab(ctx.config.prev_screen(&ctx.active_tab), ctx)?;
                    ctx.render()?;
                }
                GlobalAction::SwitchToTab(name) => {
                    if ctx.config.tabs.names.contains(name) {
                        self.change_tab(name.clone(), ctx)?;
                        ctx.render()?;
                    } else {
                        status_error!(
                            "Tab with name '{}' does not exist. Check your configuration.",
                            name
                        );
                    }
                }
                GlobalAction::NextTrack => {}
                GlobalAction::PreviousTrack => {}
                GlobalAction::Stop => {}
                GlobalAction::SeekBack => {}
                GlobalAction::SeekForward => {}
                GlobalAction::ExternalCommand { command, .. } => {
                    run_external(command.clone(), create_env(ctx, std::iter::empty::<&str>()));
                }
                GlobalAction::Quit => return Ok(KeyHandleResult::Quit),
                GlobalAction::ShowHelp => {
                    let modal = KeybindsModal::new(ctx);
                    modal!(ctx, modal);
                }
                GlobalAction::ShowOutputs => {
                    let current_partition = ctx.status.partition.clone();
                    ctx.query().id(OPEN_OUTPUTS_MODAL).replace_id(OPEN_OUTPUTS_MODAL).query(
                        move |client| {
                            let outputs = client.list_partitioned_outputs(&current_partition)?;
                            Ok(MpdQueryResult::Outputs(outputs))
                        },
                    );
                }
                GlobalAction::ShowDecoders => {
                    ctx.query()
                        .id(OPEN_DECODERS_MODAL)
                        .replace_id(OPEN_DECODERS_MODAL)
                        .query(|client| Ok(MpdQueryResult::Decoders(client.decoders()?.0)));
                }
                GlobalAction::ShowCurrentSongInfo => {
                    if let Some((_, current_song)) = ctx.find_current_song_in_queue() {
                        let items =
                            modals::info_list_modal::KeyValues::from_song(current_song, ctx);
                        modal!(
                            ctx,
                            InfoListModal::builder()
                                .items(items)
                                .title("Song info")
                                .column_widths(&[30, 70])
                                .build()
                        );
                    } else {
                        status_info!("No song is currently playing");
                    }
                }
                GlobalAction::AddRandom => {
                    modal!(ctx, AddRandomModal::new(ctx));
                }
            }
        }

        Ok(KeyHandleResult::None)
    }

    pub fn before_show(&mut self, area: Rect, ctx: &mut Ctx) -> Result<()> {
        self.calc_areas(area, ctx);

        self.layout.for_each_pane(self.area, &mut |pane, pane_area, _, _| {
            match self.panes.get_mut(&pane.pane, ctx)? {
                Panes::TabContent => {
                    active_tab_call!(self, ctx, before_show(pane_area, ctx))?;
                }
                mut pane_instance => {
                    pane_call!(pane_instance, calculate_areas(pane_area, ctx))?;
                    pane_call!(pane_instance, before_show(ctx))?;
                }
            }
            Ok(())
        })
    }

    pub fn on_youtube_search_result(
        &mut self,
        video_info: crate::youtube::YtDlpVideoInfo,
        generation: u64,
        ctx: &mut Ctx,
    ) -> Result<()> {
        if let Panes::YouTube(p) = self.panes.get_mut(&PaneType::YouTube, ctx)? {
            p.on_search_result(video_info, generation);
        }
        Ok(ctx.render()?)
    }

    pub fn on_youtube_search_complete(&mut self, generation: u64, ctx: &mut Ctx) -> Result<()> {
        if let Panes::YouTube(p) = self.panes.get_mut(&PaneType::YouTube, ctx)? {
            p.on_search_complete(generation);
        }
        Ok(ctx.render()?)
    }

    pub fn on_youtube_stream_url_ready(
        &mut self,
        url: String,
        video: YouTubeVideo,
        context: Option<crate::shared::events::RefreshContext>,
        ctx: &mut Ctx,
    ) -> Result<()> {
        log::info!("Adding YouTube video to queue, ID: {}", video.youtube_id);
        let title = video.title.clone();
        if let Some(context) = context {
            // This is a refresh for an existing song
            ctx.query()
                .id("refresh_youtube_song")
                .query(move |client| {
                    client.delete_id(context.old_song_id)?;
                    let tagged_url =
                        crate::youtube::append_youtube_id_to_url(url, &video.youtube_id);
                    let song_id = client
                        .add_id(&tagged_url, Some(QueuePosition::Absolute(context.position)))?
                        .id
                        .context("MPD did not return an ID for the refreshed song")?;

                    if context.play_after_refresh {
                        client.play_id(song_id)?;
                    }

                    Ok(MpdQueryResult::YouTubeSongAdded { song_id, video })
                });
            status_info!("Refreshed '{}' in queue", title);
        } else {
            // This is a new song
            ctx.query().id("add_youtube_song").query(move |client| {
                let tagged_url =
                    crate::youtube::append_youtube_id_to_url(url, &video.youtube_id);
                let song_id = client
                    .add_id(&tagged_url, None)?
                    .id
                    .context("MPD did not return an ID for the added song")?;
                Ok(MpdQueryResult::YouTubeSongAdded { song_id, video })
            });
            status_info!("Added '{}' to queue", title);
        }
        Ok(ctx.render()?)
    }

    pub fn on_youtube_stream_refreshed(
        &mut self,
        url: String,
        video: YouTubeVideo,
        old_song_id: u32,
        position: u32,
        ctx: &mut Ctx,
    ) -> Result<()> {
        ctx.query()
            .id("refresh_youtube_song")
            .query(move |client| {
                client.delete_id(old_song_id)?;
                let tagged_url =
                    crate::youtube::append_youtube_id_to_url(url, &video.youtube_id);
                let song_id = client
                    .add_id(&tagged_url, Some(QueuePosition::Absolute(position as usize)))?
                    .id
                    .context("MPD did not return an ID for the refreshed song")?;
                client.play_id(song_id)?;
                Ok(MpdQueryResult::YouTubeSongAdded { song_id, video })
            });
        Ok(())
    }

    pub fn on_youtube_stream_url_failed(
        &mut self,
        video: YouTubeVideo,
        _context: Option<crate::shared::events::RefreshContext>,
        ctx: &mut Ctx,
    ) -> Result<()> {
        status_error!(
            "Could not fetch stream for '{}'. The video may have been deleted.",
            video.title
        );

        let modal = menu::modal::MenuModal::new(ctx)
            .list_section(ctx, |section| {
                Some(
                    section
                        .item("Remove from library?", |_| Ok(()))
                        .item("Yes", move |ctx| {
                            ctx.app_event_sender.send(AppEvent::UiEvent(
                                UiAppEvent::YouTubeLibraryRemoveVideo(video.youtube_id),
                            ))?;
                            Ok(())
                        })
                        .item("No", |_| Ok(())),
                )
            })
            .build();

        modal!(ctx, modal);
        Ok(ctx.render()?)
    }

    pub fn on_youtube_video_info_fetched(&mut self, video: YouTubeVideo, ctx: &mut Ctx) -> Result<()> {
        ctx.data_store.add_video_to_library(&video)?;
        ctx.youtube_library.insert(video.youtube_id.clone(), video.clone());
        self.sync_videos_to_playlists_pane(&[video.clone()], ctx)?;

        if let Panes::YouTube(p) = self.panes.get_mut(&PaneType::YouTube, ctx)? {
            p.add_video(video);
        }

        if self.pending_youtube_imports > 0 {
            self.pending_youtube_imports -= 1;
            if self.pending_youtube_imports == 0 {
                if !self.pending_playlist_import.is_empty() {
                    ctx.app_event_sender
                        .send(AppEvent::UiEvent(UiAppEvent::FinalizePlaylistImport))?;
                } else {
                    status_info!("YouTube library import complete.");
                }
            }
        }
        Ok(ctx.render()?)
    }

    pub fn on_youtube_library_remove_video(&mut self, video_id: &str, ctx: &mut Ctx) -> Result<()> {
        ctx.data_store.remove_video_from_library(video_id)?;
        if let Panes::YouTube(p) = self.panes.get_mut(&PaneType::YouTube, ctx)? {
            p.remove_video(video_id);
        }
        if let Panes::RmpcPlaylists(p) = self.panes.get_mut(&PaneType::RmpcPlaylists, ctx)? {
            p.remove_video(video_id);
        }
        status_info!("Removed video from library.");
        Ok(ctx.render()?)
    }

    pub fn add_playlist_items_to_queue(
        &mut self,
        items: Vec<PlaylistItem>,
        replace: bool,
        ctx: &mut Ctx,
    ) -> Result<()> {
        if replace {
            ctx.data_store.clear_queue()?;
            ctx.command(|client| {
                client.clear()?;
                Ok(())
            });
        }

        let library_videos = self.get_youtube_master_library(ctx)?;
        let mut added_count = 0;
        let mut skipped_count = 0;

        let (existing_yt_ids, existing_local_files) = if replace {
            (std::collections::HashSet::new(), std::collections::HashSet::new())
        } else {
            (
                ctx.data_store.get_all_queue_youtube_ids()?,
                ctx.queue.iter().map(|s| s.file.clone()).collect(),
            )
        };

        for item in items {
            let is_duplicate = match &item {
                PlaylistItem::Local(path) => existing_local_files.contains(path),
                PlaylistItem::YouTube(video) => existing_yt_ids.contains(&video.youtube_id),
            };

            if is_duplicate {
                skipped_count += 1;
                continue;
            }

            added_count += 1;
            match item {
                PlaylistItem::Local(path) => {
                    ctx.command(move |client| {
                        client.add(&path, None)?;
                        Ok(())
                    });
                }
                PlaylistItem::YouTube(video) => {
                    if let Some(video) = library_videos.get(&video.youtube_id) {
                        ctx.work_sender.send(WorkRequest::GetYouTubeStreamUrl {
                            video: video.clone(),
                            context: None,
                        })?;
                    } else {
                        status_warn!(
                            "Could not find YouTube video with ID {} in library, skipping.",
                            video.youtube_id
                        );
                        added_count -= 1; // It was not actually added
                    }
                }
            }
        }

        if added_count > 0 && skipped_count > 0 {
            status_info!(
                "Added {} items to queue ({} duplicates ignored).",
                added_count,
                skipped_count
            );
        } else if added_count > 0 {
            status_info!("Added {} items to queue.", added_count);
        } else if skipped_count > 0 {
            status_info!("All {} items were already in the queue.", skipped_count);
        }

        Ok(())
    }

    pub fn on_add_items_to_playlist(
        &mut self,
        name: &str,
        items_to_add: Vec<PlaylistItem>,
        ctx: &mut Ctx,
    ) -> Result<()> {
        let playlist_id =
            ctx.data_store.get_all_playlists()?.into_iter().find(|p| p.name == name).map(|p| p.id);

        if let Some(playlist_id) = playlist_id {
            for item in items_to_add {
                match item {
                    PlaylistItem::Local(path) => {
                        ctx.data_store.add_local_file_to_playlist(playlist_id, &path)?;
                    }
                    PlaylistItem::YouTube(video) => {
                        ctx.data_store.add_video_to_library(&video)?;
                        ctx.data_store
                            .add_youtube_video_to_playlist(playlist_id, &video.youtube_id)?;
                    }
                }
            }
        } else {
            return Err(anyhow!("Playlist '{}' not found", name));
        }

        status_info!("Playlist '{}' updated.", name);
        ctx.app_event_sender.send(AppEvent::UiEvent(UiAppEvent::RefreshRmpcPlaylists))?;
        Ok(())
    }

    pub fn on_create_playlist_from_items(
        &mut self,
        name: &str,
        items: Vec<PlaylistItem>,
        ctx: &mut Ctx,
    ) -> Result<()> {
        let playlist_id = ctx.data_store.create_playlist(name)?;
        for item in items.iter() {
            match item {
                PlaylistItem::Local(path) => {
                    ctx.data_store.add_local_file_to_playlist(playlist_id, path)?;
                }
                PlaylistItem::YouTube(video) => {
                    ctx.data_store.add_video_to_library(video)?;
                    ctx.data_store
                        .add_youtube_video_to_playlist(playlist_id, &video.youtube_id)?;
                }
            }
        }

        status_info!("Created playlist '{}' with {} items", name, items.len());
        ctx.app_event_sender.send(AppEvent::UiEvent(UiAppEvent::RefreshRmpcPlaylists))?;
        Ok(())
    }

    pub fn on_import_youtube_library(&mut self, path: PathBuf, ctx: &mut Ctx) -> Result<()> {
        status_info!("Starting library import from {:?}...", path);

        #[derive(Debug, Deserialize)]
        struct TakeoutSong {
            #[serde(rename = "ID vidéo")]
            video_id: String,
        }

        let library = ctx.data_store.get_all_library_videos()?;
        let all_video_ids: std::collections::HashSet<String> =
            library.into_iter().map(|v| v.youtube_id).collect();

        let mut reader = csv::Reader::from_path(path)?;
        let mut count = 0;
        for result in reader.deserialize() {
            let record: TakeoutSong = result?;
            if !all_video_ids.contains(&record.video_id) {
                ctx.work_sender
                    .send(WorkRequest::YouTubeGetVideoInfo { id: record.video_id })?;
                count += 1;
            }
        }

        if count > 0 {
            self.pending_youtube_imports = count;
            status_info!("Importing {} new videos in the background.", count);
        } else {
            status_info!("No new videos to import.");
        }
        Ok(())
    }

    pub fn on_import_youtube_playlists(&mut self, path: PathBuf, ctx: &mut Ctx) -> Result<()> {
        status_info!("Starting playlists import from {:?}...", path);

        #[derive(Debug, Deserialize)]
        struct TakeoutSong {
            #[serde(rename = "ID vidéo")]
            video_id: String,
        }

        let mut pending_playlists = Vec::new();
        let mut all_required_video_ids = std::collections::HashSet::new();

        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "csv") {
                if let Some(playlist_name) = path.file_stem().and_then(|s| s.to_str()) {
                    let mut reader = csv::Reader::from_path(&path)?;
                    let mut video_ids = Vec::new();
                    for result in reader.deserialize() {
                        let record: TakeoutSong = result?;
                        all_required_video_ids.insert(record.video_id.clone());
                        video_ids.push(record.video_id);
                    }
                    pending_playlists.push(PendingPlaylistImport {
                        name: playlist_name.to_string(),
                        video_ids,
                    });
                }
            }
        }

        let existing_library_ids: std::collections::HashSet<String> = ctx
            .data_store
            .get_all_library_videos()?
            .into_iter()
            .map(|v| v.youtube_id)
            .collect();

        let missing_video_ids: Vec<_> = all_required_video_ids
            .difference(&existing_library_ids)
            .cloned()
            .collect();

        if !missing_video_ids.is_empty() {
            self.pending_youtube_imports = missing_video_ids.len();
            self.pending_playlist_import = pending_playlists;
            status_info!(
                "Importing {} playlists. Fetching info for {} new videos...",
                self.pending_playlist_import.len(),
                self.pending_youtube_imports
            );
            for video_id in missing_video_ids {
                ctx.work_sender.send(WorkRequest::YouTubeGetVideoInfo { id: video_id })?;
            }
        } else {
            self.pending_playlist_import = pending_playlists;
            self.on_finalize_playlist_import(ctx)?;
        }

        Ok(())
    }

    fn on_finalize_playlist_import(&mut self, ctx: &mut Ctx) -> Result<()> {
        let pending = std::mem::take(&mut self.pending_playlist_import);
        let playlist_count = pending.len();
        for playlist_to_import in pending {
            let playlist_id = match ctx.data_store.create_playlist(&playlist_to_import.name) {
                Ok(id) => id,
                Err(crate::core::data_store::DataStoreError::PlaylistNameTaken(_)) => {
                    status_warn!("Playlist '{}' already exists, skipping.", playlist_to_import.name);
                    continue;
                }
                Err(e) => return Err(e.into()),
            };

            for video_id in playlist_to_import.video_ids {
                ctx.data_store
                    .add_youtube_video_to_playlist(playlist_id, &video_id)?;
            }
        }

        status_info!("Successfully imported {} playlists.", playlist_count);
        ctx.app_event_sender.send(AppEvent::UiEvent(UiAppEvent::RefreshRmpcPlaylists))?;
        Ok(())
    }

    fn on_save_command(&mut self, name: &str, ctx: &mut Ctx) -> Result<()> {
        let items: Vec<PlaylistItem> = ctx
            .queue
            .iter()
            .map(|song| PlaylistItem::Local(song.file.clone()))
            .collect();

        if items.is_empty() {
            status_info!("Queue is empty, nothing to save.");
            return Ok(());
        }

        let playlist_id = match ctx.data_store.create_playlist(name) {
            Ok(id) => id,
            Err(crate::core::data_store::DataStoreError::PlaylistNameTaken(_)) => {
                let existing = ctx
                    .data_store
                    .get_all_playlists()?
                    .into_iter()
                    .find(|p| p.name == name)
                    .context("Failed to find playlist that should exist")?;
                ctx.data_store.delete_playlist(existing.id)?;
                ctx.data_store.create_playlist(name)?
            }
            Err(e) => return Err(e.into()),
        };

        for item in items.iter() {
            if let PlaylistItem::Local(path) = item {
                ctx.data_store.add_local_file_to_playlist(playlist_id, path)?;
            }
        }

        status_info!("Saved {} items to playlist '{}'", items.len(), name);
        ctx.app_event_sender
            .send(AppEvent::UiEvent(UiAppEvent::RefreshRmpcPlaylists))?;
        Ok(())
    }

    fn on_load_command(&mut self, name: &str, ctx: &mut Ctx) -> Result<()> {
        let all_playlists = ctx.data_store.get_all_playlists()?;
        let items = match all_playlists.into_iter().find(|p| p.name == name) {
            Some(p) => p.items,
            None => {
                status_error!("Failed to load playlist '{}': not found.", name);
                return Ok(());
            }
        };

        let library_videos = self.get_youtube_master_library(ctx)?;

        status_info!("Loading {} items from playlist '{}'...", items.len(), name);
        for item in items {
            match item {
                PlaylistItem::Local(path) => {
                    ctx.command(move |client| {
                        client.add(&path, None)?;
                        Ok(())
                    });
                }
                PlaylistItem::YouTube(video) => {
                    let id = video.youtube_id;
                    if let Some(video) = library_videos.get(&id) {
                        ctx.work_sender.send(WorkRequest::GetYouTubeStreamUrl {
                            video: video.clone(),
                            context: None,
                        })?;
                    } else {
                        status_warn!("Could not find YouTube video with ID {} in library, skipping.", id);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn on_ui_app_event(&mut self, event: UiAppEvent, ctx: &mut Ctx) -> Result<()> {
        match event {
            UiAppEvent::Modal(modal) => {
                let existing_modal = modal.replacement_id().and_then(|id| {
                    self.modals
                        .iter_mut()
                        .find(|m| m.replacement_id().as_ref().is_some_and(|m_id| *m_id == id))
                });

                if let Some(existing_modal) = existing_modal {
                    *existing_modal = modal;
                } else {
                    self.modals.push(modal);
                }

                self.on_event(UiEvent::ModalOpened, ctx)?;
                ctx.render()?;
            }
            UiAppEvent::PopConfigErrorModal => {
                let original_len = self.modals.len();
                self.modals
                    .retain(|m| m.replacement_id().is_none_or(|id| id != ERROR_CONFIG_MODAL_ID));
                let new_len = self.modals.len();
                if new_len == 0 {
                    self.on_event(UiEvent::ModalClosed, ctx)?;
                }
                if original_len != new_len {
                    ctx.render()?;
                }
            }
            UiAppEvent::PopModal(id) => {
                let original_len = self.modals.len();
                self.modals.retain(|m| m.id() != id);
                let new_len = self.modals.len();
                if new_len == 0 {
                    self.on_event(UiEvent::ModalClosed, ctx)?;
                }
                if original_len != new_len {
                    ctx.render()?;
                }
            }
            UiAppEvent::ChangeTab(tab_name) => {
                self.change_tab(tab_name, ctx)?;
                ctx.render()?;
            }
            UiAppEvent::YouTubeLibraryRemoveVideo(video_id) => {
                self.on_youtube_library_remove_video(&video_id, ctx)?;
            }
            UiAppEvent::ExecuteCommand(cmd_str) => {
                self.handle_command(cmd_str, ctx)?;
            }
            UiAppEvent::RefreshRmpcPlaylists => {
                if let Panes::RmpcPlaylists(p) = self.panes.get_mut(&PaneType::RmpcPlaylists, ctx)? {
                    p.refresh_playlists(ctx)?;
                }
                ctx.render()?;
            }
            UiAppEvent::AddPlaylistItemsToQueue(items) => {
                self.add_playlist_items_to_queue(items, false, ctx)?;
            }
            UiAppEvent::ReplaceQueueWithPlaylistItems(items) => {
                self.add_playlist_items_to_queue(items, true, ctx)?;
            }
            UiAppEvent::AddItemsToPlaylist { name, items } => {
                self.on_add_items_to_playlist(&name, items, ctx)?;
            }
            UiAppEvent::CreatePlaylistFromItems { name, items } => {
                self.on_create_playlist_from_items(&name, items, ctx)?;
            }
            UiAppEvent::ImportYouTubeLibrary { path } => {
                self.on_import_youtube_library(path, ctx)?;
            }
            UiAppEvent::ImportYouTubePlaylists { path } => {
                self.on_import_youtube_playlists(path, ctx)?;
            }
            UiAppEvent::FinalizePlaylistImport => {
                self.on_finalize_playlist_import(ctx)?;
            }
            UiAppEvent::ClearStatusMessage => {
                ctx.messages.clear();
            }
        }
        Ok(())
    }

    pub fn handle_command(&mut self, cmd_str: String, ctx: &mut Ctx) -> Result<()> {
        let parts: Vec<&str> = cmd_str.split_whitespace().collect();
        match parts.as_slice() {
            ["save", name] => {
                self.on_save_command(name, ctx)?;
            }
            ["load", name] => {
                self.on_load_command(name, ctx)?;
            }
            ["importlibrary", path] => {
                ctx.app_event_sender
                    .send(AppEvent::UiEvent(UiAppEvent::ImportYouTubeLibrary {
                        path: PathBuf::from(path),
                    }))?;
            }
            ["importplaylists", path] => {
                ctx.app_event_sender
                    .send(AppEvent::UiEvent(UiAppEvent::ImportYouTubePlaylists {
                        path: PathBuf::from(path),
                    }))?;
            }
            _ => {
                // Fallback to original behavior
                if let Ok(Args { command: Some(cmd), .. }) = cmd_str.parse() {
                    if ctx.work_sender.send(WorkRequest::Command(cmd)).is_err() {
                        log::error!("Failed to send command");
                    }
                } else {
                    status_error!("Unknown command or invalid arguments: {}", cmd_str);
                }
            }
        }
        Ok(ctx.render()?)
    }

    pub fn resize(&mut self, area: Rect, ctx: &Ctx) -> Result<()> {
        log::trace!(area:?; "Terminal was resized");
        self.calc_areas(area, ctx);

        self.layout.for_each_pane(self.area, &mut |pane, pane_area, _, _| {
            match self.panes.get_mut(&pane.pane, ctx)? {
                Panes::TabContent => {
                    active_tab_call!(self, ctx, resize(pane_area, ctx))?;
                }
                mut pane_instance => {
                    pane_call!(pane_instance, calculate_areas(pane_area, ctx))?;
                    pane_call!(pane_instance, resize(pane_area, ctx))?;
                }
            }
            Ok(())
        })
    }

    pub fn on_event(&mut self, mut event: UiEvent, ctx: &mut Ctx) -> Result<()> {
        match event {
            UiEvent::Exit => {}
            UiEvent::Database => {
                status_warn!(
                    "The music database has been updated. Some parts of the UI may have been reinitialized to prevent inconsistent behaviours."
                );
            }
            UiEvent::ConfigChanged => {
                // Call on_hide for all panes in the current tab and current layout because they
                // might not be visible after the change
                self.layout.for_each_pane(self.area, &mut |pane, _, _, _| {
                    match self.panes.get_mut(&pane.pane, ctx)? {
                        Panes::TabContent => {
                            active_tab_call!(self, ctx, on_hide(ctx))?;
                        }
                        mut pane_instance => {
                            pane_call!(pane_instance, on_hide(ctx))?;
                        }
                    }
                    Ok(())
                })?;

                self.layout = ctx.config.theme.layout.clone();
                let new_active_tab = ctx
                    .config
                    .tabs
                    .names
                    .iter()
                    .find(|tab| tab == &&ctx.active_tab)
                    .or(ctx.config.tabs.names.first())
                    .context("Expected at least one tab")?;

                let mut old_other_panes = std::mem::take(&mut self.panes.others);
                for (key, new_other_pane) in PaneContainer::init_other_panes(ctx) {
                    let old = old_other_panes.remove(&key);
                    self.panes.others.insert(key, old.unwrap_or(new_other_pane));
                }
                // We have to be careful about the order of operations here as they might cause
                // a panic if done incorrectly
                self.tabs = Self::init_tabs(ctx)?;
                ctx.active_tab = new_active_tab.clone();
                self.on_event(UiEvent::TabChanged(new_active_tab.clone()), ctx)?;

                // Call before_show here, because we have "hidden" all the panes before and this
                // will force them to reinitialize
                self.before_show(self.area, ctx)?;
            }
            _ => {}
        }

        for pane_type in &ctx.config.active_panes {
            let visible = self
                .tabs
                .get(&ctx.active_tab)
                .is_some_and(|tab| tab.panes.panes_iter().any(|pane| pane.pane == *pane_type))
                || self.layout.panes_iter().any(|pane| pane.pane == *pane_type);

            match self.panes.get_mut(pane_type, ctx)? {
                #[cfg(debug_assertions)]
                Panes::Logs(p) => p.on_event(&mut event, visible, ctx),
                Panes::Queue(p) => p.on_event(&mut event, visible, ctx),
                Panes::Directories(p) => p.on_event(&mut event, visible, ctx),
                Panes::Albums(p) => p.on_event(&mut event, visible, ctx),
                Panes::Artists(p) => p.on_event(&mut event, visible, ctx),
                Panes::RmpcPlaylists(p) => p.on_event(&mut event, visible, ctx),
                Panes::Search(p) => p.on_event(&mut event, visible, ctx),
                Panes::YouTube(p) => p.on_event(&mut event, visible, ctx),
                Panes::AlbumArtists(p) => p.on_event(&mut event, visible, ctx),
                Panes::AlbumArt(p) => p.on_event(&mut event, visible, ctx),
                Panes::Lyrics(p) => p.on_event(&mut event, visible, ctx),
                Panes::ProgressBar(p) => p.on_event(&mut event, visible, ctx),
                Panes::Header(p) => p.on_event(&mut event, visible, ctx),
                Panes::Tabs(p) => p.on_event(&mut event, visible, ctx),
                #[cfg(debug_assertions)]
                Panes::FrameCount(p) => p.on_event(&mut event, visible, ctx),
                Panes::Others(p) => p.on_event(&mut event, visible, ctx),
                Panes::Cava(p) => p.on_event(&mut event, visible, ctx),
                // Property and the dummy TabContent pane do not need to receive events
                Panes::Property(_) | Panes::TabContent => Ok(()),
            }?;
        }

        for modal in &mut self.modals {
            modal.on_event(&mut event, ctx)?;
        }

        Ok(())
    }

    pub(crate) fn on_command_finished(
        &mut self,
        id: &'static str,
        pane: Option<PaneType>,
        data: MpdQueryResult,
        ctx: &mut Ctx,
    ) -> Result<()> {
        match pane {
            Some(pane_type) => {
                let visible =
                    self.tabs.get(&ctx.active_tab).is_some_and(|tab| {
                        tab.panes.panes_iter().any(|pane| pane.pane == pane_type)
                    }) || self.layout.panes_iter().any(|pane| pane.pane == pane_type);

                match self.panes.get_mut(&pane_type, ctx)? {
                    #[cfg(debug_assertions)]
                    Panes::Logs(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::Queue(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::Directories(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::Albums(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::Artists(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::RmpcPlaylists(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::Search(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::YouTube(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::AlbumArtists(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::AlbumArt(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::Lyrics(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::ProgressBar(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::Header(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::Tabs(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::Others(p) => p.on_query_finished(id, data, visible, ctx),
                    #[cfg(debug_assertions)]
                    Panes::FrameCount(p) => p.on_query_finished(id, data, visible, ctx),
                    Panes::Cava(p) => p.on_query_finished(id, data, visible, ctx),
                    // Property and the dummy TabContent pane do not need to receive command
                    // notifications
                    Panes::Property(_) | Panes::TabContent => Ok(()),
                }?;
            }
            None => match (id, data) {
                (OPEN_OUTPUTS_MODAL, MpdQueryResult::Outputs(outputs)) => {
                    modal!(ctx, OutputsModal::new(outputs));
                }
                (OPEN_DECODERS_MODAL, MpdQueryResult::Decoders(decoders)) => {
                    modal!(ctx, DecodersModal::new(decoders));
                }
                (
                    "add_youtube_song" | "refresh_youtube_song",
                    MpdQueryResult::YouTubeSongAdded { song_id, video },
                ) => {
                    ctx.data_store
                        .add_youtube_song_to_queue(song_id, &video.youtube_id)?;
                    ctx.data_store.add_video_to_library(&video)?;
                    ctx.youtube_library
                        .insert(video.youtube_id.clone(), video.clone());
                    ctx.queue_youtube_ids.insert(song_id, video.youtube_id.clone());
                    if let Panes::YouTube(p) = self.panes.get_mut(&PaneType::YouTube, ctx)? {
                        p.add_video(video.clone());
                    }
                    if let Panes::RmpcPlaylists(p) =
                        self.panes.get_mut(&PaneType::RmpcPlaylists, ctx)?
                    {
                        p.add_video(&video);
                    }
                }
                (id, mut data) => {
                    // TODO a proper modal target
                    for modal in &mut self.modals {
                        modal.on_query_finished(id, &mut data, ctx)?;
                    }
                }
            },
        }

        Ok(())
    }
}

#[derive(Debug)]
pub enum UiAppEvent {
    Modal(Box<dyn Modal + Send + Sync>),
    PopModal(Id),
    PopConfigErrorModal,
    ChangeTab(TabName),
    YouTubeLibraryRemoveVideo(String),
    ExecuteCommand(String),
    RefreshRmpcPlaylists,
    AddPlaylistItemsToQueue(Vec<PlaylistItem>),
    ReplaceQueueWithPlaylistItems(Vec<PlaylistItem>),
    AddItemsToPlaylist {
        name: String,
        items: Vec<PlaylistItem>,
    },
    CreatePlaylistFromItems {
        name: String,
        items: Vec<PlaylistItem>,
    },
    ImportYouTubeLibrary {
        path: PathBuf,
    },
    ImportYouTubePlaylists {
        path: PathBuf,
    },
    FinalizePlaylistImport,
    ClearStatusMessage,
}

#[derive(Debug, Eq, Hash, PartialEq)]
#[allow(dead_code)]
pub enum UiEvent {
    Player,
    Database,
    Output,
    StoredPlaylist,
    LogAdded(Vec<u8>),
    ModalOpened,
    ModalClosed,
    Exit,
    LyricsIndexed,
    SongChanged,
    Reconnected,
    TabChanged(TabName),
    Displayed,
    Hidden,
    ConfigChanged,
    PlaybackStateChanged,
}

impl TryFrom<IdleEvent> for UiEvent {
    type Error = ();

    fn try_from(event: IdleEvent) -> Result<Self, ()> {
        Ok(match event {
            IdleEvent::Player => UiEvent::Player,
            IdleEvent::Database => UiEvent::Database,
            IdleEvent::StoredPlaylist => UiEvent::StoredPlaylist,
            IdleEvent::Output => UiEvent::Output,
            _ => return Err(()),
        })
    }
}

pub enum KeyHandleResult {
    None,
    Quit,
}

impl From<&Level> for Color {
    fn from(value: &Level) -> Self {
        match value {
            Level::Info => Color::Blue,
            Level::Warn => Color::Yellow,
            Level::Error => Color::Red,
            Level::Debug => Color::LightGreen,
            Level::Trace => Color::Magenta,
        }
    }
}

impl Level {
    pub fn into_style(self, config: &LevelStyles) -> Style {
        match self {
            Level::Trace => config.trace,
            Level::Debug => config.debug,
            Level::Warn => config.warn,
            Level::Error => config.error,
            Level::Info => config.info,
        }
    }
}

impl From<&FilterKind> for &'static str {
    fn from(value: &FilterKind) -> Self {
        match value {
            FilterKind::Exact => "Exact match",
            FilterKind::Contains => "Contains value",
            FilterKind::StartsWith => "Starts with value",
            FilterKind::Regex => "Regex",
        }
    }
}

impl std::fmt::Display for FilterKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FilterKind::Exact => write!(f, "Exact match"),
            FilterKind::Contains => write!(f, "Contains value"),
            FilterKind::StartsWith => write!(f, "Starts with value"),
            FilterKind::Regex => write!(f, "Regex"),
        }
    }
}

impl FilterKind {
    fn cycle(&mut self) -> &mut Self {
        *self = match self {
            FilterKind::Exact => FilterKind::Contains,
            FilterKind::Contains => FilterKind::StartsWith,
            FilterKind::StartsWith => FilterKind::Regex,
            FilterKind::Regex => FilterKind::Exact,
        };
        self
    }
}

impl Config {
    fn next_screen(&self, current_screen: &TabName) -> TabName {
        let names = &self.tabs.names;
        names
            .iter()
            .enumerate()
            .find(|(_, s)| *s == current_screen)
            .and_then(|(idx, _)| names.get((idx + 1) % names.len()))
            .unwrap_or(current_screen)
            .clone()
    }

    fn prev_screen(&self, current_screen: &TabName) -> TabName {
        let names = &self.tabs.names;
        self.tabs
            .names
            .iter()
            .enumerate()
            .find(|(_, s)| *s == current_screen)
            .and_then(|(idx, _)| {
                names.get((if idx == 0 { names.len() - 1 } else { idx - 1 }) % names.len())
            })
            .unwrap_or(current_screen)
            .clone()
    }

    fn as_header_table_block(&self) -> ratatui::widgets::Block {
        if !self.theme.draw_borders {
            return ratatui::widgets::Block::default();
        }
        Block::default().border_style(self.as_border_style())
    }

    fn as_tabs_block<'block>(&self) -> ratatui::widgets::Block<'block> {
        if !self.theme.draw_borders {
            return ratatui::widgets::Block::default()/* .padding(Padding::new(0, 0, 1, 1)) */;
        }

        ratatui::widgets::Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_set(border::ONE_EIGHTH_WIDE)
            .border_style(self.as_border_style())
    }

    fn as_border_style(&self) -> ratatui::style::Style {
        self.theme.borders_style
    }

    fn as_focused_border_style(&self) -> ratatui::style::Style {
        self.theme.highlight_border_style
    }

    fn as_text_style(&self) -> ratatui::style::Style {
        self.theme.text_color.map(|color| Style::default().fg(color)).unwrap_or_default()
    }

    fn as_styled_progress_bar(&self) -> widgets::progress_bar::ProgressBar {
        let progress_bar_colors = &self.theme.progress_bar;
        widgets::progress_bar::ProgressBar::default()
            .elapsed_style(progress_bar_colors.elapsed_style)
            .thumb_style(progress_bar_colors.thumb_style)
            .track_style(progress_bar_colors.track_style)
            .start_char(&self.theme.progress_bar.symbols[0])
            .elapsed_char(&self.theme.progress_bar.symbols[1])
            .thumb_char(&self.theme.progress_bar.symbols[2])
            .track_char(&self.theme.progress_bar.symbols[3])
            .end_char(&self.theme.progress_bar.symbols[4])
    }

    fn as_styled_scrollbar(&self) -> Option<ratatui::widgets::Scrollbar> {
        let scrollbar = self.theme.scrollbar.as_ref()?;
        let symbols = &scrollbar.symbols;
        Some(
            ratatui::widgets::Scrollbar::default()
                .orientation(ratatui::widgets::ScrollbarOrientation::VerticalRight)
                .track_symbol(if symbols[0].is_empty() { None } else { Some(&symbols[0]) })
                .thumb_symbol(&scrollbar.symbols[1])
                .begin_symbol(if symbols[2].is_empty() { None } else { Some(&symbols[2]) })
                .end_symbol(if symbols[3].is_empty() { None } else { Some(&symbols[3]) })
                .track_style(scrollbar.track_style)
                .begin_style(scrollbar.ends_style)
                .end_style(scrollbar.ends_style)
                .thumb_style(scrollbar.thumb_style),
        )
    }
}

