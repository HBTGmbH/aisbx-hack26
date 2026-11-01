//! LiteLLM connections: one gateway shared by any number of sandboxes. `connection.json` holds the
//! name and URL; the key is a global sbx custom secret under [`placeholder`], never in a file.
//! [`ensure_kit`] writes the generated mixin that wires a sandbox to a connection.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::state::state_dir;

/// A generated connection kit's directory (and so its kit name): `litellm-<name>-<agent>`.
pub const KIT_PREFIX: &str = "litellm-";

/// The custom-secret placeholder of a connection's key: `sbx-cs-aisbx-llm-<name>`.
pub const PLACEHOLDER_PREFIX: &str = "sbx-cs-aisbx-llm-";

/// The OpenCode settings every connection's generated config starts from — `share: disabled`,
/// `update: "disable"`, the provider denies, `default_agent: plan`, the permissions. No provider
/// and no model: [`ensure_kit`] adds the connection's.
pub const OPENCODE_TEMPLATE: &str = include_str!("../assets/opencode-litellm.json");

/// The Pi settings every connection's generated `settings.json` starts from — the schema and
/// `enableInstallTelemetry: false`. No provider and no model: [`pi_settings`] adds the connection's.
pub const PI_SETTINGS_TEMPLATE: &str = include_str!("../assets/pi-litellm-settings.json");

/// Pi's `models.json` schema (Pi 1.4.0 docs/models.md).
const PI_MODELS_SCHEMA: &str = "https://pi.dev/schemas/models.schema.json";

/// The longest connection name (`[a-z0-9][a-z0-9-]{0,31}`).
const MAX_NAME: usize = 32;

/// A LiteLLM gateway the user added. **No key, ever**: that is the custom secret. No models: the
/// sandboxes read them from the gateway. Old files' `models` and `default_model` are ignored.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Connection {
    pub name: String,
    pub url: String,
}

/// The agent a generated kit wires a connection into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Opencode,
    Pi,
}

impl Agent {
    pub fn as_str(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Opencode => "opencode",
            Agent::Pi => "pi",
        }
    }

    fn parse(s: &str) -> Option<Agent> {
        match s {
            "claude" => Some(Agent::Claude),
            "opencode" => Some(Agent::Opencode),
            "pi" => Some(Agent::Pi),
            _ => None,
        }
    }

    /// The agent a LiteLLM backend kit runs, or `None` for any other backend — what makes a
    /// backend need a connection.
    pub fn of_backend(kit_name: &str) -> Option<Agent> {
        match kit_name {
            "claude-litellm" => Some(Agent::Claude),
            "opencode-litellm" => Some(Agent::Opencode),
            "pi-litellm" => Some(Agent::Pi),
            _ => None,
        }
    }
}

/// Where the connections live: `state_dir()/litellm/<name>/`.
pub fn dir() -> PathBuf {
    state_dir().join("litellm")
}

fn conn_file(root: &Path, name: &str) -> PathBuf {
    root.join(name).join("connection.json")
}

/// Every readable connection, sorted by name. Never the user's under `cargo test`: the suite must
/// not depend on what whoever runs it has added (tests use [`load_all_in`]).
pub fn load_all() -> Vec<Connection> {
    if cfg!(any(test, feature = "test-support")) {
        return Vec::new();
    }
    load_all_in(&dir())
}

/// [`load_all`] under `root`. A folder whose `connection.json` is missing, unreadable, invalid
/// or names another connection is skipped.
pub fn load_all_in(root: &Path) -> Vec<Connection> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<Connection> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter_map(|name| load_in(root, &name))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The connection of that name, read now. `None` under `cargo test` (see [`load_all`]).
pub fn load(name: &str) -> Option<Connection> {
    if cfg!(any(test, feature = "test-support")) {
        return None;
    }
    load_in(&dir(), name)
}

pub fn load_in(root: &Path, name: &str) -> Option<Connection> {
    if !valid_name(name) {
        return None;
    }
    let text = fs::read_to_string(conn_file(root, name)).ok()?;
    let conn: Connection = serde_json::from_str(&text).ok()?;
    (conn.name == name && validate(&conn).is_ok()).then_some(conn)
}

/// Does a connection of that name exist: the authority for refusing a name that is taken.
pub fn exists(name: &str) -> bool {
    !cfg!(any(test, feature = "test-support")) && exists_in(&dir(), name)
}

pub fn exists_in(root: &Path, name: &str) -> bool {
    valid_name(name) && conn_file(root, name).is_file()
}

/// `[a-z0-9][a-z0-9-]{0,31}`.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= MAX_NAME
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `[A-Za-z0-9._:/@+-]{1,128}`.
pub fn valid_model_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/@+-".contains(c))
}

/// Check a URL against `https://<host>[:443][/<path>]`: https only, and port 443 only, because the
/// custom-secret swap on another port is unmeasured.
pub fn check_url(url: &str) -> Result<()> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow!("the URL must start with https://"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        bail!("the URL's host may hold only letters, digits, “.” and “-”");
    }
    if port.is_some_and(|p| p != "443") {
        bail!("only port 443 for now");
    }
    if !path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._~/-".contains(c))
    {
        bail!("the URL's path may hold only letters, digits and “._~/-”");
    }
    Ok(())
}

/// Every check of a connection, before anything is written or sbx is called.
pub fn validate(c: &Connection) -> Result<()> {
    if !valid_name(&c.name) {
        bail!(
            "the connection name must be 1–32 of a–z, 0–9 and “-”, starting with a letter or digit"
        );
    }
    check_url(&c.url)
}

/// The host a URL names, without port or path. Only meaningful for a URL [`check_url`] accepted.
pub fn host(url: &str) -> &str {
    let rest = url.strip_prefix("https://").unwrap_or(url);
    let authority = rest.split('/').next().unwrap_or(rest);
    authority.split(':').next().unwrap_or(authority)
}

/// Does saving `new` over `old` move the connection to another host? Then the key must be given
/// again: sbx cannot read the stored key back to retarget it.
pub fn moves_host(old: &Connection, new: &Connection) -> bool {
    host(&old.url) != host(&new.url)
}

/// The URL without a trailing `/` — what goes into `ANTHROPIC_BASE_URL` and `--base-url`.
fn base(url: &str) -> &str {
    url.trim_end_matches('/')
}

/// Write `connection.json`, **only** after `set-custom` exited 0 or when no key changes.
pub fn save(c: &Connection) -> Result<()> {
    save_in(&dir(), c)
}

pub fn save_in(root: &Path, c: &Connection) -> Result<()> {
    validate(c)?;
    let folder = root.join(&c.name);
    fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    let path = conn_file(root, &c.name);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(c)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))
}

/// Remove a connection's folder (its `connection.json`, generated kits and harvested list). A
/// missing folder is not an error.
pub fn remove(name: &str) -> Result<()> {
    remove_in(&dir(), name)
}

pub fn remove_in(root: &Path, name: &str) -> Result<()> {
    if !valid_name(name) {
        bail!("no connection named {name:?}");
    }
    let folder = root.join(name);
    match fs::remove_dir_all(&folder) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", folder.display())),
    }
}

/// The placeholder a connection's key is stored under. Not a secret: it is what the sandbox sees.
pub fn placeholder(name: &str) -> String {
    format!("{PLACEHOLDER_PREFIX}{name}")
}

/// The connection a placeholder belongs to, or `None` for any other placeholder.
pub fn name_of_placeholder(p: &str) -> Option<&str> {
    p.strip_prefix(PLACEHOLDER_PREFIX)
        .filter(|name| valid_name(name))
}

/// The generated kit's directory name for a connection and agent.
pub fn kit_name(name: &str, agent: Agent) -> String {
    format!("{KIT_PREFIX}{name}-{}", agent.as_str())
}

