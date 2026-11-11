//! `aisbx step`: one unit of work in a fresh sandbox, in one process — docs/step-protocol.md is
//! the contract (protocol [`PROTOCOL`]). The lifecycle here is fixed and security-relevant:
//! refuse, create, prove, log, work, log, remove, report, in that order, the removal in every
//! path. A caller chooses parameters through the spec file and reads NDJSON events from stdout.
//!
//! `aisbx doctor` lives here too: it reports what a step needs before the first one runs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::headless::{self, Args, Fail, Outcome};
use crate::kits::{self, Catalog, HeadlessRun, OutputFormat};
use crate::{sbx, skill_select, state};

/// The step protocol this binary speaks. It bumps on any incompatible change to the spec, the
/// events or the report; a caller pins it.
pub const PROTOCOL: u32 = 1;

/// Refused before anything was created: the name exists, skills are selected, no fallback rule.
pub const EXIT_REFUSED: u8 = 3;
/// The network proof failed; the sandbox was removed.
pub const EXIT_PROOF: u8 = 4;

/// How long a cached proof holds.
const PROOF_TTL: Duration = Duration::from_secs(24 * 3600);
/// How long a pre-warmed sandbox may wait for its work.
const PREWARM_TTL: Duration = Duration::from_secs(3600);
/// What a check may print before the rest is dropped (the report says how much there was).
const CHECK_STDOUT_LIMIT: usize = 4 << 20;
const CHECK_STDERR_LIMIT: usize = 1 << 20;
/// `sbx policy log --limit`, per list.
const LOG_LIMIT: u32 = 1000;

// --- the spec --------------------------------------------------------------------------------------

/// `step.json`. Unknown fields are an error: a misspelt field must not silently widen a sandbox.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub protocol: u32,
    pub name: String,
    pub backend: String,
    #[serde(default)]
    pub kits: Vec<String>,
    pub workspace: PathBuf,
    #[serde(default)]
    pub mounts: Vec<Mount>,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    #[serde(default)]
    pub probe_hosts: Vec<String>,
    #[serde(default = "yes")]
    pub network: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    pub work: Work,
    pub timeout_seconds: u64,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    pub path: PathBuf,
    #[serde(default)]
    pub readonly: bool,
}

/// Exactly one of `agent` or `checks`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Work {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentWork>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<Check>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentWork {
    pub prompt_file: PathBuf,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub max_turns: u32,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub workdir: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl Spec {
    /// Parse and validate the spec text. Every error here is exit 2: nothing ran.
    pub fn parse(text: &str) -> Result<Self, String> {
        let spec: Spec = serde_json::from_str(text).map_err(|e| format!("not a step spec: {e}"))?;
        spec.validate()?;
        Ok(spec)
    }

    fn validate(&self) -> Result<(), String> {
        if self.protocol != PROTOCOL {
            return Err(format!(
                "protocol {} is not {PROTOCOL}, the one this aisbx speaks",
                self.protocol
            ));
        }
        if !headless::valid_sandbox_name(&self.name) {
            return Err(format!(
                "{:?} is not an sbx name: letters, digits, '.' and '-', at most 63",
                self.name
            ));
        }
        if self.backend.trim().is_empty() {
            return Err("backend is empty".into());
        }
        if self.workspace.as_os_str().is_empty() {
            return Err("workspace is empty".into());
        }
        match (&self.work.agent, &self.work.checks) {
            (Some(_), None) => {}
            (None, Some(checks)) if !checks.is_empty() => {}
            (None, Some(_)) => return Err("work.checks is empty".into()),
            (Some(_), Some(_)) => return Err("work has both agent and checks; pick one".into()),
            (None, None) => return Err("work needs agent or checks".into()),
        }
        if let Some(agent) = &self.work.agent {
            if agent.model.trim().is_empty() {
                return Err("work.agent.model is empty".into());
            }
            if agent.max_turns == 0 {
                return Err("work.agent.max_turns must be at least 1".into());
            }
        }
        for (i, check) in self.work.checks.iter().flatten().enumerate() {
            if check.argv.is_empty() {
                return Err(format!("work.checks[{i}].argv is empty"));
            }
            if check.workdir.starts_with('/') || check.workdir.split('/').any(|p| p == "..") {
                return Err(format!(
                    "work.checks[{i}].workdir must be relative to the workspace, without .."
                ));
            }
            for key in check.env.keys() {
                if key.is_empty() || key.contains('=') {
                    return Err(format!("work.checks[{i}].env has a bad key {key:?}"));
                }
            }
        }
        if self.timeout_seconds == 0 {
            return Err("timeout_seconds must be at least 1".into());
        }
        for host in self.allow_hosts.iter().chain(&self.probe_hosts) {
            if host.trim().is_empty() || host.contains(char::is_whitespace) {
                return Err(format!("{host:?} is not a host"));
            }
        }
        Ok(())
    }

    fn is_agent(&self) -> bool {
        self.work.agent.is_some()
    }
}

// --- denies and proof ------------------------------------------------------------------------------

/// `api.example.com:443` → `api.example.com`.
pub fn host_of(resource: &str) -> String {
    let host = resource
        .split("://")
        .last()
        .unwrap_or(resource)
        .split('/')
        .next()
        .unwrap_or(resource);
    match host.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            h.to_ascii_lowercase()
        }
        _ => host.to_ascii_lowercase(),
    }
}

/// What `--deny-network` gets at create: every host the kits open that the caller did not allow,
/// port 80 of every allowed host, and `**` when the step has no network at all. A kit that opens
/// a wildcard while hosts are allowed is an error: denying it would close the allowed hosts too.
pub fn denies(
    kit_allows: &[String],
    allow_hosts: &[String],
    network: bool,
) -> Result<Vec<String>, String> {
    if !network {
        return Ok(vec!["**".into()]);
    }
    let allowed: BTreeSet<String> = allow_hosts.iter().map(|h| host_of(h)).collect();
    let mut out: Vec<String> = Vec::new();
    for resource in kit_allows {
        if resource.contains('*') && !allowed.is_empty() {
            return Err(format!(
                "a kit opens {resource:?}; a deny for it would also close the allowed hosts"
            ));
        }
        if allowed.contains(&host_of(resource)) || out.contains(resource) {
            continue;
        }
        out.push(resource.clone());
    }
    for host in &allowed {
        let deny = format!("{host}:80");
        if !out.contains(&deny) {
            out.push(deny);
        }
    }
    Ok(out)
}

/// One probe of the proof: a target, its protocol, and whether it must be reachable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Probe {
    pub protocol: &'static str,
    pub target: String,
    pub must_allow: bool,
}

/// The probes for a spec: every allowed host on 443 reachable, its port 80 and UDP not, every
/// probe host not. A step without network probes its allowed hosts as unreachable as well.
pub fn probes(allow_hosts: &[String], probe_hosts: &[String], network: bool) -> Vec<Probe> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for host in allow_hosts.iter().map(|h| host_of(h)) {
        if !seen.insert(host.clone()) {
            continue;
        }
        out.push(Probe {
            protocol: "tcp",
            target: format!("{host}:443"),
            must_allow: network,
        });
        out.push(Probe {
            protocol: "tcp",
            target: format!("{host}:80"),
            must_allow: false,
        });
        out.push(Probe {
            protocol: "udp",
            target: format!("{host}:443"),
            must_allow: false,
        });
    }
    for host in probe_hosts.iter().map(|h| host_of(h)) {
        if !seen.insert(host.clone()) {
            continue;
        }
        out.push(Probe {
            protocol: "tcp",
            target: format!("{host}:443"),
            must_allow: false,
        });
    }
    out
}

/// The facts of a rule that decide traffic, for the proof key.
fn rule_facts(rule: &sbx::PolicyRule) -> serde_json::Value {
    let mut resources = rule.resources.clone();
    resources.sort();
    let mut actions = rule.actions.clone();
    actions.sort();
    let mut targets: Vec<String> = rule
        .http_targets
        .iter()
        .map(|t| format!("{} {}", t.host, t.path))
        .collect();
    targets.sort();
    serde_json::json!({
        "scope": rule.scope,
        "decision": rule.decision,
        "type": rule.resource_type,
        "status": rule.status,
        "resources": resources,
        "actions": actions,
        "methods": rule.methods,
        "targets": targets,
    })
}

