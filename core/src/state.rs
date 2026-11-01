//! The registry (`registry.json`): per-sandbox facts that sbx bakes in and reports nowhere, and the
//! dashboard's preferences. Also the state folder and the clock helpers.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, UtcOffset};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entry {
    /// User-visible label. `None` → fall back to the sbx sandbox name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Created with `--clone`. Needed to reproduce the sandbox on a recreate.
    #[serde(default, skip_serializing_if = "is_false")]
    pub clone: bool,
    /// How the sandbox mounts the host's shared agent-skills store (`--skills`), which sbx reports
    /// nowhere. `None`: not recorded (see [`Registry::skills`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<crate::sbx::SkillsMode>,
    /// The names of the MCP servers the sandbox was created with (`--static-mcp`), which sbx
    /// reports nowhere, so a rebuild can repeat them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub static_mcp: Vec<String>,
    /// Combined hash of the kits present at creation, for drift detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kit_hash: Option<String>,
    /// The folder inside the workspace that the agent opens in, relative to the workspace on
    /// purpose (it survives a moved workspace). `None` or empty is the workspace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_dir: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Document {
    #[serde(default)]
    sandboxes: BTreeMap<String, Entry>,
    #[serde(default)]
    ui: Ui,
}

/// Dashboard preferences that outlive a run. Only the ones a user sets by hand belong here —
/// anything derivable from sbx is re-read on every refresh instead.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Ui {
    /// The "other folders" section is collapsed. Only an explicit collapse is remembered.
    #[serde(default, skip_serializing_if = "is_false")]
    hide_other_folders: bool,
    /// The named setup token most recently used to create or re-point a sandbox: the create form
    /// offers it first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_token: Option<String>,
    /// The LiteLLM connection of the last job with one that succeeded: the create form opens on it.
    /// A UI preference only: headless never chooses a gateway from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_litellm: Option<String>,
    /// The answers of the last create that succeeded: the next create form opens on them. Read
    /// leniently, so a value this build cannot read costs only the defaults, never the registry.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient"
    )]
    create: Option<CreateDefaults>,
}

/// The create form's answers that the next form starts from: everything but what names this one
/// sandbox (its label and start folder) and the secrets. The connection and the token are
/// `last_litellm` and `last_token`; the models are the connection's picks
/// ([`crate::model_picks`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateDefaults {
    /// The backend kit picked: the subscription kit for a setup token too.
    #[serde(default)]
    pub backend: String,
    /// On the subscription kit, the setup token radio rather than the /login.
    #[serde(default, skip_serializing_if = "is_false")]
    pub token: bool,
    /// The addons ticked, per backend kit.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub addons: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub clone: bool,
    /// The workspace's `.git` folder mounted read-only ([`crate::folders::git_guard`]).
    #[serde(default, skip_serializing_if = "is_false")]
    pub git_guard: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<crate::sbx::SkillsMode>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ports: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub deny_network: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp: Vec<String>,
}

/// `Some` of what parses as [`CreateDefaults`], `None` of anything else.
fn lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<CreateDefaults>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

#[derive(Debug)]
pub struct Registry {
    path: PathBuf,
    doc: Document,
    dirty: bool,
}

impl Registry {
    pub fn load() -> Self {
        let path = state_dir().join("registry.json");
        let doc: Document = fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self {
            path,
            doc,
            dirty: false,
        }
    }

