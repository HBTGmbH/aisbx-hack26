//! Golden tests for the account, secrets, skills, approvals, kits and Connect screens: the same
//! keys as egui events reach the same confirmation and job as through core's `on_key`. Also the
//! same-frame cases, the masked fields and the selected row's buttons.

use aisbx_core::app::{App, FoldersRow, KitsRow, Modal, Screen};
use aisbx_core::input::KeyCode;
use aisbx_core::kits::Catalog;
use aisbx_core::model::{self, Auth};
use aisbx_core::repo_skills_ui::{RepoScope, RepoSkillsForm, RepoView};
use aisbx_core::skill_select::Selection;
use aisbx_core::testkit;
use eframe::egui::{self, Key, Modifiers};

use crate::form_golden::{click, events, pressed, typed, Core, Driver, Win};

/// The cursor of the screen that is open.
fn focus(app: &App) -> usize {
    match &app.screen {
        Screen::AuthEdit(f) => f.focus,
        Screen::ConnSwitch(f) => f.focus,
        Screen::Secrets(f) => f.focus,
        Screen::Connection(f) => f.focus,
        Screen::Skills(f) => f.focus,
        Screen::RepoSkills(f) => f.focus,
        Screen::Approvals(f) => f.focus,
        other => panic!("no S7 screen is open: {other:?}"),
    }
}

/// Where `row` (as `{:?}` writes it) is among the open screen's own rows.
fn at(app: &App, row: &str) -> usize {
    fn names<R: std::fmt::Debug>(rows: Vec<R>) -> Vec<String> {
        rows.iter().map(|r| format!("{r:?}")).collect()
    }
    let rows = match &app.screen {
        Screen::AuthEdit(f) => names(f.rows()),
        Screen::ConnSwitch(f) => names(f.rows()),
        Screen::Connection(f) => names(f.rows()),
        Screen::RepoSkills(f) => names(f.rows()),
        Screen::Secrets(_) => names(app.secret_rows()),
        Screen::Skills(_) => names(app.skill_rows()),
        other => panic!("no rows for {other:?}"),
    };
    let found = rows.iter().position(|r| r == row);
    found.unwrap_or_else(|| panic!("{row} in {rows:?}"))
}

/// ↓ until the cursor is on `row`.
fn go(d: &mut dyn Driver, row: &str) -> usize {
    let target = at(d.app(), row);
    for _ in 0..80 {
        if focus(d.app()) == target {
            break;
        }
        d.press(KeyCode::Down);
    }
    assert_eq!(focus(d.app()), target, "{:?}", d.app().screen);
    target
}

/// What the keys led to: the window open and, for a job, every step as the runner sees it.
fn outcome(app: &App) -> String {
    let job = |pending: &aisbx_core::app::PendingJob| {
        let (mut shape, mut job) = (Vec::new(), Some(pending));
        while let Some(j) = job {
            for s in &j.steps {
                shape.push((&j.title, &s.label, &s.argv, s.stdin.clone()));
            }
            job = j.post.then.as_deref();
        }
        format!("{shape:?}")
    };
    match &app.modal {
        Some(Modal::Running(_, pending)) => format!("job {}", job(pending)),
        Some(Modal::Confirm(c)) => format!("confirm {:?} {:?} {}", c.title, c.body, job(&c.job)),
        Some(Modal::Prompt(p)) => format!("prompt {:?} {:?}", p.ask, p.input.value),
        Some(Modal::Help) => "help".into(),
        None => format!(
            "{:?} {:?}",
            std::mem::discriminant(&app.screen),
            app.status.as_ref().map(|(t, e, _)| (t, e))
        ),
    }
}

/// Run `script` once through core's `on_key` and once through the window's frames, from the same
/// app; both reach the same outcome, which is returned.
fn same(setup: &dyn Fn() -> App, script: &dyn Fn(&mut dyn Driver)) -> String {
    let mut core = Core(setup());
    script(&mut core);
    let want = outcome(&core.0);
    let mut win = Win::new(setup());
    script(&mut win);
    let got = outcome(&win.app);
    assert_eq!(got, want);
    got
}

/// Answer the confirmation: core's `y`, the window's `y` key.
fn yes(d: &mut dyn Driver) {
    d.press(KeyCode::Char('y'));
}

fn on_login() -> App {
    let mut sb = model::sample("claude-anthropic-proj", Some("claude-anthropic"), &[]);
    sb.auth = Some(Auth::Login);
    let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
    app.secrets = Some(testkit::sample_secrets());
    app.cursor = 0;
    app
}

