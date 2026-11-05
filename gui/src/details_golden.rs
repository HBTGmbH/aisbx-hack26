//! Golden tests for the details tabs: a click or a digit opens a tab as core's `on_key` does, and
//! each tab draws core's pieces for `Insight::sample`, a line of keys as buttons named as the
//! actions are. Also the stopped notes, the chart's hover, and `u` and *Unpublish* on ports.

use aisbx_core::app::{App, Modal};
use aisbx_core::input::{KeyCode, KeyEvent};
use aisbx_core::insight::{self, Insight, Note, Tab};
use aisbx_core::kits::Catalog;
use aisbx_core::model::{self, Sandbox};
use aisbx_core::stats::Sample;
use aisbx_core::terminal;
use eframe::egui::{self, Key, Modifiers};

use crate::actions::{self, TAB_KEYS};
use crate::details::{prose, tag};
use crate::form_golden::{pressed, Win};
use crate::resources;
use aisbx_core::actions::Action;

fn sandbox(running: bool) -> Sandbox {
    let mut sb = model::sample("box", Some("claude-anthropic"), &["web"]);
    sb.running = running;
    sb.proxy = "http://gateway.docker.internal:3128".into();
    sb
}

/// A window on `sb`, its tabs holding `Insight::sample`'s data, tall enough to show every tab's
/// body: at 760 points the Resources tab scrolls.
fn window(sb: Sandbox) -> Win {
    let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
    app.insight = Insight::sample("box");
    let mut w = Win::new(app);
    w.size = egui::vec2(1200.0, 1100.0);
    w.frame(Vec::new());
    w
}

/// `1`…`7` as egui sends a digit: the key, then its text.
fn digit(i: usize) -> Vec<egui::Event> {
    let text = (i + 1).to_string();
    vec![
        pressed(TAB_KEYS[i], Modifiers::NONE),
        egui::Event::Text(text),
    ]
}

fn open_tab(w: &mut Win, tab: Tab) {
    w.frame(digit(tab.index()));
    w.frame(Vec::new());
    assert_eq!(w.app.tab, tab);
}

/// Where `text` is drawn on the sandbox's page, right of the list — the lowest of them, so a
/// tab's own button and not the toolbar's of the same name.
fn in_panel(w: &Win, text: &str) -> egui::Pos2 {
    let found = (w.drawn_at.iter())
        .filter(|(t, r)| t == text && r.center().x > crate::shell::LIST_W)
        .max_by(|(_, a), (_, b)| a.center().y.total_cmp(&b.center().y));
    found
        .unwrap_or_else(|| panic!("{text} is not drawn"))
        .1
        .center()
}

fn click(w: &mut Win, pos: egui::Pos2) {
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    w.frame(vec![egui::Event::PointerMoved(pos), button(true)]);
    w.frame(vec![button(false)]);
    w.frame(Vec::new());
}

/// Every action a line of keys names (`p  publish another   u  unpublish`) is offered as a button
/// titled as the action is — below the toolbar, which has some of the same names — and no key of
/// such a line is drawn as a key.
fn assert_offers(w: &Win, notes: &[Note]) {
    let lines: Vec<&Note> = notes
        .iter()
        .filter(|n| crate::details::key_line(n))
        .collect();
    assert!(!lines.is_empty(), "no line of keys in {notes:?}");
    for piece in lines.iter().flat_map(|n| &n.0) {
        let Some(action) = crate::details::key_action(&piece.text) else {
            assert!(!w.drawn.contains(&piece.text), "{:?} is drawn", piece.text);
            continue;
        };
        assert!(
            !w.mono.contains(&piece.text),
            "the key {:?} is drawn",
            piece.text
        );
        let title = match action {
            // One button on every port instead, by its own name.
            Action::UnpublishPort => "Unpublish",
            _ => actions::title(&w.app, action),
        };
        let below = (w.drawn_at.iter()).any(|(t, r)| t == title && r.top() > toolbar_bottom(w));
        assert!(below, "no {title} in the tab: {:?}", w.drawn);
    }
}

/// The bottom of the page's toolbar: where its main button — *Start*, or *New session* on a
/// running sandbox — ends.
fn toolbar_bottom(w: &Win) -> f32 {
    let main = actions::title(&w.app, aisbx_core::actions::Action::Start);
    (w.drawn_at.iter())
        .filter(|(t, r)| t == main && r.center().x > crate::shell::LIST_W)
        .map(|(_, r)| r.bottom())
        .fold(f32::INFINITY, f32::min)
}

