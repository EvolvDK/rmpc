use crate::config::MpdAddress;
use crate::mpd::commands::Song;
use crate::youtube::db::Database;
use crate::youtube::download::DownloadService;
use crate::youtube::error::Result as YouTubeResult;
use crate::youtube::events::YouTubeEvent;
use crate::youtube::models::{YouTubeId, YouTubeTrack};
use crate::youtube::streaming::StreamingService;
use async_channel::unbounded;
use chrono::Utc;
use lru::LruCache;
use parking_lot::RwLock;
use std::fs;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

struct LyricsTestContext {
    temp_dir: PathBuf,
    lyrics_dir: PathBuf,
    service: Arc<DownloadService>,
}

impl LyricsTestContext {
    fn new(sub_langs: &str) -> Self {
        let _ = flexi_logger::Logger::try_with_str("debug").map(|l| l.start().ok());
        let mut temp_dir = std::env::temp_dir();
        let random_suffix: u64 = rand::random();
        temp_dir.push(format!("rmpc_test_lyrics_{random_suffix}"));
        fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let lyrics_dir = temp_dir.join("lyrics");
        fs::create_dir(&lyrics_dir).expect("Failed to create lyrics dir");

        let (tx, _rx) = unbounded::<YouTubeEvent>();
        let (sem_tx, sem_rx) = async_channel::bounded::<()>(3);
        for _ in 0..3 {
            let _ = sem_tx.send_blocking(());
        }
        let service = Arc::new(DownloadService::new(
            tx,
            None,
            Some(lyrics_dir.to_string_lossy().to_string()),
            sub_langs.to_string(),
            MpdAddress::default(),
            None,
            sem_tx,
            sem_rx,
        ));

        Self { temp_dir, lyrics_dir, service }
    }
}