fn on_litellm() -> App {
    let sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
    let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
    // One folder for every app here, so the two runs of a script name the same kit path.
    static ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let beta = || testkit::connection("beta", "https://gw.example.org");
    let root = ROOT.get_or_init(|| testkit::litellm_root_with(&[testkit::acme(), beta()]));
    app.litellm_root = root.clone();
    app.secrets = Some(testkit::sample_secrets());
    app.cursor = 0;
    app
}

#[test]
fn a_new_token_through_the_window_is_the_same_rebuild() {
    let got = same(&on_login, &|d| {
        d.type_text("A");
        for row in ["UseToken", "TokenNew"] {
            go(d, row);
            d.press(KeyCode::Enter);
        }
        go(d, "Name");
        d.press(KeyCode::End);
        (0..20).for_each(|_| d.press(KeyCode::Backspace));
        d.type_text("team");
        go(d, "Value");
        d.type_text("sk-ant-oat01-new");
        go(d, "Apply");
        d.press(KeyCode::Enter);
        yes(d);
    });
    assert!(got.starts_with("job "), "{got}");
    assert!(
        got.contains("sk-ant-oat01-new"),
        "the value goes on stdin: {got}"
    );
}

#[test]
fn moving_to_another_connection_through_the_window_is_the_same_rebuild() {
    let got = same(&on_litellm, &|d| {
        d.type_text("A");
        assert!(matches!(d.app().screen, Screen::ConnSwitch(_)));
        d.press(KeyCode::Right);
        go(d, "Apply");
        d.press(KeyCode::Enter);
    });
    assert!(got.starts_with("confirm "), "{got}");
}

#[test]
fn rotating_a_token_through_the_window_is_the_same_job() {
    let got = same(&on_login, &|d| {
        d.type_text("S");
        go(d, "Token(\"work\")");
        d.press(KeyCode::Enter);
        assert!(matches!(d.app().modal, Some(Modal::Prompt(_))));
        // An idle frame: the window's prompt field takes the keyboard.
        d.type_text("");
        d.type_text("sk-ant-oat01-rotated");
        d.press(KeyCode::Enter);
    });
    assert!(
        got.starts_with("job ") && got.contains("sk-ant-oat01-rotated"),
        "{got}"
    );
}

#[test]
fn a_new_connection_and_a_delete_through_the_window_are_the_same_jobs() {
    let fill = |d: &mut dyn Driver| {
        d.type_text("S");
        d.type_text("n");
        for (row, text) in [
            ("Name", "beta"),
            ("Url", "https://gw.example.org"),
            ("Key", "sk-litellm-new"),
        ] {
            go(d, row);
            d.type_text(text);
        }
        go(d, "Save");
        d.press(KeyCode::Enter);
    };
    let got = same(&on_login, &fill);
    assert!(
        got.starts_with("job ") && got.contains("sk-litellm-new"),
        "{got}"
    );

    let got = same(&on_login, &|d| {
        d.type_text("S");
        go(d, "Token(\"work\")");
        d.type_text("d");
        yes(d);
    });
    assert!(got.starts_with("job ") && got.contains("rm"), "{got}");
}

#[test]
fn adding_skills_through_the_window_is_the_same_job() {
    let got = same(&on_login, &|d| {
        d.type_text("K");
        go(d, "AddRepo");
        d.press(KeyCode::Enter);
        d.type_text("");
        d.type_text("owner/repo");
        d.press(KeyCode::Enter);
    });
    assert!(
        got.starts_with("job ") && got.contains("owner/repo"),
        "{got}"
    );
}

/// The picker as it opens on a host with one fetched repo — core opens it empty under test.
fn with_repo(app: &mut App) {
    let repo = RepoView {
        id: "o-r".into(),
        source: "o/r".into(),
        git_ref: None,
        commit: Some("0123456789abcdef".into()),
        problem: None,
        skills: vec!["alpha".into(), "beta".into()],
        skipped: Vec::new(),
    };
    if let Screen::RepoSkills(form) = &app.screen {
        let scope = form.scope.clone();
        app.screen = Screen::RepoSkills(RepoSkillsForm::from_parts(
            scope,
            vec![repo],
            Ok(Selection::default()),
        ));
    }
}

#[test]
fn repo_skills_tick_save_and_sync_through_the_window_are_the_same_job() {
    let got = same(&on_login, &|d| {
        d.type_text("i");
        with_repo(d.app_mut());
        let Screen::RepoSkills(f) = &d.app().screen else {
            panic!("the picker is open")
        };
        assert_eq!(f.scope, RepoScope::Sandbox("claude-anthropic-proj".into()));
        go(d, "Skill { repo: \"o-r\", name: \"alpha\", locked: false }");
        d.type_text(" ");
        for row in ["Save", "SyncNow"] {
            go(d, row);
            d.press(KeyCode::Enter);
        }
    });
    assert!(got.starts_with("job ") && got.contains("\"sync\""), "{got}");
}

