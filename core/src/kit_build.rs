//! `aisbx kit build`: the kit images a step spec needs, built outside any step, so that a step
//! with `kits_prebuilt` starts from them or fails at once (docs/step-protocol.md, *Kit images*).
//!
//! sbx builds one image per kit folder (`sbx-kit-src:<kit>-<hash of its content>`) and, at
//! create, stacks them into the sandbox's image. The build is the slow and network-bound part;
//! `sbx kit validate` does it outside a sandbox. sbx does not say before a create whether it will
//! build, so aisbx keeps its own key: a digest of the backend, the kit names and every file of
//! the kit folders the create would pass, and the sbx version. `kit build` records, per key, the
//! images sbx built for it; a step looks the key up and checks that the images are still there.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::headless::{self, Args, Fail, Outcome};
use crate::kits::{self, Catalog};
use crate::step::{now_rfc3339, sha256_hex};
use crate::{litellm, sbx, state, tmpfs};

/// The backend `kit build` and `doctor --kit` take without `--backend`.
pub const DEFAULT_BACKEND: &str = "claude-anthropic";

/// The kit folders a step's create passes to sbx for a backend and its addon kits.
#[derive(Debug, Clone)]
pub struct KitSet {
    pub backend: String,
    /// The addon kits, sorted, each once: the key does not depend on the spec's order.
    pub kits: Vec<String>,
    /// Backend first, then the addons in catalog order, as `aisbx up` passes them; each the RAM
    /// copy a step's `--tmpfs-volumes` makes where the kit declares a volume.
    pub dirs: Vec<PathBuf>,
}

impl KitSet {
    /// The backend and the kits, `claude-anthropic, rtk`: what the error and doctor name.
    pub fn label(&self) -> String {
        std::iter::once(self.backend.as_str())
            .chain(self.kits.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The command that builds this set.
    pub fn build_command(&self) -> String {
        let mut words = vec![format!("aisbx kit build --backend {}", self.backend)];
        words.extend(self.kits.iter().map(|k| format!("--kit {k}")));
        words.join(" ")
    }
}

/// The kit set a step with `backend` and `kits` creates from. Writes the RAM copies, as the
/// create would. A backend on a LiteLLM or local connection, or on a setup token, is refused:
/// its generated kits are made per sandbox, so no build ahead of the step covers them.
pub fn resolve(catalog: &Catalog, backend: &str, kits: &[String]) -> Result<KitSet, String> {
    let found = catalog
        .backend(backend)
        .ok_or_else(|| format!("no backend kit {backend:?}"))?;
    if litellm::kind_of_backend(&found.kit.name).is_some()
        || found.account() == Some(kits::Account::Token)
    {
        return Err(format!(
            "{backend} gets kits generated per sandbox (a connection or a setup token); its kit images cannot be built ahead"
        ));
    }
    let wanted: BTreeSet<String> = kits.iter().cloned().collect();
    for kit in &wanted {
        if catalog.addon(kit).is_none() {
            return Err(format!("no addon kit {kit:?}"));
        }
    }
    let mut dirs = vec![found.kit.dir.clone()];
    dirs.extend(
        catalog
            .all_addons()
            .filter(|a| wanted.contains(&a.kit.name))
            .map(|a| a.kit.dir.clone()),
    );
    let dirs = tmpfs::copies(&dirs).map_err(|e| format!("{e:#}"))?;
    Ok(KitSet {
        backend: backend.to_string(),
        kits: wanted.into_iter().collect(),
        dirs,
    })
}

/// The image key: 16 hex digits of a digest over the backend, the sorted kit names, every file
/// (path, executable bit, content) of each kit folder, and the sbx version. It changes with any
/// kit file, and not with the order of the kits.
pub fn key(set: &KitSet, sbx_version: &str) -> Result<String> {
    let mut folders: Vec<(String, Vec<serde_json::Value>)> = Vec::new();
    for dir in &set.dirs {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut files = Vec::new();
        walk(dir, dir, &mut files)?;
        folders.push((name, files));
    }
    folders.sort_by(|a, b| a.0.cmp(&b.0));
    let doc = serde_json::json!({
        "backend": set.backend,
        "kits": set.kits,
        "folders": folders,
        "sbx": sbx_version,
    });
    Ok(sha256_hex(doc.to_string().as_bytes())[..16].to_string())
}

/// Every file below `dir`, sorted by its path relative to `root` (with `/`).
fn walk(root: &Path, dir: &Path, out: &mut Vec<serde_json::Value>) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<_>>()
        .with_context(|| format!("reading {}", dir.display()))?;
    entries.sort();
    for path in entries {
        let meta = fs::metadata(&path).with_context(|| format!("reading {}", path.display()))?;
        if meta.is_dir() {
            walk(root, &path, out)?;
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        out.push(serde_json::json!([
            rel,
            executable(&meta),
            sha256_hex(&bytes)
        ]));
    }
    Ok(())
}

#[cfg(unix)]
fn executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_: &fs::Metadata) -> bool {
    false
}

/// One kit folder's image, as `sbx kit validate` named it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct KitImage {
    pub kit: String,
    pub image: String,
}

