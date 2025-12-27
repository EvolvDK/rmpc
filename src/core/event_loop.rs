use crate::{
    config::{cli::RemoteCommandQuery, Config},
    ctx::Ctx,
    mpd::{
        commands::{IdleEvent, Song, State, Status},
        errors::{ErrorCode, MpdError},
        mpd_client::{MpdClient, SaveMode},
    },
    youtube::service::YouTubeService,
    shared::{
        events::{AppEvent, ClientRequest, IdentifiedYouTubeSong, WorkDone, WorkRequest, YouTubeStreamContext},
        ext::error::ErrorExt,
        id::{self, Id},
        macros::{status_error, status_warn},
        mpd_query::{
            run_status_update, EXTERNAL_COMMAND, GLOBAL_QUEUE_UPDATE, GLOBAL_STATUS_UPDATE,
            GLOBAL_VOLUME_UPDATE, MpdQueryResult,
        },
    },
    core::scheduler::TaskGuard,
    ui::{
        modals::info_modal::InfoModal, KeyHandleResult, StatusMessage, Ui, UiAppEvent, UiEvent,
    },
};
use anyhow::Result;
use crossbeam::channel::{Receiver, RecvTimeoutError, Sender};
use ratatui::{Terminal, layout::Rect, prelude::Backend};
use std::{
    collections::HashSet,
    io::Write,
    ops::Sub,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use super::{
	command::{create_env, run_external},
	work,
};

static ON_RESIZE_SCHEDULE_ID: LazyLock<Id> = LazyLock::new(id::new);

// The `App` struct encapsulates all state and logic for the main event loop.
// This is the heart of the application's runtime.
struct App<'a, B: Backend + Write> {
    ctx: Ctx,
    terminal: Terminal<B>,
    ui: Ui<'a>,
    last_render: Instant,
    min_frame_duration: Duration,
    update_loop_guard: Option<TaskGuard<(Sender<AppEvent>, Sender<ClientRequest>)>>,
    update_db_loop_guard: Option<TaskGuard<(Sender<AppEvent>, Sender<ClientRequest>)>>,
    tmux: Option<crate::shared::tmux::TmuxHooks>,
    is_connected: bool,
    render_wanted: bool,
}

impl<'a, B: Backend + Write> App<'a, B> {
    /// Creates a new App instance, taking ownership of the terminal and UI.
    fn new(mut ctx: Ctx, terminal: Terminal<B>) -> Result<Self> {
		let size = terminal.size().expect("To be able to get terminal size");
		let area = Rect::new(0, 0, size.width, size.height);
        let mut ui = Ui::new(&ctx)?;
        ui.before_show(area, &mut ctx).expect("Initial render init to succeed");

        let max_fps = f64::from(ctx.config.max_fps);
        let min_frame_duration = Duration::from_secs_f64(1f64 / max_fps);

        Ok(Self {
            ctx,
            terminal,
            ui,
            last_render: Instant::now().sub(Duration::from_secs(10)),
            min_frame_duration,
            update_loop_guard: None,
            update_db_loop_guard: None,
            tmux: None,
            is_connected: true,
            render_wanted: true,
        })
    }
    
    /// Initializes tmux hooks and runs startup tasks.
    fn init_startup_tasks(&mut self) {
        self.tmux = match crate::shared::tmux::TmuxHooks::new() {
            Ok(Some(val)) => Some(val),
            Ok(None) => None,
            Err(err) => {
                log::error!(error:? = err; "Failed to install tmux hooks");
                None
            }
        };

        if self.ctx.config.exec_on_song_change_at_start
            && self.ctx.find_current_song_in_queue().is_some()
            && let Some(command) = &self.ctx.config.on_song_change
        {
            run_external(command.clone(), create_env(&self.ctx, std::iter::empty()));
        }

        match self.ctx.status.state {
            State::Play => {
                self.update_loop_guard = self
                    .ctx
                    .config
                    .status_update_interval_ms
                    .map(Duration::from_millis)
                    .map(|interval| self.ctx.scheduler.repeated(interval, run_status_update));
                self.ctx.song_played = Some(self.ctx.status.elapsed);
            }
            State::Pause => {
                self.ctx.song_played = Some(self.ctx.status.elapsed);
            }
            State::Stop => {}
        }
    }
    