#[test]
fn a_crossing_of_the_map_opens_the_tab_that_lists_it() {
    let mut w = window(sandbox(true));
    assert_eq!(w.app.tab, Tab::Overview);
    for text in [
        "read-write",
        "Shared skills",
        "Every host",
        "through the sbx proxy",
    ] {
        assert!(w.drawn.iter().any(|t| t == text), "{text}: {:?}", w.drawn);
    }
    let at = in_panel(&w, "read-write");
    click(&mut w, at);
    assert_eq!(w.app.tab, Tab::Mounts);
    open_tab(&mut w, Tab::Overview);
    let at = in_panel(&w, "Every host");
    click(&mut w, at);
    assert_eq!(w.app.tab, Tab::Network);
}

/// Every piece of every note is drawn.
fn assert_drawn(w: &Win, notes: &[Note]) {
    for piece in notes.iter().flat_map(|n| &n.0) {
        let text = piece.text.trim();
        assert!(
            w.drawn.iter().any(|d| d.contains(text)),
            "{text:?} is not drawn: {:?}",
            w.drawn
        );
    }
}

fn assert_has(w: &Win, text: &str) {
    assert!(
        w.drawn.iter().any(|d| d.contains(text)),
        "{text:?} is not drawn: {:?}",
        w.drawn
    );
}

#[test]
fn a_click_on_each_tab_opens_it() {
    let mut w = window(sandbox(true));
    for tab in Tab::ALL.into_iter().rev() {
        let at = in_panel(&w, tab.title());
        click(&mut w, at);
        assert_eq!(w.app.tab, tab);
    }
}

#[test]
fn the_digit_keys_open_the_tabs_as_core_does() {
    let mut w = window(sandbox(true));
    let mut twin = App::for_test(Catalog::load().unwrap(), vec![sandbox(true)]);
    for i in [3, 0, 6, 1, 5, 2, 4] {
        w.frame(digit(i));
        let c = char::from_digit(i as u32 + 1, 10).unwrap();
        twin.on_key(KeyEvent::from(KeyCode::Char(c)));
        assert_eq!(w.app.tab, twin.tab, "key {c}");
        assert_eq!(w.app.tab, Tab::ALL[i]);
    }
    // With a modifier a digit is not the grid's.
    w.frame(vec![pressed(Key::Num1, Modifiers::COMMAND)]);
    assert_eq!(w.app.tab, Tab::Network);
}

#[test]
fn a_tab_key_then_an_action_in_one_frame_act_in_order() {
    let mut w = window(sandbox(true));
    terminal::opened();
    let mut events = digit(2);
    events.extend([
        pressed(Key::X, Modifiers::NONE),
        egui::Event::Text("x".into()),
    ]);
    w.frame(events);
    w.frame(Vec::new());
    assert_eq!(w.app.tab, Tab::Mounts);
    let opened = terminal::opened().concat().join(" ");
    let shown = format!("{opened} {}", w.session.no_terminal().unwrap_or_default());
    assert!(shown.contains("exec") && shown.contains("box"), "{shown}");
}

#[test]
fn the_mounts_tab_lists_what_sbx_reports() {
    use aisbx_core::model::{BindMount, MountKind};
    let mut sb = sandbox(true);
    let mount = |host: &str, target: &str, read_only: bool, kind: MountKind| BindMount {
        host: host.into(),
        target: target.into(),
        read_only,
        kind,
        missing: false,
    };
    let workspace = sb.workspace.display().to_string();
    let mut gone = mount("/data/gone", "/data/gone", false, MountKind::Folder);
    gone.missing = true;
    sb.runtime_mounts = vec![aisbx_core::sbx::RuntimeMount {
        host_path: "/data/set".into(),
        container_target: "/mnt/set".into(),
        read_only: true,
    }];
    sb.host_mounts = Some(vec![
        mount(&workspace, &workspace, false, MountKind::Workspace),
        mount("/data/libs", "/workspace/libs", true, MountKind::Folder),
        mount("/sbx/agent-skills", "/skills", true, MountKind::Skills),
        mount("/data/set", "/mnt/set", true, MountKind::Added),
        gone,
    ]);
    let mut w = window(sb);
    open_tab(&mut w, Tab::Mounts);
    for text in [
        "Workspace",
        "Skills store",
        "Added",
        "Missing",
        "/data/libs",
        "/data/gone",
    ] {
        assert_has(&w, text);
    }
    assert_has(&w, "at /workspace/libs in the sandbox");
    assert!(
        !w.drawn.iter().any(|d| d == "Added with sbx mount"),
        "the sbx mount folder is a row of the one list: {:?}",
        w.drawn
    );
}

