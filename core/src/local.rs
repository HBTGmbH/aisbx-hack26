//! Inference engines on this computer: which ones answer on which port, what they serve, and the
//! model list a local connection offers ([`crate::litellm::Kind::Local`]). Everything here talks
//! to the host's own loopback over plain HTTP, with short timeouts, and runs on the host: a
//! sandbox reaches an engine only through the port its connection's kit allows.

use std::io::{Read, Write};
use std::net::{Ipv6Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::litellm::Connection;
use crate::models::{Limits, Model};

/// An inference engine aisbx recognizes, by its process or by its answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// llama.cpp's `llama-server`.
    LlamaServer,
    Ollama,
    LmStudio,
    /// vLLM, vLLM Metal included.
    Vllm,
    VllmMlx,
    /// `mlx_lm.server` of mlx-lm.
    MlxLm,
    Omlx,
    RapidMlx,
    MlxServe,
    Vmlx,
    Mtplx,
    /// A server that answers OpenAI's `/v1/models` and that aisbx does not know.
    Other,
}

impl Engine {
    pub const ALL: [Engine; 12] = [
        Engine::LlamaServer,
        Engine::Ollama,
        Engine::LmStudio,
        Engine::Vllm,
        Engine::VllmMlx,
        Engine::MlxLm,
        Engine::Omlx,
        Engine::RapidMlx,
        Engine::MlxServe,
        Engine::Vmlx,
        Engine::Mtplx,
        Engine::Other,
    ];

    /// The engine's name in `connection.json` and in a connection's default name.
    pub fn slug(self) -> &'static str {
        match self {
            Engine::LlamaServer => "llama-server",
            Engine::Ollama => "ollama",
            Engine::LmStudio => "lm-studio",
            Engine::Vllm => "vllm",
            Engine::VllmMlx => "vllm-mlx",
            Engine::MlxLm => "mlx-lm",
            Engine::Omlx => "omlx",
            Engine::RapidMlx => "rapid-mlx",
            Engine::MlxServe => "mlx-serve",
            Engine::Vmlx => "vmlx",
            Engine::Mtplx => "mtplx",
            Engine::Other => "openai",
        }
    }

    pub fn parse(slug: &str) -> Option<Engine> {
        Engine::ALL.into_iter().find(|e| e.slug() == slug)
    }

    /// The engine's name as its makers write it.
    pub fn label(self) -> &'static str {
        match self {
            Engine::LlamaServer => "llama-server",
            Engine::Ollama => "Ollama",
            Engine::LmStudio => "LM Studio",
            Engine::Vllm => "vLLM",
            Engine::VllmMlx => "vllm-mlx",
            Engine::MlxLm => "mlx-lm",
            Engine::Omlx => "oMLX",
            Engine::RapidMlx => "Rapid-MLX",
            Engine::MlxServe => "mlx-serve",
            Engine::Vmlx => "vMLX",
            Engine::Mtplx => "MTPLX",
            Engine::Other => "OpenAI-compatible server",
        }
    }

    /// Does the engine serve Anthropic's Messages API (`/v1/messages`), which Claude Code needs?
    /// `None` for an engine aisbx does not know.
    pub fn anthropic(self) -> Option<bool> {
        match self {
            Engine::MlxLm => Some(false),
            Engine::Other => None,
            _ => Some(true),
        }
    }

    /// The port the engine listens on when nothing else is set.
    fn default_port(self) -> Option<u16> {
        match self {
            Engine::LlamaServer | Engine::MlxLm => Some(8080),
            Engine::Ollama => Some(11434),
            Engine::LmStudio => Some(1234),
            Engine::Vllm => Some(8000),
            Engine::MlxServe => Some(11234),
            _ => None,
        }
    }

    /// The engine a command line runs: its program, or the script or Python module after it.
    fn of_command(args: &[&str]) -> Option<Engine> {
        let base = |s: &str| s.rsplit(['/', '\\']).next().unwrap_or(s).to_string();
        let words: Vec<String> = args.iter().take(4).map(|a| base(a)).collect();
        let has = |w: &str| words.iter().any(|x| x == w);
        let then = |first: &str, second: &[&str]| {
            words
                .windows(2)
                .any(|p| p[0] == first && second.contains(&p[1].as_str()))
        };
        let found = match () {
            _ if has("llama-server") || then("llama", &["serve", "server"]) => Engine::LlamaServer,
            _ if then("ollama", &["serve"]) => Engine::Ollama,
            _ if has("vllm-mlx") => Engine::VllmMlx,
            _ if then("vllm", &["serve"]) || has("vllm.entrypoints.openai.api_server") => {
                Engine::Vllm
            }
            _ if has("mlx_lm.server") || then("mlx_lm", &["server"]) => Engine::MlxLm,
            _ if then("omlx", &["serve"]) => Engine::Omlx,
            _ if then("rapid-mlx", &["serve"]) => Engine::RapidMlx,
            _ if has("mlx-serve") => Engine::MlxServe,
            _ if has("vmlx") => Engine::Vmlx,
            _ if has("mtplx") => Engine::Mtplx,
            _ => return None,
        };
        Some(found)
    }
}