    /// Runs the main event loop until a quit event is received.
    fn run(&mut self, event_rx: Receiver<AppEvent>) -> Result<()> {
        self.init_startup_tasks();
        while self.run_tick(&event_rx)? != KeyHandleResult::Quit {}
        Ok(())
    }
    
    /// Processes one "tick" of the event loop.
    fn run_tick(&mut self, event_rx: &Receiver<AppEvent>) -> Result<KeyHandleResult> {
        let now = Instant::now();
        let timeout = if self.render_wanted {
            self.min_frame_duration.checked_sub(now - self.last_render).unwrap_or_default()
        } else {
            // If no render is needed, wait a long time for the next event.
            Duration::from_secs(60)
        };

        // Poll for events
        let event = match event_rx.recv_timeout(timeout) {
            Ok(event) => Some(event),
            Err(RecvTimeoutError::Timeout) => None, // Timeout is for rendering, not an error
            Err(RecvTimeoutError::Disconnected) => return Ok(KeyHandleResult::Quit),
        };

        // Handle the event if one was received
        if let Some(app_event) = event {
            if self.handle_app_event(app_event)? == KeyHandleResult::Quit {
                return Ok(KeyHandleResult::Quit);
            }
        }

        // Render the UI if needed
        if self.render_wanted {
            self.render()?;
        }

        Ok(KeyHandleResult::Handled)
    }
    
    /// Renders one frame of the UI.
    fn render(&mut self) -> Result<()> {
        self.terminal.draw(|frame| {
            if let Err(err) = self.ui.render(frame, &mut self.ctx) {
                log::error!(error:? = err; "Failed to render a frame");
            }
        })?;
        self.last_render = Instant::now();
        self.render_wanted = false;
        self.ctx.finish_frame();
        Ok(())
    }
    
    /// The main event dispatcher. All events from the channel are processed here.
    fn handle_app_event(&mut self, event: AppEvent) -> Result<KeyHandleResult> {
        let key_handle_result = KeyHandleResult::Handled;
        match event {
            AppEvent::UserKeyInput(key_event) => {
                match self.ui.handle_key(&mut key_event.into(), &mut self.ctx) {
                    Ok(KeyHandleResult::Ignored) => {
                        // Key was not handled, continue processing
                    },
                    Ok(KeyHandleResult::Quit) => {
                        if let Err(err) = self.ui.on_event(UiEvent::Exit, &mut self.ctx) {
                            log::error!(error:? = err, event:?; "UI failed to handle quit event");
                        }
                        return Ok(KeyHandleResult::Quit);
                    }
                    Ok(KeyHandleResult::Handled) => {
                        // Key was handled, continue processing
                    }
                    Err(err) => {
                        status_error!(err:?; "Error: {}", err.to_status());
                        log::error!(error:? = err; "Error handling key input");
                        self.render_wanted = true;
                    }
                }
            },
            AppEvent::UserMouseInput(mouse_event) => self.handle_mouse_input(mouse_event)?,
            AppEvent::ConfigChanged { config, keep_old_theme } => {
                self.handle_config_change(*config, keep_old_theme)?
            }
            AppEvent::ThemeChanged { theme } => self.handle_theme_change(*theme)?,
            AppEvent::Status(msg, level, timeout) => self.handle_status_message(msg, level, timeout),
            AppEvent::InfoModal { message, title, size, replacement_id } => {
                self.handle_info_modal(message, title, size, replacement_id)?
            }
            AppEvent::IdleEvent(event) => self.handle_idle_event(event)?,
            AppEvent::WorkDone(result) => self.handle_work_done(result)?,
            AppEvent::Resized { columns, rows } => self.handle_resize(columns, rows),
            AppEvent::ResizedDebounced { columns, rows } => self.handle_resize_debounced(columns, rows)?,
            AppEvent::LostConnection => self.handle_connection_lost(),
            AppEvent::Reconnected => self.handle_reconnected()?,
            AppEvent::TmuxHook { hook } => self.handle_tmux_hook(hook)?,
            AppEvent::UiEvent(event) => self.ui.on_ui_app_event(event, &mut self.ctx)?,
            // Other events are handled inline for simplicity
            AppEvent::RequestRender => {}
            AppEvent::Log(msg) => self.ui.on_event(UiEvent::LogAdded(msg), &mut self.ctx)?,
            AppEvent::RemoteSwitchTab { tab_name } => {
				self.ui.on_ui_app_event(UiAppEvent::ChangeTab(tab_name.into()), &mut self.ctx)?
			}
            AppEvent::IpcQuery { mut stream, targets } => {
                for target in targets {
                    match target {
                        RemoteCommandQuery::ActiveTab => {
                            stream
                                .insert_response(target.to_string(), self.ctx.active_tab.0.as_str());
                        }
                    }
                }
			}
        };
        // Any event that was handled likely requires a re-render.
        self.render_wanted = true;
        Ok(key_handle_result)
    }
    
