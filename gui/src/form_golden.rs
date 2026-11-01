//! The forms through the window: the create wizard's golden answers given as egui events give the
//! job core gives for the same answers as its own keys (`core/src/create_golden.rs` pins those),
//! the same-frame key cases, the masked secret rows and the start-folder picker's round trip.

use std::path::PathBuf;

use aisbx_core::app::{App, CreateForm, CreateRow, Modal, Screen};
use aisbx_core::create::{ConnChoice, CreateStep, FieldEffect, WizardRow};
use aisbx_core::input::{KeyCode, KeyEvent, KeyModifiers};
use aisbx_core::kits::{Catalog, Provider};
use aisbx_core::{model, sbx, testkit};
use eframe::egui::{self, Key, Modifiers};

use crate::chooser::Chooser;
use crate::session::Session;
use crate::shell;
use crate::theme;

/// One frame's events.
pub(crate) type Events = Vec<egui::Event>;

pub(crate) fn pressed(key: Key, modifiers: Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

/// A character as egui sends it: its key first where it has one, then the text.
pub(crate) fn typed(c: char) -> Events {
    let key = match c {
        ' ' => Some(Key::Space),
        '?' => Some(Key::Questionmark),
        c if c.is_ascii_alphabetic() => Key::from_name(&c.to_ascii_uppercase().to_string()),
        _ => None,
    };
    let modifiers = match c.is_ascii_uppercase() {
        true => Modifiers::SHIFT,
        false => Modifiers::NONE,
    };
    key.map(|k| pressed(k, modifiers))
        .into_iter()
        .chain([egui::Event::Text(c.to_string())])
        .collect()
}

/// A key core names, as egui sends it.
pub(crate) fn events(code: KeyCode) -> Events {
    let none = Modifiers::NONE;
    let key = match code {
        KeyCode::Up => Key::ArrowUp,
        KeyCode::Down => Key::ArrowDown,
        KeyCode::Left => Key::ArrowLeft,
        KeyCode::Right => Key::ArrowRight,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Escape,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => return vec![pressed(Key::Tab, Modifiers::SHIFT)],
        KeyCode::Backspace => Key::Backspace,
        KeyCode::End => Key::End,
        KeyCode::Home => Key::Home,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::Char(c) => return typed(c),
        other => panic!("not a form key here: {other:?}"),
    };
    vec![pressed(key, none)]
}

/// A window under test.
pub(crate) struct Win {
    pub(crate) ctx: egui::Context,
    pub(crate) app: App,
    pub(crate) chooser: Option<Chooser>,
    pub(crate) session: Session,
    /// Every text drawn in the last frame.
    pub(crate) drawn: Vec<String>,
    /// The same texts, each with where it was drawn.
    pub(crate) drawn_at: Vec<(String, egui::Rect)>,
    /// Every run of text the last frame drew in the monospace face: a value, a key, a command.
    pub(crate) mono: Vec<String>,
    /// Every text of the last frame with the colour of its first piece.
    pub(crate) colored: Vec<(String, egui::Color32)>,
    /// Every filled rectangle of the last frame, with its fill: a frame behind a row or a button.
    pub(crate) fills: Vec<(egui::Rect, egui::Color32)>,
    /// Every outlined rectangle of the last frame, with its outline's colour: a control's border,
    /// the ring core's cursor is drawn as.
    pub(crate) rings: Vec<(egui::Rect, egui::Color32)>,
    /// The window's size, in points.
    pub(crate) size: egui::Vec2,
}

impl Win {
    pub(crate) fn new(app: App) -> Self {
        let ctx = egui::Context::default();
        // The window's own look, as `main` installs it: what is tested is what is drawn.
        crate::theme::install(&ctx);
        let mut w = Win {
            ctx,
            app,
            chooser: None,
            session: Session::default(),
            drawn: Vec::new(),
            drawn_at: Vec::new(),
            mono: Vec::new(),
            colored: Vec::new(),
            fills: Vec::new(),
            rings: Vec::new(),
            size: egui::vec2(1200.0, 760.0),
        };
        w.frame(Vec::new());
        w
    }

    pub(crate) fn frame(&mut self, events: Events) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)),
            events,
            ..Default::default()
        };
        let (app, chooser, session) = (&mut self.app, &mut self.chooser, &mut self.session);
        let mut output = self
            .ctx
            .run_ui(input, |ui| shell::list(ui, app, chooser, session));
        output.textures_delta.clear();
        self.drawn_at.clear();
        self.mono.clear();
        self.colored.clear();
        self.fills.clear();
        self.rings.clear();
        for clipped in &output.shapes {
            collect(&clipped.shape, self);
        }
        self.drawn = self.drawn_at.iter().map(|(t, _)| t.clone()).collect();
    }
}

fn collect(shape: &egui::Shape, w: &mut Win) {
    match shape {
        egui::Shape::Text(text) => {
            let rect = text.galley.rect.translate(text.pos.to_vec2());
            w.drawn_at.push((text.galley.text().to_string(), rect));
            let job = &text.galley.job;
            let color = (job.sections.first())
                .map(|s| s.format.color)
                .filter(|c| *c != egui::Color32::PLACEHOLDER)
                .unwrap_or(text.fallback_color);
            w.colored.push((text.galley.text().to_string(), color));
            let mono = (job.sections.iter())
                .filter(|s| s.format.font_id.family == egui::FontFamily::Monospace)
                .map(|s| job.text[s.byte_range.start.0..s.byte_range.end.0].to_string());
            w.mono.extend(mono);
        }
        egui::Shape::Rect(rect) => {
            if rect.fill.a() > 0 {
                w.fills.push((rect.rect, rect.fill));
            }
            if rect.stroke.width > 0.0 {
                w.rings.push((rect.rect, rect.stroke.color));
            }
        }
        egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, w)),
        _ => {}
    }
}

