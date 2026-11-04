//! The entire contract with the `sbx` CLI.
//!
//! Read paths (`ls`, `inspect`) parse `--json`; write paths hand back an argv that
//! [`crate::job`] runs with its output streamed into the UI.

use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

/// The oldest sbx release the dashboard accepts. docs/development.md, *sbx version*, says why.
pub const REQUIRED_VERSION: &str = "0.51.0";

/// How a sandbox mounts the host's shared agent-skills store (`--skills`). sbx reports the mode
/// nowhere, so a rebuild repeats it from the registry; see [`crate::state::Entry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillsMode {
    /// No skills mount at all — the sandbox is outside the shared trust boundary.
    Off,
    /// Read-only: the agent reads the shared skills and cannot change them. sbx's own default.
    #[default]
    Readonly,
    /// Mounted read-write: the agent can rewrite a skill every other sharing sandbox then loads.
    Readwrite,
}

impl SkillsMode {
    /// The `--skills` value, which is also how it serialises into the registry.
    pub fn flag(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Readonly => "readonly",
            Self::Readwrite => "readwrite",
        }
    }

    /// Parse what `sbx settings get skills.defaultMode` (or a registry file) says.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "readonly" => Some(Self::Readonly),
            "readwrite" => Some(Self::Readwrite),
            _ => None,
        }
    }

    /// Can the sandbox write back into the host store — i.e. is it inside the trust boundary the
    /// warning on the skills screen is about?
    pub fn writable(self) -> bool {
        self == Self::Readwrite
    }

    /// The next value when the create form's row is toggled. Ordered by how much the sandbox is
    /// given, so `space` walks from the tightest to the loosest and wraps.
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Readonly,
            Self::Readonly => Self::Readwrite,
            Self::Readwrite => Self::Off,
        }
    }

    pub fn prev(self) -> Self {
        self.next().next()
    }
}

