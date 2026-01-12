use crate::config::{MpdAddress, address::MpdPassword};
use crate::mpd::commands::Song;
use crate::shared::ytdlp::{YtDlp, YtDlpHost, YtDlpItem};
use crate::youtube::constants::{DEFAULT_LYRICS_SEARCH_LIMIT, LYRICS_DURATION_MATCH_THRESHOLD};
use crate::youtube::events::YouTubeEvent;
use crate::youtube::models::YouTubeTrack;
use async_channel::{Receiver, Sender};
use std::path::PathBuf;
use std::sync::Arc;

pub struct DownloadService {
    event_tx: Sender<YouTubeEvent>,
    cache_dir: Option<PathBuf>,
    lyrics_dir: Option<String>,
    lyrics_sub_langs: String,
    permit_tx: Sender<()>,
    permit_rx: Receiver<()>,
}

impl DownloadService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_tx: Sender<YouTubeEvent>,
        cache_dir: Option<PathBuf>,
        lyrics_dir: Option<String>,
        lyrics_sub_langs: String,
        _address: MpdAddress,
        _password: Option<MpdPassword>,
        permit_tx: Sender<()>,
        permit_rx: Receiver<()>,
    ) -> Self {
        Self { event_tx, cache_dir, lyrics_dir, lyrics_sub_langs, permit_tx, permit_rx }
    }

    pub async fn handle_event(self: &Arc<Self>, event: YouTubeEvent) {
        let inner = self.clone();
        match event {
            YouTubeEvent::Download(track) => {
                smol::spawn(async move {
                    inner.handle_download(Arc::from([track])).await;
                })
                .detach();
            }
            YouTubeEvent::DownloadMany(tracks) => {
                smol::spawn(async move {
                    inner.handle_download(tracks).await;
                })
                .detach();
            }
            YouTubeEvent::GetLyrics(song) => {
                smol::spawn(async move {
                    inner.handle_get_lyrics(song).await;
                })
                .detach();
            }
            YouTubeEvent::DownloadThumbnail(youtube_id) => {
                smol::spawn(async move {
                    inner.handle_download_thumbnail(youtube_id).await;
                })
                .detach();
            }
            _ => {}
        }
        smol::future::yield_now().await;
    }

    async fn handle_download_thumbnail(&self, youtube_id: crate::youtube::models::YouTubeId) {
        let Some(cache_dir) = &self.cache_dir else {
            return;
        };

        let thumbnails_dir = cache_dir.join("thumbnails");
        if let Err(e) = std::fs::create_dir_all(&thumbnails_dir) {
            log::error!("Failed to create thumbnails directory: {e}");
            return;
        }

        let target_path = thumbnails_dir.join(format!("{youtube_id}.jpg"));
        if target_path.exists() {
            let _ = self
                .event_tx
                .send(YouTubeEvent::ThumbnailDownloaded { youtube_id, path: target_path })
                .await;
            return;
        }

        if let Ok(yt_track) =
            crate::youtube::ytdlp::YtdlpAdapter::get_metadata(youtube_id.as_str()).await
        {
            if let Some(url) = yt_track.thumbnail_url {
                if let Ok(()) = self.permit_rx.recv().await {
                    let path = target_path.clone();
                    let res = smol::unblock(move || {
                        let mut cmd = std::process::Command::new("curl");
                        cmd.arg("-L").arg("-o").arg(&path).arg(url);
                        let output = cmd.output()?;
                        if !output.status.success() {
                            return Err(anyhow::anyhow!("curl failed"));
                        }
                        Ok::<(), anyhow::Error>(())
                    })
                    .await;

                    let _ = self.permit_tx.send(()).await;

                    if let Err(e) = res {
                        log::error!("Failed to download thumbnail for {youtube_id}: {e}");
                    } else {
                        let _ = self
                            .event_tx
                            .send(YouTubeEvent::ThumbnailDownloaded {
                                youtube_id,
                                path: target_path,
                            })
                            .await;
                    }
                }
            }
        }
    }

    async fn handle_download(&self, tracks: Arc<[YouTubeTrack]>) {
        let Some(cache_dir) = &self.cache_dir else {
            log::error!("Download failed: cache_dir not configured");

            return;
        };

        for track in tracks.iter() {
            log::info!("Starting download for: {}", track.title);

            let url = track.link.clone();

            let cache_dir = cache_dir.clone();

            // Sanitize filename

            let sanitized_artist =
                track
                    .artist
                    .chars()
                    .map(|c| {
                        if c.is_alphanumeric() || c == ' ' || c == '.' || c == '-' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect::<String>();

            let sanitized_title =
                track
                    .title
                    .chars()
                    .map(|c| {
                        if c.is_alphanumeric() || c == ' ' || c == '.' || c == '-' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect::<String>();

            let filename = format!("{sanitized_artist} - {sanitized_title}");

            let track_title = track.title.clone();

            if let Ok(()) = self.permit_rx.recv().await {
                let res = smol::unblock(move || {
                    let ytdlp = YtDlp::new(cache_dir);
                    let item = YtDlpItem {
                        id: youtube_id,
                        filename,
                        kind: YtDlpHost::Youtube,
                    };

                    ytdlp.download_single(&item)?;

                    Ok::<(), anyhow::Error>(())
                })
                .await;

                let _ = self.permit_tx.send(()).await;

                if let Err(e) = res {
                    log::error!("Download failed for {track_title}: {e}");
                } else {
                    log::info!("Download finished for: {track_title}");
                }
            }
        }
    }

    pub(crate) async fn handle_get_lyrics(&self, mut song: Song) {
        let Some(lyrics_dir) = &self.lyrics_dir else {
            log::error!("Lyrics download failed: lyrics_dir not configured");
            return;
        };

        let mut artist =
            song.metadata.get(Song::ARTIST).map(|v| v.first().to_string()).unwrap_or_default();
        let mut title =
            song.metadata.get(Song::TITLE).map(|v| v.first().to_string()).unwrap_or_default();
        let mut album = song.metadata.get(Song::ALBUM).map(|v| v.first().to_string());
        let mut duration = song.duration.unwrap_or_default();

        if (artist.is_empty() || title.is_empty())
            && let Some(yt_id) = song.youtube_id()
        {
            log::info!(
                "Metadata missing for YouTube track {yt_id}, fetching before lyrics download..."
            );
            if let Ok(yt_track) =
                crate::youtube::ytdlp::YtdlpAdapter::get_metadata(yt_id.as_str()).await
            {
                song.enrich_from_youtube(&yt_track);
                artist = yt_track.artist;
                title = yt_track.title;
                album = yt_track.album;
                duration = yt_track.duration;
            }
        }

        if artist.is_empty() || title.is_empty() {
            log::error!("Lyrics download failed: Missing artist or title for {}", song.file);
            return;
        }

        let (clean_artist, clean_title) = crate::youtube::utils::clean_metadata(&artist, &title);

        let target_path = if let Some(yt_id) = song.youtube_id() {
            PathBuf::from(lyrics_dir).join(format!("{yt_id}.lrc"))
        } else {
            match crate::shared::lrc::get_lrc_path(lyrics_dir, &song.file) {
                Ok(path) => path,
                Err(e) => {
                    log::error!("Failed to resolve lyrics path for {}: {e}", song.file);
                    return;
                }
            }
        };

        if let Some(parent) = target_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log::error!("Failed to create lyrics directory: {e}");
                return;
            }
        }

        // 2. Try LrcLib.net
        if let Some(lyrics) =
            self.fetch_from_lrclib(&clean_artist, &clean_title, album.as_deref(), duration).await
        {
            if let Err(e) = self.save_lrc(&target_path, &lyrics, &artist, &title, duration) {
                log::error!("Failed to save lyrics from lrclib: {e}");
            } else {
                log::info!("Lyrics fetched from lrclib for {artist} - {title}");
                let _ = self.event_tx.send(YouTubeEvent::LibraryUpdated).await;
                return;
            }
        }

        if let Some(lyrics) =
            self.fetch_from_lrclib_search(&clean_artist, &clean_title, duration).await
        {
            if let Err(e) = self.save_lrc(&target_path, &lyrics, &artist, &title, duration) {
                log::error!("Failed to save lyrics from lrclib search: {e}");
            } else {
                log::info!("Lyrics fetched from lrclib search for {artist} - {title}");
                let _ = self.event_tx.send(YouTubeEvent::LibraryUpdated).await;
                return;
            }
        }

        // 3. Try YouTube Direct
        if let Some(yt_id) = song.youtube_id() {
            let url = format!("https://www.youtube.com/watch?v={yt_id}");
            if self.fetch_from_ytdlp(&url, &target_path).await {
                if let Err(e) = self.post_process_lrc(&target_path, &artist, &title, duration) {
                    log::error!("Failed to post-process lyrics from youtube: {e}");
                } else {
                    log::info!("Lyrics fetched from youtube direct for {artist} - {title}");
                    let _ = self.event_tx.send(YouTubeEvent::LibraryUpdated).await;
                    return;
                }
            }
        }

        // 4. Try YouTube Search Fallback
        let query = format!("{clean_artist} {clean_title} lyrics");
        let search_query = format!("ytsearch{DEFAULT_LYRICS_SEARCH_LIMIT}:{query}");
        if self.fetch_from_ytdlp(&search_query, &target_path).await {
            if let Err(e) = self.post_process_lrc(&target_path, &artist, &title, duration) {
                log::error!("Failed to post-process lyrics from youtube search: {e}");
            } else {
                log::info!("Lyrics fetched from youtube search for {artist} - {title}");
                let _ = self.event_tx.send(YouTubeEvent::LibraryUpdated).await;
                return;
            }
        }
        log::warn!("No lyrics found for {artist} - {title}");
    }

    pub(crate) async fn fetch_from_lrclib(
        &self,
        artist: &str,
        title: &str,
        album: Option<&str>,
        duration: std::time::Duration,
    ) -> Option<String> {
        // Use async Timer instead of thread::sleep to avoid blocking the executor
        smol::Timer::after(std::time::Duration::from_secs(1)).await;
        let user_agent =
            format!("rmpc-{} (https://github.com/mierak/rmpc)", env!("CARGO_PKG_VERSION"));
        let mut cmd = smol::process::Command::new("curl");
        cmd.arg("-X")
            .arg("GET")
            .arg("-sG")
            .arg("--connect-timeout")
            .arg("10")
            .arg("--max-time")
            .arg("30")
            .arg("-H")
            .arg(format!("Lrclib-Client: {user_agent}"))
            .arg("https://lrclib.net/api/get")
            .arg("--data-urlencode")
            .arg(format!("artist_name={artist}"))
            .arg("--data-urlencode")
            .arg(format!("track_name={title}"));

        if let Some(a) = album {
            cmd.arg("--data-urlencode").arg(format!("album_name={a}"));
        }
        if duration.as_secs() > 0 {
            cmd.arg("--data-urlencode").arg(format!("duration={}", duration.as_secs()));
        }

        let output = cmd.output().await;
        match output {
            Ok(out) if out.status.success() => {
                let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
                json["syncedLyrics"]
                    .as_str()
                    .map(|s| s.to_string())
                    .or_else(|| json["plainLyrics"].as_str().map(|s| s.to_string()))
            }
            _ => None,
        }
    }

    pub(crate) async fn fetch_from_lrclib_search(
        &self,
        artist: &str,
        title: &str,
        duration: std::time::Duration,
    ) -> Option<String> {
        smol::Timer::after(std::time::Duration::from_secs(1)).await;
        let query = format!("{artist} {title}");
        let user_agent =
            format!("rmpc-{} (https://github.com/mierak/rmpc)", env!("CARGO_PKG_VERSION"));

        let mut cmd = smol::process::Command::new("curl");
        let output = cmd
            .arg("-X")
            .arg("GET")
            .arg("-sG")
            .arg("--connect-timeout")
            .arg("10")
            .arg("--max-time")
            .arg("30")
            .arg("-H")
            .arg(format!("Lrclib-Client: {user_agent}"))
            .arg("https://lrclib.net/api/search")
            .arg("--data-urlencode")
            .arg(format!("q={query}"))
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => {
                let results: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).ok()?;
                for res in results {
                    if let Some(synced) = res["syncedLyrics"].as_str() {
                        if !synced.is_empty() {
                            if duration.as_secs() > 0 {
                                if let Some(res_dur) = res["duration"].as_f64() {
                                    if (res_dur - duration.as_secs_f64()).abs()
                                        > LYRICS_DURATION_MATCH_THRESHOLD
                                    {
                                        continue;
                                    }
                                }
                            }
                            return Some(synced.to_string());
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    pub(crate) async fn fetch_from_ytdlp(&self, url: &str, target_path: &std::path::Path) -> bool {
        if let Ok(()) = self.permit_rx.recv().await {
            let output_template = target_path.with_extension("");

            let mut cmd = smol::process::Command::new("yt-dlp");
            let res = cmd
                .args([
                    "--skip-download",
                    "--write-subs",
                    "--write-auto-subs",
                    "--ignore-errors",
                    "--no-playlist",
                    "--sub-langs",
                    &self.lyrics_sub_langs,
                    "--convert-subs",
                    "lrc",
                    "--output",
                ])
                .arg(&output_template)
                .arg(url)
                .output()
                .await;

            let _ = self.permit_tx.send(()).await;

            match res {
                Ok(out) if out.status.success() => {
                    if target_path.exists() {
                        return true;
                    }
                    if let Some(parent) = target_path.parent() {
                        if let Ok(entries) = std::fs::read_dir(parent) {
                            let stem =
                                target_path.file_stem().unwrap_or_default().to_string_lossy();
                            for entry in entries.flatten() {
                                let path = entry.path();
                                if path.extension().is_some_and(|ext| ext == "lrc") {
                                    let filename =
                                        path.file_name().unwrap_or_default().to_string_lossy();
                                    if filename.starts_with(&*stem) {
                                        let _ = std::fs::rename(path, target_path);
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                    false
                }
                _ => false,
            }
        } else {
            false
        }
    }

    pub(crate) fn save_lrc(
        &self,
        path: &PathBuf,
        content: &str,
        artist: &str,
        title: &str,
        duration: std::time::Duration,
    ) -> Result<(), std::io::Error> {
        use std::io::Write;
        let mut file = std::fs::File::create(path)?;
        let minutes = duration.as_secs() / 60;
        let seconds = duration.as_secs() % 60;

        writeln!(file, "[ar:{artist}]")?;
        writeln!(file, "[ti:{title}]")?;
        writeln!(file, "[length:{minutes:02}:{seconds:02}]")?;
        write!(file, "{content}")?;
        Ok(())
    }

    fn post_process_lrc(
        &self,
        path: &PathBuf,
        artist: &str,
        title: &str,
        duration: std::time::Duration,
    ) -> Result<(), std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        if content.contains("[ar:") && content.contains("[ti:") {
            return Ok(()); // Already has tags
        }
        self.save_lrc(path, &content, artist, title, duration)
    }
}