/// A primary click at `pos`, pressed in one frame and released in the next.
pub(crate) fn click(w: &mut Win, pos: egui::Pos2) {
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    w.frame(vec![egui::Event::PointerMoved(pos), button(true)]);
    w.frame(vec![button(false)]);
}

/// Who gives the keys: core's own `on_key`, or the window's frames.
pub(crate) trait Driver {
    fn app(&self) -> &App;
    fn app_mut(&mut self) -> &mut App;
    fn press(&mut self, code: KeyCode);
    fn type_text(&mut self, text: &str);
    fn save(&mut self);
    /// The key that moves to the next button of a form's line in core's order: → for core, ←
    /// in the window, which draws the line in core's order backwards.
    fn along(&self) -> KeyCode;
}

pub(crate) struct Core(pub(crate) App);

impl Driver for Core {
    fn app(&self) -> &App {
        &self.0
    }
    fn app_mut(&mut self) -> &mut App {
        &mut self.0
    }
    fn press(&mut self, code: KeyCode) {
        self.0.on_key(KeyEvent::from(code));
    }
    fn type_text(&mut self, text: &str) {
        text.chars().for_each(|c| self.press(KeyCode::Char(c)));
    }
    fn save(&mut self) {
        self.0
            .on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    }
    fn along(&self) -> KeyCode {
        KeyCode::Right
    }
}

impl Driver for Win {
    fn app(&self) -> &App {
        &self.app
    }
    fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }
    fn press(&mut self, code: KeyCode) {
        self.frame(events(code));
    }
    /// The whole text in one frame, each character as egui sends it.
    fn type_text(&mut self, text: &str) {
        self.frame(text.chars().flat_map(typed).collect());
    }
    fn save(&mut self) {
        self.frame(vec![pressed(Key::S, Modifiers::COMMAND)]);
    }
    fn along(&self) -> KeyCode {
        KeyCode::Left
    }
}

fn form(app: &App) -> &CreateForm {
    let Screen::Create(form) = &app.screen else {
        panic!("the create form is open")
    };
    form
}

fn focused(app: &App) -> WizardRow {
    let form = form(app);
    let rows = form.wizard_rows(&app.catalog);
    rows[form.wizard_focus().min(rows.len() - 1)]
}

/// Walk the cursor to `row`, as `create_golden.rs` does: ↓ to it, then along the buttons.
fn go(d: &mut dyn Driver, row: WizardRow) {
    let buttons = CreateForm::wizard_button_rows(form(d.app()).wizard_step());
    let (target, rights) = match buttons.iter().position(|b| *b == row) {
        Some(i) => (buttons[0], i),
        None => (row, 0),
    };
    for _ in 0..200 {
        if focused(d.app()) == target {
            break;
        }
        d.press(KeyCode::Down);
    }
    assert_eq!(focused(d.app()), target);
    for _ in 0..rights {
        let along = d.along();
        d.press(along);
    }
    assert_eq!(focused(d.app()), row);
}

