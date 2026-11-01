//! Which model Claude Code's `opus` / `sonnet` / `haiku` aliases resolve to on the gateway: the
//! model list harvested from a sandbox, and the generated kit that pins the aliases.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::state::state_dir;

/// Marks a kit directory as a generated model-override kit.
pub const KIT_PREFIX: &str = "models-";

/// A model an agent is pointed at: one of Claude Code's aliases (pinned through an environment
/// variable), OpenCode's main and small model, or Pi's default model (both written into the
/// agent's config).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Opus,
    Sonnet,
    Haiku,
    /// OpenCode's `model`.
    Main,
    /// The model of OpenCode's built-in `title` agent (`agents.title.model`): session titles.
    Small,
    /// Pi's `defaultModel`.
    PiDefault,
}

impl Slot {
    /// Claude Code's aliases, the slots a `models-<sandbox>` kit pins.
    pub const ALL: [Slot; 3] = [Slot::Opus, Slot::Sonnet, Slot::Haiku];
    /// Every slot of every agent, in [`Slot::index`] order.
    pub const EVERY: [Slot; 6] = [
        Slot::Opus,
        Slot::Sonnet,
        Slot::Haiku,
        Slot::Main,
        Slot::Small,
        Slot::PiDefault,
    ];

    /// The slots of one agent, in the order the form offers them.
    pub fn of(agent: crate::litellm::Agent) -> &'static [Slot] {
        match agent {
            crate::litellm::Agent::Claude => &Slot::ALL,
            crate::litellm::Agent::Opencode => &[Slot::Main, Slot::Small],
            crate::litellm::Agent::Pi => &[Slot::PiDefault],
        }
    }

    /// The slot as the agent spells it: the alias of Claude Code's `/model` picker, OpenCode's
    /// and Pi's config key.
    pub fn alias(self) -> &'static str {
        match self {
            Slot::Opus => "opus",
            Slot::Sonnet => "sonnet",
            Slot::Haiku => "haiku",
            Slot::Main => "model",
            Slot::Small => "agents.title.model",
            Slot::PiDefault => "defaultModel",
        }
    }

    /// What the form calls the slot.
    pub fn label(self) -> &'static str {
        match self {
            Slot::Opus => "opus",
            Slot::Sonnet => "sonnet",
            Slot::Haiku => "haiku",
            Slot::Main => "main model",
            Slot::Small => "small model",
            Slot::PiDefault => "default model",
        }
    }

    /// Is this one of Claude Code's aliases, which a kit pins through the environment?
    pub fn is_claude(self) -> bool {
        Slot::ALL.contains(&self)
    }

    /// The environment variable that pins a Claude Code alias; empty for the other slots, which
    /// live in the agent's config.
    pub fn env(self) -> &'static str {
        match self {
            Slot::Opus => "ANTHROPIC_DEFAULT_OPUS_MODEL",
            Slot::Sonnet => "ANTHROPIC_DEFAULT_SONNET_MODEL",
            Slot::Haiku => "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            Slot::Main | Slot::Small | Slot::PiDefault => "",
        }
    }

    /// The companion that names the pinned model in the `/model` picker, instead of its raw id.
    fn name_env(self) -> String {
        format!("{}_NAME", self.env())
    }

    /// What the slot is actually used for — the reason to care which model is behind it.
    pub fn hint(self) -> &'static str {
        match self {
            Slot::Opus => "the opus alias, and opusplan while planning",
            Slot::Sonnet => "the sonnet alias, and opusplan while building",
            Slot::Haiku => "the haiku alias, and background work",
            Slot::Main => "the model a session starts on",
            Slot::Small => "session titles",
            Slot::PiDefault => "the model a session starts on",
        }
    }

    /// What an unset slot is called on every form and in the review.
    pub const DEFAULT: &'static str = "default";

    /// What an unset slot is called: the agent's own choice.
    pub fn unset(self) -> &'static str {
        Self::DEFAULT
    }

    /// The agent that picks the model of an unset slot.
    pub fn agent_name(self) -> &'static str {
        match self {
            Slot::Opus | Slot::Sonnet | Slot::Haiku => "Claude Code",
            Slot::Main | Slot::Small => "OpenCode",
            Slot::PiDefault => "Pi",
        }
    }

    /// The key [`crate::model_picks`] stores the slot under.
    pub fn key(self) -> &'static str {
        match self {
            Slot::Main => "main",
            Slot::Small => "small",
            Slot::PiDefault => "default",
            claude => claude.alias(),
        }
    }

    pub fn from_key(key: &str) -> Option<Slot> {
        Slot::EVERY.into_iter().find(|s| s.key() == key)
    }

    fn index(self) -> usize {
        match self {
            Slot::Opus => 0,
            Slot::Sonnet => 1,
            Slot::Haiku => 2,
            Slot::Main => 3,
            Slot::Small => 4,
            Slot::PiDefault => 5,
        }
    }
}

/// One model the gateway serves, as the fetched list names it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Model {
    /// What goes in the environment variable or the agent's config — the gateway's model id.
    pub id: String,
    /// The gateway's own display name, e.g. "Claude Opus 5".
    pub name: String,
    /// What the gateway's `/model/info` says of it, where it said anything.
    pub limits: Limits,
    /// LiteLLM's `litellm_provider`, e.g. `bedrock_converse`.
    pub provider: Option<String>,
    /// The model the gateway forwards to (LiteLLM's `key`), e.g. `eu.anthropic.claude-opus-5`.
    pub upstream: Option<String>,
}

