//! The extra per-sandbox facts behind the details tabs, and the tabs' wording. The facts are
//! fetched only for the selected sandbox and only for the tab on screen, one request at a time.

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::model::{MountKind, Sandbox, Skills, MOUNTLESS_WORKDIR};
use crate::sbx::{self, LogHost, PolicyLog, PolicyRule, Port, RuntimeMount, SkillsMode};
use crate::stats::{self, Sample};

/// How long a fetched value stays fresh before the open tab re-requests it.
const RESOURCE_TTL: Duration = Duration::from_millis(2_500);
const POLICY_TTL: Duration = Duration::from_secs(15);
const LOG_TTL: Duration = Duration::from_secs(5);
/// Short, because an ephemeral host port is reassigned on every restart.
const PORTS_TTL: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Resources,
    Mounts,
    Ports,
    Network,
    Filesystem,
    Activity,
}

impl Tab {
    pub const ALL: [Tab; 7] = [
        Tab::Overview,
        Tab::Resources,
        Tab::Mounts,
        Tab::Ports,
        Tab::Network,
        Tab::Filesystem,
        Tab::Activity,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Resources => "Resources",
            Tab::Mounts => "Mounts",
            Tab::Ports => "Ports",
            Tab::Network => "Network",
            Tab::Filesystem => "Filesystem",
            Tab::Activity => "Activity",
        }
    }

    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Tab {
        Tab::ALL[i.min(Tab::ALL.len() - 1)]
    }

    /// What this tab needs fetched before it can render. `Overview` and `Mounts` are covered by
    /// the list snapshot, so they cost nothing extra.
    fn needs(self) -> Option<Kind> {
        match self {
            Tab::Overview | Tab::Mounts => None,
            Tab::Resources => Some(Kind::Resources),
            Tab::Ports => Some(Kind::Ports),
            // One `policy ls` call answers both rule tabs.
            Tab::Network | Tab::Filesystem => Some(Kind::Policy),
            Tab::Activity => Some(Kind::Log),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Resources,
    Ports,
    Policy,
    Log,
}

/// A fetched value plus when it landed, so the caller can tell "not asked yet" from "asked and
/// it failed" and from "fresh".
#[derive(Debug)]
pub struct Cell<T> {
    pub value: Option<T>,
    pub error: Option<String>,
    at: Option<Instant>,
}

impl<T> Default for Cell<T> {
    fn default() -> Self {
        Self {
            value: None,
            error: None,
            at: None,
        }
    }
}

impl<T> Cell<T> {
    pub fn is_loading(&self) -> bool {
        self.at.is_none()
    }

    fn stale(&self, ttl: Duration) -> bool {
        self.at.is_none_or(|at| at.elapsed() > ttl)
    }

    fn set(&mut self, result: Result<T, String>) {
        match result {
            Ok(v) => {
                self.value = Some(v);
                self.error = None;
            }
            // Keep the last good value: a transient daemon hiccup must not blank the pane.
            Err(e) => self.error = Some(e),
        }
        self.at = Some(Instant::now());
    }
}

/// Everything the tabs know about one sandbox. Rebuilt from scratch whenever the selection moves.
#[derive(Debug, Default)]
pub struct Insight {
    pub name: String,
    pub resources: Cell<Sample>,
    /// Previous resource reading, kept so CPU can be reported as a percentage over the interval.
    prev: Option<Sample>,
    pub cpu: Option<f32>,
    pub ports: Cell<Vec<Port>>,
    pub rules: Cell<Vec<PolicyRule>>,
    pub log: Cell<PolicyLog>,
    inflight: Option<Kind>,
}

/// A worker result on its way back to the UI thread.
#[derive(Debug)]
pub struct Fetched {
    pub name: String,
    pub payload: Payload,
}

#[derive(Debug)]
pub enum Payload {
    Resources(Result<Sample, String>),
    Ports(Result<Vec<Port>, String>),
    Policy(Result<Vec<PolicyRule>, String>),
    Log(Result<PolicyLog, String>),
}

impl Insight {
    /// Point at a different sandbox, dropping everything known about the previous one.
    pub fn retarget(&mut self, name: &str) {
        if self.name != name {
            *self = Self {
                name: name.to_string(),
                ..Default::default()
            };
        }
    }

    /// The fetch the visible tab needs now, if any: none while one is in flight, nothing is
    /// selected or the value is fresh. `running` gates the resource probe: `sbx exec` would *start*
    /// a stopped sandbox.
    pub fn due(&self, tab: Tab, running: bool) -> Option<Kind> {
        if self.inflight.is_some() || self.name.is_empty() {
            return None;
        }
        let kind = tab.needs()?;
        let due = match kind {
            Kind::Resources => running && self.resources.stale(RESOURCE_TTL),
            // Through the daemon, not `sbx exec`: a stopped sandbox answers without starting.
            Kind::Ports => self.ports.stale(PORTS_TTL),
            Kind::Policy => self.rules.stale(POLICY_TTL),
            Kind::Log => self.log.stale(LOG_TTL),
        };
        due.then_some(kind)
    }

    /// Kick off the fetch [`Insight::due`] names, on a thread of its own. Nothing is marked or
    /// started under `cargo test`, which would run `sbx` on the machine running the suite.
    pub fn poll(&mut self, tab: Tab, running: bool, tx: &Sender<Fetched>) {
        let Some(kind) = self.due(tab, running) else {
            return;
        };
        if cfg!(any(test, feature = "test-support")) {
            return;
        }

        self.inflight = Some(kind);
        let name = self.name.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            let payload = match kind {
                Kind::Resources => Payload::Resources(sbx::probe(&name).map_err(err)),
                Kind::Ports => Payload::Ports(sbx::ports(&name).map_err(err)),
                Kind::Policy => Payload::Policy(sbx::policy_rules(&name).map_err(err)),
                Kind::Log => Payload::Log(sbx::policy_log(&name, 60).map_err(err)),
            };
            let _ = tx.send(Fetched { name, payload });
        });
    }

    /// Take a worker result. Results for a sandbox that is no longer selected are dropped.
    pub fn accept(&mut self, fetched: Fetched) {
        if fetched.name != self.name {
            return;
        }
        self.inflight = None;
        match fetched.payload {
            Payload::Resources(result) => {
                if let Ok(sample) = &result {
                    let usable = self
                        .prev
                        .as_ref()
                        .filter(|p| stats::within_interval(p, sample));
                    self.cpu = match usable {
                        // Keep the last percentage when two readings land inside one jiffy.
                        Some(prev) => stats::cpu_between(prev, sample).or(self.cpu),
                        // First reading, or a gap so long that the delta would average minutes.
                        None => None,
                    };
                    self.prev = Some(*sample);
                }
                self.resources.set(result);
            }
            Payload::Ports(result) => self.ports.set(result),
            Payload::Policy(result) => self.rules.set(result),
            Payload::Log(result) => self.log.set(result),
        }
    }

    /// The sandbox stopped (or was never running) — forget the readings so the next start does
    /// not compute a CPU delta across the gap. Called every tick, hence the early out.
    pub fn clear_resources(&mut self) {
        if self.resources.at.is_none() && self.prev.is_none() && self.cpu.is_none() {
            return;
        }
        self.resources = Cell::default();
        self.prev = None;
        self.cpu = None;
    }

    pub fn network_rules(&self) -> Vec<&PolicyRule> {
        self.filter(PolicyRule::is_network)
    }

    pub fn filesystem_rules(&self) -> Vec<&PolicyRule> {
        self.filter(PolicyRule::is_filesystem)
    }

    fn filter(&self, pred: fn(&PolicyRule) -> bool) -> Vec<&PolicyRule> {
        let mut rules: Vec<&PolicyRule> = self
            .rules
            .value
            .iter()
            .flatten()
            .filter(|r| pred(r))
            .collect();
        // Deny before allow, then the narrowest scope first, the fallback last: the order a reader
        // reasons in.
        rules.sort_by_key(|r| {
            (
                r.is_fallback(),
                r.allows(),
                r.scope == "global",
                r.name.clone(),
            )
        });
        rules
    }
}

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