/// A step of a golden case.
#[derive(Clone, Copy)]
enum Do {
    Enter(WizardRow),
    Tick(CreateRow),
    Type(CreateRow, &'static str),
    Go(WizardRow),
    Key(KeyCode),
    Walk(CreateStep),
}

fn run(d: &mut dyn Driver, script: &[Do]) {
    for step in script {
        match *step {
            Do::Enter(row) => {
                go(d, row);
                d.press(KeyCode::Enter);
            }
            Do::Tick(row) => {
                go(d, WizardRow::Field(row));
                d.press(KeyCode::Char(' '));
            }
            Do::Type(row, text) => {
                go(d, WizardRow::Field(row));
                d.press(KeyCode::End);
                for _ in 0..60 {
                    d.press(KeyCode::Backspace);
                }
                d.type_text(text);
            }
            Do::Go(row) => go(d, row),
            Do::Key(code) => d.press(code),
            Do::Walk(to) => {
                for _ in 0..CreateStep::ALL.len() {
                    if form(d.app()).wizard_step() == to {
                        break;
                    }
                    go(d, WizardRow::Next);
                    d.press(KeyCode::Enter);
                }
                assert_eq!(form(d.app()).wizard_step(), to, "{:?}", d.app().status);
            }
        }
    }
}

/// Create on the Review step or save from where the script left off; the job, or the refusal.
fn finish(d: &mut dyn Driver, save: bool) -> Result<String, String> {
    if save {
        d.save();
    } else {
        run(
            d,
            &[
                Do::Walk(CreateStep::Review),
                Do::Enter(WizardRow::CreateOnly),
            ],
        );
    }
    match d.app_mut().modal.take() {
        Some(Modal::Running(_, pending)) => {
            let attach = pending.post.attach.clone();
            let name = pending.post.register.as_ref().map(|r| r.name.clone());
            // As the runner sees it: per step label, argv and stdin, then the job it starts next.
            let (mut shape, mut job) = (Vec::new(), Some(&pending));
            while let Some(j) = job {
                for s in &j.steps {
                    shape.push((&j.title, &s.label, &s.argv, s.stdin.clone()));
                }
                job = j.post.then.as_deref();
            }
            Ok(format!("{shape:?} {attach:?} {name:?}"))
        }
        _ => Err(format!(
            "{:?}",
            d.app()
                .status
                .as_ref()
                .map(|(text, error, _)| (text, error))
        )),
    }
}

/// The test app of `create_golden.rs`, in `/proj`.
fn golden_app(setup: &dyn Fn(&mut App)) -> App {
    let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
    app.cwd = PathBuf::from("/proj");
    setup(&mut app);
    app
}

/// The window opens the form with `n`, core with `open_create`; the answers follow as keys.
fn same_job(setup: &dyn Fn(&mut App), script: &[Do], save: bool) {
    let mut core = Core(golden_app(setup));
    core.0.open_create();
    run(&mut core, script);
    let want = finish(&mut core, save);

    let mut win = Win::new(golden_app(setup));
    win.type_text("n");
    assert!(
        matches!(win.app.screen, Screen::Create(_)),
        "n opens the form"
    );
    run(&mut win, script);
    let got = finish(&mut win, save);
    assert_eq!(got, want);
}

/// Step 1's row for a provider, on a form as it opens.
fn provider(provider: Provider) -> WizardRow {
    let mut app = golden_app(&|_| {});
    app.open_create();
    let providers = form(&app).providers(&app.catalog);
    WizardRow::Provider(providers.iter().position(|p| *p == provider).unwrap())
}

fn agent(app: &App, agent: &str) -> WizardRow {
    let harnesses = form(app).harnesses(&app.catalog);
    WizardRow::Agent(harnesses.iter().position(|h| h.agent == agent).unwrap())
}

fn addon(name: &str) -> CreateRow {
    let catalog = Catalog::load().unwrap();
    CreateRow::Addon(
        catalog
            .addons
            .iter()
            .position(|a| a.kit.name == name)
            .unwrap(),
    )
}

fn stored_token(app: &mut App) {
    app.secrets = Some(sbx::Secrets {
        services: Vec::new(),
        custom: vec![
            testkit::litellm_key("acme", "litellm.example.com"),
            sbx::CustomSecret {
                scope: sbx::GLOBAL_SCOPE.into(),
                targets: vec![sbx::TOKEN_HOST.into()],
                env: String::new(),
                placeholder: sbx::named_token_placeholder("private"),
                masked: "***".into(),
            },
        ],
    });
}

/// `create_golden.rs`'s cases, given through the window: the same job, step for step.
#[test]
fn the_golden_answers_through_the_window_start_the_same_job() {
    let none = &|_: &mut App| {};
    let opencode = {
        let mut core = Core(golden_app(none));
        core.0.open_create();
        agent(&core.0, "opencode")
    };
    // g1: the defaults.
    same_job(none, &[], false);
    // g2: OpenCode with two kits and every option.
    same_job(
        none,
        &[
            Do::Enter(provider(Provider::Litellm)),
            Do::Walk(CreateStep::Harness),
            Do::Enter(opencode),
            Do::Walk(CreateStep::Kits),
            Do::Tick(addon("web")),
            Do::Tick(addon("jdk25")),
            Do::Walk(CreateStep::Workspace),
            Do::Type(CreateRow::Name, "My box"),
            Do::Tick(CreateRow::Clone),
            Do::Tick(CreateRow::Skills),
            Do::Tick(CreateRow::Skills),
            Do::Type(CreateRow::Ports, "8080:3000, 5173"),
            Do::Type(CreateRow::DenyNetwork, "api.example.com"),
        ],
        false,
    );
    // g3: /login with a kit, created with Ctrl-S / Cmd-S.
    same_job(
        none,
        &[
            Do::Enter(provider(Provider::Anthropic)),
            Do::Enter(WizardRow::Agent(0)),
            Do::Enter(WizardRow::Connection(ConnChoice::Login)),
            Do::Tick(addon("statusline")),
        ],
        true,
    );
    // g4: a stored named token.
    same_job(
        &stored_token,
        &[
            Do::Enter(provider(Provider::Anthropic)),
            Do::Enter(WizardRow::Connection(ConnChoice::Token)),
            Do::Enter(WizardRow::Field(CreateRow::TokenPick(0))),
        ],
        false,
    );
    // g5: a new token — its value goes in on stdin.
    same_job(
        none,
        &[
            Do::Enter(provider(Provider::Anthropic)),
            Do::Enter(WizardRow::Connection(ConnChoice::Token)),
            Do::Type(CreateRow::TokenName, "personal"),
            Do::Type(CreateRow::Token, "golden-test-token-value"),
        ],
        false,
    );
    // g6: an MCP server in the fixed set.
    let mcp = &|app: &mut App| {
        app.mcp = vec![sbx::McpServer {
            name: "docs".into(),
            transport: "remote http".into(),
            status: "ready".into(),
            kind: "remote".into(),
        }];
    };
    same_job(
        mcp,
        &[Do::Walk(CreateStep::Workspace), Do::Tick(CreateRow::Mcp(0))],
        false,
    );
    // g8: refusals, word for word.
    same_job(
        none,
        &[
            Do::Walk(CreateStep::Workspace),
            Do::Type(CreateRow::Ports, "8080:not-a-port"),
        ],
        true,
    );
    same_job(
        none,
        &[
            Do::Enter(provider(Provider::Anthropic)),
            Do::Enter(WizardRow::Connection(ConnChoice::Token)),
            Do::Type(CreateRow::TokenName, "Bad Name"),
            Do::Type(CreateRow::Token, "golden-test-token-value"),
        ],
        true,
    );
    // g9: a new connection, created with Cmd-S — the key first, on stdin, then the create.
    let root = testkit::litellm_root_with(&[testkit::acme()]);
    let at_root = move |app: &mut App| app.litellm_root = root.clone();
    same_job(
        &at_root,
        &[
            Do::Walk(CreateStep::Connection),
            Do::Go(WizardRow::Field(CreateRow::LitellmConn)),
            Do::Key(KeyCode::Right),
            Do::Type(CreateRow::LitellmName, "beta"),
            Do::Type(CreateRow::LitellmUrl, "https://gw.example.org/litellm"),
            Do::Type(CreateRow::LitellmKey, "gateway-test-key"),
        ],
        true,
    );
}

fn open_form() -> Win {
    let mut w = Win::new(golden_app(&|_| {}));
    w.type_text("n");
    assert!(matches!(w.app.screen, Screen::Create(_)));
    w
}

#[test]
fn a_provider_tile_lists_its_harnesses_in_words() {
    let mut w = open_form();
    w.frame(Vec::new());
    let ways = "Runs Claude Code, OpenCode or Pi";
    let joined = w.drawn.join(" ");
    assert!(joined.contains(ways), "{:?}", w.drawn);
    for text in &w.drawn {
        assert!(!text.trim_start().starts_with('·'), "{text:?}");
    }
}

#[test]
fn text_typed_with_n_in_one_frame_is_dropped() {
    let mut core = golden_app(&|_| {});
    core.open_create();
    let mut w = Win::new(golden_app(&|_| {}));
    w.type_text("nabc");
    let (got, want) = (form(&w.app), form(&core));
    assert_eq!(got.wizard_step(), CreateStep::Provider);
    assert_eq!(got.wizard_focus(), want.wizard_focus());
    assert_eq!(got.workspace.name.value, want.workspace.name.value);
}

#[test]
fn escape_on_step_one_then_x_opens_nothing() {
    let sandboxes = vec![model::sample("box", Some("claude-anthropic"), &[])];
    let mut app = App::for_test(Catalog::load().unwrap(), sandboxes);
    app.cwd = PathBuf::from("/proj");
    let mut w = Win::new(app);
    w.app.select(0);
    w.app.perform(aisbx_core::actions::Action::New);
    w.frame(Vec::new());
    aisbx_core::terminal::opened();
    let mut frame = vec![pressed(Key::Escape, Modifiers::NONE)];
    frame.extend(typed('x'));
    w.frame(frame);
    assert!(matches!(w.app.screen, Screen::List));
    assert!(w.app.handoff.is_none() && !w.session.busy() && !w.session.dialog_open());
    assert!(aisbx_core::terminal::opened().is_empty());
}

#[test]
fn save_then_enter_in_one_frame_leaves_the_job_open() {
    let mut w = open_form();
    w.frame(vec![
        pressed(Key::S, Modifiers::COMMAND),
        pressed(Key::Enter, Modifiers::NONE),
    ]);
    let Some(Modal::Running(_, pending)) = &w.app.modal else {
        panic!("the create started: {:?}", w.app.status)
    };
    assert!(pending.post.attach.is_some(), "Create & start");
    w.frame(Vec::new());
    assert!(matches!(w.app.modal, Some(Modal::Running(..))));
}

/// A workspace with two subfolders, as the picker's tests need.
fn tree() -> PathBuf {
    let root = std::env::temp_dir().join("aisbx-ui-picker-test");
    for rel in ["alpha/deep", "beta"] {
        std::fs::create_dir_all(root.join(rel)).unwrap();
    }
    root
}

fn picker_app(root: &std::path::Path) -> Win {
    let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
    app.cwd = root.to_path_buf();
    let mut w = Win::new(app);
    w.type_text("n");
    run(
        &mut w,
        &[
            Do::Walk(CreateStep::Workspace),
            Do::Type(CreateRow::Name, "keep me"),
            Do::Go(WizardRow::Field(CreateRow::StartDir)),
        ],
    );
    w
}

#[test]
fn enter_on_the_start_folder_row_then_down_opens_the_picker_unmoved() {
    let root = tree();
    let mut w = picker_app(&root);
    // ↵ there is the effect core turns into the picker.
    let mut copy = form(&w.app).clone();
    match copy.field_key(
        &w.app.catalog,
        CreateRow::StartDir,
        KeyEvent::from(KeyCode::Enter),
    ) {
        FieldEffect::OpenStartDir => {}
        FieldEffect::Ignored
        | FieldEffect::Done
        | FieldEffect::FetchModels
        | FieldEffect::EditConfig(_)
        | FieldEffect::ResetConfig(_) => panic!("↵ opens the picker"),
    }
    w.frame(vec![
        pressed(Key::Enter, Modifiers::NONE),
        pressed(Key::ArrowDown, Modifiers::NONE),
    ]);
    let Screen::StartDir(picker) = &w.app.screen else {
        panic!("the picker is open")
    };
    let mut core = Core(golden_app(&|_| {}));
    core.0.cwd = root.clone();
    core.0.open_create();
    run(
        &mut core,
        &[
            Do::Walk(CreateStep::Workspace),
            Do::Enter(WizardRow::Field(CreateRow::StartDir)),
        ],
    );
    let Screen::StartDir(want) = &core.0.screen else {
        panic!("core opens it too")
    };
    assert_eq!(picker.focus, want.focus, "the ↓ was dropped");
}

#[test]
fn the_picker_returns_to_the_wizard_with_the_answers_kept() {
    let root = tree();
    let mut w = picker_app(&root);
    w.press(KeyCode::Enter);
    assert!(matches!(w.app.screen, Screen::StartDir(_)));
    w.press(KeyCode::Esc);
    let ws = &form(&w.app).workspace;
    assert_eq!(
        (ws.name.value.as_str(), ws.start_dir.clone()),
        ("keep me", None)
    );
    assert_eq!(form(&w.app).wizard_step(), CreateStep::Workspace);

    w.press(KeyCode::Enter);
    w.press(KeyCode::Right);
    for _ in 0..10 {
        match &w.app.screen {
            Screen::StartDir(p) if p.focused() == aisbx_core::app::StartDirRow::Use => break,
            Screen::StartDir(_) => w.press(KeyCode::Down),
            _ => panic!("the picker is open"),
        }
    }
    w.press(KeyCode::Enter);
    let ws = &form(&w.app).workspace;
    assert_eq!(ws.name.value, "keep me");
    assert_eq!(ws.start_dir, Some(PathBuf::from("alpha")));
}

/// The first character typed replaces the generated label.
#[test]
fn text_then_tab_types_then_moves() {
    let mut w = open_form();
    run(
        &mut w,
        &[
            Do::Walk(CreateStep::Workspace),
            Do::Go(WizardRow::Field(CreateRow::Name)),
        ],
    );
    let at = form(&w.app).wizard_focus();
    let mut frame: Events = "box".chars().flat_map(typed).collect();
    frame.push(pressed(Key::Tab, Modifiers::NONE));
    w.frame(frame);
    assert_eq!(form(&w.app).workspace.name.value, "box");
    assert_eq!(form(&w.app).wizard_focus(), at + 1);
    w.frame(Vec::new());
    assert_eq!(
        w.ctx.memory(|m| m.focused()),
        None,
        "no widget took the Tab"
    );
}

#[test]
fn escape_then_d_on_the_kits_editor_deletes_nothing() {
    let sandboxes = vec![model::sample("box", Some("claude-anthropic"), &[])];
    let mut w = Win::new(App::for_test(Catalog::load().unwrap(), sandboxes));
    w.app.select(0);
    w.type_text("e");
    assert!(
        matches!(w.app.screen, Screen::Kits(_)),
        "e opens the editor"
    );
    let mut frame = vec![pressed(Key::Escape, Modifiers::NONE)];
    frame.extend(typed('d'));
    w.frame(frame);
    assert!(matches!(w.app.screen, Screen::List));
    assert!(w.app.modal.is_none(), "no delete confirmation");
}

#[test]
fn a_fields_problem_draws_its_command_as_code() {
    let mut w = open_form();
    run(
        &mut w,
        &[
            Do::Enter(provider(Provider::Anthropic)),
            Do::Enter(WizardRow::Connection(ConnChoice::Token)),
            Do::Type(CreateRow::TokenName, "personal"),
            Do::Key(KeyCode::PageDown),
        ],
    );
    w.frame(Vec::new());
    assert_eq!(
        form(&w.app).wizard_step(),
        CreateStep::Harness,
        "Next is refused"
    );
    let code = "claude setup-token";
    assert!(w.mono.iter().any(|m| m == code), "{:?}", w.mono);
    assert!(!w.drawn.iter().any(|t| t.contains('`')), "{:?}", w.drawn);
}

#[test]
fn secret_rows_draw_no_character_of_their_value() {
    let secret = "жшщэюя";
    let mut w = open_form();
    run(
        &mut w,
        &[
            Do::Enter(provider(Provider::Anthropic)),
            Do::Enter(WizardRow::Connection(ConnChoice::Token)),
            Do::Type(CreateRow::Token, secret),
        ],
    );
    assert_eq!(form(&w.app).connection.token.value, secret);
    let check = |w: &Win| {
        let drawn = w.drawn.join("\n");
        assert!(drawn.contains("••••••"), "{drawn}");
        assert!(!drawn.chars().any(|c| secret.contains(c)), "{drawn}");
    };
    check(&w);
    // Off the row too.
    w.press(KeyCode::Up);
    check(&w);

    let mut w = open_form();
    run(
        &mut w,
        &[
            Do::Walk(CreateStep::Connection),
            Do::Go(WizardRow::Field(CreateRow::LitellmConn)),
            Do::Key(KeyCode::Left),
            Do::Type(CreateRow::LitellmKey, secret),
        ],
    );
    assert_eq!(form(&w.app).connection.new_conn.key.value, secret);
    check(&w);
}

/// Where the first text of the last frame that starts with `start` was drawn, right of `left`.
fn text_from(w: &Win, start: &str, left: f32) -> egui::Rect {
    let found = w
        .drawn_at
        .iter()
        .find(|(t, r)| t.starts_with(start) && r.left() >= left);
    found
        .unwrap_or_else(|| panic!("{start} is drawn right of {left}: {:?}", w.drawn))
        .1
}

/// The fills of the last frame drawn behind `rect`: a row's frame, a button's.
fn fills_behind(w: &Win, rect: egui::Rect) -> Vec<egui::Color32> {
    let inner = rect.shrink(1.0);
    w.fills
        .iter()
        .filter(|(f, _)| f.contains_rect(inner))
        .map(|(_, c)| *c)
        .collect()
}

fn dark(w: &Win) -> bool {
    w.ctx.theme() == egui::Theme::Dark
}

#[test]
fn the_wizard_draws_cores_cursor_on_its_row() {
    let mut w = open_form();
    run(&mut w, &[Do::Walk(CreateStep::Harness)]);
    w.frame(Vec::new());
    let accent = theme::accent(dark(&w));
    let title = |w: &Win, name: &str| tile_title(w, name);
    // A ring is wider than the tile it marks, so it holds a tile's whole border.
    let ringed = |w: &Win, name: &str| {
        let at = title(w, name);
        let rings: Vec<egui::Rect> = (w.rings.iter())
            .filter(|(r, c)| *c == accent && r.contains_rect(at))
            .map(|(r, _)| *r)
            .collect();
        rings.iter().any(|ring| {
            w.rings.iter().any(|(inner, _)| {
                inner.contains_rect(at) && ring.contains_rect(*inner) && ring != inner
            })
        })
    };
    let picked = |w: &Win, name: &str| {
        let tint = accent.gamma_multiply(0.10);
        fills_behind(w, title(w, name)).contains(&tint)
    };
    assert!(ringed(&w, "Claude Code"), "{:?}", w.rings);
    assert!(!ringed(&w, "OpenCode"));
    assert!(picked(&w, "Claude Code") && !picked(&w, "OpenCode"));
    w.press(KeyCode::Down);
    assert!(ringed(&w, "OpenCode"), "{:?}", w.rings);
    assert!(!ringed(&w, "Claude Code"), "the ring moves");
    assert!(picked(&w, "Claude Code"), "the pick stays");
}

#[test]
fn the_wizard_buttons_sit_and_move_the_way_they_point() {
    let mut w = open_form();
    run(&mut w, &[Do::Walk(CreateStep::Review)]);
    w.frame(Vec::new());
    let x = |w: &Win, label: &str| text_from(w, label, 0.0).left();
    let order = ["Cancel", "Back", "Create only", "Create & start"];
    for pair in order.windows(2) {
        assert!(x(&w, pair[0]) < x(&w, pair[1]), "{pair:?}");
    }
    go(&mut w, WizardRow::CreateStart);
    w.press(KeyCode::Left);
    assert_eq!(focused(&w.app), WizardRow::CreateOnly, "← goes left");
    w.press(KeyCode::Left);
    assert_eq!(focused(&w.app), WizardRow::Back);
    w.press(KeyCode::Right);
    assert_eq!(focused(&w.app), WizardRow::CreateOnly, "→ goes right");
}

#[test]
fn the_cursor_on_a_button_moves_no_button() {
    let mut w = open_form();
    run(&mut w, &[Do::Walk(CreateStep::Review)]);
    let labels = ["Cancel", "Back", "Create only", "Create & start"];
    let places = |w: &mut Win| -> Vec<egui::Rect> {
        w.frame(Vec::new());
        labels.iter().map(|l| text_from(w, l, 0.0)).collect()
    };
    go(&mut w, WizardRow::CreateStart);
    let first = places(&mut w);
    w.press(KeyCode::Left);
    w.press(KeyCode::Left);
    assert_eq!(focused(&w.app), WizardRow::Back);
    assert_eq!(places(&mut w), first, "the buttons stay where they were");
    let y = first[0].center().y;
    assert!(
        first.iter().all(|r| (r.center().y - y).abs() < 0.5),
        "one line: {first:?}"
    );
    let accent = theme::accent(dark(&w));
    let back = first[1];
    let ringed = (w.rings.iter()).any(|(r, c)| *c == accent && r.contains_rect(back));
    assert!(ringed, "the cursor's ring: {:?}", w.rings);
}

#[test]
fn the_wizard_fills_its_main_action_in_the_accent() {
    let mut w = open_form();
    let accent = theme::accent(dark(&w));
    let filled = |w: &Win, label: &str| fills_behind(w, text_from(w, label, 0.0)).contains(&accent);
    assert!(filled(&w, "Next"));
    assert!(!filled(&w, "Back") && !filled(&w, "Cancel"));
    run(&mut w, &[Do::Walk(CreateStep::Review)]);
    w.frame(Vec::new());
    assert!(filled(&w, "Create & start"));
    assert!(!filled(&w, "Create only"));
}

/// Where a tile's title is drawn: under the step's heading and right of its left edge, so not the
/// rail's answer of the same name.
fn tile_title(w: &Win, name: &str) -> egui::Rect {
    let (heading, _) = form(&w.app).step_heading(form(&w.app).wizard_step(), &w.app.catalog);
    let heading = text_from(w, &heading, 0.0);
    let found = w
        .drawn_at
        .iter()
        .find(|(t, r)| t == name && r.top() > heading.bottom() && r.left() >= heading.left());
    found.unwrap_or_else(|| panic!("{name}: {:?}", w.drawn)).1
}

/// The right edge of the rail: everything drawn left of the step's heading is the rail's.
fn rail_edge(w: &Win) -> f32 {
    let (heading, _) = form(&w.app).step_heading(form(&w.app).wizard_step(), &w.app.catalog);
    text_from(w, &heading, 0.0).left()
}

/// Is `text` drawn on the rail?
fn on_rail(w: &Win, text: &str) -> Option<egui::Rect> {
    let edge = rail_edge(w);
    (w.drawn_at.iter())
        .find(|(t, r)| t == text && r.right() < edge)
        .map(|(_, r)| *r)
}

/// The rail shows the sbx name, which follows the account, and each step's answers, also on the
/// Review. A click on a step, on a Review answer or on its Edit button goes to that step.
#[test]
fn the_wizard_rail_follows_the_answers_and_leads_back() {
    let mut w = open_form();
    assert!(
        on_rail(&w, "claude-litellm-proj").is_some(),
        "{:?}",
        w.drawn_at
    );
    assert!(on_rail(&w, "LiteLLM").is_some(), "the provider's answer");
    assert!(on_rail(&w, "Claude Code").is_some(), "the harness's answer");
    run(
        &mut w,
        &[
            Do::Enter(provider(Provider::Anthropic)),
            Do::Enter(WizardRow::Connection(ConnChoice::Token)),
        ],
    );
    w.frame(Vec::new());
    assert!(
        on_rail(&w, "claude-token-proj").is_some(),
        "{:?}",
        w.drawn_at
    );

    let mut w = open_form();
    run(
        &mut w,
        &[
            Do::Walk(CreateStep::Workspace),
            Do::Go(WizardRow::Field(CreateRow::Clone)),
        ],
    );
    w.press(KeyCode::Char(' '));
    assert!(on_rail(&w, "Clone mode: on").is_some(), "{:?}", w.drawn_at);
    let kits = on_rail(&w, "Kits").expect("the step's title").center();
    click(&mut w, kits);
    assert_eq!(form(&w.app).wizard_step(), CreateStep::Kits);
    w.frame(Vec::new());
    let review = on_rail(&w, "Review").expect("the step's title").center();
    click(&mut w, review);
    assert_eq!(form(&w.app).wizard_step(), CreateStep::Review);
    w.frame(Vec::new());
    assert!(
        on_rail(&w, "claude-litellm-proj").is_some(),
        "the rail stays on the Review"
    );
    let edge = rail_edge(&w);
    let group = (w.drawn_at.iter())
        .filter(|(t, r)| t == "Kits" && r.left() > edge)
        .map(|(_, r)| r.center().y)
        .next()
        .unwrap_or_else(|| panic!("the Review has a Kits group: {:?}", w.drawn_at));
    let value = (w.drawn_at.iter())
        .filter(|(t, r)| t.starts_with("none") && r.left() > edge && r.center().y > group)
        .min_by(|a, b| a.1.top().total_cmp(&b.1.top()))
        .map(|(_, r)| r.center())
        .unwrap_or_else(|| panic!("none is drawn under Kits: {:?}", w.drawn_at));
    let edit = (w.drawn_at.iter())
        .find(|(t, r)| t == "Edit" && (r.center().y - group).abs() < 4.0)
        .map(|(_, r)| r.center())
        .unwrap_or_else(|| panic!("the Kits group has an Edit button: {:?}", w.drawn_at));
    click(&mut w, value);
    assert_eq!(form(&w.app).wizard_step(), CreateStep::Kits);
    run(&mut w, &[Do::Walk(CreateStep::Review)]);
    w.frame(Vec::new());
    click(&mut w, edit);
    assert_eq!(form(&w.app).wizard_step(), CreateStep::Kits, "Edit");
}

#[test]
fn a_bad_field_says_what_is_wrong_under_it_once_left() {
    let mut w = open_form();
    run(
        &mut w,
        &[
            Do::Walk(CreateStep::Workspace),
            Do::Type(CreateRow::Ports, "nope"),
        ],
    );
    w.frame(Vec::new());
    let problem = "“nope” is not a port mapping";
    let said = |w: &Win| w.drawn.iter().any(|t| t.starts_with(problem));
    let red = theme::danger(dark(&w));
    assert!(!said(&w), "not while it is typed: {:?}", w.drawn);
    assert!(!w.rings.iter().any(|(_, c)| *c == red));
    w.press(KeyCode::Down);
    assert!(said(&w), "{:?}", w.drawn);
    assert!(w.rings.iter().any(|(_, c)| *c == red), "the box's edge");
    w.press(KeyCode::Up);
    assert!(said(&w), "and still when the cursor is back on it");
}

#[test]
fn a_click_on_a_segment_or_a_tile_picks_that_value() {
    let beta = testkit::connection("beta", "https://beta.example.org");
    let root = testkit::litellm_root_with(&[testkit::acme(), beta]);
    let at_root = move |app: &mut App| app.litellm_root = root.clone();
    let mut w = Win::new(golden_app(&at_root));
    w.type_text("n");
    run(&mut w, &[Do::Walk(CreateStep::Connection)]);
    w.frame(Vec::new());
    // Each connection is a tile: its name, its URL under it.
    let tile = text_from(&w, "https://beta.example.org", 0.0).center();
    click(&mut w, tile);
    assert_eq!(form(&w.app).connection.litellm.as_deref(), Some("beta"));
    let tile = text_from(&w, "New connection…", 0.0).center();
    click(&mut w, tile);
    assert_eq!(form(&w.app).connection.litellm, None);
    let tile = text_from(&w, "https://litellm.example.com", 0.0).center();
    click(&mut w, tile);
    assert_eq!(form(&w.app).connection.litellm.as_deref(), Some("acme"));

    run(&mut w, &[Do::Walk(CreateStep::Workspace)]);
    w.frame(Vec::new());
    let segment = text_from(&w, "Read-write", 0.0).center();
    click(&mut w, segment);
    assert_eq!(form(&w.app).workspace.skills, sbx::SkillsMode::Readwrite);
    let segment = text_from(&w, "Off", 0.0).center();
    click(&mut w, segment);
    assert_eq!(form(&w.app).workspace.skills, sbx::SkillsMode::Off);
}

#[test]
fn the_rails_sbx_name_is_a_name_not_a_field() {
    let mut w = open_form();
    w.frame(Vec::new());
    let name = on_rail(&w, "claude-litellm-proj").expect("the rail names the sandbox");
    let caption = on_rail(&w, crate::wizard::SBX_NAME).expect("under a caption");
    assert!(caption.bottom() <= name.top(), "{caption:?} over {name:?}");
    let visuals = w.ctx.global_style().visuals.clone();
    let behind = |fill| (w.fills.iter()).any(|(r, c)| *c == fill && r.contains_rect(name));
    assert!(
        behind(visuals.code_bg_color),
        "on the code fill: {:?}",
        w.fills
    );
    assert!(!behind(visuals.extreme_bg_color), "not in a field's well");
    let ringed = (w.rings.iter()).any(|(r, _)| r.contains_rect(name));
    assert!(!ringed, "no border round the name: {:?}", w.rings);
    assert!(
        w.mono.iter().any(|m| m == "claude-litellm-proj"),
        "{:?}",
        w.mono
    );
}

#[test]
fn a_narrow_window_keeps_the_rails_marks_and_the_name() {
    let mut w = open_form();
    let line = |w: &Win, name: &str| tile_title(w, name).top();
    assert_eq!(line(&w, "Anthropic"), line(&w, "OpenAI"), "three to a line");
    assert!(on_rail(&w, "Harness").is_some());
    w.size = egui::vec2(600.0, 700.0);
    w.frame(Vec::new());
    w.frame(Vec::new());
    assert!(on_rail(&w, "Harness").is_none(), "{:?}", w.drawn_at);
    let edge = rail_edge(&w);
    let title = text_from(&w, "New sandbox", edge);
    let name = text_from(&w, "claude-litellm-proj", edge);
    assert!(name.top() > title.bottom(), "{name:?}");
    assert!(line(&w, "Anthropic") < line(&w, "OpenAI"), "two to a line");
    let mark = (w.drawn_at.iter())
        .find(|(t, r)| t == "5" && r.right() < edge)
        .map(|(_, r)| r.center())
        .unwrap_or_else(|| panic!("the rail marks step 5: {:?}", w.drawn_at));
    click(&mut w, mark);
    assert_eq!(form(&w.app).wizard_step(), CreateStep::Kits);
}

/// The shell chords (Cmd with A, E, K, U or W) never reach core, so they cannot edit a field.
#[test]
fn a_field_edits_the_way_the_platforms_fields_do() {
    use crate::form::form_key;
    let ctrl = |c| vec![KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)];
    let key = |key, modifiers| form_key(&pressed(key, modifiers));
    for letter in [Key::A, Key::E, Key::K, Key::U, Key::W] {
        assert!(key(letter, Modifiers::COMMAND).is_empty(), "{letter:?}");
        assert!(
            key(letter, Modifiers::CTRL | Modifiers::COMMAND).is_empty(),
            "{letter:?}"
        );
    }
    assert_eq!(key(Key::S, Modifiers::COMMAND), ctrl('s'), "Cmd-S creates");
    assert_eq!(
        key(Key::Backspace, Modifiers::ALT),
        ctrl('w'),
        "Option-Backspace"
    );
    let mac_cmd = Modifiers::MAC_CMD | Modifiers::COMMAND;
    assert_eq!(key(Key::Backspace, mac_cmd), ctrl('u'), "Cmd-Backspace");
    let ctrl_key = Modifiers::CTRL | Modifiers::COMMAND;
    assert_eq!(key(Key::Backspace, ctrl_key), ctrl('w'), "Ctrl-Backspace");
    let plain = key(Key::Backspace, Modifiers::NONE);
    assert_eq!(plain, vec![KeyEvent::from(KeyCode::Backspace)]);

