//! Golden tests for the create wizard. For each answer set of `create_golden.rs`, the wizard,
//! driven by keys and clicks, starts the same job as a form with those answers set on it, and
//! refuses with the same messages. Plus the wizard's own navigation.

use std::path::PathBuf;

use crate::app::{
    AccountChoice, App, CreateForm, CreateRow, Hit, Modal, PendingJob, Screen, TextInput,
};
use crate::create::{ConnChoice, CreateStep, WizardRow};
use crate::input::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use crate::kits::{Catalog, Provider};
use crate::sbx;

/// A step as the job runner sees it: label, argv, and what goes in on stdin.
type Steps = Vec<(String, Vec<String>, Option<String>)>;

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
            aisbx_core::testkit::litellm_key("acme", "litellm.example.com"),
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

fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::from(code));
}

fn ctrl_s(app: &mut App) {
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
}

/// Draw the screen at 110×40 and click where `want` landed.
fn click(app: &mut App, want: Hit) {
    crate::ui::tests_support::render_at(app, 110, 40);
    let rect = app
        .hits
        .iter()
        .rev()
        .find(|(_, hit)| *hit == want)
        .map(|(rect, _)| *rect)
        .unwrap_or_else(|| panic!("{want:?} is drawn"));
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    });
}

fn form(app: &App) -> &CreateForm {
    match &app.screen {
        Screen::Create(form) => form,
        _ => panic!("the create form is open"),
    }
}

fn backend(app: &App, name: &str) -> usize {
    let index = app.catalog.backends.iter().position(|b| b.kit.name == name);
    index.unwrap_or_else(|| panic!("the checkout ships {name}"))
}

fn addon(app: &App, name: &str) -> usize {
    let index = app.catalog.addons.iter().position(|a| a.kit.name == name);
    index.unwrap_or_else(|| panic!("the checkout ships {name}"))
}

/// The job the last key started.
fn started(app: &mut App) -> PendingJob {
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
        .map(|s| (s.label.clone(), s.argv.clone(), s.stdin.clone()))
        .collect()
}

/// The two jobs are the same: every step, the title, and everything recorded afterwards.
/// `post.restore` holds each path's own form, so only its presence is compared.
fn assert_same_job(wizard: &PendingJob, answered: &PendingJob) {
    assert_eq!(steps(wizard), steps(answered));
    assert_eq!(wizard.title, answered.title);
    let recorded = |job: &PendingJob| {
        let post = crate::app::PostJob {
            restore: None,
            ..job.post.clone()
        };
        format!("{post:?}")
    };
    assert_eq!(recorded(wizard), recorded(answered));
    assert!(wizard.post.restore.is_some() && answered.post.restore.is_some());
    assert!(wizard.fallback.is_none() && answered.fallback.is_none());
}

fn error(app: &App) -> String {
    let Some((text, err, _)) = &app.status else {
        panic!("a reason is shown")
    };
    assert!(*err, "shown as an error");
    text.clone()
}

// --- the wizard ---------------------------------------------------------------------------------

fn step(app: &App) -> CreateStep {
    form(app).wizard_step()
}

fn rows(app: &App) -> Vec<WizardRow> {
    form(app).wizard_rows(&app.catalog)
}

fn focused(app: &App) -> WizardRow {
    let rows = rows(app);
    rows[form(app).wizard_focus().min(rows.len() - 1)]
}

fn open(app: &mut App) {
    press(app, KeyCode::Char('n'));
    assert_eq!(step(app), CreateStep::Provider);
}

/// The provider row for `provider` on step 1.
fn provider(app: &App, provider: Provider) -> WizardRow {
    let providers = form(app).providers(&app.catalog);
    let index = providers.iter().position(|p| *p == provider);
    WizardRow::Provider(index.unwrap_or_else(|| panic!("{provider:?} is offered")))
}

/// ↵ on `p` on step 1: picked, and on to the Harness step — past the Connection step, on the
/// connection it opens on, for LiteLLM.
fn choose(app: &mut App, p: Provider) {
    let row = provider(app, p);
    enter(app, row);
    if p == Provider::Litellm {
        assert_eq!(step(app), CreateStep::Connection);
        next(app);
    }
    assert_eq!(step(app), CreateStep::Harness);
}