impl Model {
    /// A Claude model: its id or the model the gateway forwards to names Claude.
    pub fn is_claude(&self) -> bool {
        let claude = |s: &str| s.to_ascii_lowercase().contains("claude");
        claude(&self.id) || self.upstream.as_deref().is_some_and(claude)
    }

    /// Where the gateway sends the model, in words: `Bedrock EU`, `Vertex AI`, `Anthropic`.
    /// `None` for a provider this list does not name.
    pub fn origin(&self) -> Option<String> {
        let provider = self.provider.as_deref()?;
        let region = || {
            let prefix = self.upstream.as_deref()?.split_once('.')?.0;
            ["eu", "us", "apac", "jp", "au", "ca"]
                .contains(&prefix)
                .then(|| prefix.to_ascii_uppercase())
        };
        let name = match provider {
            p if p.starts_with("bedrock") => {
                return Some(match region() {
                    Some(r) => format!("Bedrock {r}"),
                    None => "Bedrock".into(),
                })
            }
            p if p.starts_with("vertex_ai") => "Vertex AI",
            p if p.starts_with("azure") => "Azure",
            "anthropic" => "Anthropic",
            "gemini" => "Google AI Studio",
            _ => return None,
        };
        Some(name.into())
    }
}

/// A price in micro-USD per million tokens, which is pico-USD per token. The model files keep
/// LiteLLM's unit, USD per token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Price(pub u64);

impl Price {
    fn per_token(usd: f64) -> Option<Self> {
        (usd.is_finite() && (0.0..1.0).contains(&usd)).then(|| Price((usd * 1e12).round() as u64))
    }

    /// USD per million tokens.
    pub fn per_mtok(self) -> f64 {
        self.0 as f64 / 1e6
    }
}

impl Serialize for Price {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_f64(self.0 as f64 / 1e12)
    }
}

/// A model's limits and flags from the gateway's `/model/info`: what OpenCode's and Pi's model
/// entries carry. `None` where the gateway gave no usable value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Limits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    /// Takes images.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Supports prompt caching.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caching: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_cost: Option<Price>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_cost: Option<Price>,
}

impl Limits {
    fn read(entry: &serde_json::Value) -> Self {
        let count = |key: &str| entry.get(key).and_then(|v| v.as_u64()).filter(|n| *n > 0);
        let flag = |key: &str| entry.get(key).and_then(|v| v.as_bool());
        let price = |key: &str| {
            entry
                .get(key)
                .and_then(|v| v.as_f64())
                .and_then(Price::per_token)
        };
        Self {
            context: count("context"),
            input: count("input"),
            output: count("output"),
            reasoning: flag("reasoning"),
            tool_call: flag("tool_call"),
            vision: flag("vision"),
            caching: flag("caching"),
            input_cost: price("input_cost"),
            output_cost: price("output_cost"),
        }
    }
}

/// What a sandbox pins each alias to. An unset slot is Claude Code's own default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    slots: [Option<String>; 6],
}

impl Overrides {
    pub fn get(&self, slot: Slot) -> Option<&str> {
        self.slots[slot.index()].as_deref()
    }

    pub fn set(&mut self, slot: Slot, model: Option<String>) {
        self.slots[slot.index()] = model.filter(|m| !m.trim().is_empty());
    }

    /// Nothing pinned — the sandbox needs no kit at all.
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }

    /// The pinned slots, in alias order.
    pub fn entries(&self) -> impl Iterator<Item = (Slot, &str)> {
        Slot::EVERY
            .into_iter()
            .filter_map(|slot| self.get(slot).map(|id| (slot, id)))
    }

    /// One line for the details pane: `opus claude-opus-5 · haiku claude-haiku-4.5`.
    pub fn summary(&self) -> String {
        self.entries()
            .map(|(slot, id)| format!("{} {id}", slot.alias()))
            .collect::<Vec<_>>()
            .join("  ·  ")
    }
}

// --- the gateway's model catalog ---------------------------------------------------------------

/// Sort by display name, case-insensitive, then by id, so the picker lists the models in a stable
/// alphabetical order.
fn sort_models(models: &mut [Model]) {
    models.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
}

// --- the list a sandbox fetched from the gateway ---------------------------------------------

/// The most [`crate::sbx::read_models_cache`] reads, and the most [`parse_sandbox_cache`] accepts.
pub const MAX_HARVEST_BYTES: usize = 1 << 20;
/// The most models a harvested list may hold; a longer one is refused whole.
pub const MAX_HARVEST_MODELS: usize = 500;
/// How far ahead of the host clock a sandbox's `fetched_at` may be (clock skew, not more).
const MAX_FUTURE_SECS: i64 = 300;
/// A harvested display name is cut to this many characters.
const MAX_NAME_CHARS: usize = 80;

/// Where the model list on offer came from — what the pin list's title says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Harvested from a sandbox; `fetched_at` is when that sandbox last reached the gateway.
    Gateway { sandbox: String, fetched_at: i64 },
    /// No sandbox on the connection has been harvested yet: the list is empty.
    NotYet,
}

impl Source {
    /// `gateway, 3 hours ago` / `no list yet`.
    pub fn label(&self) -> String {
        match self {
            Source::Gateway { fetched_at, .. } => {
                format!(
                    "gateway, {}",
                    crate::state::ago(u64::try_from(*fetched_at).unwrap_or(0))
                )
            }
            Source::NotYet => "no list yet".into(),
        }
    }
}

/// A validated model list out of a sandbox's cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Harvest {
    pub fetched_at: i64,
    pub base_url: String,
    pub models: Vec<Model>,
}