#[test]
fn each_tab_draws_cores_pieces_for_the_sample() {
    let sb = sandbox(true);
    let sample = Insight::sample("box");
    let mut w = window(sb.clone());

    open_tab(&mut w, Tab::Resources);
    assert_has(&w, insight::RESOURCES_SCOPE);
    assert_has(&w, "2 vCPU");
    assert_has(&w, "12%");

    open_tab(&mut w, Tab::Mounts);
    let mounts = insight::mount_notes(&sb);
    assert_drawn(&w, &mounts.about);
    assert_has(&w, insight::skills_mount_text(sb.skills.unwrap()));

    open_tab(&mut w, Tab::Ports);
    let ports = sample.ports.value.as_deref().unwrap();
    for port in ports {
        assert_has(&w, &port.url());
    }
    assert_has(&w, &tag(insight::EPHEMERAL));
    let notes = insight::port_notes(&sb, ports);
    assert_drawn(&w, &prose(&notes));
    assert_offers(&w, &notes);
    let unpublish = w.drawn.iter().filter(|d| *d == "Unpublish").count();
    assert_eq!(unpublish, ports.len(), "a button on every port");

    open_tab(&mut w, Tab::Network);
    assert_has(&w, insight::network_warning(&sb).unwrap());
    assert_has(&w, insight::PROXY_GLOSS);
    for rule in sample.network_rules() {
        assert_has(&w, &tag(rule.source()));
        for target in rule.targets() {
            assert_has(&w, &target);
        }
    }
    assert_offers(&w, &[insight::network_check_note()]);

    open_tab(&mut w, Tab::Filesystem);
    assert_has(&w, insight::FILESYSTEM_ABOUT);
    assert_eq!(sample.filesystem_rules().len(), 1);

    open_tab(&mut w, Tab::Activity);
    let log = sample.log.value.as_ref().unwrap();
    assert_has(&w, insight::log_reason(&log.blocked[0], true));
    assert_has(&w, &insight::short_time(&log.blocked[0].last_seen));
    for host in log.blocked.iter().chain(&log.allowed) {
        assert_has(&w, &host.host);
    }
    assert!(insight::asks_approval(log));
    assert_drawn(&w, &[insight::approval_note()]);

    open_tab(&mut w, Tab::Overview);
    assert_has(&w, "sbx name");
}

#[test]
fn a_stopped_sandbox_draws_the_stopped_notes_and_no_chart() {
    let mut w = window(sandbox(false));
    open_tab(&mut w, Tab::Resources);
    let notes = insight::resources_stopped_notes();
    assert_drawn(&w, &prose(&notes));
    assert_offers(&w, &notes);
    assert!(!w.drawn.iter().any(|d| d == "CPU %"), "{:?}", w.drawn);
    let chart = egui::Id::unique(resources::CPU_CHART);
    assert!(w.ctx.read_response(chart).is_none(), "no chart is drawn");

    open_tab(&mut w, Tab::Ports);
    let ports = w.app.insight.ports.value.clone().unwrap();
    assert_drawn(&w, &prose(&insight::port_notes(&sandbox(false), &ports)));
}