    /// An empty registry, read from nowhere, for tests: no test depends on the host's registry,
    /// and its path is not the user's file.
    #[cfg(any(test, feature = "test-support"))]
    pub fn empty() -> Self {
        Self {
            path: std::env::temp_dir().join("aisbx-test-registry.json"),
            doc: Document::default(),
            dirty: false,
        }
    }

    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.doc.sandboxes.get(name)
    }

    pub fn hide_other_folders(&self) -> bool {
        self.doc.ui.hide_other_folders
    }

    pub fn set_hide_other_folders(&mut self, hide: bool) {
        if self.doc.ui.hide_other_folders != hide {
            self.doc.ui.hide_other_folders = hide;
            self.dirty = true;
        }
    }

    pub fn display_name(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(|e| e.display_name.as_deref())
    }

    pub fn is_clone(&self, name: &str) -> bool {
        self.get(name).is_some_and(|e| e.clone)
    }

    pub fn set_display_name(&mut self, name: &str, label: Option<String>) {
        let entry = self.doc.sandboxes.entry(name.to_string()).or_default();
        let label = label.filter(|l| !l.trim().is_empty());
        if entry.display_name != label {
            entry.display_name = label;
            self.dirty = true;
        }
    }

    /// Where this sandbox's agent opens, relative to its workspace; `None` is the root. A value
    /// outside the workspace is ignored: the file is editable, and the value becomes `sbx exec -w`.
    pub fn start_dir(&self, name: &str) -> Option<PathBuf> {
        let rel = PathBuf::from(self.get(name)?.start_dir.as_deref()?);
        crate::startdir::is_inside(&rel).then_some(rel)
    }

    pub fn set_start_dir(&mut self, name: &str, rel: Option<&Path>) {
        let entry = self.doc.sandboxes.entry(name.to_string()).or_default();
        // The root is no start folder: it is stored as nothing, never as an empty string.
        let rel = rel
            .filter(|r| crate::startdir::is_inside(r))
            .map(|r| r.display().to_string());
        if entry.start_dir != rel {
            entry.start_dir = rel;
            self.dirty = true;
        }
    }

    pub fn set_clone(&mut self, name: &str, clone: bool) {
        let entry = self.doc.sandboxes.entry(name.to_string()).or_default();
        if entry.clone != clone {
            entry.clone = clone;
            self.dirty = true;
        }
    }

    /// How this sandbox mounts the host's shared agent-skills store, if that was ever recorded.
    /// `None` means unknown, not the default: callers show it as unrecorded and pass no `--skills`.
    pub fn skills(&self, name: &str) -> Option<crate::sbx::SkillsMode> {
        self.get(name)?.skills
    }

    /// The MCP servers a sandbox was created with, for a rebuild to repeat.
    pub fn static_mcp(&self, name: &str) -> Vec<String> {
        self.get(name)
            .map(|e| e.static_mcp.clone())
            .unwrap_or_default()
    }

    pub fn set_static_mcp(&mut self, name: &str, servers: Vec<String>) {
        let entry = self.doc.sandboxes.entry(name.to_string()).or_default();
        if entry.static_mcp != servers {
            entry.static_mcp = servers;
            self.dirty = true;
        }
    }

    pub fn set_skills(&mut self, name: &str, mode: Option<crate::sbx::SkillsMode>) {
        let entry = self.doc.sandboxes.entry(name.to_string()).or_default();
        if entry.skills != mode {
            entry.skills = mode;
            self.dirty = true;
        }
    }

    /// The token last used to create a sandbox, so the create form can offer it again. Cleared
    /// implicitly by disappearing from `sbx secret ls` — the caller checks it still exists.
    pub fn last_token(&self) -> Option<&str> {
        self.doc.ui.last_token.as_deref()
    }

    pub fn set_last_token(&mut self, name: &str) {
        if self.doc.ui.last_token.as_deref() != Some(name) {
            self.doc.ui.last_token = Some(name.to_string());
            self.dirty = true;
        }
    }

    /// The LiteLLM connection last used successfully. The caller checks that its
    /// `connection.json` still exists ([`crate::litellm::preselect`]).
    /// The answers the next create form opens on, if a create succeeded before.
    pub fn create_defaults(&self) -> Option<&CreateDefaults> {
        self.doc.ui.create.as_ref()
    }

    /// Keep the answers of a create that succeeded. The addons of the other backends stay.
    pub fn set_create_defaults(&mut self, mut defaults: CreateDefaults) {
        if let Some(old) = &self.doc.ui.create {
            for (backend, addons) in &old.addons {
                defaults
                    .addons
                    .entry(backend.clone())
                    .or_insert_with(|| addons.clone());
            }
        }
        if self.doc.ui.create.as_ref() != Some(&defaults) {
            self.doc.ui.create = Some(defaults);
            self.dirty = true;
        }
    }

    pub fn last_litellm(&self) -> Option<&str> {
        self.doc.ui.last_litellm.as_deref()
    }

    /// Called only once a job using the connection succeeded.
    pub fn set_last_litellm(&mut self, name: &str) {
        if self.doc.ui.last_litellm.as_deref() != Some(name) {
            self.doc.ui.last_litellm = Some(name.to_string());
            self.dirty = true;
        }
    }

    /// Forget the remembered connection — when it is the one being deleted.
    pub fn clear_last_litellm(&mut self, name: &str) {
        if self.doc.ui.last_litellm.as_deref() == Some(name) {
            self.doc.ui.last_litellm = None;
            self.dirty = true;
        }
    }

    pub fn kit_hash(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(|e| e.kit_hash.as_deref())
    }

    pub fn set_kit_hash(&mut self, name: &str, hash: String) {
        let entry = self.doc.sandboxes.entry(name.to_string()).or_default();
        if entry.kit_hash.as_deref() != Some(hash.as_str()) {
            entry.kit_hash = Some(hash);
            self.dirty = true;
        }
    }

    /// Forget the baseline so the next refresh adopts whatever the sandbox now carries. Used
    /// after a kit add or a recreate, where the sandbox was deliberately rebuilt.
    pub fn clear_kit_hash(&mut self, name: &str) {
        if let Some(entry) = self.doc.sandboxes.get_mut(name) {
            if entry.kit_hash.take().is_some() {
                self.dirty = true;
            }
        }
    }

    pub fn forget(&mut self, name: &str) {
        if self.doc.sandboxes.remove(name).is_some() {
            self.dirty = true;
        }
    }

    /// Drop entries for sandboxes sbx no longer knows about, so the file does not grow forever.
    pub fn retain_known(&mut self, live: &[String]) {
        let before = self.doc.sandboxes.len();
        self.doc
            .sandboxes
            .retain(|k, _| live.iter().any(|l| l == k));
        if self.doc.sandboxes.len() != before {
            self.dirty = true;
        }
    }

    /// Write only when something actually changed; write-then-rename so a crash mid-write
    /// cannot truncate the existing file.
    pub fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        // Never under `cargo test`: a test's `Registry::load` holds the real registry of whoever
        // runs the suite. Tests assert on the in-memory document.
        if cfg!(any(test, feature = "test-support")) {
            self.dirty = false;
            return Ok(());
        }
        let dir = self
            .path
            .parent()
            .context("registry path has no parent directory")?;
        fs::create_dir_all(dir)?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&self.doc)?)?;
        fs::rename(&tmp, &self.path)?;
        self.dirty = false;
        Ok(())
    }
}