fn with_approvals() -> App {
    let mut app = on_login();
    app.approvals = vec![
        testkit::held("claude-anthropic-proj", "example.org:443"),
        testkit::held("claude-anthropic-proj", "example.net:80"),
    ];
    app
}

#[test]
fn allowing_a_request_through_the_window_is_the_same_job() {
    let got = same(&with_approvals, &|d| {
        d.type_text("y");
        d.type_text("a");
    });
    assert!(got.starts_with("job ") && got.contains("allow"), "{got}");
}

#[test]
fn the_settings_page_turns_each_setting_with_its_own_button() {
    use aisbx_core::sbx::{self, CLIPBOARD_TEXT_COPY, TELEMETRY_DISABLED};
    for (needle, key) in [
        ("Turn off", TELEMETRY_DISABLED),
        ("Turn on", CLIPBOARD_TEXT_COPY),
    ] {
        let mut w = win(on_login());
        w.type_text("T");
        let Screen::Settings(form) = &mut w.app.screen else {
            panic!("T opens the settings")
        };
        form.values = vec![
            Some(testkit::bool_setting(TELEMETRY_DISABLED, false)),
            Some(testkit::bool_setting(CLIPBOARD_TEXT_COPY, false)),
            Some(testkit::bool_setting(sbx::CLIPBOARD_IMAGE_PASTE, false)),
            Some(testkit::bool_setting(sbx::SANDBOX_FUSE, true)),
        ];
        form.skills = Some(testkit::skills_default("readonly"));
        w.frame(Vec::new());
        let drawn = w.drawn.join("\n");
        for text in [
            "Settings",
            "Telemetry",
            "Clipboard copy",
            "Image paste",
            "FUSE",
            "Turn off",
            "Turn on",
            "Close",
        ] {
            assert!(drawn.contains(text), "{text}: {drawn}");
        }
        assert!(
            w.drawn.iter().any(|t| t == "on"),
            "telemetry's state: {drawn}"
        );
        assert!(
            w.drawn.iter().any(|t| t == "off"),
            "the clipboard's: {drawn}"
        );
        // The calm colour for what keeps more to the sandbox, the warning for the rest.
        let dark = w.ctx.global_style().visuals.dark_mode;
        let (warn, calm) = (crate::theme::warn(dark), crate::theme::running(dark));
        let colour = |text: &str| w.colored.iter().find(|(t, _)| t == text).map(|(_, c)| *c);
        assert_eq!(colour("on"), Some(warn), "telemetry on: {:?}", w.colored);
        assert_eq!(
            colour("off"),
            Some(calm),
            "no clipboard copy: {:?}",
            w.colored
        );
        let pos = drawn_at(&mut w, needle);
        click(&mut w, pos);
        let Some(Modal::Running(_, pending)) = &w.app.modal else {
            panic!("{needle} runs the job: {:?}", w.app.status)
        };
        assert_eq!(pending.steps[0].argv, sbx::setting_set_argv(key, "true"));
    }
}

#[test]
fn the_settings_page_sets_the_skills_default_with_a_mode_button() {
    let mut w = win(on_login());
    w.type_text("T");
    let Screen::Settings(form) = &mut w.app.screen else {
        panic!("T opens the settings")
    };
    form.skills = Some(testkit::skills_default("readonly"));
    w.frame(Vec::new());
    let drawn = w.drawn.join("\n");
    for text in ["Skills default", "read-only", "Off", "Read-write"] {
        assert!(drawn.contains(text), "{text}: {drawn}");
    }
    let pos = drawn_at(&mut w, "Read-write");
    click(&mut w, pos);
    let Some(Modal::Running(_, pending)) = &w.app.modal else {
        panic!("the mode's button runs the job: {:?}", w.app.status)
    };
    let set = aisbx_core::sbx::setting_set_argv(aisbx_core::sbx::SKILLS_DEFAULT_MODE, "readwrite");
    assert_eq!(pending.steps[0].argv, set);
}

// --- the same frame -------------------------------------------------------------------------

fn win(app: App) -> Win {
    let mut w = Win::new(app);
    w.frame(Vec::new());
    w
}

#[test]
fn s_then_d_in_one_frame_opens_the_screen_only() {
    let mut w = win(on_login());
    let mut frame = typed('S');
    frame.extend(typed('d'));
    w.frame(frame);
    assert!(matches!(w.app.screen, Screen::Secrets(_)));
    assert!(w.app.modal.is_none(), "no delete confirmation");
}

