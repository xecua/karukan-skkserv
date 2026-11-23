//! Candidate generation: karukan's neural model first, then its dictionary.

use crate::protocol::Lookup;
use karukan_engine::kana::hiragana_to_katakana;
use karukan_engine::{Dictionary, KanaKanjiConverter};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, OnceLock, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

const CACHE_ENTRIES: usize = 4096;
const MAX_COMPLETIONS: usize = 32;

/// A lookup key split into what gets converted and the okurigana to strip.
#[derive(Debug, PartialEq, Eq)]
pub struct Query {
    pub kana: String,
    pub okuri: Option<&'static str>,
}

fn is_reading_char(c: char) -> bool {
    matches!(c, '\u{3041}'..='\u{3096}' | 'ー')
}

/// Representative kana for an SKK okuri consonant (`かk` is converted as
/// `かく`, and the trailing `く` is removed from each result).
fn okuri_kana(letter: char) -> Option<&'static str> {
    Some(match letter {
        'a' => "あ",
        'b' => "ぶ",
        'c' => "ち",
        'd' => "で",
        'e' => "え",
        'f' | 'h' => "ふ",
        'g' => "ぐ",
        'i' => "い",
        'j' => "じ",
        'k' => "く",
        'm' => "む",
        'n' => "ぬ",
        'o' => "お",
        'p' => "ぷ",
        'r' => "る",
        's' => "す",
        't' => "つ",
        'u' | 'w' => "う",
        'y' => "ゆ",
        'z' => "ず",
        _ => return None,
    })
}

/// Parse a key. Abbrev, numeric (`#`), and affix (`>`) entries return `None`
/// so they are answered as misses and never reach the model.
pub fn parse_query(key: &str) -> Option<Query> {
    let mut chars = key.chars();
    let last = chars.next_back()?;
    let (stem, okuri) = if last.is_ascii_lowercase() {
        (chars.as_str(), Some(okuri_kana(last)?))
    } else {
        (key, None)
    };
    if stem.is_empty() || !stem.chars().all(is_reading_char) {
        return None;
    }
    Some(Query {
        kana: format!("{stem}{}", okuri.unwrap_or("")),
        okuri,
    })
}

/// Merge candidates in order, stripping okurigana and removing duplicates.
pub fn merge_candidates<'a>(
    query: &Query,
    sources: impl IntoIterator<Item = &'a str>,
    limit: usize,
) -> Vec<String> {
    let reading = query
        .okuri
        .and_then(|okuri| query.kana.strip_suffix(okuri))
        .unwrap_or(&query.kana);
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for surface in sources {
        let surface = match query.okuri {
            Some(okuri) => match surface.strip_suffix(okuri) {
                Some(stem) => stem,
                None => continue,
            },
            None => surface,
        };
        if surface.is_empty() || surface == reading || !seen.insert(surface.to_string()) {
            continue;
        }
        merged.push(surface.to_string());
        if merged.len() == limit {
            break;
        }
    }
    merged
}

#[derive(Default)]
struct Cache {
    entries: HashMap<String, Vec<String>>,
    order: VecDeque<String>,
}

impl Cache {
    fn insert(&mut self, key: String, value: Vec<String>) {
        if self.entries.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
            if self.order.len() > CACHE_ENTRIES
                && let Some(oldest) = self.order.pop_front()
            {
                self.entries.remove(&oldest);
            }
        }
    }
}

pub struct Backend {
    dictionary: Option<Dictionary>,
    model: OnceLock<Mutex<KanaKanjiConverter>>,
    cache: Mutex<Cache>,
    model_candidates: usize,
    max_candidates: usize,
    timeout: Duration,
}

impl Backend {
    pub fn new(
        dictionary: Option<Dictionary>,
        model_candidates: usize,
        max_candidates: usize,
        timeout: Duration,
    ) -> Self {
        Self {
            dictionary,
            model: OnceLock::new(),
            cache: Mutex::default(),
            model_candidates,
            max_candidates,
            timeout,
        }
    }

