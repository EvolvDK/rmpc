use crate::youtube::{
    security::validation::{safe_regex_match, validate_regex_pattern},
    ResolvedYouTubeSong,
};
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use itertools::Itertools;

struct DebugSkimMatcher(SkimMatcherV2);

impl std::fmt::Debug for DebugSkimMatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkimMatcherV2").finish_non_exhaustive()
    }
}

impl Default for DebugSkimMatcher {
    fn default() -> Self {
        Self(SkimMatcherV2::default().ignore_case())
    }
}

/// Defines the available search filtering modes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    #[default]
    Fuzzy,
    Contains,
    StartsWith,
    Exact,
    Regex,
}

impl SearchMode {
    const MODES: &'static [Self] = &[Self::Fuzzy, Self::Contains, Self::StartsWith, Self::Exact, Self::Regex];

    pub fn next(&self) -> Self {
        let current_index = Self::MODES.iter().position(|&m| m == *self).unwrap_or(0);
        Self::MODES[(current_index + 1) % Self::MODES.len()]
    }

    pub fn previous(&self) -> Self {
        let current_index = Self::MODES.iter().position(|&m| m == *self).unwrap_or(0);
        Self::MODES[(current_index + Self::MODES.len() - 1) % Self::MODES.len()]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Fuzzy => "Fuzzy",
            Self::Contains => "Contains",
            Self::StartsWith => "Starts With",
            Self::Exact => "Exact Match",
            Self::Regex => "Regex",
        }
    }
}

/// Represents a search result after filtering, including highlighting information.
#[derive(Debug, Clone)]
pub struct FilteredSearchResult {
    pub score: i64,
    pub song: ResolvedYouTubeSong,
    pub highlighted_indices: Vec<usize>,
}

/// Manages the state of the YouTube search UI.
#[derive(Debug, Default)]
pub struct YouTubeSearchController {
    search_mode: SearchMode,
    search_generation: u64,
    raw_results: Vec<ResolvedYouTubeSong>,
    filtered_results: Vec<FilteredSearchResult>,
    current_query: String,
    search_query: String,
    is_loading: bool,
    matcher: DebugSkimMatcher,
    pub regex_error: Option<String>,
}

impl YouTubeSearchController {
    pub fn new() -> Self {
        Self::default()
    }

    // === Public API for UI State ===

    pub fn search_mode(&self) -> SearchMode { self.search_mode }
    pub fn current_query(&self) -> &str { &self.current_query }
    pub fn filtered_results(&self) -> &[FilteredSearchResult] { &self.filtered_results }
    pub fn is_loading(&self) -> bool { self.is_loading }
    pub fn current_generation(&self) -> u64 { self.search_generation }

    // === State Mutators ===
    
    pub fn set_search_mode(&mut self, mode: SearchMode) {
        self.search_mode = mode;
        self.apply_current_filter();
    }

    pub fn next_search_mode(&mut self) {
        self.search_mode = self.search_mode.next();
        self.apply_current_filter();
    }

    pub fn previous_search_mode(&mut self) {
        self.search_mode = self.search_mode.previous();
        self.apply_current_filter();
    }

    /// Prepares for a new search, clearing previous results and returning the new generation ID.
    pub fn start_new_search(&mut self, query: String) -> u64 {
        self.current_query = query.clone();
        self.search_query = query;
        self.search_generation += 1;
        self.is_loading = true;
        self.raw_results.clear();
        self.filtered_results.clear();
        self.search_generation
    }

    /// Adds a result from the service and re-applies the current filter.
    pub fn add_search_result(&mut self, song: ResolvedYouTubeSong, generation: u64) {
        if generation == self.search_generation {
            self.raw_results.push(song);
            self.apply_current_filter();
        }
    }

    /// Marks the current search as complete.
    pub fn complete_search(&mut self, generation: u64) {
        if generation == self.search_generation {
            self.is_loading = false;
        }
    }

    /// Updates the filter based on a new query text without fetching new results.
    pub fn filter_results(&mut self, query: &str) {
        self.current_query = query.to_string();
        self.apply_current_filter();
    }

    // === Private Filtering Logic ===

    fn apply_current_filter(&mut self) {
		self.regex_error = None;
        if self.current_query.is_empty() || self.current_query == self.search_query {
            self.filtered_results = self.raw_results.iter().map(|song| FilteredSearchResult {
                score: 100, song: song.clone(), highlighted_indices: vec![]
            }).collect();
            return;
        }
        match self.search_mode {
            SearchMode::Fuzzy => self.apply_fuzzy_filter(),
            SearchMode::Contains => self.apply_simple_filter(|song, query| song.title.to_lowercase().contains(query)),
            SearchMode::StartsWith => self.apply_simple_filter(|song, query| song.title.to_lowercase().starts_with(query)),
            SearchMode::Exact => self.apply_simple_filter(|song, query| song.title.eq_ignore_ascii_case(query)),
            SearchMode::Regex => self.apply_regex_filter(),
        }
    }
    