// --- what the tabs say ------------------------------------------------------------------------
//
// The tabs' wording and the rules behind it, for both frontends. Text comes without the TUI's
// indent or styling; a key in a note is a piece of its own, so each frontend draws it as a key.

/// How a piece of a note is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceKind {
    /// The note's own words: drawn quiet.
    Plain,
    /// Something to type, such as a port spec: drawn as ordinary text.
    Value,
    /// A key that acts.
    Key,
    Warn,
    Danger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotePiece {
    pub text: String,
    pub kind: PieceKind,
}

/// One line of a tab's notes; an empty one is a blank line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note(pub Vec<NotePiece>);

impl Note {
    fn of(pieces: &[(PieceKind, &str)]) -> Note {
        let piece = |&(kind, text): &(PieceKind, &str)| NotePiece {
            text: text.to_string(),
            kind,
        };
        Note(pieces.iter().map(piece).collect())
    }

    pub fn plain(text: &str) -> Note {
        Note::of(&[(PieceKind::Plain, text)])
    }

    fn blank() -> Note {
        Note(Vec::new())
    }

    pub fn is_blank(&self) -> bool {
        self.0.is_empty()
    }

    /// The whole line's text.
    pub fn text(&self) -> String {
        self.0.iter().map(|p| p.text.as_str()).collect()
    }
}

use PieceKind::{Danger, Key, Plain, Value};

/// What a tab says while its value has not landed: `loading` until the first answer, then that
/// it has none to show.
pub fn pending_text(kind: Kind, loading: bool) -> &'static str {
    match (kind, loading) {
        (Kind::Resources, true) => "measuring…",
        (Kind::Resources, false) => "no reading yet",
        (Kind::Ports | Kind::Policy | Kind::Log, true) => "loading…",
        (Kind::Ports, false) => "not read yet",
        // Not drawn: once answered, the rule tabs say their own empty text.
        (Kind::Policy, false) => "no rules yet",
        (Kind::Log, false) => "no log yet",
    }
}

/// The Resources tab of a stopped sandbox, which is never started just to be measured.
pub fn resources_stopped_notes() -> Vec<Note> {
    vec![
        Note::plain("The sandbox is stopped."),
        Note::blank(),
        Note::plain("CPU, memory and disk are read from inside the sandbox, which needs it to be"),
        Note::plain("running — starting one just to measure it would be worse than saying so."),
        Note::blank(),
        Note::of(&[
            (Key, "↵"),
            (Plain, "  start it, or  "),
            (Key, "x"),
            (Plain, "  open a shell inside it"),
        ]),
    ]
}

/// Beside the vCPU count while there is no CPU share: the first reading has no interval.
pub const LOAD_NEEDS_SECOND: &str = "load needs a second reading";
/// Under the Resources tab's readings.
pub const RESOURCES_SCOPE: &str =
    "The sandbox microVM's own CPUs, memory and root filesystem — not the host's.";

/// The last reading failed while an older one is still shown.
pub fn reading_failed(err: &str) -> String {
    format!("last reading failed: {err}")
}

/// The Mounts tab's notes, in their three places: under the mount rows, under the `sbx mount`
/// folders, and at the foot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountNotes {
    /// How the workspace is mounted, and whether there are extra mounts.
    pub about: Vec<Note>,
    /// Under the `sbx mount` folders, where there are any.
    pub added: Option<Note>,
    /// The workspace folder is gone from the host.
    pub missing: Option<Note>,
}

