// src/youtube/security/validation.rs

use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashSet;

const MAX_QUERY_LENGTH: usize = 200;
const MAX_YOUTUBE_ID_LENGTH: usize = 20;
const MIN_QUERY_LENGTH: usize = 1;

static YOUTUBE_ID_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-zA-Z0-9_-]{11}$").expect("Invalid YouTube ID regex"));

static SAFE_QUERY_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^[a-zA-Z0-9\s\-_.,!?()'&:#+]+$").expect("Invalid query validation regex")
});


static DANGEROUS_REGEX_PATTERNS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        r"(.*)*",
        r"(.+)*",
        r"(.{0,})*",
        r"(\w*)*",
        r"(\d+)*\d+",
        r"(.*).*",
        r"(.*)+",
        r".*.*.*",
    ]
    .iter()
    .copied()
    .collect()
});

#[derive(Debug, thiserror::Error, Clone)]
pub enum ValidationError {
    #[error("Query is empty or contains only whitespace")]
    EmptyQuery,
    #[error("Query is too short (minimum {MIN_QUERY_LENGTH} characters)")]
    QueryTooShort,
    #[error("Query exceeds maximum length of {MAX_QUERY_LENGTH} characters")]
    QueryTooLong,
    #[error("Query contains unsafe characters or patterns")]
    UnsafeCharacters,
    #[error("YouTube ID is invalid format")]
    InvalidYouTubeId,
    #[error("Regex pattern is potentially unsafe or too complex")]
    UnsafeRegex,
    #[error("Input contains suspicious patterns that may indicate injection attempts")]
    SuspiciousInput,
}

/// Sanitize and validate a search query.
pub fn validate_and_sanitize_query(query: &str) -> Result<String, ValidationError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(ValidationError::EmptyQuery);
    }
    if trimmed.len() < MIN_QUERY_LENGTH {
        return Err(ValidationError::QueryTooShort);
    }
    if trimmed.len() > MAX_QUERY_LENGTH {
        return Err(ValidationError::QueryTooLong);
    }
    if !SAFE_QUERY_PATTERN.is_match(trimmed) {
        return Err(ValidationError::UnsafeCharacters);
    }
    let suspicious_patterns = ["javascript:", "<script", "eval(","document.", "window.", "location.", "setTimeout", "setInterval", "--", "/*", "*/", "drop table", "union select", "../", "..\\",];
    let query_lower = trimmed.to_lowercase();
    for pattern in &suspicious_patterns {
        if query_lower.contains(pattern) {
            return Err(ValidationError::SuspiciousInput);
        }
    }

    let sanitized = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    Ok(sanitized)
}

/// Validate a YouTube video ID
pub fn validate_youtube_id(id: &str) -> Result<(), ValidationError> {
    if id.len() != 11 { // Stricter check for length
        return Err(ValidationError::InvalidYouTubeId);
    }

    if !YOUTUBE_ID_PATTERN.is_match(id) {
        return Err(ValidationError::InvalidYouTubeId);
    }

    // Additional security check for suspicious patterns
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err(ValidationError::InvalidYouTubeId);
    }

    Ok(())
}

/// Validate a regex pattern for safety
pub fn validate_regex_pattern(pattern: &str) -> Result<Regex, ValidationError> {
    // Check against known dangerous patterns
    for dangerous in DANGEROUS_REGEX_PATTERNS.iter() {
        if pattern.contains(dangerous) {
            return Err(ValidationError::UnsafeRegex);
        }
    }
    
    // Additional complexity checks
    if pattern.len() > 1000 {
        return Err(ValidationError::UnsafeRegex);
    }
    
    // Count nested quantifiers
    let quantifier_count = pattern.matches('+').count() 
        + pattern.matches('*').count() 
        + pattern.matches('{').count();
    
    if quantifier_count > 10 {
        return Err(ValidationError::UnsafeRegex);
    }
    
    Regex::new(pattern).map_err(|_| ValidationError::UnsafeRegex)
}

/// Safely match a regex against text with length limits
pub fn safe_regex_match(pattern: &Regex, text: &str) -> bool {
    const MAX_TEXT_LENGTH: usize = 10_000;
    
    if text.len() > MAX_TEXT_LENGTH {
        return false;
    }
    
    // Use a timeout mechanism if available in your environment
    pattern.is_match(text)
}

/// Validate URL components for additional security
pub fn validate_url_component(component: &str) -> Result<(), ValidationError> {
    if component.is_empty() {
        return Ok(());
    }
    
    // Check for URL injection patterns
    let suspicious_url_patterns = [
        "javascript:",
        "data:",
        "file:",
        "ftp:",
        "about:",
        "chrome:",
        "chrome-extension:",
        "moz-extension:",
    ];
    
    let component_lower = component.to_lowercase();
    for pattern in &suspicious_url_patterns {
        if component_lower.starts_with(pattern) {
            return Err(ValidationError::SuspiciousInput);
        }
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_query_success() {
        assert_eq!(
            validate_and_sanitize_query("  hello world!  ").unwrap(),
            "hello world!"
        );
        assert!(validate_and_sanitize_query("normal-query_123").is_ok());
    }

    #[test]
    fn test_validate_query_failures() {
        assert!(matches!(validate_and_sanitize_query(""), Err(ValidationError::EmptyQuery)));
        assert!(matches!(validate_and_sanitize_query("   "), Err(ValidationError::EmptyQuery)));
        assert!(matches!(validate_and_sanitize_query("a".repeat(300)), Err(ValidationError::QueryTooLong)));
        assert!(matches!(validate_and_sanitize_query("<script>"), Err(ValidationError::UnsafeCharacters)));
        assert!(matches!(validate_and_sanitize_query("drop table users;"), Err(ValidationError::SuspiciousInput)));
    }

    #[test]
    fn test_validate_youtube_id_success() {
        assert!(validate_youtube_id("dQw4w9WgXcQ").is_ok());
        assert!(validate_youtube_id("a-b_c-d_123").is_ok());
    }

    #[test]
    fn test_validate_youtube_id_failures() {
        assert!(matches!(validate_youtube_id("toolongid123"), Err(ValidationError::InvalidYouTubeId)));
        assert!(matches!(validate_youtube_id("shortid"), Err(ValidationError::InvalidYouTubeId)));
        assert!(matches!(validate_youtube_id("!@#$%^&*()_+"), Err(ValidationError::InvalidYouTubeId)));
        assert!(matches!(validate_youtube_id("../etc/pwd"), Err(ValidationError::InvalidYouTubeId)));
    }

    #[test]
    fn test_validate_regex_pattern_safety() {
        assert!(validate_regex_pattern("simple.*").is_ok());
        // Known ReDoS pattern
        assert!(matches!(validate_regex_pattern("(a+)+"), Err(ValidationError::UnsafeRegex)));
        // Invalid syntax
        assert!(matches!(validate_regex_pattern("[invalid"), Err(ValidationError::UnsafeRegex)));
    }
}
