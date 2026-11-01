//! `aisbx skill-repo`: the configured skill repositories (`skill-repos.json`) and a local mirror of
//! each (`skill-cache/<id>/`), where git is the authority for what is cached. No token is ever
//! written: not to the list, the cache, argv or a message.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CONFIG_FILE: &str = "skill-repos.json";
pub const CACHE_DIR: &str = "skill-cache";
const VERSION: u32 = 1;

/// A hung clone or fetch (a credential prompt nobody answers, a stalled server) is killed after
/// this.
const TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Segments starting like this are a pasted token, not a repository name.
const TOKEN_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
];

const HOOKS_OFF: &str = "core.hooksPath=/dev/null";
const GITHUB: &str = "github.com";

/// Inherited variables that would point git at another repository than the one it is given.
const GIT_REPO_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
];

// --- the list ------------------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub repos: Vec<Repo>,
    /// Whether a sandbox's start moves its installed skills to the cached commit. Absent is on;
    /// written only once set (`aisbx skill-repo auto-update on|off`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: VERSION,
            repos: Vec::new(),
            auto_update: None,
        }
    }
}

impl Config {
    pub fn auto_update(&self) -> bool {
        self.auto_update != Some(false)
    }
}

/// Turn automatic skill updates at a sandbox's start on or off, for every sandbox.
pub fn set_auto_update(state: &Path, on: bool) -> Result<()> {
    update(&config_path(state), |config| {
        config.auto_update = Some(on);
        Ok(())
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Repo {
    /// What git is given (normalised by [`Source::stored`]); never carries credentials.
    pub source: String,
    /// A branch or tag; `None` is the remote's default branch.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub via: Method,
}

impl Repo {
    pub fn parsed(&self) -> Result<Source, String> {
        parse_source(&self.source)
    }

    pub fn id(&self) -> Result<String, String> {
        Ok(repo_id(&self.parsed()?, self.git_ref.as_deref()))
    }
}

/// How a fetch reaches the remote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    /// Try [`attempts`]' chain; the first success wins.
    #[default]
    Auto,
    Gh,
    Ssh,
    Token,
    Git,
}

impl Method {
    pub const ALL: [Method; 5] = [
        Method::Auto,
        Method::Gh,
        Method::Ssh,
        Method::Token,
        Method::Git,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Method::Auto => "auto",
            Method::Gh => "gh",
            Method::Ssh => "ssh",
            Method::Token => "token",
            Method::Git => "git",
        }
    }

    pub fn parse(s: &str) -> Option<Method> {
        Method::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

pub fn config_path(state: &Path) -> PathBuf {
    state.join(CONFIG_FILE)
}

pub fn cache_root(state: &Path) -> PathBuf {
    state.join(CACHE_DIR)
}

/// The list. A missing file is an empty list; a file that does not parse, has another `version`
/// or an entry this build refuses is an error naming the file — never an empty list, which the
/// next save would write over the owner's repos.
pub fn load(path: &Path) -> Result<Config> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let config: Config = serde_json::from_str(&text).with_context(|| {
        format!(
            "{} does not parse; fix or remove it (it is left as it is)",
            path.display()
        )
    })?;
    if config.version != VERSION {
        bail!(
            "{} has version {}, this aisbx reads version {VERSION}; it is left as it is",
            path.display(),
            config.version
        );
    }
    for (i, repo) in config.repos.iter().enumerate() {
        let checked = repo.parsed().and_then(|_| match &repo.git_ref {
            Some(r) if !valid_ref(r) => Err("its ref is not a branch or tag name".to_string()),
            _ => Ok(()),
        });
        // The entry is not quoted: a refused source may be a pasted token.
        if let Err(why) = checked {
            bail!(
                "{}: entry {}: {why}; it is left as it is",
                path.display(),
                i + 1
            );
        }
    }
    Ok(config)
}

/// Change the list under an exclusive lock on `<path>.lock`: load, apply `f`, then write through a
/// temp file and a rename. A list that does not load is not written.
pub fn update<T>(path: &Path, f: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
    let dir = path
        .parent()
        .context("the skill-repo list path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let _lock = lock_file(&path.with_extension("json.lock"))?;
    let mut config = load(path)?;
    let out = f(&mut config)?;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = fs::write(&tmp, serde_json::to_vec_pretty(&config)?)
        .with_context(|| format!("writing {}", tmp.display()))
        .and_then(|()| {
            fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
        });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written.map(|()| out)
}

/// An exclusive lock, held until the returned file is dropped (the OS drops it with the process).
pub fn lock_file(path: &Path) -> Result<fs::File> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.lock()
        .with_context(|| format!("locking {}", path.display()))?;
    Ok(file)
}

/// Add a repository (or, when its source and ref are already listed, keep that one entry and take
/// the new `via`), and hand back its id. `raw` and `git_ref` must have passed [`parse_source`] and
/// [`valid_ref`].
pub fn add(state: &Path, raw: &str, git_ref: Option<&str>, via: Method) -> Result<String> {
    let source = parse_source(raw).map_err(anyhow::Error::msg)?;
    let id = repo_id(&source, git_ref);
    update(&config_path(state), |config| {
        match config
            .repos
            .iter_mut()
            .find(|r| r.id().as_deref() == Ok(id.as_str()))
        {
            Some(existing) => existing.via = via,
            None => config.repos.push(Repo {
                source: source.stored(),
                git_ref: git_ref.map(str::to_string),
                via,
            }),
        }
        Ok(())
    })?;
    Ok(id)
}

/// The entries `handle` names: the one whose id it is, else every entry on that source (any ref).
pub fn matching(config: &Config, handle: &str) -> Vec<usize> {
    let by_id: Vec<usize> = config
        .repos
        .iter()
        .enumerate()
        .filter(|(_, r)| r.id().as_deref() == Ok(handle))
        .map(|(i, _)| i)
        .collect();
    if !by_id.is_empty() {
        return by_id;
    }
    let Ok(wanted) = parse_source(handle) else {
        return Vec::new();
    };
    config
        .repos
        .iter()
        .enumerate()
        .filter(|(_, r)| r.parsed().is_ok_and(|s| s.key() == wanted.key()))
        .map(|(i, _)| i)
        .collect()
}

/// Remove the entry `handle` names and its cache folder. `Ok(None)`: nothing matched. A source
/// listed with several refs is refused — name one by its id.
pub fn remove(state: &Path, handle: &str) -> Result<Option<String>> {
    let removed = update(&config_path(state), |config| {
        let found = matching(config, handle);
        match found.as_slice() {
            [] => Ok(None),
            [i] => {
                let repo = config.repos.remove(*i);
                repo.id().map(Some).map_err(anyhow::Error::msg)
            }
            more => bail!(
                "{handle} matches {} entries (one per ref); name one by its id",
                more.len()
            ),
        }
    })?;
    if let Some(id) = &removed {
        let root = cache_root(state);
        if root.is_dir() {
            let _lock = lock_file(&root.join(format!("{id}.lock")))?;
            remove_cache(&root, id)?;
        }
    }
    Ok(removed)
}

// --- sources and ids -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Https,
    /// `ssh://[user@]host/path` (`scp: false`) or `user@host:path` (`scp: true`).
    Ssh {
        user: Option<String>,
        scp: bool,
    },
}

/// A repository source that passed [`parse_source`]: a host (lowercased, maybe with `:port`) and
/// the path after it, as given minus a trailing `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub transport: Transport,
    pub host: String,
    pub path: String,
}