pub fn mount_notes(sb: &Sandbox) -> MountNotes {
    let mut about = Vec::new();
    if sb.mountless() {
        about.push(Note::plain(&format!(
            "No host folder is mounted (a mountless sandbox): the agent works in \
             {MOUNTLESS_WORKDIR},"
        )));
        about.push(Note::plain(
            "inside the sandbox. Those files survive a stop, and are deleted with the sandbox —",
        ));
        about.push(Note::plain(
            "`sbx cp` copies them out (sandbox paths must be absolute).",
        ));
    } else if sb.clone {
        about.push(Note::plain(
            "Clone mode: the agent works on a private in-container clone. Fetch its commits",
        ));
        about.push(Note::plain(&format!("with  git fetch sandbox-{}", sb.name)));
    } else {
        about.push(Note::plain(
            "The workspace is bind-mounted: edits inside the sandbox change these files.",
        ));
    }
    let no_extra = match &sb.host_mounts {
        Some(mounts) => !mounts.iter().any(|m| m.kind == MountKind::Folder),
        None => sb.ro_mounts.is_empty() && sb.rw_mounts.is_empty(),
    };
    if no_extra {
        about.push(Note::plain(
            "No extra mounts. The host-gradle and host-maven kits add read-only cache mounts.",
        ));
    }
    let added = (!sb.runtime_mounts.is_empty()).then(|| {
        Note::plain(
            "Added with sbx mount after creation; restored on every start, and mounted again by R.",
        )
    });
    let missing = sb.workspace_missing.then(|| {
        Note::of(&[(
            Danger,
            "⚠ the workspace folder no longer exists on this host",
        )])
    });
    MountNotes {
        about,
        added,
        missing,
    }
}

/// One folder on the Mounts tab: what it is, the host folder, where the sandbox sees it when that
/// is another path, what the sandbox may do in it, and whether it is gone from the host. A gone
/// workspace has its own note ([`MountNotes::missing`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountRow {
    pub kind: MountKind,
    /// The host folder, `~` for the home folder.
    pub host: String,
    pub target: Option<String>,
    pub access: String,
    pub missing: bool,
}

impl MountKind {
    /// The row's label on the Mounts tab.
    pub fn label(self) -> &'static str {
        match self {
            MountKind::Workspace => "workspace",
            MountKind::Skills => "skills",
            MountKind::Added => "added",
            MountKind::Folder => "mount",
        }
    }
}

/// The Mounts tab's folders: every bind mount `sbx inspect` reports, in its order; where it gave
/// no answer, the folders `sbx ls` lists.
pub fn mount_rows(sb: &Sandbox) -> Vec<MountRow> {
    let Some(mounts) = &sb.host_mounts else {
        return (sb.mounts().into_iter().enumerate())
            .map(|(i, (path, mode))| MountRow {
                kind: match i {
                    0 if !sb.mountless() => MountKind::Workspace,
                    _ => MountKind::Folder,
                },
                host: crate::model::tilde(path),
                target: None,
                access: mode.to_string(),
                missing: false,
            })
            .collect();
    };
    let plain = |read_only: bool| match read_only {
        true => "read-only",
        false => "read-write",
    };
    (mounts.iter())
        .map(|m| {
            let access = match (m.kind, sb.skills) {
                (MountKind::Workspace, _) if sb.clone => "read-only (clone mode)",
                (MountKind::Skills, Some(Skills::Known(SkillsMode::Readonly))) if m.read_only => {
                    skills_mount_text(Skills::Known(SkillsMode::Readonly))
                }
                (MountKind::Skills, Some(Skills::Known(SkillsMode::Readwrite))) if !m.read_only => {
                    skills_mount_text(Skills::Known(SkillsMode::Readwrite))
                }
                _ => plain(m.read_only),
            };
            MountRow {
                kind: m.kind,
                host: crate::model::tilde(&m.host),
                target: (m.target != m.host.display().to_string()).then(|| m.target.clone()),
                access: access.to_string(),
                missing: m.missing && m.kind != MountKind::Workspace,
            }
        })
        .collect()
}

/// Does [`mount_rows`] already list the skills store? Then the tab draws no separate row for it.
pub fn lists_skills(sb: &Sandbox) -> bool {
    (sb.host_mounts.iter().flatten()).any(|m| m.kind == MountKind::Skills)
}

/// The Mounts tab's separate `skills` row, after [`SKILLS_STORE`]; `None` for no row: a backend
/// without the store, or a store [`mount_rows`] lists. Where sbx reports the mounts and the store
/// is not among them, it is not mounted, whatever mode was recorded.
pub fn skills_row(sb: &Sandbox) -> Option<&'static str> {
    let skills = sb.skills.filter(|_| !lists_skills(sb))?;
    Some(match sb.host_mounts {
        Some(_) => skills_mount_text(Skills::Known(SkillsMode::Off)),
        None => skills_mount_text(skills),
    })
}

/// The Mounts tab's `skills` row: what is mounted, beside [`skills_mount_text`].
pub const SKILLS_STORE: &str = "the host's shared agent-skills store";

/// How the shared agent-skills store is mounted. It is not among the workspace mounts because it
/// is host state shared with every other sharing sandbox, and `K` is where it is managed.
pub fn skills_mount_text(skills: Skills) -> &'static str {
    match skills {
        Skills::Known(SkillsMode::Off) => "not mounted",
        // A read-only store is linked into a writable folder, so a kit can add skills beside it.
        Skills::Known(SkillsMode::Readonly) => "read-only, linked into the agent's skills folder",
        Skills::Known(SkillsMode::Readwrite) => {
            "read-write, mounted over the agent's skills folder"
        }
        Skills::Unrecorded => "mode not recorded",
    }
}