#[test]
fn escape_on_secrets_then_x_opens_no_shell() {
    let mut w = win(on_login());
    w.type_text("S");
    aisbx_core::terminal::opened();
    let mut frame = vec![pressed(Key::Escape, Modifiers::NONE)];
    frame.extend(typed('x'));
    w.frame(frame);
    assert!(matches!(w.app.screen, Screen::List));
    assert!(w.app.handoff.is_none() && !w.session.busy());
    assert!(aisbx_core::terminal::opened().is_empty());
}

/// Text in the same frame as Enter is dropped, not taken by the list, where `d` would delete.
#[test]
fn enter_on_a_service_row_then_text_lands_in_the_prompt() {
    let mut w = win(on_login());
    w.type_text("S");
    let at = go(&mut w, "Service { name: \"litellm\", scope: None }");
    let mut frame = events(KeyCode::Enter);
    frame.extend(typed('d'));
    w.frame(frame);
    let Some(Modal::Prompt(prompt)) = &w.app.modal else {
        panic!("the prompt is open: {:?}", w.app.modal)
    };
    assert_eq!(prompt.input.value, "");
    assert_eq!(focus(&w.app), at);
    w.frame(Vec::new());
    w.type_text("v1");
    let Some(Modal::Prompt(prompt)) = &w.app.modal else {
        panic!("the prompt is still open")
    };
    assert_eq!(prompt.input.value, "v1");
}

#[test]
fn a_then_down_on_approvals_allows_the_first_only() {
    let mut w = win(with_approvals());
    w.type_text("y");
    let mut frame = typed('a');
    frame.extend(events(KeyCode::Down));
    w.frame(frame);
    let Some(Modal::Running(_, pending)) = &w.app.modal else {
        panic!("an answer is running")
    };
    assert_eq!(pending.steps.len(), 1);
    assert!(pending.steps[0]
        .argv
        .iter()
        .any(|a| a.contains("example.org")));
    assert_eq!(focus(&w.app), 0);
}

/// Where the window drew `needle` in the last frame: the centre of its text.
fn drawn_at(w: &mut Win, needle: &str) -> egui::Pos2 {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1200.0, 760.0),
        )),
        ..Default::default()
    };
    let (app, chooser, session) = (&mut w.app, &mut w.chooser, &mut w.session);
    let mut output = w
        .ctx
        .run_ui(input, |ui| crate::shell::list(ui, app, chooser, session));
    output.textures_delta.clear();
    fn find(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
        match shape {
            egui::Shape::Text(t) if t.galley.text().contains(needle) => {
                Some(t.pos + t.galley.rect.center().to_vec2())
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|s| find(s, needle)),
            _ => None,
        }
    }
    output
        .shapes
        .iter()
        .find_map(|c| find(&c.shape, needle))
        .unwrap_or_else(|| panic!("{needle} is drawn"))
}

#[test]
fn a_click_on_an_approval_allows_nothing() {
    let mut w = win(with_approvals());
    w.type_text("y");
    let pos = drawn_at(&mut w, "example.net:80");
    click(&mut w, pos);
    w.frame(Vec::new());
    assert!(w.app.modal.is_none(), "{:?}", w.app.modal);
    assert_eq!(focus(&w.app), 1);
    assert_eq!(w.app.approvals.len(), 2);
    let pos = drawn_at(&mut w, "Close");
    click(&mut w, pos);
    assert!(matches!(w.app.screen, Screen::List));
}

#[test]
fn escape_on_the_account_then_s_opens_nothing() {
    let mut w = win(on_login());
    w.type_text("A");
    assert!(matches!(w.app.screen, Screen::AuthEdit(_)));
    let mut frame = vec![pressed(Key::Escape, Modifiers::NONE)];
    frame.extend(typed('S'));
    w.frame(frame);
    assert!(matches!(w.app.screen, Screen::List));
}

// --- masked fields ----------------------------------------------------------------------------

/// No character of `secret` is drawn, and its dots are.
fn hidden(w: &Win, secret: &str) {
    let drawn = w.drawn.join("\n");
    assert!(
        drawn.contains(&"•".repeat(secret.chars().count())),
        "{drawn}"
    );
    assert!(!drawn.chars().any(|c| secret.contains(c)), "{drawn}");
}

const SECRET: &str = "жшщэюя";

#[test]
fn the_token_field_of_the_account_form_is_masked() {
    let mut w = win(on_login());
    w.type_text("A");
    go(&mut w, "UseToken");
    w.press(KeyCode::Enter);
    go(&mut w, "Value");
    w.type_text(SECRET);
    let Screen::AuthEdit(f) = &w.app.screen else {
        panic!("the account form is open")
    };
    assert_eq!(f.token.value, SECRET);
    hidden(&w, SECRET);
    w.press(KeyCode::Up);
    hidden(&w, SECRET);
}