impl Drop for LyricsTestContext {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

impl LyricsTestContext {
    fn create_song(artist: &str, title: &str, youtube_id: Option<&str>) -> Song {
        let mut song = Song::default();
        if let Some(id) = youtube_id {
            song.file =
                format!("https://rr1---sn-1.googlevideo.com/videoplayback?id=song1#id={id}");
        } else {
            song.file = format!("{artist}/{title}.mp3");
        }
        song.metadata.insert("artist".to_string(), vec![artist.to_string()].into());
        song.metadata.insert("title".to_string(), vec![title.to_string()].into());
        song.duration = Some(Duration::from_secs(200));
        song
    }
}

use rstest::{fixture, rstest};

#[fixture]
fn lyrics_ctx() -> LyricsTestContext {
    LyricsTestContext::new("en")
}

#[rstest]
#[allow(clippy::used_underscore_binding)]
fn test_clean_metadata_youtube_titles(_lyrics_ctx: LyricsTestContext) {
    let cases = vec![
        (
            "Panic! At The Disco",
            "Hey Look Ma, I Made It (Official Video)",
            "Panic! At The Disco",
            "Hey Look Ma, I Made It",
        ),
        ("Journey", "Don't_Stop_Believin'", "Journey", "Don't Stop Believin'"),
        ("RUSTAGE", "FALL (Doflamingo) (feat. Oricadia)", "RUSTAGE", "FALL (Doflamingo)"),
        ("The Kid LAROI, Justin Bieber", "STAY", "The Kid LAROI", "STAY"),
    ];

    for (input_artist, input_title, expected_artist, expected_title) in cases {
        let (artist, title) = crate::youtube::utils::clean_metadata(input_artist, input_title);
        assert_eq!(artist, expected_artist);
        assert_eq!(title, expected_title);
    }
}

// lrclib tests (Real curl)
#[rstest]
fn test_lrclib_popular(lyrics_ctx: LyricsTestContext) {
    smol::block_on(async {
        let artist = "The Weeknd";
        let title = "Blinding Lights";
        let lyrics = lyrics_ctx
            .service
            .fetch_from_lrclib(artist, title, None, Duration::from_secs(200))
            .await;
        assert!(lyrics.is_some(), "Should fetch lyrics for popular song from lrclib");
    });
}

#[rstest]
fn test_lrclib_multi_artist(lyrics_ctx: LyricsTestContext) {
    smol::block_on(async {
        let artist = "The Kid LAROI";
        let title = "STAY";
        let lyrics = lyrics_ctx
            .service
            .fetch_from_lrclib(artist, title, None, Duration::from_secs(141))
            .await;
        assert!(lyrics.is_some(), "Should fetch lyrics for multi-artist song from lrclib");
    });
}

#[rstest]
fn test_lrclib_complex_titles(lyrics_ctx: LyricsTestContext) {
    smol::block_on(async {
        let cases = vec![
            ("Panic! At The Disco", "Hey Look Ma, I Made It"),
            ("Journey", "Don't Stop Believin'"),
            ("RUSTAGE", "FALL (Doflamingo)"),
        ];

        for (artist, title) in cases {
            let lyrics = lyrics_ctx
                .service
                .fetch_from_lrclib(artist, title, None, Duration::from_secs(0))
                .await;
            assert!(
                lyrics.is_some(),
                "Should fetch lyrics for complex title '{artist} - {title}' from lrclib"
            );
        }
    });
}

// yt-dlp direct tests (Real yt-dlp)
#[rstest]
fn test_ytdlp_direct_popular(lyrics_ctx: LyricsTestContext) {
    smol::block_on(async {
        let yt_id = "4NRXx6U8ABQ"; // The Weeknd - Blinding Lights
        let url = format!("https://www.youtube.com/watch?v={yt_id}");
        let target_path = lyrics_ctx.lyrics_dir.join(format!("{yt_id}.lrc"));

        let success = lyrics_ctx.service.fetch_from_ytdlp(&url, &target_path).await;
        assert!(success, "Should download lyrics directly via yt-dlp");
        assert!(target_path.exists(), "LRC file should exist at {}", target_path.display());
    });
}

#[rstest]
fn test_ytdlp_direct_multi_artist(lyrics_ctx: LyricsTestContext) {
    smol::block_on(async {
        let yt_id = "kTJczUoc26U"; // The Kid LAROI, Justin Bieber - STAY
        let url = format!("https://www.youtube.com/watch?v={yt_id}");
        let target_path = lyrics_ctx.lyrics_dir.join(format!("{yt_id}.lrc"));

        let success = lyrics_ctx.service.fetch_from_ytdlp(&url, &target_path).await;
        assert!(success, "Should download lyrics directly via yt-dlp for multi-artist song");
        assert!(target_path.exists());
    });
}

#[rstest]
fn test_ytdlp_direct_complex_titles(lyrics_ctx: LyricsTestContext) {
    smol::block_on(async {
        let cases = vec![("vqQDp522wQU"), ("VcjzHMhBtf0"), ("RFBDmwFxVr0")]; // Panic! At The Disco - Hey Look Ma, I Made It, Journey - Don't Stop Believin', RUSTAGE - Blood (Alucard Rap)

        for yt_id in cases {
            let url = format!("https://www.youtube.com/watch?v={yt_id}");
            let target_path = lyrics_ctx.lyrics_dir.join(format!("{yt_id}.lrc"));
            let success = lyrics_ctx.service.fetch_from_ytdlp(&url, &target_path).await;
            assert!(success, "Should download lyrics directly via yt-dlp for ID {yt_id}");
            assert!(target_path.exists());
        }
    });
}

// yt-dlp search tests (Real yt-dlp)
#[rstest]
fn test_ytdlp_search_popular(lyrics_ctx: LyricsTestContext) {
    smol::block_on(async {
        let _song =
            LyricsTestContext::create_song("The Weeknd", "Blinding Lights", Some("J7p4bzqLvCw"));
        let target_path = lyrics_ctx.lyrics_dir.join("J7p4bzqLvCw.lrc");

        let query = "ytsearch10:The Weeknd Blinding Lights lyrics";
        let success = lyrics_ctx.service.fetch_from_ytdlp(query, &target_path).await;
        assert!(success, "Should find and download lyrics via ytsearch10");
        assert!(target_path.exists());
    });
}

#[rstest]
fn test_ytdlp_search_multi_artist(lyrics_ctx: LyricsTestContext) {
    smol::block_on(async {
        let _song = LyricsTestContext::create_song("The Kid LAROI", "STAY", Some("XfEMj-z3TtA"));
        let target_path = lyrics_ctx.lyrics_dir.join("XfEMj-z3TtA.lrc");

        let query = "ytsearch10:The Kid LAROI STAY lyrics";
        let success = lyrics_ctx.service.fetch_from_ytdlp(query, &target_path).await;
        assert!(success, "Should find and download lyrics via ytsearch10 for multi-artist");
        assert!(target_path.exists());
    });
}

#[rstest]
fn test_ytdlp_search_complex_titles(lyrics_ctx: LyricsTestContext) {
    smol::block_on(async {
        let cases = vec![
            ("Panic! At The Disco", "Hey Look Ma, I Made It", "GXPRTZoIU1w"),
            ("Journey", "Don't_Stop_Believin'", "PIFUWHvSixw"),
            ("RUSTAGE", "FALL (Doflamingo) (feat. Oricadia)", "owW8K-nGT24"),
        ];

        for (artist, title, yt_id) in cases {
            let (clean_artist, clean_title) = crate::youtube::utils::clean_metadata(artist, title);
            let query = format!("ytsearch10:{clean_artist} {clean_title} lyrics");
            let target_path = lyrics_ctx.lyrics_dir.join(format!("{yt_id}.lrc"));
            let success = lyrics_ctx.service.fetch_from_ytdlp(&query, &target_path).await;
            assert!(success, "Should find and download lyrics via ytsearch10 for {title}");
            assert!(target_path.exists());
        }
    });
}

#[rstest]
fn test_lrc_metadata_tagging(lyrics_ctx: LyricsTestContext) {
    smol::block_on(async {
        let lyrics_content = "[00:10.00]Test line";
        let artist = "Test Artist";
        let title = "Test Title";
        let duration = Duration::from_secs(225);
        let target_path = lyrics_ctx.lyrics_dir.join("test.lrc");

        lyrics_ctx
            .service
            .save_lrc(&target_path, lyrics_content, artist, title, duration)
            .expect("Failed to save LRC");

        let content = fs::read_to_string(&target_path).expect("Failed to read saved LRC");
        assert!(content.contains("[ar:Test Artist]"), "Missing artist tag");
        assert!(content.contains("[ti:Test Title]"), "Missing title tag");
        assert!(content.contains("[length:03:45]"), "Missing length tag");
        assert!(content.contains("[00:10.00]Test line"), "Missing lyrics content");
    });
}

#[test]
fn test_db_insert_get_track() {
    let db = Database::in_memory().expect("In-memory database should be created");
    let track = YouTubeTrack {
        youtube_id: YouTubeId::new("test_id"),
        title: "Test Title".to_string(),
        artist: "Test Artist".to_string(),
        album: Some("Test Album".to_string()),
        duration: Duration::from_secs(120),
        link: "https://youtube.com/watch?v=test_id".to_string(),
        thumbnail_url: None,
        added_at: Utc::now(),
        last_modified_at: Utc::now(),
    };

    db.insert_track(&track).expect("Track should be inserted");
    let fetched =
        db.get_track("test_id").expect("Track should be fetched").expect("Track should exist");
    assert_eq!(fetched.title, "Test Title");
    assert_eq!(fetched.artist, "Test Artist");
}

#[test]
fn test_metadata_normalization() {
    use crate::youtube::ytdlp::YtdlpAdapter;

    // Test title priority: track > title > fulltitle
    let json = r#"{
        "id": "test_id",
        "track": "Track Title",
        "title": "Basic Title",
        "fulltitle": "Full Title",
        "artist": "Artist Name",
        "duration": 120
    }"#;
    let track = YtdlpAdapter::parse_metadata("test_id", json.as_bytes())
        .expect("Metadata should be parsed");
    assert_eq!(track.title, "Track Title");

