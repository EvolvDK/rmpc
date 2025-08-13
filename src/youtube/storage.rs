use std::{collections::BTreeMap, fs, path::PathBuf};

use anyhow::{Context, Result};
use itertools::Itertools;
use serde::{Deserialize, Serialize};

use super::YouTubeVideo;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PlaylistItem {
    Youtube { id: String },
    Local { path: String },
}

fn get_library_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().context("Could not find config directory")?;
    let rmpc_dir = config_dir.join("rmpc");
    Ok(rmpc_dir.join("youtube_library.json"))
}

pub fn save_library(library: &BTreeMap<String, Vec<YouTubeVideo>>) -> Result<()> {
    let path = get_library_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json_string = serde_json::to_string_pretty(library)?;
    fs::write(path, json_string)?;
    Ok(())
}

pub fn load_library() -> Result<BTreeMap<String, Vec<YouTubeVideo>>> {
    let path = get_library_path()?;
    match fs::read_to_string(path) {
        Ok(json_string) => {
            let library = serde_json::from_str(&json_string)?;
            Ok(library)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(BTreeMap::new()) // File doesn't exist, return empty library
        }
        Err(e) => Err(e.into()),
    }
}

pub fn list_playlists() -> Result<Vec<String>> {
    let dir = get_playlists_dir()?;
    fs::create_dir_all(&dir)?;
    let mut playlists = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("json") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                playlists.push(stem.to_owned());
            }
        }
    }
    playlists.sort_unstable();
    Ok(playlists)
}

pub fn delete_playlist(name: &str) -> Result<()> {
    let path = get_playlists_dir()?.join(format!("{}.json", name));
    fs::remove_file(path).context("Failed to delete playlist file")
}

pub fn rename_playlist(old_name: &str, new_name: &str) -> Result<()> {
    let dir = get_playlists_dir()?;
    let old_path = dir.join(format!("{}.json", old_name));
    let new_path = dir.join(format!("{}.json", new_name));
    fs::rename(old_path, new_path).context("Failed to rename playlist file")
}

fn get_playlists_dir() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().context("Could not find config directory")?;
    let rmpc_dir = config_dir.join("rmpc");
    Ok(rmpc_dir.join("yt-playlists"))
}

pub fn save_playlist(name: &str, items: &[PlaylistItem]) -> Result<()> {
    let dir = get_playlists_dir()?;
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", name));
    let unique_items: Vec<_> = items.iter().cloned().unique().collect();
    let json_string = serde_json::to_string_pretty(&unique_items)?;
    fs::write(path, json_string)?;
    Ok(())
}

pub fn load_playlist(name: &str) -> Result<Vec<PlaylistItem>> {
    let path = get_playlists_dir()?.join(format!("{}.json", name));
    match fs::read_to_string(path) {
        Ok(json_string) => {
            let items = serde_json::from_str(&json_string)?;
            Ok(items)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(anyhow::anyhow!("Playlist '{}' not found", name))
        }
        Err(e) => Err(e.into()),
    }
}