#[test]
fn the_key_fields_of_the_connection_forms_are_masked() {
    let mut w = win(on_login());
    w.type_text("S");
    w.type_text("n");
    let key = go(&mut w, "Key");
    w.type_text(SECRET);
    let Screen::Connection(f) = &w.app.screen else {
        panic!("the dialog is open")
    };
    assert!(f.rows()[key].masked());
    assert_eq!(f.fields.key.value, SECRET);
    hidden(&w, SECRET);

    let mut w = win(on_litellm());
    w.type_text("A");
    // ← from the first connection wraps to *new connection…*, which opens the fields.
    w.press(KeyCode::Left);
    go(&mut w, "Key");
    w.type_text(SECRET);
    let Screen::ConnSwitch(f) = &w.app.screen else {
        panic!("the move form is open")
    };
    assert_eq!(f.new_conn.key.value, SECRET);
    hidden(&w, SECRET);
}

/// Also the custom secret that is not aisbx's, at the foot of a window tall enough for the page.
#[test]
fn the_secrets_screen_draws_sbx_masked_text() {
    let mut w = win(on_login());
    w.size = egui::vec2(1200.0, 1600.0);
    w.type_text("S");
    w.frame(Vec::new());
    let drawn = w.drawn.join("\n");
    assert!(drawn.contains("sk-ant********"), "{drawn}");
    assert!(drawn.contains("Anthropic subscription"), "{drawn}");
    assert!(drawn.contains("someone-elses-secret"), "{drawn}");
}

/// Kit volumes in RAM (`aisbx up --tmpfs-volumes`): the kits editor says that even `sbx kit add`
/// loses the session history, and the account form says that a rebuild does.
#[test]
fn the_notes_of_a_sandbox_with_ram_volumes_say_the_history_goes() {
    let mut sb = model::sample("box", Some("claude-anthropic"), &[]);
    sb.tmpfs_volumes = true;
    let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
    app.open_kits("box");
    let jdk = app
        .catalog
        .addons
        .iter()
        .position(|a| a.kit.name == "jdk25");
    if let Screen::Kits(form) = &mut app.screen {
        form.addons[jdk.unwrap()] = true;
    }
    let w = Win::new(app);
    let drawn = w.drawn.join("\n");
    assert!(drawn.contains("earlier sessions are lost"), "{drawn}");

    let mut form = testkit::auth_form(aisbx_core::app::AccountChoice::New);
    form.ram_volumes = true;
    let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
    app.screen = Screen::AuthEdit(form);
    let w = Win::new(app);
    let drawn = w.drawn.join("\n");
    assert!(drawn.contains("earlier sessions are lost"), "{drawn}");
}

/// Also: a ticked kit carries *Adding*, and a click on an item ticks it through core.
#[test]
fn the_kits_editor_marks_the_kits_that_can_be_added_later() {
    let sb = model::sample("box", Some("claude-anthropic"), &[]);
    let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
    app.open_kits("box");
    let later = app
        .catalog
        .addons
        .iter()
        .filter(|a| a.create_time_reason().is_none())
        .count();
    assert!(later > 0, "jdk25, rtk and statusline can be added later");
    let mut w = Win::new(app);
    let count = |w: &Win, text: &str| w.drawn.iter().filter(|t| t.as_str() == text).count();
    assert_eq!(count(&w, "Can add later"), later, "{:?}", w.drawn);
    assert!(!w.drawn.iter().any(|t| t.contains("needs a rebuild")));
    assert_eq!(count(&w, "Adding"), 0);

    let jdk = w
        .app
        .catalog
        .addons
        .iter()
        .position(|a| a.kit.name == "jdk25")
        .unwrap();
    let name = w.app.catalog.addons[jdk].kit.display_name.clone();
    let at = drawn_at(&mut w, &name);
    click(&mut w, at);
    w.frame(Vec::new());
    let Screen::Kits(form) = &w.app.screen else {
        panic!("the kits editor is open")
    };
    assert!(form.addons[jdk], "the click ticked {name}");
    assert_eq!(count(&w, "Adding"), 1, "{:?}", w.drawn);
}