    let json_no_track = r#"{
        "id": "test_id",
        "title": "Basic Title",
        "fulltitle": "Full Title",
        "artist": "Artist Name",
        "duration": 120
    }"#;
    let track = YtdlpAdapter::parse_metadata("test_id", json_no_track.as_bytes())
        .expect("Metadata should be parsed without track field");
    assert_eq!(track.title, "Basic Title");

    // Test artist priority: artist > album_artist > creator > uploader
    let json_artist = r#"{
        "id": "test_id",
        "title": "Title",
        "artist": "Artist",
        "album_artist": "Album Artist",
        "creator": "Creator",
        "uploader": "Uploader"
    }"#;
    let track = YtdlpAdapter::parse_metadata("test_id", json_artist.as_bytes())
        .expect("Metadata should be parsed with artist");
    assert_eq!(track.artist, "Artist");

    let json_creator = r#"{
        "id": "test_id",
        "title": "Title",
        "creator": "Creator",
        "uploader": "Uploader"
    }"#;
    let track = YtdlpAdapter::parse_metadata("test_id", json_creator.as_bytes())
        .expect("Metadata should be parsed with creator");
    assert_eq!(track.artist, "Creator");
}

#[test]
fn test_ytdlp_integration_metadata() {
    smol::block_on(async {
        use crate::youtube::ytdlp::YtdlpAdapter;
        // Using a well-known video: Rick Astley - Never Gonna Give You Up
        let yt_id = "dQw4w9WgXcQ";
        let track = YtdlpAdapter::get_metadata(yt_id)
            .await
            .expect("Metadata should be fetched from YouTube");

        assert_eq!(track.youtube_id, yt_id);
        assert!(!track.title.is_empty());
        assert!(!track.artist.is_empty());
        assert!(track.duration.as_secs() > 0);
    });
}