/// The connection and agent a kit directory name wires, or `None` for any other kit.
pub fn of_kit(dir_name: &str) -> Option<(&str, Agent)> {
    let rest = dir_name.strip_prefix(KIT_PREFIX)?;
    let (name, agent) = rest.rsplit_once('-')?;
    let agent = Agent::parse(agent)?;
    valid_name(name).then_some((name, agent))
}

/// The marker variable of a generated kit: `SBX_KIT_LITELLM_<NAME>_<AGENT>`.
fn marker(name: &str, agent: Agent) -> String {
    format!("SBX_KIT_LITELLM_{name}_{}", agent.as_str())
        .to_ascii_uppercase()
        .replace('-', "_")
}

/// The path OpenCode reads the connection's config from inside the sandbox.
pub fn opencode_config_path(name: &str) -> String {
    format!("/home/agent/.config/opencode/opencode-litellm-{name}.json")
}

/// Write the generated kit for a connection and agent, and return its directory for
/// `sbx create --kit`. Rewritten every time, so a stale or edited kit never points elsewhere.
pub fn ensure_kit(c: &Connection, agent: Agent) -> Result<PathBuf> {
    ensure_kit_in(&dir(), c, agent)
}

pub fn ensure_kit_in(root: &Path, c: &Connection, agent: Agent) -> Result<PathBuf> {
    validate(c)?;
    let kit = kit_dir_in(root, &c.name, agent);
    fs::create_dir_all(&kit).with_context(|| format!("creating {}", kit.display()))?;
    let files = agent_files_in(root, c, agent)?;
    fs::write(
        crate::kits::descriptor_path(&kit),
        descriptor(c, agent, &files)?,
    )
    .with_context(|| format!("writing the kit for connection “{}”", c.name))?;
    Ok(kit)
}

/// Where [`ensure_kit_in`] writes a connection's kit for `agent` — known before it is written, so
/// a create can name it while the connection's key is still being stored.
pub fn kit_dir_in(root: &Path, name: &str, agent: Agent) -> PathBuf {
    root.join(name).join(kit_name(name, agent))
}

/// The provider id a connection's agent config names: `litellm-<name>`.
pub fn provider_id(name: &str) -> String {
    format!("{KIT_PREFIX}{name}")
}

/// The models a connection offers its agent, and what the agent starts on: the fetched list
/// minus the switched-off models, and the picks that still point into it.
#[derive(Debug, Clone, Default)]
pub struct Offer {
    pub models: Vec<crate::models::Model>,
    pub slots: crate::models::Overrides,
    /// The newest Sonnet of the whole fetched list, switched off or not: what Claude Code's
    /// picker treats every non-Claude model as ([`newest_sonnet`]).
    pub sonnet: Option<String>,
}

impl Offer {
    /// What `root` holds for a connection and agent: its fetched list ([`harvest_file`]) and its
    /// picks ([`crate::model_picks`]).
    pub fn read(root: &Path, name: &str, agent: Agent) -> Self {
        let (models, _) = crate::models::catalog_for(Some(&root.join(name).join("models.json")));
        let picks = crate::model_picks::load_in(root, name, agent);
        Self::of(&models, &picks, agent)
    }

    /// The offer of a fetched list under a set of picks.
    pub fn of(
        models: &[crate::models::Model],
        picks: &crate::model_picks::Picks,
        agent: Agent,
    ) -> Self {
        Self {
            models: picks.enabled(models).into_iter().cloned().collect(),
            slots: picks.live_slots(agent, models),
            sonnet: newest_sonnet(models).map(str::to_string),
        }
    }
}

/// The Sonnet with the highest version among `models`: of the ids that start with
/// `claude-sonnet`, else of every id that names `sonnet`. A date (six digits or more) is no
/// version. `None` when no id names Sonnet.
pub fn newest_sonnet(models: &[crate::models::Model]) -> Option<&str> {
    let version = |id: &str| -> Vec<u64> {
        let lower = id.to_ascii_lowercase();
        let after = lower
            .split_once("sonnet")
            .map_or("", |(_, rest)| rest)
            .to_string();
        (after.split(|c: char| !c.is_ascii_digit()))
            .filter(|n| !n.is_empty() && n.len() < 6)
            .filter_map(|n| n.parse().ok())
            .collect()
    };
    let named = |prefix: bool| {
        (models.iter())
            .map(|m| m.id.as_str())
            .filter(move |id| {
                let lower = id.to_ascii_lowercase();
                match prefix {
                    true => lower.starts_with("claude-sonnet"),
                    false => lower.contains("sonnet"),
                }
            })
            .max_by(|a, b| {
                version(a)
                    .cmp(&version(b))
                    .then_with(|| b.len().cmp(&a.len()))
            })
    };
    named(true).or_else(|| named(false))
}

/// The OpenCode 2 config of a connection: the template plus the connection's provider under
/// `providers` with the offered models, the main pick as `model`, and the small pick as the model
/// of the built-in `title` agent (`agents.title.model`, OpenCode 1's `small_model`).
pub fn opencode_config(c: &Connection, offer: &Offer) -> Result<serde_json::Value> {
    use crate::models::Slot;
    use serde_json::{json, Map, Value};
    let mut config: Value =
        serde_json::from_str(OPENCODE_TEMPLATE).context("the bundled OpenCode template")?;
    let obj = config
        .as_object_mut()
        .context("the OpenCode template is not an object")?;
    let provider = provider_id(&c.name);
    let mut models = Map::new();
    for m in &offer.models {
        models.insert(m.id.clone(), opencode_model(m));
    }
    let mut providers = Map::new();
    providers.insert(
        provider.clone(),
        json!({
            "name": format!("LiteLLM {}", c.name),
            "package": OPENCODE_PACKAGE,
            "settings": {
                "baseURL": format!("{}/v1", base(&c.url)),
                "apiKey": placeholder(&c.name),
            },
            "models": models,
        }),
    );
    obj.insert("providers".into(), Value::Object(providers));
    if let Some(id) = offer.slots.get(Slot::Main) {
        obj.insert("model".into(), format!("{provider}/{id}").into());
    }
    if let Some(id) = offer.slots.get(Slot::Small) {
        let title = json!({ "title": { "model": format!("{provider}/{id}") } });
        obj.insert("agents".into(), title);
    }
    Ok(config)
}

/// OpenCode 2's runtime package for an OpenAI-compatible endpoint.
const OPENCODE_PACKAGE: &str = "@opencode/ai/providers/openai-compatible";

/// One model of OpenCode 2's provider: its name, Anthropic's cache format on a Claude model, and
/// the limits, capabilities and price the gateway gave.
fn opencode_model(m: &crate::models::Model) -> serde_json::Value {
    use serde_json::{json, Map, Value};
    let l = m.limits;
    let mut entry = Map::new();
    entry.insert("name".into(), m.name.clone().into());
    if m.is_claude() {
        entry.insert("compatibility".into(), json!({"cacheFormat": "anthropic"}));
    }
    let mut limit = Map::new();
    for (key, value) in [
        ("context", l.context),
        ("input", l.input),
        ("output", l.output),
    ] {
        if let Some(v) = value {
            limit.insert(key.into(), v.into());
        }
    }
    if !limit.is_empty() {
        entry.insert("limit".into(), Value::Object(limit));
    }
    let mut caps = Map::new();
    if let Some(tools) = l.tool_call {
        caps.insert("tools".into(), tools.into());
    }
    if let Some(vision) = l.vision {
        let input = match vision {
            true => json!(["text", "image"]),
            false => json!(["text"]),
        };
        caps.insert("input".into(), input);
        caps.insert("output".into(), json!(["text"]));
    }
    if let Some(reasoning) = l.reasoning {
        caps.insert("reasoning".into(), json!({"supported": reasoning}));
    }
    if !caps.is_empty() {
        entry.insert("capabilities".into(), Value::Object(caps));
    }
    if let (Some(input), Some(output)) = (l.input_cost, l.output_cost) {
        entry.insert(
            "cost".into(),
            json!({"input": input.per_mtok(), "output": output.per_mtok()}),
        );
    }
    Value::Object(entry)
}