/// An `sbx mount` folder's access.
pub fn runtime_mount_access(mount: &RuntimeMount) -> &'static str {
    match mount.read_only {
        true => "read-only",
        false => "read-write",
    }
}

/// A published port's protocol, as sbx leaves it blank for TCP.
pub fn port_protocol(port: &Port) -> &str {
    match port.protocol.is_empty() {
        true => "tcp",
        false => &port.protocol,
    }
}

/// Beside a port whose host port is not stable across restarts.
pub const EPHEMERAL: &str = "ephemeral";

/// The Ports tab's notes, drawn after the port rows: what to press, what the service inside must
/// do, and what stops the ports answering.
pub fn port_notes(sb: &Sandbox, list: &[Port]) -> Vec<Note> {
    let mut notes = Vec::new();
    if list.is_empty() {
        notes.push(Note::plain(
            "No ports published — nothing on the host can reach into this sandbox.",
        ));
        notes.push(Note::blank());
        notes.push(Note::of(&[
            (Key, "p"),
            (Plain, "  publish one, e.g.  "),
            (Value, "8080:3000"),
            (Plain, "  (host 8080 → sandbox 3000)"),
        ]));
    } else {
        notes.push(Note::blank());
        notes.push(Note::of(&[
            (Key, "p"),
            (Plain, "  publish another   "),
            (Key, "u"),
            (Plain, "  unpublish"),
        ]));
    }
    notes.push(Note::blank());
    notes.push(Note::plain(
        "The service inside must listen on 0.0.0.0 (or [::]), not 127.0.0.1, to be reachable.",
    ));
    if !sb.running {
        notes.push(Note::plain(
            "The sandbox is stopped. Published ports are restored when it starts again;",
        ));
        notes.push(Note::plain("publishing a new one starts it first."));
    }
    if !list.is_empty() && !sb.keep_running {
        notes.push(Note::plain(
            "It stops itself shortly after its last session ends, and these stop answering with it",
        ));
        notes.push(Note::of(&[
            (Plain, "— "),
            (Key, "b"),
            (Plain, " keeps it running in the background."),
        ]));
    }
    notes
}

/// The `web` kit opens every host; the Network tab says so under the rules.
pub fn network_warning(sb: &Sandbox) -> Option<&'static str> {
    sb.addons
        .iter()
        .any(|a| a == "web")
        .then_some("⚠ the web kit allows ** — every host is reachable from this sandbox")
}

/// Beside the proxy on the Network tab.
pub const PROXY_GLOSS: &str = "enforces the rules below";
/// The rule lists' empty states.
pub const NO_NETWORK_RULES: &str = "No network rules apply to this sandbox.";
pub const NO_FILESYSTEM_RULES: &str = "No filesystem rules apply to this sandbox.";
/// The Filesystem tab's heading.
pub const FILESYSTEM_ABOUT: &str = "Which host paths any sandbox may be given as a workspace.";

/// The Network tab's last line: `c` asks the authorizer whether a host is reachable, which the
/// rules alone do not tell (a kit's allow-list adds to the local policy).
pub fn network_check_note() -> Note {
    Note::of(&[
        (Key, "c"),
        (
            Plain,
            "  check whether a host is reachable from this sandbox",
        ),
    ])
}

/// The protocols a network rule covers, where that is not just TCP: a network rule is TCP unless
/// it says otherwise, and a deny covers UDP too.
pub fn rule_protocols(rule: &PolicyRule) -> Option<&'static str> {
    if rule.resource_type != "network" || !rule.covers_udp() {
        return None;
    }
    Some(match rule.covers_tcp() {
        true => "tcp+udp",
        false => "udp",
    })
}

/// A rule's status when it is not live: under org governance a rule can be present but switched
/// off, and must not read as live.
pub fn rule_inactive(rule: &PolicyRule) -> Option<&str> {
    let status = rule.status.as_str();
    (!status.is_empty() && status != "active").then_some(status)
}

/// Which sandboxes a rule reaches beyond `sandbox`, the one it is listed for: `all sandboxes` for
/// a host-wide rule, any other scope as sbx writes it, and nothing for `sandbox`'s own.
pub fn rule_reach(rule: &PolicyRule, sandbox: &str) -> Option<String> {
    let reach = match rule.applies_to.is_empty() {
        true => rule.scope.as_str(),
        false => rule.applies_to.as_str(),
    };
    match reach {
        "" => None,
        "all" | "global" => Some("all sandboxes".into()),
        own if own.strip_prefix("sandbox:") == Some(sandbox) => None,
        other => Some(other.to_string()),
    }
}

/// How a rule's source ([`PolicyRule::source`]) stands out: a kit's in the accent colour, an
/// org's and an approved one as a warning, the rest quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceTone {
    Accent,
    Warn,
    Quiet,
}

pub fn source_tone(source: &str) -> SourceTone {
    match source {
        "kit" => SourceTone::Accent,
        "org" | "approved" => SourceTone::Warn,
        _ => SourceTone::Quiet,
    }
}

/// The Activity tab's empty log.
pub const NO_REQUESTS: &str = "The proxy has not logged any request for this sandbox yet.";

/// Why a request was let through or blocked: the daemon's reason, else the deciding rule's name.
/// A blocked default deny gets a short form, which [`approval_note`] explains.
pub fn log_reason(host: &LogHost, blocked: bool) -> &str {
    match blocked && host.default_deny() {
        true => "no rule (default deny)",
        false => host.why(),
    }
}