#[test]
fn test_queue_deduplication_and_bulk_add() {
    smol::block_on(async {
        use crate::youtube::events::YouTubeEvent;
        use crate::youtube::models::YouTubeTrack;
        use crate::youtube::streaming::StreamingService;
        use async_channel::unbounded;
        use parking_lot::RwLock;
        use std::sync::Arc;

        let (event_tx, event_rx) = unbounded::<YouTubeEvent>();
        let mock_client = Arc::new(MockYouTubeClient);
        let queue = Arc::new(RwLock::new(Vec::new()));
        let url_cache = Arc::new(RwLock::new(crate::youtube::cache::ExpiringCache::new(
            std::num::NonZeroUsize::new(10).expect("10 is non-zero"),
        )));

        let (sem_tx, sem_rx) = async_channel::bounded::<()>(3);
        for _ in 0..3 {
            let _ = sem_tx.send_blocking(());
        }

        // Add a song to the "MPD queue"
        queue.write().push(Song {
            id: 1,
            file: "https://rr1---sn-cv0tb0xn-ouxe.googlevideo.com/videoplayback?expire=1767765515&ei=q6Fdab-uAfKIvdIP05KT6QI&ip=2001%3A861%3A5b82%3Ae3f0%3A3ff6%3A30f5%3Acaa7%3A33fd&id=o-ANLDzNmmDLldUMHY1B5DSh9j9GkywtXCoVcydE_50kbo&itag=251&source=youtube&requiressl=yes&xpc=EgVo2aDSNQ%3D%3D&cps=1&met=1767743915%2C&mh=fk&mm=31%2C29&mn=sn-cv0tb0xn-ouxe%2Csn-25ge7nsd&ms=au%2Crdu&mv=m&mvi=1&pl=44&rms=au%2Cau&initcwndbps=4166250&bui=AYUSA3BeEPymsCW688TzC1ZpHijcz6skfGdMZdL2z6hNiPfzA1fLRh2-YuXV_G5A50vkAvPHqbGJHgfr&spc=wH4Qq_Yr2wsB&vprv=1&svpuc=1&mime=audio%2Fwebm&rqh=1&gir=yes&clen=4678490&dur=265.381&lmt=1737879139124145&mt=1767743107&fvip=2&keepalive=yes&fexp=51552689%2C51565115%2C51565681%2C51580968&c=ANDROID&txp=8218224&sparams=expire%2Cei%2Cip%2Cid%2Citag%2Csource%2Crequiressl%2Cxpc%2Cbui%2Cspc%2Cvprv%2Csvpuc%2Cmime%2Crqh%2Cgir%2Cclen%2Cdur%2Clmt&sig=AJfQdSswRgIhAJ2fBYKfsB61YuX5whPaSpuDV6cCfyyNouvAld9hHke3AiEA04sjTMw_RKsdF2VbqhcwU7MgQFPtkG9IzGC3IbG0E7A%3D&lsparams=cps%2Cmet%2Cmh%2Cmm%2Cmn%2Cms%2Cmv%2Cmvi%2Cpl%2Crms%2Cinitcwndbps&lsig=APaTxxMwRgIhALZW3a3DYpC3Jmd9hj97lR2MpkIzO8veOQC6Ogykxz91AiEAn-ZJ-aiBMSE7S5A2AOiiT3oGCz1ywbcHQIgGkISPRes%3D#id=TLrTTIVeYCo".to_string(),
            ..Default::default()
        });

        let metadata_cache = Arc::new(RwLock::new(LruCache::new(
            std::num::NonZeroUsize::new(10).expect("10 is non-zero"),
        )));

        let service = Arc::new(StreamingService::new(
            event_tx,
            mock_client,
            url_cache,
            metadata_cache,
            sem_tx,
            sem_rx,
        ));

        let track1 = YouTubeTrack {
            youtube_id: YouTubeId::new("TLrTTIVeYCo"),
            title: "Duplicate".to_string(),
            ..YouTubeTrack::default()
        };
        let track2 = YouTubeTrack {
            youtube_id: YouTubeId::new("new_id"),
            title: "New".to_string(),
            ..YouTubeTrack::default()
        };

        // Test bulk add
        service.handle_event(YouTubeEvent::AddManyToQueue(Arc::from([track1, track2]))).await;

        // Check results
        while let Ok(event) = event_rx.recv().await {
            if let YouTubeEvent::AddManyToQueueResult { added, skipped } = event {
                assert_eq!(added, 2);
                assert_eq!(skipped, 0);
                break;
            }
        }
    });
}

#[test]
fn test_playback_error_recovery() {
    smol::block_on(async {
        let (event_tx, event_rx) = unbounded::<YouTubeEvent>();
        let mock_client = Arc::new(MockYouTubeClient);
        let queue = Arc::new(RwLock::new(Vec::new()));
        let url_cache = Arc::new(RwLock::new(crate::youtube::cache::ExpiringCache::new(
            std::num::NonZeroUsize::new(10).expect("10 is non-zero"),
        )));

        let (sem_tx, sem_rx) = async_channel::bounded::<()>(3);
        for _ in 0..3 {
            let _ = sem_tx.send_blocking(());
        }

        let youtube_id = "test_id".to_string();
        queue
            .write()
            .push(Song { file: format!("https://mock.url#id={youtube_id}"), ..Default::default() });

        let metadata_cache = Arc::new(RwLock::new(LruCache::new(
            std::num::NonZeroUsize::new(10).expect("10 is non-zero"),
        )));

        let service = Arc::new(StreamingService::new(
            event_tx,
            mock_client,
            url_cache,
            metadata_cache,
            sem_tx,
            sem_rx,
        ));

        // Trigger playback error
        service
            .handle_event(YouTubeEvent::PlaybackError {
                youtube_id: youtube_id.clone(),
                pos: Some(123),
                _message: "403 Forbidden".to_string(),
                play_after_refresh: true,
            })
            .await;

        // Check if QueueUrlReplace event is emitted
        if let Ok(YouTubeEvent::QueueUrlReplace {
            url,
            youtube_id: returned_id,
            pos,
            play_after_replace,
        }) = event_rx.recv().await
        {
            assert_eq!(returned_id, youtube_id);
            assert_eq!(pos, Some(123));
            assert!(play_after_replace);
            assert!(url.contains("https://mock.url"));
            assert!(url.contains(&format!("#id={youtube_id}")));
        } else {
            panic!("Expected QueueUrlReplace event");
        }
    });
}