    // --- Specific Event Handlers (SRP Applied) ---
    
    fn handle_mouse_input(&mut self, mouse_event: crate::shared::mouse_event::MouseEvent) -> Result<()> {
        self.ui.handle_mouse_event(mouse_event, &mut self.ctx)
    }
    
    fn handle_config_change(&mut self, mut new_config: Config, keep_old_theme: bool) -> Result<()> {
        new_config.album_art.method = self.ctx.config.album_art.method;
        if keep_old_theme {
            new_config.theme = self.ctx.config.theme.clone();
        }
        if let Err(err) = new_config.validate() {
            status_error!(error:? = err; "Cannot change config, invalid value: '{err}'");
            return Ok(());
        }
        self.ctx.config = Arc::new(new_config);
        self.min_frame_duration = Duration::from_secs_f64(1f64 / f64::from(self.ctx.config.max_fps));
        self.ui.on_event(UiEvent::ConfigChanged, &mut self.ctx)?;
        self.terminal.clear()?;
        Ok(())
    }
    
    fn handle_theme_change(&mut self, theme: crate::config::theme::UiConfig) -> Result<()> {
        let mut config = self.ctx.config.as_ref().clone();
        config.theme = theme;
        if let Err(err) = config.validate() {
            status_error!(error:? = err; "Cannot change theme, invalid config: '{err}'");
            return Ok(());
        }
        self.ctx.config = Arc::new(config);
        self.ui.on_event(UiEvent::ConfigChanged, &mut self.ctx)?;
        self.terminal.clear()?;
        Ok(())
    }

    fn handle_status_message(&mut self, message: String, level: crate::shared::events::Level, timeout: Duration) {
        self.ctx.messages.push(StatusMessage { level, timeout, message, created: Instant::now() });
        self.ctx.scheduler.schedule(timeout, |(tx, _)| Ok(tx.send(AppEvent::RequestRender)?));
    }

    fn handle_info_modal(&mut self, message: Vec<String>, title: Option<String>, size: Option<crate::config::Size>, replacement_id: Option<String>) -> Result<()> {
        let modal = InfoModal::builder()
            .ctx(&self.ctx)
            .maybe_title(title)
            .maybe_size(size)
            .maybe_replacement_id(replacement_id)
            .message(message)
            .build();
        self.ui.on_ui_app_event(UiAppEvent::Modal(Box::new(modal)), &mut self.ctx)
    }
    
    fn handle_idle_event(&mut self, event: IdleEvent) -> Result<()> {
        let mut ui_events = HashSet::new();
        handle_idle_event(event, &self.ctx, &mut ui_events);
        for ev in ui_events {
            self.ui.on_event(ev, &mut self.ctx)?;
        }
        Ok(())
    }