    let mut w = open_form();
    run(
        &mut w,
        &[
            Do::Walk(CreateStep::Workspace),
            Do::Type(CreateRow::Ports, "8080:3000 5173"),
        ],
    );
    w.frame(vec![pressed(Key::Backspace, Modifiers::ALT)]);
    assert_eq!(form(&w.app).workspace.ports.value, "8080:3000 ");
    w.frame(vec![pressed(Key::Backspace, mac_cmd)]);
    assert_eq!(form(&w.app).workspace.ports.value, "");
}

#[test]
fn the_kits_step_marks_the_kits_that_can_go_in_later() {
    let mut w = open_form();
    run(&mut w, &[Do::Walk(CreateStep::Kits)]);
    w.frame(Vec::new());
    let badge = sentence_case(aisbx_core::create::ADD_LATER);
    let marked: Vec<f32> = (w.drawn_at.iter())
        .filter(|(t, _)| *t == badge)
        .map(|(_, r)| r.center().y)
        .collect();
    let later: Vec<&str> = (w.app.catalog.addons.iter())
        .filter(|a| a.create_time_reason().is_none())
        .map(|a| a.kit.display_name.as_str())
        .collect();
    assert_eq!(marked.len(), later.len(), "{later:?}: {:?}", w.drawn);
    for name in later {
        let title = text_from(&w, name, 0.0).center().y;
        assert!(marked.iter().any(|y| (y - title).abs() < 6.0), "{name}");
    }
    let gloss = sentence_case(aisbx_core::create::ADD_LATER_GLOSS);
    assert!(w.drawn.contains(&gloss), "{:?}", w.drawn);
}