/// The keys a connection merges into Claude Code's `~/.claude/settings.json`: a `modelPicker`
/// with one row per offered model, Claude's first, in place of the built-in lineup. Empty when
/// nothing is offered.
pub fn claude_settings(offer: &Offer) -> serde_json::Value {
    use serde_json::{json, Map, Value};
    let family = |m: &&crate::models::Model| {
        let id = m.id.to_ascii_lowercase();
        match () {
            _ if !m.is_claude() => 3,
            _ if id.contains("opus") => 0,
            _ if id.contains("sonnet") => 1,
            _ => 2,
        }
    };
    let mut models: Vec<&crate::models::Model> = offer.models.iter().collect();
    models.sort_by_key(family);
    let options: Vec<Value> = (models.into_iter())
        .map(|m| {
            let mut row = Map::new();
            row.insert("model".into(), picker_model(m).into());
            row.insert("label".into(), m.name.clone().into());
            let description = picker_description(m);
            if !description.is_empty() {
                row.insert("description".into(), description.into());
            }
            let known = m.id.to_ascii_lowercase().starts_with("claude-");
            if let (false, Some(sonnet)) = (known, &offer.sonnet) {
                row.insert("behavesAs".into(), sonnet.clone().into());
            }
            Value::Object(row)
        })
        .collect();
    if options.is_empty() {
        return json!({});
    }
    json!({"modelPicker": {"options": options, "replaceBuiltInOptions": true}})
}

/// The model a picker row names: the gateway's id, with `[1m]` on an Opus or Sonnet the gateway
/// gives a context of a million tokens or more.
fn picker_model(m: &crate::models::Model) -> String {
    let id = m.id.to_ascii_lowercase();
    let big = m.limits.context.is_some_and(|c| c >= 1_000_000);
    match big && id.starts_with("claude-") && (id.contains("opus") || id.contains("sonnet")) {
        true => format!("{}[1m]", m.id),
        false => m.id.clone(),
    }
}

/// A picker row's description: where the gateway sends the model, its context and its price,
/// each where the gateway said: `Bedrock EU · 1M ctx · $4.40 / $22.00 per MTok`.
fn picker_description(m: &crate::models::Model) -> String {
    let l = m.limits;
    let mut parts: Vec<String> = m.origin().into_iter().collect();
    if let Some(context) = l.context {
        parts.push(format!("{} ctx", tokens_short(context)));
    }
    if let (Some(input), Some(output)) = (l.input_cost, l.output_cost) {
        parts.push(format!(
            "${:.2} / ${:.2} per MTok",
            input.per_mtok(),
            output.per_mtok()
        ));
    }
    parts.join(" · ")
}

/// A token count in a few characters: `1M`, `1.5M`, `262k`, `800`.
fn tokens_short(n: u64) -> String {
    match n {
        1_000_000.. => {
            let m = format!("{:.1}", n as f64 / 1e6);
            format!("{}M", m.trim_end_matches(".0"))
        }
        1_000.. => format!("{}k", (n as f64 / 1e3).round()),
        _ => n.to_string(),
    }
}

/// Pi's `models.json` for a connection: one OpenAI-compatible provider `litellm-<name>` on the
/// gateway's `/v1`, with the placeholder as its key and the offered models.
pub fn pi_models(c: &Connection, offer: &Offer) -> serde_json::Value {
    use serde_json::{json, Map, Value};
    let models: Vec<Value> = (offer.models.iter())
        .map(|m| {
            let mut entry = Map::new();
            entry.insert("id".into(), m.id.clone().into());
            entry.insert("name".into(), m.name.clone().into());
            let l = m.limits;
            for (key, value) in [("contextWindow", l.context), ("maxTokens", l.output)] {
                if let Some(v) = value {
                    entry.insert(key.into(), v.into());
                }
            }
            if let Some(v) = l.reasoning {
                entry.insert("reasoning".into(), v.into());
            }
            Value::Object(entry)
        })
        .collect();
    let mut providers = Map::new();
    providers.insert(
        provider_id(&c.name),
        json!({
            "name": format!("LiteLLM {}", c.name),
            "api": "openai-completions",
            "baseUrl": format!("{}/v1", base(&c.url)),
            "apiKey": placeholder(&c.name),
            "models": models,
        }),
    );
    json!({ "$schema": PI_MODELS_SCHEMA, "providers": Value::Object(providers) })
}

/// Pi's `settings.json` for a connection: the template plus `defaultProvider`, and
/// `defaultModel` where it is picked.
pub fn pi_settings(c: &Connection, offer: &Offer) -> Result<serde_json::Value> {
    let mut settings: serde_json::Value =
        serde_json::from_str(PI_SETTINGS_TEMPLATE).context("the bundled Pi settings template")?;
    let obj = settings
        .as_object_mut()
        .context("the Pi settings template is not an object")?;
    obj.insert("defaultProvider".into(), provider_id(&c.name).into());
    if let Some(id) = offer.slots.get(crate::models::Slot::PiDefault) {
        obj.insert("defaultModel".into(), id.into());
    }
    Ok(settings)
}

/// One config file a connection writes for its agent: its path in the sandbox and its content.
/// `merge`: the content is keys merged into the file there (Claude Code's `settings.json`, which
/// the kits fill too), not the whole file.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigFile {
    pub path: String,
    pub value: serde_json::Value,
    pub merge: bool,
}