/// What `kit build` recorded for a key: `<state>/kit-images/<key>.json`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Record {
    pub key: String,
    pub backend: String,
    pub kits: Vec<String>,
    pub images: Vec<KitImage>,
    pub sbx_version: String,
    pub built_at: String,
}

pub fn records_dir(state: &Path) -> PathBuf {
    state.join("kit-images")
}

pub fn read_record(dir: &Path, key: &str) -> Option<Record> {
    let text = fs::read_to_string(dir.join(format!("{key}.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

/// Written to a temporary file and renamed, so that a step that reads it at the same time sees
/// the old record or the new one, never half of one.
pub fn write_record(dir: &Path, record: &Record) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{}.json", record.key));
    let tmp = dir.join(format!(".{}.{}.tmp", record.key, std::process::id()));
    let text = serde_json::to_string_pretty(record)? + "\n";
    fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
}

/// Whether the records hold this backend and kit set under another key: its kit files (or sbx)
/// changed since the last `kit build`.
fn recorded_under_another_key(dir: &Path, set: &KitSet, key: &str) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        let path = e.path();
        path.extension().is_some_and(|x| x == "json")
            && fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str::<Record>(&t).ok())
                .is_some_and(|r| r.key != key && r.backend == set.backend && r.kits == set.kits)
    })
}

/// The `sbx-kit-src:…` images in sbx's image store (`sbx template ls --json`).
pub fn present_images() -> Result<BTreeSet<String>> {
    #[derive(Deserialize)]
    struct Template {
        repository: String,
        tag: String,
    }
    if cfg!(any(test, feature = "test-support")) {
        anyhow::bail!("tests never read sbx's image store");
    }
    {
        let out = headless::sbx_command(&template_ls_argv(), false)
            .stdin(Stdio::null())
            .output()
            .context("running `sbx template ls --json`")?;
        anyhow::ensure!(
            out.status.success(),
            "`sbx template ls --json` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        let list: Vec<Template> =
            serde_json::from_slice(&out.stdout).context("reading `sbx template ls --json`")?;
        Ok(list
            .into_iter()
            .map(|t| {
                let repo = t.repository.rsplit('/').next().unwrap_or_default();
                format!("{repo}:{}", t.tag)
            })
            .collect())
    }
}

pub fn template_ls_argv() -> Vec<String> {
    ["sbx", "template", "ls", "--json"]
        .map(String::from)
        .to_vec()
}

/// Where a kit set's images stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// `kit build` ran for this very key, and sbx still has every image.
    Ready,
    /// Never built, or an image is gone from sbx's store.
    Missing,
    /// Built for an earlier state of its kit files (or another sbx); built for this one, no.
    Stale,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Ready => "ready",
            State::Missing => "missing",
            State::Stale => "stale",
        }
    }
}

/// The state of `set` under `key`, given the record folder and the images sbx has.
pub fn state_of(dir: &Path, set: &KitSet, key: &str, present: &BTreeSet<String>) -> State {
    match read_record(dir, key) {
        Some(r) if r.images.iter().all(|i| present.contains(&i.image)) => State::Ready,
        Some(_) => State::Missing,
        None if recorded_under_another_key(dir, set, key) => State::Stale,
        None => State::Missing,
    }
}

/// The image `sbx kit validate` names on stderr (`sbx-kit-src:rtk-ce5871a2fe3a`), and whether
/// it built it now (`— built (2s)`) rather than found it (`— cached`).
pub fn parse_validate(stderr: &str) -> Option<(String, bool)> {
    let image = stderr
        .split_whitespace()
        .find(|w| w.starts_with("sbx-kit-src:"))?
        .to_string();
    let built = stderr.lines().any(|l| l.contains("— built"));
    Some((image, built))
}