impl Source {
    /// `(owner, repo)` without `.git`, for a repository on github.com.
    pub fn github(&self) -> Option<(&str, &str)> {
        if self.host != GITHUB {
            return None;
        }
        let path = self.path.trim_start_matches('/');
        let (owner, repo) = path.split_once('/')?;
        let repo = repo.strip_suffix(".git").unwrap_or(repo);
        (!repo.contains('/') && !repo.is_empty()).then_some((owner, repo))
    }

    pub fn is_ssh(&self) -> bool {
        matches!(self.transport, Transport::Ssh { .. })
    }

    /// What is stored and given to git.
    pub fn stored(&self) -> String {
        match &self.transport {
            Transport::Https => format!("https://{}/{}", self.host, self.path),
            Transport::Ssh { user, scp: true } => {
                format!("{}{}:{}", user_at(user), self.host, self.path)
            }
            Transport::Ssh { user, scp: false } => {
                format!("ssh://{}{}/{}", user_at(user), self.host, self.path)
            }
        }
    }

    /// The comparison form: transport, host and path, without a trailing `.git` or `/`;
    /// lowercased on GitHub, where names are case-insensitive.
    pub fn key(&self) -> String {
        let path = self.path.trim_start_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let path = if self.host == GITHUB {
            path.to_lowercase()
        } else {
            path.to_string()
        };
        match &self.transport {
            Transport::Https => format!("https://{}/{path}", self.host),
            Transport::Ssh { user, .. } => format!("ssh://{}{}/{path}", user_at(user), self.host),
        }
    }
}

fn user_at(user: &Option<String>) -> String {
    user.as_ref().map(|u| format!("{u}@")).unwrap_or_default()
}

/// Check a source and spell it out. Accepted: `owner/repo` (GitHub), `https://host/path`,
/// `ssh://[user@]host/path` and `user@host:path`. This is a spelling check, not a secret detector:
/// it refuses userinfo on HTTPS and segments that start like a known token, nothing cleverer. No
/// message quotes the input: a refused source may be a token.
pub fn parse_source(raw: &str) -> Result<Source, String> {
    if raw.is_empty() {
        return Err("the source is empty".into());
    }
    if raw.starts_with('-') {
        return Err("a source may not start with -".into());
    }
    if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("a source may not contain whitespace or control characters".into());
    }
    if raw.contains("::") {
        return Err("<transport>:: sources are refused".into());
    }
    if raw.contains(['?', '#']) {
        return Err("a source may not carry a query or fragment".into());
    }
    if raw.contains('\\') {
        return Err("a local path is not a repository source".into());
    }
    let lower = raw.to_ascii_lowercase();
    let source = if let Some(rest) = strip_scheme(raw, &lower, "https://") {
        let (authority, path) = rest.split_once('/').ok_or("an https:// URL needs a path")?;
        if authority.contains('@') {
            return Err("an https:// URL may not carry a user or password".into());
        }
        Source {
            transport: Transport::Https,
            host: host(authority)?,
            path: url_path(path)?,
        }
    } else if let Some(rest) = strip_scheme(raw, &lower, "ssh://") {
        let (authority, path) = rest.split_once('/').ok_or("an ssh:// URL needs a path")?;
        let (user, host_part) = match authority.split_once('@') {
            Some((u, h)) => (Some(ssh_user(u)?), h),
            None => (None, authority),
        };
        Source {
            transport: Transport::Ssh { user, scp: false },
            host: host(host_part)?,
            path: url_path(path)?,
        }
    } else if lower.starts_with("file:") {
        return Err("file: sources are refused".into());
    } else if lower.starts_with("git://") {
        return Err("git:// sources are refused (unauthenticated and unencrypted)".into());
    } else if lower.starts_with("http://") {
        return Err("http:// sources are refused; use https://".into());
    } else if raw.contains("://") {
        return Err("only https:// and ssh:// URLs are accepted".into());
    } else if let Some((user, rest)) = raw
        .split_once('@')
        .filter(|(u, r)| !u.contains(['/', ':']) && r.contains(':'))
    {
        let (host_part, path) = rest.split_once(':').ok_or("user@host:path needs a path")?;
        Source {
            transport: Transport::Ssh {
                user: Some(ssh_user(user)?),
                scp: true,
            },
            host: host(host_part)?,
            path: url_path(path)?,
        }
    } else {
        let segments: Vec<&str> = raw.split('/').collect();
        match segments.as_slice() {
            [owner, repo]
                if !raw.contains([':', '@', '~'])
                    && !owner.starts_with('.')
                    && segment_ok(owner)
                    && segment_ok(repo) =>
            {
                Source {
                    transport: Transport::Https,
                    host: GITHUB.into(),
                    path: url_path(raw)?,
                }
            }
            _ => {
                return Err(
                    "not owner/repo, https://, ssh:// or user@host:path (a local path?)".into(),
                )
            }
        }
    };
    Ok(source)
}

fn strip_scheme<'a>(raw: &'a str, lower: &str, scheme: &str) -> Option<&'a str> {
    lower.starts_with(scheme).then(|| &raw[scheme.len()..])
}

fn segment_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '~'))
}

fn token_like(s: &str) -> bool {
    let s = s.to_ascii_lowercase();
    TOKEN_PREFIXES.iter().any(|p| s.starts_with(p))
}