/// The ports a discovery always asks, beside those the engine processes name.
pub const DEFAULT_PORTS: [u16; 5] = [8080, 11434, 1234, 8000, 11234];

// --- talking to an engine ----------------------------------------------------------------------

const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
const READ_TIMEOUT: Duration = Duration::from_secs(3);
/// The most of one answer that is read: a model list is a few kB.
const MAX_REPLY: usize = 4 << 20;

/// An engine's answer.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    /// The `Server` header: llama-server names itself `llama.cpp` there.
    pub server: Option<String>,
    pub body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Option<Value> {
        (self.status == 200)
            .then(|| serde_json::from_slice(&self.body).ok())
            .flatten()
    }

    /// A refusal of the key as engines give it: 401 or 403 with a JSON object, never a web
    /// page.
    fn refuses_key(&self) -> bool {
        matches!(self.status, 401 | 403)
            && serde_json::from_slice::<Value>(&self.body).is_ok_and(|v| v.is_object())
    }

    /// The engine its `Server` header names.
    fn engine(&self) -> Option<Engine> {
        (self.server.as_deref())
            .is_some_and(|s| s.starts_with("llama.cpp"))
            .then_some(Engine::LlamaServer)
    }
}

fn connect(port: u16) -> std::io::Result<TcpStream> {
    let v4 = SocketAddr::from(([127, 0, 0, 1], port));
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&v4, CONNECT_TIMEOUT)
        .or_else(|_| TcpStream::connect_timeout(&v6, CONNECT_TIMEOUT))
}

/// One HTTP/1.1 request to `localhost:<port>`, the key as a bearer where there is one.
pub fn request(
    port: u16,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: Option<&[u8]>,
) -> std::io::Result<Reply> {
    let mut stream = connect(port)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost:{port}\r\nUser-Agent: aisbx\r\n\
         Accept: application/json\r\nConnection: close\r\n"
    );
    if let Some(key) = key {
        head.push_str(&format!("Authorization: Bearer {key}\r\n"));
    }
    if let Some(body) = body {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if let Some(body) = body {
        stream.write_all(body)?;
    }
    let started = Instant::now();
    let mut raw = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    while raw.len() <= MAX_REPLY && started.elapsed() < READ_TIMEOUT * 2 {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(e) if raw.is_empty() => return Err(e),
            Err(_) => break,
        }
        if parse_reply(&raw).is_some_and(|(_, complete)| complete) {
            break;
        }
    }
    parse_reply(&raw)
        .map(|(reply, _)| reply)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no HTTP answer"))
}

fn get(port: u16, path: &str, key: Option<&str>) -> Option<Reply> {
    request(port, "GET", path, key, None).ok()
}

/// An HTTP answer out of the bytes read so far, and whether it is complete: its whole body by
/// `Content-Length` or chunked framing. `None` before the status line and headers are in.
fn parse_reply(raw: &[u8]) -> Option<(Reply, bool)> {
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..end]).ok()?;
    let rest = &raw[end + 4..];
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let mut length = None;
    let mut chunked = false;
    let mut server = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse::<usize>().ok(),
            "transfer-encoding" => chunked = value.to_ascii_lowercase().contains("chunked"),
            "server" => server = Some(value.trim().to_string()),
            _ => {}
        }
    }
    let (body, complete) = match (chunked, length) {
        (true, _) => dechunk(rest),
        (false, Some(n)) => (rest[..n.min(rest.len())].to_vec(), rest.len() >= n),
        (false, None) => (rest.to_vec(), false),
    };
    Some((
        Reply {
            status,
            server,
            body,
        },
        complete,
    ))
}

/// A chunked body as far as it is in, and whether its last chunk is.
fn dechunk(mut rest: &[u8]) -> (Vec<u8>, bool) {
    let mut body = Vec::new();
    loop {
        let Some(eol) = rest.windows(2).position(|w| w == b"\r\n") else {
            return (body, false);
        };
        let size = std::str::from_utf8(&rest[..eol])
            .ok()
            .map(|s| s.split(';').next().unwrap_or("").trim())
            .and_then(|s| usize::from_str_radix(s, 16).ok());
        let Some(size) = size else {
            return (body, false);
        };
        if size == 0 {
            return (body, true);
        }
        let data = &rest[eol + 2..];
        if data.len() < size {
            body.extend_from_slice(data);
            return (body, false);
        }
        body.extend_from_slice(&data[..size]);
        rest = data.get(size + 2..).unwrap_or_default();
    }
}

// --- engine processes ----------------------------------------------------------------------

/// An engine process on this computer, from its command line.
#[derive(Clone, PartialEq, Eq)]
pub struct Proc {
    pub engine: Engine,
    /// `--port`, else the engine's default port.
    pub port: Option<u16>,
    /// `--api-key`, as given.
    key: Option<String>,
    /// `--api-key-file`, as given.
    key_file: Option<String>,
}

/// Redacts the key, so a stray `{:?}` cannot print it.
impl std::fmt::Debug for Proc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proc")
            .field("engine", &self.engine)
            .field("port", &self.port)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("key_file", &self.key_file)
            .finish()
    }
}