impl ConfigFile {
    /// The file's name, which also names its edited copy.
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// The config files a connection writes for its agent, as generated: OpenCode's config (which
/// `OPENCODE_CONFIG` names), Pi's two files (Pi reads `~/.pi/agent`, Pi 1.4.0
/// docs/configuration.md) and Claude Code's model picker, merged into its `settings.json`.
pub fn config_files(c: &Connection, agent: Agent, offer: &Offer) -> Result<Vec<ConfigFile>> {
    let whole = |path: String, value| ConfigFile {
        path,
        value,
        merge: false,
    };
    Ok(match agent {
        Agent::Claude => vec![ConfigFile {
            path: CLAUDE_SETTINGS_PATH.to_string(),
            value: claude_settings(offer),
            merge: true,
        }],
        Agent::Opencode => vec![whole(
            opencode_config_path(&c.name),
            opencode_config(c, offer)?,
        )],
        Agent::Pi => vec![
            whole(PI_MODELS_PATH.to_string(), pi_models(c, offer)),
            whole(PI_SETTINGS_PATH.to_string(), pi_settings(c, offer)?),
        ],
    })
}

/// The names of the config files a connection writes for `agent`, in [`config_files`]' order.
pub fn config_file_names(name: &str, agent: Agent) -> Vec<String> {
    let base = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
    match agent {
        Agent::Claude => vec![base(CLAUDE_SETTINGS_PATH)],
        Agent::Opencode => vec![base(&opencode_config_path(name))],
        Agent::Pi => vec![base(PI_MODELS_PATH), base(PI_SETTINGS_PATH)],
    }
}

/// The whole files a connection's kit writes for its agent, as generated: what
/// [`config_files`] does not merge.
pub fn agent_files(
    c: &Connection,
    agent: Agent,
    offer: &Offer,
) -> Result<Vec<(String, serde_json::Value)>> {
    Ok(whole_files(config_files(c, agent, offer)?))
}

fn whole_files(files: Vec<ConfigFile>) -> Vec<(String, serde_json::Value)> {
    (files.into_iter())
        .filter(|f| !f.merge)
        .map(|f| (f.path, f.value))
        .collect()
}

/// [`config_files`] with the offer `root` holds for the connection, and the user's edited copies
/// in place of the generated files ([`effective`]).
pub fn config_files_in(root: &Path, c: &Connection, agent: Agent) -> Result<Vec<ConfigFile>> {
    let files = config_files(c, agent, &Offer::read(root, &c.name, agent))?;
    Ok(effective(root, &c.name, agent, files))
}

/// [`agent_files`] with the offer and edited copies `root` holds for the connection.
pub fn agent_files_in(
    root: &Path,
    c: &Connection,
    agent: Agent,
) -> Result<Vec<(String, serde_json::Value)>> {
    Ok(whole_files(config_files_in(root, c, agent)?))
}

/// The pre-start step that writes a connection's agent files into a sandbox
/// ([`crate::sbx::write_files_argv`]) and merges Claude Code's keys into its settings
/// ([`crate::sbx::merge_settings_argv`]): the list, picks and edited copies `root` holds now.
/// `None` when there is nothing to write.
pub fn push_argv_in(
    root: &Path,
    sandbox: &str,
    c: &Connection,
    agent: Agent,
) -> Option<Vec<String>> {
    push_argv(sandbox, &config_files_in(root, c, agent).ok()?)
}

/// [`push_argv_in`] for a set of files.
pub fn push_argv(sandbox: &str, files: &[ConfigFile]) -> Option<Vec<String>> {
    let merged: Vec<&ConfigFile> = (files.iter())
        .filter(|f| f.merge && f.value.as_object().is_some_and(|o| !o.is_empty()))
        .collect();
    if let Some(f) = merged.first() {
        return Some(crate::sbx::merge_settings_argv(
            sandbox,
            &f.path,
            &serde_json::to_string(&f.value).ok()?,
        ));
    }
    let whole: Vec<(String, String)> = (files.iter())
        .filter(|f| !f.merge)
        .filter_map(|f| Some((f.path.clone(), file_text(&f.value).ok()?)))
        .collect();
    (!whole.is_empty()).then(|| crate::sbx::write_files_argv(sandbox, &whole))
}

/// Claude Code's user settings in the sandbox, which honour `modelPicker`.
pub const CLAUDE_SETTINGS_PATH: &str = "/home/agent/.claude/settings.json";

// --- the user's edited copies ----------------------------------------------------------------

/// Where the edited copies of a connection's files for one agent live:
/// `<root>/<name>/edited-<agent>/`. One copy serves every sandbox of that harness on the
/// connection.
pub fn edits_dir(root: &Path, name: &str, agent: Agent) -> PathBuf {
    root.join(name).join(format!("edited-{}", agent.as_str()))
}

/// A file's copy, and the generated text it was made from: the copy counts as edited only while
/// it differs from that text.
fn edit_paths(root: &Path, name: &str, agent: Agent, file: &str) -> (PathBuf, PathBuf) {
    let dir = edits_dir(root, name, agent);
    (dir.join(file), dir.join(format!(".{file}.generated")))
}

/// What a config file is: as generated, the user's edited copy, or an edited copy that does
/// not read as a JSON object, which is left out for the generated file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    Generated,
    Edited,
    Invalid(String),
}

/// What `file` is for the connection and agent, and the edited copy's content where it counts.
fn edited(
    root: &Path,
    name: &str,
    agent: Agent,
    file: &str,
) -> (FileState, Option<serde_json::Value>) {
    let (copy, base) = edit_paths(root, name, agent, file);
    let Ok(text) = fs::read_to_string(&copy) else {
        return (FileState::Generated, None);
    };
    if fs::read_to_string(&base).is_ok_and(|b| b == text) {
        return (FileState::Generated, None);
    }
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) if value.is_object() => (FileState::Edited, Some(value)),
        Ok(_) => (FileState::Invalid("not a JSON object".into()), None),
        Err(e) => (FileState::Invalid(e.to_string()), None),
    }
}

/// The state of a config file's edited copy ([`FileState`]).
pub fn file_state(root: &Path, name: &str, agent: Agent, file: &str) -> FileState {
    edited(root, name, agent, file).0
}

/// `files` with each edited copy in place of the generated content.
pub fn effective(root: &Path, name: &str, agent: Agent, files: Vec<ConfigFile>) -> Vec<ConfigFile> {
    (files.into_iter())
        .map(|mut f| {
            if let (_, Some(value)) = edited(root, name, agent, f.file_name()) {
                f.value = value;
            }
            f
        })
        .collect()
}

/// The file to open in an editor for `file`: its edited copy, written from the generated content
/// first unless the user has edited it.
pub fn prepare_edit(root: &Path, name: &str, agent: Agent, file: &ConfigFile) -> Result<PathBuf> {
    let (copy, base) = edit_paths(root, name, agent, file.file_name());
    if edited(root, name, agent, file.file_name()).0 != FileState::Generated {
        return Ok(copy);
    }
    let dir = edits_dir(root, name, agent);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let text = file_text(&file.value)?;
    fs::write(&base, &text).with_context(|| format!("writing {}", base.display()))?;
    fs::write(&copy, &text).with_context(|| format!("writing {}", copy.display()))?;
    Ok(copy)
}

/// Drop a file's edited copy: the generated file counts again.
pub fn reset_edit(root: &Path, name: &str, agent: Agent, file: &str) -> Result<()> {
    let (copy, base) = edit_paths(root, name, agent, file);
    for path in [copy, base] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
        }
    }
    Ok(())
}

/// Pi's model list in the sandbox.
pub const PI_MODELS_PATH: &str = "/home/agent/.pi/agent/models.json";
/// Pi's settings in the sandbox.
pub const PI_SETTINGS_PATH: &str = "/home/agent/.pi/agent/settings.json";

/// A file's text as the agent reads it: pretty JSON and a final line break.
pub fn file_text(value: &serde_json::Value) -> Result<String> {
    Ok(format!("{}\n", serde_json::to_string_pretty(value)?))
}

/// A JSON file's text as the YAML string of a lifecycle file's `content`: a JSON string literal,
/// which YAML reads as a double-quoted scalar. sbx expands `${{ ... }}` there at create, so `${{`
/// is written as `${{`, which JSON reads back unchanged.
fn file_content(value: &serde_json::Value) -> Result<String> {
    let text = serde_json::to_string_pretty(value)?.replace("${{", "$\\u007b{");
    Ok(serde_json::to_string(&format!("{text}\n"))?)
}