/// `host[:port]`, lowercased.
fn host(raw: &str) -> Result<String, String> {
    let (name, port) = match raw.split_once(':') {
        Some((n, p)) => (n, Some(p)),
        None => (raw, None),
    };
    let label_ok = |l: &str| {
        !l.is_empty()
            && !l.starts_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    if name.is_empty() || !name.split('.').all(label_ok) {
        return Err("the host is not a host name".into());
    }
    if let Some(p) = port {
        if p.is_empty() || p.len() > 5 || !p.chars().all(|c| c.is_ascii_digit()) {
            return Err("the host has an invalid port".into());
        }
    }
    Ok(raw.to_ascii_lowercase())
}

fn ssh_user(user: &str) -> Result<String, String> {
    if user.contains(':') {
        return Err("an ssh source may not carry a password".into());
    }
    if !segment_ok(user) || user.starts_with('-') {
        return Err(
            "the ssh user name holds characters other than letters, digits, . _ ~ -".into(),
        );
    }
    if token_like(user) {
        return Err("the user name looks like a token".into());
    }
    Ok(user.to_string())
}

/// The path after the host: safe segments, no `.` / `..`, no token-looking segment; a trailing `/`
/// is dropped. A leading `/` (an absolute `user@host:/srv/repo`) is kept.
fn url_path(raw: &str) -> Result<String, String> {
    let path = raw.trim_end_matches('/');
    let body = path.strip_prefix('/').unwrap_or(path);
    if body.is_empty() {
        return Err("the source has no repository path".into());
    }
    for segment in body.split('/') {
        if segment == ".." || segment == "." {
            return Err("a source may not contain . or .. segments".into());
        }
        if token_like(segment) {
            return Err(
                "a path segment looks like a token; a token never goes in the source".into(),
            );
        }
        if !segment_ok(segment) {
            return Err(
                "a path segment holds characters other than letters, digits, . _ ~ -".into(),
            );
        }
    }
    Ok(path.to_string())
}

/// A branch or tag name: `[A-Za-z0-9._/-]{1,200}`, not starting with `-`, no `..`.
pub fn valid_ref(r: &str) -> bool {
    (1..=200).contains(&r.len())
        && !r.starts_with('-')
        && !r.contains("..")
        && r.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
}

/// The entry's id and cache folder name: a readable part (host, owner, repo; `[a-z0-9-]`, at most
/// 40 characters), `-`, and 8 hex characters of the sha256 of the normalised source and the ref.
pub fn repo_id(src: &Source, git_ref: Option<&str>) -> String {
    let key = src.key();
    let readable_from = key.split_once("://").map_or(key.as_str(), |(_, r)| r);
    let readable_from = readable_from
        .rsplit_once('@')
        .map_or(readable_from, |(_, r)| r);
    let mut readable = String::new();
    for c in readable_from.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            readable.push(c);
        } else if !readable.ends_with('-') {
            readable.push('-');
        }
    }
    let mut readable: String = readable.trim_matches('-').chars().take(40).collect();
    while readable.ends_with('-') {
        readable.pop();
    }
    if readable.is_empty() {
        readable.push_str("repo");
    }
    let digest = Sha256::digest(format!("{key}\n{}", git_ref.unwrap_or_default()).as_bytes());
    let hex: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    format!("{readable}-{hex}")
}

/// Is `name` shaped like an id (and so a cache folder this module owns)?
pub fn is_id(name: &str) -> bool {
    let Some((readable, hex)) = name.rsplit_once('-') else {
        return false;
    };
    name.len() <= 49
        && !readable.is_empty()
        && readable
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && hex.len() == 8
        && hex
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

// --- fetch methods -------------------------------------------------------------------------------

/// What a fetch may use from the host.
#[derive(Clone, Default)]
pub struct Env {
    pub github_token: Option<String>,
    pub gh_on_path: bool,
}

impl Env {
    pub fn from_host() -> Self {
        Self {
            github_token: std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty()),
            gh_on_path: on_path("gh"),
        }
    }
}

fn on_path(program: &str) -> bool {
    let names = if cfg!(windows) {
        vec![format!("{program}.exe"), program.to_string()]
    } else {
        vec![program.to_string()]
    };
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| names.iter().any(|n| dir.join(n).is_file()))
    })
}

/// Whether `m` can reach this source at all (the host's state aside): `gh` needs GitHub, `token`
/// a GitHub HTTPS source, `ssh` an SSH source or GitHub.
pub fn supports(src: &Source, m: Method) -> Result<(), String> {
    match m {
        Method::Auto | Method::Git => Ok(()),
        Method::Gh if src.github().is_none() => Err("gh works only for github.com sources".into()),
        Method::Token if src.github().is_none() || src.is_ssh() => {
            Err("token works only for https://github.com sources".into())
        }
        Method::Ssh if !src.is_ssh() && src.github().is_none() => {
            Err("ssh needs an SSH source (or a github.com one)".into())
        }
        _ => Ok(()),
    }
}

/// The methods a fetch tries, in order. `auto`: `ssh` for an SSH source, `token` for a GitHub
/// HTTPS source while `GITHUB_TOKEN` is set, `gh` for GitHub while `gh` is on PATH, then `git`.
pub fn attempts(src: &Source, via: Method, env: &Env) -> Vec<Method> {
    if via != Method::Auto {
        return vec![via];
    }
    let github = src.github().is_some();
    let mut out = Vec::new();
    if src.is_ssh() {
        out.push(Method::Ssh);
    }
    if github && !src.is_ssh() && env.github_token.is_some() {
        out.push(Method::Token);
    }
    if github && env.gh_on_path {
        out.push(Method::Gh);
    }
    out.push(Method::Git);
    out
}

/// One way to the remote: the method and what is handed to git (for a `gh` clone, `owner/repo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub method: Method,
    pub url: String,
}

/// The URL `m` uses for this source, or why it can't be used now.
pub fn route(src: &Source, m: Method, env: &Env) -> Result<Route, String> {
    supports(src, m)?;
    let url = match (m, src.github()) {
        (Method::Ssh, Some((owner, repo))) if !src.is_ssh() => {
            format!("git@{GITHUB}:{owner}/{repo}.git")
        }
        (Method::Token, _) if env.github_token.is_none() => {
            return Err("GITHUB_TOKEN is not set".into())
        }
        (Method::Gh, Some((owner, repo))) => format!("{owner}/{repo}"),
        _ => src.stored(),
    };
    Ok(Route { method: m, url })
}

/// One child process: its argv and what to set and unset in its environment.
pub struct GitCall {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
}

/// `git -c core.hooksPath=/dev/null [extra -c …] <args>`, with prompts off and no askpass.
fn git_call(config: &[&str], args: &[&str]) -> GitCall {
    let mut argv = vec!["git".to_string(), "-c".into(), HOOKS_OFF.into()];
    for c in config {
        argv.push("-c".into());
        argv.push((*c).into());
    }
    argv.extend(args.iter().map(|a| a.to_string()));
    GitCall {
        argv,
        env: vec![
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("GCM_INTERACTIVE".into(), "never".into()),
        ],
        env_remove: ["GIT_ASKPASS", "SSH_ASKPASS"]
            .iter()
            .chain(GIT_REPO_ENV)
            .map(|v| v.to_string())
            .collect(),
    }
}

const NO_CONFIG: &[&str] = &[];

/// gh as git's credential helper for one call, in place of the host's helpers.
const GH_HELPER: &[&str] = &[
    "credential.helper=",
    "credential.helper=!gh auth git-credential",
];