    pub fn install_model(&self, model: KanaKanjiConverter) {
        let _ = self.model.set(Mutex::new(model));
    }

    /// Run the model. `None` means no complete answer (not loaded, busy, or
    /// timed out), in which case the result must not be cached.
    fn convert(&self, kana: &str) -> Option<Vec<String>> {
        let model = self.model.get()?;
        let deadline = Instant::now() + self.timeout;
        let guard = loop {
            match model.try_lock() {
                Ok(guard) => break guard,
                Err(TryLockError::Poisoned(e)) => break e.into_inner(),
                Err(TryLockError::WouldBlock) if Instant::now() >= deadline => return None,
                Err(TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(2)),
            }
        };
        match guard.convert_within(kana, "", self.model_candidates, Some(deadline)) {
            Ok(candidates) => Some(candidates),
            Err(e) => {
                tracing::warn!("model conversion failed for {kana}: {e}");
                None
            }
        }
    }
}

impl Lookup for Backend {
    fn lookup(&self, reading: &str) -> Vec<String> {
        let Some(query) = parse_query(reading) else {
            return Vec::new();
        };
        let lock_cache = || self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = lock_cache().entries.get(reading) {
            return hit.clone();
        }

        let model = self.convert(&query.kana);
        let dictionary = self
            .dictionary
            .as_ref()
            .and_then(|dict| dict.exact_match_search(&query.kana));
        let katakana = query.okuri.is_none().then(|| hiragana_to_katakana(&query.kana));

        let sources = model
            .iter()
            .flatten()
            .map(String::as_str)
            .chain(dictionary.iter().flat_map(|hit| hit.candidates.iter().map(|c| c.surface.as_str())))
            .chain(katakana.as_deref());
        let merged = merge_candidates(&query, sources, self.max_candidates);

        if model.is_some() {
            lock_cache().insert(reading.to_string(), merged.clone());
        }
        merged
    }

    fn complete(&self, prefix: &str) -> Vec<String> {
        let Some(dictionary) = &self.dictionary else {
            return Vec::new();
        };
        if prefix.is_empty() || !prefix.chars().all(is_reading_char) {
            return Vec::new();
        }
        let mut seen = HashSet::new();
        dictionary
            .predictive_search(prefix, MAX_COMPLETIONS * 8)
            .into_iter()
            .filter(|hit| hit.reading != prefix && seen.insert(hit.reading.to_string()))
            .map(|hit| hit.reading.to_string())
            .take(MAX_COMPLETIONS)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_okuri_nasi_and_ari() {
        assert_eq!(
            parse_query("かんじ"),
            Some(Query { kana: "かんじ".into(), okuri: None })
        );
        assert_eq!(
            parse_query("かk"),
            Some(Query { kana: "かく".into(), okuri: Some("く") })
        );
    }

    #[test]
    fn non_kana_keys_never_reach_the_model() {
        for key in ["github", "#1", "だい#", ">てき", "かき>", "カンジ", "漢字", "k", "", "かq"] {
            assert_eq!(parse_query(key), None, "{key}");
        }
    }

    #[test]
    fn okuri_candidates_are_stems() {
        let query = parse_query("かk").unwrap();
        let merged = merge_candidates(&query, ["書く", "描く", "書く", "かく", "核"], 10);
        assert_eq!(merged, ["書", "描"]);
    }

    #[test]
    fn merge_respects_limit_and_drops_reading() {
        let query = parse_query("かんじ").unwrap();
        let merged = merge_candidates(&query, ["漢字", "かんじ", "感じ", "幹事"], 2);
        assert_eq!(merged, ["漢字", "感じ"]);
    }

    #[test]
    fn cache_is_bounded() {
        let mut cache = Cache::default();
        for i in 0..CACHE_ENTRIES + 10 {
            cache.insert(i.to_string(), vec![]);
        }
        assert_eq!(cache.entries.len(), CACHE_ENTRIES);
        assert!(!cache.entries.contains_key("0"));
    }
}
