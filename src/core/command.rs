use std::{io::Write, path::PathBuf, sync::Arc};

use anyhow::{Result, bail};
use itertools::Itertools;

use crate::{
    config::{
        cli::{AddRandom, Command, Provider, StickerCmd},
        cli_config::CliConfig,
    },
    ctx::Ctx,
    mpd::{
        QueuePosition,
        client::Client,
        commands::{IdleEvent, State, mpd_config::MpdConfig, volume::Bound},
        mpd_client::{AlbumArtOrder, Filter, MpdClient, MpdCommand, Tag, ValueChange},
        proto_client::ProtoClient,
        version::Version,
    },
    shared::{
        ext::duration::DurationExt,
        lrc::{LrcIndex, get_lrc_path},
        macros::status_error,
        mpd_client_ext::MpdClientExt,
        ytdlp::{self, YtDlp, YtDlpHost},
    },
    youtube::{manager::YouTubeManager, models::YouTubeId},
};

impl Command {
    pub fn execute(
        mut self,
        config: &CliConfig,
        youtube_manager: Option<Arc<YouTubeManager>>,
    ) -> Result<Box<dyn FnOnce(&mut Client<'_>) -> Result<()> + Send + 'static>> {
        match self {
            Command::Config { .. } => bail!("Cannot use config command here."),
            Command::Theme { .. } => bail!("Cannot use theme command here."),
            Command::Version => bail!("Cannot use version command here."),
            Command::DebugInfo => bail!("Cannot use debuginfo command here."),
            Command::Raw { .. } => bail!("Cannot use raw command here."),
            Command::Remote { .. } => bail!("Cannot use remote command here."),
            Command::AddRandom { tag, count } => Ok(Box::new(move |client| {
                match tag {
                    AddRandom::Song => {
                        client.add_random_songs(count, None)?;
                    }
                    AddRandom::Artist => {
                        client.add_random_tag(count, Tag::Artist)?;
                    }
                    AddRandom::Album => {
                        client.add_random_tag(count, Tag::Album)?;
                    }
                    AddRandom::AlbumArtist => {
                        client.add_random_tag(count, Tag::AlbumArtist)?;
                    }
                    AddRandom::Genre => {
                        client.add_random_tag(count, Tag::Genre)?;
                    }
                }
                Ok(())
            })),
            Command::Update { ref mut path, wait } | Command::Rescan { ref mut path, wait } => {
                let path = path.take();
                Ok(Box::new(move |client| {
                    let crate::mpd::commands::Update { job_id } =
                        if matches!(self, Command::Update { .. }) {
                            client.update(path.as_deref())?
                        } else {
                            client.rescan(path.as_deref())?
                        };

                    if wait {
                        loop {
                            client.idle(Some(IdleEvent::Update))?;
                            log::trace!("issuing update");
                            let crate::mpd::commands::Status { updating_db, .. } =
                                client.get_status()?;
                            log::trace!("update done");
                            match updating_db {
                                Some(current_id) if current_id > job_id => {
                                    break;
                                }
                                Some(_id) => {}
                                None => break,
                            }
                        }
                    }
                    Ok(())
                }))
            }
            Command::LyricsIndex => {
                let lyrics_dir = config.lyrics_dir.clone();
                Ok(Box::new(|_| {
                    let Some(dir) = lyrics_dir else {
                        bail!("Lyrics dir is not configured");
                    };
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&LrcIndex::index(&PathBuf::from(dir)))?
                    );
                    Ok(())
                }))
            }
            Command::Queue => {
                let db_path = config.youtube_db_path.clone();
                Ok(Box::new(move |client| {
                    let mut queue = client.playlist_info()?.unwrap_or_default();
                    if let Some(yt_manager) = youtube_manager {
                        yt_manager.enrich_songs(&mut queue);
                    } else if let Some(db_path) = db_path {
                        crate::youtube::enrich_songs_from_db(&mut queue, &db_path);
                    }
                    println!("{}", serde_json::ser::to_string(&queue)?);
                    Ok(())
                }))
            }
            Command::ListAll { files } => Ok(Box::new(|client| {
                let result = if files.is_empty() {
                    client.list_all(None)?
                } else {
                    client.send_start_cmd_list()?;
                    for file in files {
                        client.send_list_all(Some(&file))?;
                    }
                    client.send_execute_cmd_list()?;
                    client.read_response()?
                };

                result.into_files().for_each(|file| println!("{file}"));
                Ok(())
            })),
            Command::Play { position: None } => Ok(Box::new(|client| Ok(client.play()?))),
            Command::Play { position: Some(pos) } => {
                Ok(Box::new(move |client| Ok(client.play_pos(pos)?)))
            }
            Command::Pause => Ok(Box::new(|client| Ok(client.pause()?))),
            Command::TogglePause => Ok(Box::new(|client| {
                let status = client.get_status()?;
                if matches!(status.state, State::Play | State::Pause) {
                    client.pause_toggle()?;
                } else {
                    client.play()?;
                }
                Ok(())
            })),
            Command::Unpause => Ok(Box::new(|client| Ok(client.unpause()?))),
            Command::Stop => Ok(Box::new(|client| Ok(client.stop()?))),
            Command::Volume { value: Some(value) } => {
                Ok(Box::new(move |client| Ok(client.volume(value.parse()?)?)))
            }
            Command::Volume { value: None } => Ok(Box::new(|client| {
                println!("{}", client.get_status()?.volume.value());
                Ok(())
            })),
            Command::Next { keep_state } => Ok(Box::new(move |client| {
                let status = client.get_status()?;
                Ok(client.next_keep_state(keep_state, status.state)?)
            })),
            Command::Prev { rewind_to_start, keep_state } => Ok(Box::new(move |client| {
                let status = client.get_status()?;
                match rewind_to_start {
                    Some(value) => {
                        if status.elapsed.as_secs() >= value {
                            client.seek_current(ValueChange::Set(0))?;
                        } else {
                            client.prev_keep_state(keep_state, status.state)?;
                        }
                    }
                    None => {
                        client.prev_keep_state(keep_state, status.state)?;
                    }
                }
                Ok(())
            })),
            Command::Repeat { value } => {
                Ok(Box::new(move |client| Ok(client.repeat((value).into())?)))
            }
            Command::Random { value } => {
                Ok(Box::new(move |client| Ok(client.random((value).into())?)))
            }
            Command::Single { value } => {
                Ok(Box::new(move |client| Ok(client.single((value).into())?)))
            }
            Command::Consume { value } => {
                Ok(Box::new(move |client| Ok(client.consume((value).into())?)))
            }
            Command::ToggleRepeat => Ok(Box::new(move |client| {
                let status = client.get_status()?;
                Ok(client.repeat(!status.repeat)?)
            })),
            Command::ToggleRandom => Ok(Box::new(move |client| {
                let status = client.get_status()?;
                Ok(client.random(!status.random)?)
            })),
            Command::ToggleSingle { skip_oneshot } => Ok(Box::new(move |client| {
                let status = client.get_status()?;
                if skip_oneshot || client.version() < Version::new(0, 21, 0) {
                    client.single(status.single.cycle_skip_oneshot())?;
                } else {
                    client.single(status.single.cycle())?;
                }
                Ok(())
            })),
            Command::ToggleConsume { skip_oneshot } => Ok(Box::new(move |client| {
                let status = client.get_status()?;
                if skip_oneshot || client.version() < Version::new(0, 24, 0) {
                    client.consume(status.consume.cycle_skip_oneshot())?;
                } else {
                    client.consume(status.consume.cycle())?;
                }
                Ok(())
            })),
            Command::Seek { value } => {
                Ok(Box::new(move |client| Ok(client.seek_current(value.parse()?)?)))
            }
            Command::Clear => Ok(Box::new(|client| Ok(client.clear()?))),
            Command::Add { files, skip_ext_check, position }
                if files.iter().any(|path| path.is_absolute()) =>
            {
                Ok(Box::new(move |client| {
                    let Some(MpdConfig { music_directory, .. }) = client.config() else {
                        status_error!("Cannot add absolute path without socket connection to MPD");
                        return Ok(());
                    };

                    let dir = music_directory.clone();

                    let mut files = files;

                    if !skip_ext_check {
                        let supported_extensions = client
                            .decoders()?
                            .into_iter()
                            .flat_map(|decoder| decoder.suffixes)
                            .collect_vec();

                        files = files
                            .into_iter()
                            .filter(|path| {
                                path.to_string_lossy() == "/"
                                    || path.extension().and_then(|ext| ext.to_str()).is_some_and(
                                        |ext| {
                                            supported_extensions
                                                .iter()
                                                .any(|supported_ext| supported_ext == ext)
                                        },
                                    )
                            })
                            .collect_vec();
                    }

                    if let Some(QueuePosition::Absolute(_) | QueuePosition::RelativeAdd(_)) =
                        position
                    {
                        files.reverse();
                    }
                    for file in files {
                        if file.starts_with(&dir) {
                            client.add(
                                file.to_string_lossy()
                                    .trim_start_matches(&dir)
                                    .trim_start_matches('/')
                                    .trim_end_matches('/'),
                                position,
                            )?;
                        } else {
                            client.add(&file.to_string_lossy(), position)?;
                        }
                    }

                    Ok(())
                }))
            }
            Command::Add { mut files, position, .. } => Ok(Box::new(move |client| {
                if let Some(QueuePosition::Absolute(_) | QueuePosition::RelativeAdd(_)) = position {
                    files.reverse();
                }
                for file in files {
                    client.add(&file.to_string_lossy(), position)?;
                }

                Ok(())
            })),
            Command::AddYt { url, position } => {
                if crate::youtube::models::extract_youtube_id(&url).is_some() {
                    return Ok(Box::new(move |client| {
                        client.add(&url, position)?;
                        Ok(())
                    }));
                }

                let config = config.clone();
                Ok(Box::new(move |client| {
                    // Idle with message subsystem, the cli client is never subscribed to any
                    // channels so this will idle indefinitely
                    client.enter_idle(Some(IdleEvent::Message))?;

                    ytdlp::init_and_download(&config, &url, |path| {
                        client.noidle()?;
                        if let Err(err) = client.add_downloaded_file_to_queue(
                            path,
                            config.cache_dir.as_deref(),
                            position,
                        ) {
                            eprintln!("Failed to add downloaded file to queue: {err}");
                        }
                        client.enter_idle(Some(IdleEvent::Message))?;
                        Ok(())
                    })?;

                    client.noidle()?;
                    Ok(())
                }))
            }
            Command::ReplaceYt { url, youtube_id, pos, play } => {
                Ok(Box::new(move |client| {
                    let queue = client.playlist_info()?.unwrap_or_default();

                    // Strategy:
                    // 1. Try to find song at suggested 'pos' (if matches youtube_id)
                    // 2. Fallback: Find first occurrence of youtube_id in queue

                    let target_pos = if let Some(pos) = pos
                        && let Some(song) = queue.get(pos as usize)
                        && song.youtube_id().as_ref() == Some(&YouTubeId::new(&youtube_id))
                    {
                        Some(pos)
                    } else {
                        queue
                            .iter()
                            .position(|s| {
                                s.youtube_id().as_ref() == Some(&YouTubeId::new(&youtube_id))
                            })
                            .map(|i| i as u32)
                    };

                    if let Some(pos) = target_pos {
                        if let Some(song) = queue.get(pos as usize) {
                            // Idempotency check: if already valid (fresh), skip replacement
                            if !song.is_youtube_expired() {
                                log::info!(
                                    "Song {youtube_id} at pos {pos} is already refreshed, skipping replacement."
                                );
                                return Ok(());
                            }

                            log::info!("Replacing expired YouTube song {youtube_id} at pos {pos}");
                            // Add new URL at the same position. This shifts the old song to pos + 1.
                            client.add(
                                &url,
                                Some(QueuePosition::Absolute(
                                    pos.try_into().expect("pos to fit in u32"),
                                )),
                            )?;

                            // Delete the old song using its stable ID to avoid race conditions.
                            client.delete_id(song.id)?;

                            if play {
                                client.play_pos(pos.try_into().expect("pos to fit in u32"))?;
                            }
                        }
                    } else {
                        log::warn!("Could not find song {youtube_id} to replace. Skipping.");
                        // Do NOT add to end of queue to avoid duplication loop
                    }
                    Ok(())
                }))
            }
            Command::SearchYt { query, provider, interactive, limit, position } => {
                let kind: YtDlpHost = provider.into();
                let chosen_url = if interactive {
                    ytdlp::search_pick_cli(kind, query.trim(), limit)?
                } else {
                    YtDlp::search(kind, query.trim(), 1)?
                        .into_iter()
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("No results found for query '{query}'"))
                        .map(|item| item.url)?
                };
                let config = config.clone();
                Ok(Box::new(move |client| {
                    // Idle with message subsystem, the cli client is never subscribed to any
                    // channels so this will idle indefinitely
                    client.enter_idle(Some(IdleEvent::Message))?;

                    ytdlp::init_and_download(&config, &chosen_url, |path| {
                        client.noidle()?;
                        client.add_downloaded_file_to_queue(
                            path,
                            config.cache_dir.as_deref(),
                            position,
                        )?;
                        client.enter_idle(Some(IdleEvent::Message))?;
                        Ok(())
                    })?;

                    client.noidle()?;
                    Ok(())
                }))
            }
            Command::Save { name } => Ok(Box::new(move |client| {
                client.save_queue_as_playlist(&name, None)?;
                Ok(())
            })),
            Command::Load { names } => Ok(Box::new(|client| {
                client.send_start_cmd_list()?;
                for name in names {
                    client.send_load_playlist(&name, None)?;
                }
                client.send_execute_cmd_list()?;
                client.read_ok()?;
                Ok(())
            })),
            Command::Decoders => Ok(Box::new(|client| {
                println!("{}", serde_json::ser::to_string(&client.decoders()?)?);
                Ok(())
            })),
            Command::Outputs => Ok(Box::new(|client| {
                println!("{}", serde_json::ser::to_string(&client.outputs()?)?);
                Ok(())
            })),
            Command::ToggleOutput { id } => {
                Ok(Box::new(move |client| Ok(client.toggle_output(id)?)))
            }
            Command::EnableOutput { id } => {
                Ok(Box::new(move |client| Ok(client.enable_output(id)?)))
            }
            Command::DisableOutput { id } => {
                Ok(Box::new(move |client| Ok(client.disable_output(id)?)))
            }
            Command::Status => Ok(Box::new(|client| {
                println!("{}", serde_json::ser::to_string(&client.get_status()?)?);
                Ok(())
            })),
            Command::Song { path: Some(paths) } if paths.len() == 1 => {
                let db_path = config.youtube_db_path.clone();
                Ok(Box::new(move |client| {
                    let path = &paths[0];
                    if let Some(mut song) =
                        client.find_one(&[Filter::new(Tag::File, path.as_str())])?
                    {
                        if let Some(yt_manager) = youtube_manager {
                            yt_manager.enrich_song(&mut song);
                        } else if let Some(db_path) = &db_path {
                            crate::youtube::enrich_songs_from_db(
                                std::slice::from_mut(&mut song),
                                db_path,
                            );
                        }
                        println!("{}", serde_json::ser::to_string(&song)?);
                        Ok(())
                    } else {
                        println!("Song with path '{path}' not found.");
                        std::process::exit(1);
                    }
                }))
            }
            Command::Song { path: Some(paths) } => {
                let db_path = config.youtube_db_path.clone();
                Ok(Box::new(move |client| {
                    let mut songs = Vec::new();

                    for path in &paths {
                        if let Some(song) =
                            client.find_one(&[Filter::new(Tag::File, path.as_str())])?
                        {
                            songs.push(song);
                        } else {
                            println!("Song with path '{path}' not found.");
                            std::process::exit(1);
                        }
                    }

                    if let Some(yt_manager) = &youtube_manager {
                        yt_manager.enrich_songs(&mut songs);
                    } else if let Some(db_path) = &db_path {
                        crate::youtube::enrich_songs_from_db(&mut songs, db_path);
                    }

                    println!("{}", serde_json::ser::to_string(&songs)?);
                    Ok(())
                }))
            }
            Command::Song { path: None } => {
                let db_path = config.youtube_db_path.clone();
                Ok(Box::new(move |client| {
                    let current_song = client.get_current_song()?;
                    if let Some(mut song) = current_song {
                        if let Some(yt_manager) = youtube_manager {
                            yt_manager.enrich_song(&mut song);
                        } else if let Some(db_path) = &db_path {
                            crate::youtube::enrich_songs_from_db(
                                std::slice::from_mut(&mut song),
                                db_path,
                            );
                        }
                        println!("{}", serde_json::ser::to_string(&song)?);
                        Ok(())
                    } else {
                        std::process::exit(1);
                    }
                }))
            }
            Command::Mount { name, path } => {
                Ok(Box::new(move |client| Ok(client.mount(&name, &path)?)))
            }
            Command::Unmount { name } => Ok(Box::new(move |client| Ok(client.unmount(&name)?))),
            Command::ListMounts => Ok(Box::new(|client| {
                println!("{}", serde_json::ser::to_string(&client.list_mounts()?)?);
                Ok(())
            })),
            Command::ListPartitions => Ok(Box::new(|client| {
                println!("{}", serde_json::ser::to_string(&client.list_partitions()?.0)?);
                Ok(())
            })),
            Command::AlbumArt { output } => Ok(Box::new(move |client| {
                let Some(song) = client.get_current_song()? else {
                    std::process::exit(3);
                };

                let album_art = client.find_album_art(&song.file, AlbumArtOrder::EmbeddedFirst)?;

                let Some(album_art) = album_art else {
                    std::process::exit(2);
                };

                if &output == "-" {
                    std::io::stdout().write_all(&album_art)?;
                    std::io::stdout().flush()?;
                    Ok(())
                } else {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(output)?
                        .write_all(&album_art)?;
                    Ok(())
                }
            })),
            Command::Sticker { cmd: StickerCmd::Set { uri, key, value } } => {
                Ok(Box::new(move |client| {
                    client.set_sticker(&uri, &key, &value)?;
                    Ok(())
                }))
            }
            Command::Sticker { cmd: StickerCmd::Get { uri, key } } => Ok(Box::new(move |client| {
                match client.sticker(&uri, &key)? {
                    Some(sticker) => {
                        println!("{}", serde_json::ser::to_string(&sticker)?);
                    }
                    None => {
                        std::process::exit(1);
                    }
                }
                Ok(())
            })),
            Command::Sticker { cmd: StickerCmd::Delete { uri, key } } => {
                Ok(Box::new(move |client| {
                    client.delete_sticker(&uri, &key)?;
                    Ok(())
                }))
            }
            Command::Sticker { cmd: StickerCmd::DeleteAll { uri } } => {
                Ok(Box::new(move |client| {
                    client.delete_all_stickers(&uri)?;
                    Ok(())
                }))
            }
            Command::Sticker { cmd: StickerCmd::List { uri } } => Ok(Box::new(move |client| {
                let stickers = client.list_stickers(&uri)?;
                println!("{}", serde_json::ser::to_string(&stickers)?);
                Ok(())
            })),
            Command::Sticker { cmd: StickerCmd::Find { uri, key } } => {
                Ok(Box::new(move |client| {
                    let stickers = client.find_stickers(&uri, &key, None)?;
                    println!("{}", serde_json::ser::to_string(&stickers)?);
                    Ok(())
                }))
            }
            Command::SendMessage { channel, content } => Ok(Box::new(move |client| {
                client.send_message(&channel, &content)?;
                Ok(())
            })),
            Command::ImportLibrary { path } => {
                Ok(Box::new(move |_client| {
                    // We need a way to send events to youtube_manager from here.
                    // Command::execute returns a Boxed closure that takes &mut Client.
                    // This is used by CliConfig::execute_command which has access to Ctx if available.
                    // But here it's more for CLI standalone.
                    // Actually, Command::execute is also used by main.rs when running CLI commands.
                    log::info!("Importing library from {path}");
                    // This will be handled in main.rs or where Command::execute is called if Ctx is present.
                    Ok(())
                }))
            }
            Command::ImportPlaylists { path } => Ok(Box::new(move |_client| {
                log::info!("Importing playlists from {path}");
                Ok(())
            })),
            Command::GetLyrics => Ok(Box::new(|_client| {
                // In TUI, this is handled specifically in ui/mod.rs to use YouTubeManager.
                // In standalone CLI, we could implement it here if needed,
                // but it requires a lot of dependencies (yt-dlp, curl).
                Ok(())
            })),
        }
    }
}