/// The token as an HTTP header for github.com, in the child's environment only — never in argv,
/// where `ps` shows it, and never in a file.
fn with_token(mut call: GitCall, env: &Env) -> GitCall {
    if let Some(token) = &env.github_token {
        let value = format!(
            "AUTHORIZATION: basic {}",
            base64(format!("x-access-token:{token}").as_bytes())
        );
        call.env.extend([
            ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
            (
                "GIT_CONFIG_KEY_0".to_string(),
                format!("http.https://{GITHUB}/.extraheader"),
            ),
            ("GIT_CONFIG_VALUE_0".to_string(), value),
        ]);
    }
    call
}

fn path_arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The first clone of a route into `dest`.
pub fn clone_call(route: &Route, git_ref: Option<&str>, dest: &Path, env: &Env) -> GitCall {
    let dest = path_arg(dest);
    if route.method == Method::Gh {
        // gh runs git itself and takes git's flags after its own `--`, so the repository and the
        // folder come before it (both checked: `owner/repo` and an absolute path). The hooks are
        // off for that git through its environment as well as through `--config`.
        let mut argv: Vec<String> = [
            "gh",
            "repo",
            "clone",
            route.url.as_str(),
            dest.as_str(),
            "--",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        argv.extend(["--depth", "1", "--config", HOOKS_OFF].map(String::from));
        if let Some(r) = git_ref {
            argv.extend(["--branch".to_string(), r.to_string()]);
        }
        let mut call = git_call(NO_CONFIG, &[]);
        call.argv = argv;
        call.env.extend([
            ("GH_PROMPT_DISABLED".to_string(), "1".to_string()),
            ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
            ("GIT_CONFIG_KEY_0".to_string(), "core.hooksPath".to_string()),
            ("GIT_CONFIG_VALUE_0".to_string(), "/dev/null".to_string()),
        ]);
        return call;
    }
    let mut args = vec!["clone", "--depth", "1"];
    if let Some(r) = git_ref {
        args.extend(["--branch", r]);
    }
    args.extend(["--", route.url.as_str(), dest.as_str()]);
    let call = git_call(NO_CONFIG, &args);
    if route.method == Method::Token {
        with_token(call, env)
    } else {
        call
    }
}

/// Bring an existing cache to the remote's current commit: fetch, check it out, drop the rest.
pub fn update_calls(route: &Route, git_ref: Option<&str>, dir: &Path, env: &Env) -> Vec<GitCall> {
    let dir = path_arg(dir);
    let target = git_ref.unwrap_or("HEAD");
    let (config, url) = match route.method {
        Method::Gh => (GH_HELPER, format!("https://{GITHUB}/{}.git", route.url)),
        _ => (NO_CONFIG, route.url.clone()),
    };
    let dir = dir.as_str();
    let fetch = git_call(
        config,
        &[
            "-C",
            dir,
            "fetch",
            "--depth",
            "1",
            "--",
            url.as_str(),
            target,
        ],
    );
    let fetch = if route.method == Method::Token {
        with_token(fetch, env)
    } else {
        fetch
    };
    vec![
        fetch,
        git_call(
            NO_CONFIG,
            &[
                "-C",
                dir,
                "checkout",
                "--force",
                "--detach",
                "FETCH_HEAD",
                "--",
            ],
        ),
        git_call(NO_CONFIG, &["-C", dir, "clean", "-ffdx", "--"]),
    ]
}

fn command(call: &GitCall) -> Command {
    let (program, args) = call.argv.split_first().expect("a call names its program");
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null());
    for v in &call.env_remove {
        cmd.env_remove(v);
    }
    for (k, v) in &call.env {
        cmd.env(k, v);
    }
    cmd
}

/// Run a network step, its stdout dropped (stdout is the CLI's payload) and its stderr passed on,
/// and kill it once [`TIMEOUT`] has passed. The message never quotes the argv or the environment.
fn run(call: &GitCall) -> Result<(), String> {
    let what = call.argv[0].clone();
    let mut child = command(call)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("{what}: {e}"))?;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{what} failed ({status})")),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{what} timed out after {} s", TIMEOUT.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(format!("{what}: {e}")),
        }
    }
}

/// A local git command in `dir` (never the network), hardened like every call here — hooks off,
/// no prompts, stdin null, no inherited repository variables — and with no token in its
/// environment (`GITHUB_TOKEN`, `GH_TOKEN` and any `GIT_CONFIG_*` header removed): what it reads
/// or writes may end up in a sandbox.
pub fn local_git(dir: &Path, args: &[&str]) -> Command {
    let dir_arg = path_arg(dir);
    let mut all = vec!["-C", dir_arg.as_str()];
    all.extend_from_slice(args);
    let mut cmd = command(&git_call(NO_CONFIG, &all));
    for v in [
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        cmd.env_remove(v);
    }
    if let Some(parent) = dir.parent() {
        // Never answer for a repository *around* the cache (a home folder kept in git).
        cmd.env("GIT_CEILING_DIRECTORIES", parent);
    }
    cmd
}

/// A local git read in `dir` (never the network): its trimmed stdout on success.
fn read_git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = local_git(dir, args).stderr(Stdio::null()).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// --- the cache -----------------------------------------------------------------------------------

/// The cached commit and its commit time, read from git in `dir`; `None` when git cannot read it
/// (no folder, a symlink, no `.git` folder of its own, a broken repository).
pub fn cached_commit(dir: &Path) -> Option<(String, u64)> {
    let real_dir = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    if !real_dir(dir) || !real_dir(&dir.join(".git")) {
        return None;
    }
    let commit = read_git(dir, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if commit.len() < 40 || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let at = read_git(dir, &["log", "-1", "--format=%ct"])?
        .parse()
        .ok()?;
    Some((commit, at))
}

/// Delete `<root>/<id>` — only that entry, only for an id-shaped name; a symlink there is removed
/// as a link, never followed. A missing folder is fine.
pub fn remove_cache(root: &Path, id: &str) -> Result<()> {
    if !is_id(id) {
        bail!("{id:?} is not a skill-repo id");
    }
    remove_entry(&root.join(id))
}

fn remove_entry(path: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    if meta.is_dir() {
        // `remove_dir_all` does not follow symlinks inside the tree.
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .with_context(|| format!("removing {}", path.display()))
}

/// Remove id-shaped cache folders no entry owns (left by an `rm` that stopped half-way), and hand
/// back their names. Anything else in the cache root — lock files, temp folders, the owner's own
/// files — stays.
pub fn prune_orphans(root: &Path, keep: &[String]) -> Vec<String> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_id(&name) && !keep.contains(&name) && remove_cache(root, &name).is_ok() {
            removed.push(name);
        }
    }
    removed.sort();
    removed
}

