//! Display-only metadata reported by hooks and plugins: presentation fields
//! (title, display_agent, state labels) and token maps, with normalization,
//! per-source sequencing, TTLs and hard limits — per the socket API contract.
//! Semantic lifecycle state is NOT here (that is the agents engine).

use serde_json::Value;
use starcil_protocol::error::ApiError;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub const MAX_TOKEN_KEYS_PER_REPORT: usize = 16;
pub const MAX_RETAINED_TOKEN_KEYS: usize = 32;
pub const MAX_SEQ_SOURCES: usize = 32;
pub const MAX_TEXT_LEN: usize = 80;
pub const MIN_TTL_MS: u64 = 1;
pub const MAX_TTL_MS: u64 = 86_400_000;

#[derive(Debug, Clone, Default)]
pub struct MetadataStore {
    pub title: Option<Sourced<String>>,
    pub display_agent: Option<Sourced<String>>,
    pub state_labels: BTreeMap<String, Sourced<String>>,
    pub tokens: BTreeMap<String, TokenEntry>,
    /// Last accepted sequence per source (never released).
    seqs: BTreeMap<String, u64>,
}

#[derive(Debug, Clone)]
pub struct Sourced<T> {
    pub value: T,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct TokenEntry {
    pub value: String,
    pub source: String,
    pub deadline: Option<Instant>,
}

pub fn validate_source(source: &str) -> Result<(), ApiError> {
    if source.is_empty() || source.len() > MAX_TEXT_LEN {
        return Err(ApiError::invalid_params("source must be 1-80 characters"));
    }
    if !source
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'_' | b'-'))
    {
        return Err(ApiError::invalid_params(
            "source may contain only ASCII letters, digits, colon, dot, underscore, and hyphen",
        ));
    }
    Ok(())
}

pub fn valid_token_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Trim, drop control chars, collapse nothing (titles keep inner spacing),
/// cap at 80 chars. Empty result → None.
pub fn normalize_text(raw: &str) -> Option<String> {
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(MAX_TEXT_LEN).collect())
}

/// Sanitize notification text: collapse newlines/tabs/repeats into single
/// spaces, trim, cap at `cap` characters.
pub fn sanitize_notification(raw: &str, cap: usize) -> String {
    let mut out = String::new();
    let mut last_space = true;
    for c in raw.chars() {
        let c = if c.is_whitespace() { ' ' } else if c.is_control() { continue } else { c };
        if c == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        out.push(c);
    }
    let trimmed = out.trim().to_string();
    trimmed.chars().take(cap).collect()
}

pub struct MetadataReport<'a> {
    pub source: &'a str,
    pub seq: Option<u64>,
    pub ttl_ms: Option<u64>,
    pub title: Option<Option<&'a str>>,
    pub display_agent: Option<Option<&'a str>>,
    pub state_labels: Option<&'a serde_json::Map<String, Value>>,
    pub clear_state_labels: bool,
    pub tokens: Option<&'a serde_json::Map<String, Value>>,
}

impl MetadataStore {
    /// Apply a report. Returns Ok(applied). A stale seq is accepted by the
    /// API but ignored by state (returns Ok(false)).
    pub fn apply(&mut self, r: MetadataReport<'_>, now: Instant) -> Result<bool, ApiError> {
        validate_source(r.source)?;
        if let Some(ttl) = r.ttl_ms {
            if !(MIN_TTL_MS..=MAX_TTL_MS).contains(&ttl) {
                return Err(ApiError::invalid_params(format!(
                    "ttl_ms must be between {MIN_TTL_MS} and {MAX_TTL_MS}"
                )));
            }
        }
        if let Some(tokens) = r.tokens {
            if tokens.len() > MAX_TOKEN_KEYS_PER_REPORT {
                return Err(ApiError::invalid_params(format!(
                    "a report may mention at most {MAX_TOKEN_KEYS_PER_REPORT} token keys"
                )));
            }
            for (name, v) in tokens {
                if !valid_token_name(name) {
                    return Err(ApiError::invalid_params(format!("invalid token name `{name}`")));
                }
                if !v.is_string() && !v.is_null() {
                    return Err(ApiError::invalid_params(format!("token `{name}` must be a string or null")));
                }
            }
        }
        if let Some(labels) = r.state_labels {
            for k in labels.keys() {
                if !matches!(k.as_str(), "idle" | "working" | "blocked" | "done" | "unknown") {
                    return Err(ApiError::invalid_params(format!("invalid state label key `{k}`")));
                }
            }
        }
        // Sequencing: stale reports are ignored, not errored.
        if let Some(seq) = r.seq {
            if !self.seqs.contains_key(r.source) && self.seqs.len() >= MAX_SEQ_SOURCES {
                return Err(ApiError::invalid_params(format!(
                    "at most {MAX_SEQ_SOURCES} distinct sequenced sources per resource"
                )));
            }
            let last = self.seqs.get(r.source).copied();
            if let Some(last) = last {
                if seq <= last {
                    return Ok(false);
                }
            }
            self.seqs.insert(r.source.to_string(), seq);
        }
        // Presentation fields.
        if let Some(title) = r.title {
            self.title = title
                .and_then(normalize_text)
                .map(|value| Sourced { value, source: r.source.to_string() });
        }
        if let Some(da) = r.display_agent {
            self.display_agent = da
                .and_then(normalize_text)
                .map(|value| Sourced { value, source: r.source.to_string() });
        }
        if r.clear_state_labels {
            self.state_labels.clear();
        }
        if let Some(labels) = r.state_labels {
            for (k, v) in labels {
                match v.as_str().and_then(normalize_text) {
                    Some(value) => {
                        self.state_labels
                            .insert(k.clone(), Sourced { value, source: r.source.to_string() });
                    }
                    None => {
                        self.state_labels.remove(k);
                    }
                }
            }
        }
        // Token patches: string sets, null clears, omitted unchanged.
        if let Some(tokens) = r.tokens {
            let deadline = r.ttl_ms.map(|ms| now + Duration::from_millis(ms));
            for (name, v) in tokens {
                match v.as_str() {
                    Some(s) => match normalize_text(s) {
                        Some(value) => {
                            if !self.tokens.contains_key(name) && self.tokens.len() >= MAX_RETAINED_TOKEN_KEYS {
                                return Err(ApiError::invalid_params(format!(
                                    "a resource retains at most {MAX_RETAINED_TOKEN_KEYS} token keys"
                                )));
                            }
                            self.tokens.insert(
                                name.clone(),
                                TokenEntry { value, source: r.source.to_string(), deadline },
                            );
                        }
                        // Empty normalized value clears the key.
                        None => {
                            self.tokens.remove(name);
                        }
                    },
                    None => {
                        self.tokens.remove(name);
                    }
                }
            }
        }
        Ok(true)
    }