impl Proc {
    /// The key the engine was started with, and where it came from: the first line of its key
    /// file that is neither empty nor a `#` comment, else `--api-key`.
    fn key(&self, read: &dyn Fn(&str) -> Option<String>) -> Option<(String, String)> {
        if let Some(file) = &self.key_file {
            let text = read(file)?;
            let key = text
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with('#'))?;
            return Some((key.to_string(), format!("its key file {file}")));
        }
        let key = self.key.clone()?;
        Some((key, "its --api-key".into()))
    }
}

/// The engine processes in `ps -axww -o args=` output, one command line per line. A path with
/// spaces after `--api-key-file` is put back together from the words that name a file
/// (`exists`).
pub fn scan(ps: &str, exists: &dyn Fn(&str) -> bool) -> Vec<Proc> {
    let mut out = Vec::new();
    for line in ps.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some(engine) = Engine::of_command(&words) else {
            continue;
        };
        let mut port = None;
        let mut key = None;
        let mut key_file = None;
        let mut i = 0;
        while i < words.len() {
            let (flag, inline) = match words[i].split_once('=') {
                Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
                _ => (words[i], None),
            };
            let next = || words.get(i + 1).map(|w| w.to_string());
            match flag {
                "--port" => port = inline.or_else(next).and_then(|p| p.parse::<u16>().ok()),
                "--api-key" => key = inline.or_else(next),
                "--api-key-file" => {
                    key_file = inline.or_else(|| {
                        let mut path = String::new();
                        for w in &words[i + 1..] {
                            if !path.is_empty() {
                                path.push(' ');
                            }
                            path.push_str(w);
                            if exists(&path) {
                                return Some(path);
                            }
                        }
                        next()
                    })
                }
                _ => {}
            }
            i += 1;
        }
        let port = port.or_else(|| engine.default_port());
        let proc = Proc {
            engine,
            port,
            key,
            key_file,
        };
        if !out.contains(&proc) {
            out.push(proc);
        }
    }
    out
}

/// The command lines of this computer's processes. Empty on Windows, and where `ps` fails.
fn ps_output() -> String {
    if cfg!(windows) {
        return String::new();
    }
    std::process::Command::new("ps")
        .args(["-axww", "-o", "args="])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn read_key_file(path: &str) -> Option<String> {
    let path = match path.strip_prefix("~/") {
        Some(rest) => crate::model::home_dir()?.join(rest),
        None => std::path::PathBuf::from(path),
    };
    std::fs::read_to_string(path).ok()
}

// --- discovery -----------------------------------------------------------------------------

/// An engine that answered on a port of this computer.
#[derive(Clone, PartialEq)]
pub struct Found {
    pub engine: Engine,
    pub port: u16,
    /// The key aisbx read from the engine's own setup, for storing it with sbx. Never shown.
    pub key: Option<String>,
    /// Where that key came from, to say so: `its key file /path/api-keys`.
    pub key_source: Option<String>,
    /// The engine refused aisbx's request for its models: it wants a key aisbx could not read.
    pub needs_key: bool,
    pub models: Vec<Model>,
}

/// Redacts the key, so a stray `{:?}` cannot print it.
impl std::fmt::Debug for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Found")
            .field("engine", &self.engine)
            .field("port", &self.port)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("key_source", &self.key_source)
            .field("needs_key", &self.needs_key)
            .field("models", &self.models)
            .finish()
    }
}

impl Found {
    /// The URL of a connection to it.
    pub fn url(&self) -> String {
        format!("{}{}", crate::litellm::LOCAL_URL, self.port)
    }

    /// The name a new connection to it gets: the engine's, or with its port where that is taken.
    pub fn default_name(&self, taken: &dyn Fn(&str) -> bool) -> String {
        let plain = match self.engine {
            Engine::Other => "local".to_string(),
            e => e.slug().to_string(),
        };
        [plain.clone(), format!("{plain}-{}", self.port)]
            .into_iter()
            .chain((2..100).map(|n| format!("{plain}-{}-{n}", self.port)))
            .find(|n| !taken(n) && crate::litellm::valid_name(n))
            .unwrap_or(plain)
    }

    /// One line on what was found: `llama-server on localhost:8095 · 1 model · 32k context`.
    pub fn summary(&self) -> String {
        let mut parts = vec![format!(
            "{} on localhost:{}",
            self.engine.label(),
            self.port
        )];
        match (self.needs_key, self.models.len()) {
            (true, _) => parts.push("it wants a key".into()),
            (false, 1) => parts.push("1 model".into()),
            (false, n) => parts.push(format!("{n} models")),
        }
        if let Some(context) = self.models.iter().filter_map(|m| m.limits.context).min() {
            parts.push(format!("{} context", tokens_short(context)));
        }
        parts.join(" · ")
    }
}

/// A context window in a few characters: `32k` for 32768, as engines count it, `40k` for 40000.
pub(crate) fn tokens_short(n: u64) -> String {
    match n {
        1_048_576.. if n.is_multiple_of(1_048_576) => format!("{}M", n / 1_048_576),
        1_024.. if n.is_multiple_of(1_024) => format!("{}k", n / 1_024),
        1_000.. => format!("{}k", (n as f64 / 1e3).round()),
        _ => n.to_string(),
    }
}