/// Core's two lines of one sentence are one paragraph, no wider than `form::PARA_W`: the Mounts
/// tab of a sandbox that mounts no folder.
#[test]
fn a_notes_lines_flow_into_one_paragraph_no_wider_than_a_paragraph() {
    let mut sb = sandbox(false);
    sb.workspace = Default::default();
    let mut w = window(sb);
    w.app.select(1);
    w.frame(Vec::new());
    open_tab(&mut w, Tab::Mounts);
    let pieces: Vec<egui::Rect> = (w.drawn_at.iter())
        .filter(|(t, _)| {
            t.starts_with("No folder of your computer") || t.contains("copies them out")
        })
        .map(|(_, r)| *r)
        .collect();
    assert!(!pieces.is_empty(), "{:?}", w.drawn);
    let paragraph = (pieces.iter()).fold(pieces[0], |a, b| a.union(*b));
    assert!(w.size.x - crate::shell::LIST_W > crate::form::PARA_W + 100.0);
    assert!(
        paragraph.width() <= crate::form::PARA_W + 0.5,
        "{paragraph:?}"
    );
    assert!(paragraph.height() > 30.0, "it wraps: {paragraph:?}");
}

/// Three readings 2.5 s apart; the pointer at the chart's right edge reads the newest back.
#[test]
fn the_charts_hover_readout_shows_the_point_under_the_pointer() {
    let base = Sample::parse(
        "cpu  10 0 10 80 0\ncpu0 1 0 1 8 0\nMemTotal: 1048576 kB\nMemAvailable: 524288 kB\n\
         100.0 100.0\nx 1000 500 500 50% /\n",
    )
    .unwrap();
    let mut w = window(sandbox(true));
    let mut s = base;
    for _ in 0..3 {
        w.app.histories.record("box", &s);
        s.uptime_s += 2.5;
        s.cpu_total += 100;
        s.cpu_idle += 75;
    }
    open_tab(&mut w, Tab::Resources);
    let newest = *w.app.histories.get("box").unwrap().points().last().unwrap();
    assert_eq!(newest.cpu, Some(0.25));
    let readout = resources::readout(&newest);
    assert!(readout.contains("CPU 25%") && readout.contains("memory 0.5 GB of 1.0 GB"));
    assert!(!w.drawn.contains(&readout), "only under the pointer");

    for id in [resources::CPU_CHART, resources::MEMORY_CHART] {
        let rect = w.ctx.read_response(egui::Id::unique(id)).unwrap().rect;
        let at = rect.right_center() - egui::vec2(0.5, 0.0);
        w.frame(vec![egui::Event::PointerMoved(at)]);
        w.frame(Vec::new());
        assert!(w.drawn.contains(&readout), "{id}: {:?}", w.drawn);
    }
}

#[test]
fn u_prefills_from_the_ports_tab() {
    let mut w = window(sandbox(true));
    open_tab(&mut w, Tab::Ports);
    w.app.insight.ports.value.as_mut().unwrap().truncate(1);
    let mut twin = App::for_test(Catalog::load().unwrap(), vec![sandbox(true)]);
    twin.insight = Insight::sample("box");
    twin.insight.ports.value.as_mut().unwrap().truncate(1);
    twin.on_key(KeyEvent::from(KeyCode::Char('u')));

    w.frame(vec![
        pressed(Key::U, Modifiers::NONE),
        egui::Event::Text("u".into()),
    ]);
    let value = |app: &App| match &app.modal {
        Some(Modal::Prompt(p)) => p.input.value.clone(),
        other => panic!("no prompt: {other:?}"),
    };
    assert_eq!(value(&w.app), value(&twin));
    let only = &Insight::sample("box").ports.value.unwrap()[0];
    assert_eq!(value(&w.app), only.spec());
}

/// Assert that a click on `drawn` (a keycap, or a key line's button) on `tab` does what the key
/// `code` does. The window takes a session's handoff by the frame's end.
fn keycap_is_key(running: bool, tab: Tab, drawn: &str, code: KeyCode) -> Win {
    let mut w = window(sandbox(running));
    open_tab(&mut w, tab);
    let mut twin = App::for_test(Catalog::load().unwrap(), vec![sandbox(running)]);
    twin.insight = Insight::sample("box");
    twin.on_key(KeyEvent::from(KeyCode::Char(
        char::from_digit(tab.index() as u32 + 1, 10).unwrap(),
    )));
    twin.on_key(KeyEvent::from(code));
    let at = in_panel(&w, drawn);
    click(&mut w, at);
    let shape = |app: &App, session: bool| {
        let modal = app.modal.as_ref().map(std::mem::discriminant);
        (modal, std::mem::discriminant(&app.screen), session)
    };
    let started = w.app.handoff.is_some() || w.session.busy() || w.session.dialog_open();
    let started = started || !terminal::opened().is_empty();
    assert_eq!(
        shape(&w.app, started),
        shape(&twin, twin.handoff.is_some()),
        "{drawn} on {tab:?}"
    );
    w
}