/// The kit's descriptor: a v3 mixin whose content is its image ENV (an inline `build:`), its
/// egress and - for OpenCode and Pi - the agent's config as lifecycle files. No hook: the host
/// fetches the model list ([`crate::model_fetch`]) and writes the files again before every session.
/// Every value in it was checked by [`validate`] (name, URL) or is fixed; ENV values are
/// double-quoted. The files' content is JSON written by serde_json ([`file_content`]).
fn descriptor(
    c: &Connection,
    agent: Agent,
    agent_files: &[(String, serde_json::Value)],
) -> Result<String> {
    let (name, url) = (c.name.as_str(), base(&c.url));
    let host = host(url);
    let ph = placeholder(name);
    let mut env = format!("  ENV {}=\"1\"\n", marker(name, agent));
    match agent {
        Agent::Claude => {
            env.push_str(&format!("  ENV ANTHROPIC_BASE_URL=\"{url}\"\n"));
            env.push_str(&format!("  ENV ANTHROPIC_AUTH_TOKEN=\"{ph}\"\n"));
            env.push_str(&format!("  ENV AISBX_LITELLM_URL=\"{url}\"\n"));
        }
        Agent::Opencode => {
            env.push_str(&format!(
                "  ENV OPENCODE_CONFIG=\"{}\"\n",
                opencode_config_path(name)
            ));
        }
        // Pi reads its gateway from the files below, not from the environment.
        Agent::Pi => {}
    }
    let mut files = String::new();
    for (path, value) in agent_files {
        files.push_str(&format!(
            "\x20       - path: \"{path}\"\n\
             \x20         content: {}\n",
            file_content(value)?
        ));
    }
    if !files.is_empty() {
        files = format!(
            "\x20 - type: com.docker.sandbox/lifecycle@1\n\
             \x20   config:\n\
             \x20     # Written at creation; the dashboard writes them again before each session.\n\
             \x20     files:\n{files}"
        );
    }
    Ok(format!(
        "# syntax=docker/sandbox-kit:3\n\
         # Generated by aisbx — do not edit; rewritten whenever a sandbox is built on this connection.\n\
         #\n\
         # Points a sandbox at the LiteLLM connection \"{name}\" ({url}). The bearer below is only a\n\
         # placeholder: the key stays in the host keychain and the sbx proxy swaps it in on requests\n\
         # to {host}. Storing the key again rotates it for every sandbox on this connection.\n\
         #\n\
         #   sbx secret set-custom --host {host} --placeholder {ph}\n\
         schemaVersion: \"3\"\n\
         kind: mixin\n\
         displayName: \"LiteLLM connection: {name}\"\n\
         description: \"LiteLLM gateway {host}, connection {name}.\"\n\
         \n\
         build: |\n\
         \x20 FROM scratch\n\
         {env}\
         \n\
         capabilities:\n\
         \x20 - type: com.docker.sandbox/network-policy@1\n\
         \x20   config:\n\
         \x20     runtime:\n\
         \x20       allow:\n\
         \x20         - \"{host}:443\"\n\
         {files}"
    ))
}

/// The harvested model list of a connection: `<dir>/<name>/models.json`.
pub fn harvest_file(name: &str) -> PathBuf {
    dir().join(name).join("models.json")
}

/// How many models a sandbox on the connection read from the gateway last — `0` before one has,
/// and always under `cargo test`, which reads no state folder.
pub fn harvested_models(name: &str) -> usize {
    if cfg!(any(test, feature = "test-support")) {
        return 0;
    }
    crate::models::catalog_for(Some(&harvest_file(name)))
        .0
        .len()
}

/// What the create form's Connection row holds. Not `create::ConnChoice`, which is the wizard's
/// account radio on the subscription backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LitellmPick {
    Existing(String),
    New,
}

