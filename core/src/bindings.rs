//! Credential bindings: sbx's record of which credentials the user lets a kit use. sbx withholds a
//! local kit's credential until a binding approves it, so [`ensure`] adds the missing ones.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use yaml_rust2::yaml::Hash;
use yaml_rust2::{Yaml, YamlEmitter, YamlLoader};

/// One approval sbx wants for a credential a kit declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub service: String,
    pub mechanism: Mechanism,
    pub domains: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    ApiKey,
    OAuth,
}

impl Mechanism {
    /// The key under the service in `credentials.yaml` — the kit spec's spelling as well.
    fn key(self) -> &'static str {
        match self {
            Mechanism::ApiKey => "apiKey",
            Mechanism::OAuth => "oauth",
        }
    }
}

/// The bindings a kit descriptor's `credential@1` capabilities ask for, a capability group's
/// members and the optional credentials included.
pub fn from_descriptor(text: &str) -> Result<Vec<Binding>> {
    let doc = crate::kits::load_descriptor(text).context("parsing the kit descriptor")?;
    let mut out = Vec::new();
    for cap in crate::kits::capabilities(&doc) {
        if cap.kind != "credential@1" {
            continue;
        }
        let item = &cap.config;
        let Some(service) = item["service"].as_str() else {
            continue;
        };
        if !item["apiKey"].is_badvalue() {
            let domains = strings(
                item["apiKey"]["inject"]
                    .as_vec()
                    .map(|v| v.iter().map(|i| &i["domain"]).collect::<Vec<_>>()),
            );
            out.push(Binding {
                service: service.to_string(),
                mechanism: Mechanism::ApiKey,
                domains,
            });
        }
        let oauth = &item["oauth"];
        if !oauth.is_badvalue() {
            let mut hosts = vec![&oauth["tokenEndpoint"]["host"]];
            hosts.extend(oauth["resourceHosts"].as_vec().into_iter().flatten());
            out.push(Binding {
                service: service.to_string(),
                mechanism: Mechanism::OAuth,
                domains: strings(Some(hosts)),
            });
        }
    }
    Ok(out)
}

fn strings(values: Option<Vec<&Yaml>>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values.into_iter().flatten() {
        if let Some(s) = v.as_str().map(str::to_string) {
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

/// The bindings every kit in `dirs` asks for. A dir without a readable, parseable descriptor is
/// skipped.
pub fn for_kits<P: AsRef<Path>>(dirs: &[P]) -> Vec<Binding> {
    dirs.iter()
        .filter_map(|d| fs::read_to_string(crate::kits::descriptor_path(d.as_ref())).ok())
        .filter_map(|text| from_descriptor(&text).ok())
        .flatten()
        .collect()
}

/// Where sbx keeps the bindings.
pub fn credentials_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(appdata).join("sbx").join("credentials.yaml"));
        }
    }
    crate::model::home_dir().map(|h| h.join(".config").join("sbx").join("credentials.yaml"))
}