impl From<Provider> for YtDlpHost {
    fn from(p: Provider) -> Self {
        match p {
            Provider::Youtube => YtDlpHost::Youtube,
            Provider::Soundcloud => YtDlpHost::Soundcloud,
            Provider::Nicovideo => YtDlpHost::NicoVideo,
        }
    }
}

pub fn run_external_blocking<'a, E>(command: &[String], envs: E) -> Result<()>
where
    E: IntoIterator<Item = (&'a str, &'a str)> + std::fmt::Debug,
{
    let [cmd, args @ ..] = command else {
        bail!("Invalid command: {command:?}");
    };

    let mut cmd = std::process::Command::new(cmd);
    cmd.args(args);

    for (key, val) in envs {
        cmd.env(key, val);
    }

    log::debug!(command:?; "Running external command");
    log::trace!(command:?, envs:? = cmd.get_envs(); "Running external command");

    let out = match cmd.output() {
        Ok(out) => out,
        Err(err) => {
            bail!("Unexpected error when executing external command: {err:?}");
        }
    };

    if !out.status.success() {
        bail!(
            "External command failed: exit code: '{}', stdout: '{}', stderr: '{}'",
            out.status.code().map_or_else(|| "-".to_string(), |v| v.to_string()),
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    Ok(())
}

pub fn run_external<K: Into<String>, V: Into<String>>(
    command: Arc<Vec<String>>,
    envs: Vec<(K, V)>,
) {
    let envs = envs.into_iter().map(|(k, v)| (k.into(), v.into())).collect_vec();

    std::thread::spawn(move || {
        if let Err(err) = run_external_blocking(
            command.as_slice(),
            envs.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        ) {
            status_error!("{}", err);
        }
    });
}

pub fn create_env<'a>(
    ctx: &Ctx,
    selected_songs_paths: impl IntoIterator<Item = &'a str>,
) -> Vec<(String, String)> {
    let mut result = Vec::new();

    if let Some((_, current)) = ctx.find_current_song_in_queue() {
        result.push(("CURRENT_SONG".to_owned(), current.file.clone()));
        result.extend(
            current.metadata.iter().map(|(k, v)| (k.to_ascii_uppercase(), v.last().to_owned())),
        );
        let lrc_path = ctx
            .config
            .lyrics_dir
            .as_ref()
            .and_then(|dir| get_lrc_path(dir, &current.file).ok())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let lrc = ctx.find_lrc().ok().flatten();
        let duration = current.duration.map_or_else(String::new, |d| d.to_string());
        result.push(("DURATION".to_owned(), duration));
        result.push(("HAS_LRC".to_owned(), lrc.is_some().to_string()));
        result.push(("LRC_FILE".to_owned(), lrc_path));
        result.push(("FILE".to_owned(), current.file.clone()));
    }
    result.push(("PID".to_owned(), std::process::id().to_string()));

    let songs =
        selected_songs_paths.into_iter().enumerate().fold(String::new(), |mut acc, (idx, val)| {
            if idx > 0 {
                acc.push('\n');
            }
            acc.push_str(val);
            acc
        });

    if !songs.is_empty() {
        result.push(("SELECTED_SONGS".to_owned(), songs));
    }
    result.push(("VERSION".to_owned(), env!("CARGO_PKG_VERSION").to_string()));

    result.push(("STATE".to_owned(), ctx.status.state.to_string()));

    result
}