/// The engines that answer on this computer: on the ports the engine processes name, then on
/// [`DEFAULT_PORTS`]. None under `cargo test`, which must not depend on the host.
pub fn discover() -> Vec<Found> {
    if cfg!(any(test, feature = "test-support")) {
        return Vec::new();
    }
    let procs = scan(&ps_output(), &|p| Path::new(p).is_file());
    discover_with(&procs, &DEFAULT_PORTS, &read_key_file)
}

/// [`discover`] for these processes and ports, with `read` reading a key file.
pub fn discover_with(
    procs: &[Proc],
    ports: &[u16],
    read: &dyn Fn(&str) -> Option<String>,
) -> Vec<Found> {
    let mut asked: Vec<u16> = Vec::new();
    let mut out = Vec::new();
    let candidates = (procs.iter())
        .filter_map(|p| Some((p.port?, Some(p))))
        .chain(ports.iter().map(|port| (*port, None)));
    for (port, proc) in candidates {
        if asked.contains(&port) {
            continue;
        }
        asked.push(port);
        if let Some(found) = probe(port, proc, read) {
            out.push(found);
        }
    }
    out
}

/// What answers on `port`, if an OpenAI-compatible server does.
fn probe(port: u16, proc: Option<&Proc>, read: &dyn Fn(&str) -> Option<String>) -> Option<Found> {
    let key = proc.and_then(|p| p.key(read));
    let reply = get(port, "/v1/models", key.as_ref().map(|(k, _)| k.as_str()))?;
    let engine_of_proc = proc.map(|p| p.engine).or_else(|| reply.engine());
    let (key, key_source) = match key {
        Some((k, source)) => (Some(k), Some(source)),
        None => (None, None),
    };
    if reply.refuses_key() {
        return Some(Found {
            engine: engine_of_proc.unwrap_or(Engine::Other),
            port,
            key: None,
            key_source: None,
            needs_key: true,
            models: Vec::new(),
        });
    }
    let list = reply.json()?;
    list.get("data")?.as_array()?;
    let engine = engine_of_proc.unwrap_or_else(|| identify(port, &list, key.as_deref()));
    Some(Found {
        engine,
        port,
        models: read_models(port, engine, &list, key.as_deref()),
        key,
        key_source,
        needs_key: false,
    })
}

/// The engine behind an OpenAI model list: its `owned_by`, else what else it answers.
fn identify(port: u16, list: &Value, key: Option<&str>) -> Engine {
    let listed = entries(list);
    let owners: Vec<&str> = (listed.iter())
        .filter_map(|m| m.get("owned_by")?.as_str())
        .collect();
    if owners.contains(&"llamacpp") {
        return Engine::LlamaServer;
    }
    if owners.contains(&"vllm") {
        return Engine::Vllm;
    }
    let answers = |path: &str, field: &str| {
        get(port, path, key)
            .and_then(|r| r.json())
            .is_some_and(|v| v.get(field).is_some())
    };
    match () {
        _ if answers("/api/version", "version") => Engine::Ollama,
        _ if answers("/api/v0/models", "data") => Engine::LmStudio,
        _ => Engine::Other,
    }
}

fn entries(list: &Value) -> Vec<Value> {
    (list.get("data").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default()
}

/// The first positive whole number under one of `keys` (a `.` steps into an object).
fn first_count(entry: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        let mut v = entry;
        for part in key.split('.') {
            v = v.get(part)?;
        }
        v.as_u64().filter(|n| *n > 0)
    })
}

/// The chat models an engine lists, with the context window it serves where it says it.
fn read_models(port: u16, engine: Engine, list: &Value, key: Option<&str>) -> Vec<Model> {
    let mut models: Vec<Model> = Vec::new();
    for entry in entries(list) {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !crate::models::valid_id(id) || models.iter().any(|m| m.id == id) {
            continue;
        }
        let vision = (entry.pointer("/architecture/input_modalities"))
            .and_then(Value::as_array)
            .map(|m| m.iter().any(|x| x == "image"));
        models.push(Model {
            id: id.to_string(),
            name: id.to_string(),
            limits: Limits {
                context: first_count(
                    &entry,
                    &[
                        "meta.n_ctx",
                        "max_model_len",
                        "loaded_context_length",
                        "context_length",
                        "max_context_length",
                        "context_window",
                    ],
                ),
                vision,
                ..Limits::default()
            },
            provider: None,
            upstream: None,
        });
    }
    match engine {
        Engine::LmStudio => lm_studio(port, key, &mut models),
        Engine::Ollama => ollama(port, key, &mut models),
        _ => {}
    }
    models
}

/// LM Studio's own list says which models are chat models and the context each one is loaded
/// with, or the most it takes.
fn lm_studio(port: u16, key: Option<&str>, models: &mut Vec<Model>) {
    let Some(v0) = get(port, "/api/v0/models", key).and_then(|r| r.json()) else {
        return;
    };
    let info = entries(&v0);
    models.retain(|m| {
        let entry = info
            .iter()
            .find(|e| e.get("id").and_then(Value::as_str) == Some(&m.id));
        entry.and_then(|e| e.get("type")?.as_str()) != Some("embeddings")
    });
    for m in models.iter_mut() {
        let entry = info
            .iter()
            .find(|e| e.get("id").and_then(Value::as_str) == Some(&m.id));
        if let Some(e) = entry {
            m.limits.context = (m.limits.context)
                .or_else(|| first_count(e, &["loaded_context_length", "max_context_length"]));
        }
    }
}

