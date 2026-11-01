//! Finding a sandbox in the list by typing part of it: the TUI's `/`, as `less` and `vim` search.
//! Nothing is filtered out. This module says what matches; the state and keys are `App::find`'s.

use std::ops::Range;

use crate::kits::Catalog;
use crate::model::Sandbox;

/// A search in progress.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Find {
    /// What has been typed so far.
    pub query: String,
    /// The sandbox the cursor was on when the search began — `None` on the New row — where Esc
    /// puts it back.
    pub origin: Option<String>,
    /// Whether the other folders' section was unfolded then.
    pub showed_other: bool,
}

/// A character as it is compared: in lower case, so `API` finds `api`.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Where `query` occurs in `text`, ignoring case: the range of characters (not bytes) of each
/// occurrence, left to right, none overlapping the one before. Nothing for an empty query.
pub fn spans(text: &str, query: &str) -> Vec<Range<usize>> {
    let hay: Vec<char> = text.chars().map(fold).collect();
    let needle: Vec<char> = query.chars().map(fold).collect();
    let mut out = Vec::new();
    if needle.is_empty() || needle.len() > hay.len() {
        return out;
    }
    let mut at = 0;
    while at + needle.len() <= hay.len() {
        if hay[at..at + needle.len()] == needle[..] {
            out.push(at..at + needle.len());
            at += needle.len();
        } else {
            at += 1;
        }
    }
    out
}

/// The texts a sandbox is found by, in the order the list shows them: its label, its sbx name,
/// what it runs, its kits' ids, and its folder (none for a mountless sandbox).
pub fn texts(sb: &Sandbox, catalog: &Catalog) -> Vec<String> {
    let mut out = vec![sb.label.clone(), sb.name.clone(), sb.type_label(catalog)];
    out.extend(sb.kit_ids().into_iter().map(|(_, id)| id));
    if !sb.mountless() {
        out.push(sb.workspace.display().to_string());
    }
    out
}

/// Whether `sb` is a match for `query`. An empty query matches nothing: the search has not
/// started yet.
pub fn matches(sb: &Sandbox, catalog: &Catalog, query: &str) -> bool {
    !query.is_empty()
        && texts(sb, catalog)
            .iter()
            .any(|text| !spans(text, query).is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model;

    #[test]
    fn spans_ignore_case_count_characters_and_do_not_overlap() {
        assert_eq!(spans("My API api", "api"), [3..6, 7..10]);
        assert_eq!(spans("aaaa", "aa"), [0..2, 2..4]);
        assert_eq!(spans("Größe api", "API"), vec![6..9]);
        assert!(spans("api", "").is_empty());
        assert!(spans("ap", "api").is_empty());
        assert!(spans("docs", "api").is_empty());
    }

    #[test]
    fn a_sandbox_is_found_by_what_its_row_shows() {
        let catalog = Catalog::load().unwrap();
        let mut sb = model::sample("claude-litellm-api", Some("claude-litellm"), &["jdk25"]);
        sb.label = "Payments service".into();
        sb.workspace = "/work/billing".into();
        sb.litellm = Some("acme".into());
        for query in ["payments", "LITELLM-API", "acme", "jdk", "billing"] {
            assert!(matches(&sb, &catalog, query), "{query}");
        }
        for query in ["", "rtk", "opencode"] {
            assert!(!matches(&sb, &catalog, query), "{query}");
        }
    }

    #[test]
    fn a_mountless_sandbox_has_no_folder_text() {
        let catalog = Catalog::load().unwrap();
        let mut sb = model::sample("ssh-claude", None, &[]);
        sb.workspace = std::path::PathBuf::new();
        assert!(sb.mountless());
        assert!(!texts(&sb, &catalog).iter().any(String::is_empty));
    }
}