#[test]
fn test_metadata_refresh() {
    smol::block_on(async {
        use crate::youtube::metadata::MetadataService;
        use chrono::{Duration as ChronoDuration, Utc};
        use parking_lot::Mutex;

        let (event_tx, event_rx) = unbounded::<YouTubeEvent>();
        let db = Arc::new(Mutex::new(
            Database::in_memory().expect("In-memory database should be created"),
        ));
        let mock_client = Arc::new(MockYouTubeClient);

        // Add a stale track to the database
        let stale_track = YouTubeTrack {
            youtube_id: YouTubeId::new("stale_id"),
            title: "Old Title".to_string(),
            artist: "Old Artist".to_string(),
            last_modified_at: Utc::now() - ChronoDuration::days(8),
            ..YouTubeTrack::default()
        };
        db.lock().insert_track(&stale_track).expect("Track should be inserted");

        let (sem_tx, sem_rx) = async_channel::bounded::<()>(3);
        for _ in 0..3 {
            let _ = sem_tx.send_blocking(());
        }

        let service =
            Arc::new(MetadataService::new(event_tx, db.clone(), mock_client, sem_tx, sem_rx));

        // Trigger metadata refresh
        service.handle_event(YouTubeEvent::MetadataRefresh).await;

        // Wait for LibraryUpdated event
        while let Ok(event) = event_rx.recv().await {
            if matches!(event, YouTubeEvent::LibraryUpdated) {
                break;
            }
        }

        // Check if track was updated
        let updated = db
            .lock()
            .get_track("stale_id")
            .expect("Track should be fetched")
            .expect("Track should exist");
        assert_eq!(updated.title, "Mock Title");
        assert!(updated.last_modified_at > stale_track.last_modified_at);
    });
}