    /// Drop expired tokens; returns true if anything expired.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.tokens.len();
        self.tokens.retain(|_, t| t.deadline.map(|d| d > now).unwrap_or(true));
        before != self.tokens.len()
    }

    pub fn token_values(&self) -> BTreeMap<String, String> {
        self.tokens.iter().map(|(k, v)| (k.clone(), v.value.clone())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> serde_json::Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn base<'a>(source: &'a str, tokens: &'a serde_json::Map<String, Value>) -> MetadataReport<'a> {
        MetadataReport {
            source,
            seq: None,
            ttl_ms: None,
            title: None,
            display_agent: None,
            state_labels: None,
            clear_state_labels: false,
            tokens: Some(tokens),
        }
    }

    #[test]
    fn token_patch_set_clear_keep() {
        let mut s = MetadataStore::default();
        let now = Instant::now();
        let t1 = map(json!({"a": "1", "b": "2"}));
        s.apply(base("user:x", &t1), now).unwrap();
        assert_eq!(s.token_values().len(), 2);
        let t2 = map(json!({"a": null}));
        s.apply(base("user:x", &t2), now).unwrap();
        assert_eq!(s.token_values().get("b").unwrap(), "2");
        assert!(!s.token_values().contains_key("a"));
    }

    #[test]
    fn ttl_expires_only_updated_keys() {
        let mut s = MetadataStore::default();
        let now = Instant::now();
        let t1 = map(json!({"stay": "x"}));
        s.apply(base("u:1", &t1), now).unwrap();
        let t2 = map(json!({"gone": "y"}));
        let mut r = base("u:1", &t2);
        r.ttl_ms = Some(1000);
        s.apply(r, now).unwrap();
        assert!(s.expire(now + Duration::from_millis(1500)));
        let vals = s.token_values();
        assert!(vals.contains_key("stay"));
        assert!(!vals.contains_key("gone"));
    }

    #[test]
    fn stale_seq_ignored_not_error() {
        let mut s = MetadataStore::default();
        let now = Instant::now();
        let t = map(json!({"k": "v1"}));
        let mut r = base("u:1", &t);
        r.seq = Some(5);
        assert!(s.apply(r, now).unwrap());
        let t2 = map(json!({"k": "v2"}));
        let mut r2 = base("u:1", &t2);
        r2.seq = Some(4);
        assert!(!s.apply(r2, now).unwrap(), "stale seq accepted but ignored");
        assert_eq!(s.token_values().get("k").unwrap(), "v1");
    }

    #[test]
    fn limits_enforced() {
        let mut s = MetadataStore::default();
        let now = Instant::now();
        let mut big = serde_json::Map::new();
        for i in 0..17 {
            big.insert(format!("k{i}"), json!("v"));
        }
        assert!(s.apply(base("u:1", &big), now).is_err());
        assert!(validate_source("bad source with spaces").is_err());
        assert!(validate_source(&"s".repeat(81)).is_err());
        assert!(!valid_token_name("with space"));
        assert!(valid_token_name("ok_name-1"));
    }

    #[test]
    fn state_label_keys_validated_and_text_normalized() {
        let mut s = MetadataStore::default();
        let now = Instant::now();
        let empty = map(json!({}));
        let labels = map(json!({"nope": "x"}));
        let mut r = base("u:1", &empty);
        r.state_labels = Some(&labels);
        assert!(s.apply(r, now).is_err());

        let labels = map(json!({"working": "  refactoring auth\u{0007}  "}));
        let mut r = base("u:1", &empty);
        r.state_labels = Some(&labels);
        s.apply(r, now).unwrap();
        assert_eq!(s.state_labels.get("working").unwrap().value, "refactoring auth");
    }

    #[test]
    fn notification_sanitizer() {
        assert_eq!(sanitize_notification("a\n\nb\t\tc   d", 240), "a b c d");
        assert_eq!(sanitize_notification("   \u{0007}\n  ", 80), "");
        let long = "x".repeat(100);
        assert_eq!(sanitize_notification(&long, 80).len(), 80);
    }
}