/// Check that the cache's `.git/config` names the stored source as `origin` and holds no userinfo,
/// no `extraheader` and not the token's value.
pub fn check_cache(dir: &Path, stored: &str, env: &Env) -> Result<(), String> {
    let text = fs::read_to_string(dir.join(".git").join("config"))
        .map_err(|e| format!("reading the cache's .git/config: {e}"))?;
    if text.to_ascii_lowercase().contains("extraheader") {
        return Err("the cache's .git/config holds an extraheader".into());
    }
    if env
        .github_token
        .as_deref()
        .is_some_and(|t| !t.is_empty() && text.contains(t))
    {
        return Err("the cache's .git/config holds the token".into());
    }
    if text.lines().any(has_https_userinfo) {
        return Err("the cache's .git/config holds a URL with a user or password".into());
    }
    match read_git(dir, &["config", "--get", "remote.origin.url"]) {
        Some(url) if url == stored => Ok(()),
        _ => Err("the cache's origin is not the configured source".into()),
    }
}

fn has_https_userinfo(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    ["https://", "http://"].iter().any(|scheme| {
        lower.match_indices(scheme).any(|(at, _)| {
            let rest = &lower[at + scheme.len()..];
            let authority = rest.split(['/', '"', ' ']).next().unwrap_or_default();
            authority.contains('@')
        })
    })
}

/// A successful fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub commit: String,
    pub method: Method,
    /// The attempts that failed before this one.
    pub failures: Vec<String>,
}

/// Mirror one entry into `<root>/<id>`, trying its methods in order.
pub fn fetch(repo: &Repo, root: &Path, env: &Env) -> Result<Fetched, Vec<String>> {
    let src = repo.parsed().map_err(|e| vec![e])?;
    let id = repo_id(&src, repo.git_ref.as_deref());
    let routes = attempts(&src, repo.via, env)
        .into_iter()
        .map(|m| (m, route(&src, m, env)))
        .collect();
    fetch_routes(
        &id,
        &repo.source,
        repo.git_ref.as_deref(),
        routes,
        root,
        env,
    )
}

/// [`fetch`] with the routes given — the seam the tests use to fetch from a local bare repo.
/// Under an exclusive lock on `<root>/<id>.lock`: no cache → clone into `<id>.tmp-<pid>`, check,
/// rename; a cache → fetch, check out, clean, check. A failed update leaves the previous commit,
/// a failed clone no `<id>` folder; a cache that fails [`check_cache`] is deleted.
pub fn fetch_routes(
    id: &str,
    stored: &str,
    git_ref: Option<&str>,
    routes: Vec<(Method, Result<Route, String>)>,
    root: &Path,
    env: &Env,
) -> Result<Fetched, Vec<String>> {
    let fail = |e: anyhow::Error| vec![format!("{e:#}")];
    fs::create_dir_all(root)
        .with_context(|| format!("creating {}", root.display()))
        .map_err(fail)?;
    let _lock = lock_file(&root.join(format!("{id}.lock"))).map_err(fail)?;
    let dir = root.join(id);
    let mut cached = cached_commit(&dir).is_some();
    if !cached {
        // Whatever sits at that name is no cache git can read; a clone replaces it.
        remove_cache(root, id).map_err(fail)?;
    }
    let mut failures = Vec::new();
    for (method, routed) in routes {
        let route = match routed {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{}: {e}", method.as_str()));
                continue;
            }
        };
        let done = if cached {
            update_calls(&route, git_ref, &dir, env)
                .iter()
                .try_for_each(run)
                .and_then(|()| finish(&dir, stored, env))
        } else {
            let tmp = root.join(format!("{id}.tmp-{}", std::process::id()));
            let _ = remove_entry(&tmp);
            let done = run(&clone_call(&route, git_ref, &tmp, env))
                .and_then(|()| finish(&tmp, stored, env))
                .and_then(|()| {
                    fs::rename(&tmp, &dir).map_err(|e| format!("moving the clone in place: {e}"))
                });
            if done.is_err() {
                let _ = remove_entry(&tmp);
            }
            done
        };
        match done {
            Ok(()) => match cached_commit(&dir) {
                Some((commit, _)) => {
                    return Ok(Fetched {
                        commit,
                        method,
                        failures,
                    })
                }
                None => failures.push(format!("{}: git cannot read the cache", method.as_str())),
            },
            Err(e) => {
                failures.push(format!("{}: {e}", method.as_str()));
                if cached && cached_commit(&dir).is_none() {
                    // The check deleted it (or the update broke it): the next route clones.
                    let _ = remove_cache(root, id);
                    cached = false;
                }
            }
        }
    }
    Err(failures)
}

/// Point `origin` at the stored source, then run [`check_cache`]; a cache that fails it is deleted.
fn finish(dir: &Path, stored: &str, env: &Env) -> Result<(), String> {
    let dir_arg = path_arg(dir);
    let set = run(&git_call(
        NO_CONFIG,
        &[
            "-C",
            dir_arg.as_str(),
            "config",
            "--",
            "remote.origin.url",
            stored,
        ],
    ))
    .and_then(|()| check_cache(dir, stored, env));
    if set.is_err() {
        let _ = remove_entry(dir);
    }
    set
}

// --- ls ------------------------------------------------------------------------------------------

/// One listed entry with what git says about its cache.
pub struct Listed {
    pub id: String,
    pub repo: Repo,
    pub cache: Option<(PathBuf, String, u64)>,
}

pub fn list(state: &Path) -> Result<Vec<Listed>> {
    let config = load(&config_path(state))?;
    let root = cache_root(state);
    Ok(config
        .repos
        .into_iter()
        .filter_map(|repo| {
            let id = repo.id().ok()?;
            let dir = root.join(&id);
            let cache = cached_commit(&dir).map(|(commit, at)| (dir, commit, at));
            Some(Listed { id, repo, cache })
        })
        .collect())
}

pub fn list_json(listed: &[Listed], auto_update: bool) -> serde_json::Value {
    let repos: Vec<serde_json::Value> = listed
        .iter()
        .map(|l| {
            serde_json::json!({
                "id": l.id,
                "source": l.repo.source,
                "ref": l.repo.git_ref,
                "via": l.repo.via.as_str(),
                "cache": l.cache.as_ref().map(|(path, commit, at)| serde_json::json!({
                    "path": path.to_string_lossy(),
                    "commit": commit,
                    "committed_at": at,
                })),
            })
        })
        .collect();
    serde_json::json!({ "auto_update": auto_update, "repos": repos })
}

pub fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