    fn handle_resize(&mut self, columns: u16, rows: u16) {
        self.ctx.scheduler.schedule_replace(
            *ON_RESIZE_SCHEDULE_ID,
            Duration::from_millis(500),
            move |(tx, _)| Ok(tx.send(AppEvent::ResizedDebounced { columns, rows })?),
        );
    }

    fn handle_resize_debounced(&mut self, columns: u16, rows: u16) -> Result<()> {
        self.ui.resize(Rect::new(0, 0, columns, rows), &self.ctx)?;
        if let Some(cmd) = &self.ctx.config.on_resize {
            let mut env = create_env(&self.ctx, std::iter::empty::<&str>());
            env.push(("COLS".to_owned(), columns.to_string()));
            env.push(("ROWS".to_owned(), rows.to_string()));
            run_external(cmd.clone(), env);
        }
        self.terminal.clear()?;
        Ok(())
    }
    
    fn handle_reconnected(&mut self) -> Result<()> {
        for ev in [IdleEvent::Player, IdleEvent::Playlist, IdleEvent::Options] {
            self.handle_idle_event(ev)?;
        }
        self.ui.on_event(UiEvent::Reconnected, &mut self.ctx)?;
        status_warn!("rmpc reconnected to MPD and will reinitialize");
        self.is_connected = true;
        Ok(())
    }

    fn handle_connection_lost(&mut self) {
        if self.ctx.status.state != State::Stop {
            self.update_loop_guard = None;
            self.ctx.status.state = State::Stop;
        }
        if self.is_connected {
            status_error!("rmpc lost connection to MPD and will try to reconnect");
        }
        self.is_connected = false;
    }

    fn handle_tmux_hook(&mut self, _hook: String) -> Result<()> {
        if let Some(tmux) = &mut self.tmux {
            let old_visible = tmux.visible;
            tmux.update_visible()?;
            let event = match (tmux.visible, old_visible) {
                (true, false) => UiEvent::Displayed,
                (false, true) => UiEvent::Hidden,
                _ => return Ok(()),
            };
            self.ui.on_event(event, &mut self.ctx)?;
        }
        Ok(())
    }
    