/// Ollama lists every pulled model: only those whose capabilities name completion stay, with the
/// context window of those it has loaded.
fn ollama(port: u16, key: Option<&str>, models: &mut Vec<Model>) {
    models.retain(|m| {
        let body = serde_json::json!({ "model": m.id }).to_string();
        let shown = request(port, "POST", "/api/show", key, Some(body.as_bytes()))
            .ok()
            .and_then(|r| r.json());
        let caps = shown
            .as_ref()
            .and_then(|s| s.get("capabilities")?.as_array().cloned());
        caps.is_none_or(|c| c.iter().any(|x| x == "completion"))
    });
    let Some(ps) = get(port, "/api/ps", key).and_then(|r| r.json()) else {
        return;
    };
    let loaded = (ps.get("models").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();
    for m in models.iter_mut() {
        let entry = loaded.iter().find(|e| {
            ["model", "name"]
                .iter()
                .any(|k| e.get(*k).and_then(Value::as_str) == Some(&m.id))
        });
        if let Some(e) = entry {
            m.limits.context = m
                .limits
                .context
                .or_else(|| first_count(e, &["context_length"]));
        }
    }
}

// --- a connection's model list -------------------------------------------------------------

/// Where a local connection's key comes from for a fetch on the host: given, or read from the
/// setup of the engine process that listens on its port.
fn key_for(port: u16, given: Option<&str>) -> Option<String> {
    if let Some(key) = given.filter(|k| !k.trim().is_empty()) {
        return Some(key.trim().to_string());
    }
    if cfg!(any(test, feature = "test-support")) {
        return None;
    }
    let procs = scan(&ps_output(), &|p| Path::new(p).is_file());
    (procs.iter())
        .filter(|p| p.port == Some(port))
        .find_map(|p| p.key(&read_key_file))
        .map(|(key, _)| key)
}

/// The models a local connection's engine serves now, asked on the host. `key`: the engine's
/// key where the caller has it; else aisbx reads it from the engine's process, if it can.
pub fn fetch(c: &Connection, key: Option<&str>) -> Result<Vec<Model>> {
    let Some(port) = crate::litellm::port(&c.url) else {
        bail!("{} is not a local engine", c.name);
    };
    let key = key_for(port, key);
    fetch_at(
        port,
        c.engine.as_deref().and_then(Engine::parse),
        key.as_deref(),
    )
}

/// [`fetch`] from the engine on `port`.
pub fn fetch_at(port: u16, engine: Option<Engine>, key: Option<&str>) -> Result<Vec<Model>> {
    let reply = request(port, "GET", "/v1/models", key, None)
        .with_context(|| format!("nothing answers on localhost:{port} — is the engine running?"))?;
    match reply.status {
        200 => {}
        _ if reply.refuses_key() => bail!(
            "the engine on localhost:{port} wants a key that aisbx could not read: give it with \
             --key-stdin"
        ),
        status => bail!("the engine on localhost:{port} answered {status} for /v1/models"),
    }
    let list: Value = serde_json::from_slice(&reply.body)
        .with_context(|| format!("localhost:{port} answered no JSON for /v1/models"))?;
    if list.get("data").and_then(Value::as_array).is_none() {
        bail!("localhost:{port} answered no OpenAI model list for /v1/models");
    }
    let engine = engine.unwrap_or_else(|| identify(port, &list, key));
    let models = read_models(port, engine, &list, key);
    if models.is_empty() {
        bail!(
            "{} on localhost:{port} serves no model yet — load one, then read the list again",
            engine.label()
        );
    }
    Ok(models)
}

/// The dashboard's step that reads a local connection's models: this program's own
/// `local fetch NAME`, the key on stdin where the dashboard has one (`--key-stdin`).
pub fn fetch_step(c: &Connection, key: Option<&str>) -> crate::job::Step {
    let mut argv = crate::headless::job_argv(&["local", "fetch", &c.name]);
    let label = format!(
        "read the models of {} on {}",
        c.name,
        crate::litellm::place(&c.url)
    );
    match key.map(str::trim).filter(|k| !k.is_empty()) {
        Some(key) => {
            argv.push("--key-stdin".into());
            crate::job::Step::with_stdin(label, argv, key.to_string())
        }
        None => crate::job::Step::new(label, argv),
    }
}

/// A fetched list in the form of a fetcher's cache ([`crate::models::parse_sandbox_cache`]), so a
/// background fetch on the host takes the dashboard's one way of keeping a list.
pub fn cache_text(models: &[Model], now: i64) -> String {
    let models: Vec<Value> = (models.iter())
        .map(|m| {
            let mut entry =
                serde_json::to_value(m.limits).unwrap_or_else(|_| serde_json::json!({}));
            entry["id"] = m.id.clone().into();
            entry["name"] = m.name.clone().into();
            entry
        })
        .collect();
    serde_json::json!({ "version": 1, "fetched_at": now, "models": models }).to_string()
}

/// Keep a fetched list as the connection's model list ([`crate::litellm::harvest_file`]),
/// labelled with its engine. `Ok(false)`: the stored list is as new.
pub fn store(root: &Path, c: &Connection, models: Vec<Model>, now: i64) -> Result<bool> {
    let label = (c.engine.as_deref().and_then(Engine::parse)).map_or("local engine", Engine::label);
    let harvest = crate::models::Harvest {
        fetched_at: now,
        base_url: String::new(),
        models,
    };
    crate::models::store_harvest(
        &root.join(&c.name).join("models.json"),
        label,
        &harvest,
        now,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A fake engine on a free port of 127.0.0.1: every request gets the answer `route` gives
    /// its method, path and bearer; the requests are kept.
    fn serve(
        route: impl Fn(&str, &str, Option<&str>) -> (u16, String) + Send + Sync + 'static,
    ) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // Read the whole body: an unread one resets the connection before the answer.
                let complete = |buf: &[u8]| {
                    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        return false;
                    };
                    let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                    let length = (head.lines())
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    buf.len() >= end + 4 + length
                };
                while !complete(&buf) {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let first = text.lines().next().unwrap_or_default().to_string();
                let mut words = first.split_whitespace();
                let (method, path) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
                let bearer = (text.lines())
                    .find_map(|l| l.strip_prefix("Authorization: Bearer "))
                    .map(str::trim);
                log.lock().unwrap().push(format!("{method} {path}"));
                let (status, body) = route(method, path, bearer);
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: \
                     {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        (port, seen)
    }

    fn not_found() -> (u16, String) {
        (404, "{}".into())
    }

    /// llama-server's answer, as measured from b12110 (2026-11-08), trimmed.
    const LLAMA_MODELS: &str = r#"{"models":[{"name":"k2-horizon-7b","model":"k2-horizon-7b"}],
        "object":"list","data":[{"id":"k2-horizon-7b","object":"model","owned_by":"llamacpp",
        "architecture":{"input_modalities":["text"],"output_modalities":["text"]},
        "meta":{"n_ctx":32768,"n_ctx_train":524288,"n_params":8999178240}}]}"#;

    #[test]
    fn a_key_protected_llama_server_is_found_by_its_process_and_key_file() {
        let (port, seen) = serve(|method, path, bearer| match (method, path, bearer) {
            ("GET", "/v1/models", Some("k-1")) => (200, LLAMA_MODELS.into()),
            ("GET", "/v1/models", _) => (401, r#"{"error":"Invalid API Key"}"#.into()),
            _ => not_found(),
        });
        let ps = format!(
            "/sbin/launchd\n/opt/homebrew/bin/llama-server --model /Users/x/Library/Application \
             Support/m.gguf --alias k2 --api-key-file /Users/x/Key Dir/api-keys --host 127.0.0.1 \
             --port {port}\n"
        );
        let procs = scan(&ps, &|p| p == "/Users/x/Key Dir/api-keys");
        assert_eq!(procs.len(), 1, "{procs:?}");
        assert_eq!(procs[0].engine, Engine::LlamaServer);
        assert_eq!(
            procs[0].key_file.as_deref(),
            Some("/Users/x/Key Dir/api-keys")
        );
        let read =
            |p: &str| (p == "/Users/x/Key Dir/api-keys").then(|| "# a comment\nk-1\n".into());
        let found = discover_with(&procs, &[], &read);
        assert_eq!(found.len(), 1, "{found:?}");
        let f = &found[0];
        assert_eq!(
            (f.engine, f.port, f.needs_key),
            (Engine::LlamaServer, port, false)
        );
        assert_eq!(f.key.as_deref(), Some("k-1"));
        assert_eq!(
            f.key_source.as_deref(),
            Some("its key file /Users/x/Key Dir/api-keys")
        );
        assert_eq!(f.models.len(), 1);
        assert_eq!(f.models[0].id, "k2-horizon-7b");
        assert_eq!(f.models[0].limits.context, Some(32768));
        assert_eq!(f.models[0].limits.vision, Some(false));
        assert_eq!(
            f.summary(),
            format!("llama-server on localhost:{port} · 1 model · 32k context")
        );
        assert!(!format!("{f:?}").contains("k-1"), "the key never prints");
        assert_eq!(seen.lock().unwrap().as_slice(), ["GET /v1/models"]);
    }

    /// A web server's refusal is a page, not an engine's JSON; llama-server names itself.
    #[test]
    fn only_a_json_refusal_is_an_engine_wanting_a_key() {
        let (page, _) = serve(|_, _, _| (403, "<!DOCTYPE HTML><html></html>".into()));
        assert!(discover_with(&[], &[page], &|_| None).is_empty());
        let raw = b"HTTP/1.1 401 Unauthorized\r\nServer: llama.cpp\r\nContent-Type: \
                    application/json\r\nContent-Length: 2\r\n\r\n{}";
        let (reply, complete) = parse_reply(raw).unwrap();
        assert!(complete && reply.refuses_key());
        assert_eq!(reply.engine(), Some(Engine::LlamaServer));
        let html =
            b"HTTP/1.0 403 Forbidden\r\nServer: SimpleHTTP/0.6\r\nContent-Length: 6\r\n\r\n<html>";
        let (reply, _) = parse_reply(html).unwrap();
        assert!(!reply.refuses_key());
        assert_eq!(reply.engine(), None);
    }

    #[test]
    fn without_its_key_an_engine_is_found_wanting_one() {
        let (port, _) = serve(|_, _, _| (401, "{}".into()));
        let found = discover_with(&[], &[port], &|_| None);
        assert_eq!(found.len(), 1);
        assert!(found[0].needs_key && found[0].models.is_empty() && found[0].key.is_none());
        assert_eq!(found[0].engine, Engine::Other);
        assert!(
            found[0].summary().ends_with("it wants a key"),
            "{}",
            found[0].summary()
        );
    }

    #[test]
    fn ollama_is_told_by_its_version_and_keeps_only_completion_models() {
        let (port, _) = serve(|method, path, _| match (method, path) {
            ("GET", "/v1/models") => (
                200,
                r#"{"object":"list","data":[{"id":"qwen3.5:9b","owned_by":"library"},
                    {"id":"nomic-embed-text:latest","owned_by":"library"}]}"#
                    .into(),
            ),
            ("GET", "/api/version") => (200, r#"{"version":"0.40.15"}"#.into()),
            ("POST", "/api/show") => (200, r#"{"capabilities":["completion","tools"]}"#.into()),
            ("GET", "/api/ps") => (
                200,
                r#"{"models":[{"name":"qwen3.5:9b","model":"qwen3.5:9b","context_length":32768}]}"#
                    .into(),
            ),
            _ => not_found(),
        });
        let found = discover_with(&[], &[port], &|_| None);
        assert_eq!(found[0].engine, Engine::Ollama);
        let ids: Vec<&str> = found[0].models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["qwen3.5:9b", "nomic-embed-text:latest"]);
        assert_eq!(found[0].models[0].limits.context, Some(32768));
        assert_eq!(found[0].models[1].limits.context, None);
    }

    #[test]
    fn ollama_drops_a_model_whose_capabilities_name_no_completion() {
        let (port, _) = serve(|method, path, _| match (method, path) {
            ("GET", "/v1/models") => (200, r#"{"data":[{"id":"chat"},{"id":"embed"}]}"#.into()),
            ("GET", "/api/version") => (200, r#"{"version":"0.40.15"}"#.into()),
            ("POST", "/api/show") => (200, r#"{"capabilities":["embedding"]}"#.into()),
            _ => not_found(),
        });
        let models = fetch_at(port, Some(Engine::Ollama), None);
        assert!(
            models
                .unwrap_err()
                .to_string()
                .contains("serves no model yet"),
            "an embedding model is no chat model"
        );
    }

    #[test]
    fn lm_studio_gives_the_loaded_context_and_drops_embeddings() {
        let (port, _) = serve(|method, path, _| {
            match (method, path) {
            ("GET", "/v1/models") => (
                200,
                r#"{"data":[{"id":"qwen/qwen3.5-9b","owned_by":"organization_owner"},
                    {"id":"text-embedding-nomic","owned_by":"organization_owner"}]}"#
                    .into(),
            ),
            ("GET", "/api/v0/models") => (
                200,
                r#"{"data":[{"id":"qwen/qwen3.5-9b","type":"llm","max_context_length":262144,
                    "loaded_context_length":32768},{"id":"text-embedding-nomic","type":"embeddings"}]}"#
                    .into(),
            ),
            _ => not_found(),
        }
        });
        let found = discover_with(&[], &[port], &|_| None);
        assert_eq!(found[0].engine, Engine::LmStudio);
        assert_eq!(found[0].models.len(), 1);
        assert_eq!(found[0].models[0].limits.context, Some(32768));
    }

    #[test]
    fn vllm_says_its_window_in_the_model_list() {
        let (port, _) = serve(|_, path, _| match path {
            "/v1/models" => (
                200,
                r#"{"data":[{"id":"Qwen/Qwen3.5-9B","owned_by":"vllm","max_model_len":40960}]}"#
                    .into(),
            ),
            _ => not_found(),
        });
        let models = fetch_at(port, None, None).unwrap();
        assert_eq!(models[0].limits.context, Some(40960));
        let found = discover_with(&[], &[port], &|_| None);
        assert_eq!(found[0].engine, Engine::Vllm);
    }

    #[test]
    fn a_server_that_is_no_model_list_is_not_found() {
        let (port, _) = serve(|_, _, _| (200, r#"{"hello":"world"}"#.into()));
        assert!(discover_with(&[], &[port], &|_| None).is_empty());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let closed = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(discover_with(&[], &[closed], &|_| None).is_empty());
        let err = fetch_at(closed, None, None).unwrap_err().to_string();
        assert!(err.contains("is the engine running"), "{err}");
    }

    #[test]
    fn a_fetch_names_what_the_engine_refused() {
        let (port, _) = serve(|_, _, bearer| match bearer {
            Some("good") => (200, LLAMA_MODELS.into()),
            _ => (401, "{}".into()),
        });
        let err = fetch_at(port, None, None).unwrap_err().to_string();
        assert!(err.contains("wants a key"), "{err}");
        let models = fetch_at(port, Some(Engine::LlamaServer), Some("good")).unwrap();
        assert_eq!(models[0].id, "k2-horizon-7b");
        let mut conn = Connection::new("k2", format!("http://localhost:{port}"));
        conn.engine = Some("llama-server".into());
        let models = fetch(&conn, Some(" good \n")).unwrap();
        assert_eq!(models.len(), 1, "a key read from stdin is trimmed");
        let gateway = Connection::new("acme", "https://litellm.example.com");
        assert!(fetch(&gateway, None).is_err());
    }

    #[test]
    fn a_chunked_answer_is_read_whole() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        let (reply, complete) = parse_reply(raw).unwrap();
        assert!(complete);
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, b"{\"a\":1}");
        let (_, partial) = parse_reply(&raw[..raw.len() - 5]).unwrap();
        assert!(!partial);
        assert!(parse_reply(b"HTTP/1.1 200 OK\r\nContent-Le").is_none());
    }

    #[test]
    fn engine_processes_are_told_by_their_command_lines() {
        let ps = "\
/usr/bin/python3.12 -m mlx_lm.server --model m --port 8081
/Users/x/venv/bin/python3.12 /Users/x/venv/bin/vllm serve Qwen/Qwen3.5-9B --api-key s3cret
/Applications/Ollama.app/Contents/Resources/ollama serve
/opt/homebrew/bin/llama-server -m x.gguf --port=9001 --api-key=k
/Users/x/bin/rapid-mlx serve /m --port 8002
/usr/bin/vim notes.txt --port 1
";
        let procs = scan(ps, &|_| false);
        let got: Vec<(Engine, Option<u16>)> = procs.iter().map(|p| (p.engine, p.port)).collect();
        assert_eq!(
            got,
            [
                (Engine::MlxLm, Some(8081)),
                (Engine::Vllm, Some(8000)),
                (Engine::Ollama, Some(11434)),
                (Engine::LlamaServer, Some(9001)),
                (Engine::RapidMlx, Some(8002)),
            ]
        );
        let no_file = |_: &str| None;
        assert_eq!(
            procs[1].key(&no_file),
            Some(("s3cret".to_string(), "its --api-key".to_string()))
        );
        assert_eq!(procs[3].key(&no_file).unwrap().0, "k");
        assert_eq!(procs[0].key(&no_file), None);
        assert!(!format!("{procs:?}").contains("s3cret"));
    }

    #[test]
    fn every_engine_round_trips_through_its_slug() {
        for e in Engine::ALL {
            assert_eq!(Engine::parse(e.slug()), Some(e));
            assert!(crate::litellm::valid_name(e.slug()), "{e:?}");
        }
        assert_eq!(Engine::parse("gpt"), None);
        assert_eq!(Engine::MlxLm.anthropic(), Some(false));
        assert_eq!(Engine::LlamaServer.anthropic(), Some(true));
        assert_eq!(Engine::Other.anthropic(), None);
    }

    #[test]
    fn a_window_is_said_as_engines_count_it() {
        assert_eq!(tokens_short(32_768), "32k");
        assert_eq!(tokens_short(131_072), "128k");
        assert_eq!(tokens_short(40_000), "40k");
        assert_eq!(tokens_short(1_048_576), "1M");
        assert_eq!(tokens_short(800), "800");
    }

    #[test]
    fn a_found_engine_gets_a_free_name() {
        let found = Found {
            engine: Engine::LlamaServer,
            port: 8095,
            key: None,
            key_source: None,
            needs_key: false,
            models: Vec::new(),
        };
        assert_eq!(found.default_name(&|_| false), "llama-server");
        assert_eq!(
            found.default_name(&|n| n == "llama-server"),
            "llama-server-8095"
        );
        let other = Found {
            engine: Engine::Other,
            ..found.clone()
        };
        assert_eq!(other.default_name(&|_| false), "local");
        assert_eq!(found.url(), "http://localhost:8095");
    }

    #[test]
    fn a_fetched_list_is_kept_as_the_connections_list() {
        let root = std::env::temp_dir().join(format!(
            "aisbx-local-store-{}-{}",
            std::process::id(),
            crate::state::now_epoch()
        ));
        let mut conn = Connection::new("k2", "http://localhost:8095");
        conn.engine = Some("llama-server".into());
        let model = Model {
            id: "k2-horizon-7b".into(),
            name: "k2-horizon-7b".into(),
            limits: Limits {
                context: Some(32768),
                ..Limits::default()
            },
            ..Model::default()
        };
        assert!(store(&root, &conn, vec![model.clone()], 1_800_000_000).unwrap());
        assert!(
            !store(&root, &conn, vec![model], 1_800_000_000).unwrap(),
            "not newer"
        );
        let (models, _) = crate::models::catalog_for(Some(&root.join("k2/models.json")));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].limits.context, Some(32768));
        let _ = std::fs::remove_dir_all(&root);
    }
}