    fn apply_simple_filter<F>(&mut self, predicate: F)
    where
        F: Fn(&ResolvedYouTubeSong, &str) -> bool,
    {
        let query = match self.search_mode {
            SearchMode::Exact => self.current_query.clone(),
            _ => self.current_query.to_lowercase(),
        };
        self.filtered_results = self.raw_results.iter()
            .filter(|song| predicate(song, &query))
            .map(|song| FilteredSearchResult {
                score: 100,
                song: song.clone(),
                highlighted_indices: vec![],
            })
            .collect();
    }
    
    fn apply_fuzzy_filter(&mut self) {
        self.filtered_results = self.raw_results.iter().filter_map(|song| {
            self.matcher.0.fuzzy_indices(&song.title, &self.current_query)
                .map(|(score, indices)| FilteredSearchResult {
                    score, song: song.clone(), highlighted_indices: indices,
                })
        }).sorted_by_key(|result| -result.score).collect();
    }

    fn apply_regex_filter(&mut self) {
        match validate_regex_pattern(&self.current_query) {
            Ok(regex) => {
                self.filtered_results = self.raw_results.iter()
                    .filter(|song| safe_regex_match(&regex, &song.title))
                    .map(|song| FilteredSearchResult { score: 100, song: song.clone(), highlighted_indices: vec![] })
                    .collect();
            }
            Err(e) => {
                self.filtered_results.clear();
                self.regex_error = Some(e.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::youtube::ResolvedYouTubeSong;

    fn create_song(title: &str) -> ResolvedYouTubeSong {
        ResolvedYouTubeSong { title: title.to_string(), ..Default::default() }
    }

    fn setup_controller() -> YouTubeSearchController {
        let mut controller = YouTubeSearchController::new();
        // Generation 1 is the initial state
        controller.start_new_search("initial".to_string());
        controller.add_search_result(create_song("Rust Programming Tutorial"), 1);
        controller.add_search_result(create_song("The RUSTY Robot"), 1);
        controller.add_search_result(create_song("Another Video"), 1);
        controller.complete_search(1);
        controller
    }

    #[test]
    fn test_search_mode_cycle() {
        let mode = SearchMode::Fuzzy;
        assert_eq!(mode.next(), SearchMode::Contains);
        let mode = SearchMode::Regex;
        assert_eq!(mode.next(), SearchMode::Fuzzy);
        let mode = SearchMode::Fuzzy;
        assert_eq!(mode.previous(), SearchMode::Regex);
    }

    #[test]
    fn test_filter_contains() {
        let mut controller = setup_controller();
        controller.set_search_mode(SearchMode::Contains);
        controller.filter_results("rust");
        assert_eq!(controller.filtered_results().len(), 2);
    }

    #[test]
    fn test_filter_starts_with() {
        let mut controller = setup_controller();
        controller.set_search_mode(SearchMode::StartsWith);
        controller.filter_results("Rust"); // case-insensitive
        assert_eq!(controller.filtered_results().len(), 1);
        assert_eq!(controller.filtered_results()[0].song.title, "Rust Programming Tutorial");
    }

    #[test]
    fn test_filter_exact() {
        let mut controller = setup_controller();
        controller.set_search_mode(SearchMode::Exact);
        controller.filter_results("The RUSTY Robot"); // case-insensitive
        assert_eq!(controller.filtered_results().len(), 1);
        controller.filter_results("The RUSTY");
        assert_eq!(controller.filtered_results().len(), 0);
    }

    #[test]
    fn test_filter_regex_valid() {
        let mut controller = setup_controller();
        controller.set_search_mode(SearchMode::Regex);
        controller.filter_results(r"(?i)rust.*"); // case-insensitive regex
        assert_eq!(controller.filtered_results().len(), 2);
        assert!(controller.regex_error.is_none());
    }

    #[test]
    fn test_filter_regex_invalid() {
        let mut controller = setup_controller();
        controller.set_search_mode(SearchMode::Regex);
        controller.filter_results(r"[invalid");
        assert_eq!(controller.filtered_results().len(), 0);
        assert!(controller.regex_error.is_some());
    }
    
    #[test]
    fn test_old_generation_results_are_ignored() {
        let mut controller = YouTubeSearchController::new();
        let gen1 = controller.start_new_search("query1".to_string());
        let _gen2 = controller.start_new_search("query2".to_string());
        
        // Add a result for the old generation, it should be ignored.
        controller.add_search_result(create_song("A Song"), gen1);
        assert!(controller.raw_results.is_empty());
        assert!(controller.filtered_results.is_empty());
    }
}