/// An id the gateway may serve and a generated kit may carry: `[A-Za-z0-9._:/@+-]{1,128}`
/// ([`crate::litellm::valid_model_id`]), the pattern `litellm-models.sh` filters with too.
pub fn valid_id(id: &str) -> bool {
    crate::litellm::valid_model_id(id)
}

/// A display name from a sandbox: control characters out, at most [`MAX_NAME_CHARS`], trimmed.
fn clean_name(name: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    kept.trim().to_string()
}

/// The models of a cache or host file: valid ids only, each once, named (the id, capped like any
/// name, when it has none).
/// `None` for no list, a list over [`MAX_HARVEST_MODELS`], or one with no valid id left.
fn harvest_models(value: &serde_json::Value) -> Option<Vec<Model>> {
    let list = value.get("models")?.as_array()?;
    if list.len() > MAX_HARVEST_MODELS {
        return None;
    }
    let mut out: Vec<Model> = Vec::new();
    for entry in list {
        let Some(id) = entry
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|id| valid_id(id))
        else {
            continue;
        };
        if out.iter().any(|m| m.id == id) {
            continue;
        }
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .map(clean_name)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| clean_name(id));
        let text = |key: &str| {
            (entry.get(key).and_then(|v| v.as_str()))
                .filter(|s| valid_id(s))
                .map(str::to_string)
        };
        out.push(Model {
            id: id.to_string(),
            name,
            limits: Limits::read(entry),
            provider: text("provider"),
            upstream: text("upstream"),
        });
    }
    (!out.is_empty()).then_some(out)
}

/// A `fetched_at` that is an integer > 0 and at most [`MAX_FUTURE_SECS`] past `now`.
fn fetched_at(value: &serde_json::Value, now: i64) -> Option<i64> {
    value
        .get("fetched_at")?
        .as_i64()
        .filter(|t| *t > 0 && *t <= now.saturating_add(MAX_FUTURE_SECS))
}

/// A base URL worth showing: `https://`, printable ASCII, at most 300 characters; else empty.
fn clean_base_url(value: &serde_json::Value) -> String {
    value
        .get("base_url")
        .and_then(|v| v.as_str())
        .filter(|u| {
            u.starts_with("https://") && u.len() <= 300 && u.chars().all(|c| c.is_ascii_graphic())
        })
        .unwrap_or_default()
        .to_string()
}

/// Read the cache `litellm-models.sh` wrote in a sandbox (`sbx exec <name> cat <cache>`). Sandbox
/// data is untrusted: a cache over [`MAX_HARVEST_BYTES`], not JSON, without a valid `fetched_at`
/// or without a valid model is refused whole (`None`); invalid ids are dropped.
pub fn parse_sandbox_cache(text: &str, now: i64) -> Option<Harvest> {
    if text.len() > MAX_HARVEST_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(Harvest {
        fetched_at: fetched_at(&value, now)?,
        base_url: clean_base_url(&value),
        models: harvest_models(&value)?,
    })
}

/// What [`store_harvest`] writes, and the only fields it keeps.
#[derive(Debug, Serialize)]
struct HostFile<'a> {
    fetched_at: i64,
    harvested_at: i64,
    sandbox: &'a str,
    base_url: &'a str,
    models: Vec<HostModel<'a>>,
}

#[derive(Debug, Serialize)]
struct HostModel<'a> {
    id: &'a str,
    name: &'a str,
    #[serde(flatten)]
    limits: Limits,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<&'a str>,
}

/// The host file, validated like a sandbox's cache (it is a hand-editable file). `None` when it is
/// missing or invalid — which [`store_harvest`] treats as "anything newer replaces it".
fn read_host(path: &Path) -> Option<(String, Harvest)> {
    let text = fs::read_to_string(path).ok()?;
    if text.len() > MAX_HARVEST_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let sandbox = value
        .get("sandbox")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let harvest = Harvest {
        fetched_at: value.get("fetched_at")?.as_i64().filter(|t| *t > 0)?,
        base_url: clean_base_url(&value),
        models: harvest_models(&value)?,
    };
    Some((clean_name(sandbox), harvest))
}

/// Keep a sandbox's list on the host, only when its `fetched_at` is newer than the stored one or
/// the stored file is missing or invalid; `Ok(false)` otherwise. The compare and the atomic write
/// happen under an exclusive lock on `<path>.lock`.
pub fn store_harvest(path: &Path, sandbox: &str, h: &Harvest, now: i64) -> Result<bool> {
    let dir = path
        .parent()
        .context("the model list path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let lock_path = path.with_extension("json.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("opening {}", lock_path.display()))?;
    lock.lock()
        .with_context(|| format!("locking {}", lock_path.display()))?;
    if read_host(path).is_some_and(|(_, stored)| stored.fetched_at >= h.fetched_at) {
        return Ok(false);
    }
    let file = HostFile {
        fetched_at: h.fetched_at,
        harvested_at: now,
        sandbox,
        base_url: &h.base_url,
        models: h
            .models
            .iter()
            .map(|m| HostModel {
                id: &m.id,
                name: &m.name,
                limits: m.limits,
                provider: m.provider.as_deref(),
                upstream: m.upstream.as_deref(),
            })
            .collect(),
    };
    // A temp file per process and call: a process that ignores the lock cannot truncate ours.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = fs::write(&tmp, serde_json::to_vec_pretty(&file)?)
        .with_context(|| format!("writing {}", tmp.display()))
        .and_then(|()| {
            fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
        });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written.map(|()| true)
}

