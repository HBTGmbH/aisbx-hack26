//! Golden tests for the create form: for a fixed set of answers, entered through key events
//! only, the whole job it starts (each step's label, argv and stdin, the job title, and what
//! gets registered).

use std::path::{Path, PathBuf};

use crate::app::{App, CreateForm, CreateRow, Modal, PendingJob, Screen};
use crate::create::{ConnChoice, CreateStep, WizardRow};
use crate::input::{KeyCode, KeyEvent, KeyModifiers};
use crate::kits::{Catalog, Provider};
use crate::sbx;

/// A step as the job runner sees it: label, argv, and whether a value goes in on stdin.
type Steps = Vec<(String, Vec<String>, bool)>;

/// Not a real token, and shaped like none: the checks below only need it to be non-empty.
const TOKEN_VALUE: &str = "golden-test-token-value";

const BAD_PORT: &str =
    "“8080:not-a-port” is not a port mapping — use 8080:3000, 3000, or 127.0.0.1:8080:3000/tcp";
const BAD_HOST: &str =
    "“bad!host” is not a host to deny — use api.example.com, *.example.com or **";
const BAD_NAME: &str = "name the token in lower-case letters, digits and dashes";
const TAKEN: &str = "a token of that name exists — pick it in the list";
const NO_VALUE: &str =
    "paste a setup token, or pick /login — generate one with `claude setup-token`";

fn new_app() -> App {
    let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
    app.cwd = PathBuf::from("/proj");
    app
}

/// A named token stored on the host, the way `sbx secret ls --json` reports one.
fn with_stored_token(app: &mut App, name: &str) {
    app.secrets = Some(sbx::Secrets {
        services: Vec::new(),
        custom: vec![
            crate::testkit::litellm_key("acme", "litellm.example.com"),
            sbx::CustomSecret {
                scope: sbx::GLOBAL_SCOPE.into(),
                targets: vec![sbx::TOKEN_HOST.into()],
                env: String::new(),
                placeholder: sbx::named_token_placeholder(name),
                masked: "***".into(),
            },
        ],
    });
}

fn strings(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|a| a.to_string()).collect()
}

fn path(p: &Path) -> String {
    p.display().to_string()
}

/// A kit directory of this checkout, backend or addon.
fn kit(app: &App, name: &str) -> String {
    let catalog = &app.catalog;
    let dir = catalog
        .backends
        .iter()
        .map(|b| &b.kit)
        .chain(catalog.addons.iter().map(|a| &a.kit))
        .find(|k| k.name == name)
        .unwrap_or_else(|| panic!("the checkout ships {name}"))
        .dir
        .clone();
    path(&dir)
}

/// The generated kit wiring the test app's LiteLLM connection, `acme`, into `agent`.
fn conn_kit(app: &App, agent: &str) -> String {
    path(
        &app.litellm_root
            .join("acme")
            .join(format!("litellm-acme-{agent}")),
    )
}

/// ↵ on a provider on step 1: picked, and on to the Harness step — past the Connection step,
/// on the connection it opens on, for LiteLLM.
fn choose(app: &mut App, provider: Provider) {
    let providers = form(app).providers(&app.catalog);
    let index = providers.iter().position(|p| *p == provider);
    let row = WizardRow::Provider(index.unwrap_or_else(|| panic!("{provider:?} is offered")));
    enter(app, row);
    if provider == Provider::Litellm {
        assert_eq!(step(app), CreateStep::Connection);
        enter(app, WizardRow::Next);
    }
    assert_eq!(step(app), CreateStep::Harness);
}

/// Step 3's row for an agent of the picked provider.
fn agent(app: &App, agent: &str) -> WizardRow {
    let harnesses = form(app).harnesses(&app.catalog);
    let index = harnesses.iter().position(|h| h.agent == agent);
    WizardRow::Agent(index.unwrap_or_else(|| panic!("the checkout ships {agent}")))
}

fn addon(app: &App, name: &str) -> CreateRow {
    let index = app.catalog.addons.iter().position(|a| a.kit.name == name);
    CreateRow::Addon(index.unwrap_or_else(|| panic!("the checkout ships {name}")))
}

fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::from(code));
}

fn form(app: &App) -> &CreateForm {
    let Screen::Create(form) = &app.screen else {
        panic!("the create form is open")
    };
    form
}

fn step(app: &App) -> CreateStep {
    form(app).wizard_step()
}

fn focused(app: &App) -> WizardRow {
    let form = form(app);
    let rows = form.wizard_rows(&app.catalog);
    rows[form.wizard_focus().min(rows.len() - 1)]
}

/// Walk the cursor to `row` on the current step. A step's buttons are one row to ↓, so the ones
/// after the first are reached from it with →.
fn go(app: &mut App, row: WizardRow) {
    if let Some(fold) = field_of(row).and_then(CreateRow::fold) {
        if !form(app).fold_open(fold) {
            enter(app, WizardRow::Field(fold));
        }
    }
    let buttons = CreateForm::wizard_button_rows(step(app));
    let (target, rights) = match buttons.iter().position(|b| *b == row) {
        Some(i) => (buttons[0], i),
        None => (row, 0),
    };
    for _ in 0..200 {
        if focused(app) == target {
            break;
        }
        press(app, KeyCode::Down);
    }
    assert_eq!(focused(app), target, "the cursor reaches {target:?}");
    for _ in 0..rights {
        press(app, KeyCode::Right);
    }
    assert_eq!(focused(app), row);
}