    fn handle_work_done(&mut self, result: Result<WorkDone>) -> Result<()> {
        match result {
            Ok(work) => match work {
                WorkDone::LyricsIndexed { index } => {
                    self.ctx.lrc_index = index;
                    self.ui.on_event(UiEvent::LyricsIndexed, &mut self.ctx)?;
                }
                WorkDone::SingleLrcIndexed { lrc_entry } => {
                    if let Some(lrc_entry) = lrc_entry {
                        self.ctx.lrc_index.add(lrc_entry);
                    }
                    self.ui.on_event(UiEvent::LyricsIndexed, &mut self.ctx)?;
                }
                WorkDone::YouTubeSearchResult { song_info, generation } => {
                    self.ui.on_youtube_search_result(song_info, generation, &mut self.ctx)?;
                }
                WorkDone::YouTubeSearchFinished { generation } => {
                    self.ui.on_youtube_search_complete(generation, &mut self.ctx)?;
                }
                WorkDone::YouTubeStreamUrlReady { url, song, context } => {
                    self.ui.on_youtube_stream_url_ready(url, song, context, &mut self.ctx)?;
                }
                WorkDone::YouTubeStreamUrlFailed { song, context, error } => {
                    if let IdentifiedYouTubeSong::Full(full_song) = song {
                        self.ui.on_youtube_stream_url_failed(full_song, context, error, &mut self.ctx)?;
                    } else {
                        status_error!("Stream URL failed for a song with only an ID, cannot display info.");
                    }
                }
                WorkDone::YouTubeSongInfoFetched { song, context } => {
                    if let Some(stream_context) = context.as_ref() {
                        let request = WorkRequest::GetYouTubeStreamUrl {
                            song: IdentifiedYouTubeSong::Full(song.clone()),
                            context: Some(stream_context.clone()),
                        };
                        self.ctx.work_sender.send(request)?;
                    }
                    self.ui.on_youtube_song_info_fetched(song, context, &mut self.ctx)?;
                }
                WorkDone::YouTubeSongInfoFailed { id, error, context } => {
                    status_warn!("Could not fetch info for YouTube video ID '{}': {}", id, error);
                    if context.is_some() {
                        status_error!("Failed to recover expired stream for video ID '{}'.", id);
                    }
                    self.ui.on_youtube_import_item_failed(&mut self.ctx)?;
                }
                WorkDone::MpdCommandFinished { id, target, data } => {
                    // This logic prevents move errors by checking for global handlers before matching and moving data.
                    let is_global_event = if target.is_none() {
                        matches!((id, &data), (GLOBAL_STATUS_UPDATE, MpdQueryResult::Status { .. })
                            | (GLOBAL_VOLUME_UPDATE, MpdQueryResult::Volume(_))
                            | (GLOBAL_QUEUE_UPDATE, MpdQueryResult::Queue(_))
                            | (EXTERNAL_COMMAND, MpdQueryResult::ExternalCommand(_, _)))
                    } else {
                        false
                    };

                    if is_global_event {
                        match (id, data) {
                            (
                                GLOBAL_STATUS_UPDATE,
                                MpdQueryResult::Status { data, source_event },
                            ) => self.process_global_status_update(data, source_event)?,
                            (GLOBAL_VOLUME_UPDATE, MpdQueryResult::Volume(volume)) => {
                                self.ctx.status.volume = volume
                            }
                            (GLOBAL_QUEUE_UPDATE, MpdQueryResult::Queue(queue)) => {
                                self.process_global_queue_update(queue.unwrap_or_default())?
                            }
                            (
                                EXTERNAL_COMMAND,
                                MpdQueryResult::ExternalCommand(cmd, songs),
                            ) => {
                                run_external(
                                    cmd,
                                    create_env(&self.ctx, songs.iter().map(|s| s.file.as_str())),
                                );
                            }
                            _ => unreachable!("Should be covered by is_global_event check"),
                        }
                    } else {
                        self.ui.on_command_finished(id, target, data, &mut self.ctx)?
                    }
                }
                WorkDone::None => {}
            },
            Err(err) => {
                if !try_handle_expired_stream_error(&err, &mut self.ctx, &mut self.ui) {
                    status_error!("{}", err);
                }
            }
        }
        Ok(())
    }
    