/// The models a LiteLLM connection offers for pinning, and where they came from: its harvested
/// list (`harvest`, [`crate::litellm::harvest_file`]) when it parses and is non-empty, else none.
pub fn catalog_for(harvest: Option<&Path>) -> (Vec<Model>, Source) {
    let Some((sandbox, harvest)) = harvest.and_then(read_host) else {
        return (Vec::new(), Source::NotYet);
    };
    let source = Source::Gateway {
        sandbox,
        fetched_at: harvest.fetched_at,
    };
    (named_from(harvest.models, &[]), source)
}

/// A harvested list, sorted, with the names of `bundled` (the list shown before) filled in where
/// it has only ids.
pub fn named_from(mut models: Vec<Model>, bundled: &[Model]) -> Vec<Model> {
    for m in &mut models {
        if m.name == clean_name(&m.id) {
            if let Some(name) = display_name(bundled, &m.id) {
                m.name = name;
            }
        }
    }
    sort_models(&mut models);
    models
}

/// The gateway's display name for a model id, if the catalog still knows it. An unknown id is
/// shown as it is: its pin keeps working.
pub fn display_name(catalog: &[Model], id: &str) -> Option<String> {
    catalog.iter().find(|m| m.id == id).map(|m| m.name.clone())
}

// --- the generated kit -------------------------------------------------------------------------

/// Where the generated kits live. Under the test guard, a scratch folder instead, as for the token
/// kits ([`crate::tokens::kits_dir`]): the create tests write model kits.
pub fn kits_dir() -> PathBuf {
    if cfg!(any(test, feature = "test-support")) {
        std::env::temp_dir().join("aisbx-tests-models")
    } else {
        state_dir().join("models")
    }
}

/// The kit directory name for a sandbox. A name that has to be reshaped (sbx allows `.` and `_`)
/// carries a short digest of the original, so two sandboxes never share one kit.
pub(crate) fn slug(sandbox: &str) -> String {
    let mapped: String = sandbox
        .chars()
        .map(|c| match c.is_ascii_alphanumeric() {
            true => c.to_ascii_lowercase(),
            false => '-',
        })
        .collect();
    if mapped == sandbox {
        return mapped;
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(sandbox.as_bytes());
    format!("{mapped}-{:02x}{:02x}", digest[0], digest[1])
}

pub fn kit_dir(sandbox: &str) -> PathBuf {
    kits_dir().join(format!("{KIT_PREFIX}{}", slug(sandbox)))
}

/// Is this kit directory name a generated model-override kit?
pub fn is_kit(dir_name: &str) -> bool {
    dir_name
        .strip_prefix(KIT_PREFIX)
        .is_some_and(|rest| !rest.is_empty())
}

/// Write the kit that pins a sandbox's aliases, and return the directory to pass to `--kit`;
/// `None` for an empty selection. A pin whose id fails [`valid_id`] is dropped.
pub fn ensure_kit(sandbox: &str, over: &Overrides, catalog: &[Model]) -> Result<Option<PathBuf>> {
    let over = &valid_pins(over);
    if over.is_empty() {
        remove_kit(sandbox)?;
        return Ok(None);
    }
    let dir = kit_dir(sandbox);
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating the model kit directory for {sandbox}"))?;
    fs::write(
        crate::kits::descriptor_path(&dir),
        descriptor(sandbox, over, catalog),
    )
    .with_context(|| format!("writing the model kit for {sandbox}"))?;
    Ok(Some(dir))
}

/// Drop a sandbox's generated kit. A missing directory is not an error.
pub fn remove_kit(sandbox: &str) -> Result<()> {
    let dir = kit_dir(sandbox);
    match fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", dir.display())),
    }
}

/// Read a sandbox's pins back out of the kit `sbx inspect` reports. A deleted directory reads as
/// no pins.
pub fn read_kit(dir: &Path) -> Overrides {
    let mut over = Overrides::default();
    let pairs = match fs::read_to_string(crate::kits::descriptor_path(dir)) {
        Ok(text) => crate::kits::load_descriptor(&text)
            .ok()
            .and_then(|doc| doc["build"].as_str().map(env_lines))
            .unwrap_or_default(),
        Err(_) => return over,
    };
    for (key, value) in pairs {
        // Exact key match: `…_MODEL_NAME` also starts with `…_MODEL`, and picking that up would
        // pin the alias to the display name.
        if let Some(slot) = Slot::ALL.into_iter().find(|s| s.env() == key) {
            over.set(slot, Some(value));
        }
    }
    over
}

/// The `ENV KEY="value"` lines of a generated kit's inline Dockerfile, one variable each, values
/// unescaped as [`dockerfile_quoted`] escaped them.
fn env_lines(build: &str) -> Vec<(String, String)> {
    build
        .lines()
        .filter_map(|line| line.trim().strip_prefix("ENV "))
        .filter_map(|rest| rest.split_once('='))
        .filter_map(|(key, value)| {
            let inner = value.trim().strip_prefix('"')?.strip_suffix('"')?;
            let mut out = String::new();
            let mut chars = inner.chars();
            while let Some(c) = chars.next() {
                out.push(match c {
                    '\\' => chars.next()?,
                    c => c,
                });
            }
            Some((key.trim().to_string(), out))
        })
        .collect()
}