/// The proof key: the effective rules, the allowed and probed hosts and the network flag, hashed.
/// A sandbox name is not part of it, so the proof of one step holds for the next with the same
/// policy.
pub fn proof_key(rules: &[sbx::PolicyRule], probes: &[Probe], network: bool) -> String {
    let mut facts: Vec<String> = rules
        .iter()
        .filter(|r| r.is_network())
        .map(|r| rule_facts(r).to_string())
        .collect();
    facts.sort();
    let doc = serde_json::json!({
        "protocol": PROTOCOL,
        "network": network,
        "rules": facts,
        "probes": probes,
    });
    sha256_hex(doc.to_string().as_bytes())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Where cached proofs live: `<state>/proofs/<key>.json`.
pub fn proofs_dir(state: &Path) -> PathBuf {
    state.join("proofs")
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CachedProof {
    at: u64,
    key: String,
    results: Vec<ProbeResult>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProbeResult {
    pub protocol: String,
    pub target: String,
    pub must_allow: bool,
    pub allowed: bool,
    pub detail: String,
}

impl ProbeResult {
    pub fn ok(&self) -> bool {
        self.allowed == self.must_allow
    }
}

fn read_cached_proof(dir: &Path, key: &str, now: u64) -> Option<Vec<ProbeResult>> {
    let text = fs::read_to_string(dir.join(format!("{key}.json"))).ok()?;
    let cached: CachedProof = serde_json::from_str(&text).ok()?;
    let fresh = cached.key == key && now.saturating_sub(cached.at) < PROOF_TTL.as_secs();
    (fresh && cached.results.iter().all(ProbeResult::ok)).then_some(cached.results)
}

fn write_cached_proof(dir: &Path, key: &str, results: &[ProbeResult], now: u64) {
    let _ = fs::create_dir_all(dir);
    let doc = CachedProof {
        at: now,
        key: key.into(),
        results: results.to_vec(),
    };
    let _ = fs::write(
        dir.join(format!("{key}.json")),
        serde_json::to_string_pretty(&doc).unwrap_or_default(),
    );
}

/// Run every probe against the daemon, in parallel: each is one read-only `sbx policy check`.
fn run_probes(name: &str, probes: &[Probe]) -> Vec<ProbeResult> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = probes
            .iter()
            .map(|probe| {
                scope.spawn(move || {
                    let (allowed, detail) =
                        match sbx::check_target(name, &probe.target, Some(probe.protocol)) {
                            Ok(r) => (r.allowed, r.detail),
                            // An unanswered probe counts as the wrong answer, never as proof.
                            Err(e) => (!probe.must_allow, format!("no answer: {e:#}")),
                        };
                    ProbeResult {
                        protocol: probe.protocol.into(),
                        target: probe.target.clone(),
                        must_allow: probe.must_allow,
                        allowed,
                        detail,
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a probe thread does not panic"))
            .collect()
    })
}

// --- events ----------------------------------------------------------------------------------------

/// RFC 3339, UTC, milliseconds: `2026-11-11T09:12:03.456Z`.
pub fn now_rfc3339() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    rfc3339_ms(d.as_secs(), d.subsec_millis())
}

pub fn rfc3339_ms(secs: u64, ms: u32) -> String {
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe as i64 + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// The `sbx exec -w` folder of a check. Like `docker exec -w` it names a folder inside the sandbox, so it is
/// the workspace where sbx mounted it ([`sbx::sandbox_path`]: `/c/Users/me/w` for `C:\Users\me\w` on
/// Windows) and the check's relative `workdir`, joined with `/`.
fn check_workdir(workspace: &Path, workdir: &str) -> String {
    let mut path = sbx::sandbox_path(workspace);
    for part in workdir.split('/').filter(|p| !p.is_empty() && *p != ".") {
        if !path.ends_with('/') {
            path.push('/');
        }
        path.push_str(part);
    }
    path
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// One event line on stdout, numbered. Every event has `t`, `seq`, `kind` and `phase`.
pub struct Emitter {
    seq: u64,
    out: Box<dyn Write + Send>,
}

impl Emitter {
    pub fn stdout() -> Self {
        Self {
            seq: 0,
            out: Box::new(std::io::stdout()),
        }
    }

    pub fn into_buffer(buffer: Arc<Mutex<Vec<u8>>>) -> Self {
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().expect("sink").extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        Self {
            seq: 0,
            out: Box::new(Sink(buffer)),
        }
    }

    /// Write one event; `fields` come after the four fixed ones.
    pub fn emit(&mut self, kind: &str, phase: &str, fields: serde_json::Value) {
        self.seq += 1;
        let mut doc = serde_json::Map::new();
        doc.insert("t".into(), now_rfc3339().into());
        doc.insert("seq".into(), self.seq.into());
        doc.insert("kind".into(), kind.into());
        doc.insert("phase".into(), phase.into());
        if let serde_json::Value::Object(extra) = fields {
            for (k, v) in extra {
                doc.insert(k, v);
            }
        }
        let line = serde_json::Value::Object(doc).to_string();
        let _ = writeln!(self.out, "{line}");
        let _ = self.out.flush();
    }

    pub fn phase(&mut self, phase: &str, status: &str, ms: u64, detail: &str) {
        self.emit(
            "phase",
            phase,
            serde_json::json!({"status": status, "ms": ms, "detail": detail}),
        );
    }

    pub fn warn(&mut self, phase: &str, message: &str) {
        eprintln!("aisbx: warning: {message}");
        self.emit("warn", phase, serde_json::json!({"message": message}));
    }

    pub fn sbx(&mut self, phase: &str, argv: &[String], exit: Option<i32>, ms: u64) {
        self.emit(
            "sbx",
            phase,
            serde_json::json!({"argv": argv, "exit": exit, "ms": ms}),
        );
    }
}

// --- the lifecycle ---------------------------------------------------------------------------------

/// Why a phase ended the step. The sandbox is removed before the code is returned.
#[derive(Debug, Clone)]
struct Stop {
    phase: &'static str,
    code: u8,
    message: String,
}

impl Stop {
    fn new(phase: &'static str, code: u8, message: impl Into<String>) -> Self {
        Self {
            phase,
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Default, Serialize)]
struct Timings {
    create: u64,
    prove: u64,
    work: u64,
    remove: u64,
    total: u64,
}

#[derive(Debug, Default, Serialize)]
struct AgentOutcome {
    started: bool,
    exit: Option<i32>,
    timed_out: bool,
    interrupted: bool,
    lines: u64,
    init_seen: bool,
    result_seen: bool,
    assistant_seen: bool,
}

#[derive(Debug, Default, Serialize)]
struct CheckOutcome {
    index: usize,
    argv: Vec<String>,
    workdir: String,
    exit: Option<i32>,
    timed_out: bool,
    stdout_bytes: usize,
    stderr_bytes: usize,
}

#[derive(Debug, Default)]
struct NetworkOutcome {
    blocked: Vec<String>,
    foreign: Vec<String>,
}

/// Everything the report says, filled in as the phases run.
#[derive(Debug, Default)]
struct Run {
    name: String,
    created_at: String,
    ms: Timings,
    proof_cached: bool,
    agent: Option<AgentOutcome>,
    checks: Vec<CheckOutcome>,
    network: NetworkOutcome,
    image: String,
    created: bool,
    removed: bool,
    interrupted: bool,
}

fn ms(since: Instant) -> u64 {
    since.elapsed().as_millis() as u64
}

fn write_json(path: &Path, doc: &serde_json::Value) {
    let text = serde_json::to_string_pretty(doc).unwrap_or_default();
    if let Err(e) = fs::write(path, text + "\n") {
        eprintln!("aisbx: could not write {}: {e}", path.display());
    }
}

/// `sbx policy log` as the two files hold it.
fn log_json(log: &sbx::PolicyLog) -> serde_json::Value {
    let host = |h: &sbx::LogHost| serde_json::json!({"host": h.host, "count": h.count, "last_seen": h.last_seen, "why": h.why()});
    serde_json::json!({
        "allowed": log.allowed.iter().map(host).collect::<Vec<_>>(),
        "blocked": log.blocked.iter().map(host).collect::<Vec<_>>(),
    })
}

fn counts(hosts: &[sbx::LogHost]) -> BTreeMap<String, u64> {
    hosts
        .iter()
        .map(|h| (h.host.to_ascii_lowercase(), h.count))
        .collect()
}

/// What the proxy saw during the work that the caller must judge: hosts blocked (new blocks
/// only; the kits' install may have been blocked before), and allowed hosts outside the allow
/// list with new traffic.
pub fn judge_network(
    before: &sbx::PolicyLog,
    after: &sbx::PolicyLog,
    allow_hosts: &[String],
) -> (Vec<String>, Vec<String>) {
    let allowed: BTreeSet<String> = allow_hosts.iter().map(|h| host_of(h)).collect();
    let (b_blocked, a_blocked) = (counts(&before.blocked), counts(&after.blocked));
    let (b_allowed, a_allowed) = (counts(&before.allowed), counts(&after.allowed));
    let blocked: Vec<String> = a_blocked
        .iter()
        .filter(|(host, count)| **count > b_blocked.get(*host).copied().unwrap_or(0))
        .map(|(host, _)| host.clone())
        .collect();
    let foreign: Vec<String> = a_allowed
        .iter()
        .filter(|(host, count)| {
            !allowed.contains(&host_of(host))
                && **count > b_allowed.get(*host).copied().unwrap_or(0)
        })
        .map(|(host, _)| host.clone())
        .collect();
    (blocked, foreign)
}

/// `aisbx step` — the subcommand. Parses the flags, then runs [`Step::run`].
pub(crate) fn main(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &["prewarm", "protocol"], &["spec", "dir"])?;
    if args.has("protocol") {
        println!("{PROTOCOL}");
        return Ok(0);
    }
    if let Some(extra) = args.positional.first().or(args.trailing.first()) {
        return Err(Fail::usage(format!("step takes no argument: {extra}")));
    }
    let spec_path = args
        .one("spec")?
        .ok_or_else(|| Fail::usage("--spec FILE is required"))?;
    let dir = args
        .one("dir")?
        .ok_or_else(|| Fail::usage("--dir DIR is required"))?;
    let text = fs::read_to_string(&spec_path)
        .map_err(|e| Fail::usage(format!("reading the spec {spec_path}: {e}")))?;
    let spec = Spec::parse(&text).map_err(|e| Fail::usage(format!("{spec_path}: {e}")))?;
    let dir = PathBuf::from(dir);
    fs::create_dir_all(&dir)
        .map_err(|e| Fail::usage(format!("creating {}: {e}", dir.display())))?;
    let prompt = match &spec.work.agent {
        Some(agent) => Some(
            fs::read_to_string(&agent.prompt_file)
                .map_err(|e| {
                    Fail::usage(format!(
                        "reading the prompt {}: {e}",
                        agent.prompt_file.display()
                    ))
                })
                .and_then(|p| {
                    if p.trim().is_empty() {
                        Err(Fail::usage("the prompt is empty"))
                    } else {
                        Ok(p)
                    }
                })?,
        ),
        None => None,
    };
    let workspace = spec
        .workspace
        .canonicalize()
        .map(sbx::plain_path)
        .map_err(|e| Fail::usage(format!("workspace {}: {e}", spec.workspace.display())))?;
    let catalog = Catalog::load().map_err(|e| Fail::sandbox(format!("{e:#}")))?;
    let backend = catalog
        .backend(&spec.backend)
        .ok_or_else(|| Fail::usage(format!("no backend kit {:?}", spec.backend)))?;
    let mut kit_dirs = vec![backend.kit.dir.clone()];
    for kit in &spec.kits {
        let addon = catalog
            .addon(kit)
            .ok_or_else(|| Fail::usage(format!("no addon kit {kit:?}")))?;
        kit_dirs.push(addon.kit.dir.clone());
    }
    let kit_allows: Vec<String> = kit_dirs
        .iter()
        .flat_map(|d| kits::runtime_allows(d))
        .collect();
    let deny = denies(&kit_allows, &spec.allow_hosts, spec.network).map_err(Fail::usage)?;

    let spec_doc = serde_json::to_value(&spec).unwrap_or_default();
    write_json(&dir.join("spec.json"), &spec_doc);
    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let flag = Arc::clone(&interrupted);
        let _ = ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst));
    }
    let mut step = Step {
        spec,
        spec_digest: sha256_hex(spec_doc.to_string().as_bytes()),
        prompt,
        workspace,
        agent: backend.agent.clone(),
        deny,
        dir,
        prewarm: args.has("prewarm"),
        emit: Emitter::stdout(),
        interrupted,
        run: Run::default(),
        started: Instant::now(),
    };
    Ok(step.run())
}

struct Step {
    spec: Spec,
    spec_digest: String,
    prompt: Option<String>,
    workspace: PathBuf,
    /// The sbx agent name of the backend (`claude-anthropic`).
    agent: String,
    deny: Vec<String>,
    dir: PathBuf,
    prewarm: bool,
    emit: Emitter,
    interrupted: Arc<AtomicBool>,
    run: Run,
    started: Instant,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Prewarmed {
    name: String,
    spec_sha256: String,
    at: u64,
    proof_cached: bool,
}

impl Step {
    fn prewarmed_path(&self) -> PathBuf {
        self.dir.join("prewarmed.json")
    }

    fn interrupted(&self) -> bool {
        self.interrupted.load(Ordering::SeqCst)
    }

    /// The whole lifecycle; the exit code of the process.
    fn run(&mut self) -> u8 {
        self.run.name = self.spec.name.clone();
        self.run.created_at = now_rfc3339();
        let outcome = self.phases();
        let removal = match self.run.created && !self.run.removed {
            true => self.remove(),
            false => Ok(()),
        };
        let code = match (&removal, &outcome) {
            (Err(stop), _) => {
                self.emit.phase("remove", "fail", 0, &stop.message);
                stop.code
            }
            (Ok(()), Err(stop)) => stop.code,
            (Ok(()), Ok(code)) => *code,
        };
        let failed = match (&removal, &outcome) {
            (Err(stop), _) => Some(stop.clone()),
            (Ok(()), Err(stop)) => Some(stop.clone()),
            _ => None,
        };
        self.run.ms.total = ms(self.started);
        self.report(code, failed.as_ref());
        code
    }

    /// Phases 1 to 7; `Ok` carries the exit code when every phase held.
    fn phases(&mut self) -> Result<u8, Stop> {
        let resumed = self.refuse()?;
        if !resumed {
            self.create()?;
            self.prove()?;
            if self.prewarm {
                let doc = Prewarmed {
                    name: self.spec.name.clone(),
                    spec_sha256: self.spec_digest.clone(),
                    at: epoch_secs(),
                    proof_cached: self.run.proof_cached,
                };
                write_json(
                    &self.prewarmed_path(),
                    &serde_json::to_value(&doc).unwrap_or_default(),
                );
                self.emit.phase(
                    "report",
                    "ok",
                    0,
                    "pre-warmed: the sandbox waits for its work",
                );
                // Not removed: the next step continues with it.
                self.run.removed = true;
                return Ok(0);
            }
        }
        let _ = fs::remove_file(self.prewarmed_path());
        let before = self.log("network-before.json")?;
        let work = self.work()?;
        let after = self.log("network-after.json")?;
        let (blocked, foreign) = judge_network(&before, &after, &self.spec.allow_hosts);
        self.run.network = NetworkOutcome { blocked, foreign };
        self.remove()?;
        Ok(work)
    }

    /// Phase 1. `Ok(true)`: a pre-warmed sandbox for this very spec waits; skip to the work.
    fn refuse(&mut self) -> Result<bool, Stop> {
        let at = Instant::now();
        self.emit.phase("refuse", "start", 0, "");
        let stop = |message: String| Stop::new("refuse", EXIT_REFUSED, message);
        let existing =
            headless::find(&self.spec.name).map_err(|f| Stop::new("refuse", f.code, f.message))?;
        let prewarmed: Option<Prewarmed> = fs::read_to_string(self.prewarmed_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
        if let Some(entry) = existing {
            let fresh = prewarmed.as_ref().is_some_and(|p| {
                p.name == self.spec.name
                    && p.spec_sha256 == self.spec_digest
                    && epoch_secs().saturating_sub(p.at) < PREWARM_TTL.as_secs()
            });
            if fresh && !self.prewarm {
                self.run.created = true;
                self.run.proof_cached = prewarmed.as_ref().is_some_and(|p| p.proof_cached);
                self.run.image = entry.agent.clone();
                self.emit
                    .phase("refuse", "ok", ms(at), "continuing a pre-warmed sandbox");
                return Ok(true);
            }
            if prewarmed.is_some() {
                // Stale, or for another spec: it goes, and the step starts over.
                self.emit.warn(
                    "refuse",
                    "the pre-warmed sandbox is stale or for another spec; removing it",
                );
                self.run.created = true;
                self.remove()?;
                self.run.created = false;
                self.run.removed = false;
            } else {
                let message = format!("a sandbox named {} exists", self.spec.name);
                self.emit.phase("refuse", "fail", ms(at), &message);
                return Err(stop(message));
            }
        }
        let selection = skill_select::load(&skill_select::selection_path(&state::state_dir()));
        match selection {
            Ok(sel) if !sel.global.is_empty() => {
                let message = "aisbx has globally selected skills; remove them first".to_string();
                self.emit.phase("refuse", "fail", ms(at), &message);
                return Err(stop(message));
            }
            Ok(sel)
                if sel
                    .sandboxes
                    .get(&self.spec.name)
                    .is_some_and(|p| !p.is_empty()) =>
            {
                let message = format!("{} has a skill selection; remove it first", self.spec.name);
                self.emit.phase("refuse", "fail", ms(at), &message);
                return Err(stop(message));
            }
            Ok(_) => {}
            Err(e) => {
                let message = format!("the skill selection cannot be read: {e:#}");
                self.emit.phase("refuse", "fail", ms(at), &message);
                return Err(stop(message));
            }
        }
        let rules = sbx::global_policy_rules()
            .map_err(|e| stop(format!("the network policy cannot be read: {e:#}")))?;
        if !rules.iter().any(sbx::PolicyRule::is_fallback) {
            let message = sbx::POLICY_INIT_HINT.to_string();
            self.emit.phase("refuse", "fail", ms(at), &message);
            return Err(stop(message));
        }
        self.emit.phase("refuse", "ok", ms(at), "");
        Ok(false)
    }

    /// Phase 2: `aisbx up` with the denies, through the same code as the command.
    fn create(&mut self) -> Result<(), Stop> {
        let at = Instant::now();
        self.emit.phase("create", "start", 0, "");
        let mut argv: Vec<String> = vec![
            "--backend".into(),
            self.spec.backend.clone(),
            "--skills".into(),
            "off".into(),
            "--no-mcp".into(),
            "--no-host-folders".into(),
            "--tmpfs-volumes".into(),
            "--name".into(),
            self.spec.name.clone(),
        ];
        for kit in &self.spec.kits {
            argv.extend(["--kit".into(), kit.clone()]);
        }
        for deny in &self.deny {
            argv.extend(["--deny-network".into(), deny.clone()]);
        }
        for mount in &self.spec.mounts {
            let path = mount.path.display().to_string();
            argv.extend([
                "--mount".into(),
                if mount.readonly {
                    format!("{path}:ro")
                } else {
                    path
                },
            ]);
        }
        if let Some(cpus) = self.spec.cpus {
            argv.extend(["--cpus".into(), cpus.to_string()]);
        }
        if let Some(memory) = &self.spec.memory {
            argv.extend(["--memory".into(), memory.clone()]);
        }
        argv.push(self.workspace.display().to_string());
        let shown: Vec<String> = std::iter::once("aisbx".to_string())
            .chain(std::iter::once("up".to_string()))
            .chain(argv.iter().cloned())
            .collect();
        // The sandbox may exist half-made when `up` fails: always try to remove from here on.
        self.run.created = true;
        let result = headless::up_inner(&argv);
        let exit = match &result {
            Ok(_) => Some(0),
            Err(f) => Some(i32::from(f.code)),
        };
        self.emit.sbx("create", &shown, exit, ms(at));
        if let Err(f) = result {
            let message = format!("sbx create failed: {}", f.message);
            self.emit.phase("create", "fail", ms(at), &message);
            return Err(Stop::new("create", headless::EXIT_SANDBOX, message));
        }
        // What the kits opened beyond the create-time denies: closed now, and said. Under `**`
        // nothing is open, whatever the kits say; the proof checks that.
        let rules = match self.spec.network {
            true => self.sandbox_rules("create")?,
            false => Vec::new(),
        };
        let allowed: BTreeSet<String> = self.spec.allow_hosts.iter().map(|h| host_of(h)).collect();
        let denied: BTreeSet<String> = rules
            .iter()
            .filter(|r| r.is_network() && !r.allows())
            .flat_map(|r| r.resources.iter().cloned())
            .collect();
        for rule in rules.iter().filter(|r| r.is_network() && r.allows()) {
            for resource in &rule.resources {
                let open = self.spec.network && allowed.contains(&host_of(resource));
                if open || denied.contains(resource) || self.deny.contains(resource) {
                    continue;
                }
                if resource.contains('*') && self.spec.network && !allowed.is_empty() {
                    let message = format!("a rule opens {resource:?} to this sandbox; closing it would close the allowed hosts too");
                    self.emit.phase("create", "fail", ms(at), &message);
                    return Err(Stop::new("create", headless::EXIT_SANDBOX, message));
                }
                let argv = vec![
                    "sbx".to_string(),
                    "policy".into(),
                    "deny".into(),
                    "network".into(),
                    "--sandbox".into(),
                    self.spec.name.clone(),
                    resource.clone(),
                ];
                let t = Instant::now();
                let ok = headless::quiet(&argv);
                self.emit
                    .sbx("create", &argv, Some(if ok { 0 } else { 1 }), ms(t));
                self.emit.warn(
                    "create",
                    &format!("{resource} was open to the sandbox beyond the kits' descriptors; denied now"),
                );
                if !ok {
                    let message = format!("could not deny {resource}");
                    self.emit.phase("create", "fail", ms(at), &message);
                    return Err(Stop::new("create", headless::EXIT_SANDBOX, message));
                }
            }
        }
        if let Ok(inspected) = sbx::inspect(&self.spec.name) {
            self.run.image = inspected.image;
        }
        self.run.ms.create = ms(at);
        self.emit.phase("create", "ok", ms(at), "");
        Ok(())
    }

    fn sandbox_rules(&mut self, phase: &'static str) -> Result<Vec<sbx::PolicyRule>, Stop> {
        let t = Instant::now();
        let argv = vec![
            "sbx".to_string(),
            "policy".into(),
            "ls".into(),
            self.spec.name.clone(),
            "--json".into(),
        ];
        let rules = sbx::policy_rules(&self.spec.name);
        self.emit
            .sbx(phase, &argv, Some(if rules.is_ok() { 0 } else { 1 }), ms(t));
        rules.map_err(|e| {
            Stop::new(
                phase,
                headless::EXIT_SANDBOX,
                format!("the sandbox's rules cannot be read: {e:#}"),
            )
        })
    }

    /// Phase 3: the proof, cached by its key.
    fn prove(&mut self) -> Result<(), Stop> {
        let at = Instant::now();
        self.emit.phase("prove", "start", 0, "");
        let rules = self.sandbox_rules("prove")?;
        let probes = probes(
            &self.spec.allow_hosts,
            &self.spec.probe_hosts,
            self.spec.network,
        );
        let key = proof_key(&rules, &probes, self.spec.network);
        let now = epoch_secs();
        let dir = proofs_dir(&state::state_dir());
        let (results, cached) = match read_cached_proof(&dir, &key, now) {
            Some(results) => (results, true),
            None => {
                let t = Instant::now();
                let results = run_probes(&self.spec.name, &probes);
                let argv = vec![
                    "sbx".to_string(),
                    "policy".into(),
                    "check".into(),
                    "network".into(),
                    format!("×{}", results.len()),
                ];
                self.emit.sbx("prove", &argv, Some(0), ms(t));
                (results, false)
            }
        };
        let failed: Vec<&ProbeResult> = results.iter().filter(|r| !r.ok()).collect();
        let doc = serde_json::json!({
            "key": key,
            "cached": cached,
            "ok": failed.is_empty(),
            "probes": results,
            "rules": rules.iter().map(rule_facts).collect::<Vec<_>>(),
        });
        write_json(&self.dir.join("policy.json"), &doc);
        if !failed.is_empty() {
            let message = failed
                .iter()
                .map(|r| {
                    format!(
                        "{} {} is {}",
                        r.protocol,
                        r.target,
                        if r.allowed {
                            "reachable but must not be"
                        } else {
                            "unreachable but must be"
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            self.emit.phase("prove", "fail", ms(at), &message);
            return Err(Stop::new("prove", EXIT_PROOF, message));
        }
        if !cached {
            write_cached_proof(&dir, &key, &results, now);
        }
        self.run.proof_cached = cached;
        self.run.ms.prove = ms(at);
        self.emit.emit(
            "phase",
            "prove",
            serde_json::json!({"status": "ok", "ms": ms(at), "detail": "", "cached": cached, "probes": results.len()}),
        );
        Ok(())
    }

    /// Phases 4 and 6: the proxy log, saved.
    fn log(&mut self, file: &str) -> Result<sbx::PolicyLog, Stop> {
        let at = Instant::now();
        self.emit.phase("log", "start", 0, file);
        let argv = vec![
            "sbx".to_string(),
            "policy".into(),
            "log".into(),
            self.spec.name.clone(),
            "--json".into(),
        ];
        let log = sbx::policy_log(&self.spec.name, LOG_LIMIT);
        self.emit
            .sbx("log", &argv, Some(if log.is_ok() { 0 } else { 1 }), ms(at));
        let log = log.map_err(|e| {
            let message = format!("the proxy log cannot be read: {e:#}");
            self.emit.phase("log", "fail", ms(at), &message);
            Stop::new("log", headless::EXIT_SANDBOX, message)
        })?;
        write_json(&self.dir.join(file), &log_json(&log));
        self.emit.phase("log", "ok", ms(at), file);
        Ok(log)
    }

    /// Phase 5. `Ok` carries the exit code the work decides: 124 on timeout, 130 on Ctrl-C,
    /// else 0 — the agent's or a check's own exit is in the report, not here.
    fn work(&mut self) -> Result<u8, Stop> {
        let at = Instant::now();
        self.emit.phase("work", "start", 0, "");
        if self.interrupted() {
            self.run.interrupted = true;
            self.emit
                .phase("work", "fail", 0, "interrupted before the work started");
            return Err(Stop::new("work", headless::EXIT_INTERRUPTED, "interrupted"));
        }
        let outcome = match self.spec.is_agent() {
            true => self.work_agent(),
            false => self.work_checks(),
        };
        self.run.ms.work = ms(at);
        match outcome {
            Ok(code) => {
                self.emit.phase(
                    "work",
                    if code == 0 { "ok" } else { "fail" },
                    ms(at),
                    match code {
                        headless::EXIT_TIMEOUT => "timed out",
                        headless::EXIT_INTERRUPTED => "interrupted",
                        _ => "",
                    },
                );
                Ok(code)
            }
            Err(stop) => {
                self.emit.phase("work", "fail", ms(at), &stop.message);
                Err(stop)
            }
        }
    }

    fn work_agent(&mut self) -> Result<u8, Stop> {
        let agent = self.spec.work.agent.clone().expect("an agent step");
        let prompt = self.prompt.clone().expect("the prompt was read");
        let name = self.spec.name.clone();
        let sandbox = |m: String| Stop::new("work", headless::EXIT_SANDBOX, m);
        let mut extra = kits::max_turns_args(&self.agent, agent.max_turns);
        extra.extend(agent.args.iter().cloned());
        let timeout_text = format!("{}s", self.spec.timeout_seconds);
        let argv = headless::agent_argv(
            &name,
            &self.agent,
            &HeadlessRun {
                model: Some(&agent.model),
                effort: agent.effort.as_deref(),
                format: OutputFormat::StreamJson,
                print_timeout: &timeout_text,
                extra: &extra,
            },
        )
        .map_err(|f| Stop::new("work", f.code, f.message))?;
        let run_dir = format!("/tmp/aisbx-step-{}-{}", epoch_secs(), std::process::id());
        let t = Instant::now();
        let upload = headless::upload_prompt(&name, &run_dir, &prompt, false);
        self.emit.sbx(
            "work",
            &sbx::prompt_upload_argv(&name, &run_dir),
            Some(if upload.is_ok() { 0 } else { 1 }),
            ms(t),
        );
        upload.map_err(|f| sandbox(f.message))?;

        let launch = sbx::headless_exec_argv(&name, None, &run_dir, &argv);
        let stream_path = self.dir.join("stream.ndjson");
        let mut stream = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stream_path)
            .map_err(|e| sandbox(format!("opening {}: {e}", stream_path.display())))?;
        let started = Instant::now();
        let mut child = headless::sbx_command(&launch, false)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| sandbox(format!("could not run sbx: {e}")))?;
        let stdout = child.stdout.take().expect("piped");
        let (tx, rx) = mpsc::channel::<String>();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut outcome = AgentOutcome::default();
        let deadline = started + Duration::from_secs(self.spec.timeout_seconds);
        let mut stopped = false;
        let status: Option<ExitStatus> = loop {
            while let Ok(line) = rx.try_recv() {
                self.take_agent_line(&mut stream, &mut outcome, &line);
            }
            if let Ok(Some(s)) = child.try_wait() {
                break Some(s);
            }
            let timed_out = Instant::now() >= deadline;
            let interrupted = self.interrupted();
            if timed_out || interrupted {
                outcome.timed_out = timed_out && !interrupted;
                outcome.interrupted = interrupted;
                stopped = true;
                break headless::stop_agent(&mut child, &name, &run_dir);
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        // The pipe closes when the agent and everything holding its stdout are gone.
        while let Ok(line) = rx.recv_timeout(Duration::from_secs(2)) {
            self.take_agent_line(&mut stream, &mut outcome, &line);
        }
        drop(rx);
        let _ = reader.join();
        self.emit.sbx(
            "work",
            &launch[..launch.len().min(4)],
            status.and_then(|s| s.code()),
            ms(started),
        );
        outcome.started = stopped || headless::quiet(&sbx::headless_started_argv(&name, &run_dir));
        if !stopped {
            headless::quiet(&sbx::headless_cleanup_argv(&name, &run_dir));
        }
        outcome.exit = status.and_then(|s| s.code());
        for title in headless::held_requests(&name, true) {
            self.emit
                .warn("work", &format!("held for approval and dismissed: {title}"));
        }
        let code = if outcome.interrupted {
            self.run.interrupted = true;
            headless::EXIT_INTERRUPTED
        } else if outcome.timed_out {
            headless::EXIT_TIMEOUT
        } else if !outcome.started {
            self.run.agent = Some(outcome);
            return Err(sandbox("the agent never started".into()));
        } else {
            0
        };
        self.run.agent = Some(outcome);
        Ok(code)
    }

    fn take_agent_line(&mut self, stream: &mut File, outcome: &mut AgentOutcome, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        outcome.lines += 1;
        let _ = writeln!(stream, "{line}");
        let _ = stream.flush();
        let parsed: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => serde_json::json!({"raw": line}),
        };
        match (parsed["type"].as_str(), parsed["subtype"].as_str()) {
            (Some("system"), Some("init")) => outcome.init_seen = true,
            (Some("result"), _) => outcome.result_seen = true,
            (Some("assistant"), _) => outcome.assistant_seen = true,
            _ => {}
        }
        self.emit
            .emit("agent", "work", serde_json::json!({"line": parsed}));
    }

    fn work_checks(&mut self) -> Result<u8, Stop> {
        let checks = self.spec.work.checks.clone().expect("a checks step");
        let name = self.spec.name.clone();
        let deadline = Instant::now() + Duration::from_secs(self.spec.timeout_seconds);
        let mut records: Vec<serde_json::Value> = Vec::new();
        let mut code = 0u8;
        for (index, check) in checks.iter().enumerate() {
            if self.interrupted() {
                self.run.interrupted = true;
                code = headless::EXIT_INTERRUPTED;
                break;
            }
            let workdir = check_workdir(&self.workspace, &check.workdir);
            let run_dir = format!(
                "/tmp/aisbx-step-{}-{}-{index}",
                epoch_secs(),
                std::process::id()
            );
            let mut argv = vec![
                "sbx".to_string(),
                "exec".into(),
                "-w".into(),
                workdir.clone(),
            ];
            for (k, v) in &check.env {
                argv.extend(["-e".into(), format!("{k}={v}")]);
            }
            argv.extend([
                name.clone(),
                "sh".into(),
                "-c".into(),
                CHECK_WRAPPER.into(),
                "sh".into(),
                run_dir.clone(),
            ]);
            argv.extend(check.argv.iter().cloned());
            let t = Instant::now();
            let mut child = headless::sbx_command(&argv, false)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| {
                    Stop::new(
                        "work",
                        headless::EXIT_SANDBOX,
                        format!("could not run sbx: {e}"),
                    )
                })?;
            let out = collect(child.stdout.take().expect("piped"), CHECK_STDOUT_LIMIT);
            let err = collect(child.stderr.take().expect("piped"), CHECK_STDERR_LIMIT);
            let mut timed_out = false;
            let status = loop {
                if let Ok(Some(s)) = child.try_wait() {
                    break Some(s);
                }
                if Instant::now() >= deadline || self.interrupted() {
                    timed_out = !self.interrupted();
                    break headless::stop_agent(&mut child, &name, &run_dir);
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            let (stdout, stdout_bytes) = out.join().unwrap_or_default();
            let (stderr, stderr_bytes) = err.join().unwrap_or_default();
            if !timed_out && !self.interrupted() {
                headless::quiet(&sbx::headless_cleanup_argv(&name, &run_dir));
            }
            let exit = status.and_then(|s| s.code());
            let shown: Vec<String> = ["sbx", "exec", "-w", workdir.as_str(), name.as_str()]
                .iter()
                .map(|s| s.to_string())
                .chain(check.argv.iter().cloned())
                .collect();
            self.emit.sbx("work", &shown, exit, ms(t));
            self.emit.emit(
                "check",
                "work",
                serde_json::json!({"index": index, "argv": check.argv, "workdir": check.workdir, "exit": exit, "ms": ms(t), "timed_out": timed_out}),
            );
            records.push(serde_json::json!({
                "index": index, "argv": check.argv, "workdir": check.workdir, "env": check.env,
                "exit": exit, "timed_out": timed_out,
                "stdout": String::from_utf8_lossy(&stdout), "stderr": String::from_utf8_lossy(&stderr),
                "stdout_bytes": stdout_bytes, "stderr_bytes": stderr_bytes,
                "stdout_limit": CHECK_STDOUT_LIMIT, "stderr_limit": CHECK_STDERR_LIMIT,
            }));
            self.run.checks.push(CheckOutcome {
                index,
                argv: check.argv.clone(),
                workdir: check.workdir.clone(),
                exit,
                timed_out,
                stdout_bytes,
                stderr_bytes,
            });
            if self.interrupted() {
                self.run.interrupted = true;
                code = headless::EXIT_INTERRUPTED;
                break;
            }
            if timed_out {
                code = headless::EXIT_TIMEOUT;
                break;
            }
        }
        write_json(
            &self.dir.join("checks.json"),
            &serde_json::Value::Array(records),
        );
        Ok(code)
    }

    /// Phase 7: remove, always, then prove the name is gone.
    fn remove(&mut self) -> Result<(), Stop> {
        if self.run.removed {
            return Ok(());
        }
        let at = Instant::now();
        self.emit.phase("remove", "start", 0, "");
        let fail = |m: String| Stop::new("remove", headless::EXIT_SANDBOX, m);
        let argv = sbx::rm_argv(&self.spec.name);
        let result = headless::rm_inner(&self.spec.name);
        self.emit.sbx(
            "remove",
            &argv,
            Some(if result.is_ok() { 0 } else { 1 }),
            ms(at),
        );
        if let Err(f) = result {
            // A sandbox that never got made fails to be removed too: then it is gone either way.
            if headless::find(&self.spec.name).ok().flatten().is_some() {
                let message = format!("could not remove {}: {}", self.spec.name, f.message);
                self.emit.phase("remove", "fail", ms(at), &message);
                return Err(fail(message));
            }
        }
        let t = Instant::now();
        let listed = sbx::list();
        self.emit.sbx(
            "remove",
            &["sbx".to_string(), "ls".into(), "--json".into()],
            Some(if listed.is_ok() { 0 } else { 1 }),
            ms(t),
        );
        match listed {
            Ok(entries) if entries.iter().any(|e| e.name == self.spec.name) => {
                let message = format!("{} still exists after sbx rm", self.spec.name);
                self.emit.phase("remove", "fail", ms(at), &message);
                Err(fail(message))
            }
            Ok(_) => {
                self.run.removed = true;
                self.run.ms.remove = ms(at);
                self.emit.phase("remove", "ok", ms(at), "");
                Ok(())
            }
            Err(e) => {
                let message = format!("cannot tell whether {} is gone: {e:#}", self.spec.name);
                self.emit.phase("remove", "fail", ms(at), &message);
                Err(fail(message))
            }
        }
    }

    /// Phase 8.
    fn report(&mut self, code: u8, failed: Option<&Stop>) {
        let work = match (&self.run.agent, self.spec.is_agent()) {
            (Some(agent), _) => serde_json::json!({
                "kind": "agent", "started": agent.started, "exit": agent.exit,
                "timed_out": agent.timed_out, "interrupted": agent.interrupted,
                "lines": agent.lines, "init_seen": agent.init_seen,
                "assistant_seen": agent.assistant_seen, "result_seen": agent.result_seen,
            }),
            (None, true) => serde_json::json!({"kind": "agent", "started": false}),
            (None, false) => serde_json::json!({
                "kind": "checks",
                "results": self.run.checks.iter().map(|c| serde_json::json!({
                    "index": c.index, "argv": c.argv, "workdir": c.workdir, "exit": c.exit,
                    "timed_out": c.timed_out, "stdout_bytes": c.stdout_bytes, "stderr_bytes": c.stderr_bytes,
                })).collect::<Vec<_>>(),
            }),
        };
        let doc = serde_json::json!({
            "protocol": PROTOCOL,
            "name": self.run.name,
            "phase": failed.map_or("report", |s| s.phase),
            "ok": failed.is_none(),
            "exit": code,
            "error": failed.map(|s| s.message.clone()),
            "created_at": self.run.created_at,
            "finished_at": now_rfc3339(),
            "ms": self.run.ms,
            "proof_cached": self.run.proof_cached,
            "prewarm": self.prewarm,
            "work": work,
            "network": {"blocked": self.run.network.blocked, "foreign": self.run.network.foreign},
            "image": self.run.image,
            "sbx_version": sbx::version().unwrap_or_default(),
            "aisbx_version": crate::version(),
        });
        write_json(&self.dir.join("report.json"), &doc);
        self.emit.phase(
            "report",
            if failed.is_none() { "ok" } else { "fail" },
            self.run.ms.total,
            &failed.map(|s| s.message.clone()).unwrap_or_default(),
        );
    }
}

/// The `sh` a check runs in: record the PID for [`sbx::headless_kill_argv`], then become the
/// command.
const CHECK_WRAPPER: &str = r#"d=$1; shift
umask 077 && mkdir -p "$d" && echo $$ > "$d/pid"
exec "$@""#;

/// Read a child's pipe to the end on a thread, keeping the first `limit` bytes and counting all.
fn collect<R: Read + Send + 'static>(
    mut reader: R,
    limit: usize,
) -> std::thread::JoinHandle<(Vec<u8>, usize)> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut total = 0usize;
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            total += n;
            let room = limit.saturating_sub(kept.len());
            kept.extend_from_slice(&buf[..n.min(room)]);
        }
        (kept, total)
    })
}

// --- doctor ----------------------------------------------------------------------------------------

/// `aisbx doctor --json`: one document, exit 0 whatever it says.
pub fn doctor_json() -> serde_json::Value {
    let mut problems: Vec<String> = Vec::new();
    let sbx_version = match sbx::version() {
        Ok(v) => Some(v),
        Err(e) => {
            problems.push(format!("sbx: {e:#}"));
            None
        }
    };
    let sbx_ok = sbx_version
        .as_deref()
        .is_some_and(sbx::meets_required_version);
    if sbx_version.is_some() && !sbx_ok {
        problems.push(format!(
            "sbx is older than {}, the version this aisbx needs",
            sbx::REQUIRED_VERSION
        ));
    }
    let signed_in = match Command::new("sbx").args(["ls", "--json"]).output() {
        Ok(out) if out.status.success() => Some(true),
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stderr).to_ascii_lowercase();
            if text.contains("not signed in") {
                problems.push("not signed in to Docker: run `sbx login`".into());
                Some(false)
            } else {
                problems.push(format!("sbx ls failed: {}", text.trim()));
                None
            }
        }
        Err(_) => None,
    };
    let fallback = match sbx::global_policy_rules() {
        Ok(rules) => {
            let has = rules.iter().any(sbx::PolicyRule::is_fallback);
            if !has {
                problems.push(sbx::POLICY_INIT_HINT.to_string());
            }
            Some(has)
        }
        Err(_) => None,
    };
    let (kits, user_dirs, skipped) = match Catalog::load() {
        Ok(catalog) => {
            let names: Vec<serde_json::Value> = catalog
                .backends
                .iter()
                .map(|b| serde_json::json!({"name": b.kit.name, "kind": "backend", "source": if b.kit.user { "user" } else { "aisbx" }}))
                .chain(catalog.all_addons().map(|a| {
                    serde_json::json!({"name": a.kit.name, "kind": "addon", "source": if a.kit.user { "user" } else { "aisbx" }})
                }))
                .collect();
            (
                names,
                catalog.user_dirs.clone(),
                catalog.user_problems.clone(),
            )
        }
        Err(e) => {
            problems.push(format!("kits: {e:#}"));
            (Vec::new(), kits::user_kit_dirs(), Vec::new())
        }
    };
    for skip in &skipped {
        problems.push(format!("kit skipped: {skip}"));
    }
    serde_json::json!({
        "aisbx_version": crate::version(),
        "step_protocol": PROTOCOL,
        "sbx": {"version": sbx_version, "required": sbx::REQUIRED_VERSION, "ok": sbx_ok, "signed_in": signed_in},
        "policy": {"fallback_rule": fallback},
        "kits": {"user_dirs": user_dirs, "found": kits, "skipped": skipped},
        "proofs_dir": proofs_dir(&state::state_dir()),
        "ok": problems.is_empty(),
        "problems": problems,
    })
}

/// `aisbx doctor` without `--json`: one line per fact.
pub fn doctor_text(doc: &serde_json::Value) -> String {
    let mut lines = vec![
        format!(
            "aisbx {}  step protocol {}",
            doc["aisbx_version"].as_str().unwrap_or(""),
            doc["step_protocol"]
        ),
        format!(
            "sbx {}  (needs {}+): {}",
            doc["sbx"]["version"].as_str().unwrap_or("missing"),
            doc["sbx"]["required"].as_str().unwrap_or(""),
            if doc["sbx"]["ok"].as_bool().unwrap_or(false) {
                "ok"
            } else {
                "not ok"
            }
        ),
        format!(
            "signed in: {}",
            match doc["sbx"]["signed_in"].as_bool() {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            }
        ),
        format!(
            "deny-all fallback rule: {}",
            match doc["policy"]["fallback_rule"].as_bool() {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            }
        ),
    ];
    for dir in doc["kits"]["user_dirs"].as_array().into_iter().flatten() {
        lines.push(format!("user kits: {}", dir.as_str().unwrap_or("")));
    }
    let found: Vec<String> = doc["kits"]["found"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|k| {
            format!(
                "{} ({})",
                k["name"].as_str().unwrap_or(""),
                k["source"].as_str().unwrap_or("")
            )
        })
        .collect();
    lines.push(format!("kits: {}", found.join(", ")));
    for problem in doc["problems"].as_array().into_iter().flatten() {
        lines.push(format!("problem: {}", problem.as_str().unwrap_or("")));
    }
    lines.push(if doc["ok"].as_bool().unwrap_or(false) {
        "ok".into()
    } else {
        "not ok".into()
    });
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::EXIT_USAGE;

    #[test]
    fn a_check_workdir_is_a_folder_inside_the_sandbox() {
        assert_eq!(check_workdir(Path::new("/w"), ""), "/w");
        assert_eq!(check_workdir(Path::new("/w"), "app"), "/w/app");
        assert_eq!(check_workdir(Path::new("/w"), "web/src/./"), "/w/web/src");
        #[cfg(windows)]
        {
            assert_eq!(
                check_workdir(Path::new(r"C:\Users\me\source"), "pwr-webclient/sub"),
                "/c/Users/me/source/pwr-webclient/sub"
            );
            assert_eq!(
                check_workdir(Path::new(r"\\?\C:\Users\me\source"), "app"),
                "/c/Users/me/source/app"
            );
        }
    }

    const SPEC: &str = r#"{"protocol": 1, "name": "sf-1", "backend": "claude-anthropic", "kits": ["jdk25"],
      "workspace": "/w", "mounts": [{"path": "/w/node_modules", "readonly": true}],
      "allow_hosts": ["api.anthropic.com"], "probe_hosts": ["github.com", "example.com"],
      "work": {"agent": {"prompt_file": "p.md", "model": "haiku", "max_turns": 3, "args": ["--tools", "Read"]}},
      "timeout_seconds": 60}"#;

    #[test]
    fn a_spec_parses_with_its_defaults() {
        let spec = Spec::parse(SPEC).unwrap();
        assert_eq!(spec.protocol, PROTOCOL);
        assert!(spec.network);
        assert_eq!(spec.kits, ["jdk25"]);
        assert!(spec.mounts[0].readonly);
        assert_eq!(spec.work.agent.as_ref().unwrap().max_turns, 3);
        assert!(spec.is_agent());
    }

    #[test]
    fn unknown_and_bad_fields_are_refused() {
        let bad = SPEC.replacen("\"kits\"", "\"kit\"", 1);
        assert!(Spec::parse(&bad).unwrap_err().contains("not a step spec"));
        let wrong = SPEC.replacen("\"protocol\": 1", "\"protocol\": 2", 1);
        assert!(Spec::parse(&wrong).unwrap_err().contains("protocol 2"));
        let both = SPEC.replacen(
            "\"work\": {",
            "\"work\": {\"checks\": [{\"workdir\": \"\", \"argv\": [\"true\"]}], ",
            1,
        );
        assert!(Spec::parse(&both).unwrap_err().contains("both"));
        let none = SPEC.replacen("\"timeout_seconds\": 60", "\"timeout_seconds\": 0", 1);
        assert!(Spec::parse(&none).unwrap_err().contains("timeout_seconds"));
        let name = SPEC.replacen("\"sf-1\"", "\"Bad Name\"", 1);
        assert!(Spec::parse(&name).unwrap_err().contains("not an sbx name"));
    }

    #[test]
    fn a_checks_spec_needs_relative_workdirs() {
        let checks = r#"{"protocol": 1, "name": "c-1", "backend": "claude-anthropic", "workspace": "/w",
          "network": false, "work": {"checks": [{"workdir": "app", "argv": ["npm", "test"], "env": {"CI": "1"}}]},
          "timeout_seconds": 5}"#;
        let spec = Spec::parse(checks).unwrap();
        assert!(!spec.network && !spec.is_agent());
        let abs = checks.replacen("\"app\"", "\"/app\"", 1);
        assert!(Spec::parse(&abs).unwrap_err().contains("relative"));
        let empty = checks.replacen("[\"npm\", \"test\"]", "[]", 1);
        assert!(Spec::parse(&empty).unwrap_err().contains("argv is empty"));
    }

    #[test]
    fn denies_close_what_the_kits_open_except_the_allowed_hosts() {
        let kit = strings(&[
            "api.anthropic.com",
            "claude.ai",
            "downloads.claude.ai",
            "claude.ai",
        ]);
        let out = denies(&kit, &strings(&["api.anthropic.com:443"]), true).unwrap();
        assert_eq!(
            out,
            strings(&["claude.ai", "downloads.claude.ai", "api.anthropic.com:80"])
        );
        assert_eq!(denies(&kit, &[], false).unwrap(), strings(&["**"]));
        let wild = strings(&["*.example.com"]);
        assert!(denies(&wild, &strings(&["api.anthropic.com"]), true).is_err());
        assert_eq!(
            denies(&wild, &[], true).unwrap(),
            strings(&["*.example.com"])
        );
    }

    #[test]
    fn host_of_strips_scheme_port_and_path() {
        assert_eq!(host_of("API.Example.com:443"), "api.example.com");
        assert_eq!(host_of("https://a.b/c"), "a.b");
        assert_eq!(host_of("a.b"), "a.b");
        assert_eq!(host_of("**"), "**");
    }

    #[test]
    fn probes_cover_allowed_hosts_three_ways_and_probe_hosts_once() {
        let out = probes(
            &strings(&["api.anthropic.com"]),
            &strings(&["github.com", "api.anthropic.com"]),
            true,
        );
        let names: Vec<String> = out
            .iter()
            .map(|p| format!("{} {} {}", p.protocol, p.target, p.must_allow))
            .collect();
        assert_eq!(
            names,
            [
                "tcp api.anthropic.com:443 true",
                "tcp api.anthropic.com:80 false",
                "udp api.anthropic.com:443 false",
                "tcp github.com:443 false"
            ]
        );
        let offline = probes(&strings(&["api.anthropic.com"]), &[], false);
        assert!(offline.iter().all(|p| !p.must_allow));
    }

    fn rule(scope: &str, decision: &str, resources: &[&str]) -> sbx::PolicyRule {
        sbx::PolicyRule {
            scope: scope.into(),
            decision: decision.into(),
            resource_type: "network".into(),
            resources: strings(resources),
            ..Default::default()
        }
    }

    #[test]
    fn the_proof_key_depends_on_rules_probes_and_network_but_not_on_order() {
        let a = [
            rule("sandbox:x", "allow", &["a", "b"]),
            rule("global", "deny", &["**"]),
        ];
        let b = [
            rule("global", "deny", &["**"]),
            rule("sandbox:x", "allow", &["b", "a"]),
        ];
        let probes = probes(&strings(&["a"]), &[], true);
        assert_eq!(proof_key(&a, &probes, true), proof_key(&b, &probes, true));
        assert_ne!(proof_key(&a, &probes, true), proof_key(&a, &probes, false));
        let more = probes_with(&["a"], &["c"]);
        assert_ne!(proof_key(&a, &probes, true), proof_key(&a, &more, true));
        let mut fs = rule("sandbox:x", "allow", &["a"]);
        fs.resource_type = "filesystem:read".into();
        assert_eq!(
            proof_key(&[fs], &probes, true),
            proof_key(&[], &probes, true)
        );
    }

    fn probes_with(allow: &[&str], probe: &[&str]) -> Vec<Probe> {
        probes(&strings(allow), &strings(probe), true)
    }

    #[test]
    fn a_cached_proof_is_reused_only_while_fresh_and_green() {
        let dir = std::env::temp_dir().join(format!("aisbx-proofs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let green = vec![ProbeResult {
            protocol: "tcp".into(),
            target: "a:443".into(),
            must_allow: true,
            allowed: true,
            detail: String::new(),
        }];
        write_cached_proof(&dir, "k", &green, 1_000);
        assert_eq!(
            read_cached_proof(&dir, "k", 1_000 + 60),
            Some(green.clone())
        );
        assert!(read_cached_proof(&dir, "k", 1_000 + PROOF_TTL.as_secs()).is_none());
        assert!(read_cached_proof(&dir, "other", 1_000).is_none());
        let red = vec![ProbeResult {
            allowed: false,
            ..green[0].clone()
        }];
        write_cached_proof(&dir, "r", &red, 1_000);
        assert!(read_cached_proof(&dir, "r", 1_000).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn events_carry_the_four_fixed_fields_and_count_up() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let mut emit = Emitter::into_buffer(Arc::clone(&buffer));
        emit.phase("create", "start", 0, "");
        emit.sbx("create", &strings(&["sbx", "create"]), Some(0), 12);
        emit.emit(
            "agent",
            "work",
            serde_json::json!({"line": {"type": "result"}}),
        );
        let text = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(line["seq"], i as u64 + 1);
            assert!(line["t"].as_str().unwrap().ends_with('Z'));
            assert!(line["kind"].is_string() && line["phase"].is_string());
        }
        assert_eq!(lines[0]["kind"], "phase");
        assert_eq!(lines[0]["status"], "start");
        assert_eq!(lines[1]["argv"], serde_json::json!(["sbx", "create"]));
        assert_eq!(lines[1]["ms"], 12);
        assert_eq!(lines[2]["line"]["type"], "result");
    }

    #[test]
    fn timestamps_are_rfc3339_utc_with_milliseconds() {
        assert_eq!(rfc3339_ms(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339_ms(951_782_400, 7), "2000-02-29T00:00:00.007Z");
        assert_eq!(rfc3339_ms(1_762_851_123, 456), "2025-11-11T08:52:03.456Z");
        let now = now_rfc3339();
        assert!(state::parse_rfc3339(&now).is_some(), "{now}");
    }

    fn log(allowed: &[(&str, u64)], blocked: &[(&str, u64)]) -> sbx::PolicyLog {
        let host = |(h, c): &(&str, u64)| sbx::LogHost {
            host: (*h).into(),
            count: *c,
            ..Default::default()
        };
        sbx::PolicyLog {
            allowed: allowed.iter().map(host).collect(),
            blocked: blocked.iter().map(host).collect(),
        }
    }

    #[test]
    fn the_network_judgement_counts_only_new_traffic() {
        let before = log(
            &[("api.anthropic.com:443", 2)],
            &[("ports.ubuntu.com:443", 1)],
        );
        let after = log(
            &[("api.anthropic.com:443", 9), ("telemetry.example:443", 1)],
            &[("ports.ubuntu.com:443", 1), ("github.com:443", 1)],
        );
        let (blocked, foreign) = judge_network(&before, &after, &strings(&["api.anthropic.com"]));
        assert_eq!(blocked, strings(&["github.com:443"]));
        assert_eq!(foreign, strings(&["telemetry.example:443"]));
        let (none, no_foreign) = judge_network(&after, &after, &strings(&["api.anthropic.com"]));
        assert!(none.is_empty() && no_foreign.is_empty());
    }

    #[test]
    fn the_subcommands_and_help_name_step_and_doctor() {
        assert!(headless::is_subcommand("step"));
        assert!(headless::is_subcommand("doctor"));
        assert!(headless::HELP.contains("aisbx step --spec FILE --dir DIR"));
        assert!(headless::HELP.contains("aisbx doctor [--json]"));
        assert_eq!(main(&strings(&["--protocol"])).unwrap(), 0);
        assert_eq!(
            main(&strings(&["--dir", "x"])).unwrap_err().code,
            EXIT_USAGE
        );
        assert_eq!(
            main(&strings(&["--spec", "/nowhere.json", "--dir", "x"]))
                .unwrap_err()
                .code,
            EXIT_USAGE
        );
    }

    #[test]
    fn doctor_text_lists_the_facts_and_problems() {
        let doc = serde_json::json!({
            "aisbx_version": "0.51.0.3", "step_protocol": 1,
            "sbx": {"version": "v0.51.0", "required": "0.51.0", "ok": true, "signed_in": false},
            "policy": {"fallback_rule": true},
            "kits": {"user_dirs": ["/u/kits"], "found": [{"name": "claude-anthropic", "source": "aisbx"}], "skipped": []},
            "ok": false, "problems": ["not signed in to Docker: run `sbx login`"],
        });
        let text = doctor_text(&doc);
        assert!(text.contains("aisbx 0.51.0.3  step protocol 1"));
        assert!(text.contains("signed in: no"));
        assert!(text.contains("user kits: /u/kits"));
        assert!(text.contains("kits: claude-anthropic (aisbx)"));
        assert!(text.ends_with("not ok"));
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tests/step")
            .join(name);
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// The golden fixtures are the contract another repository tests against: every line has
    /// the four fixed fields, the phases run in the documented order, and the reports have the
    /// documented keys.
    #[test]
    fn the_golden_fixtures_follow_the_protocol() {
        let order = [
            "refuse", "create", "prove", "log", "work", "log", "remove", "report",
        ];
        for name in ["checks.ndjson", "agent.ndjson", "prewarmed.ndjson"] {
            let mut phases = Vec::new();
            for (i, line) in fixture(name).lines().filter(|l| !l.is_empty()).enumerate() {
                let d: serde_json::Value = serde_json::from_str(line).unwrap();
                assert_eq!(d["seq"], i as u64 + 1, "{name}");
                assert!(
                    state::parse_rfc3339(d["t"].as_str().unwrap()).is_some(),
                    "{name}"
                );
                assert!(
                    ["phase", "sbx", "agent", "check", "warn"]
                        .contains(&d["kind"].as_str().unwrap()),
                    "{name}: {line}"
                );
                assert!(d["phase"].as_str().is_some_and(|p| !p.is_empty()), "{name}");
                if d["kind"] == "phase" && d["status"] != "start" {
                    phases.push(d["phase"].as_str().unwrap().to_string());
                }
            }
            assert_eq!(phases.last().map(String::as_str), Some("report"), "{name}");
            let done: Vec<&str> = phases
                .iter()
                .map(String::as_str)
                .filter(|p| *p != "report")
                .collect();
            let expected: Vec<&str> = match name {
                // A continued pre-warm skips create and prove.
                "prewarmed.ndjson" => vec!["refuse", "log", "work", "log", "remove"],
                _ => order[..order.len() - 1].to_vec(),
            };
            assert_eq!(done, expected, "{name}");
        }
        for (name, kind) in [
            ("checks-report.json", "checks"),
            ("agent-report.json", "agent"),
        ] {
            let d: serde_json::Value = serde_json::from_str(&fixture(name)).unwrap();
            assert_eq!(d["protocol"], PROTOCOL, "{name}");
            for key in [
                "name",
                "phase",
                "ok",
                "exit",
                "error",
                "created_at",
                "finished_at",
                "ms",
                "proof_cached",
                "prewarm",
                "work",
                "network",
                "image",
                "sbx_version",
                "aisbx_version",
            ] {
                assert!(d.get(key).is_some(), "{name} lacks {key}");
            }
            for key in ["create", "prove", "work", "remove", "total"] {
                assert!(d["ms"][key].is_u64(), "{name} ms lacks {key}");
            }
            assert_eq!(d["work"]["kind"], kind, "{name}");
        }
        let agent: serde_json::Value = serde_json::from_str(&fixture("agent-report.json")).unwrap();
        assert!(
            agent["work"]["init_seen"].as_bool().unwrap()
                && agent["work"]["result_seen"].as_bool().unwrap()
        );
        for name in ["checks-spec.json", "agent-spec.json"] {
            Spec::parse(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}