#[test]
fn the_generated_label_says_typing_replaces_it() {
    let mut w = open_form();
    run(
        &mut w,
        &[
            Do::Walk(CreateStep::Workspace),
            Do::Go(WizardRow::Field(CreateRow::Name)),
        ],
    );
    w.frame(Vec::new());
    let generated = sentence_case(aisbx_core::create::GENERATED_GLOSS);
    assert!(w.drawn.contains(&generated), "{:?}", w.drawn);
    w.type_text("api");
    w.frame(Vec::new());
    assert_eq!(form(&w.app).workspace.name.value, "api");
    assert!(!w.drawn.contains(&generated), "{:?}", w.drawn);
    let gloss = sentence_case(aisbx_core::create::field_gloss(CreateRow::Name));
    assert!(w.drawn.contains(&gloss), "{:?}", w.drawn);
}

/// `text` with its first letter in upper case, as the window writes core's lower-case lines.
fn sentence_case(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The bar between the way out and the way on says what the cursor's row does, so the keys read
/// core's help without a pointer; the create shortcut moves to the rail.
#[test]
fn the_wizard_bar_says_what_the_cursors_row_does() {
    let mut w = open_form();
    w.frame(Vec::new());
    let note_of = |w: &Win| {
        let help = form(&w.app).help(&w.app, focused(&w.app));
        let start: String = help.text.chars().take(24).collect();
        let back = text_from(w, "Back", 0.0);
        let beside = |r: &egui::Rect| r.top() < back.bottom() && r.bottom() > back.top();
        let found = (w.drawn_at.iter()).find(|(t, r)| t.starts_with(&start) && beside(r));
        found
            .unwrap_or_else(|| panic!("{start} in the bar: {:?}", w.drawn))
            .1
    };
    let note = note_of(&w);
    let (cancel, back) = (text_from(&w, "Cancel", 0.0), text_from(&w, "Back", 0.0));
    assert!(
        note.left() > cancel.right() && note.right() < back.left(),
        "{note:?}"
    );
    let first = focused(&w.app);
    w.press(KeyCode::Up);
    assert_ne!(focused(&w.app), first);
    note_of(&w);
    let hint = (w.drawn_at.iter()).find(|(t, _)| t.ends_with("creates and starts from any step"));
    let (_, hint) = hint.unwrap_or_else(|| panic!("the shortcut: {:?}", w.drawn));
    assert!(hint.right() < rail_edge(&w), "on the rail: {hint:?}");
}