#[test]
#[allow(clippy::too_many_lines)]
fn test_full_youtube_url_refresh_flow() {
    smol::block_on(async {
        use crate::config::Config;
        use crate::core::scheduler::Scheduler;
        use crate::ctx::{Ctx, StickersSupport};
        use crate::mpd::commands::{Song, State, Status};
        use crate::mpd::version::Version;
        use std::collections::{HashMap, HashSet};

        // 1. Setup Environment
        // Ctx uses crossbeam channels
        let (app_tx, _) = crossbeam::channel::unbounded::<crate::AppEvent>();
        let (work_tx, _) = crossbeam::channel::unbounded::<crate::WorkRequest>();
        let (client_tx, _) =
            crossbeam::channel::unbounded::<crate::shared::events::ClientRequest>();

        // StreamingService uses async_channel
        let (event_tx, event_rx) = async_channel::unbounded::<YouTubeEvent>();

        let config = Arc::new(Config::default());

        let yt_id = "dQw4w9WgXcQ";
        let mpd_id = 100;
        // A realistic-looking long YouTube streaming URL with our fragment
        let full_url = format!(
            "https://rr1---sn-cv0tb0xn-ouxe.googlevideo.com/videoplayback?expire=1767765515&id=o-ANLDzNmmDLldUMHY1B5DSh9j9GkywtXCoVcydE_50kbo&itag=251&source=youtube&requiressl=yes#id={yt_id}"
        );

        let song = Song { id: mpd_id, file: full_url.clone(), ..Default::default() };

        let ctx = Ctx {
            mpd_version: Version { major: 0, minor: 23, patch: 0 },
            config: config.clone(),
            status: Status { state: State::Play, songid: Some(mpd_id), ..Status::default() },
            queue: vec![song.clone()],
            stickers: HashMap::new(),
            active_tab: "Queue".to_string().into(),
            supported_commands: HashSet::new(),
            db_update_start: None,
            app_event_sender: app_tx.clone(),
            work_sender: work_tx.clone(),
            client_request_sender: client_tx.clone(),
            needs_render: std::cell::Cell::new(false),
            stickers_to_fetch: std::cell::RefCell::new(HashSet::new()),
            lrc_index: crate::shared::lrc::LrcIndex::default(),
            rendered_frames: 0,
            scheduler: Scheduler::new((app_tx, client_tx)),
            messages: crate::shared::ring_vec::RingVec::default(),
            last_status_update: std::time::Instant::now(),
            song_played: None,
            stickers_supported: StickersSupport::Unsupported,
            input: crate::ui::input::InputManager::default(),
            key_resolver: crate::shared::keys::KeyResolver::new(&config),
            ytdlp_manager: crate::shared::ytdlp::YtDlpManager::new(work_tx),
            cached_queue_time_total: Duration::default(),
            youtube_manager: std::sync::Arc::new(
                crate::youtube::manager::YouTubeManager::new(
                    &std::path::PathBuf::from("/tmp/test_ctx_full_flow.db"),
                    None,
                    None,
                    String::new(),
                    crate::config::MpdAddress::default(),
                    None,
                )
                .expect("YouTubeManager should be created"),
            ),
        };

        // 2. Simulate MpdError with truncated and multi-line URL from user's log
        let error_msg = concat!(
            "ACK [5@0] {playid} Failed to decode \"https://rr1---sn-cv0tb0xn-ouxe.googlevideo.com/videoplayback?expire=1767765515&ei=q6Fdab-",
            "    uAfKIvdIP05KT6QI&ip=2001%3A861%3A5b82%3Ae3f0%3A3ff6%3A30f5%3Acaa7%3A33fd&id=o-ANLDzNmmDLldUMHY1B5DSh9j9GkywtXCoVcydE_50kbo&itag=251&source=youtube&requiressl=yes&xpc=EgVo2aDSNQ%3D%3D&cps=1&met=1767743915%2C&mh=fk&mm=31%2C29&mn=sn-cv0tb0xn-",
            "    ouxe%2Csn-25ge7nsd&ms=au%2Crdu&mv=m&mvi=1&pl=44&rms=au%2Cau&initcwndbps=4166250&bui=AYUSA3BeEPymsCW688TzC1ZpHijcz6skfGdMZdL2z6hNiPfzA1fLRh2-YuXV_G5A50vkAvPHqbGJHgfr&; got HTTP status 403"
        );

        let found_song = ctx
            .find_failed_song_in_queue(error_msg)
            .expect("Should find song in queue from multi-line error message");
        assert_eq!(found_song.id, mpd_id);
        assert_eq!(found_song.youtube_id().as_ref(), Some(&YouTubeId::new(yt_id)));

        // Verify we can find its position for replacement
        // In MPD, pos is the index in the queue
        assert_eq!(ctx.song_position(found_song.id).expect("Song should be in queue"), 0);

        // 4. Verify Refresh Logic (StreamingService)
        let mock_client = Arc::new(MockYouTubeClient);
        let url_cache = Arc::new(RwLock::new(crate::youtube::cache::ExpiringCache::new(
            std::num::NonZeroUsize::new(10).expect("10 is non-zero"),
        )));

        let (sem_tx, sem_rx) = async_channel::bounded::<()>(3);
        for _ in 0..3 {
            let _ = sem_tx.send_blocking(());
        }

        let metadata_cache = Arc::new(RwLock::new(LruCache::new(
            std::num::NonZeroUsize::new(10).expect("10 is non-zero"),
        )));

        let service = Arc::new(StreamingService::new(
            event_tx,
            mock_client,
            url_cache,
            metadata_cache,
            sem_tx,
            sem_rx,
        ));

        // Send the error event to the service (this is what event_loop.rs does)
        service
            .handle_event(YouTubeEvent::PlaybackError {
                youtube_id: yt_id.to_string(),
                pos: Some(0),
                _message: error_msg.to_string(),
                play_after_refresh: true,
            })
            .await;

        // Verify that the service fetches a new URL and emits QueueUrlReplace
        match event_rx.recv().await {
            Ok(YouTubeEvent::QueueUrlReplace {
                url,
                youtube_id: returned_id,
                pos,
                play_after_replace,
            }) => {
                assert_eq!(returned_id, yt_id);
                assert_eq!(pos, Some(0));
                assert!(play_after_replace);
                assert!(url.contains("https://mock.url"), "New URL should be from our mock client");
                assert!(
                    url.contains(&format!("#id={yt_id}")),
                    "New URL should still have the YouTube ID fragment"
                );
            }
            other => panic!("Expected QueueUrlReplace event, got {other:?}"),
        }
    });
}