/// Add what `wanted` lacks to the bindings file's text. `None` when nothing is missing, else the
/// new text and one line per service and mechanism that changed. A file of another shape than sbx
/// documents is an error.
pub fn merge(existing: &str, wanted: &[Binding]) -> Result<Option<(String, Vec<String>)>> {
    let mut root = YamlLoader::load_from_str(existing)
        .context("parsing the bindings file")?
        .into_iter()
        .next()
        .unwrap_or(Yaml::Null);
    if root.is_null() {
        root = Yaml::Hash(Hash::new());
    }
    let Yaml::Hash(top) = &mut root else {
        bail!("the bindings file is not a mapping");
    };
    let bindings = child_hash(top, "bindings")?;
    let mut changes = Vec::new();
    for want in wanted {
        let service = child_hash(bindings, &want.service)?;
        // The mechanism key itself is the approval, so a new one is a change without a domain too.
        let new = !service.contains_key(&Yaml::String(want.mechanism.key().into()));
        let mechanism = child_hash(service, want.mechanism.key())?;
        let domains = match mechanism
            .entry(Yaml::String("domains".into()))
            .or_insert_with(|| Yaml::Array(Vec::new()))
        {
            Yaml::Array(list) => list,
            v @ Yaml::Null => {
                *v = Yaml::Array(Vec::new());
                v.as_mut_vec().expect("just set")
            }
            _ => bail!(
                "bindings.{}.{}.domains is not a list",
                want.service,
                want.mechanism.key()
            ),
        };
        let added: Vec<&String> = want
            .domains
            .iter()
            .filter(|d| !domains.iter().any(|have| have.as_str() == Some(d.as_str())))
            .collect();
        if added.is_empty() && !new {
            continue;
        }
        domains.extend(added.iter().map(|d| Yaml::String(d.to_string())));
        changes.push(format!(
            "{} {}{}",
            want.service,
            want.mechanism.key(),
            match added.is_empty() {
                true => String::new(),
                false => format!(
                    " ({})",
                    added
                        .iter()
                        .map(|d| d.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        ));
    }
    if changes.is_empty() {
        return Ok(None);
    }
    let mut text = String::new();
    YamlEmitter::new(&mut text)
        .dump(&root)
        .context("writing the bindings file")?;
    let mut text = text.strip_prefix("---\n").unwrap_or(&text).to_string();
    text.push('\n');
    Ok(Some((text, changes)))
}

/// The mapping under `key`, created when absent or empty.
fn child_hash<'a>(parent: &'a mut Hash, key: &str) -> Result<&'a mut Hash> {
    let slot = parent
        .entry(Yaml::String(key.to_string()))
        .or_insert_with(|| Yaml::Hash(Hash::new()));
    if slot.is_null() {
        *slot = Yaml::Hash(Hash::new());
    }
    match slot {
        Yaml::Hash(h) => Ok(h),
        _ => bail!("{key} in the bindings file is not a mapping"),
    }
}

/// [`merge`] into the file at `path`, written only when something was missing. Returns the
/// changes, empty when there were none.
pub fn ensure_at(path: &Path, wanted: &[Binding]) -> Result<Vec<String>> {
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let existing = match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let Some((text, changes)) = merge(existing.as_deref().unwrap_or(""), wanted)
        .with_context(|| format!("in {}", path.display()))?
    else {
        return Ok(Vec::new());
    };
    let dir = path.parent().context("bindings path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension(format!("yaml.aisbx-{}.tmp", std::process::id()));
    let written = fs::write(&tmp, text.as_bytes())
        .with_context(|| format!("writing {}", tmp.display()))
        .and_then(|()| keep_private(&tmp, path))
        .and_then(|()| {
            fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
        });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written.map(|()| changes)
}

/// Give the new file the old one's mode, or owner-only for a new file.
fn keep_private(tmp: &Path, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::metadata(path)
            .map(|m| m.permissions())
            .unwrap_or_else(|_| fs::Permissions::from_mode(0o600));
        fs::set_permissions(tmp, perms).with_context(|| format!("chmod {}", tmp.display()))?;
    }
    #[cfg(not(unix))]
    let _ = (tmp, path);
    Ok(())
}

/// Approve what the kits in `dirs` declare, in sbx's real bindings file. Nothing is written under
/// `cargo test`: the tests use [`ensure_at`] on a scratch path.
pub fn ensure<P: AsRef<Path>>(dirs: &[P]) -> Result<Vec<String>> {
    if cfg!(any(test, feature = "test-support")) {
        return Ok(Vec::new());
    }
    let wanted = for_kits(dirs);
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let path = credentials_path().context("no home directory for sbx's credentials.yaml")?;
    ensure_at(&path, &wanted)
}

/// The line that tells the user what [`ensure`] approved, or why it could not.
pub fn note(result: &Result<Vec<String>>) -> Option<(bool, String)> {
    let file = credentials_path()
        .map(|p| crate::model::tilde(&p))
        .unwrap_or_else(|| "credentials.yaml".into());
    match result {
        Ok(changes) if changes.is_empty() => None,
        Ok(changes) => Some((
            false,
            format!(
                "approved kit credentials in {file}: {}",
                changes.join("; ")
            ),
        )),
        Err(e) => Some((
            true,
            format!(
                "could not approve kit credentials in {file} ({e:#}) — sbx withholds them until it is"
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agy_descriptor() -> String {
        let root = crate::kits::find_repo_root().expect("repo root");
        fs::read_to_string(crate::kits::descriptor_path(&root.join("kits/agy"))).unwrap()
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aisbx-bindings-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("sbx").join("credentials.yaml")
    }

    #[test]
    fn the_agy_kit_asks_for_both_of_its_credentials() {
        let wanted = from_descriptor(&agy_descriptor()).unwrap();
        assert_eq!(
            wanted,
            vec![
                Binding {
                    service: "google".into(),
                    mechanism: Mechanism::ApiKey,
                    domains: vec!["generativelanguage.googleapis.com".into()],
                },
                Binding {
                    service: "antigravity".into(),
                    mechanism: Mechanism::OAuth,
                    domains: vec![
                        "oauth2.googleapis.com".into(),
                        "aicode.googleapis.com".into(),
                        "cloudcode-pa.googleapis.com".into(),
                        "daily-cloudcode-pa.googleapis.com".into(),
                        "www.googleapis.com".into(),
                    ],
                },
            ]
        );
    }

    #[test]
    fn every_kit_credential_is_bound() {
        let root = crate::kits::find_repo_root().expect("repo root");
        let agy = for_kits(&[root.join("kits/agy"), root.join("kits/web")]);
        assert_eq!(
            agy,
            from_descriptor(&agy_descriptor()).unwrap(),
            "both of agy's, none of web's"
        );
        assert_eq!(agy[0].service, "google");
        assert_eq!(agy[0].mechanism, Mechanism::ApiKey);
        assert_eq!(
            agy[0].domains,
            vec!["generativelanguage.googleapis.com".to_string()]
        );
        assert!(for_kits(&[
            root.join("kits/claude-litellm"),
            root.join("kits/opencode-litellm")
        ])
        .is_empty());
        let conn = crate::testkit::litellm_root_with(&[crate::testkit::acme()]);
        let kit = crate::litellm::ensure_kit_in(
            &conn,
            &crate::testkit::acme(),
            crate::litellm::Agent::Claude,
        )
        .unwrap();
        assert!(for_kits(&[kit]).is_empty());
        let _ = fs::remove_dir_all(&conn);
        assert!(for_kits(&[root.join("kits/web"), root.join("kits/nope")]).is_empty());
    }

    #[test]
    fn the_sign_in_workloads_ask_for_their_bindings() {
        let root = crate::kits::find_repo_root().expect("repo root");
        assert_eq!(
            for_kits(&[root.join("kits/claude-anthropic")]),
            vec![
                Binding {
                    service: "anthropic".into(),
                    mechanism: Mechanism::ApiKey,
                    domains: vec!["api.anthropic.com".into(), "mcp-proxy.anthropic.com".into()],
                },
                Binding {
                    service: "anthropic".into(),
                    mechanism: Mechanism::OAuth,
                    domains: vec!["platform.claude.com".into()],
                },
            ]
        );
        assert_eq!(
            for_kits(&[root.join("kits/codex-openai")]),
            vec![
                Binding {
                    service: "openai".into(),
                    mechanism: Mechanism::ApiKey,
                    domains: vec!["api.openai.com".into(), "openai.com".into()],
                },
                Binding {
                    service: "openai".into(),
                    mechanism: Mechanism::OAuth,
                    domains: vec!["auth.openai.com".into(), "chatgpt.com".into()],
                },
            ]
        );
        assert!(for_kits(&[root.join("kits/claude-token")]).is_empty());
    }

    #[test]
    fn a_missing_file_is_created_and_a_second_pass_changes_nothing() {
        let path = scratch("create");
        let wanted = from_descriptor(&agy_descriptor()).unwrap();
        let changes = ensure_at(&path, &wanted).unwrap();
        assert_eq!(changes.len(), 2, "{changes:?}");
        let text = fs::read_to_string(&path).unwrap();
        let doc = &YamlLoader::load_from_str(&text).unwrap()[0];
        let oauth = doc["bindings"]["antigravity"]["oauth"]["domains"]
            .as_vec()
            .unwrap();
        assert_eq!(oauth.len(), 5);
        assert_eq!(oauth[0].as_str(), Some("oauth2.googleapis.com"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(ensure_at(&path, &wanted).unwrap().is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        let _ = fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn a_merge_keeps_what_sbx_wrote() {
        let existing = concat!(
            "version: 1\n",
            "bindings:\n",
            "    anthropic:\n",
            "        oauth:\n",
            "            domains:\n",
            "                - platform.claude.com\n",
            "    antigravity:\n",
            "        oauth:\n",
            "            domains: [oauth2.googleapis.com]\n",
        );
        let wanted = vec![Binding {
            service: "antigravity".into(),
            mechanism: Mechanism::OAuth,
            domains: vec!["oauth2.googleapis.com".into(), "www.googleapis.com".into()],
        }];
        let (text, changes) = merge(existing, &wanted).unwrap().unwrap();
        assert_eq!(changes, vec!["antigravity oauth (www.googleapis.com)"]);
        let doc = &YamlLoader::load_from_str(&text).unwrap()[0];
        assert_eq!(doc["version"].as_i64(), Some(1));
        assert_eq!(
            doc["bindings"]["anthropic"]["oauth"]["domains"][0].as_str(),
            Some("platform.claude.com")
        );
        let domains = doc["bindings"]["antigravity"]["oauth"]["domains"]
            .as_vec()
            .unwrap();
        let domains: Vec<_> = domains.iter().filter_map(Yaml::as_str).collect();
        assert_eq!(domains, ["oauth2.googleapis.com", "www.googleapis.com"]);
        assert!(merge(&text, &wanted).unwrap().is_none());
    }

    #[test]
    fn a_file_of_another_shape_is_never_overwritten() {
        let wanted = from_descriptor(&agy_descriptor()).unwrap();
        assert!(merge("- a list\n", &wanted).is_err());
        assert!(merge("bindings: [x]\n", &wanted).is_err());
        assert!(merge(
            "bindings:\n  antigravity:\n    oauth:\n      domains: x\n",
            &wanted
        )
        .is_err());
        let path = scratch("shape");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "bindings: [x]\n").unwrap();
        assert!(ensure_at(&path, &wanted).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "bindings: [x]\n");
        let _ = fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn the_note_names_the_change_or_the_failure() {
        assert_eq!(note(&Ok(Vec::new())), None);
        let (err, text) = note(&Ok(vec!["antigravity oauth (x)".into()])).unwrap();
        assert!(!err && text.contains("antigravity oauth (x)"), "{text}");
        let (err, text) = note(&Err(anyhow::anyhow!("boom"))).unwrap();
        assert!(
            err && text.contains("boom") && text.contains("withholds"),
            "{text}"
        );
    }
}