    fn process_global_status_update(&mut self, new_status: Status, source_event: Option<IdleEvent>) -> Result<()> { 
        let previous_status = std::mem::replace(&mut self.ctx.status, new_status);
        let previous_state = previous_status.state;
        let previous_song_id = previous_status.songid;

        if self.ctx.config.reflect_changes_to_playlist && matches!(source_event, Some(IdleEvent::Playlist)) {
            if let (Some(current_playlist), Some(new_playlist)) = (previous_status.lastloadedplaylist, self.ctx.status.lastloadedplaylist.as_ref()) {
                if &current_playlist == new_playlist {
                    self.ctx.command(move |client| {
                        client.save_queue_as_playlist(&current_playlist, Some(SaveMode::Replace))?;
                        Ok(())
                    });
                }
            }
        }

        match (previous_status.updating_db, self.ctx.status.updating_db) {
            (None, Some(_)) => {
                self.ctx.db_update_start = Some(Instant::now());
                self.update_db_loop_guard = Some(self.ctx.scheduler.repeated(Duration::from_secs(1), |(tx, _)| Ok(tx.send(AppEvent::RequestRender)?)));
            }
            (Some(_), None) => {
                self.ctx.db_update_start = None;
                self.update_db_loop_guard = None;
            }
            _ => {}
        }
        
        if previous_state != self.ctx.status.state {
            self.ui.on_event(UiEvent::PlaybackStateChanged, &mut self.ctx)?;
        }

        match self.ctx.status.state {
            State::Play => {
                if previous_state == State::Play {
                    // Update played duration on continuous playback.
                    if let Some(played) = &mut self.ctx.song_played {
                        *played += self.ctx.last_status_update.elapsed();
                    }
                } else {
                    // Transitioning into Play state, start the update loop.
                    self.update_loop_guard = self.ctx.config.status_update_interval_ms
                        .map(Duration::from_millis)
                        .map(|interval| self.ctx.scheduler.repeated(interval, run_status_update));
                }
            }
            State::Pause | State::Stop => self.update_loop_guard = None,
        }

        if self.ctx.status.songid != previous_song_id {
            if let Some(command) = &self.ctx.config.on_song_change {
                // Populate environment for on_song_change hook.
                let mut env = create_env(&self.ctx, std::iter::empty());

                // Find the file path of the previous song if it existed.
                let prev_song_file = (previous_state != State::Stop)
                    .then_some(
                        previous_song_id
                            .and_then(|id| self.ctx.queue.iter().find(|song| song.id == id))
                            .map(|song| song.file.clone()),
                    )
                    .flatten();

                // Add PREV_SONG and PREV_ELAPSED to the environment.
                if let (Some(prev_file), Some(played_duration)) =
                    (prev_song_file, self.ctx.song_played)
                {
                    env.push(("PREV_SONG".to_owned(), prev_file));
                    env.push((
                        "PREV_ELAPSED".to_owned(),
                        played_duration.as_secs().to_string(),
                    ));
                }

                run_external(command.clone(), env);
            }
            
            // Check if the new song is a YouTube song with incomplete metadata and refresh if needed.
            if let Some(song_id) = self.ctx.status.songid {
                if let Some(yt_id) = self.ctx.queue_youtube_ids.get(&song_id).cloned() {
                    let needs_refresh = if let Some(yt_song) = self.ctx.youtube_library.get(&yt_id) {
                        yt_song.title.eq_ignore_ascii_case("Unknown Title")
                            || yt_song.artist.eq_ignore_ascii_case("Unknown Artist")
                            || yt_song.duration_secs == 0
                    } else {
                        true
                    };

                    if needs_refresh {
                        log::info!(
                            "Metadata missing/incomplete for song {} (YouTube ID: {}), triggering refresh.",
                            song_id,
                            yt_id
                        );
                        let context = YouTubeStreamContext {
                            old_song_id: song_id,
                            position: self.ctx.status.song.unwrap_or(0) as usize,
                            play_after_refresh: self.ctx.status.state == State::Play,
                        };
                        let request = WorkRequest::YouTubeGetSongInfo {
                            id: yt_id,
                            context: Some(context),
                        };
                        if let Err(e) = self.ctx.work_sender.send(request) {
                            log::error!("Failed to send YouTube info fetch request: {}", e);
                        }
                    }
                }
            }
            
            self.ctx.song_played = Some(Duration::ZERO);
            self.ui.on_event(UiEvent::SongChanged, &mut self.ctx)?;
        }
        
        if self.ctx.status.state == State::Stop {
            self.ctx.song_played = None;
        }

        self.ctx.last_status_update = Instant::now();
        Ok(())
    }
    
    fn process_global_queue_update(&mut self, new_queue: Vec<Song>) -> Result<()> {
        self.ctx.data_store.sync_queue_from_mpd(&new_queue)?;
        
        let db_song_ids = self.ctx.data_store.get_all_queue_song_ids()?;
        let new_queue_song_ids: HashSet<u32> = new_queue.iter().map(|s| s.id).collect();
        let ids_to_remove: Vec<u32> = db_song_ids.difference(&new_queue_song_ids).copied().collect();

        if !ids_to_remove.is_empty() {
            self.ctx.data_store.remove_songs_from_queue(&ids_to_remove)?;
        }
        
        self.ctx.queue_youtube_ids = self.ctx.data_store.get_all_queue_youtube_mappings()?;
        self.ctx.queue = new_queue;
        Ok(())
    }
}

