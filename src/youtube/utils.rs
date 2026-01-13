use crate::youtube::models::YouTubeId;

pub fn parse_csv_for_ids(path: &std::path::Path) -> Vec<YouTubeId> {
    let mut ids = Vec::new();
    if let Ok(mut rdr) = csv::Reader::from_path(path) {
        for result in rdr.records().flatten() {
            if let Some(url_or_id) = result.get(0) {
                if let Some(id) = YouTubeId::from_any(url_or_id) {
                    ids.push(id);
                }
            }
        }
    }
    ids
}

pub fn clean_metadata(artist: &str, title: &str) -> (String, String) {
    let mut clean_artist = artist.to_string();
    let mut clean_title = title.to_string();

    // 1. If title contains " - ", it's often "Artist - Title"
    if title.contains(" - ") {
        let parts: Vec<&str> = title.splitn(2, " - ").collect();
        clean_artist = parts[0].trim().to_string();
        clean_title = parts[1].trim().to_string();
    }

    // 2. Handle multiple artists (e.g., "Artist1, Artist2")
    if clean_artist.contains(',') {
        if let Some(first) = clean_artist.split(',').next() {
            clean_artist = first.trim().to_string();
        }
    }

    // 3. Title Cleaning
    // 3a. Remove the LAST suffix in () or []
    let last_bracket_start = clean_title.rfind(['(', '[']);
    if let Some(start) = last_bracket_start {
        let opening_char = clean_title.chars().nth(start).unwrap_or('(');
        let closing_char = if opening_char == '(' { ')' } else { ']' };

        if let Some(end) = clean_title[start..].find(closing_char) {
            let absolute_end = start + end + 1;
            // Only remove if it's actually the last thing (or followed only by whitespace)
            if clean_title[absolute_end..].trim().is_empty() {
                clean_title.replace_range(start..absolute_end, "");
            }
        }
    }

    clean_title = clean_title.trim().to_string();

    // 3b. Remove (feat.*) or (Official.*) patterns if they remain
    let patterns = ["feat.", "official"];
    let mut to_remove = None;

    for pattern in patterns {
        if let Some(idx) = clean_title.to_lowercase().find(pattern) {
            // Check if preceded by ( or [
            if idx > 0 {
                let prev_char = clean_title.as_bytes()[idx - 1];
                if prev_char == b'(' || prev_char == b'[' {
                    let opening_char = prev_char as char;
                    let closing_char = if opening_char == '(' { ')' } else { ']' };

                    // Find the matching closing bracket
                    if let Some(end_idx) = clean_title[idx..].find(closing_char) {
                        #[allow(clippy::range_plus_one)]
                        {
                            to_remove = Some((idx - 1)..(idx + end_idx + 1));
                        }
                        break;
                    }
                    // If no closing bracket, remove to the end
                    to_remove = Some((idx - 1)..clean_title.len());
                    break;
                }
            }
        }
    }

    if let Some(range) = to_remove {
        clean_title.replace_range(range, "");
    }

    (clean_artist.trim().to_string(), clean_title.trim().to_string().replace('_', " "))
}