/// Was a request blocked for want of a rule, rather than by one? Under `balanced` / `deny-all`
/// that is a request the sandbox held for approval, which `y` answers.
pub fn asks_approval(log: &PolicyLog) -> bool {
    log.blocked.iter().any(|h| h.default_deny())
}

/// Under the blocked requests when [`asks_approval`].
pub fn approval_note() -> Note {
    Note::of(&[
        (
            Plain,
            "no rule allowed those — the sandbox asked for approval: ",
        ),
        (Key, "y"),
        (Plain, " answers it"),
    ])
}

/// `2026-07-24T10:39:45.03+02:00` → `10:39:45`. The date is almost always today, and the pane is
/// narrow.
pub fn short_time(iso: &str) -> String {
    iso.split('T')
        .nth(1)
        .and_then(|t| t.get(..8))
        .unwrap_or(iso)
        .to_string()
}

/// A fully-populated insight for rendering tests, built from real sbx output, so the layouts
/// meet realistic shapes and lengths.
#[cfg(any(test, feature = "test-support"))]
impl Insight {
    pub fn sample(name: &str) -> Self {
        use crate::sbx::{LogHost, PolicyLog};

        let host = |host: &str, proxy: &str, count: u64| LogHost {
            host: host.into(),
            proxy_type: proxy.into(),
            rule: String::new(),
            reason: String::new(),
            last_seen: "2026-07-24T10:39:45.030935+02:00".into(),
            count,
        };
        let mut insight = Self {
            name: name.to_string(),
            ..Default::default()
        };
        insight.resources.set(Ok(Sample::parse(
            "cpu  34 0 28 9631 9 0 18\ncpu0 1 0 12 956 0 0 12\ncpu1 0 0 0 968 0 0 0\n\
                 MemTotal:       16595744 kB\nMemAvailable:   16237568 kB\n505.37 5046.83\n\
                 overlay         20466256 320668  19080628   2% /\n",
        )
        .expect("the captured probe output parses")));
        insight.cpu = Some(0.12);
        insight.ports.set(Ok(vec![
            Port {
                host_ip: "127.0.0.1".into(),
                host_port: 8080,
                sandbox_port: 3000,
                protocol: "tcp".into(),
                ephemeral: false,
            },
            Port {
                host_ip: "127.0.0.1".into(),
                host_port: 54321,
                sandbox_port: 8000,
                protocol: "tcp".into(),
                ephemeral: true,
            },
        ]));
        insight.rules.set(Ok(vec![
            PolicyRule {
                name: "kit:claude-anthropic-proj".into(),
                scope: format!("sandbox:{name}"),
                applies_to: format!("sandbox:{name}"),
                resource_type: "network".into(),
                decision: "allow".into(),
                resources: vec![
                    "api.anthropic.com".into(),
                    "console.anthropic.com".into(),
                    "claude.ai".into(),
                    "platform.claude.com".into(),
                    "downloads.claude.ai:443".into(),
                    "github.com".into(),
                    "*.githubusercontent.com".into(),
                ],
                origin: "scoped".into(),
                status: "active".into(),
                provenance: crate::sbx::Provenance {
                    created_via: "provisioned".into(),
                },
                actions: vec!["net:connect:tcp".into()],
                ..Default::default()
            },
            // An answered approval and an HTTP rule.
            PolicyRule {
                name: "approval:d231080b".into(),
                scope: format!("sandbox:{name}"),
                applies_to: format!("sandbox:{name}"),
                resource_type: "network".into(),
                decision: "allow".into(),
                resources: vec!["registry.npmjs.org:443".into()],
                origin: "scoped".into(),
                status: "active".into(),
                provenance: crate::sbx::Provenance {
                    created_via: "approval".into(),
                },
                actions: vec!["net:connect:tcp".into()],
                ..Default::default()
            },
            PolicyRule {
                name: "c29a50e1".into(),
                scope: format!("sandbox:{name}"),
                applies_to: format!("sandbox:{name}"),
                resource_type: "http".into(),
                decision: "deny".into(),
                resources: vec!["api.github.com".into()],
                origin: "scoped".into(),
                status: "active".into(),
                provenance: crate::sbx::Provenance {
                    created_via: "added".into(),
                },
                actions: vec!["http:request:post".into()],
                methods: vec!["POST".into()],
                // With a query parameter.
                http_targets: vec![crate::sbx::HttpTarget {
                    host: "api.github.com".into(),
                    path: "/repos/**".into(),
                    query: vec![crate::sbx::QueryParam {
                        name: "state".into(),
                        value_pattern: "open".into(),
                    }],
                    query_match: "subset".into(),
                }],
                editable: Some(true),
                ..Default::default()
            },
            PolicyRule {
                name: "default-fs-read-allow-all".into(),
                scope: "global".into(),
                applies_to: "all".into(),
                resource_type: "filesystem:read".into(),
                decision: "allow".into(),
                resources: vec!["**".into()],
                origin: "local".into(),
                status: "active".into(),
                ..Default::default()
            },
            PolicyRule {
                name: crate::sbx::FALLBACK_RULE.into(),
                scope: "global".into(),
                applies_to: "all".into(),
                resource_type: "network".into(),
                decision: "deny".into(),
                resources: vec!["**".into()],
                origin: "local".into(),
                status: "active".into(),
                editable: Some(false),
                reason: "Implicit fallback when no applicable policy decides the request.".into(),
                actions: vec!["net:connect:tcp".into(), "net:connect:udp".into()],
                ..Default::default()
            },
        ]));
        insight.log.set(Ok(PolicyLog {
            blocked: vec![LogHost {
                rule: "no applicable policies for op(action=net:connect:tcp, \
                       resource=net:domain:registry.example.com:443)"
                    .into(),
                reason: "No matching allow rule (default deny)".into(),
                ..host("registry.example.com:443", "forward", 3)
            }],
            allowed: vec![
                host("api.anthropic.com:443", "forward", 128),
                host("github.com:443", "forward-bypass", 4),
            ],
        }));
        insight
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BindMount;

    fn rule(kind: &str, decision: &str, scope: &str, name: &str) -> PolicyRule {
        PolicyRule {
            name: name.into(),
            scope: scope.into(),
            resource_type: kind.into(),
            decision: decision.into(),
            ..Default::default()
        }
    }

    #[test]
    fn tabs_round_trip_through_their_index() {
        for tab in Tab::ALL {
            assert_eq!(Tab::from_index(tab.index()), tab);
        }
        assert_eq!(Tab::from_index(99), Tab::Activity);
    }

    #[test]
    fn retarget_drops_the_previous_sandboxs_data() {
        let mut insight = Insight::default();
        insight.retarget("a");
        insight.log.set(Ok(PolicyLog::default()));
        assert!(insight.log.value.is_some());

        insight.retarget("a"); // same sandbox → keep what we have
        assert!(insight.log.value.is_some());

        insight.retarget("b");
        assert!(insight.log.value.is_none());
        assert_eq!(insight.name, "b");
    }

    #[test]
    fn rules_split_by_type_and_sort_deny_first() {
        let mut insight = Insight::default();
        insight.rules.set(Ok(vec![
            rule("network", "deny", "global", crate::sbx::FALLBACK_RULE),
            rule("network", "allow", "sandbox:s", "kit:s"),
            rule("filesystem:read", "allow", "global", "fs-read"),
            rule("network", "deny", "global", "block-all"),
        ]));
        let net: Vec<&str> = insight
            .network_rules()
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(net, vec!["block-all", "kit:s", crate::sbx::FALLBACK_RULE]);
        assert_eq!(insight.filesystem_rules().len(), 1);
    }

    #[test]
    fn a_failed_fetch_keeps_the_last_good_value() {
        let mut insight = Insight::default();
        insight.retarget("a");
        insight
            .rules
            .set(Ok(vec![rule("network", "allow", "global", "r")]));
        insight.rules.set(Err("daemon busy".into()));
        assert_eq!(insight.rules.value.as_ref().unwrap().len(), 1);
        assert_eq!(insight.rules.error.as_deref(), Some("daemon busy"));
        assert!(!insight.rules.is_loading());
    }

    #[test]
    fn cpu_load_needs_two_readings_close_together() {
        let base = Sample::parse(
            "cpu  10 0 10 80 0\ncpu0 1 0 1 8 0\nMemTotal: 1048576 kB\nMemAvailable: 524288 kB\n\
             100.0 100.0\nx 1000 500 500 50% /\n",
        )
        .unwrap();
        let mut insight = Insight::default();
        insight.retarget("a");

        let feed = |insight: &mut Insight, sample: Sample| {
            insight.accept(Fetched {
                name: "a".into(),
                payload: Payload::Resources(Ok(sample)),
            })
        };

        feed(&mut insight, base);
        assert_eq!(insight.cpu, None, "the first reading has no interval");

        let mut soon = base;
        soon.uptime_s += 2.5;
        soon.cpu_total += 100;
        soon.cpu_idle += 75;
        feed(&mut insight, soon);
        assert_eq!(insight.cpu, Some(0.25));

        // Back after a while: the delta would span minutes, so start over rather than lie.
        let mut later = soon;
        later.uptime_s += 600.0;
        later.cpu_total += 100;
        later.cpu_idle += 100;
        feed(&mut insight, later);
        assert_eq!(insight.cpu, None);
    }

    #[test]
    fn due_never_probes_a_stopped_sandbox() {
        let mut insight = Insight::default();
        assert_eq!(insight.due(Tab::Resources, true), None, "nothing selected");
        insight.retarget("a");
        assert_eq!(insight.due(Tab::Resources, true), Some(Kind::Resources));
        assert_eq!(insight.due(Tab::Resources, false), None);
        assert_eq!(insight.due(Tab::Overview, true), None);
        assert_eq!(insight.due(Tab::Mounts, false), None);
        assert_eq!(insight.due(Tab::Ports, false), Some(Kind::Ports));
        assert_eq!(insight.due(Tab::Network, false), Some(Kind::Policy));
        assert_eq!(insight.due(Tab::Filesystem, false), Some(Kind::Policy));
        assert_eq!(insight.due(Tab::Activity, false), Some(Kind::Log));
        // Fresh values are not fetched again.
        let sampled = Insight::sample("a");
        for tab in Tab::ALL {
            assert_eq!(sampled.due(tab, true), None, "{tab:?}");
        }
        insight.inflight = Some(Kind::Log);
        assert_eq!(insight.due(Tab::Ports, true), None, "one at a time");
    }

    #[test]
    fn poll_spawns_nothing_under_the_host_guard() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut insight = Insight::default();
        insight.retarget("a");
        for tab in Tab::ALL {
            insight.poll(tab, true, &tx);
        }
        assert_eq!(insight.inflight, None);
        assert_eq!(insight.due(Tab::Resources, true), Some(Kind::Resources));
        drop(tx);
        assert!(rx.recv().is_err(), "no worker holds a sender");
    }