/// The form's row a wizard row is, if it is one.
fn field_of(row: WizardRow) -> Option<CreateRow> {
    match row {
        WizardRow::Field(field) => Some(field),
        _ => None,
    }
}

/// ↵ on a row: pick a radio and go on, press a button.
fn enter(app: &mut App, row: WizardRow) {
    go(app, row);
    press(app, KeyCode::Enter);
}

/// Space on a row of the current step: tick a checkbox, walk the skills modes.
fn tick(app: &mut App, row: CreateRow) {
    go(app, WizardRow::Field(row));
    press(app, KeyCode::Char(' '));
}

/// Replace what a text row of the current step holds with `text`.
fn type_into(app: &mut App, row: CreateRow, text: &str) {
    go(app, WizardRow::Field(row));
    press(app, KeyCode::End);
    for _ in 0..100 {
        press(app, KeyCode::Backspace);
    }
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

/// Next until `to`, each step passing.
fn walk_to(app: &mut App, to: CreateStep) {
    for _ in 0..CreateStep::ALL.len() {
        if step(app) == to {
            break;
        }
        let from = step(app);
        enter(app, WizardRow::Next);
        assert_ne!(step(app), from, "Next leaves {from:?}: {:?}", app.status);
    }
    assert_eq!(step(app), to);
}

/// On to the review, press `Create only` and hand back the job it started.
fn create_only(app: &mut App) -> PendingJob {
    walk_to(app, CreateStep::Review);
    enter(app, WizardRow::CreateOnly);
    match app.modal.take() {
        Some(Modal::Running(_, pending)) => pending,
        _ => panic!(
            "a job was started; the status line says {:?}",
            app.status.as_ref().map(|(text, _, _)| text)
        ),
    }
}

fn steps(job: &PendingJob) -> Steps {
    job.steps
        .iter()
        .map(|s| (s.label.clone(), s.argv.clone(), s.stdin.is_some()))
        .collect()
}

/// Create on a form that must be refused — Ctrl-S, which creates from any step, since Next does not
/// get past the step owning the bad field — and check the exact reason.
fn refused(app: &mut App, message: &str) {
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(app.modal.is_none(), "no job was started");
    assert!(
        matches!(app.screen, Screen::Create(_)),
        "the form stays open"
    );
    let Some((text, err, _)) = &app.status else {
        panic!("a reason is shown")
    };
    assert!(*err, "shown as an error");
    assert_eq!(text.as_str(), message);
}

#[test]
fn g1_defaults() {
    let mut app = new_app();
    app.open_create();
    let job = create_only(&mut app);
    assert_eq!(
        steps(&job),
        vec![(
            "sbx create claude-litellm-proj".to_string(),
            strings(&[
                "sbx",
                "create",
                "--kit",
                &conn_kit(&app, "claude"),
                "--name",
                "claude-litellm-proj",
                "--skills",
                "readonly",
                "--cpus",
                "4",
                "-m",
                "4g",
                &kit(&app, "claude-litellm"),
                "/proj",
            ]),
            false,
        )]
    );
    assert_eq!(job.title, "Creating Claude Code · LiteLLM acme");
    // The connection the form opened on, remembered only once the create succeeded.
    assert_eq!(job.post.last_litellm.as_deref(), Some("acme"));
    let reg = job.post.register.expect("the sandbox is registered");
    assert_eq!(reg.name, "claude-litellm-proj");
    assert_eq!(reg.label, "Claude Code · LiteLLM acme");
    assert!(!reg.clone);
    assert_eq!(reg.skills, Some(sbx::SkillsMode::Readonly));
    assert_eq!(reg.start_dir, None);
    assert!(reg.static_mcp.is_empty());
}

#[test]
fn g2_opencode_options() {
    let mut app = new_app();
    app.open_create();
    choose(&mut app, Provider::Litellm);
    let row = agent(&app, "opencode");
    enter(&mut app, row);
    walk_to(&mut app, CreateStep::Kits);
    let row = addon(&app, "web");
    tick(&mut app, row);
    let row = addon(&app, "rtk");
    tick(&mut app, row);
    walk_to(&mut app, CreateStep::Workspace);
    type_into(&mut app, CreateRow::Name, "My box");
    tick(&mut app, CreateRow::Clone);
    // readonly → readwrite → off.
    tick(&mut app, CreateRow::Skills);
    tick(&mut app, CreateRow::Skills);
    type_into(&mut app, CreateRow::Ports, "8080:3000, 5173");
    type_into(&mut app, CreateRow::DenyNetwork, "api.example.com");
    let job = create_only(&mut app);
    assert_eq!(
        steps(&job),
        vec![(
            "sbx create opencode-litellm-proj".to_string(),
            strings(&[
                "sbx",
                "create",
                "--kit",
                &conn_kit(&app, "opencode"),
                "--kit",
                &kit(&app, "rtk"),
                "--kit",
                &kit(&app, "web"),
                "--name",
                "opencode-litellm-proj",
                "--clone",
                "-p",
                "8080:3000",
                "-p",
                "5173",
                "--deny-network",
                "api.example.com",
                "--skills",
                "off",
                "--cpus",
                "4",
                "-m",
                "4g",
                &kit(&app, "opencode-litellm"),
                "/proj",
            ]),
            false,
        )]
    );
    assert_eq!(job.title, "Creating My box");
    let reg = job.post.register.expect("the sandbox is registered");
    assert_eq!(reg.name, "opencode-litellm-proj");
    assert_eq!(reg.label, "My box");
    assert!(reg.clone);
    assert_eq!(reg.skills, Some(sbx::SkillsMode::Off));
    assert_eq!(reg.start_dir, None);
    assert!(reg.static_mcp.is_empty());
}

#[test]
fn g3_login() {
    let mut app = new_app();
    app.open_create();
    choose(&mut app, Provider::Anthropic);
    let row = agent(&app, "claude");
    enter(&mut app, row);
    enter(&mut app, WizardRow::Connection(ConnChoice::Login));
    let row = addon(&app, "jdk25");
    tick(&mut app, row);
    let job = create_only(&mut app);
    assert_eq!(
        steps(&job),
        vec![(
            "sbx create claude-anthropic-proj".to_string(),
            strings(&[
                "sbx",
                "create",
                "--kit",
                &kit(&app, "jdk25"),
                "--name",
                "claude-anthropic-proj",
                "--skills",
                "readonly",
                "--cpus",
                "4",
                "-m",
                "4g",
                &kit(&app, "claude-anthropic"),
                "/proj",
            ]),
            false,
        )]
    );
    assert_eq!(job.title, "Creating Claude Code · Anthropic /login (JDK25)");
    let reg = job.post.register.expect("the sandbox is registered");
    assert_eq!(reg.name, "claude-anthropic-proj");
    assert_eq!(reg.label, "Claude Code · Anthropic /login (JDK25)");
    assert!(!reg.clone);
    assert_eq!(reg.skills, Some(sbx::SkillsMode::Readonly));
    assert_eq!(reg.start_dir, None);
    assert!(reg.static_mcp.is_empty());
}

#[test]
fn g4_named_token() {
    let mut app = new_app();
    with_stored_token(&mut app, "private");
    app.open_create();
    choose(&mut app, Provider::Anthropic);
    // The only token on the host is what the radio picks, with the cursor on it; ↵ takes it.
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    enter(&mut app, WizardRow::Field(CreateRow::TokenPick(0)));
    let job = create_only(&mut app);
    assert_eq!(
        steps(&job),
        vec![(
            "sbx create claude-token-proj".to_string(),
            strings(&[
                "sbx",
                "create",
                "--kit",
                &path(&crate::tokens::kit_dir("private")),
                "--name",
                "claude-token-proj",
                "--skills",
                "readonly",
                "--cpus",
                "4",
                "-m",
                "4g",
                &kit(&app, "claude-token"),
                "/proj",
            ]),
            false,
        )]
    );
    assert_eq!(job.title, "Creating Claude Code · Anthropic token private");
    let reg = job.post.register.expect("the sandbox is registered");
    assert_eq!(reg.name, "claude-token-proj");
    assert_eq!(reg.label, "Claude Code · Anthropic token private");
    assert!(!reg.clone);
    assert_eq!(
        reg.skills,
        Some(sbx::SkillsMode::Readonly),
        "the v3 token workload declares where Claude Code reads skills"
    );
    assert_eq!(reg.start_dir, None);
    assert!(reg.static_mcp.is_empty());
}

#[test]
fn g5_new_token() {
    let existed = crate::tokens::kit_dir("personal").exists();
    let mut app = new_app();
    app.open_create();
    choose(&mut app, Provider::Anthropic);
    // No token on the host, so the radio means a new one.
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    type_into(&mut app, CreateRow::TokenName, "personal");
    type_into(&mut app, CreateRow::Token, TOKEN_VALUE);
    let job = create_only(&mut app);
    assert_eq!(
        steps(&job),
        vec![
            (
                "sbx secret set-custom (sbx-cs-aisbx-tok-personal)".to_string(),
                strings(&[
                    "sbx",
                    "secret",
                    "set-custom",
                    "--host",
                    "api.anthropic.com",
                    "--placeholder",
                    "sbx-cs-aisbx-tok-personal",
                    "--value-stdin",
                ]),
                true,
            ),
            (
                "sbx create claude-token-proj".to_string(),
                strings(&[
                    "sbx",
                    "create",
                    "--kit",
                    &path(&crate::tokens::kit_dir("personal")),
                    "--name",
                    "claude-token-proj",
                    "--skills",
                    "readonly",
                    "--cpus",
                    "4",
                    "-m",
                    "4g",
                    &kit(&app, "claude-token"),
                    "/proj",
                ]),
                false,
            ),
        ]
    );
    assert_eq!(job.steps[0].stdin.as_deref(), Some(TOKEN_VALUE));
    assert!(
        job.steps
            .iter()
            .all(|s| s.argv.iter().all(|a| !a.contains(TOKEN_VALUE))),
        "the token goes in on stdin, never in an argv"
    );
    assert_eq!(job.title, "Creating Claude Code · Anthropic token personal");
    let reg = job.post.register.expect("the sandbox is registered");
    assert_eq!(reg.name, "claude-token-proj");
    assert_eq!(reg.label, "Claude Code · Anthropic token personal");
    assert!(!reg.clone);
    assert_eq!(reg.skills, Some(sbx::SkillsMode::Readonly));
    assert_eq!(reg.start_dir, None);
    assert!(reg.static_mcp.is_empty());
    if !existed {
        crate::tokens::remove_kit("personal").unwrap();
    }
}

#[test]
fn g6_mcp() {
    let mut app = new_app();
    app.mcp = vec![sbx::McpServer {
        name: "notion".into(),
        status: "ready".into(),
        ..sbx::McpServer::default()
    }];
    app.open_create();
    walk_to(&mut app, CreateStep::Workspace);
    tick(&mut app, CreateRow::Mcp(0));
    let job = create_only(&mut app);
    assert_eq!(
        steps(&job),
        vec![(
            "sbx create claude-litellm-proj".to_string(),
            strings(&[
                "sbx",
                "create",
                "--kit",
                &conn_kit(&app, "claude"),
                "--name",
                "claude-litellm-proj",
                "--static-mcp",
                "notion",
                "--skills",
                "readonly",
                "--cpus",
                "4",
                "-m",
                "4g",
                &kit(&app, "claude-litellm"),
                "/proj",
            ]),
            false,
        )]
    );
    assert_eq!(job.title, "Creating Claude Code · LiteLLM acme");
    // The connection the form opened on, remembered only once the create succeeded.
    assert_eq!(job.post.last_litellm.as_deref(), Some("acme"));
    let reg = job.post.register.expect("the sandbox is registered");
    assert_eq!(reg.name, "claude-litellm-proj");
    assert_eq!(reg.label, "Claude Code · LiteLLM acme");
    assert!(!reg.clone);
    assert_eq!(reg.skills, Some(sbx::SkillsMode::Readonly));
    assert_eq!(reg.start_dir, None);
    assert_eq!(reg.static_mcp, vec!["notion".to_string()]);
}

#[test]
fn g6_no_mcp() {
    let mut app = new_app();
    app.mcp = vec![sbx::McpServer {
        name: "notion".into(),
        status: "ready".into(),
        ..sbx::McpServer::default()
    }];
    app.open_create();
    walk_to(&mut app, CreateStep::Workspace);
    tick(&mut app, CreateRow::Mcp(0));
    tick(&mut app, CreateRow::McpNone);
    let job = create_only(&mut app);
    let argv = &steps(&job)[0].1;
    let at = argv
        .iter()
        .position(|a| a == "--static-mcp")
        .expect("a fixed set");
    assert_eq!(argv[at + 1], "none");
    assert_eq!(
        argv.iter().filter(|a| *a == "--static-mcp").count(),
        1,
        "sbx refuses none beside a server: {argv:?}"
    );
    assert!(!argv.contains(&"notion".to_string()), "{argv:?}");
    let reg = job.post.register.expect("the sandbox is registered");
    assert_eq!(reg.static_mcp, vec!["none".to_string()]);
}

#[test]
fn g7_start_dir() {
    // The picker walks the real tree, so the workspace has to exist: `proj` with one folder.
    let base = std::env::temp_dir().join(format!("aisbx-golden-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("proj");
    std::fs::create_dir_all(root.join("sub")).unwrap();

    let mut app = new_app();
    app.cwd = root.clone();
    app.open_create();
    walk_to(&mut app, CreateStep::Workspace);
    enter(&mut app, WizardRow::Field(CreateRow::StartDir));
    assert!(
        matches!(app.screen, Screen::StartDir(_)),
        "the picker is open"
    );
    // The cursor opens on `sub`: → steps into it and lands on `..`, ↓ on *Use*, ↵ takes it.
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(app.screen, Screen::Create(_)),
        "back on the create form"
    );
    assert_eq!(
        step(&app),
        CreateStep::Workspace,
        "on the step it was opened from"
    );
    let job = create_only(&mut app);
    assert_eq!(
        steps(&job),
        vec![(
            "sbx create claude-litellm-proj".to_string(),
            strings(&[
                "sbx",
                "create",
                "--kit",
                &conn_kit(&app, "claude"),
                "--name",
                "claude-litellm-proj",
                "--skills",
                "readonly",
                "--cpus",
                "4",
                "-m",
                "4g",
                &kit(&app, "claude-litellm"),
                &path(&root),
            ]),
            false,
        )]
    );
    assert_eq!(job.title, "Creating Claude Code · LiteLLM acme");
    // The connection the form opened on, remembered only once the create succeeded.
    assert_eq!(job.post.last_litellm.as_deref(), Some("acme"));
    let reg = job.post.register.expect("the sandbox is registered");
    assert_eq!(reg.name, "claude-litellm-proj");
    assert_eq!(reg.label, "Claude Code · LiteLLM acme");
    assert!(!reg.clone);
    assert_eq!(reg.skills, Some(sbx::SkillsMode::Readonly));
    assert_eq!(reg.start_dir, Some(PathBuf::from("sub")));
    assert!(reg.static_mcp.is_empty());
    let _ = std::fs::remove_dir_all(&base);
}

/// A host folder for this sandbox alone, picked with the keys: ↵ on *mount a folder…* opens the
/// picker on the folder above the workspace, → goes into `data`, and its first button adds it,
/// read-only. sbx gets it after the workspace, the generated `folders-<sandbox>` kit names it for
/// the agent, and the create records it for a rebuild.
#[test]
fn g7b_more_folders() {
    let base = std::env::temp_dir().join(format!("aisbx-golden-more-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    for d in ["data", "proj-more"] {
        std::fs::create_dir_all(base.join(d)).unwrap();
    }
    let base = crate::sbx::plain_path(std::fs::canonicalize(&base).unwrap());
    let (workspace, data) = (base.join("proj-more"), base.join("data"));
    let spec = format!("{}:ro", path(&data));

    let mut app = new_app();
    app.cwd = workspace.clone();
    app.open_create();
    walk_to(&mut app, CreateStep::Workspace);
    enter(&mut app, WizardRow::Field(CreateRow::FolderAdd));
    let Screen::StartDir(picker) = &app.screen else {
        panic!("↵ opens the folder picker")
    };
    assert_eq!(picker.here(), base, "on the folder above the workspace");
    assert_eq!(picker.dirs, ["data", "proj-more"]);
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert_eq!(form(&app).workspace.extra_folders, [spec.as_str()]);
    assert_eq!(focused(&app), WizardRow::Field(CreateRow::Folder(0)));
    let job = create_only(&mut app);
    assert_eq!(
        steps(&job),
        vec![(
            "sbx create claude-litellm-proj-more".to_string(),
            strings(&[
                "sbx",
                "create",
                "--kit",
                &conn_kit(&app, "claude"),
                "--kit",
                &path(&crate::folders::kit_dir("claude-litellm-proj-more")),
                "--name",
                "claude-litellm-proj-more",
                "--skills",
                "readonly",
                "--cpus",
                "4",
                "-m",
                "4g",
                &kit(&app, "claude-litellm"),
                &path(&workspace),
                &spec,
            ]),
            false,
        )]
    );
    let reg = job.post.register.expect("the sandbox is registered");
    assert_eq!(reg.extra_folders, [spec]);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn g8_refusals() {
    let mut app = new_app();
    app.open_create();
    walk_to(&mut app, CreateStep::Workspace);
    type_into(&mut app, CreateRow::Ports, "8080:not-a-port");
    refused(&mut app, BAD_PORT);

    let mut app = new_app();
    app.open_create();
    walk_to(&mut app, CreateStep::Workspace);
    type_into(&mut app, CreateRow::DenyNetwork, "example.com bad!host");
    refused(&mut app, BAD_HOST);

    let mut app = new_app();
    app.open_create();
    choose(&mut app, Provider::Anthropic);
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    type_into(&mut app, CreateRow::TokenName, "Bad Name");
    type_into(&mut app, CreateRow::Token, TOKEN_VALUE);
    refused(&mut app, BAD_NAME);

    let mut app = new_app();
    with_stored_token(&mut app, "private");
    app.open_create();
    choose(&mut app, Provider::Anthropic);
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    tick(&mut app, CreateRow::TokenNew);
    type_into(&mut app, CreateRow::TokenName, "private");
    type_into(&mut app, CreateRow::Token, TOKEN_VALUE);
    refused(&mut app, TAKEN);

    let mut app = new_app();
    app.open_create();
    choose(&mut app, Provider::Anthropic);
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    type_into(&mut app, CreateRow::TokenName, "personal");
    refused(&mut app, NO_VALUE);
}

/// Not a real key either: the checks only need a value to look for in every argv.
const GATEWAY_KEY: &str = "golden-test-gateway-value";

/// *new connection…*, picked with → on the Connection row and typed in: the key is stored first,
/// on stdin; nothing is written until that succeeded (the post-job carries the connection), and
/// the create on the new connection's kit follows it.
#[test]
fn g9_new_connection() {
    let mut app = new_app();
    app.litellm_root = crate::testkit::litellm_root_with(&[crate::testkit::acme()]);
    app.open_create();
    walk_to(&mut app, CreateStep::Connection);
    go(&mut app, WizardRow::Field(CreateRow::LitellmConn));
    press(&mut app, KeyCode::Right);
    type_into(&mut app, CreateRow::LitellmName, "beta");
    type_into(
        &mut app,
        CreateRow::LitellmUrl,
        "https://gw.example.org/litellm",
    );
    type_into(&mut app, CreateRow::LitellmKey, GATEWAY_KEY);
    // Ctrl-S from the Connection step: the key, then the create, with no Models step between.
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    let job = match app.modal.take() {
        Some(Modal::Running(_, pending)) => pending,
        _ => panic!("a job was started: {:?}", app.status),
    };
    assert_eq!(
        steps(&job),
        vec![(
            "sbx secret set-custom (sbx-cs-aisbx-llm-beta)".to_string(),
            strings(&[
                "sbx",
                "secret",
                "set-custom",
                "--host",
                "gw.example.org",
                "--placeholder",
                "sbx-cs-aisbx-llm-beta",
                "--value-stdin",
            ]),
            true,
        )]
    );
    assert_eq!(job.steps[0].stdin.as_deref(), Some(GATEWAY_KEY));
    assert!(
        !app.litellm_root.join("beta").exists(),
        "nothing is written before the key is stored"
    );
    let (conn, agent) = job.post.save_connection.clone().expect("saved afterwards");
    assert_eq!(agent, Some(crate::litellm::Agent::Claude));
    assert_eq!(conn.url, "https://gw.example.org/litellm");
    let Some(crate::app::Restore::Create(restore)) = job.post.restore.as_ref() else {
        panic!("the form comes back on a failure")
    };
    assert_eq!(restore.connection.new_conn.name.value, "beta");
    assert!(
        restore.connection.new_conn.key.value.is_empty(),
        "all but the key"
    );

    let create = job.post.then.as_deref().expect("the create follows");
    // Once the key is stored the connection exists: a failed create comes back on it.
    let Some(crate::app::Restore::Create(after)) = create.post.restore.as_ref() else {
        panic!("the form comes back on a failed create too")
    };
    assert_eq!(after.connection.litellm.as_deref(), Some("beta"));
    assert!(after
        .connection
        .connections
        .iter()
        .any(|c| c.name == "beta"));
    assert!(after.connection.new_conn.key.value.is_empty());
    assert_eq!(after.wizard_step(), CreateStep::Review);
    assert!(
        create.post.defaults.is_some(),
        "a create keeps its answers as defaults"
    );
    let beta_kit = app.litellm_root.join("beta").join("litellm-beta-claude");
    assert_eq!(
        steps(create),
        vec![(
            "sbx create claude-litellm-proj".to_string(),
            strings(&[
                "sbx",
                "create",
                "--kit",
                &path(&beta_kit),
                "--name",
                "claude-litellm-proj",
                "--skills",
                "readonly",
                "--cpus",
                "4",
                "-m",
                "4g",
                &kit(&app, "claude-litellm"),
                "/proj",
            ]),
            false,
        )]
    );
    assert_eq!(create.post.last_litellm.as_deref(), Some("beta"));
    assert!(
        [&job, create]
            .iter()
            .flat_map(|j| &j.steps)
            .all(|s| s.argv.iter().all(|a| !a.contains(GATEWAY_KEY))),
        "the key goes in on stdin, never in an argv"
    );
    let _ = std::fs::remove_dir_all(&app.litellm_root);
}

/// Next on the Connection step with *new connection…*: the key is stored and the connection
/// written first, then its model list is fetched, and the form comes back on the Harness step
/// with the connection picked — on the Connection step, its key emptied, when storing the key
/// fails.
#[test]
fn g9b_next_stores_a_new_connection_and_fetches_its_models() {
    let mut app = new_app();
    app.litellm_root = crate::testkit::litellm_root_with(&[crate::testkit::acme()]);
    app.open_create();
    walk_to(&mut app, CreateStep::Connection);
    go(&mut app, WizardRow::Field(CreateRow::LitellmConn));
    press(&mut app, KeyCode::Right);
    type_into(&mut app, CreateRow::LitellmName, "beta");
    type_into(&mut app, CreateRow::LitellmUrl, "https://gw.example.org");
    type_into(&mut app, CreateRow::LitellmKey, GATEWAY_KEY);
    enter(&mut app, WizardRow::Next);
    let job = match app.modal.take() {
        Some(Modal::Running(_, pending)) => pending,
        _ => panic!("a job was started: {:?}", app.status),
    };
    assert_eq!(job.steps.len(), 1);
    assert_eq!(
        job.steps[0].label,
        "sbx secret set-custom (sbx-cs-aisbx-llm-beta)"
    );
    assert_eq!(job.steps[0].stdin.as_deref(), Some(GATEWAY_KEY));
    assert!(
        !app.litellm_root.join("beta").exists(),
        "nothing is written before the key is stored"
    );
    let Some(crate::app::Restore::Create(failed)) = job.post.restore.as_ref() else {
        panic!("the form comes back on a failure")
    };
    assert_eq!(failed.wizard_step(), CreateStep::Connection);
    assert!(failed.connection.new_conn.key.value.is_empty());

    let fetch = job.post.then.as_deref().expect("the fetch follows");
    assert_eq!(fetch.title, "Fetching the models of beta");
    let names: Vec<&str> = fetch.steps.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(
        names,
        [
            "sbx create aisbx-models-beta",
            "fetch the model list of beta"
        ]
    );
    assert_eq!(
        fetch.post.harvest_models,
        Some(("aisbx-models-beta".to_string(), "beta".to_string()))
    );
    for back in [&fetch.post.resume, &fetch.post.restore] {
        let Some(crate::app::Restore::Create(form)) = back.as_ref() else {
            panic!("the form comes back after the fetch, whatever its outcome")
        };
        assert_eq!(form.wizard_step(), CreateStep::Harness);
        assert_eq!(form.connection.litellm.as_deref(), Some("beta"));
        assert!(form.connection.new_conn.key.value.is_empty());
    }
    assert!(
        [&job, fetch]
            .iter()
            .flat_map(|j| &j.steps)
            .all(|s| s.argv.iter().all(|a| !a.contains(GATEWAY_KEY))),
        "the key goes in on stdin, never in an argv"
    );
    let _ = std::fs::remove_dir_all(&app.litellm_root);
}

/// *new connection...* is refused on the Connection step for each field that fails a save's
/// checks, for a name that exists and for an empty key.
#[test]
fn g10_new_connection_refusals() {
    let cases: [(&str, &str, &str, &str); 6] = [
        (
            "Bad_Name",
            "https://h",
            GATEWAY_KEY,
            "name the connection in a–z, 0–9 and “-” (at most 32)",
        ),
        (
            "acme",
            "https://h",
            GATEWAY_KEY,
            "connection acme exists — pick it in the list",
        ),
        (
            "beta",
            "http://x",
            GATEWAY_KEY,
            "the URL must start with https://",
        ),
        (
            "beta",
            "https://h:4000",
            GATEWAY_KEY,
            "only port 443 for now",
        ),
        (
            "beta",
            "https://h/a b",
            GATEWAY_KEY,
            "the URL's path may hold only letters, digits and “._~/-”",
        ),
        (
            "beta",
            "https://h",
            "",
            "paste the gateway's key — it is stored in the host keychain",
        ),
    ];
    for (name, url, key, message) in cases {
        let mut app = new_app();
        app.open_create();
        walk_to(&mut app, CreateStep::Connection);
        go(&mut app, WizardRow::Field(CreateRow::LitellmConn));
        press(&mut app, KeyCode::Left);
        type_into(&mut app, CreateRow::LitellmName, name);
        type_into(&mut app, CreateRow::LitellmUrl, url);
        type_into(&mut app, CreateRow::LitellmKey, key);
        refused(&mut app, message);
        // Next is refused on the step that owns the field too.
        enter(&mut app, WizardRow::Next);
        assert_eq!(step(&app), CreateStep::Connection, "{message}");
    }
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// The wizard's pages from any row: PgDn is Next, PgUp is Back but never off step 1, and a text
/// row leaves both alone. A refused PgDn stays, says why, and marks the step's fields.
#[test]
fn page_keys_walk_the_steps_from_any_row() {
    let mut app = new_app();
    app.open_create();
    press(&mut app, KeyCode::PageUp);
    assert_eq!(step(&app), CreateStep::Provider, "nothing before step 1");
    for want in [
        CreateStep::Connection,
        CreateStep::Harness,
        CreateStep::Models,
        CreateStep::Kits,
        CreateStep::Workspace,
        CreateStep::Review,
    ] {
        press(&mut app, KeyCode::PageDown);
        assert_eq!(step(&app), want);
    }
    press(&mut app, KeyCode::PageDown);
    assert_eq!(step(&app), CreateStep::Review, "nothing after the review");
    press(&mut app, KeyCode::PageUp);
    type_into(&mut app, CreateRow::Ports, "8080");
    press(&mut app, KeyCode::PageUp);
    assert_eq!(step(&app), CreateStep::Kits, "from a text row too");
    assert_eq!(form(&app).workspace.ports.value, "8080", "nothing typed");

    press(&mut app, KeyCode::PageDown);
    type_into(&mut app, CreateRow::Ports, "8080:not-a-port");
    assert!(!form(&app).wizard.refused);
    press(&mut app, KeyCode::PageDown);
    assert_eq!(step(&app), CreateStep::Workspace, "refused");
    assert_eq!(app.status.as_ref().map(|s| s.0.as_str()), Some(BAD_PORT));
    assert!(form(&app).wizard.refused);
    press(&mut app, KeyCode::PageUp);
    assert!(!form(&app).wizard.refused, "cleared on another step");
}

/// The trail's numbers jump from any row that takes no typing, forward only past steps whose
/// answers pass; on a text row they are typed.
#[test]
fn digits_jump_to_a_step_unless_a_field_takes_them() {
    let mut app = new_app();
    app.open_create();
    for (key, want) in [
        ('5', CreateStep::Kits),
        ('1', CreateStep::Provider),
        ('4', CreateStep::Models),
        ('2', CreateStep::Connection),
        ('3', CreateStep::Harness),
        ('7', CreateStep::Review),
        ('6', CreateStep::Workspace),
    ] {
        press(&mut app, KeyCode::Char(key));
        assert_eq!(step(&app), want, "{key}");
    }
    go(&mut app, WizardRow::Field(CreateRow::Ports));
    press(&mut app, KeyCode::Char('7'));
    assert_eq!(step(&app), CreateStep::Workspace, "typed, not jumped");
    assert_eq!(form(&app).workspace.ports.value, "7");

    type_into(&mut app, CreateRow::Ports, "8080:not-a-port");
    go(&mut app, WizardRow::Field(CreateRow::Clone));
    press(&mut app, KeyCode::Char('7'));
    assert_eq!(step(&app), CreateStep::Workspace, "stopped on the bad step");
    assert_eq!(app.status.as_ref().map(|s| s.0.as_str()), Some(BAD_PORT));
}

/// Ctrl-C and Esc on step 1 close the wizard and keep its answers: the next New in this folder
/// brings them back where they were. Cancel drops them, and another folder starts afresh.
#[test]
fn an_interrupted_wizard_comes_back_on_the_next_new() {
    let mut app = new_app();
    app.open_create();
    walk_to(&mut app, CreateStep::Workspace);
    type_into(&mut app, CreateRow::Name, "keep me");
    app.on_key(ctrl('c'));
    assert!(matches!(app.screen, Screen::List), "Ctrl-C closes it");
    app.open_create();
    assert_eq!(step(&app), CreateStep::Workspace, "where it was");
    assert_eq!(form(&app).workspace.name.value, "keep me");
    let said = app.status.as_ref().map(|s| (s.0.as_str(), s.1));
    assert_eq!(said, Some((crate::create::DRAFT_BACK, false)));

    for _ in 0..5 {
        press(&mut app, KeyCode::PageUp);
    }
    press(&mut app, KeyCode::Esc);
    assert!(
        matches!(app.screen, Screen::List),
        "Esc on step 1 closes it"
    );
    app.open_create();
    assert_eq!(form(&app).workspace.name.value, "keep me", "kept again");

    // Another folder starts afresh and leaves this folder's answers where they are.
    app.on_key(ctrl('c'));
    app.cwd = PathBuf::from("/elsewhere");
    app.open_create();
    assert_ne!(form(&app).workspace.name.value, "keep me");
    enter(&mut app, WizardRow::Cancel);
    app.cwd = PathBuf::from("/proj");
    app.open_create();
    assert_eq!(form(&app).workspace.name.value, "keep me");

    enter(&mut app, WizardRow::Cancel);
    assert!(app.create_draft.is_none(), "Cancel drops them");
    app.open_create();
    assert_eq!(step(&app), CreateStep::Provider);
    assert_ne!(form(&app).workspace.name.value, "keep me");
}

/// A create that fails gives the form back on its review, as typed but for the pasted token.
#[test]
fn a_failed_create_brings_the_form_back_without_its_secret() {
    let mut app = new_app();
    app.open_create();
    choose(&mut app, Provider::Anthropic);
    let claude = agent(&app, "claude");
    enter(&mut app, claude);
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    type_into(&mut app, CreateRow::TokenName, "personal");
    type_into(&mut app, CreateRow::Token, TOKEN_VALUE);
    walk_to(&mut app, CreateStep::Workspace);
    type_into(&mut app, CreateRow::Name, "mine");
    let job = create_only(&mut app);
    let Some(crate::app::Restore::Create(back)) = job.post.restore.as_ref() else {
        panic!("the form comes back on a failure")
    };
    assert_eq!(back.wizard_step(), CreateStep::Review);
    assert_eq!(back.workspace.name.value, "mine");
    assert_eq!(back.connection.token_name.value, "personal");
    assert!(back.connection.token.value.is_empty(), "never the token");
}

/// A click on one value of a choice row picks that value and puts the cursor on the row: a skills
/// mode, a connection, *new connection…* — whose pins go with the connection they were made on.
/// A stored connection with its key goes on to the next step.
#[test]
fn a_click_on_a_choice_picks_that_value() {
    let mut app = new_app();
    let beta = crate::testkit::connection("beta", "https://beta.example.org");
    app.litellm_root = crate::testkit::litellm_root_with(&[crate::testkit::acme(), beta]);
    app.open_create();
    walk_to(&mut app, CreateStep::Workspace);
    let rows = form(&app).wizard_rows(&app.catalog);
    let skills = rows
        .iter()
        .position(|r| *r == WizardRow::Field(CreateRow::Skills))
        .expect("the skills row");
    assert_eq!(form(&app).choices(CreateRow::Skills).len(), 3);
    app.click(crate::app::Hit::FormChoice(skills, 2));
    assert_eq!(form(&app).workspace.skills, sbx::SkillsMode::Readwrite);
    assert_eq!(form(&app).chosen(CreateRow::Skills), Some(2));
    assert_eq!(focused(&app), WizardRow::Field(CreateRow::Skills));
    app.click(crate::app::Hit::FormChoice(skills, 9));
    assert_eq!(
        form(&app).workspace.skills,
        sbx::SkillsMode::Readwrite,
        "no such value"
    );

    press(&mut app, KeyCode::Char('2'));
    let rows = form(&app).wizard_rows(&app.catalog);
    let conn = rows
        .iter()
        .position(|r| *r == WizardRow::Field(CreateRow::LitellmConn))
        .expect("the connection row");
    let choices = form(&app).choices(CreateRow::LitellmConn);
    assert_eq!(choices, ["acme", "beta", crate::create::NEW_CONNECTION]);
    if let Screen::Create(f) = &mut app.screen {
        f.connection
            .models
            .set(crate::models::Slot::Opus, Some("m-1".into()));
    }
    app.click(crate::app::Hit::FormChoice(conn, 1));
    assert_eq!(form(&app).connection.litellm.as_deref(), Some("beta"));
    assert_eq!(
        form(&app).connection.models,
        Default::default(),
        "pins dropped"
    );
    assert_eq!(
        step(&app),
        CreateStep::Connection,
        "key missing: stays, for its warning"
    );
    app.click(crate::app::Hit::FormChoice(conn, 2));
    assert_eq!(form(&app).connection.litellm, None, "new connection…");
    assert_eq!(form(&app).chosen(CreateRow::LitellmConn), Some(2));
    assert_eq!(step(&app), CreateStep::Connection, "its fields come next");
    app.click(crate::app::Hit::FormChoice(conn, 0));
    assert_eq!(form(&app).connection.litellm.as_deref(), Some("acme"));
    assert_eq!(
        step(&app),
        CreateStep::Harness,
        "key stored: on to the next step"
    );
}