#[test]
fn the_kits_editor_groups_the_kits_as_the_wizard_does() {
    let sb = model::sample("box", Some("claude-anthropic"), &[]);
    let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
    app.open_kits("box");
    let mut w = Win::new(app);
    w.size = egui::vec2(1200.0, 1800.0);
    (0..2).for_each(|_| w.frame(Vec::new()));
    let top = |w: &Win, text: &str| {
        (w.drawn_at.iter())
            .find(|(t, _)| t.starts_with(text))
            .map(|(_, r)| r.top())
            .unwrap_or_else(|| panic!("{text} is not drawn: {:?}", w.drawn))
    };
    let hosts = top(&w, crate::wizard::HOST_CACHES);
    let others = top(&w, crate::wizard::ADDITIONAL_KITS);
    assert!(hosts < others, "the host caches come first");
    for addon in &w.app.catalog.addons {
        // A long title is cut on its item: the start is enough to find it.
        let title: String = addon.kit.display_name.chars().take(8).collect();
        let at = top(&w, &title);
        match model::is_host_folder_kit(&addon.kit.name) {
            true => assert!(
                hosts < at && at < others,
                "{} is a host cache",
                addon.kit.name
            ),
            false => assert!(
                at > others,
                "{} comes after the host caches",
                addon.kit.name
            ),
        }
    }
}

/// The tooltip is core's help for the kit, with its first sentence whole, which the item cuts.
#[test]
fn a_kit_in_the_kits_editor_says_what_it_costs_under_the_pointer() {
    let sb = model::sample("box", Some("claude-anthropic"), &[]);
    let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
    app.open_kits("box");
    let headroom = (app.catalog.addons.iter()).position(|a| a.kit.name == "headroom");
    let help = match &app.screen {
        Screen::Kits(form) => form.help(&app.catalog, KitsRow::Addon(headroom.unwrap())),
        other => panic!("the kits editor is open: {other:?}"),
    };
    let cost = aisbx_core::create::ONLY_NOW_NOTE;
    assert!(help.text.starts_with(cost), "{}", help.text);
    let mut w = Win::new(app);
    let name = w.app.catalog.addons[headroom.unwrap()]
        .kit
        .display_name
        .clone();
    let at = drawn_at(&mut w, &name[..8]);
    // egui shows a tooltip once the pointer has rested for half a second, a frame a 60th.
    w.frame(vec![egui::Event::PointerMoved(at)]);
    (0..45).for_each(|_| w.frame(Vec::new()));
    assert!(w.drawn.contains(&help.text), "{:?}", w.drawn);
}

// --- the selected row's buttons -------------------------------------------------------------

/// After `open`, what a click on the button `label` does through the window, and what `key` does
/// through core: the same outcome, which is returned.
fn button_is_key(
    setup: &dyn Fn() -> App,
    open: &dyn Fn(&mut dyn Driver),
    key: KeyCode,
    label: &str,
) -> String {
    let mut core = Core(setup());
    open(&mut core);
    core.press(key);
    let want = outcome(&core.0);
    let mut w = Win::new(setup());
    open(&mut w);
    w.frame(Vec::new());
    let at = (w.drawn_at.iter())
        .find(|(t, _)| t == label)
        .unwrap_or_else(|| panic!("{label} is not drawn: {:?}", w.drawn))
        .1
        .center();
    click(&mut w, at);
    w.frame(Vec::new());
    assert_eq!(outcome(&w.app), want, "{label}");
    want
}

#[test]
fn a_requests_buttons_answer_it_as_its_keys_do() {
    let open = |d: &mut dyn Driver| d.type_text("y");
    let allow = button_is_key(&with_approvals, &open, KeyCode::Char('a'), "Allow");
    assert!(
        allow.starts_with("job ") && allow.contains("allow"),
        "{allow}"
    );
    let dismiss = button_is_key(&with_approvals, &open, KeyCode::Char('d'), "Dismiss");
    assert!(dismiss.contains("dismiss"), "{dismiss}");
}

#[test]
fn a_secret_rows_buttons_are_its_keys() {
    let open = |d: &mut dyn Driver| d.type_text("S");
    let sign_out = button_is_key(&on_login, &open, KeyCode::Char('d'), "Sign out");
    assert!(sign_out.starts_with("confirm"), "{sign_out}");
    let new = button_is_key(
        &on_login,
        &open,
        KeyCode::Char('n'),
        "New LiteLLM connection",
    );
    assert!(
        !new.starts_with("confirm") && !new.starts_with("job"),
        "{new}"
    );
    let on_acme = |d: &mut dyn Driver| {
        d.type_text("S");
        go(d, "Connection(\"acme\")");
    };
    button_is_key(&on_login, &on_acme, KeyCode::Enter, "Edit");
}