/// The host's `skills.defaultMode`, what an omitted `--skills` would mean and what the create form
/// opens on. `readonly` on any failure, which is sbx's own built-in value.
pub fn skills_default_mode() -> SkillsMode {
    Command::new("sbx")
        .args(["settings", "get", "skills.defaultMode"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| SkillsMode::parse(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LsEntry {
    pub name: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub status: String,
    /// Primary workspace first, then any extra mounts (`/path:ro`).
    #[serde(default)]
    pub workspaces: Vec<String>,
    #[serde(default)]
    pub workspace_missing: bool,
    /// Published host->sandbox port mappings, the same rows as `sbx ports --json`.
    #[serde(default)]
    pub ports: Vec<Port>,
    /// When the sandbox was last used, RFC 3339 with fractional seconds. It is not a creation
    /// time: that is [`LsEntry::created_at`].
    #[serde(default)]
    pub last_used_at: Option<String>,
    /// When the sandbox's container was created, RFC 3339 UTC. A `sbx kit add` replaces the
    /// container and so moves it.
    #[serde(default)]
    pub created_at: Option<String>,
    /// The CPU limit given at creation (`--cpus`), absent when none was given. A rebuild repeats
    /// it; see [`Create::cpus`].
    #[serde(default)]
    pub cpus: Option<u32>,
    /// The memory limit as it was given (`4g`). Kept for display; a rebuild passes
    /// [`LsEntry::memory_mib`] instead, which is the same value normalised.
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub memory_mib: Option<u64>,
    /// The guest has stopped responding. Absent means false.
    #[serde(default)]
    pub guest_unresponsive: bool,
    /// The sandbox is in **detached mode**: it keeps running after its sessions end instead of
    /// stopping itself — what `sbx run -d --name <existing>` ([`keep_running_argv`]) switches on.
    #[serde(default)]
    pub detached: bool,
}

impl LsEntry {
    /// The memory limit as `sbx create -m` takes it back, normalised to MiB where sbx did that.
    pub fn memory_spec(&self) -> Option<String> {
        match (self.memory_mib, &self.memory) {
            (Some(mib), _) if mib > 0 => Some(format!("{mib}m")),
            (_, Some(given)) if !given.trim().is_empty() => Some(given.trim().to_string()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NetworkPolicy {
    /// `global` when the sandbox rides the local policy, `sandbox` when scoped rules exist.
    #[serde(default)]
    pub scope: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Secret {
    #[serde(default)]
    pub name: String,
    /// How sbx got it — `uploaded` for a stored secret, `proxy` for a proxy-managed one.
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InspectEntry {
    /// Absolute kit dirs, in the order they were stacked. Empty for a sandbox without a kit record.
    #[serde(default)]
    pub kits: Vec<String>,
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub image_digest: String,
    #[serde(default)]
    pub sessions: u32,
    // `uptime` is not read: it counts from container creation, not from the last start.
    /// `oauth · anthropic`, `api key`, … — how the agent inside authenticates.
    #[serde(default)]
    pub auth_mode: String,
    /// The sandbox's docker network name.
    #[serde(default)]
    pub network: String,
    #[serde(default)]
    pub network_policy: NetworkPolicy,
    /// `host:port` of the egress proxy that enforces the network policy.
    #[serde(default)]
    pub proxy: String,
    /// The credentials sbx has told this sandbox's proxy about, with their source. It is not a
    /// record of what is stored: `sbx secret ls` is.
    #[serde(default)]
    pub secrets: Vec<Secret>,
    #[serde(default)]
    pub mcp_gateway: bool,
    #[serde(default)]
    pub daemon_version: String,
    /// The sbx release that created the sandbox. Empty for one created before sbx reported it.
    #[serde(default)]
    pub created_version: String,
    /// Host folders added to the sandbox **after it was created**, with `sbx mount`. Read leniently
    /// through [`InspectEntry::dynamic_mounts`], since one malformed element must not cost the
    /// whole `inspect` (and with it the sandbox's kit list).
    #[serde(default)]
    pub runtime_mounts: Vec<serde_json::Value>,
    /// Every folder bind-mounted from the host, in sbx's order: the workspace (in clone mode the
    /// clone's source), the extra workspaces, the skills store, kit folders and `sbx mount` folders.
    /// Read leniently through [`InspectEntry::bind_mounts`], like `runtime_mounts`.
    #[serde(default)]
    pub host_mounts: Vec<serde_json::Value>,
    /// See [`LsEntry::guest_unresponsive`]; read from both, since either may carry it.
    #[serde(default)]
    pub guest_unresponsive: bool,
    /// See [`LsEntry::detached`]; read from both, like the flag above.
    #[serde(default)]
    pub detached: bool,
}

impl InspectEntry {
    /// The `sbx mount` mounts this sandbox carries. It skips an element of an unknown shape
    /// instead of failing on it.
    pub fn dynamic_mounts(&self) -> Vec<RuntimeMount> {
        lenient_mounts(&self.runtime_mounts)
    }

    /// Every host folder the sandbox mounts ([`InspectEntry::host_mounts`]), skipping an element
    /// of an unknown shape.
    pub fn bind_mounts(&self) -> Vec<HostMount> {
        lenient_mounts(&self.host_mounts)
    }
}

fn lenient_mounts(values: &[serde_json::Value]) -> Vec<RuntimeMount> {
    values
        .iter()
        .filter_map(|v| serde_json::from_value::<RuntimeMount>(v.clone()).ok())
        .filter(|m| !m.host_path.is_empty())
        .collect()
}

/// One bind mount of `host_mounts` in `sbx inspect --json`: the same fields as a [`RuntimeMount`].
pub type HostMount = RuntimeMount;

/// One host folder mounted into the sandbox: an element of `runtime_mounts` (added with
/// `sbx mount`) or of `host_mounts` (every bind mount) in `sbx inspect --json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RuntimeMount {
    #[serde(default)]
    pub host_path: String,
    /// Where it appears inside the sandbox — the host path itself unless one was given.
    #[serde(default)]
    pub container_target: String,
    #[serde(default)]
    pub read_only: bool,
}

impl RuntimeMount {
    /// `HOST[:TARGET][:ro]`, the `sbx mount` spelling — the target left implicit when it is the
    /// host path, which is `sbx mount`'s own default.
    pub fn spec(&self) -> String {
        let mut spec = self.host_path.clone();
        let explicit = !self.container_target.is_empty() && self.container_target != self.host_path;
        // sbx reads `HOST:ro` as the target path `ro`, so a read-only mount names its target.
        if explicit || self.read_only {
            spec.push(':');
            spec.push_str(match explicit {
                true => &self.container_target,
                false => &self.host_path,
            });
        }
        if self.read_only {
            spec.push_str(":ro");
        }
        spec
    }

    /// Where it is inside the sandbox.
    pub fn target(&self) -> &str {
        match self.container_target.is_empty() {
            true => &self.host_path,
            false => &self.container_target,
        }
    }
}

/// One row of `sbx policy ls <sandbox> --json` — a rule that applies to that sandbox, whether it
/// comes from the kit, the local policy or an org policy.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PolicyRule {
    /// The rule's id — what `sbx policy rm network --id` takes — and, for a rule `sbx policy`
    /// created, also its `name` (an answered approval is `approval:<id>`).
    #[serde(default)]
    pub name: String,
    /// `global` or `sandbox:<name>`.
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub applies_to: String,
    /// `network`, `http`, `filesystem:read`, `filesystem:write`.
    #[serde(default)]
    pub resource_type: String,
    /// `allow` or `deny`.
    #[serde(default)]
    pub decision: String,
    /// Hosts / CIDRs / paths the rule matches. For an HTTP rule, the hosts; the paths are in
    /// `http_targets`.
    #[serde(default)]
    pub resources: Vec<String>,
    /// `local`, `scoped` or `org` — which layer holds the rule.
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub status: String,
    /// Whether `sbx policy rm` may remove it: `false` for a kit's rules and the built-in presets,
    /// `true` for everything `sbx policy` or an approval made.
    #[serde(default)]
    pub editable: Option<bool>,
    /// How the rule came to exist.
    #[serde(default)]
    pub provenance: Provenance,
    /// What the rule covers: `net:connect:tcp`, `net:connect:udp`, `http:request:<method>`, …
    #[serde(default)]
    pub actions: Vec<String>,
    /// An HTTP rule's methods (`GET`, `POST`, … or `ANY`).
    #[serde(default)]
    pub methods: Vec<String>,
    /// An HTTP rule's host + path pairs.
    #[serde(default)]
    pub http_targets: Vec<HttpTarget>,
    /// Why the rule exists, in words, where sbx gives a reason.
    #[serde(default)]
    pub reason: String,
}

/// The name of the policy's fallback rule: the global `deny **` that decides a request no other
/// rule matches. See [`PolicyRule::is_fallback`].
pub const FALLBACK_RULE: &str = "default-deny-all";

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Provenance {
    /// `default` (a preset), `added` (`sbx policy allow|deny`, `--deny-network`), `provisioned` (a
    /// kit or the agent), `approval` (an answered approval request).
    #[serde(default)]
    pub created_via: String,
}

/// One host + path an HTTP rule matches, with the query parameters it requires.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct HttpTarget {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub path: String,
    /// The parameters a request must carry for the rule to match.
    #[serde(default)]
    pub query: Vec<QueryParam>,
    /// How [`HttpTarget::query`] matches: `subset` (every parameter present, in any order, other
    /// parameters allowed) is what `--query` writes.
    #[serde(default)]
    pub query_match: String,
}

/// One `--query name=value` of an HTTP rule: an exact name and a value that may start or end
/// with `*`, both in URL query encoding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct QueryParam {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value_pattern: String,
}

impl QueryParam {
    /// `name=value`, the form `--query` takes and sbx prints back.
    pub fn spec(&self) -> String {
        format!("{}={}", self.name, self.value_pattern)
    }
}

impl HttpTarget {
    /// The query as sbx prints the rule itself (`Added rule …: GET
    /// api.example.com/search?team=platform&label=release*`) — empty when there is none.
    pub fn query_suffix(&self) -> String {
        match self.query.is_empty() {
            true => String::new(),
            false => format!(
                "?{}",
                self.query
                    .iter()
                    .map(QueryParam::spec)
                    .collect::<Vec<_>>()
                    .join("&")
            ),
        }
    }
}

impl PolicyRule {
    /// A rule about where traffic may go: a host rule, or an HTTP rule narrowing one. Both belong
    /// on the Network tab.
    pub fn is_network(&self) -> bool {
        self.resource_type == "network" || self.is_http()
    }

    /// An HTTP method-and-path rule.
    pub fn is_http(&self) -> bool {
        self.resource_type == "http"
    }

    pub fn is_filesystem(&self) -> bool {
        self.resource_type.starts_with("filesystem")
    }

    pub fn allows(&self) -> bool {
        self.decision.eq_ignore_ascii_case("allow")
    }

    /// Did the sandbox's kit put this rule there? sbx reports a kit's deny list as
    /// `created_via: added`, so the `kit:` name counts as well.
    pub fn is_kit_rule(&self) -> bool {
        self.provenance.created_via == "provisioned"
            || self.origin == "kit"
            || self.name.starts_with("kit:")
    }

    /// Is this the policy's fallback, which decides a request no other rule matches, rather than
    /// a rule someone wrote?
    pub fn is_fallback(&self) -> bool {
        self.name == FALLBACK_RULE && self.provenance.created_via.is_empty()
    }

    /// Who put the rule there, in the words the Network tab uses: `kit`, `approved`, `added`,
    /// `preset`, `org` or `fallback`. Read off `provenance` and the name rather than `origin`, since
    /// a kit's own rule reports the same `origin` (`scoped`) as one the user added.
    pub fn source(&self) -> &str {
        if self.origin == "org" {
            return "org";
        }
        if self.is_fallback() {
            return "fallback";
        }
        if self.is_kit_rule() {
            return "kit";
        }
        match self.provenance.created_via.as_str() {
            "approval" => "approved",
            "default" => "preset",
            "added" => "added",
            _ if self.origin.is_empty() => "local",
            _ => &self.origin,
        }
    }

    /// Does the rule cover UDP as well as (or instead of) TCP? A deny covers both by default, an
    /// allow only TCP unless asked.
    pub fn covers_udp(&self) -> bool {
        self.actions.iter().any(|a| a == "net:connect:udp")
    }

    pub fn covers_tcp(&self) -> bool {
        self.actions.is_empty() || self.actions.iter().any(|a| a == "net:connect:tcp")
    }

    /// What the rule matches, for display: the hosts, or for an HTTP rule `POST
    /// api.example.com/admin/**` — with its query, `GET api.example.com/search?team=platform`.
    pub fn targets(&self) -> Vec<String> {
        if !self.is_http() {
            return self.resources.clone();
        }
        let methods = match self.methods.is_empty() {
            true => "ANY".to_string(),
            false => self.methods.join(","),
        };
        let paths: Vec<String> = match self.http_targets.is_empty() {
            true => self.resources.iter().map(|h| format!("{h}/**")).collect(),
            false => self
                .http_targets
                .iter()
                .map(|t| format!("{}{}{}", t.host, t.path, t.query_suffix()))
                .collect(),
        };
        paths
            .into_iter()
            .map(|p| format!("{methods} {p}"))
            .collect()
    }

    /// Is this one of the sandbox's **own** rules: scoped to it, not its kit's, and one that
    /// `sbx policy rm` may remove? A rebuild repeats these, since `sbx rm` takes them along.
    pub fn owned_by(&self, sandbox: &str) -> bool {
        self.scope == format!("sandbox:{sandbox}")
            && !self.is_kit_rule()
            && self.editable != Some(false)
            && self.origin != "org"
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PolicyLsOutput {
    #[serde(default)]
    rules: Vec<PolicyRule>,
}

/// One host in `sbx policy log --json`, with how often the proxy saw it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LogHost {
    #[serde(default)]
    pub host: String,
    /// `forward`, `forward-bypass`, `mitm`, …
    #[serde(default)]
    pub proxy_type: String,
    /// The rule that decided it — often empty for default decisions. A default deny names itself
    /// at length here (`no applicable policies for op(action=net:connect:tcp, …)`), which is why
    /// [`LogHost::reason`] is shown in preference when there is one.
    #[serde(default)]
    pub rule: String,
    /// Why it was blocked, in words — `No matching allow rule (default deny)` for a request no rule
    /// covered, which under `balanced` / `deny-all` is also one that asked for approval. Absent on
    /// allowed hosts.
    #[serde(default)]
    pub reason: String,
    /// When the proxy last decided a request for this host.
    #[serde(default)]
    pub last_seen: String,
    /// How many requests the proxy decided this way since `first_seen`.
    #[serde(default)]
    pub count: u64,
}

impl LogHost {
    /// The short answer to "why?" — the reason where sbx gives one, else the rule's name.
    pub fn why(&self) -> &str {
        match self.reason.trim().is_empty() {
            true => self.rule.trim(),
            false => self.reason.trim(),
        }
    }

    /// Blocked because nothing allowed it, rather than because a rule denied it — the case an
    /// approval answers.
    pub fn default_deny(&self) -> bool {
        self.reason.to_ascii_lowercase().contains("default deny")
            || self.rule.starts_with("no applicable policies")
    }
}

/// `sbx policy log <sandbox> --json`: `{allowed: [...], blocked: [...]}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PolicyLog {
    #[serde(default)]
    pub blocked: Vec<LogHost>,
    #[serde(default)]
    pub allowed: Vec<LogHost>,
}

impl PolicyLog {
    pub fn is_empty(&self) -> bool {
        self.blocked.is_empty() && self.allowed.is_empty()
    }
}

/// One published port mapping, as reported by `sbx ports <sandbox> --json`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Port {
    #[serde(default)]
    pub host_ip: String,
    #[serde(default)]
    pub host_port: u16,
    #[serde(default)]
    pub sandbox_port: u16,
    /// `tcp4`, `tcp`, `udp`, … as sbx reports it.
    #[serde(default)]
    pub protocol: String,
    /// The host port was assigned by the OS, so it changes on every restart. sbx does not emit
    /// the key, so this is always false.
    #[serde(default)]
    pub ephemeral: bool,
}

impl Port {
    /// `/tcp4`, `/tcp`, `/udp` — the protocol as sbx reported it, or nothing only when sbx said
    /// nothing.
    ///
    /// A spec with no protocol means `tcp4`, so `/tcp` is spelled out too: without it, a
    /// dual-stack mapping comes back IPv4-only.
    fn proto_suffix(&self) -> String {
        match self.protocol.is_empty() {
            true => String::new(),
            false => format!("/{}", self.protocol),
        }
    }

    /// `8080:3000/tcp4` - the spelling `--unpublish` expects back.
    pub fn spec(&self) -> String {
        format!(
            "{}:{}{}",
            self.host_port,
            self.sandbox_port,
            self.proto_suffix()
        )
    }

    /// What to paste into a browser. Loopback only — sbx never binds a public interface unless
    /// the user asked for one explicitly.
    pub fn url(&self) -> String {
        let host = match self.host_ip.as_str() {
            "" | "0.0.0.0" | "127.0.0.1" => "127.0.0.1".to_string(),
            "::" | "::1" => "[::1]".to_string(),
            other => other.to_string(),
        };
        format!("http://{host}:{}", self.host_port)
    }
}

/// Whether the policy would let this sandbox reach a host, and the daemon's own wording for it.
#[derive(Debug, Clone)]
pub struct Reachability {
    pub allowed: bool,
    pub detail: String,
    /// Denied because **no rule** matched, rather than by a deny rule — `deny_kind: "implicit"`
    /// in the JSON. Under `balanced` / `deny-all` that is exactly a destination the sandbox would
    /// *ask* for.
    pub implicit: bool,
}

/// `sbx policy check network --json`.
#[derive(Debug, Clone, Default, Deserialize)]
struct CheckOutput {
    #[serde(default)]
    allowed: bool,
    #[serde(default)]
    target: String,
    /// `explicit` (a deny rule matched) or `implicit` (nothing allowed it). Absent when allowed.
    #[serde(default)]
    deny_kind: String,
    #[serde(default)]
    reason: String,
}

/// Is this sbx new enough for what this dashboard asks of it? See [`REQUIRED_VERSION`] for what
/// that is. Major, minor and patch are compared; a missing part counts as 0. An unrecognised
/// version string is accepted rather than blocking on a format change.
pub fn meets_required_version(version: &str) -> bool {
    let parse = |s: &str| -> Option<(u32, u32, u32)> {
        let s = s.trim_start_matches('v');
        // `0.49.2-rc1` or `0.49.2+sha` → `0.49.2`.
        let s = s.split(['-', '+']).next()?;
        let mut parts = s.split('.');
        Some((
            parts.next()?.parse().ok()?,
            parts.next().unwrap_or("0").parse().ok()?,
            parts.next().unwrap_or("0").parse().ok()?,
        ))
    };
    match (parse(version), parse(REQUIRED_VERSION)) {
        (Some(have), Some(need)) => have >= need,
        _ => true,
    }
}

/// The `sbx version --json` payload. Only `client` is read: `server` is the daemon's half, and it
/// says `unavailable` while the daemon is not running, the normal state at dashboard start.
#[derive(Debug, Clone, Default, Deserialize)]
struct VersionOutput {
    #[serde(default)]
    client: VersionClient,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct VersionClient {
    #[serde(default)]
    version: String,
}

/// The CLI's own version, `v0.51.0`, from `sbx version --json`.
pub fn version() -> Result<String> {
    let out = Command::new("sbx")
        .args(["version", "--json"])
        .output()
        .context("running `sbx version` — is the sbx CLI installed and on PATH?")?;
    let version = parse_json::<VersionOutput>(&out.stdout)
        .ok()
        .filter(|_| out.status.success())
        .map(|v| v.client.version)
        .filter(|v| !v.is_empty());
    version.ok_or_else(|| {
        anyhow!(
            "`sbx version --json` gave no version — aisbx needs sbx {REQUIRED_VERSION} or later"
        )
    })
}

/// The gate both frontends run at start: the sbx version, or why it is missing or too old.
pub fn checked_version() -> Result<String> {
    let raw = version().context(
        "the sbx CLI is required — install Docker Sandboxes and make sure `sbx` is on PATH",
    )?;
    gate(raw)
}

fn gate(version: String) -> Result<String> {
    anyhow::ensure!(
        meets_required_version(&version),
        "this sbx is {version}, but this dashboard needs {}+ — update sbx; docs/development.md, \
         *sbx version*, says why",
        REQUIRED_VERSION
    );
    Ok(version)
}

/// Every local sandbox. `sbx ls --json` is a bare array of rows, and an empty host answers `[]`.
pub fn list() -> Result<Vec<LsEntry>> {
    let out = Command::new("sbx")
        .args(["ls", "--json"])
        .output()
        .context("running `sbx ls --json`")?;
    if !out.status.success() {
        return Err(anyhow!(
            "sbx ls failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_json(&out.stdout).context("parsing `sbx ls --json`")
}

pub fn inspect(name: &str) -> Result<InspectEntry> {
    let out = Command::new("sbx")
        .args(["inspect", name, "--json"])
        .output()
        .with_context(|| format!("running `sbx inspect {name} --json`"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "sbx inspect {name} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_json(&out.stdout).with_context(|| format!("parsing `sbx inspect {name} --json`"))
}

/// Policy rules that apply to one sandbox — the kit's `caps.network.allow`, the global local
/// policy, and anything an org policy adds, already intersected by the daemon.
pub fn policy_rules(name: &str) -> Result<Vec<PolicyRule>> {
    let out = Command::new("sbx")
        .args(["policy", "ls", name, "--json"])
        .output()
        .with_context(|| format!("running `sbx policy ls {name} --json`"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "sbx policy ls {name} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let parsed: PolicyLsOutput = parse_json(&out.stdout)
        .with_context(|| format!("parsing `sbx policy ls {name} --json`"))?;
    Ok(parsed.rules)
}

/// What the egress proxy actually allowed and blocked for one sandbox, newest first. `--limit`
/// bounds each of the two lists, not their sum.
pub fn policy_log(name: &str, limit: u32) -> Result<PolicyLog> {
    let out = Command::new("sbx")
        .args([
            "policy",
            "log",
            name,
            "--json",
            "--limit",
            &limit.to_string(),
        ])
        .output()
        .with_context(|| format!("running `sbx policy log {name} --json`"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "sbx policy log {name} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_json(&out.stdout).with_context(|| format!("parsing `sbx policy log {name} --json`"))
}

/// Read `/proc` and `df` inside a **running** sandbox for its resource usage. Never probe a
/// stopped sandbox: `sbx exec` starts it. Each probe opens a session, which keeps an idle sandbox
/// alive.
pub fn probe(name: &str) -> Result<crate::stats::Sample> {
    let out = Command::new("sbx")
        .args(["exec", name, "sh", "-c", crate::stats::PROBE])
        .output()
        .with_context(|| format!("running `sbx exec {name}` to read /proc"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "reading resource usage from {name} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    crate::stats::Sample::parse(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| anyhow!("could not parse /proc output from {name}"))
}

/// Ports published from this sandbox to the host. Safe on a stopped sandbox: it answers `[]` and
/// does not start it.
pub fn ports(name: &str) -> Result<Vec<Port>> {
    let out = Command::new("sbx")
        .args(["ports", name, "--json"])
        .output()
        .with_context(|| format!("running `sbx ports {name} --json`"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "sbx ports {name} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_json(&out.stdout).with_context(|| format!("parsing `sbx ports {name} --json`"))
}

/// Ask the daemon whether the policy in force for `sandbox` would allow reaching `target`.
/// Read-only: it opens no connection and creates no approval request.
pub fn check_host(sandbox: &str, target: &str) -> Result<Reachability> {
    let out = Command::new("sbx")
        .args([
            "policy",
            "check",
            "network",
            "--json",
            "--sandbox",
            sandbox,
            target,
        ])
        .output()
        .with_context(|| format!("running `sbx policy check network {target}`"))?;
    // A deny exits 1 with the answer on stdout, so the status alone decides nothing.
    match parse_json::<CheckOutput>(&out.stdout) {
        Ok(check) => Ok(reachability(&check, target)),
        Err(_) => Err(anyhow!(
            "sbx policy check network said nothing usable: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

fn reachability(check: &CheckOutput, asked: &str) -> Reachability {
    let target = match check.target.is_empty() {
        true => asked,
        false => &check.target,
    };
    let implicit = !check.allowed && check.deny_kind == "implicit";
    let detail = match (check.allowed, check.reason.trim()) {
        (true, _) => format!("Allowed: {target}"),
        (false, "") => format!("Denied: {target}"),
        (false, reason) => format!("Denied: {target} — {}", reason.to_lowercase()),
    };
    Reachability {
        allowed: check.allowed,
        detail,
        implicit,
    }
}

// --- approval requests ---------------------------------------------------------------------------

/// One request a sandbox is holding for the user to answer — an element of `sbx policy approval
/// ls --json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Approval {
    /// `network:<opaque>` — what `sbx policy approval respond` takes.
    #[serde(default)]
    pub id: String,
    /// The sandbox that asked.
    #[serde(default, rename = "sandbox_name")]
    pub sandbox: String,
    /// The destination: `example.org:443`, or `GET api.example.com:443/v1/data` for an HTTP rule.
    #[serde(default)]
    pub title: String,
    /// sbx's own detail, one fact per line: `Protocol: TCP`, `Resource type: domain`, the reason.
    #[serde(default)]
    pub detail: String,
    /// What the request can be answered with: `allow` and `dismiss`.
    #[serde(default)]
    pub options: Vec<ApprovalOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ApprovalOption {
    /// What `--option` takes.
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub label: String,
}

impl Approval {
    pub fn offers(&self, option: &str) -> bool {
        self.options.iter().any(|o| o.id == option)
    }

    /// Why it is asking, without the protocol and resource-type lines: `no matching allow rule
    /// (default deny)`, or under organization governance `approval required by policy "…"`. The
    /// whole detail, on one line, when it has nothing else.
    pub fn reason(&self) -> String {
        let lines: Vec<&str> = self
            .detail
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let reason: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| !l.starts_with("Protocol:") && !l.starts_with("Resource type:"))
            .collect();
        match reason.is_empty() {
            true => lines.join(" "),
            false => reason.join(" "),
        }
    }

    /// Held only because nothing allows it — sbx's `no matching allow rule (default deny)`, as
    /// [`LogHost::default_deny`] reads it — and not by an organization's policy, whose reason is
    /// worth reading. A place that already says as much can leave this reason out.
    pub fn default_deny(&self) -> bool {
        self.reason().to_ascii_lowercase().contains("default deny")
    }
}

/// Every approval request pending on this host, for every sandbox. A failure is an error, never
/// an empty list.
pub fn approvals() -> Result<Vec<Approval>> {
    let out = Command::new("sbx")
        .args(["policy", "approval", "ls", "--json"])
        .output()
        .context("running `sbx policy approval ls --json`")?;
    if !out.status.success() {
        return Err(anyhow!(
            "sbx policy approval ls failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_json(&out.stdout).context("parsing `sbx policy approval ls --json`")
}

/// Answer one pending request. `option` is one the request offers — `allow` records a rule for
/// that destination, scoped to the sandbox that asked; `dismiss` leaves it blocked.
pub fn approval_respond_argv(id: &str, option: &str) -> Vec<String> {
    vec![
        "sbx".into(),
        "policy".into(),
        "approval".into(),
        "respond".into(),
        id.into(),
        "--option".into(),
        option.into(),
    ]
}

/// One service secret in `sbx secret ls --json` — the identifiers a kit's `credentials:` list
/// names and sbx's own built-in services.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Service {
    /// [`GLOBAL_SCOPE`], or the name of the one sandbox it applies to.
    #[serde(default)]
    pub scope: String,
    /// sbx's own TYPE column — `service` for everything this dashboard shows.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// The identifier a kit's `credentials:` names, e.g. `litellm`.
    #[serde(default)]
    pub name: String,
    /// sbx's masked rendering of the value (`(stored)`, `(oauth configured)`) — never the value,
    /// which stays in the host keychain.
    #[serde(default, rename = "secret")]
    pub masked: String,
}

impl Service {
    /// A stored global service secret, for tests that only care that one exists.
    #[cfg(any(test, feature = "test-support"))]
    pub fn sample(name: &str) -> Self {
        Self {
            scope: GLOBAL_SCOPE.into(),
            kind: "service".into(),
            name: name.into(),
            masked: "sk***".into(),
        }
    }

    pub fn is_global(&self) -> bool {
        self.scope == GLOBAL_SCOPE
    }

    /// Is this the OAuth credential `/login` produces, rather than an API key stored by hand?
    /// sbx prints `(oauth configured)` in the value column for one.
    pub fn is_oauth(&self) -> bool {
        self.masked.to_ascii_lowercase().contains("oauth")
    }
}

/// One custom secret in `sbx secret ls --json` — a value keyed to a host and an environment
/// variable rather than to a service identifier. Never the value itself, which stays in the
/// keychain.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CustomSecret {
    /// [`GLOBAL_SCOPE`], or the name of the one sandbox it applies to.
    #[serde(default)]
    pub scope: String,
    /// Hosts the proxy swaps the placeholder on (`--host` is repeatable).
    #[serde(default)]
    pub targets: Vec<String>,
    /// The variable sbx sets inside the sandbox — to the placeholder, never to the value. Empty
    /// for a row stored with no `--env`, which is what a named setup token is.
    #[serde(default)]
    pub env: String,
    /// What the sandbox actually sees, and what `secret rm --placeholder` identifies it by.
    #[serde(default)]
    pub placeholder: String,
    /// sbx's masked rendering of the value. Never the value itself.
    #[serde(default, rename = "secret")]
    pub masked: String,
}

impl CustomSecret {
    /// Does the proxy swap this row in on the Anthropic API? A row can name more than one host,
    /// so it is matched per entry rather than compared whole.
    pub fn hits_token_host(&self) -> bool {
        self.targets.iter().any(|t| t.trim() == TOKEN_HOST)
    }

    /// Does the proxy swap this row in on `host`?
    pub fn targets_host(&self, host: &str) -> bool {
        self.targets.iter().any(|t| t.trim() == host)
    }

    /// The hosts as one string, for a column.
    pub fn targets_label(&self) -> String {
        self.targets.join(", ")
    }
}

/// What `sbx secret ls --json` reports: the service secrets and the custom secrets.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Secrets {
    /// The service secrets — the identifiers a kit's `credentials:` names.
    #[serde(default, rename = "secrets")]
    pub services: Vec<Service>,
    #[serde(default, rename = "custom_secrets")]
    pub custom: Vec<CustomSecret>,
}

impl Secrets {
    pub fn has_service(&self, name: &str) -> bool {
        self.services.iter().any(|s| s.name == name)
    }

    pub fn service(&self, name: &str) -> Option<&Service> {
        self.services.iter().find(|s| s.name == name)
    }

    /// The credential `/login` writes: sbx's own `anthropic` service, kept on the host at global
    /// scope and shared by every `/login` sandbox on the machine.
    pub fn oauth(&self) -> Option<&Service> {
        self.service(OAUTH_SERVICE)
    }

    /// The credential Antigravity's sign-in writes: the `antigravity` service, kept on the host at
    /// global scope and shared by every `agy` sandbox.
    pub fn agy_login(&self) -> Option<&Service> {
        self.services
            .iter()
            .find(|s| s.name == AGY_OAUTH_SERVICE && s.is_global())
    }

    /// Every named setup token stored on this host, in name order — the global rows aisbx writes
    /// for [`secret_set_named_token_argv`], each with the row that holds it.
    pub fn tokens(&self) -> Vec<(&str, &CustomSecret)> {
        let mut out: Vec<(&str, &CustomSecret)> = self
            .custom
            .iter()
            .filter(|c| c.scope == GLOBAL_SCOPE && c.hits_token_host())
            .filter_map(|c| token_name_of(&c.placeholder).map(|name| (name, c)))
            .collect();
        out.sort_by_key(|(name, _)| *name);
        out.dedup_by_key(|(name, _)| *name);
        out
    }

    /// The names of those tokens — what a new sandbox can be pointed at without pasting anything.
    pub fn token_names(&self) -> Vec<String> {
        self.tokens()
            .iter()
            .map(|(n, _)| (*n).to_string())
            .collect()
    }

    /// The global row holding a LiteLLM connection's key, whatever host it targets — the caller
    /// compares the target with the connection's host ([`CustomSecret::targets_host`]).
    pub fn litellm_key(&self, name: &str) -> Option<&CustomSecret> {
        let placeholder = crate::litellm::placeholder(name);
        self.custom
            .iter()
            .find(|c| c.scope == GLOBAL_SCOPE && c.placeholder == placeholder)
    }

    /// Whether a connection's key is stored for its host: `Some(true)` stored, `Some(false)` a row
    /// for another host, `None` no row.
    pub fn litellm_key_state(&self, name: &str, host: &str) -> Option<bool> {
        self.litellm_key(name).map(|c| c.targets_host(host))
    }

    /// The global LiteLLM key rows, as `(connection name, row)` in name order — what `S` lists,
    /// next to the connections, and orphans when no `connection.json` names them.
    pub fn litellm_keys(&self) -> Vec<(&str, &CustomSecret)> {
        let mut out: Vec<(&str, &CustomSecret)> = self
            .custom
            .iter()
            .filter(|c| c.scope == GLOBAL_SCOPE)
            .filter_map(|c| crate::litellm::name_of_placeholder(&c.placeholder).map(|n| (n, c)))
            .collect();
        out.sort_by_key(|(name, _)| *name);
        out.dedup_by_key(|(name, _)| *name);
        out
    }

    /// Custom secrets that are neither a named token nor a LiteLLM key — someone else's
    /// `sbx secret set-custom`, listed so the picture is complete but never changed from here.
    pub fn foreign_custom(&self) -> Vec<&CustomSecret> {
        self.custom
            .iter()
            .filter(|c| {
                let named = c.scope == GLOBAL_SCOPE
                    && c.hits_token_host()
                    && token_name_of(&c.placeholder).is_some();
                let litellm = c.scope == GLOBAL_SCOPE
                    && crate::litellm::name_of_placeholder(&c.placeholder).is_some();
                !named && !litellm
            })
            .collect()
    }

    /// Is a token actually stored behind this placeholder (the global row of a named token)?
    ///
    /// The test is the *target*, not the scope: a row only holds a token if the proxy would swap
    /// it in on [`TOKEN_HOST`].
    pub fn holds(&self, placeholder: &str) -> bool {
        self.custom
            .iter()
            .any(|c| c.placeholder == placeholder && c.hits_token_host())
    }
}

/// Secrets stored on this host, from `sbx secret ls --json`. sbx masks the values, and none of
/// them is read here.
pub fn secrets() -> Result<Secrets> {
    let out = Command::new("sbx")
        .args(["secret", "ls", "--json"])
        .output()
        .context("running `sbx secret ls --json`")?;
    if !out.status.success() {
        return Err(anyhow!(
            "sbx secret ls failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_json(&out.stdout).context("parsing `sbx secret ls --json`")
}

/// Decode the payload of a `--json` read: the first value that starts a line, with any text
/// before or after it skipped.
///
/// **Only a `{` or `[` that starts a line is a candidate.** sbx indents nested values, so a top
/// level that does not fit is an error instead of a match on some nested value.
fn parse_json<T: serde::de::DeserializeOwned>(stdout: &[u8]) -> Result<T> {
    let text = String::from_utf8_lossy(stdout);
    let mut last: Option<serde_json::Error> = None;
    let line_starts = std::iter::once(0).chain(text.match_indices('\n').map(|(at, _)| at + 1));
    for at in line_starts.filter(|at| matches!(text[*at..].chars().next(), Some('{' | '['))) {
        match serde_json::Deserializer::from_str(&text[at..])
            .into_iter::<T>()
            .next()
        {
            Some(Ok(value)) => return Ok(value),
            Some(Err(e)) => last = Some(e),
            None => {}
        }
    }
    match last {
        Some(e) => Err(e.into()),
        None => Err(anyhow!("no JSON in sbx output: {}", text.trim())),
    }
}

// --- argv builders ------------------------------------------------------------------------

/// Everything `sbx create` needs for one sandbox.
#[derive(Debug, Clone, Copy)]
pub struct Create<'a> {
    pub name: &'a str,
    /// The agent the sandbox runs — what `sbx inspect` will report as `agent`: a v3 workload's
    /// folder name, or one of sbx's built-in agents when no workload is given. It is the
    /// positional only in the second case; see [`Create::agent_kit`].
    pub agent: &'a str,
    /// The workload the sandbox is built from — every backend of this repo is one. **It is the
    /// agent positional**, `sbx create <kit-dir> --kit <mixin>...`. The folder's name is the agent,
    /// so the path must end in it. A path here is never also passed as `--kit`.
    pub agent_kit: Option<&'a Path>,
    /// Every kit the sandbox is built from, backend first. The one named by `agent_kit` is the
    /// positional and is skipped here; the rest are `--kit` mixins, all v3.
    pub kit_dirs: &'a [&'a Path],
    /// The host folder to mount, at its own absolute path. **Empty means no workspace at all**: a
    /// rebuild brings a mountless sandbox back mountless.
    pub workspace: &'a Path,
    /// Extra workspaces at their own host paths, already spelled for sbx: `/path:ro` (read-only) or
    /// `/path` (read-write - only ever from an explicit `aisbx up --mount DIR`).
    pub mounts: &'a [String],
    pub clone: bool,
    /// Ports to publish while the container is built, in sbx's own grammar.
    pub ports: &'a [String],
    /// How to mount the host's shared agent-skills store (`--skills`); see
    /// [`crate::kits::Backend::shares_skills`]. `None` passes no flag, so the host's
    /// `skills.defaultMode` decides: only a rebuild of a sandbox with no recorded mode does that.
    pub skills: Option<SkillsMode>,
    /// Hosts to **deny** this one sandbox, `--deny-network`.
    pub deny_network: &'a [String],
    /// MCP servers to put in this sandbox's fixed set, `--static-mcp`. Names as `sbx mcp ls`
    /// reports them; the set is chosen once, at creation.
    pub static_mcp: &'a [String],
    /// `--cpus` and `-m`. `None` is aisbx's default ([`default_cpus`], [`default_memory`]), which
    /// the create form always gets; `Some(0)` / `Some("")` passes nothing, which is sbx's "auto".
    /// A rebuild repeats the limits sbx reports (see [`LsEntry::cpus`]), so one that reports none
    /// comes back with the default.
    pub cpus: Option<u32>,
    pub memory: Option<&'a str>,
    /// `--template`: a saved sbx template (`sbx template save`) to build the container from
    /// instead of the agent's stock image. Nothing passes one: `aisbx up` refuses `--template`,
    /// since sbx builds no sandbox from a saved template under a v3 workload.
    pub template: Option<&'a str>,
}

/// The limits a sandbox gets unless asked for others; docs/sandboxes.md, *CPU and memory limits*, says
/// why. [`default_cpus`] and [`default_memory`] lower them on a small host.
pub const DEFAULT_CPUS: u32 = 4;
pub const DEFAULT_MEMORY_MIB: u64 = 4096;

/// [`DEFAULT_CPUS`], or fewer on a host that has fewer: sbx refuses more CPUs than the host has.
pub fn default_cpus() -> u32 {
    default_cpus_for(host_cpus())
}

/// [`DEFAULT_MEMORY_MIB`] as `-m` spells it, or sbx's own default where that is less: half the
/// host's memory, at least 512 MiB.
pub fn default_memory() -> String {
    default_memory_for(host_memory_mib())
}

fn default_cpus_for(host: Option<u32>) -> u32 {
    host.map_or(DEFAULT_CPUS, |h| DEFAULT_CPUS.min(h.max(1)))
}

fn default_memory_for(host_mib: Option<u64>) -> String {
    let mib = host_mib.map_or(DEFAULT_MEMORY_MIB, |h| {
        DEFAULT_MEMORY_MIB.min((h / 2).max(512))
    });
    if mib.is_multiple_of(1024) {
        format!("{}g", mib / 1024)
    } else {
        format!("{mib}m")
    }
}

/// The host's CPU count. `None` under `cfg(test)`, so an argv a test pins is the same on every
/// host, and where std cannot tell; either way the default is used unchanged.
fn host_cpus() -> Option<u32> {
    if cfg!(any(test, feature = "test-support")) {
        None
    } else {
        std::thread::available_parallelism()
            .ok()
            .and_then(|n| u32::try_from(n.get()).ok())
    }
}

/// The host's physical memory in MiB, read once. `None` under `cfg(test)` (see [`host_cpus`]) and
/// where it cannot be read; then the default is used unchanged.
fn host_memory_mib() -> Option<u64> {
    if cfg!(any(test, feature = "test-support")) {
        None
    } else {
        static HOST: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
        *HOST.get_or_init(read_host_memory_mib)
    }
}

/// std has no call for it, so each OS's own tool: `sysctl` on macOS, `/proc/meminfo` on Linux,
/// PowerShell's `Win32_ComputerSystem` on Windows (the launcher already needs PowerShell there).
fn read_host_memory_mib() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        meminfo_total_mib(&std::fs::read_to_string("/proc/meminfo").ok()?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let argv: &[&str] = if cfg!(windows) {
            &[
                "powershell",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
            ]
        } else {
            &["sysctl", "-n", "hw.memsize"]
        };
        let out = Command::new(argv[0])
            .args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        bytes_to_mib(&String::from_utf8_lossy(&out.stdout))
    }
}

/// A byte count alone on its output, as `sysctl -n hw.memsize` and PowerShell print it.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn bytes_to_mib(text: &str) -> Option<u64> {
    let bytes: u64 = text.trim().parse().ok()?;
    (bytes > 0).then_some(bytes / (1024 * 1024))
}

/// `MemTotal:  16384000 kB` from `/proc/meminfo`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn meminfo_total_mib(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    (kib > 0).then_some(kib / 1024)
}

/// Create a sandbox without attaching. Extra `:ro` workspaces follow the primary one.
pub fn create_argv(spec: Create<'_>) -> Vec<String> {
    let mut argv = vec!["sbx".into(), "create".into()];
    for kit in spec.kit_dirs {
        // `--kit` is for mixins (sbx's own help: "must be a mixin"); the sandbox kit goes in
        // the agent's place below.
        if spec.agent_kit.is_some_and(|k| k == *kit) {
            continue;
        }
        argv.push("--kit".into());
        argv.push(kit_ref(kit));
    }
    argv.push("--name".into());
    argv.push(spec.name.into());
    if spec.clone {
        argv.push("--clone".into());
    }
    for port in spec.ports {
        argv.push("-p".into());
        argv.push(port.clone());
    }
    for host in spec.deny_network {
        argv.push("--deny-network".into());
        argv.push(host.clone());
    }
    // Repeated, not one comma-separated value: a name with a comma cannot break it.
    for server in spec.static_mcp {
        argv.push("--static-mcp".into());
        argv.push(server.clone());
    }
    // Spelled out, so the form's mode is built even where `skills.defaultMode` differs.
    if let Some(mode) = spec.skills {
        argv.push("--skills".into());
        argv.push(mode.flag().into());
    }
    let cpus = spec.cpus.unwrap_or_else(default_cpus);
    if cpus > 0 {
        argv.push("--cpus".into());
        argv.push(cpus.to_string());
    }
    let memory = spec
        .memory
        .map_or_else(default_memory, |m| m.trim().to_string());
    if !memory.is_empty() {
        argv.push("-m".into());
        argv.push(memory);
    }
    // Last of the flags, just before the agent positional.
    if let Some(template) = spec.template {
        argv.push("--template".into());
        argv.push(template.into());
    }
    argv.push(match spec.agent_kit {
        Some(kit) => kit_ref(kit),
        None => spec.agent.into(),
    });
    // On `sbx create`, no path means a mountless sandbox, not the current directory.
    if !spec.workspace.as_os_str().is_empty() {
        argv.push(spec.workspace.display().to_string());
        argv.extend(spec.mounts.iter().cloned());
    }
    argv
}

/// sbx's global flag for a caller with no person to answer: a command that would ask fails and
/// names the command to run instead.
pub const NO_INPUT: &str = "--no-input";

/// `argv` with [`NO_INPUT`] right after `sbx`, before the subcommand, where `sbx exec` cannot hand
/// it to the command it runs. Another program's argv, or one that has the flag there, is unchanged.
pub fn no_input(argv: &[String]) -> Vec<String> {
    let mut out = argv.to_vec();
    let is_sbx = out.first().map(String::as_str) == Some("sbx");
    if is_sbx && out.get(1).map(String::as_str) != Some(NO_INPUT) {
        out.insert(1, NO_INPUT.to_string());
    }
    out
}

/// Attach to (and start) an existing sandbox. Runs in the foreground with the terminal handed
/// over — never through the job runner.
///
/// `workdir` is where the agent opens ([`crate::startdir`]); `None` is the top of the workspace.
/// `sbx run` launches the kit's own entrypoint, so this works for every agent. The caller checks
/// that the directory exists, on the host, where the same path is the same directory: sbx refuses
/// a missing one only after the terminal handover.
pub fn attach_argv(name: &str, workdir: Option<&Path>) -> Vec<String> {
    let mut argv = vec![
        "sbx".to_string(),
        "run".into(),
        "--name".into(),
        name.into(),
    ];
    if let Some(dir) = workdir {
        argv.push("--workdir".into());
        argv.push(dir.display().to_string());
    }
    argv
}

/// Open an interactive shell inside the sandbox: bash, or `sh` on an image without bash. Like
/// [`attach_argv`], this takes over the terminal - `-it` needs a real TTY.
///
/// `workdir` is the sandbox's start folder, so a shell opens where its agent does. `None`
/// leaves the default, which is the top of the workspace.
pub fn shell_argv(name: &str, workdir: Option<&Path>) -> Vec<String> {
    let mut argv = vec!["sbx".to_string(), "exec".into(), "-it".into()];
    if let Some(dir) = workdir {
        argv.push("-w".into());
        argv.push(dir.display().to_string());
    }
    argv.extend([
        name.to_string(),
        "sh".into(),
        "-c".into(),
        "command -v bash >/dev/null 2>&1 && exec bash || exec sh".into(),
    ]);
    argv
}

/// Run one command inside the sandbox, non-interactively — and start the sandbox if it is
/// stopped, which `sbx exec` does either way. No TTY: the output goes to the stdio the caller
/// hands it.
pub fn exec_argv(name: &str, cmd: &[&str]) -> Vec<String> {
    let mut argv = vec!["sbx".to_string(), "exec".into(), name.into()];
    argv.extend(cmd.iter().map(|c| (*c).to_string()));
    argv
}

// --- headless runs (`aisbx run`) -------------------------------------------------------------------

/// The argv element a headless launch puts the prompt into — see [`headless_exec_argv`].
pub const PROMPT_SLOT: &str = "@@AISBX_PROMPT@@";

/// The `sh` a headless launch runs in, inside the sandbox: record the PID (which `exec` keeps for
/// the agent), read the prompt from the file [`prompt_upload_argv`] wrote, swap it in for
/// [`PROMPT_SLOT`], and become the agent. Exit 125 means the prompt never arrived — a sandbox
/// failure, not the agent's.
const HEADLESS_WRAPPER: &str = r#"d=$1; shift
echo $$ > "$d/pid"
prompt=$(cat "$d/prompt") || exit 125
for a in "$@"; do
  shift
  if [ "$a" = "@@AISBX_PROMPT@@" ]; then set -- "$@" "$prompt"; else set -- "$@" "$a"; fi
done
exec "$@""#;

/// Write the prompt — read from this command's stdin — to `<run_dir>/prompt` inside the sandbox.
/// Owner-only, like everything in the run dir. docs/headless.md, *Why `run` works this way*, says
/// why a file.
pub fn prompt_upload_argv(name: &str, run_dir: &str) -> Vec<String> {
    vec![
        "sbx".into(),
        "exec".into(),
        "-i".into(),
        name.into(),
        "sh".into(),
        "-c".into(),
        r#"umask 077 && mkdir -p "$1" && cat > "$1/prompt""#.into(),
        "sh".into(),
        run_dir.into(),
    ]
}

/// Launch `agent_argv` non-interactively through [`HEADLESS_WRAPPER`], optionally in `workdir`
/// (`-w`; the default is the primary workspace — in clone mode, the sandbox's own clone).
pub fn headless_exec_argv(
    name: &str,
    workdir: Option<&Path>,
    run_dir: &str,
    agent_argv: &[String],
) -> Vec<String> {
    let mut argv = vec!["sbx".to_string(), "exec".into()];
    if let Some(dir) = workdir {
        argv.push("-w".into());
        argv.push(dir.display().to_string());
    }
    argv.extend([
        name.to_string(),
        "sh".into(),
        "-c".into(),
        HEADLESS_WRAPPER.into(),
        "sh".into(),
        run_dir.into(),
    ]);
    argv.extend(agent_argv.iter().cloned());
    argv
}

/// Did the agent start? The wrapper writes its PID before anything else, so a run whose
/// `sbx exec` failed before reaching the sandbox leaves no PID file behind.
pub fn headless_started_argv(name: &str, run_dir: &str) -> Vec<String> {
    vec![
        "sbx".into(),
        "exec".into(),
        name.into(),
        "test".into(),
        "-s".into(),
        format!("{run_dir}/pid"),
    ]
}

/// The script [`headless_kill_argv`] runs: collect the agent's whole process tree *first* — leaves
/// before parents — then TERM all of it, give the agent ten seconds, and KILL what is left of the
/// same list. Collect first: a child is reparented the moment its parent dies. Always exits 0.
pub const HEADLESS_KILL: &str = r#"p=$(cat "$1/pid" 2>/dev/null) || exit 0
tree() { for c in $(pgrep -P "$1" 2>/dev/null); do tree "$c"; done; echo "$1"; }
pids=$(tree "$p")
kill -TERM $pids 2>/dev/null
i=0; while kill -0 "$p" 2>/dev/null && [ "$i" -lt 10 ]; do sleep 1; i=$((i + 1)); done
kill -KILL $pids 2>/dev/null
exit 0"#;

/// Stop a headless agent and everything it started, by the PID its launch recorded.
pub fn headless_kill_argv(name: &str, run_dir: &str) -> Vec<String> {
    vec![
        "sbx".into(),
        "exec".into(),
        name.into(),
        "sh".into(),
        "-c".into(),
        HEADLESS_KILL.into(),
        "sh".into(),
        run_dir.into(),
    ]
}

pub fn headless_cleanup_argv(name: &str, run_dir: &str) -> Vec<String> {
    vec![
        "sbx".into(),
        "exec".into(),
        name.into(),
        "rm".into(),
        "-rf".into(),
        run_dir.into(),
    ]
}

/// Whether the host's global network policy still has to be initialized (`sbx policy init
/// allow-all|balanced|deny-all`). No CLI command reports it, so it is read from the daemon's
/// `GET /policy/network/setup`, on the socket `sbx daemon status --json` names. `None` - daemon
/// down, Windows, an unknown answer - means "don't know", and nothing is warned about.
pub fn network_policy_needs_init() -> Option<bool> {
    #[derive(Deserialize)]
    struct Status {
        #[serde(default)]
        status: String,
        #[serde(default)]
        socket: String,
    }
    #[derive(Deserialize)]
    struct Setup {
        needed: bool,
    }
    let out = Command::new("sbx")
        .args(["daemon", "status", "--json"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let status: Status = parse_json(&out.stdout).ok()?;
    if status.status != "running" || status.socket.is_empty() {
        return None;
    }
    let body = daemon_get(&status.socket, "/policy/network/setup")?;
    serde_json::from_str::<Setup>(&body).ok().map(|s| s.needed)
}

/// One `GET` on the daemon's Unix socket, returning the body of a `200`.
#[cfg(unix)]
fn daemon_get(socket: &str, path: &str) -> Option<String> {
    use std::io::{Read, Write};
    use std::time::Duration;
    let mut stream = std::os::unix::net::UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .ok()?;
    write!(stream, "GET {path} HTTP/1.0\r\nHost: sandboxd\r\n\r\n").ok()?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    http_ok_body(&raw)
}

/// The daemon listens on a named pipe on Windows; not read there.
#[cfg(not(unix))]
fn daemon_get(_socket: &str, _path: &str) -> Option<String> {
    None
}

fn http_ok_body(raw: &str) -> Option<String> {
    let (head, body) = raw.split_once("\r\n\r\n")?;
    let status = head.lines().next()?.split_whitespace().nth(1)?;
    (status == "200").then(|| body.trim().to_string())
}

/// The fix for [`network_policy_needs_init`], in one line for the UI and `aisbx up`.
pub const POLICY_INIT_HINT: &str = "the network policy is not initialized — run \
    `sbx policy init deny-all` (or balanced / allow-all) first";

/// Hosts the **local** policy allows to every sandbox. Empty on a `deny-all` host, where the kits'
/// allow-lists are the whole egress story; under `balanced` this is GitHub, npm, Maven Central and
/// the rest of its rule sets. Read from `sbx policy ls --json` (no sandbox: the global rules).
pub fn global_network_allows() -> Result<Vec<String>> {
    let out = Command::new("sbx")
        .args(["policy", "ls", "--json"])
        .output()
        .context("running `sbx policy ls --json`")?;
    if !out.status.success() {
        return Err(anyhow!(
            "sbx policy ls failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let parsed: PolicyLsOutput =
        parse_json(&out.stdout).context("parsing `sbx policy ls --json`")?;
    Ok(global_allows(&parsed.rules))
}

fn global_allows(rules: &[PolicyRule]) -> Vec<String> {
    rules
        .iter()
        .filter(|r| r.scope == GLOBAL_SCOPE && r.is_network() && r.allows())
        .flat_map(|r| r.resources.iter().cloned())
        .collect()
}

/// Hand the sandbox the friendly label the dashboard knows it by, as `~/.claude/sandbox-label`,
/// before each start.
///
/// The value goes in as `$0` rather than interpolated into the script, so a label containing
/// quotes, `$` or backticks is data and never shell syntax. A failure is harmless: the status
/// line then falls back to `SANDBOX_NAME` (the sbx name).
pub fn label_argv(name: &str, label: &str) -> Vec<String> {
    vec![
        "sbx".into(),
        "exec".into(),
        name.into(),
        "sh".into(),
        "-c".into(),
        r#"mkdir -p "$HOME/.claude" && printf %s "$0" > "$HOME/.claude/sandbox-label""#.into(),
        label.into(),
    ]
}

/// The `sh -c` program of [`write_files_argv`]: per file its path, its count of chunks and the
/// chunks, written to a temp file beside it and renamed over it.
const WRITE_FILES: &str = r#"while [ $# -gt 0 ]; do f=$1; n=$2; shift 2; t="$f.aisbx-tmp"; mkdir -p "${f%/*}" && : > "$t" || exit 1; while [ "$n" -gt 0 ]; do printf %s "$1" >> "$t" || exit 1; shift; n=$((n - 1)); done; mv -f "$t" "$f" || exit 1; done"#;

/// The most bytes one argument of [`write_files_argv`] carries: far below Linux's 128 KiB limit
/// on a single argument.
const WRITE_CHUNK: usize = 32 * 1024;

/// Write files into a sandbox (and start it if it is stopped): `sbx exec <name> sh -c '<write>' sh
/// <path> <n> <chunk>...`. The text travels in arguments of at most [`WRITE_CHUNK`] bytes, never
/// as shell text, so the step needs no stdin.
pub fn write_files_argv(name: &str, files: &[(String, String)]) -> Vec<String> {
    let mut argv: Vec<String> = ["sbx", "exec", name, "sh", "-c", WRITE_FILES, "sh"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for (path, text) in files {
        let chunks = chunks(text, WRITE_CHUNK);
        argv.push(path.clone());
        argv.push(chunks.len().to_string());
        argv.extend(chunks.into_iter().map(str::to_string));
    }
    argv
}

/// The `sh -c` program of [`merge_settings_argv`]: the file's path, then the keys as chunks of
/// one JSON object, which `jq` lays over the file's top level. A missing or empty file starts as
/// `{}`; a file `jq` cannot read is left as it is.
const MERGE_SETTINGS: &str = r#"f=$1; shift; t="$f.aisbx-tmp"; mkdir -p "${f%/*}" || exit 1; [ -s "$f" ] || printf '{}\n' > "$f" || exit 1; printf %s "$@" | jq --slurpfile s "$f" '$s[0] + .' > "$t" && mv -f "$t" "$f" || { rm -f "$t"; exit 1; }"#;

/// Merge the top-level keys of a JSON object into a JSON file in a sandbox, keeping every other
/// key: `sbx exec <name> sh -c '<merge>' sh <path> <chunk>...`. The text travels in arguments of
/// at most [`WRITE_CHUNK`] bytes.
pub fn merge_settings_argv(name: &str, path: &str, json: &str) -> Vec<String> {
    let mut argv: Vec<String> = ["sbx", "exec", name, "sh", "-c", MERGE_SETTINGS, "sh", path]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend(chunks(json, WRITE_CHUNK).into_iter().map(str::to_string));
    argv
}

/// `text` cut into pieces of at most `max` bytes, each ending on a character boundary.
fn chunks(text: &str, max: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len().min(max);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (head, tail) = rest.split_at(end);
        out.push(head);
        rest = tail;
    }
    out
}

/// Where `litellm-models.sh` keeps the gateway's model list inside the model fetcher
/// ([`crate::model_fetch`]).
pub const MODELS_CACHE: &str = "/home/agent/.cache/aisbx/litellm-models.json";

/// Runs the script path it gets with the rest of its arguments, exiting 127 when the sandbox has
/// no such file — [`kit_script_argv`]'s `sh -c` program.
const KIT_SCRIPT: &str = r#"f=$1; shift; [ -f "$f" ] || exit 127; exec sh "$f" "$@""#;

/// Run a script a kit ships in a sandbox (and start the sandbox if it is stopped):
/// `sbx exec <name> sh -c '<run>' sh <path> <args>...`. The path and arguments travel as
/// positional parameters, never as shell text.
pub fn kit_script_argv(name: &str, script: &str, args: &[String]) -> Vec<String> {
    let mut argv: Vec<String> = ["sbx", "exec", name, "sh", "-c", KIT_SCRIPT, "sh", script]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend(args.iter().cloned());
    argv
}

/// The `host-skills` kit's hook, at its in-sandbox path (`kits/host-skills/files/host-skills/`).
pub const HOST_SKILLS_HOOK: &str = "/usr/local/lib/aisbx/host-skills/startup.sh";

/// Run the `host-skills` hook in a sandbox, to completion, before a session starts (and start the
/// sandbox if it is stopped), so the links exist before the agent's first skill scan. The script
/// always exits 0; whatever it prints is its status, one line per skill folder.
pub fn host_skills_argv(name: &str) -> Vec<String> {
    kit_script_argv(name, HOST_SKILLS_HOOK, &[])
}

/// Run the skill-repo sync script ([`crate::skill_select::SCRIPT`]) in a sandbox with these
/// arguments: `sbx exec -i <sandbox> sh -c '<script>' sh <args>…`, the prompt upload's pattern.
/// The script travels as an argument, so no kit has to ship it; a `put` reads a tar on stdin.
pub fn repo_skills_argv(name: &str, args: &[String]) -> Vec<String> {
    let mut argv: Vec<String> = ["sbx", "exec", "-i", name, "sh", "-c"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.push(crate::skill_select::SCRIPT.to_string());
    argv.push("sh".into());
    argv.extend(args.iter().cloned());
    argv
}

/// The sandbox's model-list cache ([`MODELS_CACHE`]), read with one captured `sbx exec … cat`.
///
/// At most [`crate::models::MAX_HARVEST_BYTES`] are read; a longer answer, a failed exec or a
/// missing file is `None`. What comes back is sandbox data — [`crate::models::parse_sandbox_cache`]
/// validates it before anything reaches the host.
pub fn read_models_cache(name: &str) -> Option<String> {
    use std::io::Read;
    let limit = crate::models::MAX_HARVEST_BYTES;
    let mut child = Command::new("sbx")
        .args(["exec", name, "cat", MODELS_CACHE])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut buf = Vec::new();
    let read = child
        .stdout
        .take()
        .map(|out| out.take(limit as u64 + 1).read_to_end(&mut buf));
    if buf.len() > limit {
        let _ = child.kill();
    }
    let status = child.wait().ok()?;
    if !status.success() || buf.len() > limit || !matches!(read, Some(Ok(_))) {
        return None;
    }
    String::from_utf8(buf).ok()
}

pub fn rm_argv(name: &str) -> Vec<String> {
    vec!["sbx".into(), "rm".into(), "--force".into(), name.into()]
}

// --- prune ---------------------------------------------------------------------------------------

/// One sandbox `sbx prune` would remove.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PruneCandidate {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub agent: String,
    /// RFC 3339, UTC.
    #[serde(default)]
    pub stopped_at: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<String>,
}

/// What `sbx prune --dry-run --json` answers.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PrunePreview {
    #[serde(default)]
    pub would_remove: Vec<PruneCandidate>,
    /// Stopped sandboxes sbx leaves alone because it cannot tell when they stopped. The element
    /// shape is unknown, so only a name is taken out of them - see [`PrunePreview::skipped`].
    #[serde(default)]
    skipped_unknown_stop: Vec<serde_json::Value>,
}

impl PrunePreview {
    /// The names of the sandboxes sbx will skip: a bare string, or an object's `name`.
    pub fn skipped(&self) -> Vec<String> {
        self.skipped_unknown_stop
            .iter()
            .filter_map(|v| v.as_str().or_else(|| v.get("name")?.as_str()))
            .map(str::to_string)
            .collect()
    }
}

/// What `sbx prune` would remove, as sbx itself reports it. Read-only: it never starts a sandbox.
/// A failure is an error rather than an empty preview.
pub fn prune_preview() -> Result<PrunePreview> {
    let out = Command::new("sbx")
        .args(["prune", "--dry-run", "--json"])
        .output()
        .context("running `sbx prune --dry-run --json`")?;
    if !out.status.success() {
        return Err(anyhow!(
            "sbx prune --dry-run failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_json(&out.stdout).context("parsing `sbx prune --dry-run --json`")
}

/// Remove every stopped sandbox. `--force` because the confirmation prompt has no terminal in the
/// job runner — the dashboard has already asked, with [`prune_preview`]'s list on screen.
pub fn prune_argv() -> Vec<String> {
    vec!["sbx".into(), "prune".into(), "--force".into()]
}

/// Forward a host port into a sandbox that already exists; a stopped one is started first.
/// `spec` is sbx's port grammar, `[[HOST_IP:]HOST_PORT:]SANDBOX_PORT[/PROTOCOL]` - a bare `3000`
/// takes an ephemeral host port, no protocol means `tcp4`, IPv4 only, and `/tcp` binds both
/// families. [`Create::ports`] takes the same grammar at creation.
pub fn ports_publish_argv(name: &str, spec: &str) -> Vec<String> {
    vec![
        "sbx".into(),
        "ports".into(),
        name.into(),
        "--publish".into(),
        spec.into(),
    ]
}

pub fn ports_unpublish_argv(name: &str, spec: &str) -> Vec<String> {
    vec![
        "sbx".into(),
        "ports".into(),
        name.into(),
        "--unpublish".into(),
        spec.into(),
    ]
}

pub fn stop_argv(name: &str) -> Vec<String> {
    vec!["sbx".into(), "stop".into(), name.into()]
}

/// Switch an existing sandbox to **detached mode**: it keeps running after every session ends,
/// until it is stopped or removed - `sbx run --detach` against an existing sandbox. Only a
/// rebuild undoes the switch; sbx reports it as [`LsEntry::detached`].
pub fn keep_running_argv(name: &str) -> Vec<String> {
    vec![
        "sbx".into(),
        "run".into(),
        "--detach".into(),
        "--name".into(),
        name.into(),
    ]
}

/// Put one `sbx mount` mount back, on a rebuild. The sandbox must be running: `sbx mount` refuses
/// a stopped one.
pub fn mount_argv(name: &str, mount: &RuntimeMount) -> Vec<String> {
    vec!["sbx".into(), "mount".into(), name.into(), mount.spec()]
}

/// The commands that put one of a sandbox's **own** policy rules back after a rebuild —
/// `sbx policy allow|deny network --sandbox <name> ...`, one per HTTP target. An HTTP rule keeps
/// its method, path and every `--query`, in order, so a rebuild never widens it.
pub fn rule_argvs(rule: &PolicyRule, sandbox: &str) -> Vec<Vec<String>> {
    let verb = match rule.allows() {
        true => "allow",
        false => "deny",
    };
    let base = |resource: String| -> Vec<String> {
        vec![
            "sbx".into(),
            "policy".into(),
            verb.into(),
            "network".into(),
            "--sandbox".into(),
            sandbox.into(),
            resource,
        ]
    };
    if rule.is_http() {
        let methods = match rule.methods.is_empty() {
            true => "ANY".to_string(),
            false => rule.methods.join(","),
        };
        let targets: Vec<HttpTarget> = match rule.http_targets.is_empty() {
            true => rule
                .resources
                .iter()
                .map(|h| HttpTarget {
                    host: h.clone(),
                    path: "/**".into(),
                    ..Default::default()
                })
                .collect(),
            false => rule.http_targets.clone(),
        };
        return targets
            .into_iter()
            .filter(|t| !t.host.is_empty())
            .map(|t| {
                let mut argv = base(t.host);
                argv.extend(["--method".into(), methods.clone(), "--path".into(), t.path]);
                for param in &t.query {
                    argv.extend(["--query".into(), param.spec()]);
                }
                argv
            })
            .collect();
    }
    let hosts: Vec<&String> = rule.resources.iter().filter(|h| !h.is_empty()).collect();
    if hosts.is_empty() {
        return Vec::new();
    }
    let mut argv = base(
        hosts
            .iter()
            .map(|h| h.as_str())
            .collect::<Vec<_>>()
            .join(","),
    );
    // An allow covers TCP unless told otherwise, a deny both; say so only where the rule differs.
    match (rule.covers_tcp(), rule.covers_udp(), rule.allows()) {
        (true, true, true) => argv.extend(["--protocol".into(), "tcp,udp".into()]),
        (false, true, _) => argv.extend(["--protocol".into(), "udp".into()]),
        (true, false, false) if !rule.actions.is_empty() => {
            argv.extend(["--protocol".into(), "tcp".into()])
        }
        _ => {}
    }
    vec![argv]
}

/// The rules a rebuild re-adds with [`rule_argvs`] — this sandbox's own, bar the plain network
/// denies, which go back in at creation as `--deny-network` ([`deny_specs`]).
pub fn rules_to_readd<'a>(rules: &'a [PolicyRule], name: &str) -> Vec<&'a PolicyRule> {
    rules
        .iter()
        .filter(|r| r.owned_by(name) && r.is_network())
        .filter(|r| r.is_http() || r.allows())
        .collect()
}

/// The `-p` arguments that reproduce a set of published ports on a fresh container. The two rows
/// of a dual-stack publish fold back into one mapping, the protocol is always spelled out, and an
/// ephemeral port comes back as a bare sandbox port, which asks for a fresh one.
///
/// A loopback binding is left implicit (the protocol says which families); anything else keeps
/// its host IP, since that was an explicit choice.
pub fn publish_specs(ports: &[Port]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for port in ports {
        let spec = match port.ephemeral || port.host_port == 0 {
            true => format!("{}{}", port.sandbox_port, port.proto_suffix()),
            false => {
                let host = match port.host_ip.as_str() {
                    "" | "127.0.0.1" | "::1" | "[::1]" => String::new(),
                    ip if ip.contains(':') => format!("[{ip}]:"),
                    ip => format!("{ip}:"),
                };
                format!(
                    "{host}{}:{}{}",
                    port.host_port,
                    port.sandbox_port,
                    port.proto_suffix()
                )
            }
        };
        if !out.contains(&spec) {
            out.push(spec);
        }
    }
    out
}

/// The per-sandbox network **denies** that apply to `name`, as `--deny-network` arguments.
///
/// Only the sandbox's own plain *network* denies: an HTTP deny goes back through [`rule_argvs`],
/// since `--deny-network <host>` would widen it to the whole host.
pub fn deny_specs(rules: &[PolicyRule], name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for rule in rules {
        if !rule.owned_by(name) || rule.decision != "deny" || rule.resource_type != "network" {
            continue;
        }
        for host in &rule.resources {
            if !host.is_empty() && !out.contains(host) {
                out.push(host.clone());
            }
        }
    }
    out
}

// --- MCP servers ---------------------------------------------------------------------------------

/// One registered MCP server, as `sbx mcp ls --json` reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct McpServer {
    #[serde(default)]
    pub name: String,
    /// How the gateway talks to it — `local stdio`, `remote http`, …
    #[serde(default)]
    pub transport: String,
    /// Whether it is usable now: `ready`, or what it still needs. Empty when sbx says nothing.
    #[serde(default)]
    pub status: String,
    /// `local` or `remote` — sbx's own coarser classification of the transport.
    #[serde(default, rename = "type")]
    pub kind: String,
}

impl McpServer {
    /// Is the server ready to use, as far as `sbx mcp ls` can tell? Only the status `ready` counts.
    pub fn ready(&self) -> bool {
        self.status.trim() == "ready"
    }
}

/// The setting that turns telemetry off for sbx's CLI and daemon.
pub const TELEMETRY_DISABLED: &str = "telemetry.disabled";

/// The setting that lets processes in a sandbox copy text to the host clipboard.
pub const CLIPBOARD_TEXT_COPY: &str = "clipboard.textCopy";

/// The setting that lets agents in a sandbox read images from the host clipboard on a paste.
pub const CLIPBOARD_IMAGE_PASTE: &str = "clipboard.imagePaste";

/// The setting that gives new local sandboxes `/dev/fuse` and `SYS_ADMIN`.
pub const SANDBOX_FUSE: &str = "sandbox.fuse";

/// The skills mode a create without `--skills` gets: `off`, `readonly` or `readwrite`.
pub const SKILLS_DEFAULT_MODE: &str = "skills.defaultMode";

/// One setting, as `sbx settings get <key> --json` reports it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Setting {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub value: serde_json::Value,
    #[serde(default)]
    pub default: serde_json::Value,
    /// `default`, `override` while `sbx settings set` keeps another value, or `envvar` while the
    /// daemon's environment sets it.
    #[serde(default)]
    pub source: String,
    /// The environment variable that wins over a stored value, for a setting that has one.
    #[serde(default)]
    pub env_var: String,
    /// The daemon follows a change only after `sbx daemon restart`.
    #[serde(default)]
    pub requires_restart: bool,
    #[serde(default)]
    pub description: String,
}

impl Setting {
    /// A boolean setting's value; `None` for any other kind.
    pub fn flag(&self) -> Option<bool> {
        self.value.as_bool()
    }

    /// Does the daemon's environment set it? Then `sbx settings set` stores a value that does not
    /// apply.
    pub fn from_env(&self) -> bool {
        self.source == "envvar"
    }
}

/// Read one setting from sbx.
pub fn setting(key: &str) -> Result<Setting> {
    let out = Command::new("sbx")
        .args(["settings", "get", key, "--json"])
        .output()
        .with_context(|| format!("running `sbx settings get {key} --json`"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "sbx settings get {key} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_json(&out.stdout).with_context(|| format!("parsing `sbx settings get {key} --json`"))
}

/// The environment variable that turns sbx's telemetry off for the CLI it is set for.
pub const NO_TELEMETRY_ENV: &str = "SBX_NO_TELEMETRY";

/// What [`no_telemetry_env`] sets `SBX_NO_TELEMETRY` to: `1`, unless it already has a value.
pub fn no_telemetry_value(current: Option<&std::ffi::OsStr>) -> Option<&'static str> {
    current.is_none().then_some("1")
}

/// Set `SBX_NO_TELEMETRY=1` for this process, so every `sbx` it starts inherits it; a value
/// already set stays. Call first in `main`, before any thread starts.
pub fn no_telemetry_env() {
    if let Some(value) = no_telemetry_value(std::env::var_os(NO_TELEMETRY_ENV).as_deref()) {
        std::env::set_var(NO_TELEMETRY_ENV, value);
    }
}

/// Does `telemetry` still stand at sbx's default, telemetry on, which aisbx turns off? A value
/// someone stored, or set through `SBX_NO_TELEMETRY`, is theirs and stays.
pub fn telemetry_left_on(telemetry: &Setting) -> bool {
    telemetry.key == TELEMETRY_DISABLED
        && telemetry.source == "default"
        && telemetry.value == serde_json::Value::Bool(false)
}

/// Turn sbx's telemetry off where nobody chose a value ([`telemetry_left_on`]). `Ok(true)`: it
/// was turned off now. Never under the test guard.
pub fn disable_telemetry_by_default() -> Result<bool> {
    if cfg!(any(test, feature = "test-support")) {
        return Ok(false);
    }
    if !telemetry_left_on(&setting(TELEMETRY_DISABLED)?) {
        return Ok(false);
    }
    let argv = setting_set_argv(TELEMETRY_DISABLED, "true");
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .output()
        .context("running `sbx settings set telemetry.disabled true`")?;
    anyhow::ensure!(
        out.status.success(),
        "sbx settings set telemetry.disabled true failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(true)
}

/// Store a setting on this host. It asks nothing, also without a terminal.
pub fn setting_set_argv(key: &str, value: &str) -> Vec<String> {
    vec![
        "sbx".into(),
        "settings".into(),
        "set".into(),
        key.into(),
        value.into(),
    ]
}

/// The `--static-mcp` value for a sandbox with no MCP server and no tools to find one. sbx reserves
/// the name and reads it in any case.
pub const STATIC_MCP_NONE: &str = "none";

/// Is this fixed MCP set the empty one, [`STATIC_MCP_NONE`]?
pub fn is_no_mcp(servers: &[String]) -> bool {
    servers
        .iter()
        .any(|s| s.eq_ignore_ascii_case(STATIC_MCP_NONE))
}

/// Every MCP server registered on this host. A *failure* is also an empty list: an unexpected
/// shape must not stop a sandbox being created.
pub fn mcp_servers() -> Vec<McpServer> {
    let Ok(out) = Command::new("sbx").args(["mcp", "ls", "--json"]).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_mcp_json(&out.stdout)
}

fn parse_mcp_json(stdout: &[u8]) -> Vec<McpServer> {
    parse_json::<Vec<McpServer>>(stdout).unwrap_or_default()
}

// --- shared agent skills -------------------------------------------------------------------------

/// Where sbx keeps the shared store, so the UI can name it rather than describing it.
///
/// A guess by platform, and only the fallback for a host where `sbx skills ls --json` failed:
/// [`SkillsStore::store`] carries the real path.
pub fn skills_store() -> Option<std::path::PathBuf> {
    let home = crate::model::home_dir()?;
    #[cfg(target_os = "macos")]
    {
        Some(home.join("Library/Application Support/com.docker.sandboxes/sandboxes/agent-skills"))
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(|local| {
                Path::new(&local)
                    .join("DockerSandboxes")
                    .join("sandboxes")
                    .join("state")
                    .join("agent-skills")
            })
            .or_else(|| Some(home.join("agent-skills")))
    }
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    {
        let base = std::env::var_os("XDG_STATE_HOME")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("state"));
        Some(
            base.join("sandboxes")
                .join("sandboxes")
                .join("agent-skills"),
        )
    }
}

/// Host directories `sbx skills import` reads, in the order it resolves conflicts (first wins).
pub const SKILL_SOURCES: &[&str] = &[
    ".agents/skills",
    ".claude/skills",
    ".config/opencode/skills",
    ".copilot/skills",
    ".cursor/skills",
    ".factory/skills",
];

/// The [`SKILL_SOURCES`] that exist on this host, as `~/<rel>` — what `sbx skills import` would
/// read. Probes the real home folder, also under tests.
pub fn host_skill_sources() -> Vec<String> {
    crate::model::home_dir()
        .map(|home| {
            SKILL_SOURCES
                .iter()
                .filter(|rel| home.join(rel).is_dir())
                .map(|rel| format!("~/{rel}"))
                .collect()
        })
        .unwrap_or_default()
}

/// What the shared store holds, as `sbx skills ls --json` reports it: names only, without where
/// each skill came from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SkillsStore {
    /// Where the store is on this host — the path sbx mounts into every sharing sandbox.
    #[serde(default)]
    pub store: String,
    #[serde(default)]
    pub skills: Vec<String>,
}

/// The shared store's contents. Read-only and cheap (no daemon round-trip is needed to list a
/// directory, though sbx may start the daemon anyway).
pub fn skills_ls() -> Result<SkillsStore> {
    let out = Command::new("sbx")
        .args(["skills", "ls", "--json"])
        .output()
        .context("running `sbx skills ls --json`")?;
    if !out.status.success() {
        return Err(anyhow!(
            "sbx skills ls failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_json(&out.stdout).context("parsing `sbx skills ls --json`")
}

/// Copy the host's agent skills into the shared store; without `force`, only show what would be
/// copied (`--dry-run`).
///
/// **`--force` is not optional for the copy.** Without it sbx prompts before replacing a skill,
/// and the job runner has no terminal. The caller confirms first instead.
pub fn skills_import_argv(force: bool) -> Vec<String> {
    let mut argv = vec!["sbx".into(), "skills".into(), "import".into()];
    if force {
        argv.push("--force".into());
    } else {
        argv.push("--dry-run".into());
    }
    argv
}

/// Install skills from a Git repository into the shared store. `repo` is what sbx takes -
/// GitHub `owner/repository` shorthand or a Git URL - and `skills` narrows it to named skills;
/// empty installs every `SKILL.md` the repository holds. `--force` for the same reason as
/// [`skills_import_argv`].
pub fn skills_add_argv(repo: &str, skills: &[String]) -> Vec<String> {
    let mut argv = vec![
        "sbx".into(),
        "skills".into(),
        "add".into(),
        repo.into(),
        "--force".into(),
    ];
    for skill in skills {
        argv.push("--skill".into());
        argv.push(skill.clone());
    }
    argv
}

/// Refresh the skills `sbx skills add` installed, from their repositories. All of them with no
/// names; an imported skill has no repository and is skipped either way.
pub fn skills_update_argv(skills: &[String]) -> Vec<String> {
    let mut argv = vec!["sbx".into(), "skills".into(), "update".into()];
    argv.extend(skills.iter().cloned());
    argv
}

/// Remove skills from the shared store. `--force` because removal asks, and the confirmation
/// before this is where that is answered.
pub fn skills_rm_argv(skills: &[String]) -> Vec<String> {
    let mut argv = vec!["sbx".into(), "skills".into(), "rm".into()];
    argv.extend(skills.iter().cloned());
    argv.push("--force".into());
    argv
}

/// Does this look like something `sbx skills add` accepts — `owner/repo` shorthand or a Git URL?
/// A spelling check, not a promise: sbx decides what it can clone.
pub fn skills_repo_ok(repo: &str) -> bool {
    let repo = repo.trim();
    if repo.is_empty() || repo.chars().any(char::is_whitespace) {
        return false;
    }
    if repo.contains("://") || repo.starts_with("git@") {
        return true;
    }
    let mut parts = repo.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(owner), Some(name), None) if !owner.is_empty() && !name.is_empty()
    )
}

// --- SSH, and the editors that ride it -----------------------------------------------------------

/// The SSH host name of a sandbox — the whole integration story in one string.
pub fn ssh_host(name: &str) -> String {
    format!("{name}.sbx")
}

/// Write (or refresh) the managed `Host *.sbx` block. Idempotent, and safe to re-run.
pub fn setup_ssh_argv() -> Vec<String> {
    vec!["sbx".into(), "setup".into(), "ssh".into()]
}

/// Has `sbx setup ssh` been run on this host? Read from the managed marker in `~/.ssh/config`
/// rather than remembered. Only `~/.ssh/config` is read, because the generated include file holds
/// a fence of its own.
pub fn ssh_configured() -> bool {
    crate::model::home_dir()
        .map(|h| h.join(".ssh").join("config"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_some_and(|text| ssh_fenced(&text))
}

/// Does an `~/.ssh/config` hold the include block `sbx setup ssh` writes,
/// `# >>> Docker Sandboxes (managed: <profile>) >>>`?
fn ssh_fenced(config: &str) -> bool {
    config.contains("# >>> Docker Sandboxes (managed:")
}

// --- named Anthropic setup tokens ------------------------------------------------------------

/// The variable Claude Code reads a long-lived subscription token from.
pub const TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// sbx's own service identifier for the Anthropic credential — the one `/login` proxy-manages at
/// global scope, which is why every `/login` sandbox on a machine shares one account.
pub const OAUTH_SERVICE: &str = "anthropic";
/// The service the `agy` kit's OAuth credential names — Antigravity's Google sign-in,
/// proxy-managed and kept at global scope the same way `/login` is, so every `agy` sandbox on a
/// machine shares one Google account. Separate from the kit's `google` API-key service.
pub const AGY_OAUTH_SERVICE: &str = "antigravity";
/// The only host a token is ever swapped in for.
pub const TOKEN_HOST: &str = "api.anthropic.com";
/// The scope `sbx secret ls --json` reports for a secret that applies to every sandbox.
pub const GLOBAL_SCOPE: &str = "global";

/// A service secret the **MCP gateway** owns, rather than an agent credential: sbx reserves the
/// whole `mcp:` prefix for them. The gateway uses them on the host; none is injected into a
/// sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSecret<'a> {
    /// The registered server the secret belongs to.
    pub server: &'a str,
    /// What it is for: `client_secret`, a header placeholder's name, or `client_secret:identity` /
    /// `…:endpoint` for the binding records sbx maintains itself.
    pub key: &'a str,
}

/// Recognise a `mcp:`-prefixed service secret. `None` for every ordinary one.
pub fn mcp_secret(name: &str) -> Option<McpSecret<'_>> {
    let rest = name.strip_prefix("mcp:")?;
    let (server, key) = rest.split_once(':').unwrap_or((rest, ""));
    Some(McpSecret { server, key })
}
/// Marks a placeholder as one of aisbx's *named* tokens, and carries the name.
const TOKEN_PREFIX: &str = "sbx-cs-aisbx-tok-";

/// The placeholder a named token is stored under, and that every sandbox using it carries in
/// [`TOKEN_ENV`]. Deterministic: a container keeps the placeholder it was built with.
pub fn named_token_placeholder(name: &str) -> String {
    format!("{TOKEN_PREFIX}{name}")
}

/// The name behind a placeholder, or `None` if it is not a named token.
pub fn token_name_of(placeholder: &str) -> Option<&str> {
    placeholder
        .strip_prefix(TOKEN_PREFIX)
        .filter(|name| !name.is_empty())
}

/// A token name that survives the round trip through a placeholder: lower case, and nothing that
/// would run the name and the prefix together ambiguously.
pub fn token_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Store or rotate the named token itself: one global row, no environment variable anywhere.
/// **The value is written to the child's stdin** (`--value-stdin`), never to argv, where `ps`
/// would show it to every process on the host - see [`crate::job::Step::stdin`]. Rotating takes
/// effect at once for *every* sandbox pointed at the name.
///
/// **No `-g`**: global is the default scope, and `--global` is deprecated.
pub fn secret_set_named_token_argv(placeholder: &str) -> Vec<String> {
    vec![
        "sbx".into(),
        "secret".into(),
        "set-custom".into(),
        "--host".into(),
        TOKEN_HOST.into(),
        "--placeholder".into(),
        placeholder.into(),
        "--value-stdin".into(),
    ]
}

/// Store a LiteLLM connection's key: one **global** custom secret with no `--env`, targeted at the
/// gateway's host, under the connection's placeholder ([`crate::litellm::placeholder`]). The value
/// goes on stdin, never in argv. The same shape as [`secret_set_named_token_argv`], and the same
/// facts hold: storing it again rotates it for every sandbox on the connection, and a new host
/// under the same placeholder replaces the old target. Never a sandbox-scoped row - it would
/// shadow this one and kill the swap (see [`crate::tokens`]).
pub fn secret_set_litellm_key_argv(host: &str, placeholder: &str) -> Vec<String> {
    vec![
        "sbx".into(),
        "secret".into(),
        "set-custom".into(),
        "--host".into(),
        host.into(),
        "--placeholder".into(),
        placeholder.into(),
        "--value-stdin".into(),
    ]
}

/// Drop a LiteLLM connection's key. Every sandbox on the connection stops authenticating at once.
/// `-f`, like every removal here: no terminal to confirm on, and a placeholder that matches nothing
/// is then an exit 0.
pub fn secret_rm_litellm_key_argv(placeholder: &str) -> Vec<String> {
    vec![
        "sbx".into(),
        "secret".into(),
        "rm".into(),
        "--placeholder".into(),
        placeholder.into(),
        "-f".into(),
    ]
}

/// Drop a named token from the host, and with it the account every sandbox pointed at that name
/// was using. Only ever reached from the secrets screen: deleting a *sandbox* never touches a
/// shared token.
///
/// No `-g`, for the reason in [`secret_set_named_token_argv`]. `-f` for the reason in
/// [`secret_rm_token_argv`].
pub fn secret_rm_named_token_argv(placeholder: &str) -> Vec<String> {
    vec![
        "sbx".into(),
        "secret".into(),
        "rm".into(),
        "--placeholder".into(),
        placeholder.into(),
        "-f".into(),
    ]
}

/// Store or replace a service secret — the gateway token a kit's `credentials:` names, and any
/// built-in service. Value on **stdin**, never in argv.
///
/// `scope` is `None` for the global row every new sandbox is created from, or a sandbox name for
/// one stored against that sandbox alone (`sbx secret set openai --sandbox my-sandbox`). Both
/// shapes turn up in `sbx secret ls`, and naming the wrong one writes a second, different secret.
///
/// **The service name leads and the scope follows it as a flag**: `--global` and the positional
/// scope are deprecated.
///
/// **`-f` is not optional**, also for a value on stdin: without it, `sbx secret set` asks before
/// replacing an existing secret, and the job runner has no terminal to answer on.
pub fn secret_set_service_argv(scope: Option<&str>, name: &str) -> Vec<String> {
    let mut argv = vec!["sbx".into(), "secret".into(), "set".into(), name.into()];
    if let Some(sandbox) = scope {
        argv.push("--sandbox".into());
        argv.push(sandbox.into());
    }
    argv.push("-f".into());
    argv
}

/// Remove a service secret from the keychain, at the scope it is stored in. For the global
/// `anthropic` row this is the documented way to switch which account `/login` means: drop it,
/// then `/login` again inside any Claude sandbox.
///
/// Same grammar as [`secret_set_service_argv`]: name first, `--sandbox` for a scoped row,
/// nothing at all for the global one.
pub fn secret_rm_service_argv(scope: Option<&str>, name: &str) -> Vec<String> {
    let mut argv = vec!["sbx".into(), "secret".into(), "rm".into(), name.into()];
    if let Some(sandbox) = scope {
        argv.push("--sandbox".into());
        argv.push(sandbox.into());
    }
    argv.push("-f".into());
    argv
}

/// Add a kit to a live sandbox: sbx swaps the container for one with the kit appended. A stopped
/// sandbox is started first. What it refuses is [`crate::kits`]'s to predict
/// (`kit_add_blocker`); the caller falls back to a full recreate.
pub fn kit_add_argv(name: &str, kit_dir: &Path) -> Vec<String> {
    vec![
        "sbx".into(),
        "kit".into(),
        "add".into(),
        name.into(),
        kit_ref(kit_dir),
    ]
}

/// How a local kit directory must be spelled for sbx: **no trailing separator**, which
/// `sbx kit add` rejects as not canonical.
fn kit_ref(dir: &Path) -> String {
    dir.display().to_string()
}

#[cfg(test)]
mod daemon_tests {
    use super::*;

    #[test]
    fn only_a_200_body_is_an_answer() {
        let ok = "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"needed\":true}\n";
        assert_eq!(http_ok_body(ok).as_deref(), Some("{\"needed\":true}"));
        assert_eq!(http_ok_body("HTTP/1.0 404 Not Found\r\n\r\n{}"), None);
        assert_eq!(http_ok_body("garbage"), None);
    }
}

/// Live checks against the sbx daemon on this machine, ignored by default: they need a working sbx
/// install and real sandboxes. docs/development.md, *sbx version*, says how to run them.
#[cfg(test)]
mod live {
    use super::*;

    /// The names `sbx <cmd> -q` prints, one per line — a second opinion that shares no parser
    /// with the JSON read it checks.
    fn quiet_names(args: &[&str]) -> Vec<String> {
        let out = Command::new("sbx").args(args).output().expect("sbx -q");
        assert!(out.status.success(), "sbx {args:?} failed");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    }

    #[test]
    #[ignore = "talks to the local sbx daemon; run explicitly"]
    fn live_json_shapes_still_parse() {
        println!("sbx version: {}", version().expect("sbx version --json"));
        println!(
            "network policy needs init: {:?}",
            network_policy_needs_init()
        );
        let sandboxes = list().expect("sbx ls --json");
        println!("{} sandbox(es)", sandboxes.len());
        // `ls -q` shares no parser with the JSON read: a moved shape cannot pass as no sandboxes.
        let mut names: Vec<String> = sandboxes.iter().map(|s| s.name.clone()).collect();
        names.sort();
        let mut quiet = quiet_names(&["ls", "-q"]);
        quiet.sort();
        assert_eq!(
            names, quiet,
            "`ls --json` and `ls -q` disagree about which sandboxes exist"
        );
        // Host-wide reads. A field that moves shows up as an empty list or a blank column, so they
        // are printed.
        let secrets = secrets().expect("sbx secret ls --json");
        println!(
            "{} service secret(s), {} custom; named tokens {:?}",
            secrets.services.len(),
            secrets.custom.len(),
            secrets.token_names()
        );
        for svc in &secrets.services {
            println!("  service {} @{} → {}", svc.name, svc.scope, svc.masked);
            assert!(!svc.name.is_empty() && !svc.scope.is_empty(), "{svc:?}");
        }
        for c in &secrets.custom {
            println!(
                "  custom {} @{} env={:?} → {}",
                c.placeholder, c.scope, c.env, c.masked
            );
            assert!(!c.placeholder.is_empty() && !c.targets.is_empty(), "{c:?}");
        }
        let servers = mcp_servers();
        println!("{} MCP server(s) registered", servers.len());
        for m in &servers {
            println!("  {} [{}] {} ({})", m.name, m.transport, m.status, m.kind);
            assert!(!m.name.is_empty() && !m.transport.is_empty(), "{m:?}");
        }
        // `mcp_servers` folds a failure into an empty list on purpose (the create form must still
        // open), so the count is checked against the plain listing here instead.
        assert_eq!(
            servers.len(),
            quiet_names(&["mcp", "ls", "-q"]).len(),
            "`mcp ls --json` no longer reads as the servers `mcp ls -q` names"
        );
        let store = skills_ls().expect("sbx skills ls --json");
        println!("skills store {}: {:?}", store.store, store.skills);
        assert!(!store.store.is_empty(), "the store path is reported");
        // What an omitted `--skills` would mean, and so what the create form opens on. A setting
        // sbx renamed or dropped answers nothing and reads as `readonly`.
        println!("skills.defaultMode: {:?}", skills_default_mode().flag());
        // The settings screen's rows: booleans, read as sbx prints them.
        for key in [
            TELEMETRY_DISABLED,
            CLIPBOARD_TEXT_COPY,
            CLIPBOARD_IMAGE_PASTE,
            SANDBOX_FUSE,
        ] {
            let setting = setting(key).expect("sbx settings get --json");
            println!(
                "{key}: {:?} ({}, restart {})",
                setting.flag(),
                setting.source,
                setting.requires_restart
            );
            assert_eq!(setting.key, key);
            assert!(setting.flag().is_some(), "{key} is no longer a bool");
        }
        let skills = setting(SKILLS_DEFAULT_MODE).expect("sbx settings get --json");
        println!(
            "{SKILLS_DEFAULT_MODE}: {:?} ({})",
            skills.value, skills.source
        );
        let mode = skills.value.as_str().and_then(SkillsMode::parse);
        assert!(
            mode.is_some(),
            "skills.defaultMode is no longer one of the three modes"
        );
        // What `sbx prune` would sweep — read-only, and it never starts a sandbox. Every candidate
        // is a stopped sandbox from the list above.
        let prune = prune_preview().expect("sbx prune --dry-run --json");
        println!(
            "prune --dry-run: {} candidate(s), {} skipped",
            prune.would_remove.len(),
            prune.skipped().len()
        );
        for c in &prune.would_remove {
            println!(
                "  {} ({}) stopped {:?} {:?}",
                c.name, c.agent, c.stopped_at, c.workspaces
            );
            assert!(
                sandboxes
                    .iter()
                    .any(|s| s.name == c.name && !s.status.eq_ignore_ascii_case("running")),
                "{} is a prune candidate but not a stopped sandbox",
                c.name
            );
            assert!(
                c.stopped_at
                    .as_deref()
                    .and_then(crate::state::parse_rfc3339)
                    .is_some(),
                "{c:?}: stopped_at no longer parses"
            );
        }
        println!("ssh configured: {}", ssh_configured());
        // Approval requests, host-wide. A key that moves shows up here as an empty column, and a
        // shape that moves as a list the plain one disagrees with.
        let pending = approvals().expect("sbx policy approval ls --json");
        println!("{} approval request(s) pending", pending.len());
        for a in &pending {
            println!(
                "  {} {} → {} ({}) [{:?}]",
                a.sandbox,
                a.id,
                a.title,
                a.reason(),
                a.options
            );
            assert!(
                !a.id.is_empty() && !a.sandbox.is_empty() && !a.title.is_empty(),
                "{a:?}"
            );
        }
        let mut ids: Vec<String> = pending.iter().map(|a| a.id.clone()).collect();
        ids.sort();
        let mut quiet = quiet_names(&["policy", "approval", "ls", "-q"]);
        quiet.sort();
        assert_eq!(
            ids, quiet,
            "`approval ls --json` and `approval ls -q` disagree"
        );
        for entry in &sandboxes {
            let inspect = inspect(&entry.name).expect("sbx inspect --json");
            println!(
                "{}: agent={} status={} kits={} auth={:?} proxy={:?} policy={:?} daemon={:?}",
                entry.name,
                entry.agent,
                entry.status,
                inspect.kits.len(),
                inspect.auth_mode,
                inspect.proxy,
                inspect.network_policy.scope,
                inspect.daemon_version,
            );
            // `last_used_at` and `created_at` are what the rows and the Overview draw ages from, so
            // a format change has to fail here rather than silently blank them. Printed with what
            // the parser made of them.
            println!(
                "  created {:?} → {:?}, last used {:?} → {:?}",
                entry.created_at,
                entry
                    .created_at
                    .as_deref()
                    .and_then(crate::state::parse_rfc3339),
                entry.last_used_at,
                entry
                    .last_used_at
                    .as_deref()
                    .and_then(crate::state::parse_rfc3339),
            );
            for stamp in [entry.last_used_at.as_deref(), entry.created_at.as_deref()]
                .into_iter()
                .flatten()
            {
                assert!(
                    crate::state::parse_rfc3339(stamp).is_some(),
                    "{stamp:?} no longer parses — the rows would show no age"
                );
            }
            // The recorded limits (absent for "auto"), the unresponsive flag, and the mounts added
            // with `sbx mount`: what a rebuild repeats or a row warns about. Detached mode is on
            // both payloads, which have to agree.
            println!(
                "  cpus {:?} memory {:?} ({:?} MiB) unresponsive {} detached {} · {} sbx-mount mount(s): {:?}",
                entry.cpus,
                entry.memory,
                entry.memory_mib,
                entry.guest_unresponsive || inspect.guest_unresponsive,
                entry.detached,
                inspect.dynamic_mounts().len(),
                inspect.dynamic_mounts().iter().map(RuntimeMount::spec).collect::<Vec<_>>(),
            );
            assert_eq!(
                entry.detached, inspect.detached,
                "{}: `ls` and `inspect` disagree about detached mode",
                entry.name
            );
            assert_eq!(
                inspect.runtime_mounts.len(),
                inspect.dynamic_mounts().len(),
                "a runtime_mounts element no longer has the measured shape"
            );
            // What the Mounts tab lists: every bind mount, as host → target and access.
            let bound: Vec<String> = (inspect.bind_mounts().iter())
                .map(|m| format!("{} → {} ro={}", m.host_path, m.target(), m.read_only))
                .collect();
            println!("  {} host mount(s): {bound:?}", bound.len());
            assert_eq!(
                inspect.host_mounts.len(),
                bound.len(),
                "a host_mounts element no longer has the measured shape"
            );
            // Everything below is what the details tabs read; a shape change must fail loudly.
            let rules = policy_rules(&entry.name).expect("sbx policy ls --json");
            println!(
                "  {} rule(s): {} network, {} filesystem",
                rules.len(),
                rules.iter().filter(|r| r.is_network()).count(),
                rules.iter().filter(|r| r.is_filesystem()).count(),
            );
            assert!(
                rules.iter().all(|r| !r.decision.is_empty()),
                "every rule carries a decision"
            );
            let log = policy_log(&entry.name, 10).expect("sbx policy log --json");
            println!(
                "  log: {} allowed, {} blocked",
                log.allowed.len(),
                log.blocked.len()
            );
            for host in log.allowed.iter().chain(&log.blocked) {
                assert!(!host.host.is_empty() && host.count > 0, "{host:?}");
            }
            // `sbx ports --json` is a bare array and its field names are not in the official
            // docs, so print what came back: a renamed key shows up here as a zero column.
            let ports = ports(&entry.name).expect("sbx ports --json");
            println!("  {} published port(s)", ports.len());
            // The list entry carries them too, and a recreate reads them from there to publish them
            // again. The two have to agree, and both are empty while stopped.
            assert_eq!(
                entry.ports.len(),
                ports.len(),
                "{}: `ls --json` and `ports --json` disagree",
                entry.name
            );
            if !entry.ports.is_empty() {
                println!("    republish as: {:?}", publish_specs(&entry.ports));
            }
            for p in &ports {
                println!(
                    "    {} → {} ({}{})",
                    p.url(),
                    p.sandbox_port,
                    if p.protocol.is_empty() {
                        "tcp"
                    } else {
                        &p.protocol
                    },
                    if p.ephemeral { ", ephemeral" } else { "" }
                );
                assert!(p.sandbox_port > 0, "a mapping names a sandbox port");
            }
            for rule in rules.iter().filter(|r| r.is_network()) {
                println!(
                    "    {} {} [{}] {:?}",
                    rule.decision,
                    rule.resource_type,
                    rule.source(),
                    rule.targets()
                );
            }
            let check = check_host(&entry.name, "api.anthropic.com").expect("policy check");
            println!(
                "  api.anthropic.com → {} (implicit {})",
                check.detail, check.implicit
            );
            // What a rebuild has to pass back as `--deny-network`. Read off the same rules above,
            // so a scope or decision that is renamed shows up as an empty list here.
            let denies = deny_specs(&rules, &entry.name);
            println!("  {} own deny rule(s): {denies:?}", denies.len());
            // ...and what it re-adds after creating: HTTP rules, hand-added allows and answered
            // approvals. A query comes back as `--query`, which writes `subset` matching - a rule
            // with another mode would change meaning in a rebuild.
            for rule in rules_to_readd(&rules, &entry.name) {
                println!("  re-add on rebuild: {:?}", rule_argvs(rule, &entry.name));
                for target in &rule.http_targets {
                    assert!(
                        matches!(target.query_match.as_str(), "" | "subset"),
                        "{}: a query_match `rule_argvs` cannot re-make: {target:?}",
                        entry.name
                    );
                }
            }
            // A `--env`-less custom secret reports its name as "-", which the Overview renders as
            // "placeholder only" rather than a secret called "-".
            for secret in &inspect.secrets {
                println!("  secret {:?} ({})", secret.name, secret.source);
            }
            println!("  mcp gateway: {}", inspect.mcp_gateway);

            // `sbx exec` would start a stopped sandbox — never probe one.
            if entry.status.eq_ignore_ascii_case("running") {
                let sample = probe(&entry.name).expect("resource probe");
                println!(
                    "  {} vCPU, mem {}/{}, disk {}/{}, up {:.0}s",
                    sample.cpus,
                    crate::stats::gb(sample.mem_used_kb()),
                    crate::stats::gb(sample.mem_total_kb),
                    crate::stats::gb(sample.disk_used_kb),
                    crate::stats::gb(sample.disk_total_kb),
                    sample.uptime_s,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A create with nothing extra asked for: no `-p`, and no `--skills`. A macro, so the borrowed
    /// fields stay borrowed from the caller's locals instead of having to outlive the call.
    macro_rules! plain {
        ($name:expr, $agent:expr, $kits:expr, $ws:expr) => {
            Create {
                name: $name,
                agent: $agent,
                agent_kit: None,
                kit_dirs: $kits,
                workspace: $ws,
                mounts: &[],
                clone: false,
                ports: &[],
                skills: None,
                deny_network: &[],
                static_mcp: &[],
                cpus: None,
                memory: None,
                template: None,
            }
        };
    }

    #[test]
    fn every_sbx_call_gets_no_telemetry_unless_a_value_is_set() {
        assert_eq!(NO_TELEMETRY_ENV, "SBX_NO_TELEMETRY");
        assert_eq!(no_telemetry_value(None), Some("1"));
        assert_eq!(no_telemetry_value(Some(std::ffi::OsStr::new("0"))), None);
    }

    #[test]
    fn telemetry_is_turned_off_only_where_nobody_chose() {
        let at = |value: bool, source: &str| Setting {
            key: TELEMETRY_DISABLED.into(),
            value: serde_json::Value::Bool(value),
            source: source.into(),
            ..Setting::default()
        };
        assert!(telemetry_left_on(&at(false, "default")));
        assert!(!telemetry_left_on(&at(false, "override")), "chosen on");
        assert!(!telemetry_left_on(&at(false, "envvar")));
        assert!(!telemetry_left_on(&at(true, "override")), "off already");
        let other = Setting {
            key: CLIPBOARD_TEXT_COPY.into(),
            ..at(false, "default")
        };
        assert!(!telemetry_left_on(&other));
    }

    #[test]
    fn a_template_goes_just_before_the_agent() {
        let reference = "my-template-claude-anthropic:20261020t030000z";
        let argv = create_argv(Create {
            template: Some(reference),
            ..plain!("n", "claude", &[], Path::new("/proj"))
        });
        let agent = argv.iter().position(|a| a == "claude").unwrap();
        assert_eq!(argv[agent - 2], "--template");
        assert_eq!(argv[agent - 1], reference);
        let bare = create_argv(plain!("n", "claude", &[], Path::new("/proj")));
        assert!(!bare.iter().any(|a| a == "--template"), "{bare:?}");
        let n = bare.len();
        let expected = [
            &bare[..n - 2],
            &["--template".to_string(), reference.to_string()][..],
            &bare[n - 2..],
        ]
        .concat();
        assert_eq!(argv, expected, "only the two template words are new");
    }

    #[test]
    fn the_gate_names_the_version_it_refuses() {
        assert_eq!(gate("v0.51.0".into()).unwrap(), "v0.51.0");
        assert!(gate("v0.50.1".into()).is_err());
        let err = gate("v0.50.1".into()).unwrap_err().to_string();
        assert!(
            err.starts_with("this sbx is v0.50.1, but this dashboard needs 0.51.0+ — "),
            "{err}"
        );
    }

    #[test]
    fn version_gate_accepts_0510_and_newer() {
        assert!(meets_required_version("v0.51.0"));
        assert!(meets_required_version("0.51.0"));
        assert!(meets_required_version("v0.51.1"));
        assert!(meets_required_version("v0.52.0"));
        assert!(meets_required_version("v1.0.0"));
        assert!(!meets_required_version("v0.50.1"));
        assert!(!meets_required_version("v0.50.0"));
        assert!(!meets_required_version("v0.50"));
        assert!(!meets_required_version("v0.49.2"));
        assert!(!meets_required_version("v0.49.1"));
        assert!(!meets_required_version("v0.49.0"));
        assert!(!meets_required_version("v0.49"));
        assert!(!meets_required_version("v0.49.2-rc1"));
        assert!(!meets_required_version("v0.48.0"));
        assert!(!meets_required_version("v0.47.0"));
        assert!(!meets_required_version("v0.46.0"));
        assert!(!meets_required_version("v0.45.1"));
        assert!(!meets_required_version("v0.43.0"));
        assert!(!meets_required_version("v0.42.1"));
        assert!(!meets_required_version("v0.38.0"));
        // Unparseable → do not block the user.
        assert!(meets_required_version("dev"));
    }

    #[test]
    fn the_default_limits_shrink_on_a_small_host() {
        assert_eq!(default_cpus_for(None), 4);
        assert_eq!(default_cpus_for(Some(10)), 4);
        assert_eq!(default_cpus_for(Some(4)), 4);
        assert_eq!(default_cpus_for(Some(2)), 2);
        assert_eq!(
            default_cpus_for(Some(0)),
            1,
            "never 0, which would mean sbx's auto"
        );
        assert_eq!(default_memory_for(None), "4g");
        assert_eq!(default_memory_for(Some(16 * 1024)), "4g");
        assert_eq!(default_memory_for(Some(8 * 1024)), "4g");
        assert_eq!(default_memory_for(Some(6 * 1024)), "3g");
        assert_eq!(default_memory_for(Some(5000)), "2500m");
        assert_eq!(default_memory_for(Some(4 * 1024)), "2g");
        assert_eq!(default_memory_for(Some(700)), "512m", "sbx's minimum");
        // sbx's own ceiling is 75% of the host: the default stays below it on every host.
        for host in [700, 2048, 4096, 5000, 5461, 6144, 8192, 65536] {
            let m = default_memory_for(Some(host));
            let mib: u64 = match m.strip_suffix('g') {
                Some(g) => g.parse::<u64>().unwrap() * 1024,
                None => m.strip_suffix('m').unwrap().parse().unwrap(),
            };
            assert!(mib <= (host * 3 / 4).max(512), "{host} MiB host: {m}");
        }
    }

    #[test]
    fn host_memory_parses_from_sysctl_powershell_and_meminfo() {
        assert_eq!(bytes_to_mib("17179869184\n"), Some(16384));
        assert_eq!(
            bytes_to_mib("17179869184\r\n"),
            Some(16384),
            "PowerShell's line end"
        );
        assert_eq!(bytes_to_mib(""), None);
        assert_eq!(bytes_to_mib("0"), None);
        assert_eq!(bytes_to_mib("sysctl: unknown oid"), None);
        let meminfo = "MemTotal:        8254436 kB\nMemFree:  100 kB\n";
        assert_eq!(meminfo_total_mib(meminfo), Some(8060));
        assert_eq!(meminfo_total_mib("MemFree: 100 kB\n"), None);
    }

    /// The real reading on this host, which `cfg(test)` otherwise hides.
    #[test]
    #[ignore]
    fn live_host_memory_reads() {
        let mib = read_host_memory_mib();
        println!(
            "host memory {mib:?} MiB → default -m {}, CPUs {:?} → default --cpus {}",
            default_memory_for(mib),
            std::thread::available_parallelism().ok(),
            default_cpus_for(
                std::thread::available_parallelism()
                    .ok()
                    .and_then(|n| u32::try_from(n.get()).ok())
            )
        );
        assert!(mib.is_some_and(|m| m >= 512), "{mib:?}");
    }

    #[test]
    fn the_list_fields_parse_and_round_trip_into_a_rebuild() {
        let raw = br#"[
  {"name":"aisbx-p47-gw","id":"5212aef6-ec55-495f-893f-f903982ac052","agent":"claude",
   "status":"running","last_used_at":"2026-10-07T16:23:31.655354Z",
   "workspaces":["/w"],"cpus":2,"memory":"4g","memory_mib":4096,
   "created_at":"2026-10-07T16:23:20Z","detached":true},
  {"name":"plain","agent":"claude","status":"stopped","workspaces":["/w"],
   "created_at":"2026-07-24T16:02:07Z","workspace_missing":true}
]"#;
        let parsed: Vec<LsEntry> = parse_json(raw).unwrap();
        let (gw, plain) = (&parsed[0], &parsed[1]);
        assert!(gw.detached, "0.48 reports detached mode on the row");
        assert!(!plain.detached, "absent means not detached");
        assert_eq!(gw.cpus, Some(2));
        assert_eq!(gw.memory.as_deref(), Some("4g"));
        // Normalised to MiB for `-m`, which takes it back exactly.
        assert_eq!(gw.memory_spec().as_deref(), Some("4096m"));
        assert_eq!(
            gw.created_at
                .as_deref()
                .and_then(crate::state::parse_rfc3339),
            Some(1_791_390_200)
        );
        assert!(!gw.guest_unresponsive, "absent means responsive");
        assert_eq!(plain.cpus, None);
        assert_eq!(plain.memory_spec(), None, "no limit recorded is sbx's auto");
        assert!(plain.workspace_missing);

        let argv = create_argv(Create {
            cpus: gw.cpus,
            memory: gw.memory_spec().as_deref(),
            ..plain!("aisbx-p47-gw", "claude", &[], Path::new("/w"))
        });
        let agent = argv.iter().position(|a| a == "claude").unwrap();
        let cpus = argv.iter().position(|a| a == "--cpus").expect("--cpus");
        assert_eq!(argv[cpus + 1], "2");
        let mem = argv.iter().position(|a| a == "-m").expect("-m");
        assert_eq!(argv[mem + 1], "4096m");
        assert!(
            cpus < agent && mem < agent,
            "flags precede the agent: {argv:?}"
        );
        // …and aisbx's default where nothing was recorded.
        let bare = create_argv(plain!("plain", "claude", &[], Path::new("/w")));
        let flag = |argv: &[String], f: &str| {
            argv.iter()
                .position(|a| a == f)
                .map(|i| argv[i + 1].clone())
        };
        assert_eq!(flag(&bare, "--cpus").as_deref(), Some("4"), "{bare:?}");
        assert_eq!(flag(&bare, "-m").as_deref(), Some("4g"), "{bare:?}");
        // `0` and an empty memory are sbx's auto: no flag at all.
        let auto = create_argv(Create {
            cpus: Some(0),
            memory: Some(""),
            ..plain!("plain", "claude", &[], Path::new("/w"))
        });
        assert!(!auto.iter().any(|a| a == "--cpus" || a == "-m"), "{auto:?}");
    }

    #[test]
    fn runtime_mounts_are_the_sbx_mount_mounts_and_spell_back_into_sbx_mount() {
        let raw = br#"{"kits":["/k/opencode-litellm"],"runtime_mounts":[
            {"host_path":"/h/extra","container_target":"/h/extra","read_only":true},
            {"host_path":"/h/data","container_target":"/workspace/data","read_only":false},
            "something else entirely"],"guest_unresponsive":true}"#;
        let inspect: InspectEntry = parse_json(raw).unwrap();
        assert_eq!(
            inspect.kits.len(),
            1,
            "the kit list survives a strange element"
        );
        assert!(inspect.guest_unresponsive);
        let mounts = inspect.dynamic_mounts();
        assert_eq!(mounts.len(), 2);
        // Read-only at its own path: the target has to be named, or `ro` is read as one.
        assert_eq!(mounts[0].spec(), "/h/extra:/h/extra:ro");
        assert_eq!(mounts[1].spec(), "/h/data:/workspace/data");
        assert_eq!(mounts[1].target(), "/workspace/data");
        assert_eq!(
            mount_argv("s", &mounts[0]),
            vec!["sbx", "mount", "s", "/h/extra:/h/extra:ro"]
        );
        let at_own_path = RuntimeMount {
            host_path: "/h/x".into(),
            ..Default::default()
        };
        assert_eq!(at_own_path.spec(), "/h/x");
        assert_eq!(at_own_path.target(), "/h/x");
    }

    /// `sbx policy approval ls --json` with two held requests, pretty-printed as sbx prints it.
    const APPROVALS: &str = r#"[
  {
    "created_at": "2026-10-21T16:50:44.898858+02:00",
    "detail": "Protocol: TCP\nResource type: domain\nno matching allow rule (default deny)",
    "id": "network:HiCwaE1d0lPz-8YpcbZoi1vWVStbbPHu_f5wLZvrKIU",
    "options": [
      {
        "id": "allow",
        "label": "Allow"
      },
      {
        "id": "dismiss",
        "label": "Dismiss"
      }
    ],
    "sandbox_name": "aisbx-p49-sh",
    "title": "ports.ubuntu.com:80"
  },
  {
    "created_at": "2026-10-21T16:51:10.263657+02:00",
    "detail": "Protocol: TCP\nResource type: domain\nno matching allow rule (default deny)",
    "id": "network:njriXdTy763ngoZrIY38brSheWmRXby7Wb3siQDKXdc",
    "options": [
      {
        "id": "allow",
        "label": "Allow"
      },
      {
        "id": "dismiss",
        "label": "Dismiss"
      }
    ],
    "sandbox_name": "aisbx-p49-sh",
    "title": "example.org:443"
  }
]
"#;

    #[test]
    fn pending_approvals_are_read_from_the_049_json() {
        let approvals: Vec<Approval> =
            parse_json(APPROVALS.as_bytes()).expect("the 0.49 JSON parses");
        assert_eq!(approvals.len(), 2);
        let first = &approvals[0];
        assert_eq!(
            first.id,
            "network:HiCwaE1d0lPz-8YpcbZoi1vWVStbbPHu_f5wLZvrKIU"
        );
        assert_eq!(first.sandbox, "aisbx-p49-sh", "read from `sandbox_name`");
        assert_eq!(first.title, "ports.ubuntu.com:80");
        // The detail is one fact per line; the reason is the line that is not a label.
        assert_eq!(first.reason(), "no matching allow rule (default deny)");
        assert!(first.default_deny());
        assert!(first.offers("allow") && first.offers("dismiss"));
        assert!(!first.offers("deny"));
        assert_eq!(first.options[0].label, "Allow");
        assert_eq!(approvals[1].title, "example.org:443");
        assert_eq!(
            approval_respond_argv(&first.id, "allow"),
            vec![
                "sbx",
                "policy",
                "approval",
                "respond",
                "network:HiCwaE1d0lPz-8YpcbZoi1vWVStbbPHu_f5wLZvrKIU",
                "--option",
                "allow"
            ]
        );

        // An organization's reason is kept whole, and a detail with nothing but labels is shown as
        // it is, on one line.
        let org = Approval {
            detail: "Protocol: TCP\nResource type: domain\napproval required by policy \"x\""
                .into(),
            ..first.clone()
        };
        assert_eq!(org.reason(), "approval required by policy \"x\"");
        assert!(!org.default_deny(), "a policy's reason is worth reading");
        let bare = Approval {
            detail: "Protocol: TCP\nResource type: ip".into(),
            ..first.clone()
        };
        assert_eq!(bare.reason(), "Protocol: TCP Resource type: ip");
        assert!(!bare.default_deny());

        // Nothing pending is an empty array - and a table, or any other text, is an error, which
        // the refresh does not mistake for "nothing pending".
        assert_eq!(parse_json::<Vec<Approval>>(b"[]\n").unwrap(), Vec::new());
        assert!(parse_json::<Vec<Approval>>(b"No pending approvals.\n").is_err());
        let table = concat!(
            "APPROVAL    SANDBOX   TITLE             DETAIL          OPTIONS\n",
            "network:x   s         example.org:443   Protocol: TCP   allow (Allow)\n",
        );
        assert!(parse_json::<Vec<Approval>>(table.as_bytes()).is_err());
    }

    #[test]
    fn a_reachability_check_reads_the_json_and_tells_a_held_request_from_a_denied_one() {
        let allowed: CheckOutput = parse_json(br#"{"action":"net:connect:tcp","allowed":true,"context":"sandbox:s","governance":{"active":false},"resource_type":"net:domain","resource_value":"api.anthropic.com:443","target":"api.anthropic.com:443","type":"network"}"#).unwrap();
        let r = reachability(&allowed, "api.anthropic.com");
        assert!(r.allowed && !r.implicit);
        assert_eq!(r.detail, "Allowed: api.anthropic.com:443");

        let denied: CheckOutput = parse_json(br#"{"allowed":false,"deny_kind":"explicit","origin":"local","reason":"Denied by local rule","rule":"local:0430fd72","target":"ads.example.com:443"}"#).unwrap();
        let r = reachability(&denied, "ads.example.com");
        assert!(!r.allowed && !r.implicit);
        assert_eq!(
            r.detail,
            "Denied: ads.example.com:443 — denied by local rule"
        );

        // No rule at all: the request a sandbox would hold for approval.
        let implicit: CheckOutput = parse_json(br#"{"allowed":false,"deny_kind":"implicit","reason":"No matching allow rule (default deny)","target":"neverland.example:443"}"#).unwrap();
        let r = reachability(&implicit, "neverland.example");
        assert!(!r.allowed && r.implicit);
        assert!(
            r.detail.starts_with("Denied: neverland.example:443"),
            "{}",
            r.detail
        );
    }

    /// The rules of one sandbox, as sbx reports them: its kit's allow-list and deny list, a
    /// `--deny-network` deny, HTTP rules (two of them with a query), an answered approval and a
    /// global preset.
    fn rules_fixture() -> Vec<PolicyRule> {
        let raw = br#"{"rules":[
          {"id":"93a4","name":"kit:s","scope":"sandbox:s","applies_to":"sandbox:s","resource_type":"network","decision":"allow","resources":["api.anthropic.com:443","**"],"origin":"scoped","layer":"local","status":"active","editable":false,"sandbox_id":"s","provenance":{"created_via":"provisioned","source":"sandboxes"},"actions":["net:connect:tcp"]},
          {"id":"94fb","name":"kit:s:deny","policy_id":"bab4","scope":"sandbox:s","applies_to":"sandbox:s","resource_type":"network","decision":"deny","resources":["play.example.com"],"origin":"scoped","layer":"local","status":"active","editable":false,"sandbox_id":"s","provenance":{"created_via":"added"},"actions":["net:connect:tcp","net:connect:udp"]},
          {"id":"0430","name":"0430","scope":"sandbox:s","applies_to":"sandbox:s","resource_type":"network","decision":"deny","resources":["ads.example.com"],"origin":"scoped","status":"active","provenance":{"created_via":"added"},"actions":["net:connect:tcp","net:connect:udp"]},
          {"id":"c29a","name":"c29a","scope":"sandbox:s","applies_to":"sandbox:s","resource_type":"http","decision":"deny","resources":["api.example.com"],"origin":"scoped","status":"active","provenance":{"created_via":"added"},"actions":["http:request:post"],"methods":["POST"],"http_targets":[{"host":"api.example.com","path":"/admin/**"}]},
          {"id":"fdb1","name":"fdb1","scope":"sandbox:s","applies_to":"sandbox:s","resource_type":"http","decision":"allow","resources":["api.example.org"],"origin":"scoped","status":"active","provenance":{"created_via":"added"},"actions":["http:request:get","http:request:head"],"methods":["GET","HEAD"],"http_targets":[{"host":"api.example.org","path":"/v1/**"}]},
          {"id":"d231","name":"approval:d231","scope":"sandbox:s","applies_to":"sandbox:s","resource_type":"network","decision":"allow","resources":["example.org:443"],"origin":"scoped","status":"active","provenance":{"created_via":"approval"},"actions":["net:connect:tcp"]},
          {"id":"c3c3","name":"c3c3","policy_id":"4621","scope":"sandbox:s","applies_to":"sandbox:s","resource_type":"http","decision":"allow","resources":["api.example.com"],"origin":"scoped","layer":"local","status":"active","editable":true,"sandbox_id":"s","provenance":{"created_via":"added"},"actions":["http:request:get"],"methods":["GET"],"http_targets":[{"host":"api.example.com","path":"/search","query":[{"name":"team","value_pattern":"platform"},{"name":"label","value_pattern":"release*"}],"query_match":"subset"}]},
          {"id":"a37d","name":"a37d","policy_id":"4621","scope":"sandbox:s","applies_to":"sandbox:s","resource_type":"http","decision":"allow","resources":["api.example.net"],"origin":"scoped","layer":"local","status":"active","editable":true,"sandbox_id":"s","provenance":{"created_via":"added"},"actions":["http:request:*"],"http_targets":[{"host":"api.example.net","path":"/**","query":[{"name":"k","value_pattern":"%2A"},{"name":"k","value_pattern":"a+b"}],"query_match":"subset"}]},
          {"id":"default-fs-read-allow-all","name":"default-fs-read-allow-all","scope":"global","applies_to":"all","resource_type":"filesystem:read","decision":"allow","resources":["**"],"origin":"local","provenance":{"created_via":"added"},"actions":["fs:mount:read"]},
          {"id":"default-deny-all","name":"default-deny-all","policy_name":"default-deny-all","scope":"global","applies_to":"all","resource_type":"network","decision":"deny","resources":["**"],"origin":"local","layer":"local","status":"active","reason":"Implicit fallback when no applicable policy decides the request.","editable":false,"actions":["net:connect:tcp","net:connect:udp"]}
        ]}"#;
        parse_json::<PolicyLsOutput>(raw).unwrap().rules
    }

    #[test]
    fn rules_say_where_they_came_from_and_what_they_match() {
        let rules = rules_fixture();
        let by = |name: &str| rules.iter().find(|r| r.name == name).unwrap();
        // A kit's rule reports `origin: scoped`; the provenance is what says "kit" - and for the
        // kit's deny list, which sbx reports as `added`, the name.
        assert_eq!(by("kit:s").source(), "kit");
        assert_eq!(by("kit:s:deny").source(), "kit");
        assert!(
            !by("kit:s:deny").owned_by("s"),
            "the kit puts its deny back by itself"
        );
        assert_eq!(by("0430").source(), "added");
        assert_eq!(by("approval:d231").source(), "approved");
        // HTTP rules belong on the Network tab, spelled with their method and path.
        assert!(by("c29a").is_network() && by("c29a").is_http());
        assert_eq!(by("c29a").targets(), vec!["POST api.example.com/admin/**"]);
        assert_eq!(by("fdb1").targets(), vec!["GET,HEAD api.example.org/v1/**"]);
        // The query, spelled as sbx's own CLI prints the rule; no `methods` means any.
        assert_eq!(
            by("c3c3").targets(),
            vec!["GET api.example.com/search?team=platform&label=release*"]
        );
        assert_eq!(
            by("a37d").targets(),
            vec!["ANY api.example.net/**?k=%2A&k=a+b"]
        );
        assert_eq!(by("a37d").http_targets[0].query_match, "subset");
        assert_eq!(by("0430").targets(), vec!["ads.example.com"]);
        assert!(by("0430").covers_udp(), "a 0.45 deny covers UDP as well");
        assert!(!by("kit:s").covers_udp());
        assert!(!by("default-fs-read-allow-all").is_network());
        // Owned: scoped to the sandbox and not the kit's — what `sbx rm` takes with it.
        assert!(
            !by("kit:s").owned_by("s"),
            "the kit puts its own rules back"
        );
        assert!(by("0430").owned_by("s") && by("approval:d231").owned_by("s"));
        assert!(!by("0430").owned_by("other"));
        assert!(!by("default-fs-read-allow-all").owned_by("s"));
        // The fallback decides what no rule matches: listed, never a rule of the sandbox's.
        let fallback = by(FALLBACK_RULE);
        assert!(fallback.is_fallback() && fallback.is_network() && !fallback.allows());
        assert_eq!(fallback.source(), "fallback");
        assert!(!fallback.owned_by("s"));
        assert!(rules.iter().filter(|r| r.is_fallback()).count() == 1);
    }

    #[test]
    fn a_rebuild_puts_back_every_rule_the_sandbox_owns_in_the_shape_it_had() {
        let rules = rules_fixture();
        assert_eq!(deny_specs(&rules, "s"), vec!["ads.example.com"]);
        let readd: Vec<&str> = rules_to_readd(&rules, "s")
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(readd, vec!["c29a", "fdb1", "approval:d231", "c3c3", "a37d"]);
        let argvs: Vec<Vec<String>> = rules_to_readd(&rules, "s")
            .into_iter()
            .flat_map(|r| rule_argvs(r, "s"))
            .collect();
        assert_eq!(
            argvs,
            vec![
                vec![
                    "sbx",
                    "policy",
                    "deny",
                    "network",
                    "--sandbox",
                    "s",
                    "api.example.com",
                    "--method",
                    "POST",
                    "--path",
                    "/admin/**"
                ],
                vec![
                    "sbx",
                    "policy",
                    "allow",
                    "network",
                    "--sandbox",
                    "s",
                    "api.example.org",
                    "--method",
                    "GET,HEAD",
                    "--path",
                    "/v1/**"
                ],
                vec![
                    "sbx",
                    "policy",
                    "allow",
                    "network",
                    "--sandbox",
                    "s",
                    "example.org:443"
                ],
                vec![
                    "sbx",
                    "policy",
                    "allow",
                    "network",
                    "--sandbox",
                    "s",
                    "api.example.com",
                    "--method",
                    "GET",
                    "--path",
                    "/search",
                    "--query",
                    "team=platform",
                    "--query",
                    "label=release*"
                ],
                // Any method, the whole path, and a repeated name kept twice, still URL-encoded.
                vec![
                    "sbx",
                    "policy",
                    "allow",
                    "network",
                    "--sandbox",
                    "s",
                    "api.example.net",
                    "--method",
                    "ANY",
                    "--path",
                    "/**",
                    "--query",
                    "k=%2A",
                    "--query",
                    "k=a+b"
                ],
            ]
        );
        // A UDP allow keeps its protocol; a TCP one (the default) says nothing.
        let udp = PolicyRule {
            scope: "sandbox:s".into(),
            resource_type: "network".into(),
            decision: "allow".into(),
            resources: vec!["dns.example:53".into()],
            actions: vec!["net:connect:tcp".into(), "net:connect:udp".into()],
            ..Default::default()
        };
        assert_eq!(rule_argvs(&udp, "s")[0].last().unwrap(), "tcp,udp");
        assert!(rules_to_readd(&rules, "nobody").is_empty());
    }

    #[test]
    fn keeping_a_sandbox_running_is_a_detached_run_of_the_existing_one() {
        assert_eq!(
            keep_running_argv("s"),
            vec!["sbx", "run", "--detach", "--name", "s"]
        );
        assert!(!keep_running_argv("s").iter().any(|a| a == "--detached"));
    }

    #[test]
    fn inspect_reports_detached_mode_too() {
        let raw = br#"{"name":"aisbx-p48-sh","agent":"shell","state":"stopped","detached":true,
            "secrets":[{"name":"mcpgateway","source":"uploaded"}],"mcp_gateway":true,
            "runtime_mounts":[],"sessions":0,"daemon_version":"v0.48.0"}"#;
        let inspect: InspectEntry = parse_json(raw).unwrap();
        assert!(inspect.detached);
        assert!(!parse_json::<InspectEntry>(b"{}").unwrap().detached);
    }

    #[test]
    fn no_input_goes_right_after_sbx_once() {
        let own = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        let argv = own(&["sbx", "exec", "box", "agent", "--no-input"]);
        let out = no_input(&argv);
        assert_eq!(
            out,
            own(&["sbx", "--no-input", "exec", "box", "agent", "--no-input"]),
            "before the subcommand, the agent's own argv untouched"
        );
        assert_eq!(no_input(&out), out, "once");
        let git = own(&["git", "status"]);
        assert_eq!(no_input(&git), git, "not an sbx call");
        assert!(no_input(&[]).is_empty());
    }

    #[test]
    fn a_setting_reads_as_sbx_prints_it_and_is_set_without_a_question() {
        let raw = br#"{"default": false,
          "description": "Disable telemetry. SBX_NO_TELEMETRY=1 also disables telemetry.",
          "key": "telemetry.disabled", "requires_restart": true, "source": "override",
          "type": "bool", "value": true}"#;
        let setting: Setting = parse_json(raw).unwrap();
        assert_eq!(setting.key, TELEMETRY_DISABLED);
        assert_eq!(setting.flag(), Some(true));
        assert_eq!(setting.source, "override");
        assert!(setting.requires_restart);
        let paste = br#"{"default": false,
          "description": "Allow sandboxed agents to read the host clipboard for image paste (Ctrl+V).",
          "env_var": "DOCKER_SANDBOXES_CLIPBOARD_IMAGE_PASTE", "key": "clipboard.imagePaste",
          "source": "default", "type": "bool", "value": false}"#;
        let mut paste: Setting = parse_json(paste).unwrap();
        assert_eq!(paste.env_var, "DOCKER_SANDBOXES_CLIPBOARD_IMAGE_PASTE");
        assert!(!paste.from_env() && !paste.requires_restart);
        paste.source = "envvar".into();
        assert!(paste.from_env(), "sbx's docs: the environment's value wins");
        let text: Setting = parse_json(br#"{"key":"proxy","value":"http://p:3128"}"#).unwrap();
        assert_eq!(text.flag(), None, "not a boolean setting");
        assert_eq!(
            setting_set_argv(TELEMETRY_DISABLED, "true"),
            ["sbx", "settings", "set", "telemetry.disabled", "true"]
        );
    }

    #[test]
    fn inspect_lists_every_bind_mount_and_skips_a_bad_element() {
        let raw = br#"{"name":"box","host_mounts":[
            {"container_target":"/w","host_path":"/w","read_only":false},
            {"container_target":"/workspace/data","host_path":"/h/data","read_only":true},
            {"host_path":42},
            {"container_target":"/x","host_path":"","read_only":true}
        ],"runtime_mounts":[]}"#;
        let inspect: InspectEntry = parse_json(raw).unwrap();
        let mounts = inspect.bind_mounts();
        assert_eq!(
            mounts.len(),
            2,
            "a malformed and an empty element are skipped"
        );
        assert_eq!(mounts[1].host_path, "/h/data");
        assert_eq!(mounts[1].target(), "/workspace/data");
        assert!(mounts[1].read_only && !mounts[0].read_only);
        assert!(parse_json::<InspectEntry>(b"{}")
            .unwrap()
            .bind_mounts()
            .is_empty());
    }

    #[test]
    fn inspect_reports_the_release_that_created_the_sandbox() {
        let raw = br#"{"name":"aisbx-probe-wd","agent":"probe-wd-kit","state":"running",
            "created_version":"v0.51.0","uptime":"34s","mcp_gateway":true,"runtime_mounts":[],
            "sessions":0,"daemon_version":"v0.51.0","daemon_uptime":"14m",
            "host_mounts":[{"container_target":"/w","host_path":"/w","read_only":false}],
            "working_dir":"","effective_working_dir":"/w"}"#;
        let inspect: InspectEntry = parse_json(raw).unwrap();
        assert_eq!(inspect.created_version, "v0.51.0");
        assert_eq!(inspect.daemon_version, "v0.51.0");
        let older: InspectEntry = parse_json(br#"{"daemon_version":"v0.51.0"}"#).unwrap();
        assert!(older.created_version.is_empty());
    }

    #[test]
    fn the_version_payload_reads_the_client_half() {
        let raw = br#"{"client":{"version":"v0.42.1","revision":"cc6e400","build_tags":"cloud"},"server":{"state":"unavailable","error":"daemon not running"}}"#;
        let parsed: VersionOutput = parse_json(raw).unwrap();
        assert_eq!(parsed.client.version, "v0.42.1");
    }

    #[test]
    fn skips_daemon_startup_noise_before_json() {
        let raw = b"Starting sandboxd daemon...\nDaemon started (PID: 1)\n[{\"name\":\"a\",\"agent\":\"claude\",\"status\":\"stopped\",\"workspaces\":[\"/w\"]}]";
        let parsed: Vec<LsEntry> = parse_json(raw).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "a");
    }

    #[test]
    fn decodes_a_bare_array_and_ignores_noise_on_both_sides() {
        // `sbx ports --json` answers with an array, not an object.
        let ports: Vec<Port> = parse_json(b"[]").unwrap();
        assert!(ports.is_empty());

        // A bracket inside a notice line is not where the payload starts — the payload starts
        // its own line, and whatever follows it is ignored.
        let raw = b"note [1] pending\n[{\"host_ip\":\"127.0.0.1\",\"host_port\":8080,\"sandbox_port\":3000,\"protocol\":\"tcp\"}]\ndone";
        let ports: Vec<Port> = parse_json(raw).unwrap();
        assert_eq!(ports.len(), 1);
        // The protocol is kept even for `tcp`: a bare spec would mean `tcp4`.
        assert_eq!(ports[0].spec(), "8080:3000/tcp");
        assert_eq!(ports[0].url(), "http://127.0.0.1:8080");
    }

    #[test]
    fn a_top_level_that_does_not_fit_is_an_error_not_a_nested_match() {
        // An object top level holding the rows, pretty-printed as sbx prints JSON.
        let object_ls = b"{\n  \"sandboxes\": [\n    {\n      \"name\": \"a\",\n      \"workspaces\": []\n    }\n  ]\n}\n";
        assert!(parse_json::<Vec<LsEntry>>(object_ls).is_err());

        #[derive(Debug, Deserialize)]
        struct ObjectShape {
            #[serde(default)]
            #[allow(dead_code)]
            sandboxes: Vec<LsEntry>,
        }
        // The row array, against a reader shaped like that object.
        let new_ls =
            b"[\n  {\n    \"name\": \"a\",\n    \"workspaces\": [\n      \"/w\"\n    ]\n  }\n]\n";
        assert!(parse_json::<ObjectShape>(new_ls).is_err());
        assert_eq!(
            parse_json::<Vec<LsEntry>>(new_ls).unwrap()[0].workspaces,
            vec!["/w"]
        );
    }

    #[test]
    fn the_048_policy_log_parses() {
        let raw = br#"{
  "allowed": [
    {"sandbox_id": "aisbx-p48-sh", "host": "github.com:443", "proxy_type": "forward-bypass",
     "count": 3, "first_seen": "2026-10-17T23:00:46.33636+02:00",
     "last_seen": "2026-10-17T23:00:47.273277+02:00"}
  ],
  "blocked": [
    {"sandbox_id": "aisbx-p48-sh", "host": "example.org:443", "proxy_type": "forward",
     "rule": "no applicable policies for op(action=net:connect:tcp, resource=net:domain:example.org:443)",
     "reason": "No matching allow rule (default deny)", "count": 1,
     "first_seen": "2026-10-17T23:01:10.189433+02:00",
     "last_seen": "2026-10-17T23:01:10.189433+02:00"}
  ]
}"#;
        let log: PolicyLog = parse_json(raw).unwrap();
        assert!(!log.is_empty());
        assert_eq!(log.allowed[0].host, "github.com:443");
        assert_eq!(log.allowed[0].count, 3);
        assert!(
            log.allowed[0].why().is_empty(),
            "an allowed host carries no rule"
        );
        let blocked = &log.blocked[0];
        assert_eq!(blocked.count, 1);
        assert!(blocked.default_deny());
        assert_eq!(blocked.why(), "No matching allow rule (default deny)");
        assert_eq!(blocked.last_seen, "2026-10-17T23:01:10.189433+02:00");
        // An empty log is two empty lists.
        let empty: PolicyLog =
            parse_json(b"{\n  \"allowed\": [],\n  \"blocked\": []\n}\n").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn the_prune_preview_is_read_as_json() {
        let raw = br#"{
  "would_remove": [
    {"name": "claude-anthropic-shop", "agent": "claude",
     "stopped_at": "2026-10-16T22:57:21Z", "workspaces": ["/Users/u/src/shop"]},
    {"name": "docs-agy", "agent": "agy", "stopped_at": "2026-10-10T18:48:28Z",
     "workspaces": ["/Users/u/src/docs"]}
  ],
  "skipped_unknown_stop": []
}"#;
        let preview: PrunePreview = parse_json(raw).unwrap();
        assert_eq!(preview.would_remove.len(), 2);
        assert_eq!(preview.would_remove[1].name, "docs-agy");
        assert_eq!(preview.would_remove[1].agent, "agy");
        assert_eq!(
            preview.would_remove[0]
                .stopped_at
                .as_deref()
                .and_then(crate::state::parse_rfc3339),
            Some(1_792_191_441)
        );
        assert!(preview.skipped().is_empty());
        // The skipped list's element shape is unknown; a name is taken out of either a string or
        // an object, and anything else is passed over rather than failing the preview.
        let skipped: PrunePreview =
            parse_json(br#"{"would_remove": [], "skipped_unknown_stop": ["a", {"name": "b"}, 7]}"#)
                .unwrap();
        assert_eq!(skipped.skipped(), vec!["a", "b"]);
    }

    #[test]
    fn port_specs_round_trip_into_the_unpublish_grammar() {
        let udp = Port {
            host_port: 5000,
            sandbox_port: 5000,
            protocol: "udp".into(),
            ..Default::default()
        };
        assert_eq!(udp.spec(), "5000:5000/udp");
        assert_eq!(
            ports_unpublish_argv("s", &udp.spec()).last().unwrap(),
            "5000:5000/udp"
        );
        // An IPv6 host binding has to be bracketed before it can be pasted into a browser.
        let v6 = Port {
            host_ip: "::1".into(),
            host_port: 8080,
            sandbox_port: 80,
            ..Default::default()
        };
        assert_eq!(v6.url(), "http://[::1]:8080");
    }

    /// `sbx secret ls --json` as sbx prints it, plus one scoped service row.
    fn secrets_fixture() -> Secrets {
        let raw = format!(
            r#"{{
              "secrets": [
                {{"scope": "global", "type": "service", "name": "litellm", "secret": "(stored)"}},
                {{"scope": "global", "type": "service", "name": "anthropic", "secret": "(oauth configured)"}},
                {{"scope": "claude-ki", "type": "service", "name": "openai", "secret": "(stored)"}}
              ],
              "custom_secrets": [
                {{"scope": "global", "targets": ["{host}"], "placeholder": "{private}", "secret": "sk-ant********"}},
                {{"scope": "global", "targets": ["{host}"], "placeholder": "{work}", "secret": "sk-ant********"}},
                {{"scope": "global", "targets": ["api.example.com", "api.other.io"], "env": "SOME_KEY", "placeholder": "someone-elses-secret", "secret": "sk***"}},
                {{"scope": "claude-token-old", "targets": ["{host}"], "env": "{env}", "placeholder": "sbx-cs-aisbx-claude-token-old", "secret": "sk-ant***TEST"}}
              ],
              "shadowed_services": [],
              "env_only_count": 0
            }}"#,
            host = TOKEN_HOST,
            env = TOKEN_ENV,
            private = named_token_placeholder("private"),
            work = named_token_placeholder("work"),
        );
        parse_json(raw.as_bytes()).expect("the 0.42 payload decodes")
    }

    #[test]
    fn reads_the_042_secret_payload() {
        let secrets = secrets_fixture();
        assert_eq!(
            secrets
                .services
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["litellm", "anthropic", "openai"]
        );
        // The masked value is sbx's own rendering, shown verbatim to say *how* the credential
        // was stored — `(oauth configured)` is the one value that is read rather than just shown.
        assert_eq!(secrets.service("litellm").unwrap().masked, "(stored)");
        assert_eq!(secrets.oauth().unwrap().masked, "(oauth configured)");
        assert!(secrets.oauth().unwrap().is_oauth());
        assert!(!secrets.service("litellm").unwrap().is_oauth());
        assert!(secrets.oauth().unwrap().is_global());
        // A scoped service row keeps its scope: it is a different secret from the global one.
        let scoped = secrets
            .services
            .iter()
            .find(|s| s.name == "openai")
            .unwrap();
        assert!(!scoped.is_global());
        assert_eq!(scoped.scope, "claude-ki");
        assert_eq!(scoped.kind, "service");

        assert_eq!(secrets.custom.len(), 4);
        let scoped = secrets.custom.last().unwrap();
        assert_eq!(scoped.scope, "claude-token-old");
        assert_eq!(scoped.env, TOKEN_ENV);
        assert_eq!(scoped.placeholder, "sbx-cs-aisbx-claude-token-old");
        // A `--env`-less row (a named token) simply has no `env` key.
        assert_eq!(secrets.custom[0].env, "");
        assert_eq!(
            secrets.custom[2].targets_label(),
            "api.example.com, api.other.io"
        );
        assert!(secrets.has_service("anthropic"));
        assert!(!secrets.has_service("claude-token-old"));

        // An empty store is an object with two empty lists, never prose.
        let empty: Secrets = parse_json(
            br#"{"secrets":[],"custom_secrets":[],"shadowed_services":[],"env_only_count":0}"#,
        )
        .unwrap();
        assert!(empty.services.is_empty() && empty.custom.is_empty());
    }

    #[test]
    fn the_token_secret_never_carries_its_value_in_argv() {
        // Global, spelled by saying nothing at all: global is the default and `--global` is
        // deprecated. No `--value` / `-t`: the token goes in on stdin, so `ps` cannot show it.
        let named = secret_set_named_token_argv(&named_token_placeholder("private"));
        assert!(!named.contains(&"-g".to_string()), "{named:?}");
        assert!(!named.contains(&"--sandbox".to_string()), "{named:?}");
        assert!(!named.iter().any(|a| a == "--value" || a == "-t"));
        assert_eq!(named.last().map(String::as_str), Some("--value-stdin"));
        assert!(
            !named.iter().any(|a| a == "--env"),
            "a global --env row lands in EVERY sandbox: {named:?}"
        );
        assert!(named.contains(&TOKEN_HOST.to_string()), "{named:?}");
    }

    #[test]
    fn named_tokens_are_read_back_off_the_global_rows() {
        let placeholder = named_token_placeholder("private");
        assert_eq!(token_name_of(&placeholder), Some("private"));
        assert_eq!(token_name_of("sbx-cs-aisbx-claude-token-ki"), None);
        assert_eq!(token_name_of("sbx-cs-aisbx-tok-"), None);

        let secrets = secrets_fixture();
        assert_eq!(
            secrets.token_names(),
            vec!["private", "work"],
            "someone else's global custom secret is not a token of ours"
        );
        assert!(
            secrets.holds(&placeholder),
            "the global row holds the value"
        );
        assert!(
            secrets.holds("sbx-cs-aisbx-claude-token-old"),
            "so does a scoped row"
        );
        assert!(!secrets.holds(&named_token_placeholder("gone")));
    }

    #[test]
    fn litellm_keys_are_global_rows_read_back_by_placeholder_and_host() {
        let set = secret_set_litellm_key_argv("litellm.example.com", "sbx-cs-aisbx-llm-acme");
        assert_eq!(
            set,
            [
                "sbx",
                "secret",
                "set-custom",
                "--host",
                "litellm.example.com",
                "--placeholder",
                "sbx-cs-aisbx-llm-acme",
                "--value-stdin"
            ]
        );
        assert!(!set
            .iter()
            .any(|a| a == "--env" || a == "--sandbox" || a == "--value"));
        assert_eq!(
            secret_rm_litellm_key_argv("sbx-cs-aisbx-llm-acme"),
            [
                "sbx",
                "secret",
                "rm",
                "--placeholder",
                "sbx-cs-aisbx-llm-acme",
                "-f"
            ]
        );
        let row = |scope: &str, host: &str, placeholder: &str| CustomSecret {
            scope: scope.into(),
            targets: vec![host.into()],
            env: String::new(),
            placeholder: placeholder.into(),
            masked: "sk-***".into(),
        };
        let secrets = Secrets {
            services: Vec::new(),
            custom: vec![
                row(GLOBAL_SCOPE, "litellm.example.com", "sbx-cs-aisbx-llm-acme"),
                row(GLOBAL_SCOPE, "old.example.com", "sbx-cs-aisbx-llm-moved"),
                row(GLOBAL_SCOPE, "x.example.com", "sbx-cs-aisbx-llm-orphan"),
                row("some-box", "litellm.example.com", "sbx-cs-aisbx-llm-scoped"),
            ],
        };
        assert_eq!(
            secrets.litellm_key_state("acme", "litellm.example.com"),
            Some(true)
        );
        assert_eq!(
            secrets.litellm_key_state("moved", "new.example.com"),
            Some(false)
        );
        assert_eq!(secrets.litellm_key_state("absent", "h"), None);
        // A sandbox-scoped row is not a connection's key.
        assert!(secrets.litellm_key("scoped").is_none());
        let names: Vec<&str> = secrets.litellm_keys().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["acme", "moved", "orphan"]);
        // The global rows are not listed as foreign; the scoped one is.
        let foreign: Vec<&str> = secrets
            .foreign_custom()
            .iter()
            .map(|c| c.placeholder.as_str())
            .collect();
        assert_eq!(foreign, ["sbx-cs-aisbx-llm-scoped"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_kit_script_runs_with_its_arguments_else_exits_127() {
        let dir = std::env::temp_dir().join(format!("aisbx-kit-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("hook.sh");
        std::fs::write(&script, "echo hook \"$@\"\n").unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("sh")
                .args(["-c", KIT_SCRIPT, "sh"])
                .arg(&script)
                .args(args)
                .output()
                .unwrap();
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            )
        };
        assert_eq!(run(&["a b", "$c"]), (Some(0), "hook a b $c".to_string()));
        std::fs::remove_file(&script).unwrap();
        assert_eq!(run(&[]).0, Some(127));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn written_files_come_out_as_they_went_in() {
        let dir = std::env::temp_dir().join(format!("aisbx-write-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (a, b) = (dir.join("deep/a.json"), dir.join("b.json"));
        let big: String = "é$`\\\"'%s\n".repeat(9000);
        let files = [
            (a.display().to_string(), big.clone()),
            (b.display().to_string(), "-n %d\n".to_string()),
        ];
        let argv = write_files_argv("box", &files);
        assert_eq!(&argv[..3], ["sbx", "exec", "box"]);
        // Linux refuses a single argument over 128 KiB; keep every one at half of that.
        assert!(
            big.len() > 64 * 1024,
            "the big file needs more than one argument"
        );
        assert!(
            argv.iter().all(|arg| arg.len() <= 64 * 1024),
            "every argument is short"
        );
        let out = std::process::Command::new("sh")
            .args(&argv[4..])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), big);
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "-n %d\n");
        assert!(!dir.join("b.json.aisbx-tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn merged_settings_keep_every_other_key() {
        let dir = std::env::temp_dir().join(format!("aisbx-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("deep/settings.json");
        let path = file.display().to_string();
        let merge = |json: &str| {
            let argv = merge_settings_argv("box", &path, json);
            assert_eq!(&argv[..3], ["sbx", "exec", "box"]);
            assert!(argv.iter().all(|arg| arg.len() <= 64 * 1024));
            std::process::Command::new("sh")
                .args(&argv[4..])
                .output()
                .unwrap()
        };
        let read = || -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap()
        };
        // A missing file starts empty.
        assert!(merge(r#"{"a": 1}"#).status.success());
        assert_eq!(read(), serde_json::json!({"a": 1}));
        std::fs::write(
            &file,
            r#"{"env": {"X": "1"}, "modelPicker": {"old": true}}"#,
        )
        .unwrap();
        let big = serde_json::json!({"modelPicker": {"options": ["é$`\\\"'%s\n".repeat(9000)]}});
        let out = merge(&big.to_string());
        assert!(out.status.success(), "{out:?}");
        assert_eq!(read()["env"], serde_json::json!({"X": "1"}), "kept");
        assert_eq!(read()["modelPicker"], big["modelPicker"], "replaced whole");
        // A file jq cannot read stays as it is.
        std::fs::write(&file, "not json").unwrap();
        assert!(!merge(r#"{"a": 1}"#).status.success());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "not json");
        assert!(!dir.join("deep/settings.json.aisbx-tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn custom_rows_split_into_named_tokens_and_strangers() {
        let secrets = secrets_fixture();
        assert_eq!(secrets.token_names(), vec!["private", "work"]);
        assert_eq!(secrets.tokens()[0].1.masked, "sk-ant********");
        assert_eq!(
            secrets
                .foreign_custom()
                .iter()
                .map(|c| c.placeholder.as_str())
                .collect::<Vec<_>>(),
            vec!["someone-elses-secret", "sbx-cs-aisbx-claude-token-old"],
            "a stranger's custom secret is listed, never touched"
        );
    }

    #[test]
    fn writing_a_service_secret_forces_the_overwrite_and_reads_the_value_from_stdin() {
        let set = secret_set_service_argv(None, "litellm");
        assert_eq!(set, vec!["sbx", "secret", "set", "litellm", "-f"]);
        assert!(!set.iter().any(|a| a == "--token" || a == "-t"));

        assert_eq!(
            secret_rm_service_argv(None, OAUTH_SERVICE),
            vec!["sbx", "secret", "rm", "anthropic", "-f"]
        );
        // A secret stored against one sandbox is a different row from the global one, so leaving
        // `--sandbox` off would write or delete the wrong thing.
        assert_eq!(
            secret_rm_service_argv(Some("claude-ki"), "openai"),
            vec![
                "sbx",
                "secret",
                "rm",
                "openai",
                "--sandbox",
                "claude-ki",
                "-f"
            ]
        );
        // A named token is dropped by placeholder at global scope — the same row the set wrote,
        // and that scope is the default rather than a flag.
        let rm = secret_rm_named_token_argv(&named_token_placeholder("private"));
        assert!(!rm.contains(&"-g".to_string()), "{rm:?}");
        assert!(!rm.contains(&"--sandbox".to_string()), "{rm:?}");
        assert!(
            rm.contains(&"sbx-cs-aisbx-tok-private".to_string()),
            "{rm:?}"
        );
        assert!(rm.contains(&"-f".to_string()), "{rm:?}");
    }

    #[test]
    fn token_names_have_to_round_trip_through_a_placeholder() {
        assert!(token_name_ok("private") && token_name_ok("work-2"));
        for bad in ["", "Work", "my token", "-lead", "tok_en", &"x".repeat(33)] {
            assert!(!token_name_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn create_argv_orders_flags_then_positionals() {
        let kit = PathBuf::from("/repo/kits/opencode-litellm");
        let mounts = ["/home/u/.m2/repository:ro".to_string()];
        let argv = create_argv(Create {
            mounts: &mounts,
            ..plain!(
                "opencode-proj",
                "opencode",
                &[kit.as_path()],
                Path::new("/proj")
            )
        });
        assert_eq!(
            argv,
            vec![
                "sbx",
                "create",
                "--kit",
                "/repo/kits/opencode-litellm",
                "--name",
                "opencode-proj",
                "--cpus",
                "4",
                "-m",
                "4g",
                "opencode",
                "/proj",
                "/home/u/.m2/repository:ro",
            ]
        );
    }

    #[test]
    fn create_argv_carries_the_038_deny_and_mcp_flags() {
        let kit = PathBuf::from("/repo/kits/claude-litellm");
        let deny = ["ads.example.com".to_string(), "**.tracker.io".to_string()];
        let mcp = ["notion".to_string(), "gh".to_string()];
        let argv = create_argv(Create {
            deny_network: &deny,
            static_mcp: &mcp,
            ..plain!(
                "claude-proj",
                "claude",
                &[kit.as_path()],
                Path::new("/proj")
            )
        });
        assert_eq!(
            argv,
            vec![
                "sbx",
                "create",
                "--kit",
                "/repo/kits/claude-litellm",
                "--name",
                "claude-proj",
                "--deny-network",
                "ads.example.com",
                "--deny-network",
                "**.tracker.io",
                "--static-mcp",
                "notion",
                "--static-mcp",
                "gh",
                "--cpus",
                "4",
                "-m",
                "4g",
                "claude",
                "/proj",
            ]
        );
        // Neither is written when nothing was asked for.
        let plain = create_argv(plain!(
            "claude-proj",
            "claude",
            &[kit.as_path()],
            Path::new("/proj")
        ));
        assert!(!plain.iter().any(|a| a.starts_with("--deny-network")));
        assert!(!plain.iter().any(|a| a.starts_with("--static-mcp")));
    }

    #[test]
    fn deny_specs_takes_only_this_sandboxs_own_network_denies() {
        let rule = |scope: &str, decision: &str, kind: &str, hosts: &[&str]| PolicyRule {
            scope: scope.into(),
            decision: decision.into(),
            resource_type: kind.into(),
            resources: hosts.iter().map(|h| h.to_string()).collect(),
            ..Default::default()
        };
        let rules = vec![
            rule("sandbox:mine", "deny", "network", &["ads.example.com"]),
            // …and again, to prove duplicates fold rather than doubling the flag.
            rule(
                "sandbox:mine",
                "deny",
                "network",
                &["ads.example.com", "b.io"],
            ),
            rule("sandbox:mine", "allow", "network", &["litellm.example.com"]),
            rule("sandbox:mine", "deny", "filesystem:write", &["/etc"]),
            rule("global", "deny", "network", &["everyones-problem.io"]),
            rule("sandbox:other", "deny", "network", &["not-mine.io"]),
        ];
        assert_eq!(deny_specs(&rules, "mine"), vec!["ads.example.com", "b.io"]);
        assert!(deny_specs(&rules, "nobody").is_empty());
    }

    #[test]
    fn mcp_json_parses_the_0_48_shape() {
        let raw = r#"[
  {"name": "aisbx-probe", "transport": "local stdio", "status": "ready", "type": "local",
   "gateway": {"name": "LOCAL", "local": true, "operator": "managed by you", "decision": "local", "signed_in_as": "someone"}},
  {"name": "notion", "transport": "remote http", "status": "needs auth · sbx mcp auth notion", "type": "remote",
   "gateway": {"name": "LOCAL", "local": true, "operator": "managed by you", "decision": "local", "signed_in_as": "someone"}}
]"#;
        let servers = parse_mcp_json(raw.as_bytes());
        assert_eq!(
            servers.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["aisbx-probe", "notion"]
        );
        assert_eq!(servers[0].transport, "local stdio");
        assert_eq!(servers[0].status, "ready");
        assert_eq!(servers[0].kind, "local");
        assert!(servers[0].ready());
        // Anything not affirmatively ready reads as "check it", never as a promise.
        assert_eq!(servers[1].transport, "remote http");
        assert!(!servers[1].ready());
    }

    #[test]
    fn a_status_that_only_mentions_ready_is_not_ready() {
        let with = |status: &str| McpServer {
            status: status.into(),
            ..McpServer::default()
        };
        assert!(with("ready").ready());
        assert!(with(" ready\n").ready());
        for status in ["not ready", "unready", "ready? no", "Ready", ""] {
            assert!(!with(status).ready(), "{status:?}");
        }
    }

    #[test]
    fn an_empty_or_unreadable_mcp_registry_is_an_empty_list() {
        assert!(parse_mcp_json(b"[]\n").is_empty());
        assert!(parse_mcp_json(b"").is_empty());
        // An object is a shape that does not fit, not a list of servers to find somewhere inside
        // it.
        let object = br#"{"gateway":{"name":"LOCAL","local":true},"servers":[{"name":"x","transport":"local stdio"}]}"#;
        assert!(parse_mcp_json(object).is_empty());
        // A human-readable table must not be misread as rows either.
        let table = "LOCAL · managed by you · ✓ on\n\n  aisbx-probe   local stdio   ✓ ready\n";
        assert!(parse_mcp_json(table.as_bytes()).is_empty());
    }

    #[test]
    fn only_the_include_block_counts_as_ssh_set_up() {
        let include =
            "Host x\n# >>> Docker Sandboxes (managed: default) >>>\nInclude ~/.docker/x\n";
        assert!(ssh_fenced(include));
        assert!(!ssh_fenced(
            "# >>> docker sandboxes (managed) >>>\nHost *.sbx\n"
        ));
        assert!(!ssh_fenced("Host github.com\n  User git\n"));
    }

    #[test]
    fn kit_references_carry_no_trailing_separator() {
        // `sbx kit add` rejects a trailing slash as "not canonical".
        let argv = kit_add_argv("s", Path::new("/repo/kits/web"));
        assert_eq!(argv.last().unwrap(), "/repo/kits/web");
        let argv = create_argv(plain!(
            "s",
            "claude",
            &[Path::new("/repo/kits/web")],
            Path::new("/p")
        ));
        assert!(argv.iter().all(|a| !a.ends_with(std::path::MAIN_SEPARATOR)));
    }

    #[test]
    fn every_flag_precedes_the_agent_positional() {
        let ports = ["8080:3000".to_string(), "3000".to_string()];
        let argv = create_argv(Create {
            clone: true,
            ports: &ports,
            skills: Some(SkillsMode::Off),
            ..plain!("n", "claude", &[], Path::new("/proj"))
        });
        let agent = argv.iter().position(|a| a == "claude").unwrap();
        for flag in ["--clone", "-p", "--skills"] {
            let at = argv
                .iter()
                .position(|a| a == flag)
                .unwrap_or_else(|| panic!("{flag} missing from {argv:?}"));
            assert!(at < agent, "{flag} must precede the agent: {argv:?}");
        }
        assert_eq!(argv.iter().filter(|a| *a == "-p").count(), 2);
        assert_eq!(argv[agent + 1], "/proj", "workspace follows the agent");
    }

    #[test]
    fn skills_mode_is_spelled_out_or_left_to_the_host() {
        for (mode, flag) in [
            (SkillsMode::Off, "off"),
            (SkillsMode::Readonly, "readonly"),
            (SkillsMode::Readwrite, "readwrite"),
        ] {
            let argv = create_argv(Create {
                skills: Some(mode),
                ..plain!("n", "claude", &[], Path::new("/proj"))
            });
            let at = argv.iter().position(|a| a == "--skills").expect("flag");
            assert_eq!(argv[at + 1], flag, "{argv:?}");
        }
        let argv = create_argv(plain!("n", "claude", &[], Path::new("/proj")));
        assert!(!argv.iter().any(|a| a == "--skills"), "{argv:?}");
    }

    #[test]
    fn mcp_secrets_are_recognised() {
        let modern = mcp_secret("mcp:slack:client_secret").expect("an mcp secret");
        assert_eq!(modern.server, "slack");
        assert_eq!(modern.key, "client_secret");

        // A custom request header's placeholder (`--header 'Authorization: Bearer ${api-key}'`).
        let header = mcp_secret("mcp:acme:api-key").expect("an mcp secret");
        assert_eq!((header.server, header.key), ("acme", "api-key"));

        // sbx's own binding record, which shares the server's prefix.
        assert_eq!(
            mcp_secret("mcp:acme:api-key:endpoint").map(|m| m.key),
            Some("api-key:endpoint")
        );

        // Only a colon separates the key: a dot is part of the name.
        assert_eq!(
            mcp_secret("mcp:slack.client_secret").map(|m| (m.server, m.key)),
            Some(("slack.client_secret", ""))
        );

        // Everything else is an ordinary service secret and must not be dressed up as one.
        for plain in ["litellm", "anthropic", "github", "mcpgateway"] {
            assert_eq!(mcp_secret(plain), None, "{plain}");
        }
    }

    #[test]
    fn skills_mode_cycles_both_ways() {
        let mut mode = SkillsMode::default();
        assert_eq!(mode, SkillsMode::Readonly, "sbx's own built-in default");
        for _ in 0..3 {
            mode = mode.next();
        }
        assert_eq!(mode, SkillsMode::Readonly);
        assert_eq!(SkillsMode::Readonly.prev(), SkillsMode::Off);
        assert!(SkillsMode::Readwrite.writable());
        assert!(!SkillsMode::Readonly.writable());
        assert_eq!(SkillsMode::parse("READWRITE"), Some(SkillsMode::Readwrite));
        assert_eq!(SkillsMode::parse("readonly\n"), Some(SkillsMode::Readonly));
        assert_eq!(SkillsMode::parse("rw"), None);
    }

    #[test]
    fn published_ports_fold_back_into_one_publish_argument_each() {
        let row = |host: &str, host_port: u16, sandbox_port: u16, proto: &str, eph: bool| Port {
            host_ip: host.into(),
            host_port,
            sandbox_port,
            protocol: proto.into(),
            ephemeral: eph,
        };
        let ports = vec![
            row("127.0.0.1", 18080, 8080, "tcp4", false),
            row("127.0.0.1", 18081, 8080, "tcp", false),
            row("::1", 18081, 8080, "tcp", false),
            row("127.0.0.1", 49152, 8084, "tcp4", true),
            row("192.168.1.5", 9000, 9000, "udp", false),
        ];
        assert_eq!(
            publish_specs(&ports),
            vec![
                // The `tcp4` default, reproduced as what it is rather than by relying on it.
                "18080:8080/tcp4".to_string(),
                // The loopback pair is one dual-stack mapping — and `/tcp` is what keeps it one:
                // bare, it would come back `tcp4` and lose its `::1` half.
                "18081:8080/tcp".to_string(),
                // An OS-assigned host port cannot be reproduced — ask for a fresh one.
                "8084/tcp4".to_string(),
                // An explicit host IP was a choice, so it survives.
                "192.168.1.5:9000:9000/udp".to_string(),
            ]
        );
        assert!(publish_specs(&[]).is_empty());
        // A row sbx reported with no protocol at all is passed on as it came.
        assert_eq!(
            publish_specs(&[row("127.0.0.1", 1, 2, "", false)]),
            vec!["1:2".to_string()]
        );
    }

    #[test]
    fn a_sandbox_kit_is_the_agent_positional_and_never_a_mixin() {
        let backend = PathBuf::from("/repo/kits/claude-token");
        let token = PathBuf::from("/state/tokens/token-private");
        let argv = create_argv(Create {
            agent_kit: Some(backend.as_path()),
            ..plain!(
                "claude-token-proj",
                "claude-token",
                &[backend.as_path(), token.as_path()],
                Path::new("/proj")
            )
        });
        assert_eq!(
            argv,
            vec![
                "sbx",
                "create",
                "--kit",
                "/state/tokens/token-private",
                "--name",
                "claude-token-proj",
                "--cpus",
                "4",
                "-m",
                "4g",
                "/repo/kits/claude-token",
                "/proj",
            ]
        );
        assert!(
            !argv.contains(&"claude-token".to_string()),
            "the kit's name is not passed: {argv:?}"
        );
        // …and without a workload the positional is sbx's own agent, the mixins `--kit`s.
        let mixin = PathBuf::from("/repo/kits/claude-litellm");
        let argv = create_argv(plain!(
            "claude-proj",
            "claude",
            &[mixin.as_path()],
            Path::new("/proj")
        ));
        assert_eq!(&argv[argv.len() - 2..], ["claude", "/proj"]);
        assert_eq!(argv[2..4], ["--kit", "/repo/kits/claude-litellm"]);
    }

    #[test]
    fn an_empty_workspace_creates_a_mountless_sandbox() {
        let mounts = ["/home/u/.m2/repository:ro".to_string()];
        let argv = create_argv(Create {
            mounts: &mounts,
            ..plain!("scratch", "claude", &[], Path::new(""))
        });
        assert_eq!(
            argv,
            vec!["sbx", "create", "--name", "scratch", "--cpus", "4", "-m", "4g", "claude"]
        );
    }

    #[test]
    fn skills_import_never_runs_without_an_answer_to_the_overwrite_prompt() {
        // No terminal on the job runner, so the only two shapes are "look" and "yes, replace".
        assert_eq!(
            skills_import_argv(false),
            vec!["sbx", "skills", "import", "--dry-run"]
        );
        assert_eq!(
            skills_import_argv(true),
            vec!["sbx", "skills", "import", "--force"]
        );
    }

    #[test]
    fn the_042_skills_commands_are_spelled_for_the_job_runner() {
        assert_eq!(
            skills_add_argv("anthropics/skills", &[]),
            vec!["sbx", "skills", "add", "anthropics/skills", "--force"]
        );
        let repo = "https://github.com/anthropics/skills";
        assert_eq!(
            skills_add_argv(repo, &["pdf".into(), "docx".into()]),
            vec!["sbx", "skills", "add", repo, "--force", "--skill", "pdf", "--skill", "docx"]
        );
        assert_eq!(skills_update_argv(&[]), vec!["sbx", "skills", "update"]);
        assert_eq!(
            skills_update_argv(&["pdf".into()]),
            vec!["sbx", "skills", "update", "pdf"]
        );
        assert_eq!(
            skills_rm_argv(&["pdf".into()]),
            vec!["sbx", "skills", "rm", "pdf", "--force"]
        );
        // A real payload, with one skill added from a repository.
        let store: SkillsStore = parse_json(br#"{"store":"/Users/u/Library/Application Support/com.docker.sandboxes/sandboxes/agent-skills","skills":["frontend-design"]}"#).unwrap();
        assert_eq!(store.skills, vec!["frontend-design"]);
        assert!(store.store.ends_with("agent-skills"));
    }

    #[test]
    fn a_skills_repository_is_shorthand_or_a_url() {
        for ok in [
            "anthropics/skills",
            "https://github.com/anthropics/skills",
            "git@github.com:org/skills.git",
            "ssh://git@host/x.git",
        ] {
            assert!(skills_repo_ok(ok), "{ok}");
        }
        for bad in [
            "",
            "skills",
            "a/b/c",
            "anthropics /skills",
            "/skills",
            "owner/",
        ] {
            assert!(!skills_repo_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_sandbox_is_reachable_at_its_own_name() {
        assert_eq!(ssh_host("claude-litellm-proj"), "claude-litellm-proj.sbx");
        assert_eq!(setup_ssh_argv(), vec!["sbx", "setup", "ssh"]);
    }
}