// --- clock --------------------------------------------------------------------------------

static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();

/// Capture this machine's UTC offset. Call it once from `main`, before any thread starts: the
/// `time` crate reads the local offset only in a single-threaded process. If that fails,
/// timestamps fall back to UTC and say so.
pub fn init_clock() {
    let _ = LOCAL_OFFSET.set(UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC));
}

fn offset() -> UtcOffset {
    LOCAL_OFFSET.get().copied().unwrap_or(UtcOffset::UTC)
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Unix seconds → `2026-07-24 10:39`, in local time (UTC, marked as such, when the offset could
/// not be determined at start-up).
pub fn format_local(epoch: u64) -> String {
    let Ok(utc) = OffsetDateTime::from_unix_timestamp(epoch as i64) else {
        return "—".into();
    };
    let offset = offset();
    let t = utc.to_offset(offset);
    let suffix = if offset.is_utc() && !LOCAL_OFFSET.get().is_some_and(|o| o.is_utc()) {
        " UTC"
    } else {
        ""
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}{suffix}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
    )
}

/// An RFC 3339 UTC instant as sbx writes it (whole or fractional seconds) -> Unix seconds; `None`
/// for anything else. Hand-rolled: `time`'s parser needs a feature this crate does not take.
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let s = s.trim();
    let (date, rest) = s.split_once('T').or_else(|| s.split_once(' '))?;
    let time = rest.trim_end_matches('Z');
    // Only UTC: sbx emits nothing else, and an ignored offset would skew the prune decision.
    if rest.len() == time.len() {
        return None;
    }
    let time = time.split_once('.').map_or(time, |(t, _)| t);
    let mut d = date.split('-');
    let (y, mo, dy): (i32, u8, u8) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let mut t = time.split(':');
    let (h, mi, sec): (u8, u8, u8) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next().unwrap_or("0").parse().ok()?,
    );
    let date = time::Date::from_calendar_date(y, time::Month::try_from(mo).ok()?, dy).ok()?;
    let time = time::Time::from_hms(h, mi, sec).ok()?;
    u64::try_from(date.with_time(time).assume_utc().unix_timestamp()).ok()
}

/// Unix seconds → "3 days ago", for the coarse sense of age a dashboard actually needs.
pub fn ago(epoch: u64) -> String {
    let secs = now_epoch().saturating_sub(epoch);
    let plural = |n: u64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match secs {
        0..=90 => "just now".into(),
        s if s < 3_600 => plural(s / 60, "minute"),
        s if s < 86_400 => plural(s / 3_600, "hour"),
        s if s < 2_592_000 => plural(s / 86_400, "day"),
        s if s < 31_536_000 => plural(s / 2_592_000, "month"),
        s => plural(s / 31_536_000, "year"),
    }
}

/// `$XDG_STATE_HOME/aisbx` when set, else the platform default.
pub fn state_dir() -> PathBuf {
    state_dir_named("aisbx")
}

