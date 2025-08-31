// src/youtube/security/command_security.rs

use crate::youtube::{
    error::YouTubeError,
    parse_song_info_json,
    service::{events::YouTubeEventEmitter},
    ResolvedYouTubeSong,
};
use anyhow::{Context, Result};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::mpsc,
    time::timeout,
};

const MAX_OUTPUT_SIZE: usize = 10 * 1024 * 1024; // 10MB
const DEFAULT_AUDIO_FORMAT: &str = "bestaudio[protocol^=https]";

#[derive(Debug, Clone)]
pub struct SecureYtDlpCommand {
    timeout: Duration,
    max_output_size: usize,
    ytdlp_path: String,
    extra_args: Vec<String>,
    verbose: bool,
}

impl Default for SecureYtDlpCommand {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_output_size: MAX_OUTPUT_SIZE,
            ytdlp_path: "yt-dlp".to_string(),
            extra_args: Vec::new(),
            verbose: false,
        }
    }
}

impl SecureYtDlpCommand {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_ytdlp_path(mut self, path: &str) -> Self {
        self.ytdlp_path = path.to_string();
        self
    }

    pub fn with_extra_args(mut self, args: &[String]) -> Self {
        self.extra_args = args.to_vec();
        self
    }

    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    fn build_base_command(&self) -> Command {
        let mut cmd = Command::new(&self.ytdlp_path);
        cmd.arg("--no-warnings");
        
        if !self.verbose {
            cmd.arg("--quiet");
        }
        
        cmd.args(&self.extra_args);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd
    }

    fn build_search_url(&self, sanitized_query: &str) -> String {
        format!(
            "https://music.youtube.com/search?q={}",
            urlencoding::encode(sanitized_query)
        )
    }

    fn build_video_url(&self, youtube_id: &str) -> String {
        format!("https://www.youtube.com/watch?v={}", youtube_id)
    }
    