/// The harness row for an agent.
fn agent(app: &App, agent: &str) -> WizardRow {
    let harnesses = form(app).harnesses(&app.catalog);
    let index = harnesses.iter().position(|h| h.agent == agent);
    WizardRow::Agent(index.unwrap_or_else(|| panic!("{agent} is offered")))
}

/// Walk the cursor to `row` on the current step: ↓ to reach it, → along the button line.
fn go(app: &mut App, row: WizardRow) {
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

fn enter(app: &mut App, row: WizardRow) {
    go(app, row);
    press(app, KeyCode::Enter);
}

fn tick(app: &mut App, row: WizardRow) {
    go(app, row);
    press(app, KeyCode::Char(' '));
}

/// Replace what the focused text row holds with `text`.
fn replace_text(app: &mut App, text: &str) {
    press(app, KeyCode::End);
    for _ in 0..100 {
        press(app, KeyCode::Backspace);
    }
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn type_into(app: &mut App, row: CreateRow, text: &str) {
    go(app, WizardRow::Field(row));
    replace_text(app, text);
}

fn next(app: &mut App) {
    enter(app, WizardRow::Next);
}

/// Next until `to`, each step passing.
fn walk_to(app: &mut App, to: CreateStep) {
    for _ in 0..CreateStep::ALL.len() {
        if step(app) == to {
            break;
        }
        let from = step(app);
        next(app);
        assert_ne!(step(app), from, "Next leaves {from:?}: {:?}", app.status);
    }
    assert_eq!(step(app), to);
}

/// From wherever the wizard is: on to the review, then `Create only`.
fn create_only(app: &mut App) -> PendingJob {
    walk_to(app, CreateStep::Review);
    enter(app, WizardRow::CreateOnly);
    started(app)
}

/// The job a new form starts when `answer` sets its answers through the form's own methods, not
/// keys. `button` is the review step's button that creates.
fn answered(
    app: &mut App,
    button: WizardRow,
    answer: impl FnOnce(&Catalog, &mut CreateForm),
) -> PendingJob {
    open(app);
    let catalog = std::sync::Arc::clone(&app.catalog);
    let Screen::Create(form) = &mut app.screen else {
        panic!("the create form is open")
    };
    answer(&catalog, form);
    form.wizard_set_step(&catalog, CreateStep::Review);
    enter(app, button);
    started(app)
}

/// Space on a row, as a method call: a tick, with the label regenerated the way a key does it.
fn space(catalog: &Catalog, form: &mut CreateForm, row: CreateRow) {
    form.field_key(catalog, row, KeyEvent::from(KeyCode::Char(' ')));
}

/// A label typed by hand, which stops it following the kits.
fn type_label(form: &mut CreateForm, text: &str) {
    form.workspace.name = TextInput::new(text);
    form.workspace.name_touched = true;
}

// --- g1–g8's answer sets -------------------------------------------------------------------------

#[test]
fn w1_defaults() {
    let mut app = new_app();
    open(&mut app);
    let wizard = create_only(&mut app);

    let expected = answered(&mut new_app(), WizardRow::CreateOnly, |_, _| {});
    assert_same_job(&wizard, &expected);
    assert_eq!(wizard.title, "Creating Claude Code LiteLLM");
}

#[test]
fn w2_opencode_options() {
    let mut app = new_app();
    open(&mut app);
    choose(&mut app, Provider::Litellm);
    let row = agent(&app, "opencode");
    enter(&mut app, row);
    assert_eq!(step(&app), CreateStep::Models);
    next(&mut app);
    let web = WizardRow::Field(CreateRow::Addon(addon(&app, "web")));
    tick(&mut app, web);
    let rtk = WizardRow::Field(CreateRow::Addon(addon(&app, "rtk")));
    tick(&mut app, rtk);
    next(&mut app);
    type_into(&mut app, CreateRow::Name, "My box");
    tick(&mut app, WizardRow::Field(CreateRow::Clone));
    // readonly → readwrite → off.
    tick(&mut app, WizardRow::Field(CreateRow::Skills));
    tick(&mut app, WizardRow::Field(CreateRow::Skills));
    type_into(&mut app, CreateRow::Ports, "8080:3000, 5173");
    type_into(&mut app, CreateRow::DenyNetwork, "api.example.com");
    let wizard = create_only(&mut app);

    let mut app = new_app();
    let (opencode, web, rtk) = (
        backend(&app, "opencode-litellm"),
        addon(&app, "web"),
        addon(&app, "rtk"),
    );
    let expected = answered(&mut app, WizardRow::CreateOnly, |catalog, form| {
        let harnesses = form.harnesses(catalog);
        let h = harnesses.iter().position(|h| h.backends == [opencode]);
        form.pick_harness(catalog, h.expect("OpenCode on LiteLLM"));
        space(catalog, form, CreateRow::Addon(web));
        space(catalog, form, CreateRow::Addon(rtk));
        type_label(form, "My box");
        form.workspace.clone = true;
        form.workspace.skills = sbx::SkillsMode::Off;
        form.workspace.ports = TextInput::new("8080:3000, 5173");
        form.workspace.deny_network = TextInput::new("api.example.com");
    });
    assert_same_job(&wizard, &expected);
    assert_eq!(wizard.title, "Creating My box");
}

#[test]
fn w3_login() {
    let mut app = new_app();
    open(&mut app);
    choose(&mut app, Provider::Anthropic);
    enter(&mut app, WizardRow::Connection(ConnChoice::Login));
    assert_eq!(step(&app), CreateStep::Kits);
    let jdk = WizardRow::Field(CreateRow::Addon(addon(&app, "jdk25")));
    tick(&mut app, jdk);
    let wizard = create_only(&mut app);

    let mut app = new_app();
    let jdk = addon(&app, "jdk25");
    let expected = answered(&mut app, WizardRow::CreateOnly, |catalog, form| {
        form.pick_connection(catalog, ConnChoice::Login);
        space(catalog, form, CreateRow::Addon(jdk));
    });
    assert_same_job(&wizard, &expected);
    assert_eq!(wizard.title, "Creating Claude Code (JDK25)");
}

#[test]
fn w4_named_token() {
    let mut app = new_app();
    with_stored_token(&mut app, "private");
    open(&mut app);
    choose(&mut app, Provider::Anthropic);
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    assert_eq!(step(&app), CreateStep::Harness);
    assert_eq!(focused(&app), WizardRow::Field(CreateRow::TokenPick(0)));
    // ↵ on an existing token picks it and goes on.
    press(&mut app, KeyCode::Enter);
    assert_eq!(step(&app), CreateStep::Kits);
    let wizard = create_only(&mut app);

    let mut app = new_app();
    with_stored_token(&mut app, "private");
    let expected = answered(&mut app, WizardRow::CreateOnly, |catalog, form| {
        form.pick_connection(catalog, ConnChoice::Token);
    });
    assert_same_job(&wizard, &expected);
    assert_eq!(wizard.steps.len(), 1, "an existing token stores nothing");
}

#[test]
fn w5_new_token() {
    // Not g5's name: the two run in parallel, and each removes the kit it made.
    let existed = crate::tokens::kit_dir("wizard-golden").exists();
    let mut app = new_app();
    open(&mut app);
    choose(&mut app, Provider::Anthropic);
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    // No token on the host: the radio means a new one, and the cursor is on its rows.
    assert_eq!(focused(&app), WizardRow::Field(CreateRow::TokenNew));
    type_into(&mut app, CreateRow::TokenName, "wizard-golden");
    type_into(&mut app, CreateRow::Token, TOKEN_VALUE);
    let wizard = create_only(&mut app);

    let expected = answered(&mut new_app(), WizardRow::CreateOnly, |catalog, form| {
        form.pick_connection(catalog, ConnChoice::Token);
        form.connection.token_name = TextInput::new("wizard-golden");
        form.connection.token = TextInput::new(TOKEN_VALUE);
    });
    assert_same_job(&wizard, &expected);
    assert_eq!(wizard.steps[0].stdin.as_deref(), Some(TOKEN_VALUE));
    assert!(
        wizard
            .steps
            .iter()
            .all(|s| s.argv.iter().all(|a| !a.contains(TOKEN_VALUE))),
        "the token goes in on stdin, never in an argv"
    );
    if !existed {
        crate::tokens::remove_kit("wizard-golden").unwrap();
    }
}

fn notion() -> Vec<sbx::McpServer> {
    vec![sbx::McpServer {
        name: "notion".into(),
        status: "ready".into(),
        ..sbx::McpServer::default()
    }]
}

#[test]
fn w6_mcp() {
    let mut app = new_app();
    app.mcp = notion();
    open(&mut app);
    walk_to(&mut app, CreateStep::Workspace);
    tick(&mut app, WizardRow::Field(CreateRow::Mcp(0)));
    let wizard = create_only(&mut app);

    let mut app = new_app();
    app.mcp = notion();
    let expected = answered(&mut app, WizardRow::CreateOnly, |_, form| {
        form.workspace.static_mcp[0] = true;
    });
    assert_same_job(&wizard, &expected);
}

#[test]
fn w7_start_dir() {
    // The picker walks the real tree, so the workspace has to exist: `proj` with one folder.
    let base = std::env::temp_dir().join(format!("aisbx-wizard-golden-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("proj");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    // The cursor opens on `sub`: → steps into it and lands on `..`, ↓ on *Use*, ↵ takes it.
    let pick = |app: &mut App| {
        assert!(
            matches!(app.screen, Screen::StartDir(_)),
            "the picker is open"
        );
        press(app, KeyCode::Right);
        press(app, KeyCode::Down);
        press(app, KeyCode::Enter);
    };

    let mut app = new_app();
    app.cwd = root.clone();
    open(&mut app);
    let label = "Typed before the picker";
    walk_to(&mut app, CreateStep::Workspace);
    type_into(&mut app, CreateRow::Name, label);
    enter(&mut app, WizardRow::Field(CreateRow::StartDir));
    pick(&mut app);
    assert_eq!(step(&app), CreateStep::Workspace, "back on step 4");
    assert_eq!(form(&app).workspace.name.value, label);
    let wizard = create_only(&mut app);

    let mut app = new_app();
    app.cwd = root.clone();
    let expected = answered(&mut app, WizardRow::CreateOnly, |_, form| {
        type_label(form, label);
        form.workspace.start_dir = Some(PathBuf::from("sub"));
    });
    assert_same_job(&wizard, &expected);
    let reg = wizard.post.register.expect("the sandbox is registered");
    assert_eq!(reg.start_dir, Some(PathBuf::from("sub")));
    let _ = std::fs::remove_dir_all(&base);
}

/// Next on the step owning the field: refused with the create's own message, and the wizard
/// stays where it is. Ctrl-S from the review says the same, from the same step.
fn refused_on(app: &mut App, owner: CreateStep, message: &str) {
    assert_eq!(step(app), owner);
    next(app);
    assert_eq!(step(app), owner, "Next stays");
    assert_eq!(error(app), message);
    app.status = None;
    ctrl_s(app);
    assert!(app.modal.is_none(), "no job was started");
    assert_eq!(step(app), owner);
    assert_eq!(error(app), message);
}

#[test]
fn w8_refusals() {
    let mut app = new_app();
    open(&mut app);
    walk_to(&mut app, CreateStep::Workspace);
    type_into(&mut app, CreateRow::Ports, "8080:not-a-port");
    refused_on(&mut app, CreateStep::Workspace, BAD_PORT);

    let mut app = new_app();
    open(&mut app);
    walk_to(&mut app, CreateStep::Workspace);
    type_into(&mut app, CreateRow::DenyNetwork, "example.com bad!host");
    refused_on(&mut app, CreateStep::Workspace, BAD_HOST);

    let mut app = new_app();
    open(&mut app);
    choose(&mut app, Provider::Anthropic);
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    type_into(&mut app, CreateRow::TokenName, "Bad Name");
    type_into(&mut app, CreateRow::Token, TOKEN_VALUE);
    refused_on(&mut app, CreateStep::Harness, BAD_NAME);

    let mut app = new_app();
    with_stored_token(&mut app, "private");
    open(&mut app);
    choose(&mut app, Provider::Anthropic);
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    enter(&mut app, WizardRow::Field(CreateRow::TokenNew));
    type_into(&mut app, CreateRow::TokenName, "private");
    type_into(&mut app, CreateRow::Token, TOKEN_VALUE);
    refused_on(&mut app, CreateStep::Harness, TAKEN);

    let mut app = new_app();
    open(&mut app);
    choose(&mut app, Provider::Anthropic);
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    type_into(&mut app, CreateRow::TokenName, "personal");
    refused_on(&mut app, CreateStep::Harness, NO_VALUE);
}

// --- Ctrl-S ------------------------------------------------------------------------------------

#[test]
fn ctrl_s_on_every_step_is_create_and_start() {
    let expected = answered(&mut new_app(), WizardRow::CreateStart, |_, _| {});
    assert!(expected.post.attach.is_some(), "Create & start attaches");

    for target in CreateStep::ALL {
        let mut app = new_app();
        open(&mut app);
        walk_to(&mut app, target);
        ctrl_s(&mut app);
        assert_same_job(&started(&mut app), &expected);
        assert!(
            matches!(app.screen, Screen::List),
            "{target:?}: the wizard closed"
        );
    }

    let mut app = new_app();
    open(&mut app);
    walk_to(&mut app, CreateStep::Review);
    enter(&mut app, WizardRow::CreateStart);
    assert_same_job(&started(&mut app), &expected);
}

#[test]
fn ctrl_s_with_a_bad_field_goes_to_its_step() {
    let mut app = new_app();
    open(&mut app);
    walk_to(&mut app, CreateStep::Workspace);
    type_into(&mut app, CreateRow::Ports, "8080:not-a-port");
    for _ in 0..5 {
        press(&mut app, KeyCode::Esc);
    }
    assert_eq!(step(&app), CreateStep::Provider);
    ctrl_s(&mut app);
    assert!(app.modal.is_none(), "no job was started");
    assert_eq!(step(&app), CreateStep::Workspace);
    assert_eq!(error(&app), BAD_PORT);
}

// --- navigation --------------------------------------------------------------------------------

#[test]
fn n_and_the_new_card_open_the_wizard_on_step_one() {
    let mut app = new_app();
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(step(&app), CreateStep::Provider);
    let opened = form(&app).clone();
    // An empty host: the cursor starts on the New row.
    let mut app = new_app();
    press(&mut app, KeyCode::Enter);
    assert_eq!(step(&app), CreateStep::Provider);

    let mut fresh = new_app();
    fresh.open_create();
    let defaults = form(&fresh);
    assert_eq!(opened.harness.backend, defaults.harness.backend);
    assert_eq!(opened.connection.account, defaults.connection.account);
    assert_eq!(opened.kits.addons, defaults.kits.addons);
    assert_eq!(opened.workspace.name.value, defaults.workspace.name.value);
    assert_eq!(opened.workspace.skills, defaults.workspace.skills);
}

/// The answers worth losing: a typed label, a pasted token, ticked kits and ports.
fn answers(app: &App) -> String {
    let form = form(app);
    format!(
        "{:?} {:?} {:?} {:?} {:?} {:?} {:?}",
        form.harness.backend,
        form.connection.account,
        form.connection.token.value,
        form.kits.addons,
        form.workspace.name.value,
        form.workspace.ports.value,
        form.connection.models,
    )
}

#[test]
fn esc_steps_back_keeping_every_answer_and_closes_on_step_one() {
    let mut app = new_app();
    open(&mut app);
    choose(&mut app, Provider::Anthropic);
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    type_into(&mut app, CreateRow::Token, TOKEN_VALUE);
    next(&mut app);
    let jdk = WizardRow::Field(CreateRow::Addon(addon(&app, "jdk25")));
    tick(&mut app, jdk);
    next(&mut app);
    type_into(&mut app, CreateRow::Name, "Kept");
    type_into(&mut app, CreateRow::Ports, "8080");
    next(&mut app);
    assert_eq!(step(&app), CreateStep::Review);
    let before = answers(&app);

    for back_to in [
        CreateStep::Workspace,
        CreateStep::Kits,
        CreateStep::Harness,
        CreateStep::Provider,
    ] {
        press(&mut app, KeyCode::Esc);
        assert_eq!(step(&app), back_to);
        assert_eq!(answers(&app), before, "on {back_to:?}");
    }
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.screen, Screen::List), "Esc on step 1 closes");
}

#[test]
fn cancel_closes_from_every_step() {
    for target in CreateStep::ALL {
        let mut app = new_app();
        open(&mut app);
        walk_to(&mut app, target);
        enter(&mut app, WizardRow::Cancel);
        assert!(matches!(app.screen, Screen::List), "{target:?}");
        assert!(app.modal.is_none());
    }
}

#[test]
fn back_is_inert_on_step_one_and_steps_back_elsewhere() {
    let mut app = new_app();
    open(&mut app);
    enter(&mut app, WizardRow::Back);
    assert_eq!(step(&app), CreateStep::Provider);
    assert!(matches!(app.screen, Screen::Create(_)));
    // A click on it does nothing either.
    let at = rows(&app)
        .iter()
        .position(|r| *r == WizardRow::Back)
        .unwrap();
    click(&mut app, Hit::FormRow(at));
    assert_eq!(step(&app), CreateStep::Provider);

    walk_to(&mut app, CreateStep::Kits);
    enter(&mut app, WizardRow::Back);
    assert_eq!(step(&app), CreateStep::Models);
}

#[test]
fn a_click_on_a_new_connection_field_keeps_the_cursor_in_it() {
    let mut app = new_app();
    app.litellm_root = aisbx_core::testkit::litellm_root_with(&[aisbx_core::testkit::acme()]);
    open(&mut app);
    walk_to(&mut app, CreateStep::Connection);
    go(&mut app, WizardRow::Field(CreateRow::LitellmConn));
    press(&mut app, KeyCode::Right);
    for (row, marker) in [
        (CreateRow::LitellmName, "zq-name"),
        (CreateRow::LitellmUrl, "zq-url"),
        (CreateRow::LitellmKey, "zq-key-marker"),
    ] {
        let at = rows(&app)
            .iter()
            .position(|r| *r == WizardRow::Field(row))
            .unwrap_or_else(|| panic!("{row:?} is offered"));
        click(&mut app, Hit::FormRow(at));
        assert_eq!(
            focused(&app),
            WizardRow::Field(row),
            "a click stays on {row:?}"
        );
        for c in marker.chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let typed = &form(&app).connection.new_conn;
        let field = match row {
            CreateRow::LitellmName => &typed.name,
            CreateRow::LitellmUrl => &typed.url,
            _ => &typed.key,
        };
        assert_eq!(field.value, marker, "typed into {row:?}");
    }
    let screen = crate::ui::tests_support::render_at(&mut app, 110, 40);
    assert!(screen.contains("zq-url"));
    assert!(!screen.contains("zq-key-marker"), "the key is masked");
    // No model row: every sandbox reads the gateway's models itself.
    for row in ["models  ", "default model"] {
        assert!(!screen.contains(row), "{row}: {screen}");
    }
}

#[test]
fn the_button_line_has_the_drawn_buttons() {
    let mut app = new_app();
    open(&mut app);
    for target in CreateStep::ALL {
        walk_to(&mut app, target);
        let buttons = CreateForm::wizard_button_rows(target).to_vec();
        assert_eq!(form(&app).wizard_buttons(), buttons.len());
        let want = match target {
            CreateStep::Review => vec![
                WizardRow::CreateStart,
                WizardRow::CreateOnly,
                WizardRow::Back,
                WizardRow::Cancel,
            ],
            _ => vec![WizardRow::Next, WizardRow::Back, WizardRow::Cancel],
        };
        assert_eq!(buttons, want, "{target:?}");
        go(&mut app, buttons[0]);
        let mut seen = vec![focused(&app)];
        for _ in 1..buttons.len() {
            press(&mut app, KeyCode::Right);
            seen.push(focused(&app));
        }
        assert_eq!(seen, buttons, "→ walks the line on {target:?}");
        press(&mut app, KeyCode::Right);
        assert_eq!(focused(&app), buttons[0], "and wraps");
        press(&mut app, KeyCode::Left);
        assert_eq!(focused(&app), *buttons.last().unwrap());
        assert!(app.on_form_buttons());
    }
}

#[test]
fn next_is_refused_on_the_step_owning_the_field() {
    let mut app = new_app();
    open(&mut app);
    walk_to(&mut app, CreateStep::Workspace);
    type_into(&mut app, CreateRow::Ports, "8080:not-a-port");
    next(&mut app);
    assert_eq!(step(&app), CreateStep::Workspace);
    assert_eq!(error(&app), BAD_PORT);

    let mut app = new_app();
    open(&mut app);
    choose(&mut app, Provider::Anthropic);
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    assert_eq!(form(&app).connection.account, AccountChoice::New);
    next(&mut app);
    assert_eq!(step(&app), CreateStep::Harness);
    assert_eq!(error(&app), NO_VALUE);
}

#[test]
fn the_trail_goes_back_at_once_and_forward_only_past_good_steps() {
    let mut app = new_app();
    open(&mut app);
    walk_to(&mut app, CreateStep::Workspace);
    type_into(&mut app, CreateRow::Ports, "8080:not-a-port");
    click(&mut app, Hit::WizardStep(CreateStep::Harness));
    assert_eq!(step(&app), CreateStep::Harness);
    click(&mut app, Hit::WizardStep(CreateStep::Review));
    assert_eq!(step(&app), CreateStep::Workspace, "stops on the bad port");
    assert_eq!(error(&app), BAD_PORT);

    let mut app = new_app();
    open(&mut app);
    click(&mut app, Hit::WizardStep(CreateStep::Review));
    assert_eq!(step(&app), CreateStep::Review);
    click(&mut app, Hit::WizardStep(CreateStep::Kits));
    assert_eq!(step(&app), CreateStep::Kits);
}

#[test]
fn enter_on_a_radio_picks_it_and_goes_on() {
    // A harness with nothing to answer under it goes on to the next step.
    let mut app = new_app();
    open(&mut app);
    choose(&mut app, Provider::Google);
    let row = agent(&app, "agy");
    enter(&mut app, row);
    assert_eq!(step(&app), CreateStep::Kits);
    assert_eq!(form(&app).harness.backend, backend(&app, "agy"));

    // A gateway harness, its connection picked on the step before, goes on to its models.
    let mut app = new_app();
    open(&mut app);
    choose(&mut app, Provider::Litellm);
    let row = agent(&app, "opencode");
    enter(&mut app, row);
    assert_eq!(step(&app), CreateStep::Models);
    assert_eq!(
        form(&app).harness.backend,
        backend(&app, "opencode-litellm")
    );

    // Claude Code on Anthropic goes on to its account radios, and /login on to Kits.
    let mut app = new_app();
    open(&mut app);
    choose(&mut app, Provider::Anthropic);
    let row = agent(&app, "claude");
    enter(&mut app, row);
    assert_eq!(focused(&app), WizardRow::Connection(ConnChoice::Login));
    press(&mut app, KeyCode::Enter);
    assert_eq!(step(&app), CreateStep::Kits);
    assert_eq!(
        form(&app).harness.backend,
        backend(&app, "claude-anthropic")
    );

    let mut app = new_app();
    with_stored_token(&mut app, "private");
    open(&mut app);
    choose(&mut app, Provider::Anthropic);
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    assert_eq!(step(&app), CreateStep::Harness);
    assert_eq!(focused(&app), WizardRow::Field(CreateRow::TokenPick(0)));
}

#[test]
fn the_harness_step_offers_only_the_providers_harnesses() {
    let mut app = new_app();
    open(&mut app);
    let agents = |app: &App| -> Vec<WizardRow> {
        let rows = rows(app).into_iter();
        rows.filter(|r| matches!(r, WizardRow::Agent(_))).collect()
    };
    choose(&mut app, Provider::OpenAi);
    assert_eq!(agents(&app), vec![agent(&app, "codex")]);
    assert_eq!(focused(&app), agent(&app, "codex"), "preselected");
    let screen = crate::ui::tests_support::render_at(&mut app, 110, 40);
    assert!(screen.contains("Codex"), "{screen}");
    assert!(!screen.contains("OpenCode"), "{screen}");
    press(&mut app, KeyCode::Esc);
    choose(&mut app, Provider::Litellm);
    assert_eq!(agents(&app).len(), 3);
    let screen = crate::ui::tests_support::render_at(&mut app, 110, 40);
    for name in ["Claude Code", "OpenCode", "Pi"] {
        assert!(screen.contains(name), "{name}: {screen}");
    }
    assert!(!screen.contains("Codex"), "{screen}");
}

#[test]
fn switching_the_harness_keeps_the_other_answers() {
    let mut app = new_app();
    open(&mut app);
    // Claude Code on the gateway, with a pin, a kit, a label and a port.
    walk_to(&mut app, CreateStep::Harness);
    let claude = agent(&app, "claude");
    let opencode = agent(&app, "opencode");
    next(&mut app);
    assert_eq!(step(&app), CreateStep::Models);
    go(
        &mut app,
        WizardRow::Field(CreateRow::Model(crate::models::Slot::ALL[0])),
    );
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert!(
        form(&app)
            .connection
            .models
            .get(crate::models::Slot::ALL[0])
            .is_some(),
        "a model is pinned"
    );
    next(&mut app);
    let jdk = WizardRow::Field(CreateRow::Addon(addon(&app, "jdk25")));
    tick(&mut app, jdk);
    next(&mut app);
    type_into(&mut app, CreateRow::Name, "Mine");
    type_into(&mut app, CreateRow::Ports, "8080");
    let before = answers(&app);

    click(&mut app, Hit::WizardStep(CreateStep::Harness));
    enter(&mut app, opencode);
    click(&mut app, Hit::WizardStep(CreateStep::Harness));
    enter(&mut app, claude);
    assert_eq!(answers(&app), before);
    click(&mut app, Hit::WizardStep(CreateStep::Provider));
    choose(&mut app, Provider::Anthropic);
    click(&mut app, Hit::WizardStep(CreateStep::Provider));
    choose(&mut app, Provider::Litellm);
    assert_eq!(answers(&app), before);
}

#[test]
fn the_backend_picked_per_provider_comes_back() {
    let mut app = new_app();
    with_stored_token(&mut app, "private");
    open(&mut app);
    choose(&mut app, Provider::Anthropic);
    enter(&mut app, WizardRow::Connection(ConnChoice::Token));
    press(&mut app, KeyCode::Enter);
    let anthropic = backend(&app, "claude-anthropic");
    let token = AccountChoice::Named("private".into());
    assert_eq!(form(&app).harness.backend, anthropic);
    assert_eq!(form(&app).connection.account, token);

    // While on a setup token the provider is still Anthropic, and the harness Claude Code.
    click(&mut app, Hit::WizardStep(CreateStep::Provider));
    assert_eq!(focused(&app), provider(&app, Provider::Anthropic));
    click(&mut app, Hit::WizardStep(CreateStep::Harness));
    assert_eq!(form(&app).selected_agent(&app.catalog), Some("claude"));
    assert_eq!(
        focused(&app),
        agent(&app, "claude"),
        "the cursor starts on it"
    );

    click(&mut app, Hit::WizardStep(CreateStep::Provider));
    choose(&mut app, Provider::Litellm);
    let first = backend(&app, "claude-litellm");
    assert_eq!(form(&app).harness.backend, first, "the provider's first");
    let opencode = agent(&app, "opencode");
    enter(&mut app, opencode);
    assert_eq!(
        form(&app).harness.backend,
        backend(&app, "opencode-litellm")
    );
    click(&mut app, Hit::WizardStep(CreateStep::Provider));
    choose(&mut app, Provider::Anthropic);
    assert_eq!(form(&app).harness.backend, anthropic);
    assert_eq!(form(&app).connection.account, token);
    click(&mut app, Hit::WizardStep(CreateStep::Provider));
    choose(&mut app, Provider::Litellm);
    assert_eq!(
        form(&app).harness.backend,
        backend(&app, "opencode-litellm"),
        "OpenCode comes back"
    );
}