/// A value as a Dockerfile double-quoted string: a backslash escapes `\`, `"` and `$`, which would
/// otherwise end the string or start a variable.
fn dockerfile_quoted(value: &str) -> String {
    let mut out = String::from('"');
    for c in value.chars() {
        if matches!(c, '\\' | '"' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Claude Code's pins whose ids pass [`valid_id`] — the only ones a generated kit may name
/// anywhere. The other agents' slots live in their config, never in a kit.
fn valid_pins(over: &Overrides) -> Overrides {
    let mut valid = Overrides::default();
    let pins = over
        .entries()
        .filter(|(slot, id)| slot.is_claude() && valid_id(id));
    for (slot, id) in pins {
        valid.set(slot, Some(id.to_string()));
    }
    valid
}

/// The kit's descriptor: a v3 mixin whose whole content is its variables, as image ENV in an
/// inline `build:`. A display name has no line break ([`clean_name`]), so it fits a block scalar.
fn descriptor(sandbox: &str, over: &Overrides, catalog: &[Model]) -> String {
    // Filtered here as well as in `ensure_kit`: the header's summary names the ids too.
    let over = &valid_pins(over);
    let mut vars = String::new();
    for (slot, id) in over.entries() {
        vars.push_str(&format!(
            "\x20 ENV {}={}\n",
            slot.env(),
            dockerfile_quoted(id)
        ));
        // The `_NAME` companion: the picker shows the raw id unless it is told a name.
        if let Some(name) = display_name(catalog, id) {
            vars.push_str(&format!(
                "\x20 ENV {}={}\n",
                slot.name_env(),
                dockerfile_quoted(&name)
            ));
        }
    }
    let summary = over.summary();
    // Explanations go into YAML comments, not a `description:` block scalar: a comment cannot
    // change what the kit means.
    format!(
        "# syntax=docker/sandbox-kit:3\n\
         # Generated by aisbx — do not edit; rewritten whenever {sandbox} is built.\n\
         #\n\
         # Pins which model Claude Code's aliases resolve to on the LiteLLM gateway. Without this\n\
         # kit the aliases keep Claude Code's own defaults, which the gateway may not serve.\n\
         # The model ids come from the list a sandbox fetched from the gateway (GET /v1/models,\n\
         # kept per connection in ~/.local/state/aisbx/litellm/<name>/models.json), or from\n\
         # the connection's own connection.json on a host that has not fetched one yet.\n\
         #\n\
         #   {summary}\n\
         schemaVersion: \"3\"\n\
         kind: mixin\n\
         displayName: \"Claude models for {sandbox}\"\n\
         description: \"Pins the opus/sonnet/haiku aliases for {sandbox}.\"\n\
         \n\
         build: |\n\
         \x20 FROM scratch\n\
         {vars}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The create tests write model kits, so a test build keeps them out of the state folder.
    #[test]
    fn a_test_build_writes_model_kits_to_a_scratch_folder() {
        assert!(kits_dir().starts_with(std::env::temp_dir()));
        assert!(kit_dir("s").starts_with(kits_dir()));
    }

    #[test]
    fn valid_ids_follow_the_scripts_pattern() {
        for good in [
            "claude-opus-5",
            "glm-4.7-flash",
            "openai/gpt-5",
            "a",
            "m@v1:2+x_y",
            "-x",
            ".x",
            "_internal",
        ] {
            assert!(valid_id(good), "{good}");
        }
        let long = "a".repeat(129);
        for bad in [
            "",
            "*",
            "openai/*",
            "bad\nid",
            "evil\"quote",
            "a b",
            "x\u{7}",
            "é",
            long.as_str(),
        ] {
            assert!(!valid_id(bad), "{bad:?}");
        }
        assert!(valid_id(&"a".repeat(128)));
    }

    const NOW: i64 = 1_800_000_000;

    fn cache_json(fetched_at: &str, models: &str) -> String {
        format!(
            r#"{{"version":1,"fetched_at":{fetched_at},"attempted_at":1,"base_url":"https://litellm.example.com","models":{models}}}"#
        )
    }

    #[test]
    fn a_sandbox_cache_is_validated_before_it_is_used() {
        let text = cache_json(
            &NOW.to_string(),
            r#"[{"id":"glm-5"},{"id":"bad\nid","name":"x"},{"id":"evil\"q"},{"id":"glm-5"},
                {"id":"kimi","name":"Kimi\u0007 K2\u001b[31m"},{"id":"long","name":"NAME_LONG"},{"id":5}]"#,
        )
        .replace("NAME_LONG", &"n".repeat(200));
        let h = parse_sandbox_cache(&text, NOW).expect("a valid cache");
        assert_eq!(h.fetched_at, NOW);
        assert_eq!(h.base_url, "https://litellm.example.com");
        let ids: Vec<&str> = h.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            ["glm-5", "kimi", "long"],
            "invalid ids dropped, each once"
        );
        assert_eq!(h.models[0].name, "glm-5", "no name: the id");
        assert_eq!(
            h.models[1].name, "Kimi K2[31m",
            "control characters stripped"
        );
        assert_eq!(h.models[2].name.chars().count(), 80, "names capped");

        // A valid 128-character id with no name, or a blank or control-only one: the id is kept
        // whole, the name it falls back to is capped like any other.
        let m127 = "m".repeat(127);
        let text = cache_json(
            "1",
            &serde_json::json!([
                {"id": format!("m{m127}")},
                {"id": format!("mm{m127}")},
                {"id": format!("a{m127}"), "name": "  "},
                {"id": format!("b{m127}"), "name": "\u{7}\u{1b}"},
            ])
            .to_string(),
        );
        let h = parse_sandbox_cache(&text, NOW).expect("a valid cache");
        assert_eq!(
            h.models.len(),
            3,
            "the 129-character id is dropped: {:?}",
            h.models
        );
        for m in &h.models {
            assert_eq!(m.id.len(), 128, "{m:?}");
            assert_eq!(m.name.chars().count(), 80, "{m:?}");
            assert!(m.id.starts_with(&m.name), "{m:?}");
        }

        // `fetched_at`: missing, zero, negative, a float, a string, too far ahead → refused.
        let ahead = (NOW + 301).to_string();
        for bad in ["null", "0", "-5", "1.5", "\"1800000000\"", ahead.as_str()] {
            assert_eq!(
                parse_sandbox_cache(&cache_json(bad, r#"[{"id":"a"}]"#), NOW),
                None,
                "{bad}"
            );
        }
        assert!(parse_sandbox_cache(
            &cache_json(&(NOW + 300).to_string(), r#"[{"id":"a"}]"#),
            NOW
        )
        .is_some());
        // No list, an empty one, one with no valid id, or over 500 models → refused.
        for bad in ["null", "[]", r#"[{"id":"*"}]"#] {
            assert_eq!(
                parse_sandbox_cache(&cache_json("1", bad), NOW),
                None,
                "{bad}"
            );
        }
        let many = |n: usize| {
            let items: Vec<String> = (0..n).map(|i| format!(r#"{{"id":"m{i}"}}"#)).collect();
            format!("[{}]", items.join(","))
        };
        assert_eq!(
            parse_sandbox_cache(&cache_json("1", &many(500)), NOW)
                .unwrap()
                .models
                .len(),
            500
        );
        assert_eq!(parse_sandbox_cache(&cache_json("1", &many(501)), NOW), None);
        // Over 1 MiB is refused whole, whatever it says; not JSON neither.
        let padded = format!(
            "{}{}",
            cache_json("1", r#"[{"id":"a"}]"#),
            " ".repeat(MAX_HARVEST_BYTES)
        );
        assert_eq!(parse_sandbox_cache(&padded, NOW), None);
        assert_eq!(parse_sandbox_cache("not json", NOW), None);
        // A base URL that is not https:// is not kept.
        let http = cache_json("1", r#"[{"id":"a"}]"#).replace("https://", "http://");
        assert_eq!(parse_sandbox_cache(&http, NOW).unwrap().base_url, "");
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aisbx-models-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn harvest(fetched_at: i64, ids: &[&str]) -> Harvest {
        Harvest {
            fetched_at,
            base_url: "https://litellm.example.com".into(),
            models: ids
                .iter()
                .map(|id| Model {
                    id: id.to_string(),
                    name: id.to_string(),
                    ..Default::default()
                })
                .collect(),
        }
    }

    #[test]
    fn only_a_newer_fetch_replaces_the_host_file() {
        let dir = temp_dir("store");
        let path = dir.join("gateway-models.json");
        assert!(
            store_harvest(&path, "a", &harvest(NOW - 100, &["x"]), NOW).unwrap(),
            "no file yet"
        );
        let first = fs::read(&path).unwrap();
        let stored: serde_json::Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(stored["fetched_at"], NOW - 100);
        assert_eq!(stored["harvested_at"], NOW);
        assert_eq!(stored["sandbox"], "a");
        assert_eq!(
            stored["models"],
            serde_json::json!([{"id": "x", "name": "x"}])
        );

        // The same `fetched_at` (a fresh skip read again) or an older one: nothing written.
        assert!(!store_harvest(&path, "a", &harvest(NOW - 100, &["y"]), NOW + 50).unwrap());
        assert!(!store_harvest(&path, "b", &harvest(NOW - 900, &["z"]), NOW + 50).unwrap());
        assert_eq!(fs::read(&path).unwrap(), first);
        // A newer one replaces it.
        assert!(store_harvest(&path, "b", &harvest(NOW, &["w"]), NOW + 60).unwrap());
        let (sandbox, h) = read_host(&path).unwrap();
        assert_eq!((sandbox.as_str(), h.fetched_at), ("b", NOW));
        // An invalid host file counts as missing.
        fs::write(&path, "{broken").unwrap();
        assert!(store_harvest(&path, "c", &harvest(1, &["v"]), NOW).unwrap());
        assert_eq!(tmp_files(&dir), Vec::<String>::new());
        let _ = fs::remove_dir_all(&dir);
    }

    fn tmp_files(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".tmp"))
            .collect()
    }

    /// Two dashboards harvesting at once: the newest fetch wins whatever the order, and the file
    /// is whole JSON afterwards — no shared temp file, no compare-then-write race.
    #[test]
    fn concurrent_harvests_keep_the_newest_fetch() {
        let dir = temp_dir("concurrent");
        let path = dir.join("gateway-models.json");
        for round in 0..5 {
            let _ = fs::remove_file(&path);
            let start = std::sync::Arc::new(std::sync::Barrier::new(16));
            let writers: Vec<_> = (0..16i64)
                .map(|i| {
                    let (path, start) = (path.clone(), start.clone());
                    std::thread::spawn(move || {
                        let id = format!("m{i}");
                        start.wait();
                        // Rotated per round, so the newest writer is not always the first spawned.
                        let at = NOW - ((i + round) % 16) * 10;
                        store_harvest(&path, &id, &harvest(at, &[id.as_str()]), NOW).unwrap();
                    })
                })
                .collect();
            for w in writers {
                w.join().unwrap();
            }
            let (sandbox, h) = read_host(&path).expect("whole, valid JSON");
            assert_eq!(h.fetched_at, NOW, "round {round}: the newest fetch is kept");
            assert_eq!(h.models[0].id, sandbox, "round {round}: its own list");
            assert_eq!(tmp_files(&dir), Vec::<String>::new());
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// A connection offers the list a sandbox on it harvested, and nothing before one has.
    #[test]
    fn the_catalog_is_the_harvested_list_or_nothing() {
        let dir = temp_dir("catalog");
        let path = dir.join("models.json");
        assert_eq!(catalog_for(None), (Vec::new(), Source::NotYet));
        // No harvested file, or an empty or invalid one: nothing to pin.
        assert_eq!(
            catalog_for(Some(path.as_path())),
            (Vec::new(), Source::NotYet)
        );
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, r#"{"fetched_at":5,"models":[]}"#).unwrap();
        assert_eq!(catalog_for(Some(path.as_path())).1, Source::NotYet);

        store_harvest(&path, "s", &harvest(NOW, &["zz-new", "claude-opus-5"]), NOW).unwrap();
        let (models, source) = catalog_for(Some(path.as_path()));
        assert_eq!(
            source,
            Source::Gateway {
                sandbox: "s".into(),
                fetched_at: NOW
            }
        );
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids.len(), 2, "only the harvested models");
        assert!(ids.contains(&"zz-new") && ids.contains(&"claude-opus-5"));
        assert_eq!(Source::NotYet.label(), "no list yet");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_the_gateway_says_of_a_model_survives_the_host_file() {
        let text = cache_json(
            &NOW.to_string(),
            r#"[{"id":"claude-opus-5-5","context":1000000,"vision":true,"caching":true,
                 "input_cost":0.0000044,"output_cost":0.000022,"provider":"bedrock_converse",
                 "upstream":"eu.anthropic.claude-opus-5-5"},
                {"id":"glm-5.3","vision":"yes","input_cost":-1,"output_cost":2,
                 "provider":"bad provider","upstream":"x\ny"}]"#,
        );
        let harvest = parse_sandbox_cache(&text, NOW).expect("a valid cache");
        let opus = &harvest.models[0];
        assert_eq!(opus.limits.vision, Some(true));
        assert_eq!(opus.limits.caching, Some(true));
        assert_eq!(opus.limits.input_cost, Some(Price(4_400_000)));
        assert_eq!(opus.limits.output_cost.map(Price::per_mtok), Some(22.0));
        assert_eq!(opus.origin().as_deref(), Some("Bedrock EU"));
        assert!(opus.is_claude());
        let glm = &harvest.models[1];
        assert_eq!(
            glm.limits,
            Limits::default(),
            "no value of the wrong type or range"
        );
        assert_eq!((&glm.provider, &glm.upstream), (&None, &None));
        assert!(!glm.is_claude());
        assert_eq!(glm.origin(), None);

        let dir = temp_dir("fields");
        let path = dir.join("models.json");
        store_harvest(&path, "s", &harvest, NOW).unwrap();
        let (models, _) = catalog_for(Some(path.as_path()));
        let back = models.iter().find(|m| m.id == "claude-opus-5-5").unwrap();
        assert_eq!(back, opus, "the host file keeps every field");
        let _ = fs::remove_dir_all(&dir);
    }

    /// An id the model-list pattern accepts can be pinned, a punctuation-led one alone or next to
    /// an ordinary one: the harvest, the catalog and the kit share one pattern.
    #[test]
    fn a_punctuation_led_connection_model_survives_into_the_pin_kit() {
        let catalog = harvest(NOW, &["_internal", "glm-5"]).models;
        assert!(crate::litellm::valid_model_id("_internal"));

        let mut mixed = Overrides::default();
        mixed.set(Slot::Opus, Some("_internal".into()));
        mixed.set(Slot::Haiku, Some("glm-5".into()));
        let text = descriptor("aisbx-selftest-punct", &mixed, &catalog);
        for want in [
            "ENV ANTHROPIC_DEFAULT_OPUS_MODEL=\"_internal\"",
            "ENV ANTHROPIC_DEFAULT_HAIKU_MODEL=\"glm-5\"",
        ] {
            assert!(text.contains(want), "{want} missing in {text}");
        }
        let mut alone = Overrides::default();
        alone.set(Slot::Sonnet, Some(".dot".into()));
        assert!(descriptor("aisbx-selftest-punct", &alone, &catalog)
            .contains("ENV ANTHROPIC_DEFAULT_SONNET_MODEL=\".dot\""));
    }

    #[test]
    fn the_generated_kit_never_carries_an_invalid_id() {
        let mut over = Overrides::default();
        over.set(Slot::Opus, Some("evil\"\n  CLAUDE_CODE_X: \"1".into()));
        over.set(Slot::Haiku, Some("glm-5".into()));
        let text = descriptor("aisbx-selftest-invalid", &over, &[]);
        assert!(!text.contains("evil"), "{text}");
        assert!(
            text.contains("ENV ANTHROPIC_DEFAULT_HAIKU_MODEL=\"glm-5\""),
            "{text}"
        );
        assert!(
            text.contains("litellm/<name>/models.json"),
            "the header names the new source: {text}"
        );

        // Nothing valid left: no kit at all.
        let mut only_bad = Overrides::default();
        only_bad.set(Slot::Opus, Some("openai/*".into()));
        assert_eq!(
            ensure_kit("aisbx-selftest-invalid", &only_bad, &[]).unwrap(),
            None
        );
    }

    #[test]
    fn an_empty_selection_needs_no_kit() {
        let over = Overrides::default();
        assert!(over.is_empty());
        assert_eq!(over.summary(), "");
        assert_eq!(
            ensure_kit("aisbx-selftest-models-empty", &over, &[]).unwrap(),
            None
        );
    }

    #[test]
    fn the_generated_kit_round_trips_through_the_reader() {
        let catalog = vec![Model {
            id: "claude-opus-5".into(),
            name: "Claude Opus 5".into(),
            ..Default::default()
        }];
        let mut over = Overrides::default();
        over.set(Slot::Opus, Some("claude-opus-5".into()));
        over.set(Slot::Haiku, Some("claude-haiku-4.5".into()));

        let dir = ensure_kit("aisbx-selftest-models", &over, &catalog)
            .expect("writing the kit")
            .expect("a non-empty selection produces a kit");
        let text = fs::read_to_string(crate::kits::descriptor_path(&dir)).unwrap();
        let doc = crate::kits::load_descriptor(&text).unwrap();
        assert_eq!(doc["kind"].as_str(), Some("mixin"), "{text}");
        let build = doc["build"].as_str().unwrap();
        assert!(build.starts_with("FROM scratch\n"), "{build}");
        assert!(
            build.contains("ENV ANTHROPIC_DEFAULT_OPUS_MODEL=\"claude-opus-5\""),
            "{build}"
        );
        assert!(
            build.contains("ENV ANTHROPIC_DEFAULT_HAIKU_MODEL=\"claude-haiku-4.5\""),
            "{build}"
        );
        // A model the catalog knows gets a display name; one it does not is left as its id.
        assert!(
            build.contains("ENV ANTHROPIC_DEFAULT_OPUS_MODEL_NAME=\"Claude Opus 5\""),
            "{build}"
        );
        assert!(
            !build.contains("ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME"),
            "{build}"
        );
        // An unset slot must not appear at all — an empty value would pin the alias to nothing.
        assert!(!build.contains("ANTHROPIC_DEFAULT_SONNET_MODEL"), "{build}");
        // No capabilities: a model kit must not widen anything, and `sbx kit add` takes it.
        assert!(doc["capabilities"].is_badvalue(), "{text}");
        assert_eq!(crate::kits::Kit::read(&dir).unwrap().kit_add_blocker, None);

        // …and the reader gets exactly the selection back, `_NAME` lines and all.
        assert_eq!(read_kit(&dir), over);

        // The round trip is also what `model::resolve` relies on to derive a sandbox's models.
        let dir_name = dir.file_name().unwrap().to_string_lossy().to_string();
        assert!(is_kit(&dir_name));

        remove_kit("aisbx-selftest-models").unwrap();
        assert!(!dir.exists());
        // Deleting a kit twice, or reading one that is gone, is a normal state.
        remove_kit("aisbx-selftest-models").unwrap();
        assert_eq!(read_kit(&dir), Overrides::default());
    }

    /// A display name is whatever the gateway calls a model: quotes, backslashes and `$` reach
    /// the container as they are, and come back out of the kit the same.
    #[test]
    fn a_display_name_survives_the_dockerfile_quoting() {
        let odd = "Opus \"5\" $HOME \\ `x` ${{ kit.env.A }}";
        let build = format!(
            "FROM scratch\nENV ANTHROPIC_DEFAULT_OPUS_MODEL_NAME={}\n",
            dockerfile_quoted(odd)
        );
        assert_eq!(
            env_lines(&build),
            [(
                "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME".to_string(),
                odd.to_string()
            )]
        );
        assert!(
            !dockerfile_quoted(odd).contains(" $HOME"),
            "a variable stays literal"
        );
        // The exact escapes Docker reads back (measured with an ENV line of `"`, `$`, `\` and `).
        assert_eq!(dockerfile_quoted(r#"a"b$c\d`e"#), r#""a\"b\$c\\d`e""#);
    }

    #[test]
    fn kit_names_stay_distinct_when_a_sandbox_name_has_to_be_reshaped() {
        assert!(kit_dir("claude-litellm-proj").ends_with("models-claude-litellm-proj"));
        // `.` and `_` are legal in an sbx name but not in a kit name, so those names carry a
        // digest — otherwise `my.proj` and `my-proj` would share one kit and silently overwrite
        // each other's models.
        let dotted = kit_dir("my.proj");
        let dashed = kit_dir("my-proj");
        assert_ne!(dotted, dashed);
        assert!(dotted
            .to_string_lossy()
            .starts_with(dashed.to_string_lossy().as_ref()));
    }

    #[test]
    fn only_the_three_aliases_are_recognised() {
        assert!(is_kit("models-x"));
        assert!(!is_kit("models-"));
        assert!(!is_kit("jdk25"));
        assert!(!is_kit("token-private"));
        for slot in Slot::ALL {
            assert!(slot.env().starts_with("ANTHROPIC_DEFAULT_"));
            assert!(slot.env().ends_with("_MODEL"));
        }
    }

    /// The kit passes sbx's own validator, which decides whether a sandbox can be built at all. Run
    /// it with the other live checks: `cargo test -- --ignored --nocapture live`.
    #[test]
    #[ignore = "runs sbx; part of the live suite"]
    fn live_sbx_validates_the_generated_kit() {
        let models = harvest(NOW, &["claude-opus-5", "glm-5"]).models;
        let mut over = Overrides::default();
        for (slot, model) in Slot::ALL.into_iter().zip(models.iter()) {
            over.set(slot, Some(model.id.clone()));
        }
        let dir = ensure_kit("aisbx-selftest-models", &over, &models)
            .unwrap()
            .expect("the selftest pins at least one alias");
        let out = std::process::Command::new("sbx")
            .args(["kit", "validate", "--json"])
            .arg(&dir)
            .output()
            .expect("running `sbx kit validate`");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        println!("sbx kit validate {} →\n{text}", dir.display());
        remove_kit("aisbx-selftest-models").ok();
        assert!(out.status.success(), "{text}");
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
        assert_eq!(report["valid"], serde_json::json!(true), "{text}");
        assert_eq!(report["warnings"], serde_json::json!([]), "{text}");
        assert!(!text.to_lowercase().contains("deprecat"), "{text}");
    }
}