#[test]
fn a_secret_row_offers_only_what_its_keys_do_there() {
    use aisbx_core::app::SecretRow;
    let app = on_login();
    let labels = |row: SecretRow| -> Vec<&str> {
        let actions = crate::secrets::row_actions(&app, &row);
        actions.iter().map(|(label, _, _)| *label).collect()
    };
    assert_eq!(labels(SecretRow::Oauth), ["Sign out"]);
    assert_eq!(
        labels(SecretRow::AgyLogin),
        Vec::<&str>::new(),
        "sbx lists no sign-in"
    );
    assert_eq!(
        labels(SecretRow::Connection("acme".into())),
        ["Edit", "Delete"]
    );
    assert_eq!(
        labels(SecretRow::Token("work".into())),
        ["Replace", "Delete"]
    );
    let service = SecretRow::global_service;
    assert_eq!(
        labels(service("mcp:slack:client_secret")),
        ["Replace", "Delete"]
    );
    assert_eq!(labels(service("never-stored")), ["Store"]);
    assert_eq!(labels(SecretRow::Close), Vec::<&str>::new());
}

#[test]
fn a_skills_remove_button_is_d() {
    let with_store = || {
        let mut app = on_login();
        app.skills = Some(aisbx_core::sbx::SkillsStore {
            store: "/store".into(),
            skills: vec!["pdf".into()],
        });
        app
    };
    let open = |d: &mut dyn Driver| d.type_text("K");
    let remove = button_is_key(&with_store, &open, KeyCode::Char('d'), "Remove");
    assert!(remove.starts_with("confirm"), "{remove}");
}

#[test]
fn a_paragraph_runs_no_wider_than_a_readable_line() {
    let mut w = win(on_login());
    w.frame(typed('S'));
    w.frame(Vec::new());
    let notes = on_login()
        .secret_notes(&aisbx_core::app::SecretRow::Oauth)
        .join(" ");
    let (_, rect) = (w.drawn_at.iter())
        .find(|(t, _)| *t == notes)
        .unwrap_or_else(|| panic!("the /login's notes are drawn: {:?}", w.drawn));
    assert!(rect.width() <= 640.0, "{rect:?}");
}

#[test]
fn connect_opens_an_app_by_its_button() {
    let mut w = win(on_login());
    w.frame(typed('o'));
    w.frame(Vec::new());
    assert!(!w.drawn.iter().any(|t| t.contains('↵')), "{:?}", w.drawn);
    let opens = (w.drawn.iter())
        .filter(|t| *t == crate::connect::OPEN)
        .count();
    assert!(opens >= 1, "{:?}", w.drawn);
    // The terminal is one the window opens: ↵ on it would hand it over.
    let pos = drawn_at(&mut w, "Terminal (ssh)");
    aisbx_core::terminal::opened();
    click(&mut w, pos);
    w.frame(Vec::new());
    let Screen::Connect(form) = &w.app.screen else {
        panic!("still on Connect: a click selects")
    };
    let rows = form.rows();
    assert!(
        matches!(rows[form.focus], aisbx_core::app::ConnectRow::Target(0)),
        "{rows:?}"
    );
    assert!(w.app.handoff.is_none() && w.app.modal.is_none() && w.app.status.is_none());
    assert!(aisbx_core::terminal::opened().is_empty() && !w.session.busy());
}

#[test]
fn the_connection_arrows_step_as_the_arrow_keys_do() {
    let picked = |app: &App| match &app.screen {
        Screen::ConnSwitch(f) => f.picked().map(|c| c.name.clone()),
        other => panic!("the move form is open: {other:?}"),
    };
    for (arrow, key) in [("›", KeyCode::Right), ("‹", KeyCode::Left)] {
        let mut core = Core(on_litellm());
        core.type_text("A");
        let before = picked(&core.0);
        core.press(key);
        let mut w = Win::new(on_litellm());
        w.type_text("A");
        w.frame(Vec::new());
        let at = (w.drawn_at.iter())
            .find(|(t, _)| t == arrow)
            .unwrap_or_else(|| panic!("{arrow} is drawn: {:?}", w.drawn))
            .1
            .center();
        click(&mut w, at);
        w.frame(Vec::new());
        assert_ne!(picked(&core.0), before, "{arrow} moves");
        assert_eq!(picked(&w.app), picked(&core.0), "{arrow}");
    }
}