/// The form's first pick: the remembered connection if it still exists, else the only one, else
/// the first by name; with none, *new connection…*. `all` is sorted by name.
pub fn preselect(all: &[Connection], last: Option<&str>) -> LitellmPick {
    if let Some(last) = last.filter(|l| all.iter().any(|c| c.name == *l)) {
        return LitellmPick::Existing(last.to_string());
    }
    match all.first() {
        Some(c) => LitellmPick::Existing(c.name.clone()),
        None => LitellmPick::New,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(name: &str, url: &str) -> Connection {
        Connection {
            name: name.into(),
            url: url.into(),
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aisbx-litellm-{tag}-{}-{}",
            std::process::id(),
            crate::state::now_epoch()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn urls_are_checked_as_r9_says() {
        for bad in [
            "http://x",
            "https://h:4000",
            "https://h;rm",
            "https://h/a b",
            "https://",
            "https://h:",
            "ftp://h",
            "https://h/a'b",
            "https://h/a\"b",
        ] {
            assert!(check_url(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(check_url("https://h:4000")
            .unwrap_err()
            .to_string()
            .contains("only port 443 for now"));
        for good in [
            "https://h",
            "https://h:443/litellm",
            "https://litellm.example.com",
        ] {
            assert!(check_url(good).is_ok(), "{good:?} must be accepted");
        }
        assert_eq!(host("https://h:443/litellm"), "h");
        assert_eq!(host("https://litellm.example.com/"), "litellm.example.com");
    }

    #[test]
    fn names_and_model_ids_are_checked() {
        for bad in ["Name_1", "a\"b", "", "-a", "A", &"a".repeat(33)] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        for good in ["acme", "a", "0-x", &"a".repeat(32)] {
            assert!(valid_name(good), "{good:?}");
        }
        assert!(valid_model_id("anthropic/claude-sonnet:4@v1+x.y_z"));
        assert!(!valid_model_id("a b"));
        assert!(!valid_model_id("a\"b"));
        assert!(!valid_model_id(""));
        assert!(!valid_model_id(&"m".repeat(129)));
        assert!(validate(&conn("acme", "https://litellm.example.com")).is_ok());
        assert!(validate(&conn("Name_1", "https://h")).is_err());
        assert!(validate(&conn("acme", "http://x")).is_err());
    }

    #[test]
    fn a_placeholder_names_its_connection() {
        assert_eq!(placeholder("acme"), "sbx-cs-aisbx-llm-acme");
        assert_eq!(name_of_placeholder("sbx-cs-aisbx-llm-acme"), Some("acme"));
        assert_eq!(name_of_placeholder("sbx-cs-aisbx-llm-"), None);
        assert_eq!(name_of_placeholder("sbx-cs-aisbx-tok-acme"), None);
    }

    #[test]
    fn a_kit_name_carries_connection_and_agent() {
        assert_eq!(kit_name("acme", Agent::Claude), "litellm-acme-claude");
        assert_eq!(of_kit("litellm-acme-claude"), Some(("acme", Agent::Claude)));
        assert_eq!(
            of_kit("litellm-my-gw-opencode"),
            Some(("my-gw", Agent::Opencode))
        );
        assert_eq!(of_kit("litellm-acme-pi"), Some(("acme", Agent::Pi)));
        assert_eq!(of_kit("litellm-acme-gemini"), None);
        assert_eq!(of_kit("litellm--claude"), None);
        assert_eq!(of_kit("claude-litellm"), None);
        assert_eq!(of_kit("token-acme"), None);
        assert_eq!(
            marker("my-gw", Agent::Opencode),
            "SBX_KIT_LITELLM_MY_GW_OPENCODE"
        );
        assert_eq!(Agent::of_backend("claude-litellm"), Some(Agent::Claude));
        assert_eq!(Agent::of_backend("opencode-litellm"), Some(Agent::Opencode));
        assert_eq!(Agent::of_backend("pi-litellm"), Some(Agent::Pi));
        assert_eq!(Agent::parse("pi"), Some(Agent::Pi));
        assert_eq!(Agent::Pi.as_str(), "pi");
        assert_eq!(Agent::of_backend("claude-anthropic"), None);
    }

    /// A generated descriptor, parsed the way the catalog parses a kit's: the document, and its
    /// capabilities by type.
    fn parsed(c: &Connection, agent: Agent) -> (String, yaml_rust2::Yaml) {
        let files = agent_files(c, agent, &offer()).unwrap();
        let text = descriptor(c, agent, &files).unwrap();
        let doc = crate::kits::load_descriptor(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        (text, doc)
    }

    fn capability(doc: &yaml_rust2::Yaml, kind: &str) -> crate::kits::Capability {
        crate::kits::capabilities(doc)
            .into_iter()
            .find(|c| c.kind == kind)
            .unwrap_or_else(|| panic!("no {kind}"))
    }

    fn strings(v: &yaml_rust2::Yaml) -> Vec<String> {
        v.as_vec()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect()
    }

    /// The lifecycle files a descriptor writes, as (path, parsed JSON content).
    fn files(doc: &yaml_rust2::Yaml) -> Vec<(String, serde_json::Value)> {
        let life = capability(doc, "lifecycle@1");
        life.config["files"]
            .as_vec()
            .into_iter()
            .flatten()
            .map(|f| {
                (
                    f["path"].as_str().unwrap().to_string(),
                    serde_json::from_str(f["content"].as_str().unwrap()).unwrap(),
                )
            })
            .collect()
    }

    /// Two models with limits, the second switched off, and the first picked for every slot.
    fn offer() -> Offer {
        use crate::models::{Limits, Model, Slot};
        let models = vec![
            Model {
                id: "gpt-x".into(),
                name: "GPT X".into(),
                limits: Limits {
                    context: Some(200_000),
                    input: Some(200_000),
                    output: Some(8_000),
                    reasoning: Some(true),
                    tool_call: Some(true),
                    ..Default::default()
                },
                ..Default::default()
            },
            Model {
                id: "mini".into(),
                name: "mini".into(),
                ..Default::default()
            },
        ];
        let mut slots = crate::models::Overrides::default();
        slots.set(Slot::Main, Some("gpt-x".into()));
        slots.set(Slot::Small, Some("mini".into()));
        slots.set(Slot::PiDefault, Some("gpt-x".into()));
        Offer {
            models,
            slots,
            sonnet: None,
        }
    }

    /// A model as a gateway in front of Bedrock describes it.
    fn gateway_model(
        id: &str,
        upstream: Option<&str>,
        vision: Option<bool>,
    ) -> crate::models::Model {
        use crate::models::{Limits, Model, Price};
        Model {
            id: id.into(),
            name: id.into(),
            limits: Limits {
                context: Some(1_000_000),
                output: Some(128_000),
                reasoning: Some(true),
                tool_call: Some(true),
                vision,
                input_cost: Some(Price(4_400_000)),
                output_cost: Some(Price(22_000_000)),
                ..Default::default()
            },
            provider: upstream.map(|_| "bedrock_converse".into()),
            upstream: upstream.map(str::to_string),
        }
    }

    /// Every connection kit: a v3 mixin with the host reachable while it runs, no hook (the host
    /// fetches the list), and no credential.
    fn assert_common(text: &str, doc: &yaml_rust2::Yaml, agent: Agent) {
        assert_eq!(doc["schemaVersion"].as_str(), Some("3"), "{text}");
        assert_eq!(doc["kind"].as_str(), Some("mixin"));
        assert!(
            text.starts_with("# syntax=docker/sandbox-kit:3\n"),
            "{text}"
        );
        let build = doc["build"].as_str().expect("an inline build");
        assert!(build.starts_with("FROM scratch\n"), "{build}");
        assert!(
            build.contains(&format!(
                "ENV SBX_KIT_LITELLM_ACME_{}=\"1\"",
                agent.as_str().to_ascii_uppercase()
            )),
            "{build}"
        );
        let net = capability(doc, "network-policy@1");
        assert_eq!(
            strings(&net.config["runtime"]["allow"]),
            ["litellm.example.com:443"]
        );
        assert!(
            net.config["install"].is_badvalue(),
            "no install hook to allow"
        );
        let hooks = crate::kits::capabilities(doc)
            .into_iter()
            .find(|c| c.kind == "lifecycle@1");
        if let Some(life) = hooks {
            assert!(life.config["install"].is_badvalue(), "{text}");
            assert!(life.config["startup"].is_badvalue(), "{text}");
        }
        assert!(!text.contains("litellm-models.sh"), "{text}");
        assert!(
            crate::kits::capabilities(doc)
                .iter()
                .all(|c| c.kind != "credential@1"),
            "the key is a custom secret, not a kit credential"
        );
    }

    #[test]
    fn the_claude_kit_points_at_the_url_with_the_placeholder() {
        let (text, doc) = parsed(&conn("acme", "https://litellm.example.com/"), Agent::Claude);
        assert_common(&text, &doc, Agent::Claude);
        let build = doc["build"].as_str().unwrap();
        for want in [
            "ENV ANTHROPIC_BASE_URL=\"https://litellm.example.com\"",
            "ENV ANTHROPIC_AUTH_TOKEN=\"sbx-cs-aisbx-llm-acme\"",
            "ENV AISBX_LITELLM_URL=\"https://litellm.example.com\"",
        ] {
            assert!(build.contains(want), "{want:?} missing in\n{build}");
        }
        assert!(!text.contains("OPENCODE_CONFIG"), "{text}");
        assert!(
            !text.contains("lifecycle@1"),
            "Claude Code reads its environment: no files, no hooks\n{text}"
        );
    }

    #[test]
    fn the_opencode_kit_carries_its_config_from_the_template() {
        let c = conn("acme", "https://litellm.example.com");
        let (text, doc) = parsed(&c, Agent::Opencode);
        assert_common(&text, &doc, Agent::Opencode);
        assert!(doc["build"].as_str().unwrap().contains(
            "ENV OPENCODE_CONFIG=\"/home/agent/.config/opencode/opencode-litellm-acme.json\""
        ));
        assert!(!text.contains("ANTHROPIC_"), "{text}");
        let config = opencode_config(&c, &offer()).unwrap();
        assert_eq!(
            files(&doc),
            [(
                "/home/agent/.config/opencode/opencode-litellm-acme.json".to_string(),
                config.clone()
            )]
        );
        assert_eq!(config["share"], "disabled");
        assert_eq!(config["update"], "disable");
        assert_eq!(config["default_agent"], "plan");
        assert_eq!(config["model"], "litellm-acme/gpt-x");
        assert_eq!(config["agents"]["title"]["model"], "litellm-acme/mini");
        for v1 in ["provider", "small_model", "agent", "mode"] {
            assert!(config.get(v1).is_none(), "OpenCode 2's keys only: {v1}");
        }
        let p = &config["providers"]["litellm-acme"];
        assert_eq!(p["package"], "@opencode/ai/providers/openai-compatible");
        assert_eq!(p["settings"]["baseURL"], "https://litellm.example.com/v1");
        assert_eq!(p["settings"]["apiKey"], "sbx-cs-aisbx-llm-acme");
        assert_eq!(
            p["models"],
            serde_json::json!({
                "gpt-x": {"name": "GPT X",
                          "capabilities": {"tools": true, "reasoning": {"supported": true}},
                          "limit": {"context": 200000, "input": 200000, "output": 8000}},
                "mini": {"name": "mini"},
            })
        );
        // Nothing picked: OpenCode picks from the list.
        let bare = opencode_config(&c, &Offer::default()).unwrap();
        assert!(bare.get("model").is_none() && bare.get("agents").is_none());
        assert_eq!(
            bare["providers"]["litellm-acme"]["models"],
            serde_json::json!({})
        );
        assert!(config["policies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["resource"] == "github-copilot"));
        // The template itself names no provider and no model.
        let template: serde_json::Value = serde_json::from_str(OPENCODE_TEMPLATE).unwrap();
        assert!(template.get("provider").is_none() && template.get("model").is_none());
    }

    #[test]
    fn a_claude_model_caches_the_anthropic_way_and_every_model_says_what_it_takes() {
        let offer = Offer {
            models: vec![
                gateway_model(
                    "claude-opus-5-5",
                    Some("eu.anthropic.claude-opus-5-5"),
                    Some(true),
                ),
                gateway_model(
                    "02-top-(sonnet)",
                    Some("eu.anthropic.claude-sonnet-5"),
                    None,
                ),
                gateway_model("glm-5.3", None, Some(false)),
            ],
            ..Offer::default()
        };
        let config = opencode_config(&conn("acme", "https://h"), &offer).unwrap();
        let models = &config["providers"]["litellm-acme"]["models"];
        assert_eq!(
            models["claude-opus-5-5"],
            serde_json::json!({
                "name": "claude-opus-5-5",
                "compatibility": {"cacheFormat": "anthropic"},
                "limit": {"context": 1000000, "output": 128000},
                "capabilities": {"tools": true, "input": ["text", "image"], "output": ["text"],
                                 "reasoning": {"supported": true}},
                "cost": {"input": 4.4, "output": 22.0},
            })
        );
        assert_eq!(
            models["02-top-(sonnet)"]["compatibility"]["cacheFormat"], "anthropic",
            "an alias of a Claude model is Claude"
        );
        assert!(
            models["02-top-(sonnet)"]["capabilities"]
                .get("input")
                .is_none(),
            "no word on images: OpenCode decides"
        );
        assert!(models["glm-5.3"].get("compatibility").is_none());
        assert_eq!(
            models["glm-5.3"]["capabilities"]["input"],
            serde_json::json!(["text"])
        );
    }

    #[test]
    fn the_newest_sonnet_is_the_highest_version() {
        let ids = |ids: &[&str]| -> Vec<crate::models::Model> {
            (ids.iter())
                .map(|id| gateway_model(id, None, None))
                .collect()
        };
        let newest = |list: &[&str]| newest_sonnet(&ids(list)).map(str::to_string);
        assert_eq!(
            newest(&[
                "claude-sonnet-4.6",
                "claude-sonnet-5-5",
                "claude-sonnet-5",
                "claude-sonnet-4-5-20250929",
                "02-high-(sonnet-9-9)",
                "claude-opus-6",
            ]),
            Some("claude-sonnet-5-5".into()),
            "Claude Code knows the claude-sonnet ids; a date is no version"
        );
        assert_eq!(
            newest(&["02-high-(sonnet-4-6)", "sonnet-5"]),
            Some("sonnet-5".into()),
            "with no claude-sonnet id, any id that names Sonnet"
        );
        assert_eq!(newest(&["glm-5.3", "claude-opus-5"]), None);
    }

    #[test]
    fn claude_codes_picker_lists_the_offered_models_claude_first() {
        let offer = Offer {
            models: vec![
                gateway_model("glm-5.3", None, None),
                gateway_model(
                    "claude-haiku-5-5",
                    Some("eu.anthropic.claude-haiku-5-5"),
                    None,
                ),
                gateway_model(
                    "claude-sonnet-5-5",
                    Some("eu.anthropic.claude-sonnet-5-5"),
                    None,
                ),
                gateway_model(
                    "claude-opus-5-5",
                    Some("eu.anthropic.claude-opus-5-5"),
                    None,
                ),
            ],
            sonnet: Some("claude-sonnet-5-5".into()),
            ..Offer::default()
        };
        let settings = claude_settings(&offer);
        assert_eq!(settings["modelPicker"]["replaceBuiltInOptions"], true);
        let rows = settings["modelPicker"]["options"].as_array().unwrap();
        let models: Vec<&str> = rows.iter().map(|r| r["model"].as_str().unwrap()).collect();
        assert_eq!(
            models,
            [
                "claude-opus-5-5[1m]",
                "claude-sonnet-5-5[1m]",
                "claude-haiku-5-5",
                "glm-5.3"
            ]
        );
        assert_eq!(
            rows[0],
            serde_json::json!({
                "model": "claude-opus-5-5[1m]",
                "label": "claude-opus-5-5",
                "description": "Bedrock EU · 1M ctx · $4.40 / $22.00 per MTok",
            })
        );
        assert_eq!(rows[3]["behavesAs"], "claude-sonnet-5-5");
        assert_eq!(rows[3]["description"], "1M ctx · $4.40 / $22.00 per MTok");
        assert!(rows[..3].iter().all(|r| r.get("behavesAs").is_none()));
        assert_eq!(claude_settings(&Offer::default()), serde_json::json!({}));
        assert_eq!(tokens_short(1_048_576), "1M");
        assert_eq!(tokens_short(1_500_000), "1.5M");
        assert_eq!(tokens_short(262_144), "262k");
        assert_eq!(tokens_short(800), "800");
    }

    #[test]
    fn claude_codes_picker_is_merged_before_each_session_not_baked_into_the_kit() {
        let c = conn("acme", "https://litellm.example.com");
        let offer = Offer {
            models: vec![gateway_model("glm-5.3", None, None)],
            ..Offer::default()
        };
        let files = config_files(&c, Agent::Claude, &offer).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].merge);
        assert_eq!(files[0].path, "/home/agent/.claude/settings.json");
        assert!(agent_files(&c, Agent::Claude, &offer).unwrap().is_empty());
        let argv = push_argv("box", &files).unwrap();
        assert_eq!(&argv[..3], ["sbx", "exec", "box"]);
        assert_eq!(argv[7], "/home/agent/.claude/settings.json");
        let sent: serde_json::Value = serde_json::from_str(&argv[8..].concat()).unwrap();
        assert_eq!(sent, files[0].value);
        // Nothing offered: nothing to merge.
        let empty = config_files(&c, Agent::Claude, &Offer::default()).unwrap();
        assert_eq!(push_argv("box", &empty), None);
    }

    #[test]
    fn the_file_names_are_the_files_names() {
        let c = conn("acme", "https://h");
        for agent in [Agent::Claude, Agent::Opencode, Agent::Pi] {
            let files = config_files(&c, agent, &offer()).unwrap();
            let names: Vec<&str> = files.iter().map(ConfigFile::file_name).collect();
            assert_eq!(config_file_names("acme", agent), names, "{agent:?}");
        }
    }

    #[test]
    fn an_edited_copy_replaces_the_generated_file_until_it_is_reset() {
        let root = scratch("edit");
        let c = conn("acme", "https://litellm.example.com");
        save_in(&root, &c).unwrap();
        let generated = config_files(&c, Agent::Opencode, &offer()).unwrap();
        let file = &generated[0];
        let name = file.file_name().to_string();
        assert_eq!(name, "opencode-litellm-acme.json");
        assert_eq!(
            file_state(&root, "acme", Agent::Opencode, &name),
            FileState::Generated
        );
        let path = prepare_edit(&root, "acme", Agent::Opencode, file).unwrap();
        assert!(path.ends_with("acme/edited-opencode/opencode-litellm-acme.json"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&path).unwrap()).unwrap(),
            file.value,
            "the copy starts as the generated file"
        );
        assert_eq!(
            file_state(&root, "acme", Agent::Opencode, &name),
            FileState::Generated,
            "an unchanged copy is no edit"
        );
        assert_eq!(
            effective(&root, "acme", Agent::Opencode, generated.clone()),
            generated
        );

        fs::write(&path, r#"{"model": "mine"}"#).unwrap();
        assert_eq!(
            file_state(&root, "acme", Agent::Opencode, &name),
            FileState::Edited
        );
        let used = effective(&root, "acme", Agent::Opencode, generated.clone());
        assert_eq!(used[0].value, serde_json::json!({"model": "mine"}));
        assert_eq!(used[0].path, file.path);
        // The kit and the push carry the copy.
        let kit = ensure_kit_in(&root, &c, Agent::Opencode).unwrap();
        let text = fs::read_to_string(crate::kits::descriptor_path(&kit)).unwrap();
        assert!(text.contains("mine"), "{text}");
        // Another harness on the connection keeps its own files.
        assert_eq!(
            file_state(&root, "acme", Agent::Pi, &name),
            FileState::Generated
        );
        // Opening it again keeps the edit.
        prepare_edit(&root, "acme", Agent::Opencode, file).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"model": "mine"}"#);

        fs::write(&path, "{ broken").unwrap();
        assert!(matches!(
            file_state(&root, "acme", Agent::Opencode, &name),
            FileState::Invalid(_)
        ));
        assert_eq!(
            effective(&root, "acme", Agent::Opencode, generated.clone()),
            generated,
            "a copy that does not parse is left out"
        );
        reset_edit(&root, "acme", Agent::Opencode, &name).unwrap();
        assert!(!path.exists());
        assert_eq!(
            file_state(&root, "acme", Agent::Opencode, &name),
            FileState::Generated
        );
        reset_edit(&root, "acme", Agent::Opencode, &name).unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_files_content_is_never_a_kit_expression() {
        let value = serde_json::json!({"name": "odd ${{ kit.env.HOME }} \"quoted\" \\ name"});
        let content = file_content(&value).unwrap();
        assert!(!content.contains("${{"), "{content}");
        let text: String = serde_json::from_str(&content).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            value
        );
    }

    #[test]
    fn the_pi_kit_carries_models_and_settings_with_the_placeholder() {
        let c = conn("acme", "https://litellm.example.com/");
        let (text, doc) = parsed(&c, Agent::Pi);
        assert_common(&text, &doc, Agent::Pi);
        // No agent env beyond the marker, no install env.
        for unwanted in ["ANTHROPIC_", "OPENCODE_CONFIG", "export PI"] {
            assert!(!text.contains(unwanted), "{unwanted:?} in\n{text}");
        }
        assert_eq!(doc["build"].as_str().unwrap().matches("ENV ").count(), 1);

        let models = pi_models(&c, &offer());
        assert_eq!(
            models["$schema"],
            "https://pi.dev/schemas/models.schema.json"
        );
        let providers = models["providers"].as_object().unwrap();
        assert_eq!(providers.len(), 1, "{models}");
        let p = &providers["litellm-acme"];
        assert_eq!(p["name"], "LiteLLM acme");
        assert_eq!(p["api"], "openai-completions");
        assert_eq!(p["baseUrl"], "https://litellm.example.com/v1");
        assert_eq!(p["apiKey"], "sbx-cs-aisbx-llm-acme");
        assert!(
            p.get("headers").is_none() && p.get("oauth").is_none(),
            "{p}"
        );
        assert_eq!(
            p["models"],
            serde_json::json!([
                {"id": "gpt-x", "name": "GPT X", "contextWindow": 200000, "maxTokens": 8000,
                 "reasoning": true},
                {"id": "mini", "name": "mini"},
            ])
        );

        let settings = pi_settings(&c, &offer()).unwrap();
        assert_eq!(
            settings["$schema"],
            "https://pi.dev/schemas/settings.schema.json"
        );
        assert_eq!(settings["enableInstallTelemetry"], false);
        assert_eq!(settings["defaultProvider"], "litellm-acme");
        assert_eq!(settings["defaultModel"], "gpt-x");
        let bare = pi_settings(&c, &Offer::default()).unwrap();
        assert!(
            bare.get("defaultModel").is_none(),
            "nothing picked: Pi picks"
        );
        // The template itself names no provider and no model.
        let template: serde_json::Value = serde_json::from_str(PI_SETTINGS_TEMPLATE).unwrap();
        assert!(
            template.get("defaultProvider").is_none() && template.get("defaultModel").is_none()
        );
        assert_eq!(
            files(&doc),
            [
                ("/home/agent/.pi/agent/models.json".to_string(), models),
                ("/home/agent/.pi/agent/settings.json".to_string(), settings),
            ]
        );

        let root = scratch("pi");
        let kit = ensure_kit_in(&root, &c, Agent::Pi).unwrap();
        assert!(kit.ends_with("acme/litellm-acme-pi"));
        assert!(kit.join("litellm-acme-pi.yaml").is_file());
        let read = crate::kits::Kit::read(&kit).expect("the generated kit parses");
        assert_eq!(read.name, "litellm-acme-pi");
        assert!(read.credentials.is_empty());
        assert!(!read.is_workload);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn connections_round_trip_through_their_folder() {
        let root = scratch("rt");
        let c = conn("acme", "https://litellm.example.com");
        assert!(!exists_in(&root, "acme"));
        assert!(load_all_in(&root).is_empty());
        save_in(&root, &c).unwrap();
        assert!(exists_in(&root, "acme"));
        assert_eq!(load_in(&root, "acme"), Some(c.clone()));
        save_in(&root, &conn("beta", "https://h")).unwrap();
        let names: Vec<String> = load_all_in(&root).into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["acme", "beta"]);
        // An invalid connection is never written.
        assert!(save_in(&root, &conn("Bad_Name", "https://h")).is_err());
        let text = fs::read_to_string(root.join("acme/connection.json")).unwrap();
        assert!(!text.contains("key"), "{text}");
        let kit = ensure_kit_in(&root, &c, Agent::Opencode).unwrap();
        assert!(kit.ends_with("acme/litellm-acme-opencode"));
        assert!(
            kit.join("litellm-acme-opencode.yaml").is_file(),
            "its descriptor"
        );
        let read = crate::kits::Kit::read(&kit).expect("the generated kit parses");
        assert_eq!(read.name, "litellm-acme-opencode");
        assert!(read.credentials.is_empty());
        remove_in(&root, "acme").unwrap();
        assert!(!exists_in(&root, "acme"));
        remove_in(&root, "acme").unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_form_opens_on_the_last_used_then_the_only_then_the_first() {
        let two = [conn("acme", "https://h"), conn("beta", "https://h")];
        assert_eq!(
            preselect(&two, Some("beta")),
            LitellmPick::Existing("beta".into())
        );
        // A remembered connection that is gone is ignored.
        assert_eq!(
            preselect(&two, Some("gone")),
            LitellmPick::Existing("acme".into())
        );
        assert_eq!(
            preselect(std::slice::from_ref(&two[1]), None),
            LitellmPick::Existing("beta".into())
        );
        assert_eq!(preselect(&two, None), LitellmPick::Existing("acme".into()));
        assert_eq!(preselect(&[], Some("acme")), LitellmPick::New);
    }

    #[test]
    fn only_another_host_moves_a_connection() {
        let old = conn("acme", "https://litellm.example.com");
        let moved = |url: &str| moves_host(&old, &conn("acme", url));
        assert!(moved("https://gw.example.org"));
        assert!(moved("https://sub.litellm.example.com"));
        assert!(!moved("https://litellm.example.com/v2"));
        assert!(!moved("https://litellm.example.com:443/litellm"));
        assert!(!moved("https://litellm.example.com"));
    }

    #[test]
    fn a_connection_written_with_models_still_loads() {
        let root = scratch("old");
        fs::create_dir_all(root.join("acme")).unwrap();
        fs::write(
            root.join("acme").join("connection.json"),
            r#"{"name": "acme", "url": "https://litellm.example.com", "default_model": "m-1",
                "models": [{"id": "m-1", "name": "Model One", "context": 200000}]}"#,
        )
        .unwrap();
        let conn = load_in(&root, "acme").expect("it loads");
        assert_eq!(conn, conn_of("acme", "https://litellm.example.com"));
        save_in(&root, &conn).unwrap();
        let text = fs::read_to_string(root.join("acme").join("connection.json")).unwrap();
        assert!(
            !text.contains("models") && !text.contains("default_model"),
            "{text}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    fn conn_of(name: &str, url: &str) -> Connection {
        conn(name, url)
    }

    #[test]
    fn the_harvest_file_is_per_connection() {
        assert!(harvest_file("acme").ends_with("litellm/acme/models.json"));
    }
}