/// The public entry point to start the event loop. It creates and runs the App.
pub fn init<B: Backend + Write + Send + 'static>(
    ctx: Ctx,
    event_rx: Receiver<AppEvent>,
    work_rx: Receiver<WorkRequest>,
    terminal: Terminal<B>,
    youtube_service: Arc<dyn YouTubeService>,
) -> std::io::Result<std::thread::JoinHandle<Terminal<B>>> {
    std::thread::Builder::new()
        .name("main".to_owned())
        .spawn(move || {
            // Pass the service to the worker's init function.
            work::init(
                work_rx,
                ctx.client_request_sender.clone(),
                ctx.app_event_sender.clone(),
                ctx.config.clone(),
                youtube_service,
            )
            .expect("Worker initialization failed");

            let mut app = App::new(ctx, terminal).expect("App initialization failed");
            if let Err(e) = app.run(event_rx) {
                log::error!("Event loop terminated with error: {:?}", e);
            }
            app.terminal
        })
}

fn try_handle_expired_stream_error(err: &anyhow::Error, ctx: &mut Ctx, _ui: &mut Ui) -> bool {
    if let Some(MpdError::Mpd(response)) = err.downcast_ref::<MpdError>() {
        if !is_youtube_url_error(&response.code, &response.command, &response.message) {
            return false;
        }

        if let (Some(song_id), Some(pos)) = (ctx.status.songid, ctx.status.song) {
            let context = YouTubeStreamContext {
                old_song_id: song_id,
                position: pos as usize,
                play_after_refresh: true,
            };

            match identify_song_for_refresh(song_id, &response.message, ctx) {
                Some(IdentifiedYouTubeSong::Full(song)) => {
                    // We have full metadata, proceed directly to get the stream URL.
                    let request = WorkRequest::GetYouTubeStreamUrl {
                        song: IdentifiedYouTubeSong::Full(song),
                        context: Some(context),
                    };
                    if let Err(e) = ctx.work_sender.send(request) {
                        log::error!("Failed to send YouTube stream refresh request: {}", e);
                    } else {
                        log::info!("Detected expired YouTube URL, triggering refresh for song: {}", song_id);
                        return true;
                    }
                }
                Some(IdentifiedYouTubeSong::IdOnly(id)) => {
                    // We only have the ID. We must fetch metadata first.
                    log::info!("Identified expired song by ID only ({}). Fetching full metadata before refreshing stream.", id);
                    let request = WorkRequest::YouTubeGetSongInfo {
                        id,
                        context: Some(context), // Pass the context along
                    };
                    if ctx.work_sender.send(request).is_err() {
                        log::error!("Failed to send YouTube info fetch request for refresh.");
                    }
                    return true;
                }
                None => {
                    log::warn!("Could not identify YouTube song for refresh: song_id={}", song_id);
                }
            }
        }
    }
    false
}


/// Determines if an MPD error is related to YouTube URL problems
fn is_youtube_url_error(code: &ErrorCode, command: &str, message: &str) -> bool {
    match code {
        ErrorCode::NoExist if matches!(command, "playid" | "playId") => true,
        ErrorCode::UnknownCmd if matches!(command, "playid" | "playId") => {
            message.contains("HTTP status 403") || message.contains("got HTTP status 4")
        }
        ErrorCode::System => {
            message.contains("HTTP status 403") 
                || message.contains("HTTP status 410")  // Gone
                || message.contains("HTTP status 404")  // Not found
                || message.contains("Failed to decode")
        }
        ErrorCode::PlayerSync if matches!(command, "playid" | "playId") => {
            message.contains("HTTP status") || message.contains("decode")
        }
        _ => false,
    }
}