#[test]
fn the_folders_screen_draws_the_list_and_its_buttons_act_on_their_folder() {
    use aisbx_core::folders::{self, Access, Entry};
    let nanos = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH))
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let state = std::env::temp_dir().join(format!("aisbx-tests-gui-folders-{nanos}"));
    let _ = std::fs::remove_dir_all(&state);
    let entry = Entry {
        path: "/srv/kb".into(),
        access: Access::Write,
        purpose: "the knowledge base".into(),
    };
    folders::put(&state, entry).unwrap();
    let mut app = on_login();
    app.mounts_state = state.clone();
    let mut w = win(app);
    w.type_text("F");
    w.frame(Vec::new());
    let drawn = w.drawn.join("\n");
    for text in [
        "Host folders",
        "/srv/kb",
        "the knowledge base",
        "Read-write",
        "Add a folder",
        "Add folder",
    ] {
        assert!(drawn.contains(text), "{text}: {drawn}");
    }
    // The path in the code face, as the field takes it; read-write, where the agent can delete,
    // in the warning colour.
    assert!(w.mono.iter().any(|t| t == "/srv/kb"), "{:?}", w.mono);
    let warn = crate::theme::warn(w.ctx.global_style().visuals.dark_mode);
    assert!(
        (w.colored.iter()).any(|(t, c)| t == "Read-write" && *c == warn),
        "{:?}",
        w.colored
    );
    // Selected, the folder draws no key help under it: its buttons are on its line.
    let pos = drawn_at(&mut w, "the knowledge base");
    click(&mut w, pos);
    w.frame(Vec::new());
    assert!(
        matches!(&w.app.screen, Screen::Folders(f) if f.focus == 0),
        "the click selects the folder"
    );
    let drawn = w.drawn.join("\n");
    let Screen::Folders(f) = &w.app.screen else {
        panic!("the folders screen is open")
    };
    let help = f.help(FoldersRow::Folder(0)).text;
    assert!(help.starts_with("Read-write: "), "{help}");
    assert!(!w.drawn.contains(&help), "{drawn}");
    // Edit puts it in the fields, and the bar's button says it saves the change.
    let pos = drawn_at(&mut w, "Edit");
    click(&mut w, pos);
    w.frame(Vec::new());
    let drawn = w.drawn.join("\n");
    for text in ["Change the folder", "Save changes", "Add a new one instead"] {
        assert!(drawn.contains(text), "{text}: {drawn}");
    }
    assert!(!w.drawn.iter().any(|t| t == "Add folder"), "{drawn}");
    let pos = drawn_at(&mut w, "Add a new one instead");
    click(&mut w, pos);
    w.frame(Vec::new());
    assert!(w.drawn.iter().any(|t| t == "Add folder"), "{:?}", w.drawn);
    // The fields are empty again: the button's key was not typed into the path.
    let Screen::Folders(f) = &w.app.screen else {
        panic!("still the folders screen")
    };
    assert_eq!((f.editing, f.path.value.as_str()), (None, ""));
    // The access radios are a click each; the fields start read-only.
    let pos = drawn_at(&mut w, "the agent can change and delete what is in it");
    click(&mut w, pos);
    let Screen::Folders(f) = &w.app.screen else {
        panic!("still the folders screen")
    };
    assert_eq!(f.access, Access::Write);
    let pos = drawn_at(&mut w, "Delete");
    click(&mut w, pos);
    let left = folders::load(&folders::config_path(&state)).unwrap();
    assert!(left.folders.is_empty(), "{left:?}");
    std::fs::remove_dir_all(&state).unwrap();
}

#[test]
fn browse_on_the_folders_screen_opens_the_picker_and_use_fills_the_field() {
    let nanos = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH))
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let tmp = std::env::temp_dir().join(format!("aisbx-tests-gui-browse-{nanos}"));
    std::fs::create_dir_all(tmp.join("kb")).unwrap();
    let tmp = std::fs::canonicalize(&tmp).unwrap();
    let mut app = on_login();
    app.mounts_state = tmp.join("state");
    let mut w = win(app);
    w.type_text("F");
    w.frame(Vec::new());
    let Screen::Folders(f) = &mut w.app.screen else {
        panic!("the folders screen")
    };
    f.path = aisbx_core::app::TextInput::new(tmp.display().to_string());
    w.frame(Vec::new());
    let pos = drawn_at(&mut w, "Browse…");
    click(&mut w, pos);
    w.frame(Vec::new());
    let drawn = w.drawn.join("\n");
    for text in ["Pick a host folder", "Use this folder", "→  kb"] {
        assert!(drawn.contains(text), "{text}: {drawn}");
    }
    assert!(!drawn.contains("Start here"), "{drawn}");
    let pos = drawn_at(&mut w, "→  kb");
    click(&mut w, pos);
    w.frame(Vec::new());
    let pos = drawn_at(&mut w, "Use this folder");
    click(&mut w, pos);
    w.frame(Vec::new());
    let Screen::Folders(f) = &w.app.screen else {
        panic!("back on the folders screen")
    };
    let home = aisbx_core::model::home_dir();
    assert_eq!(
        aisbx_core::folders::typed_path(&f.path.value, home.as_deref()).unwrap(),
        tmp.join("kb")
    );
    std::fs::remove_dir_all(&tmp).unwrap();
}