/// Standard base64 with padding — the one encoding the token header needs.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aisbx-skill-repos-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake token, built at run time so no credential-shaped literal sits in the source.
    fn fake_token() -> String {
        format!("{}-{}", "fake", "token-value-0123456789")
    }

    #[test]
    fn the_four_source_forms_are_accepted_and_spelled_out() {
        for (raw, stored) in [
            ("anthropics/skills", "https://github.com/anthropics/skills"),
            (
                "https://GitLab.Example.com/team/skills.git/",
                "https://gitlab.example.com/team/skills.git",
            ),
            (
                "ssh://git@Example.com:2222/team/skills.git",
                "ssh://git@example.com:2222/team/skills.git",
            ),
            ("git@github.com:o/r.git", "git@github.com:o/r.git"),
        ] {
            assert_eq!(
                parse_source(raw).map(|s| s.stored()),
                Ok(stored.to_string()),
                "{raw}"
            );
        }
    }

    #[test]
    fn every_fixed_refusal_case_is_refused() {
        let userinfo_token = format!("https://x-access-token:{}@github.com/o/r", fake_token());
        let cases: Vec<(&str, String)> = vec![
            ("leading dash", "-uupload-pack=touch/x".into()),
            ("whitespace", "o/r x".into()),
            ("tab", "o/r\t".into()),
            ("control character", "o/r\u{7}".into()),
            ("file://", "file:///tmp/repo".into()),
            ("file: without slashes", "file:repo".into()),
            ("git://", "git://github.com/o/r".into()),
            ("http://", "http://github.com/o/r".into()),
            ("ext::", "ext::sh".into()),
            ("other <x>::", "fd::17".into()),
            ("other scheme", "ftp://example.com/o/r".into()),
            ("absolute local path", "/tmp/repo".into()),
            ("relative local path", "./repo".into()),
            ("parent local path", "../repo".into()),
            ("home path", "~/repo".into()),
            ("windows path", "C:\\repo".into()),
            ("bare name", "repo".into()),
            ("three segments", "a/b/c".into()),
            ("host:path without user", "example.com:o/r".into()),
            (".. segment", "https://github.com/o/../r".into()),
            (". segment", "https://github.com/o/./r".into()),
            (".. in scp path", "git@example.com:o/../r".into()),
            ("https user", "https://user@github.com/o/r".into()),
            (
                "https user and password",
                "https://user:pw@github.com/o/r".into(),
            ),
            ("https token userinfo", userinfo_token),
            ("ssh password", "ssh://user:pw@example.com/o/r".into()),
            ("query", "https://github.com/o/r?x=1".into()),
            ("fragment", "https://github.com/o/r#main".into()),
            ("empty", String::new()),
        ];
        for (what, raw) in &cases {
            assert!(parse_source(raw).is_err(), "{what}: {raw:?} was accepted");
        }
        for prefix in TOKEN_PREFIXES {
            let segment = format!("{prefix}abc123");
            for raw in [
                format!("{segment}/r"),
                format!("o/{segment}"),
                format!("https://github.com/{segment}/r"),
                format!("{segment}@example.com:o/r"),
            ] {
                assert!(
                    parse_source(&raw).is_err(),
                    "token-like {raw:?} was accepted"
                );
            }
        }
    }

    #[test]
    fn refs_are_branch_or_tag_names() {
        for good in ["main", "release/1.2", "v1.0.0", "feat_x-y"] {
            assert!(valid_ref(good), "{good}");
        }
        let long = "x".repeat(201);
        for bad in ["", "-x", "a..b", "a b", "a;b", "a~1", "a:b", long.as_str()] {
            assert!(!valid_ref(bad), "{bad:?}");
        }
        assert!(valid_ref(&"x".repeat(200)));
    }

    fn id_of(raw: &str, git_ref: Option<&str>) -> String {
        repo_id(&parse_source(raw).unwrap(), git_ref)
    }

    fn id_shaped(id: &str) -> bool {
        (1..=49).contains(&id.len())
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    }

    #[test]
    fn spellings_of_one_repo_share_an_id_and_a_ref_changes_it() {
        let id = id_of("o/r", None);
        assert_eq!(id_of("https://github.com/O/R.git/", None), id);
        assert_eq!(id_of("https://github.com/o/r", None), id);
        assert!(id.starts_with("github-com-o-r-"), "{id}");
        assert_ne!(id_of("o/r", Some("v1")), id);
        assert_ne!(id_of("o/other", None), id);
        let long = format!("https://example.com/{}/{}", "a".repeat(60), "b".repeat(60));
        for id in [
            id.clone(),
            id_of("o/r", Some("v1")),
            id_of(&long, None),
            id_of("git@example.com:/srv/Team_Skills.git", Some("main")),
        ] {
            assert!(id_shaped(&id), "{id}");
            assert!(is_id(&id), "{id}");
        }
        assert!(!is_id("notes"));
        assert!(!is_id(&format!("{id}.lock")));
        assert!(!is_id(&format!("{id}.tmp-42")));
        assert!(!is_id("../x-0123abcd"));
    }

    #[test]
    fn the_list_round_trips_and_keeps_one_entry_per_source_and_ref() {
        let state = scratch("config");
        let id = add(&state, "o/r", None, Method::Auto).unwrap();
        assert_eq!(
            add(&state, "https://github.com/O/R.git", None, Method::Ssh).unwrap(),
            id
        );
        let config = load(&config_path(&state)).unwrap();
        assert_eq!(config.repos.len(), 1);
        assert_eq!(config.repos[0].source, "https://github.com/o/r");
        assert_eq!(config.repos[0].via, Method::Ssh, "a new --via is taken");
        let tagged = add(&state, "o/r", Some("v1"), Method::Auto).unwrap();
        assert_ne!(tagged, id);

        let text = fs::read_to_string(config_path(&state)).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["repos"][1]["ref"], "v1");
        assert_eq!(doc["repos"][1]["via"], "auto");

        let listed = list(&state).unwrap();
        let json = list_json(&listed, true);
        assert_eq!(json["repos"][0]["id"], id.as_str());
        assert_eq!(json["repos"][0]["ref"], serde_json::Value::Null);
        assert_eq!(json["repos"][0]["cache"], serde_json::Value::Null);
        assert_eq!(json["repos"][1]["ref"], "v1");

        assert!(remove(&state, "o/r").is_err(), "one source, two refs");
        assert_eq!(remove(&state, &tagged).unwrap(), Some(tagged));
        assert_eq!(remove(&state, "o/r").unwrap(), Some(id));
        assert_eq!(remove(&state, "o/r").unwrap(), None);
        assert!(list(&state).unwrap().is_empty());
    }

    #[test]
    fn auto_update_round_trips_and_an_absent_key_means_on() {
        let state = scratch("auto-update");
        let path = config_path(&state);
        assert!(load(&path).unwrap().auto_update(), "no file: on");
        let id = add(&state, "o/r", None, Method::Auto).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("auto_update"),
            "not written until set: {text}"
        );
        assert!(load(&path).unwrap().auto_update());

        set_auto_update(&state, false).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["auto_update"], false);
        assert_eq!(doc["version"], 1);
        let config = load(&path).unwrap();
        assert!(!config.auto_update());
        assert_eq!(config.repos[0].id().unwrap(), id, "the repos are kept");

        set_auto_update(&state, true).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["auto_update"], true);
        assert!(load(&path).unwrap().auto_update());

        // The previous build's shape, without the key.
        fs::write(
            &path,
            r#"{"version": 1, "repos": [{"source": "https://github.com/o/r", "via": "auto"}]}"#,
        )
        .unwrap();
        let old = load(&path).unwrap();
        assert!(old.auto_update());
        assert_eq!(old.repos.len(), 1);
        assert_eq!(
            list_json(&list(&state).unwrap(), false)["auto_update"],
            false
        );
        let _ = fs::remove_dir_all(&state);
    }

    #[test]
    fn a_list_this_build_cannot_read_is_reported_and_left_alone() {
        for (tag, body) in [
            ("corrupt", "{not json".to_string()),
            ("v2", r#"{"version": 2, "repos": []}"#.to_string()),
            (
                "bad-entry",
                r#"{"version": 1, "repos": [{"source": "file:///tmp/r"}]}"#.to_string(),
            ),
        ] {
            let state = scratch(&format!("unreadable-{tag}"));
            let path = config_path(&state);
            fs::write(&path, &body).unwrap();
            let err = load(&path).unwrap_err();
            assert!(format!("{err:#}").contains(CONFIG_FILE), "{tag}: {err:#}");
            assert!(add(&state, "o/r", None, Method::Auto).is_err(), "{tag}");
            assert!(remove(&state, "o/r").is_err(), "{tag}");
            assert!(list(&state).is_err(), "{tag}");
            assert_eq!(fs::read_to_string(&path).unwrap(), body, "{tag}");
        }
        let missing = scratch("missing");
        assert!(load(&config_path(&missing)).unwrap().repos.is_empty());
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (input, out) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), out);
        }
    }

    fn env_value<'a>(call: &'a GitCall, key: &str) -> Option<&'a str> {
        call.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn all_calls(src: &Source, m: Method, env: &Env) -> (Route, Vec<GitCall>) {
        let r = route(src, m, env).unwrap();
        let mut calls = vec![clone_call(&r, Some("main"), Path::new("/tmp/c"), env)];
        calls.extend(update_calls(&r, Some("main"), Path::new("/tmp/c"), env));
        (r, calls)
    }

    #[test]
    fn the_token_reaches_git_only_through_the_child_env() {
        let token = fake_token();
        let encoded = base64(format!("x-access-token:{token}").as_bytes());
        let env = Env {
            github_token: Some(token.clone()),
            gh_on_path: true,
        };
        let src = parse_source("o/r").unwrap();
        for m in [Method::Token, Method::Gh, Method::Ssh, Method::Git] {
            let (_, calls) = all_calls(&src, m, &env);
            for call in &calls {
                assert!(
                    !call
                        .argv
                        .iter()
                        .any(|a| a.contains(&token) || a.contains(&encoded)),
                    "{m:?}: token in argv"
                );
                let header = env_value(call, "GIT_CONFIG_VALUE_0");
                if m == Method::Token && call.argv.iter().any(|a| a == "clone" || a == "fetch") {
                    assert_eq!(
                        header,
                        Some(format!("AUTHORIZATION: basic {encoded}").as_str())
                    );
                    assert_eq!(
                        env_value(call, "GIT_CONFIG_KEY_0"),
                        Some("http.https://github.com/.extraheader")
                    );
                } else {
                    assert!(
                        !header.is_some_and(|h| h.contains(&encoded)),
                        "{m:?}: token outside the token method"
                    );
                }
            }
        }
    }

    #[test]
    fn every_call_has_hooks_off_no_prompts_and_dashdash_before_the_url() {
        let env = Env {
            github_token: Some(fake_token()),
            gh_on_path: true,
        };
        for (raw, methods) in [
            (
                "o/r",
                vec![Method::Token, Method::Gh, Method::Ssh, Method::Git],
            ),
            ("git@example.com:o/r.git", vec![Method::Ssh, Method::Git]),
            ("https://example.com/o/r", vec![Method::Git]),
        ] {
            let src = parse_source(raw).unwrap();
            for m in methods {
                let (r, calls) = all_calls(&src, m, &env);
                for call in &calls {
                    assert!(call.argv.iter().any(|a| a == HOOKS_OFF), "{raw} {m:?}");
                    assert_eq!(env_value(call, "GIT_TERMINAL_PROMPT"), Some("0"));
                    assert_eq!(env_value(call, "GCM_INTERACTIVE"), Some("never"));
                    for gone in ["GIT_ASKPASS", "SSH_ASKPASS"] {
                        assert!(call.env_remove.iter().any(|v| v == gone));
                    }
                    let dashdash = call.argv.iter().position(|a| a == "--");
                    assert!(dashdash.is_some(), "{raw} {m:?}: {:?}", call.argv);
                    if call.argv[0] == "gh" {
                        // gh's own `--` introduces git's flags; the repo and folder come before it.
                        assert_eq!(call.argv[3], r.url);
                        assert_eq!(env_value(call, "GIT_CONFIG_VALUE_0"), Some("/dev/null"));
                        continue;
                    }
                    assert_eq!(call.argv[0], "git");
                    if let Some(url_at) = call
                        .argv
                        .iter()
                        .position(|a| a.contains("://") || a.contains('@') || *a == r.url)
                    {
                        assert!(dashdash < Some(url_at), "{raw} {m:?}: {:?}", call.argv);
                    }
                }
            }
        }
        let src = parse_source("o/r").unwrap();
        let (_, calls) = all_calls(&src, Method::Gh, &env);
        assert!(calls[1]
            .argv
            .windows(2)
            .any(|w| w == ["-c", "credential.helper=!gh auth git-credential"]));
        assert!(calls[1]
            .argv
            .contains(&"https://github.com/o/r.git".to_string()));
    }

    #[test]
    fn auto_tries_the_methods_in_order() {
        let none = Env::default();
        let both = Env {
            github_token: Some(fake_token()),
            gh_on_path: true,
        };
        let only_gh = Env {
            github_token: None,
            gh_on_path: true,
        };
        use Method::*;
        for (raw, env, want) in [
            ("o/r", &both, vec![Token, Gh, Git]),
            ("o/r", &only_gh, vec![Gh, Git]),
            ("o/r", &none, vec![Git]),
            ("git@github.com:o/r.git", &both, vec![Ssh, Gh, Git]),
            ("git@github.com:o/r.git", &none, vec![Ssh, Git]),
            ("https://example.com/o/r", &both, vec![Git]),
            ("ssh://git@example.com/o/r", &both, vec![Ssh, Git]),
        ] {
            let src = parse_source(raw).unwrap();
            assert_eq!(attempts(&src, Auto, env), want, "{raw}");
        }
        let src = parse_source("o/r").unwrap();
        assert_eq!(attempts(&src, Gh, &none), vec![Gh]);
        let err = route(&src, Token, &none).unwrap_err();
        assert!(err.contains("GITHUB_TOKEN"), "{err}");
        assert_eq!(
            route(&src, Ssh, &none).unwrap().url,
            "git@github.com:o/r.git"
        );
        let other = parse_source("https://example.com/o/r").unwrap();
        for m in [Gh, Token, Ssh] {
            assert!(supports(&other, m).is_err(), "{m:?}");
        }
        assert!(supports(&parse_source("git@github.com:o/r").unwrap(), Token).is_err());
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// Plain git for building the fixture, away from the host's own configuration.
    fn sh_git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(work: &Path, name: &str) -> String {
        fs::write(work.join(name), name).unwrap();
        sh_git(work, &["add", name]);
        sh_git(work, &["commit", "-qm", name]);
        sh_git(work, &["push", "-q", "origin", "HEAD:main"]);
        sh_git(work, &["rev-parse", "HEAD"])
    }

    fn listed_commit(state: &Path) -> Option<String> {
        let doc = list_json(&list(state).unwrap(), true);
        doc["repos"][0]["cache"]["commit"]
            .as_str()
            .map(str::to_string)
    }

    #[test]
    fn fetch_mirrors_a_local_bare_repo_and_guards_the_cache() {
        if !git_available() {
            eprintln!("skipped: no git on PATH");
            return;
        }
        let base = scratch("fetch");
        sh_git(&base, &["init", "-q", "--bare", "up.git"]);
        sh_git(&base, &["init", "-q", "work"]);
        let up = base.join("up.git");
        let work = base.join("work");
        let up_path = up.to_string_lossy().into_owned();
        sh_git(&work, &["remote", "add", "origin", up_path.as_str()]);
        let first = commit(&work, "one");

        let state = base.join("state");
        let stored = "https://example.com/team/skills";
        let id = add(&state, stored, None, Method::Git).unwrap();
        let root = cache_root(&state);
        let good = || {
            vec![(
                Method::Git,
                Ok(Route {
                    method: Method::Git,
                    url: format!("file://{}", up.display()),
                }),
            )]
        };
        let bad = || {
            vec![(
                Method::Git,
                Ok(Route {
                    method: Method::Git,
                    url: format!("file://{}", base.join("missing.git").display()),
                }),
            )]
        };
        let env = Env::default();

        assert_eq!(listed_commit(&state), None, "not cached yet");
        let fetched = fetch_routes(&id, stored, None, good(), &root, &env).unwrap();
        assert_eq!(fetched.commit, first);
        assert_eq!(listed_commit(&state), Some(first.clone()));
        assert_eq!(
            sh_git(&root.join(&id), &["config", "--get", "remote.origin.url"]),
            stored
        );

        let second = commit(&work, "two");
        fetch_routes(&id, stored, None, good(), &root, &env).unwrap();
        assert_eq!(listed_commit(&state), Some(second.clone()));
        assert!(root.join(&id).join("two").is_file());

        assert!(fetch_routes(&id, stored, None, bad(), &root, &env).is_err());
        assert_eq!(
            listed_commit(&state),
            Some(second),
            "a failed fetch keeps the cache"
        );

        let other = add(&state, "https://example.com/team/other", None, Method::Git).unwrap();
        assert!(fetch_routes(&other, stored, None, bad(), &root, &env).is_err());
        let left: Vec<String> = fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&other) && !n.ends_with(".lock"))
            .collect();
        assert!(left.is_empty(), "a failed clone left {left:?}");

        sh_git(&root.join(&id), &["config", "http.extraheader", "x"]);
        assert!(fetch_routes(&id, stored, None, good(), &root, &env).is_err());
        assert!(!root.join(&id).exists(), "an extraheader deletes the cache");

        let with_token = Env {
            github_token: Some(fake_token()),
            gh_on_path: false,
        };
        fetch_routes(&id, stored, None, good(), &root, &with_token).unwrap();
        let planted = fake_token();
        sh_git(&root.join(&id), &["config", "aisbx.note", planted.as_str()]);
        assert!(fetch_routes(&id, stored, None, good(), &root, &with_token).is_err());
        assert!(
            !root.join(&id).exists(),
            "the token's value deletes the cache"
        );

        fs::create_dir_all(root.join(&id)).unwrap();
        assert_eq!(
            listed_commit(&state),
            None,
            "a folder git cannot read is not cached"
        );
        fetch_routes(&id, stored, None, good(), &root, &env).unwrap();
        assert!(listed_commit(&state).is_some(), "a clone replaces it");
    }

    #[test]
    fn rm_deletes_only_its_own_folder_and_never_follows_a_link() {
        let state = scratch("rm");
        let outside = scratch("rm-outside");
        fs::write(outside.join("keep"), "x").unwrap();
        let root = cache_root(&state);
        let id = add(&state, "o/r", None, Method::Auto).unwrap();
        fs::create_dir_all(root.join(&id).join("sub")).unwrap();
        fs::create_dir_all(root.join("keep-me")).unwrap();
        assert_eq!(remove(&state, &id).unwrap(), Some(id.clone()));
        assert!(!root.join(&id).exists());
        assert!(root.join("keep-me").is_dir());

        #[cfg(unix)]
        {
            let linked = add(&state, "o/linked", None, Method::Auto).unwrap();
            std::os::unix::fs::symlink(&outside, root.join(&linked)).unwrap();
            assert_eq!(remove(&state, "o/linked").unwrap(), Some(linked.clone()));
            assert!(fs::symlink_metadata(root.join(&linked)).is_err());
            assert!(
                outside.join("keep").is_file(),
                "the link's target is untouched"
            );
        }

        assert!(remove_cache(&root, "keep-me").is_err());
        assert!(remove_cache(&root, "../rm-outside").is_err());
        assert!(outside.join("keep").is_file());
    }

    #[test]
    fn orphan_pruning_spares_everything_that_is_not_an_id() {
        let root = scratch("prune");
        let kept = id_of("o/kept", None);
        let orphan = id_of("o/gone", None);
        for dir in [
            kept.clone(),
            orphan.clone(),
            "notes".to_string(),
            format!("{kept}.tmp-1"),
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join(format!("{orphan}.lock")), "").unwrap();
        assert_eq!(
            prune_orphans(&root, std::slice::from_ref(&kept)),
            vec![orphan.clone()]
        );
        assert!(!root.join(&orphan).exists());
        assert!(root.join(&kept).is_dir());
        assert!(root.join("notes").is_dir());
        assert!(root.join(format!("{kept}.tmp-1")).is_dir());
        assert!(root.join(format!("{orphan}.lock")).is_file());
    }
}
