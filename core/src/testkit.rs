//! Fixtures shared by the tests of this crate and of the frontends, which build it with the
//! `test-support` feature. Never compiled into a normal build.

/// The auth editor as it looks for a sandbox currently on the shared `/login`.
pub fn auth_form(account: crate::app::AccountChoice) -> crate::app::AuthForm {
    crate::app::AuthForm {
        sandbox: "claude-anthropic-proj".into(),
        current: crate::model::Auth::Login,
        account,
        tokens: vec!["private".into(), "work".into()],
        token_name: crate::app::TextInput::new("personal"),
        token: crate::app::TextInput::new("sk-ant-oat01-0123456789"),
        focus: 0,
        ram_volumes: false,
    }
}

/// A host with one of everything stored: the `/login` credential, two named setup tokens,
/// a service secret no kit declares, the
/// key of the LiteLLM connection `acme` and one no connection names, and a custom secret that is
/// none of aisbx's business.
pub fn sample_secrets() -> crate::sbx::Secrets {
    use crate::sbx::{self, CustomSecret, Service};
    let token = |scope: &str, env: &str, placeholder: String| CustomSecret {
        scope: scope.into(),
        targets: vec![sbx::TOKEN_HOST.into()],
        env: env.into(),
        placeholder,
        masked: "sk-ant********".into(),
    };
    crate::sbx::Secrets {
        services: vec![
            Service::sample("litellm"),
            Service {
                masked: "(oauth configured)".into(),
                ..Service::sample(sbx::OAUTH_SERVICE)
            },
            // MCP gateway credentials: a header placeholder's and a client secret.
            Service::sample("mcp:acme:api-key"),
            Service::sample("mcp:slack:client_secret"),
        ],
        custom: vec![
            token(
                sbx::GLOBAL_SCOPE,
                "",
                sbx::named_token_placeholder("private"),
            ),
            token(sbx::GLOBAL_SCOPE, "", sbx::named_token_placeholder("work")),
            litellm_key("acme", "litellm.example.com"),
            litellm_key("gone", "litellm.example.com"),
            CustomSecret {
                scope: sbx::GLOBAL_SCOPE.into(),
                targets: vec!["api.example.com".into()],
                env: "SOME_API_KEY".into(),
                placeholder: "someone-elses-secret".into(),
                masked: "sk***".into(),
            },
        ],
    }
}

/// A request held for approval, as `sbx policy approval ls --json` reports one.
pub fn held(sandbox: &str, title: &str) -> crate::sbx::Approval {
    crate::sbx::Approval {
        id: format!("network:{sandbox}-{title}"),
        sandbox: sandbox.into(),
        title: title.into(),
        detail: "Protocol: TCP\nResource type: domain\nno matching allow rule (default deny)"
            .into(),
        options: vec![
            crate::sbx::ApprovalOption {
                id: "allow".into(),
                label: "Allow".into(),
            },
            crate::sbx::ApprovalOption {
                id: "dismiss".into(),
                label: "Dismiss".into(),
            },
        ],
    }
}

/// The URL of the LiteLLM connection every test app has ([`acme`]).
pub const ACME_URL: &str = "https://litellm.example.com";

/// A LiteLLM connection for tests: `name` on `url`.
pub fn connection(name: &str, url: &str) -> crate::litellm::Connection {
    crate::litellm::Connection {
        name: name.into(),
        url: url.into(),
    }
}

/// The connection every test app is given ([`litellm_root`]): `acme` on [`ACME_URL`].
pub fn acme() -> crate::litellm::Connection {
    connection("acme", ACME_URL)
}

/// The global custom secret holding a connection's key for `host`, as `sbx secret ls` lists it.
pub fn litellm_key(name: &str, host: &str) -> crate::sbx::CustomSecret {
    crate::sbx::CustomSecret {
        scope: crate::sbx::GLOBAL_SCOPE.into(),
        targets: vec![host.into()],
        env: String::new(),
        placeholder: crate::litellm::placeholder(name),
        masked: "sk-l********".into(),
    }
}

/// A fresh folder of LiteLLM connections for one test, holding exactly `conns` — the stand-in
/// for `state_dir()/litellm`, which no test may read.
pub fn litellm_root_with(conns: &[crate::litellm::Connection]) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "aisbx-tests-litellm-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch folder for connections");
    for c in conns {
        crate::litellm::save_in(&root, c).expect("a valid test connection");
        harvest_in(&root, &c.name, &harvested(&c.name));
    }
    root
}

/// The model ids a sandbox on a test connection last read from its gateway: `claude-sonnet-5`
/// and `gpt-6` for [`acme`], `<name>-large` and `<name>-small` for any other.
pub fn harvested(name: &str) -> Vec<String> {
    match name {
        "acme" => vec!["claude-sonnet-5".into(), "gpt-6".into()],
        _ => vec![format!("{name}-large"), format!("{name}-small")],
    }
}

/// Keep `ids` as connection `name`'s harvested list under `root`, as the dashboard does after a
/// sandbox on it read the gateway.
pub fn harvest_in(root: &std::path::Path, name: &str, ids: &[String]) {
    let now = crate::state::now_epoch() as i64;
    let harvest = crate::models::Harvest {
        fetched_at: now,
        base_url: ACME_URL.into(),
        models: ids
            .iter()
            .map(|id| crate::models::Model {
                id: id.clone(),
                name: id.clone(),
                ..Default::default()
            })
            .collect(),
    };
    crate::models::store_harvest(
        &root.join(name).join("models.json"),
        "fixture",
        &harvest,
        now,
    )
    .expect("a harvested list for a test connection");
}

/// The folder of connections [`crate::app::App::for_test`] starts on: [`acme`] alone, written
/// once per test process.
pub fn litellm_root() -> std::path::PathBuf {
    static ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| litellm_root_with(&[acme()])).clone()
}

/// The home folder [`crate::app::App::for_test`] looks for host caches in: one folder, with a file
/// in it, for every folder a host-folder kit looks for, written once per test process.
pub fn host_home() -> std::path::PathBuf {
    static HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home =
            std::env::temp_dir().join(format!("aisbx-tests-host-home-{}", std::process::id()));
        let catalog = crate::kits::Catalog::load().expect("kits/ must be discoverable");
        for addon in &catalog.addons {
            let Some(found) = crate::model::host_cache_detection_in(&addon.kit.name, None) else {
                continue;
            };
            for dir in &found.looked_for {
                let dir = home.join(dir.trim_start_matches("~/"));
                std::fs::create_dir_all(&dir).expect("a host cache folder");
                std::fs::write(dir.join("entry"), "").expect("a host cache entry");
            }
        }
        home
    })
    .clone()
}