#[test]
fn a_key_in_a_note_is_a_button_that_does_what_the_key_does() {
    terminal::opened();
    let check = keycap_is_key(true, Tab::Network, "Check host", KeyCode::Char('c'));
    assert!(matches!(check.app.modal, Some(Modal::Prompt(_))));
    let publish = keycap_is_key(true, Tab::Ports, "Publish port", KeyCode::Char('p'));
    assert!(matches!(publish.app.modal, Some(Modal::Prompt(_))));
    let answer = keycap_is_key(true, Tab::Activity, "y", KeyCode::Char('y'));
    assert!(matches!(
        answer.app.screen,
        aisbx_core::app::Screen::Approvals(_)
    ));
    keycap_is_key(false, Tab::Resources, "Shell", KeyCode::Char('x'));
    terminal::opened();
}

/// A bar's track and fill are the 8-point-high rects; none of them lies under the text.
#[test]
fn a_meter_writes_its_percentage_beside_the_bar() {
    let mut w = window(sandbox(true));
    open_tab(&mut w, Tab::Resources);
    let percent = (w.drawn_at.iter())
        .find(|(t, _)| t == "12%")
        .expect("the CPU's share is drawn")
        .1;
    let bars: Vec<egui::Rect> = (w.fills.iter())
        .map(|(r, _)| *r)
        .filter(|r| (r.height() - 8.0).abs() < 0.5)
        .collect();
    assert!(bars.len() >= 3, "a track for each meter: {:?}", w.fills);
    for bar in bars {
        assert!(!bar.intersects(percent), "{bar:?} under {percent:?}");
    }
}

#[test]
fn a_ports_unpublish_names_that_port() {
    let mut w = window(sandbox(true));
    open_tab(&mut w, Tab::Ports);
    let ports = w.app.insight.ports.value.clone().unwrap();
    assert_eq!(ports.len(), 2);
    let second = (w.drawn_at.iter())
        .filter(|(t, _)| t == "Unpublish")
        .map(|(_, r)| r.center())
        .max_by(|a, b| a.y.total_cmp(&b.y))
        .expect("an Unpublish on each port");
    click(&mut w, second);
    match &w.app.modal {
        Some(Modal::Prompt(p)) => assert_eq!(p.input.value, ports[1].spec()),
        other => panic!("no prompt: {other:?}"),
    }
}

#[test]
fn the_activity_tables_line_up() {
    let mut w = window(sandbox(true));
    open_tab(&mut w, Tab::Activity);
    for title in ["Host", "Via", "Requests", "Last seen"] {
        let xs: Vec<f32> = (w.drawn_at.iter())
            .filter(|(t, r)| t == title && r.center().x > crate::shell::LIST_W)
            .map(|(_, r)| r.left())
            .collect();
        assert_eq!(xs.len(), 2, "{title} heads both tables: {xs:?}");
        assert!((xs[0] - xs[1]).abs() < 0.5, "{title}: {xs:?}");
    }
}

#[test]
fn a_rule_names_its_reach_only_beyond_the_sandbox() {
    let mut w = window(sandbox(true));
    let rules = w.app.insight.rules.value.as_mut().unwrap();
    rules.push(aisbx_core::sbx::PolicyRule {
        name: "host-wide".into(),
        scope: "global".into(),
        applies_to: "all".into(),
        resource_type: "network".into(),
        decision: "deny".into(),
        resources: vec!["ads.example.com".into()],
        ..Default::default()
    });
    open_tab(&mut w, Tab::Network);
    assert!(
        !w.drawn.iter().any(|d| d.contains("sandbox:box")),
        "{:?}",
        w.drawn
    );
    assert_eq!(
        w.drawn.iter().filter(|d| *d == "All sandboxes").count(),
        2,
        "the host-wide deny and the policy's fallback: {:?}",
        w.drawn
    );
    assert_has(&w, "Fallback");
    for heading in ["Blocked", "Allowed"] {
        assert_has(&w, heading);
    }
    for word in ["deny", "allow"] {
        assert!(
            !w.drawn.contains(&word.to_string()),
            "{word}: {:?}",
            w.drawn
        );
    }
}