fn state_dir_named(name: &str) -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join(name);
    }
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
            return PathBuf::from(local).join(name).join("state");
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".local").join("state").join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_from_sbx_parses_and_anything_else_does_not() {
        // The shape of `sbx ls --json`.
        assert_eq!(parse_rfc3339("2026-08-07T13:54:29Z"), Some(1_786_110_869));
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        // Fractional seconds, which sbx uses elsewhere in the same payloads.
        assert_eq!(
            parse_rfc3339("2026-08-07T13:54:29.123456Z"),
            Some(1_786_110_869)
        );
        // Not UTC, malformed, or empty: `None`, never a number off by an offset.
        for bad in [
            "2026-08-07T13:54:29+02:00",
            "2026-08-07 13:54:29",
            "2026-08-07",
            "yesterday",
            "",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn an_age_reads_in_its_largest_unit() {
        let days = |n: u64| now_epoch().saturating_sub(n * 86_400 + 600);
        assert_eq!(ago(now_epoch()), "just now");
        assert_eq!(ago(days(1)), "1 day ago");
        assert_eq!(ago(days(45)), "1 month ago");
        assert_eq!(ago(days(200)), "6 months ago");
        assert_eq!(ago(days(400)), "1 year ago");
        assert_eq!(ago(days(3 * 365 + 20)), "3 years ago");
    }

    #[test]
    fn a_registry_with_keys_this_build_does_not_know_still_loads() {
        let text = r#"{"sandboxes": {
            "claude-anthropic-ki": {"display_name": "KI", "kit_hash": "abc", "skills": "readonly",
                                    "start_dir": "notes", "added_later": 1},
            "devserver": {"display_name": "Dev server", "clone": true, "other": true}},
          "ui": {"last_token": "private", "new_pref": "x"}}"#;
        let doc: Document = serde_json::from_str(text).expect("the registry parses");
        let ki = &doc.sandboxes["claude-anthropic-ki"];
        assert_eq!(ki.display_name.as_deref(), Some("KI"));
        assert_eq!(ki.skills, Some(crate::sbx::SkillsMode::Readonly));
        assert_eq!(doc.ui.last_token.as_deref(), Some("private"));
        let dev = &doc.sandboxes["devserver"];
        assert_eq!(dev.display_name.as_deref(), Some("Dev server"));
        assert!(dev.clone, "the keys beside an unknown one survive");
        let json = serde_json::to_string(&doc).unwrap();
        assert!(
            !json.contains("added_later") && !json.contains("new_pref"),
            "{json}"
        );
    }

    #[test]
    fn create_defaults_round_trip_and_a_bad_value_costs_only_them() {
        let doc: Document = serde_json::from_str(
            r#"{"sandboxes": {"a": {"display_name": "A"}}, "ui": {"create": {"backend": 3}}}"#,
        )
        .expect("a bad create entry still parses the registry");
        assert_eq!(doc.ui.create, None);
        assert_eq!(doc.sandboxes["a"].display_name.as_deref(), Some("A"));
        let mut reg = Registry::empty();
        let mut first = CreateDefaults {
            backend: "claude-litellm".into(),
            ..Default::default()
        };
        first
            .addons
            .insert("claude-litellm".into(), vec!["rtk".into()]);
        reg.set_create_defaults(first);
        let mut second = CreateDefaults {
            backend: "opencode-litellm".into(),
            clone: true,
            ..Default::default()
        };
        second
            .addons
            .insert("opencode-litellm".into(), vec!["web".into()]);
        reg.set_create_defaults(second);
        let kept = reg.create_defaults().unwrap();
        assert_eq!(kept.backend, "opencode-litellm");
        assert!(kept.clone);
        assert_eq!(
            kept.addons["claude-litellm"],
            ["rtk"],
            "another backend's addons stay"
        );
        assert_eq!(kept.addons["opencode-litellm"], ["web"]);
        let json = serde_json::to_string(&reg.doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ui.create.as_ref(), Some(kept));
    }

    #[test]
    fn the_last_litellm_connection_round_trips_and_clears() {
        let doc: Document =
            serde_json::from_str(r#"{"ui": {"last_litellm": "acme"}}"#).expect("parses");
        let mut reg = Registry {
            path: PathBuf::from("/nonexistent/registry.json"),
            doc,
            dirty: false,
        };
        assert_eq!(reg.last_litellm(), Some("acme"));
        reg.clear_last_litellm("other");
        assert_eq!(reg.last_litellm(), Some("acme"));
        assert!(!reg.dirty);
        reg.set_last_litellm("beta");
        assert_eq!(reg.last_litellm(), Some("beta"));
        reg.clear_last_litellm("beta");
        assert_eq!(reg.last_litellm(), None);
        let json = serde_json::to_string(&reg.doc).unwrap();
        assert!(!json.contains("last_litellm"), "{json}");
        let old: Document = serde_json::from_str(r#"{"ui": {"last_token": "t"}}"#).unwrap();
        assert_eq!(old.ui.last_litellm, None);
    }
}