/// `sbx kit validate DIR`, its stderr echoed to ours as it comes and kept, its stdout on our
/// stderr too: stdout carries only the JSON.
fn validate(dir: &Path) -> Result<(String, bool), String> {
    let argv: Vec<String> = vec![
        "sbx".into(),
        "kit".into(),
        "validate".into(),
        dir.display().to_string(),
    ];
    let mut child = headless::sbx_command(&argv, false)
        .stdin(Stdio::null())
        .stdout(headless::stderr_stdio())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("running sbx kit validate: {e}"))?;
    let mut text = String::new();
    if let Some(stderr) = child.stderr.take() {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            eprintln!("{line}");
            text.push_str(&line);
            text.push('\n');
        }
    }
    let status = child
        .wait()
        .map_err(|e| format!("running sbx kit validate: {e}"))?;
    if !status.success() {
        return Err(format!(
            "sbx kit validate {} failed ({status})",
            dir.display()
        ));
    }
    parse_validate(&text)
        .ok_or_else(|| format!("sbx kit validate {} named no kit image", dir.display()))
}

/// `aisbx kit build --kit NAME… [--backend B] [--json]`.
pub(crate) fn main(raw: &[String]) -> Outcome {
    let (sub, rest) = match raw.split_first() {
        Some((sub, rest)) => (sub.as_str(), rest),
        None => return Err(Fail::usage("kit what? `aisbx kit build`")),
    };
    if sub != "build" {
        return Err(Fail::usage(format!("unknown kit subcommand: {sub}")));
    }
    let args = Args::parse(rest, &["json"], &["kit", "backend"])?;
    if let Some(extra) = args.positional.first().or(args.trailing.first()) {
        return Err(Fail::usage(format!("kit build takes no argument: {extra}")));
    }
    let backend = args
        .one("backend")?
        .unwrap_or_else(|| DEFAULT_BACKEND.into());
    let kits = split_kits(&args.all("kit"));
    let catalog = Catalog::load().map_err(|e| Fail::sandbox(format!("{e:#}")))?;
    let set = resolve(&catalog, &backend, &kits).map_err(Fail::usage)?;
    let sbx_version = sbx::version().map_err(|e| Fail::sandbox(format!("{e:#}")))?;
    let mut images = Vec::new();
    let mut any_built = false;
    for dir in &set.dirs {
        let kit = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        eprintln!("aisbx: kit {kit}: sbx kit validate {}", dir.display());
        let (image, built) = validate(dir).map_err(Fail::sandbox)?;
        any_built |= built;
        images.push((KitImage { kit, image }, dir.clone(), built));
    }
    // The key after the build: a folder that changed during it gives a key no step finds.
    let key = key(&set, &sbx_version).map_err(|e| Fail::sandbox(format!("{e:#}")))?;
    let record = Record {
        key: key.clone(),
        backend: set.backend.clone(),
        kits: set.kits.clone(),
        images: images.iter().map(|(i, _, _)| i.clone()).collect(),
        sbx_version: sbx_version.clone(),
        built_at: now_rfc3339(),
    };
    write_record(&records_dir(&state::state_dir()), &record)
        .map_err(|e| Fail::sandbox(format!("{e:#}")))?;
    let status = |built: bool| if built { "built" } else { "cached" };
    if args.has("json") {
        let doc = serde_json::json!({
            "key": key,
            "backend": set.backend,
            "kits": set.kits,
            "status": status(any_built),
            "images": images.iter().map(|(i, dir, built)| serde_json::json!({
                "kit": i.kit, "dir": dir, "image": i.image, "status": status(*built),
            })).collect::<Vec<_>>(),
            "sbx_version": sbx_version,
        });
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
    } else {
        for (i, _, built) in &images {
            println!("{}\t{}\t{}", i.kit, i.image, status(*built));
        }
        println!("key {key}: {}", status(any_built));
    }
    Ok(0)
}

/// `--kit` values, each one kit or several separated by commas, as one sorted set.
pub fn split_kits(values: &[String]) -> Vec<String> {
    let set: BTreeSet<String> = values
        .iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(String::from)
        .collect();
    set.into_iter().collect()
}