fn identify_song_for_refresh(
    song_id: u32,
    _error_message: &str,
    ctx: &Ctx,
) -> Option<IdentifiedYouTubeSong> {
    // Priority 1: Get the song from the queue and extract the ID from its URL.
    // This is the most reliable source as it's exactly what was added to MPD.
    let youtube_id = ctx
        .queue
        .iter()
        .find(|s| s.id == song_id)
        .and_then(|s| s.youtube_id().map(String::from))
        // Priority 2: Fallback to database lookup if the song is somehow missing from the queue.
        .or_else(|| {
            log::debug!("Falling back to database lookup for song_id={}", song_id);
            ctx.data_store.get_youtube_id_for_song(song_id).ok().flatten().map(|(id, _)| id)
        })?;

    Some(
        ctx.youtube_library
            .get(&youtube_id)
            .cloned()
            .map(IdentifiedYouTubeSong::Full)
            .unwrap_or(IdentifiedYouTubeSong::IdOnly(youtube_id)),
    )
}

fn handle_idle_event(event: IdleEvent, ctx: &Ctx, result_ui_evs: &mut HashSet<UiEvent>) {
    match event {
        IdleEvent::Mixer if ctx.supported_commands.contains("getvol") => {
            ctx.query()
                .id(GLOBAL_VOLUME_UPDATE)
                .replace_id("volume")
                .query(move |client| Ok(MpdQueryResult::Volume(client.get_volume()?)));
        }
        IdleEvent::Mixer => {
            ctx.query().id(GLOBAL_STATUS_UPDATE).replace_id("status").query(move |client| {
                Ok(MpdQueryResult::Status {
                    data: client.get_status()?,
                    source_event: Some(IdleEvent::Mixer),
                })
            });
        }
        IdleEvent::Options => {
            ctx.query().id(GLOBAL_STATUS_UPDATE).replace_id("status").query(move |client| {
                Ok(MpdQueryResult::Status {
                    data: client.get_status()?,
                    source_event: Some(IdleEvent::Options),
                })
            });
        }
        IdleEvent::Player => {
            ctx.query().id(GLOBAL_STATUS_UPDATE).replace_id("status").query(move |client| {
                Ok(MpdQueryResult::Status {
                    data: client.get_status()?,
                    source_event: Some(IdleEvent::Player),
                })
            });
        }
        IdleEvent::Playlist => {
            let fetch_stickers = ctx.should_fetch_stickers;
            ctx.query().id(GLOBAL_QUEUE_UPDATE).replace_id("playlist").query(move |client| {
                Ok(MpdQueryResult::Queue(client.playlist_info(fetch_stickers)?))
            });
            if ctx.config.reflect_changes_to_playlist {
                // Do not replace because we want to update currently loaded playlist if any
                ctx.query().id(GLOBAL_STATUS_UPDATE).replace_id("status_from_playlist").query(
                    move |client| {
                        Ok(MpdQueryResult::Status {
                            data: client.get_status()?,
                            source_event: Some(IdleEvent::Playlist),
                        })
                    },
                );
            }
        }
        IdleEvent::Sticker => {
            let fetch_stickers = ctx.should_fetch_stickers;
            ctx.query().id(GLOBAL_QUEUE_UPDATE).replace_id("playlist").query(move |client| {
                Ok(MpdQueryResult::Queue(client.playlist_info(fetch_stickers)?))
            });
        }
        IdleEvent::StoredPlaylist => {}
        IdleEvent::Database => {
            ctx.query().id(GLOBAL_STATUS_UPDATE).replace_id("status").query(move |client| {
                Ok(MpdQueryResult::Status {
                    data: client.get_status()?,
                    source_event: Some(IdleEvent::Database),
                })
            });
        }
        IdleEvent::Update => {}
        IdleEvent::Output => {}
        IdleEvent::Partition
        | IdleEvent::Subscription
        | IdleEvent::Message
        | IdleEvent::Neighbor
        | IdleEvent::Mount => {
            log::warn!(event:?; "Received unhandled event");
        }
    }

    if let Ok(ev) = event.try_into() {
        result_ui_evs.insert(ev);
    }
}