#[test]
fn test_find_failed_song_correct_priority() {
    use crate::config::Config;
    use crate::core::scheduler::Scheduler;
    use crate::ctx::{Ctx, StickersSupport};
    use crate::mpd::commands::{Song, State, Status};
    use crate::mpd::version::Version;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    let (app_tx, _) = crossbeam::channel::unbounded::<crate::AppEvent>();
    let (client_tx, _) = crossbeam::channel::unbounded::<crate::shared::events::ClientRequest>();
    let config = Arc::new(Config::default());

    let yt_id_1 = "id_1_current";
    let yt_id_2 = "id_2_failed";

    let song_1 = Song {
        id: 1,
        file: format!("https://rr1---sn-1.googlevideo.com/videoplayback?id=song1#id={yt_id_1}"),
        ..Default::default()
    };
    let song_2 = Song {
        id: 2,
        file: format!("https://rr1---sn-2.googlevideo.com/videoplayback?id=song2#id={yt_id_2}"),
        ..Default::default()
    };

    let ctx = Ctx {
        mpd_version: Version { major: 0, minor: 23, patch: 0 },
        config: config.clone(),
        status: Status {
            state: State::Play,
            songid: Some(1), // Currently playing song 1
            ..Status::default()
        },
        queue: vec![song_1, song_2],
        stickers: HashMap::new(),
        active_tab: "Queue".to_string().into(),
        supported_commands: HashSet::new(),
        db_update_start: None,
        app_event_sender: app_tx.clone(),
        work_sender: crossbeam::channel::unbounded().0,
        client_request_sender: crossbeam::channel::unbounded().0,
        needs_render: std::cell::Cell::new(false),
        stickers_to_fetch: std::cell::RefCell::new(HashSet::new()),
        lrc_index: crate::shared::lrc::LrcIndex::default(),
        rendered_frames: 0,
        scheduler: Scheduler::new((app_tx, client_tx)),
        messages: crate::shared::ring_vec::RingVec::default(),
        last_status_update: std::time::Instant::now(),
        song_played: None,
        stickers_supported: StickersSupport::Unsupported,
        input: crate::ui::input::InputManager::default(),
        key_resolver: crate::shared::keys::KeyResolver::new(&config),
        ytdlp_manager: crate::shared::ytdlp::YtDlpManager::new(crossbeam::channel::unbounded().0),
        cached_queue_time_total: Duration::default(),
        youtube_manager: std::sync::Arc::new(
            crate::youtube::manager::YouTubeManager::new(
                &std::path::PathBuf::from("/tmp/test_ctx_priority.db"),
                None,
                None,
                String::new(),
                crate::config::MpdAddress::default(),
                None,
            )
            .expect("YouTubeManager should be created"),
        ),
    };
    // Error message for song 2 (failing to switch from 1 to 2)
    let error_msg = "ACK [5@0] {playid} Failed to decode \"https://rr1---sn-2.googlevideo.com/videoplayback?id=song2\"";

    let found_song = ctx.find_failed_song_in_queue(error_msg).expect("Should find song in queue");

    // SHOULD match song 2, NOT song 1 (even though song 1 is currently playing)
    assert_eq!(found_song.id, 2);
    assert_eq!(found_song.youtube_id().as_ref(), Some(&YouTubeId::new(yt_id_2)));
}