    /// Analyzes the output of a finished command, returning the stdout on success
    /// or a specific, parsed YouTubeError on failure. Adheres to SRP and DRY.
    fn process_command_output(&self, output: std::process::Output) -> Result<Vec<u8>, YouTubeError> {
        if output.status.success() {
            if output.stdout.len() > self.max_output_size {
                return Err(YouTubeError::ResponseParseError(format!(
                    "Output size exceeds maximum of {} bytes", self.max_output_size
                )));
            }
            return Ok(output.stdout);
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        log::debug!("yt-dlp failed with status {}. stderr: {}", output.status, stderr);

        // Parse stderr for specific, actionable errors.
        if stderr.contains("Video unavailable") || stderr.contains("Private video") {
            return Err(YouTubeError::VideoUnavailable);
        }
        if stderr.contains("HTTP Error 403") || stderr.contains("Access denied") {
            return Err(YouTubeError::VideoUnavailable); // Often due to region locks
        }
        if stderr.contains("Name or service not known") || stderr.contains("Could not resolve host") {
            return Err(YouTubeError::NetworkError(
                "DNS or host resolution failed".to_string(),
            ));
        }
        // Fallback for any other failure.
        Err(YouTubeError::CommandFailed(stderr.to_string()))
    }

    /// A robust, unified way to execute a command and process its result.
    async fn run_and_process_command(&self, mut command: Command) -> Result<Vec<u8>, YouTubeError> {
        let output_future = command.output();
        let output = timeout(self.timeout, output_future)
            .await
            .map_err(|_| YouTubeError::Timeout(self.timeout))?
            .map_err(YouTubeError::from)?; // Converts std::io::Error to our specific type

        self.process_command_output(output)
    }

    /// Executes a search with yt-dlp and streams results back via the emitter in real-time.
    pub async fn execute_search_streaming(
        &self,
        sanitized_query: &str,
        max_results: usize,
        generation: u64,
        emitter: &dyn YouTubeEventEmitter,
    ) -> Result<(), YouTubeError> {
        let search_url = self.build_search_url(sanitized_query);
        let mut command = self.build_base_command();
        command
            .arg("--print-json") // Use --print-json for one object per line
            .arg("--playlist-items")
            .arg(format!("1:{}", max_results))
            .arg(&search_url);

        let operation = async {
            let mut child = command.spawn().map_err(YouTubeError::from)?;
            let stdout = child.stdout.take().context("Failed to capture stdout").map_err(|e| YouTubeError::Internal(e.to_string()))?;
            let stderr = child.stderr.take().context("Failed to capture stderr").map_err(|e| YouTubeError::Internal(e.to_string()))?;

            let mut reader = BufReader::new(stdout).lines();
            let mut results_count = 0;

            // Concurrently read stderr to capture error messages without blocking stdout.
            let (stderr_tx, mut stderr_rx) = mpsc::channel(1);
            tokio::spawn(async move {
                let mut stderr_lines = BufReader::new(stderr).lines();
                let mut stderr_output = Vec::new();
                while let Ok(Some(line)) = stderr_lines.next_line().await {
                    stderr_output.push(Ok(line));
                }
                let _ = stderr_tx.send(stderr_output).await;
            });

            while let Some(line) = reader.next_line().await.map_err(|e| YouTubeError::ResponseParseError(e.to_string()))? {
                if let Ok(song_info) = parse_song_info_json(line.as_bytes()) {
                    if emitter.emit_search_result(song_info, generation).is_err() {
                        log::warn!("Event emitter channel closed, stopping search stream.");
                        break;
                    }
                    results_count += 1;
                }
            }

            let status = child.wait().await.map_err(|e| YouTubeError::CommandFailed(e.to_string()))?;
            if !status.success() {
                if let Some(stderr_lines) = stderr_rx.recv().await {
                    let stderr_str = stderr_lines.into_iter().filter_map(Result::ok).collect::<Vec<_>>().join("\n");
                    // Use the same robust parsing as our other commands.
                    let fake_output = std::process::Output { status, stdout: vec![], stderr: stderr_str.into_bytes() };
                    if let Err(e) = self.process_command_output(fake_output) {
                        return Err(e);
                    }
                }
                return Err(YouTubeError::CommandFailed("yt-dlp failed with no specific error message.".to_string()));
            }

            emitter.emit_search_complete(generation, results_count).map_err(|_| YouTubeError::Internal("Emitter closed".to_string()))?;
            Ok(())
        };

        timeout(self.timeout, operation)
            .await
            .map_err(|_| YouTubeError::Timeout(self.timeout))?
    }

    pub async fn get_stream_url(&self, youtube_id: &str) -> Result<String, YouTubeError> {
        let video_url = self.build_video_url(youtube_id);
        let mut command = self.build_base_command();
        command.arg("-g").arg("-f").arg(DEFAULT_AUDIO_FORMAT).arg(&video_url);

        let stdout = self.run_and_process_command(command).await?;
        let url = String::from_utf8(stdout).map_err(|e| YouTubeError::ResponseParseError(e.to_string()))?.trim().to_string();

        if url.is_empty() || !url.starts_with("https://") {
            // If we get here, it's likely a silent failure for an unavailable video.
            return Err(YouTubeError::VideoUnavailable);
        }
        Ok(url)
    }

    pub async fn get_song_info(&self, youtube_id: &str) -> Result<Option<ResolvedYouTubeSong>, YouTubeError> {
        let video_url = self.build_video_url(youtube_id);
        let mut command = self.build_base_command();
        command.arg("--dump-json").arg(&video_url);

        let stdout = self.run_and_process_command(command).await?;
        if stdout.is_empty() {
            return Ok(None);
        }

        Ok(Some(parse_song_info_json(&stdout).map_err(|e| YouTubeError::ResponseParseError(e.to_string()))?))
    }
    
    pub async fn get_song_info_batch(&self, youtube_ids: &[String]) -> Result<Vec<ResolvedYouTubeSong>, YouTubeError> {
        let mut command = self.build_base_command();
        command.arg("--dump-json");
        for id in youtube_ids {
            command.arg(self.build_video_url(id));
        }

        let stdout = self.run_and_process_command(command).await?;
        let mut results = Vec::new();
        let mut lines = BufReader::new(&stdout[..]).lines();
        while let Some(line) = lines.next_line().await.map_err(|e| YouTubeError::ResponseParseError(e.to_string()))? {
            if let Ok(song) = parse_song_info_json(line.as_bytes()) {
                results.push(song);
            }
        }
        Ok(results)
    }

    pub async fn health_check(&self) -> Result<(), YouTubeError> {
        let mut command = Command::new(&self.ytdlp_path);
        command.arg("--version");
        self.run_and_process_command(command).await?;
        Ok(())
    }
}