/// `aisbx doctor --kit …`: for each kit set (one per `--kit` value, its kits separated by
/// commas), whether its images are ready, missing or stale. The problems name what to run.
pub fn doctor_kit_images(backend: &str, sets: &[Vec<String>]) -> (serde_json::Value, Vec<String>) {
    let mut problems = Vec::new();
    let catalog = match Catalog::load() {
        Ok(c) => c,
        Err(e) => return (serde_json::json!([]), vec![format!("kits: {e:#}")]),
    };
    let sbx_version = sbx::version().unwrap_or_default();
    let present = match present_images() {
        Ok(p) => p,
        Err(e) => {
            problems.push(format!("kit images: {e:#}"));
            BTreeSet::new()
        }
    };
    let dir = records_dir(&state::state_dir());
    let mut rows = Vec::new();
    for kits in sets {
        let set = match resolve(&catalog, backend, kits) {
            Ok(set) => set,
            Err(e) => {
                problems.push(format!("kit set {}: {e}", kits.join(",")));
                rows.push(serde_json::json!({"backend": backend, "kits": kits, "key": null, "state": null, "error": e}));
                continue;
            }
        };
        let key = match key(&set, &sbx_version) {
            Ok(k) => k,
            Err(e) => {
                let e = format!("{e:#}");
                problems.push(format!("kit set {}: {e}", set.label()));
                rows.push(serde_json::json!({"backend": backend, "kits": set.kits, "key": null, "state": null, "error": e}));
                continue;
            }
        };
        let state = state_of(&dir, &set, &key, &present);
        if state != State::Ready {
            problems.push(format!(
                "kit image {}: {}; run {}",
                state.as_str(),
                set.label(),
                set.build_command()
            ));
        }
        rows.push(serde_json::json!({
            "backend": set.backend, "kits": set.kits, "key": key, "state": state.as_str(), "error": null,
        }));
    }
    (serde_json::Value::Array(rows), problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kit(root: &Path, name: &str, body: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(dir.join("files")).unwrap();
        fs::write(dir.join(format!("{name}.yaml")), body).unwrap();
        fs::write(dir.join("files/run.sh"), "echo hi\n").unwrap();
        dir
    }

    /// A fresh scratch folder per test.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aisbx-kit-build-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn set(backend: &str, kits: &[&str], dirs: &[PathBuf]) -> KitSet {
        KitSet {
            backend: backend.into(),
            kits: split_kits(&kits.iter().map(|k| k.to_string()).collect::<Vec<_>>()),
            dirs: dirs.to_vec(),
        }
    }

    #[test]
    fn the_key_follows_every_kit_file_but_not_the_order() {
        let tmp = scratch("key");
        let b = kit(&tmp, "claude-anthropic", "kind: workload\n");
        let r = kit(&tmp, "rtk", "kind: mixin\n");
        let j = kit(&tmp, "jdk25", "kind: mixin\n");
        let one = key(
            &set(
                "claude-anthropic",
                &["rtk", "jdk25"],
                &[b.clone(), r.clone(), j.clone()],
            ),
            "v1",
        )
        .unwrap();
        let other = key(
            &set(
                "claude-anthropic",
                &["jdk25", "rtk"],
                &[b.clone(), j.clone(), r.clone()],
            ),
            "v1",
        )
        .unwrap();
        assert_eq!(one, other);
        assert_eq!(one.len(), 16);
        assert_ne!(
            one,
            key(
                &set(
                    "claude-anthropic",
                    &["rtk", "jdk25"],
                    &[b.clone(), r.clone(), j.clone()]
                ),
                "v2"
            )
            .unwrap()
        );
        fs::write(r.join("files/run.sh"), "echo changed\n").unwrap();
        let changed = key(
            &set(
                "claude-anthropic",
                &["rtk", "jdk25"],
                &[b.clone(), r.clone(), j.clone()],
            ),
            "v1",
        )
        .unwrap();
        assert_ne!(one, changed);
        fs::write(r.join("files/extra"), "").unwrap();
        assert_ne!(
            changed,
            key(
                &set("claude-anthropic", &["rtk", "jdk25"], &[b, r, j]),
                "v1"
            )
            .unwrap()
        );
    }

    #[test]
    fn kit_values_split_on_commas_into_a_sorted_set() {
        let values = ["rtk,jdk25".to_string(), " rtk ".into(), "".into()];
        assert_eq!(split_kits(&values), ["jdk25", "rtk"]);
    }

    #[test]
    fn validate_output_names_the_image_and_whether_it_was_built() {
        let built = "\nKITS\n  ○ /k/sfprobe — building…\n  ✓ /k/sfprobe — built (2s)\n      sbx-kit-src:sfprobe-a2bda3fa6f37\n";
        assert_eq!(
            parse_validate(built),
            Some(("sbx-kit-src:sfprobe-a2bda3fa6f37".into(), true))
        );
        let cached = "\nKITS\n  ✓ /k/rtk — cached\n      sbx-kit-src:rtk-ce5871a2fe3a\n";
        assert_eq!(
            parse_validate(cached),
            Some(("sbx-kit-src:rtk-ce5871a2fe3a".into(), false))
        );
        assert_eq!(
            parse_validate("KITS\n  ✓ /k/rtk — valid (directory)\n"),
            None
        );
    }

    #[test]
    fn a_set_is_ready_only_with_its_record_and_every_image() {
        let dir = scratch("records").join("kit-images");
        let s = set("claude-anthropic", &["rtk"], &[]);
        let none = BTreeSet::new();
        assert_eq!(state_of(&dir, &s, "aaaa", &none), State::Missing);
        let record = |key: &str| Record {
            key: key.into(),
            backend: "claude-anthropic".into(),
            kits: vec!["rtk".into()],
            images: vec![KitImage {
                kit: "rtk".into(),
                image: "sbx-kit-src:rtk-1".into(),
            }],
            sbx_version: "v1".into(),
            built_at: "2026-11-14T00:00:00.000Z".into(),
        };
        write_record(&dir, &record("old")).unwrap();
        // Built before the kit files changed: stale.
        assert_eq!(state_of(&dir, &s, "aaaa", &none), State::Stale);
        // Another set's record says nothing about this one.
        assert_eq!(
            state_of(&dir, &set("claude-anthropic", &[], &[]), "aaaa", &none),
            State::Missing
        );
        write_record(&dir, &record("aaaa")).unwrap();
        assert_eq!(state_of(&dir, &s, "aaaa", &none), State::Missing);
        let present: BTreeSet<String> = ["sbx-kit-src:rtk-1".to_string()].into();
        assert_eq!(state_of(&dir, &s, "aaaa", &present), State::Ready);
        assert_eq!(
            read_record(&dir, "aaaa").unwrap().images,
            record("aaaa").images
        );
        assert!(fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")));
    }

    #[test]
    fn the_label_and_the_command_name_the_backend_and_the_kits() {
        let s = set("claude-anthropic", &["rtk", "jdk25"], &[]);
        assert_eq!(s.label(), "claude-anthropic, jdk25, rtk");
        assert_eq!(
            s.build_command(),
            "aisbx kit build --backend claude-anthropic --kit jdk25 --kit rtk"
        );
    }

    #[test]
    fn backends_with_generated_kits_cannot_be_built_ahead() {
        let catalog = Catalog::load().unwrap();
        for backend in ["claude-litellm", "claude-local", "claude-token"] {
            if catalog.backend(backend).is_some() {
                let e = resolve(&catalog, backend, &[]).unwrap_err();
                assert!(e.contains("cannot be built ahead"), "{backend}: {e}");
            }
        }
        assert!(
            resolve(&catalog, "claude-anthropic", &["no-such-kit".into()])
                .unwrap_err()
                .contains("no addon kit")
        );
        let ok = resolve(&catalog, "claude-anthropic", &["rtk".into()]).unwrap();
        assert_eq!(ok.kits, ["rtk"]);
        assert_eq!(ok.dirs.len(), 2);
        assert!(
            present_images().is_err(),
            "tests never read sbx's image store"
        );
    }

    #[test]
    fn kits_prebuilt_is_protocol_2_and_absent_from_a_spec_that_does_not_say_it() {
        use crate::step::Spec;
        let spec = r#"{"protocol": 2, "name": "sf-1", "backend": "claude-anthropic", "kits": ["rtk"],
          "workspace": "/w", "work": {"checks": [{"workdir": "", "argv": ["true"]}]},
          "timeout_seconds": 60, "kits_prebuilt": true}"#;
        assert!(Spec::parse(spec).unwrap().kits_prebuilt);
        let one = spec.replacen("\"protocol\": 2", "\"protocol\": 1", 1);
        assert_eq!(
            Spec::parse(&one).unwrap_err(),
            "kits_prebuilt needs protocol 2"
        );
        let without = spec.replacen(", \"kits_prebuilt\": true", "", 1);
        let parsed = Spec::parse(&without).unwrap();
        assert!(!parsed.kits_prebuilt);
        // The spec copy, and the pre-warm digest over it, stay what they were without the field.
        assert!(serde_json::to_value(&parsed)
            .unwrap()
            .get("kits_prebuilt")
            .is_none());
    }

    #[test]
    fn doctor_text_names_each_kit_set_and_its_state() {
        let doc = serde_json::json!({
            "aisbx_version": "0.51.0.6", "step_protocol": 1,
            "sbx": {"version": "v0.51.0", "required": "0.51.0", "ok": true, "signed_in": true},
            "policy": {"fallback_rule": true},
            "kits": {"user_dirs": [], "found": [], "skipped": []},
            "kit_images": [{"backend": "claude-anthropic", "kits": ["rtk"], "key": "0123456789abcdef", "state": "stale", "error": null}],
            "ok": false,
            "problems": ["kit image stale: claude-anthropic, rtk; run aisbx kit build --backend claude-anthropic --kit rtk"],
        });
        let text = crate::step::doctor_text(&doc);
        assert!(
            text.contains("kit images claude-anthropic, rtk: stale"),
            "{text}"
        );
        assert!(text.contains("problem: kit image stale"), "{text}");
    }

    #[test]
    fn kit_build_takes_only_build_and_known_kits() {
        let strings = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(main(&[]).unwrap_err().code, headless::EXIT_USAGE);
        assert_eq!(
            main(&strings(&["make"])).unwrap_err().code,
            headless::EXIT_USAGE
        );
        assert_eq!(
            main(&strings(&["build", "--kit", "no-such-kit"]))
                .unwrap_err()
                .code,
            headless::EXIT_USAGE
        );
        assert!(headless::is_subcommand("kit"));
        assert!(headless::HELP.contains("aisbx kit build --kit NAME"));
    }

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tests/step")
            .join(name);
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// The golden runs of `kits_prebuilt` (tests/step/): `kit build` built the probe kit, a step
    /// created from its images, and a step after a change to the kit stopped in create at once.
    #[test]
    fn the_kit_fixtures_follow_the_protocol() {
        let build: serde_json::Value = serde_json::from_str(&fixture("kit-build.json")).unwrap();
        for key in ["key", "backend", "kits", "status", "images", "sbx_version"] {
            assert!(build.get(key).is_some(), "kit-build.json lacks {key}");
        }
        assert_eq!(build["key"].as_str().unwrap().len(), 16);
        for image in build["images"].as_array().unwrap() {
            assert!(image["image"].as_str().unwrap().starts_with("sbx-kit-src:"));
            assert!(["built", "cached"].contains(&image["status"].as_str().unwrap()));
        }
        for (name, phases) in [
            (
                "prebuilt.ndjson",
                vec![
                    "refuse", "create", "prove", "log", "work", "log", "remove", "report",
                ],
            ),
            ("kit-missing.ndjson", vec!["refuse", "create", "report"]),
        ] {
            let mut done = Vec::new();
            let mut calls = Vec::new();
            for (i, line) in fixture(name).lines().filter(|l| !l.is_empty()).enumerate() {
                let d: serde_json::Value = serde_json::from_str(line).unwrap();
                assert_eq!(d["seq"], i as u64 + 1, "{name}");
                assert!(
                    state::parse_rfc3339(d["t"].as_str().unwrap()).is_some(),
                    "{name}"
                );
                if d["kind"] == "phase" && d["status"] != "start" {
                    done.push(d["phase"].as_str().unwrap().to_string());
                }
                if d["kind"] == "sbx" && d["phase"] == "create" {
                    calls.push(d["argv"][1].as_str().unwrap().to_string());
                }
            }
            assert_eq!(done, phases, "{name}");
            // The image lookup comes first; only a step whose images are there creates.
            let expected: &[&str] = match name {
                "prebuilt.ndjson" => &["version", "template", "up"],
                _ => &["version", "template"],
            };
            assert_eq!(calls, expected, "{name}");
        }
        let ok: serde_json::Value = serde_json::from_str(&fixture("prebuilt-report.json")).unwrap();
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["kit_key"], build["key"]);
        assert!(ok.get("error_kind").is_none());
        let missing: serde_json::Value =
            serde_json::from_str(&fixture("kit-missing-report.json")).unwrap();
        assert_eq!(missing["exit"], headless::EXIT_SANDBOX);
        assert_eq!(missing["phase"], "create");
        assert_eq!(missing["error_kind"], "kit_image");
        assert!(missing["error"]
            .as_str()
            .unwrap()
            .starts_with("kit image not built: claude-anthropic, sfprobe; run aisbx kit build"));
        assert_ne!(
            missing["kit_key"], build["key"],
            "the changed kit has another key"
        );
        for name in ["prebuilt-spec.json", "kit-missing-spec.json"] {
            let spec = crate::step::Spec::parse(&fixture(name)).unwrap();
            assert!(spec.kits_prebuilt, "{name}");
        }
    }
}