struct MockYouTubeClient;
impl crate::youtube::ytdlp::YouTubeClient for MockYouTubeClient {
    fn search(
        &self,
        _query: &str,
        _limit: usize,
    ) -> YouTubeResult<
        Pin<
            Box<
                dyn futures_lite::Stream<Item = YouTubeResult<crate::youtube::models::SearchResult>>
                    + Send,
            >,
        >,
    > {
        unimplemented!()
    }
    fn get_streaming_url(
        &self,
        _youtube_id: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = YouTubeResult<String>> + Send + 'static>> {
        Box::pin(async { Ok("https://mock.url".to_string()) })
    }
    fn get_metadata(
        &self,
        youtube_id: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = YouTubeResult<YouTubeTrack>> + Send + 'static>>
    {
        let youtube_id = youtube_id.to_string();
        Box::pin(async move {
            Ok(YouTubeTrack {
                youtube_id: YouTubeId::new(youtube_id),
                title: "Mock Title".to_string(),
                artist: "Mock Artist".to_string(),
                ..Default::default()
            })
        })
    }
}

#[test]
fn test_lrc_path_resolution() {
    let lyrics_dir = "/home/user/.lyrics";

    // Test YouTube track resolution
    let yt_song =
        Song { file: "https://youtube.com/watch?v=abc#id=abc".to_string(), ..Default::default() };
    let yt_path = if let Some(yt_id) = yt_song.youtube_id() {
        std::path::PathBuf::from(lyrics_dir).join(format!("{yt_id}.lrc"))
    } else {
        panic!("Should have youtube_id");
    };
    assert_eq!(yt_path, std::path::PathBuf::from("/home/user/.lyrics/abc.lrc"));

    // Test local track resolution
    let song_file = "artist/album/song.flac";
    let local_path =
        crate::shared::lrc::get_lrc_path(lyrics_dir, song_file).expect("Should get lrc path");
    assert_eq!(local_path, std::path::PathBuf::from("/home/user/.lyrics/artist/album/song.lrc"));
}

#[rstest]
fn test_thumbnail_download(lyrics_ctx: LyricsTestContext) {
    smol::block_on(async {
        // Use a known video ID
        let youtube_id = YouTubeId::new("dQw4w9WgXcQ");
        let cache_dir = lyrics_ctx.temp_dir.clone();

        // Create a new service with the temp dir as cache_dir
        let (tx, rx) = unbounded::<YouTubeEvent>();
        let (sem_tx, sem_rx) = async_channel::bounded::<()>(3);
        for _ in 0..3 {
            let _ = sem_tx.send_blocking(());
        }

        let service = Arc::new(DownloadService::new(
            tx,
            Some(cache_dir.clone()),
            Some(lyrics_ctx.lyrics_dir.to_string_lossy().to_string()),
            "en".to_string(),
            MpdAddress::default(),
            None,
            sem_tx,
            sem_rx,
        ));

        // Trigger download
        service.handle_event(YouTubeEvent::DownloadThumbnail(youtube_id.clone())).await;

        // Wait for result
        match rx.recv().await {
            Ok(YouTubeEvent::ThumbnailDownloaded { youtube_id: returned_id, path }) => {
                assert_eq!(returned_id, youtube_id);
                assert!(path.exists(), "Thumbnail file should exist");
                assert!(path.starts_with(&cache_dir), "Thumbnail should be in cache dir");
                assert_eq!(path.extension().expect("path should have extension"), "jpg");
            }
            Ok(other) => panic!("Unexpected event: {other:?}"),
            Err(e) => panic!("Channel error: {e:?}"),
        }
    });
}

#[test]
fn test_import_library_csv() {
    smol::block_on(async {
        use crate::youtube::events::YouTubeEvent;
        use crate::youtube::import::ImportService;
        use parking_lot::Mutex;
        use std::io::Write;

        let temp_dir =
            std::env::temp_dir().join(format!("rmpc_test_import_lib_{}", rand::random::<u64>()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");
        let csv_path = temp_dir.join("test.csv");

        let mut file = std::fs::File::create(&csv_path).expect("Failed to create CSV file");
        writeln!(file, "ID vidéo,Song Title,Album Title,Artist Name 1,Artist Name 2").unwrap();
        writeln!(file, "iqizXvvWnmM,Lumière,Clair Obscur: Expedition 33 (Original Soundtrack),Lorien Testard,Alice Duport-Percier").unwrap();
        writeln!(
            file,
            "Vqc9AqhJ6Xg,歌うたいのバラッド - Uta Utai No Ballad,Mirai Kuso/Utautaino Ballad,aluto,"
        )
        .unwrap();

        let (event_tx, event_rx) = async_channel::unbounded::<YouTubeEvent>();
        let db = Arc::new(Mutex::new(Database::in_memory().expect("DB init failed")));
        let client = Arc::new(MockYouTubeClient);
        let (permit_tx, permit_rx) = async_channel::bounded(10);
        permit_tx.send(()).await.unwrap(); // Allow at least one
        permit_tx.send(()).await.unwrap();

        let service = Arc::new(ImportService::new(
            event_tx,
            db.clone(),
            MpdAddress::default(),
            None,
            permit_tx,
            permit_rx,
            client,
        ));

        service
            .handle_event(YouTubeEvent::ImportLibrary(csv_path.to_string_lossy().to_string()))
            .await;

        // Wait for finish
        let mut finished = false;
        while let Ok(event) = event_rx.recv().await {
            if let YouTubeEvent::ImportFinished { success, skipped, failed } = event {
                assert_eq!(success, 2);
                assert_eq!(skipped, 0);
                assert_eq!(failed, 0);
                finished = true;
                break;
            }
        }
        assert!(finished, "Import did not finish");

        // Verify DB
        let db_lock = db.lock();
        let track1 = db_lock.get_track("iqizXvvWnmM").unwrap().expect("Track 1 missing");
        assert_eq!(track1.title, "Mock Title"); // Mock client returns "Mock Title"

        std::fs::remove_dir_all(&temp_dir).unwrap();
    });
}

#[test]
fn test_import_playlist_folder() {
    smol::block_on(async {
        use crate::youtube::events::YouTubeEvent;
        use crate::youtube::import::ImportService;
        use parking_lot::Mutex;
        use std::io::Write;

        let temp_dir =
            std::env::temp_dir().join(format!("rmpc_test_import_pl_{}", rand::random::<u64>()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let playlists_dir = temp_dir.join("playlists");
        std::fs::create_dir(&playlists_dir).unwrap();

        let csv_path = playlists_dir.join("playlist-test.csv");
        let mut file = std::fs::File::create(&csv_path).expect("Failed to create CSV file");
        writeln!(file, "ID vidéo,Code temporel de création de la vidéo de la playlist").unwrap();
        writeln!(file, "iqizXvvWnmM,2025-08-08T16:45:33+00:00").unwrap();
        writeln!(file, "1XYh1K3pQjI,2025-08-08T16:45:43+00:00").unwrap();

        let (event_tx, event_rx) = async_channel::unbounded::<YouTubeEvent>();
        let db = Arc::new(Mutex::new(Database::in_memory().expect("DB init failed")));
        let client = Arc::new(MockYouTubeClient);
        let (permit_tx, permit_rx) = async_channel::bounded(10);
        for _ in 0..10 {
            permit_tx.send(()).await.unwrap();
        }

        let service = Arc::new(ImportService::new(
            event_tx,
            db.clone(),
            MpdAddress::default(),
            None,
            permit_tx,
            permit_rx,
            client,
        ));

        service
            .handle_event(YouTubeEvent::ImportPlaylists(
                playlists_dir.to_string_lossy().to_string(),
            ))
            .await;

        // Wait for LibraryUpdated
        let mut finished = false;
        while let Ok(event) = event_rx.recv().await {
            if let YouTubeEvent::LibraryUpdated = event {
                finished = true;
                break;
            }
        }
        assert!(finished, "Playlist import did not finish");

        // Verify DB
        let db_lock = db.lock();
        let track1 = db_lock.get_track("iqizXvvWnmM").unwrap().expect("Track 1 missing");
        let track2 = db_lock.get_track("1XYh1K3pQjI").unwrap().expect("Track 2 missing");
        assert_eq!(track1.title, "Mock Title");
        assert_eq!(track2.title, "Mock Title");

        std::fs::remove_dir_all(&temp_dir).unwrap();
    });
}