    fn notes(notes: &[Note]) -> Vec<String> {
        notes.iter().map(Note::text).collect()
    }

    #[test]
    fn the_stopped_resources_say_which_keys_start_it() {
        let got = resources_stopped_notes();
        assert_eq!(got[0].text(), "The sandbox is stopped.");
        assert!(got[1].is_blank() && got[4].is_blank());
        let keys: Vec<&str> = got[5]
            .0
            .iter()
            .filter(|p| p.kind == PieceKind::Key)
            .map(|p| p.text.as_str())
            .collect();
        assert_eq!(keys, ["↵", "x"]);
        assert_eq!(pending_text(Kind::Resources, true), "measuring…");
        assert_eq!(pending_text(Kind::Resources, false), "no reading yet");
        assert_eq!(pending_text(Kind::Log, false), "no log yet");
        assert_eq!(reading_failed("busy"), "last reading failed: busy");
    }

    fn reported(host: &str, target: &str, read_only: bool, kind: MountKind) -> BindMount {
        BindMount {
            host: host.into(),
            target: target.into(),
            read_only,
            kind,
            missing: false,
        }
    }

    #[test]
    fn the_mounts_tab_lists_what_sbx_reports() {
        let mut sb = crate::model::sample("s", Some("claude-anthropic"), &[]);
        sb.workspace = "/proj".into();
        sb.skills = Some(Skills::Known(SkillsMode::Readonly));
        let store = "/h/agent-skills";
        let mut gone = reported("/h/gone", "/h/gone", false, MountKind::Folder);
        gone.missing = true;
        let mut ws = reported("/proj", "/proj", false, MountKind::Workspace);
        ws.missing = true;
        sb.host_mounts = Some(vec![
            ws,
            reported("/h/data", "/workspace/data", true, MountKind::Folder),
            reported(
                store,
                "/home/agent/.local/share/aisbx/skills-store",
                true,
                MountKind::Skills,
            ),
            reported("/h/added", "/mnt/added", true, MountKind::Added),
            gone,
        ]);
        let rows = mount_rows(&sb);
        let labels: Vec<&str> = rows.iter().map(|r| r.kind.label()).collect();
        assert_eq!(labels, ["workspace", "mount", "skills", "added", "mount"]);
        assert_eq!(rows[0].target, None, "at its host path");
        assert_eq!(rows[1].target.as_deref(), Some("/workspace/data"));
        assert_eq!(rows[1].access, "read-only");
        assert_eq!(
            rows[2].access,
            skills_mount_text(Skills::Known(SkillsMode::Readonly)),
            "the recorded mode words it where sbx agrees"
        );
        assert!(!rows[0].missing, "the workspace has its own note");
        assert!(rows[4].missing);
        assert!(lists_skills(&sb));
        assert_eq!(
            notes(&mount_notes(&sb).about).len(),
            1,
            "extra mounts listed"
        );

        // sbx's report wins where the recorded mode disagrees with it.
        sb.skills = Some(Skills::Known(SkillsMode::Readwrite));
        assert_eq!(mount_rows(&sb)[2].access, "read-only");

        // Clone mode: the source at sbx's own path, read-only.
        sb.clone = true;
        sb.host_mounts = Some(vec![reported(
            "/proj",
            "/run/sandbox/source",
            true,
            MountKind::Workspace,
        )]);
        let rows = mount_rows(&sb);
        assert_eq!(rows[0].access, "read-only (clone mode)");
        assert_eq!(rows[0].target.as_deref(), Some("/run/sandbox/source"));
        assert!(!lists_skills(&sb));
        assert_eq!(
            skills_row(&sb),
            Some("not mounted"),
            "sbx reports no store mount, whatever the registry says"
        );
        assert_eq!(notes(&mount_notes(&sb).about).len(), 3, "no extra mounts");

        // No answer from sbx: the folders `sbx ls` lists.
        sb.host_mounts = None;
        sb.clone = false;
        sb.ro_mounts = vec!["/cache".into()];
        let rows = mount_rows(&sb);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[1].kind, rows[1].access.as_str()),
            (MountKind::Folder, "read-only")
        );
        assert_eq!(
            skills_row(&sb),
            Some(skills_mount_text(Skills::Known(SkillsMode::Readwrite))),
            "without a report, the recorded mode"
        );
        sb.skills = None;
        assert_eq!(skills_row(&sb), None, "no store on this backend");
    }

    #[test]
    fn mount_notes_follow_how_the_workspace_is_mounted() {
        let mut sb = crate::model::sample("s", Some("claude-anthropic"), &[]);
        let bind = mount_notes(&sb);
        assert_eq!(
            notes(&bind.about),
            [
                "The workspace is bind-mounted: edits inside the sandbox change these files.",
                "No extra mounts. The host-gradle and host-maven kits add read-only cache mounts."
            ]
        );
        assert_eq!((bind.added, bind.missing), (None, None));

        sb.clone = true;
        sb.ro_mounts = vec!["/cache".into()];
        sb.workspace_missing = true;
        let clone = mount_notes(&sb);
        assert_eq!(notes(&clone.about)[1], "with  git fetch sandbox-s");
        assert_eq!(clone.about.len(), 2, "an extra mount: no 'no extra mounts'");
        let missing = clone.missing.unwrap();
        assert_eq!(missing.0[0].kind, PieceKind::Danger);
        assert!(missing.text().contains("no longer exists"));

        sb.workspace = Default::default();
        sb.runtime_mounts = vec![RuntimeMount {
            host_path: "/data".into(),
            ..Default::default()
        }];
        let mountless = mount_notes(&sb);
        assert!(notes(&mountless.about)[0].contains("works in /home/agent/workspace,"));
        assert!(mountless.added.unwrap().text().contains("sbx mount"));
        assert_eq!(runtime_mount_access(&sb.runtime_mounts[0]), "read-write");
    }

    #[test]
    fn the_skills_store_text_names_each_mode() {
        let all = [
            (Skills::Known(SkillsMode::Off), "not mounted"),
            (
                Skills::Known(SkillsMode::Readonly),
                "read-only, linked into the agent's skills folder",
            ),
            (
                Skills::Known(SkillsMode::Readwrite),
                "read-write, mounted over the agent's skills folder",
            ),
            (Skills::Unrecorded, "mode not recorded"),
        ];
        for (skills, want) in all {
            assert_eq!(skills_mount_text(skills), want);
        }
    }

    #[test]
    fn port_notes_follow_the_list_and_the_lifecycle() {
        let mut sb = crate::model::sample("s", Some("claude-anthropic"), &[]);
        sb.running = true;
        let ports = Insight::sample("s").ports.value.unwrap();
        let empty = port_notes(&sb, &[]);
        assert!(empty[0].text().starts_with("No ports published"));
        assert_eq!(empty[2].0[2].kind, PieceKind::Value);
        assert!(!notes(&empty).iter().any(|n| n.contains("stops itself")));

        let listed = notes(&port_notes(&sb, &ports));
        assert_eq!(listed[1], "p  publish another   u  unpublish");
        assert!(listed[3].contains("0.0.0.0"));
        assert_eq!(listed[5], "— b keeps it running in the background.");

        sb.keep_running = true;
        assert_eq!(
            port_notes(&sb, &ports).len(),
            4,
            "kept running: nothing stops them"
        );
        sb.running = false;
        let stopped = notes(&port_notes(&sb, &ports));
        assert!(stopped[4].starts_with("The sandbox is stopped."));
        assert_eq!(port_protocol(&ports[0]), "tcp");
        let blank = Port::default();
        assert_eq!(port_protocol(&blank), "tcp");
        let udp = Port {
            protocol: "udp".into(),
            ..Port::default()
        };
        assert_eq!(port_protocol(&udp), "udp");
    }

    #[test]
    fn the_network_tab_flags_web_protocols_and_inactive_rules() {
        let web = crate::model::sample("s", Some("claude-anthropic"), &["web"]);
        assert!(network_warning(&web).unwrap().contains("**"));
        let plain = crate::model::sample("s", Some("claude-anthropic"), &[]);
        assert_eq!(network_warning(&plain), None);

        let with = |actions: &[&str], status: &str| PolicyRule {
            actions: actions.iter().map(|a| a.to_string()).collect(),
            status: status.into(),
            ..rule("network", "deny", "global", "r")
        };
        let both = with(&["net:connect:tcp", "net:connect:udp"], "active");
        assert_eq!(rule_protocols(&both), Some("tcp+udp"));
        assert_eq!(rule_protocols(&with(&["net:connect:udp"], "")), Some("udp"));
        assert_eq!(rule_protocols(&with(&["net:connect:tcp"], "")), None);
        assert_eq!(rule_protocols(&with(&[], "")), None, "TCP by default");
        let mut http = both.clone();
        http.resource_type = "http".into();
        assert_eq!(rule_protocols(&http), None);
        assert_eq!(rule_inactive(&both), None);
        assert_eq!(rule_inactive(&with(&[], "")), None);
        assert_eq!(rule_inactive(&with(&[], "inactive")), Some("inactive"));
        assert_eq!(source_tone("kit"), SourceTone::Accent);
        assert_eq!(source_tone("approved"), SourceTone::Warn);
        assert_eq!(source_tone("org"), SourceTone::Warn);
        assert_eq!(source_tone("added"), SourceTone::Quiet);
        assert_eq!(network_check_note().0[0].text, "c");
    }

    #[test]
    fn a_rule_names_its_reach_only_beyond_its_own_sandbox() {
        let reach = |scope: &str, applies_to: &str| {
            let rule = PolicyRule {
                applies_to: applies_to.into(),
                ..rule("network", "allow", scope, "r")
            };
            rule_reach(&rule, "s")
        };
        assert_eq!(reach("sandbox:s", "sandbox:s"), None);
        assert_eq!(
            reach("sandbox:s", ""),
            None,
            "the scope when sbx leaves it out"
        );
        assert_eq!(reach("global", "all"), Some("all sandboxes".into()));
        assert_eq!(reach("global", ""), Some("all sandboxes".into()));
        assert_eq!(reach("sandbox:t", "sandbox:t"), Some("sandbox:t".into()));
        assert_eq!(reach("sandbox:sx", ""), Some("sandbox:sx".into()));
        assert_eq!(reach("", ""), None);
    }

    #[test]
    fn the_activity_tab_shortens_a_default_deny() {
        let log = Insight::sample("s").log.value.unwrap();
        assert!(asks_approval(&log));
        assert_eq!(log_reason(&log.blocked[0], true), "no rule (default deny)");
        // The same entry, allowed, keeps the daemon's own words.
        let reason = "No matching allow rule (default deny)";
        assert_eq!(log_reason(&log.blocked[0], false), reason);
        let named = LogHost {
            rule: "deny-evil".into(),
            ..log.allowed[0].clone()
        };
        assert_eq!(log_reason(&named, true), "deny-evil");
        let none = PolicyLog {
            blocked: vec![named],
            allowed: Vec::new(),
        };
        assert!(!asks_approval(&none));
        assert!(approval_note().text().ends_with("y answers it"));
    }

    #[test]
    fn timestamps_shrink_to_the_clock_time() {
        assert_eq!(short_time("2026-07-24T10:39:45.030935+02:00"), "10:39:45");
        assert_eq!(short_time("nonsense"), "nonsense");
    }

    #[test]
    fn results_for_a_stale_selection_are_ignored() {
        let mut insight = Insight::default();
        insight.retarget("a");
        insight.accept(Fetched {
            name: "b".into(),
            payload: Payload::Log(Ok(PolicyLog::default())),
        });
        assert!(insight.log.value.is_none());
    }
}
