//! The sandbox page's tab strip and tab bodies: the TUI's seven tabs, in its order. Core decides
//! what each tab says (`app.insight`, `insight`) and changes the tab; this module draws a tab as
//! facts or tables, with named buttons where the TUI names keys.

use std::path::Path;

use aisbx_core::actions::Action;
use aisbx_core::app::{App, Hit};
use aisbx_core::input::{KeyCode, KeyEvent};
use aisbx_core::insight::{self, Kind, Note, PieceKind, SourceTone, Tab};
use aisbx_core::model::{self, MountKind, Sandbox};
use aisbx_core::sbx::{LogHost, PolicyRule, Port};
use eframe::egui::{self, Color32, RichText};

use crate::table::{self, Button, Cell, Row};
use crate::{actions, boundary, form, overview, resources, theme};

/// How tall one line of the tab strip is, and the room on each side of a title.
const TAB_HEIGHT: f32 = 34.0;
const TAB_PAD: f32 = 12.0;

/// How many cells of `cell` width go on one line of `width`, and how many lines seven tabs take.
pub(crate) fn tab_grid(width: f32, cell: f32) -> (usize, usize) {
    let n = Tab::ALL.len();
    let columns = ((width / cell.max(1.0)).floor() as usize).clamp(1, n);
    // As even as the lines allow: four and three, not six and one.
    let lines = n.div_ceil(columns);
    (n.div_ceil(lines), lines)
}

/// Where each tab goes in a strip `width` wide, given each title's width: one line, each tab as
/// wide as its title, while they fit — else the even grid of [`tab_grid`], every cell as wide as
/// the widest title.
pub(crate) fn tab_cells(width: f32, titles: &[f32]) -> Vec<egui::Rect> {
    let natural: f32 = titles.iter().map(|w| w + 2.0 * TAB_PAD).sum();
    if natural <= width {
        let mut x = 0.0;
        return (titles.iter())
            .map(|w| {
                let cell = egui::Rect::from_min_size(
                    egui::pos2(x, 0.0),
                    egui::vec2(w + 2.0 * TAB_PAD, TAB_HEIGHT),
                );
                x += cell.width();
                cell
            })
            .collect();
    }
    let widest = titles.iter().copied().fold(0.0, f32::max);
    let (columns, _) = tab_grid(width, widest + 2.0 * TAB_PAD);
    let cell = egui::vec2(width / columns as f32, TAB_HEIGHT);
    (0..titles.len())
        .map(|i| {
            let at = egui::vec2((i % columns) as f32 * cell.x, (i / columns) as f32 * cell.y);
            egui::Rect::from_min_size(at.to_pos2(), cell)
        })
        .collect()
}

/// The strip of tab titles over a hairline, the open one strong in the ink with a bar under it.
/// Drawn only while a sandbox is selected. Too narrow for one line, the tabs form an even grid
/// ([`tab_cells`]), so the strip still reads as one control.
pub fn strip(ui: &mut egui::Ui, app: &mut App) {
    if app.selected().is_none() {
        return;
    }
    let dark = ui.visuals().dark_mode;
    let size = 14.0;
    let galleys: Vec<_> = Tab::ALL
        .iter()
        .map(|t| {
            let font = match app.tab == *t {
                true => egui::FontId::new(size, theme::strong_family()),
                false => egui::FontId::proportional(size),
            };
            (ui.painter()).layout_no_wrap(t.title().to_string(), font, Color32::PLACEHOLDER)
        })
        .collect();
    // Measured in the strong weight, the wider: a tab does not move when it opens.
    let titles: Vec<f32> = (Tab::ALL.iter())
        .map(|t| {
            let font = egui::FontId::new(size, theme::strong_family());
            (ui.painter())
                .layout_no_wrap(t.title().to_string(), font, Color32::PLACEHOLDER)
                .size()
                .x
        })
        .collect();
    let width = ui.available_width();
    let cells = tab_cells(width, &titles);
    let height = cells.iter().map(|c| c.bottom()).fold(0.0, f32::max);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let visuals = ui.visuals().clone();
    let line = egui::Stroke::new(1.0, theme::line(dark));
    ui.painter()
        .hline(rect.x_range(), rect.bottom() - 0.5, line);
    let mut clicked = None;
    for (i, (tab, galley)) in Tab::ALL.into_iter().zip(galleys).enumerate() {
        let cell_rect = cells[i].translate(rect.min.to_vec2());
        let response = ui.interact(
            cell_rect,
            ui.scope_id().with(("tab", i)),
            egui::Sense::CLICK,
        );
        let open = app.tab == tab;
        let bar = |colour: Color32, high: f32| {
            let under = egui::Rect::from_min_max(
                egui::pos2(cell_rect.left() + 4.0, cell_rect.bottom() - high),
                egui::pos2(cell_rect.right() - 4.0, cell_rect.bottom()),
            );
            ui.painter().rect_filled(under, 1.0, colour);
        };
        let color = match open {
            true => {
                bar(theme::accent(dark), 2.5);
                theme::accent(dark)
            }
            false if response.hovered() => {
                bar(visuals.weak_text_color(), 1.0);
                visuals.strong_text_color()
            }
            false => visuals.weak_text_color(),
        };
        ui.painter()
            .galley(cell_rect.center() - galley.size() / 2.0, galley, color);
        let title = tab.title();
        response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, open, title));
        let tip = format!("{} tab, key {}", tab.title(), i + 1);
        if response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(tip)
            .clicked()
        {
            clicked = Some(i);
        }
    }
    ui.add_space(10.0);
    if let Some(i) = clicked {
        app.click(Hit::TabAt(i));
    }
}

/// What a click in the open tab asked for: an action, as its key runs it, the unpublishing of
/// one listed port ([`App::unpublish`]), or another tab — a crossing of the Overview's map.
#[derive(Debug, Clone)]
pub enum Clicked {
    Action(Action),
    Unpublish(Port),
    Tab(Tab),
}

impl Clicked {
    /// Do it, as the toolbar's buttons do (`actions::run`); a tab through core, as a click on its
    /// title opens it.
    pub fn run(self, app: &mut App, busy: bool) {
        match self {
            Clicked::Action(action) => actions::run(app, action, busy),
            Clicked::Unpublish(port) => app.unpublish(&port),
            Clicked::Tab(tab) => app.click(Hit::TabAt(tab.index())),
        }
    }
}

/// The open tab's body — none on the New row, which the panel's head explains. Returns what a
/// button in it asked for.
pub fn body(ui: &mut egui::Ui, app: &App) -> Option<Clicked> {
    let sb = app.selected()?;
    let action = match app.tab {
        Tab::Overview => {
            let tab = boundary::show(ui, app, sb);
            ui.add_space(14.0);
            let action = overview::panel(ui, app);
            return tab.map(Clicked::Tab).or(action.map(Clicked::Action));
        }
        Tab::Resources => resources::show(ui, app, sb),
        Tab::Mounts => mounts(ui, app, sb),
        Tab::Ports => return ports(ui, app, sb),
        Tab::Network => network(ui, app, sb),
        Tab::Filesystem => filesystem(ui, app, sb),
        Tab::Activity => activity(ui, app),
    };
    action.map(Clicked::Action)
}

// --- drawing core's notes -----------------------------------------------------------------------

fn weak(ui: &egui::Ui, text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(ui.visuals().weak_text_color())
}

fn coloured(text: impl Into<String>, color: Color32) -> RichText {
    theme::strong(RichText::new(text.into())).color(color)
}

/// One piece of a note: its words quiet, a key as a key, a warning in its colour.
fn piece(ui: &egui::Ui, kind: PieceKind, text: &str) -> RichText {
    let dark = ui.visuals().dark_mode;
    match kind {
        PieceKind::Plain => weak(ui, text),
        PieceKind::Value => RichText::new(text).monospace(),
        PieceKind::Key => coloured(text, theme::accent(dark)).monospace(),
        PieceKind::Warn => coloured(text, theme::warn(dark)),
        PieceKind::Danger => coloured(text, theme::danger(dark)),
    }
}

/// Join core's terminal-width lines into paragraphs: a line that does not end a sentence flows on
/// into a next line that does not start one. A line that starts with a key keeps its own line.
fn paragraphs(notes: &[Note]) -> Vec<Note> {
    let words_only = |n: &Note| !n.is_blank() && n.0.iter().all(|p| p.kind == PieceKind::Plain);
    let open = |n: &Note| words_only(n) && !n.text().trim_end().ends_with('.');
    let goes_on = |n: &Note| {
        let first = n.0.first().filter(|p| p.kind == PieceKind::Plain);
        first.is_some_and(|p| !p.text.starts_with(char::is_uppercase))
    };
    let mut out: Vec<Note> = Vec::new();
    for note in notes {
        match out.last_mut() {
            Some(last) if open(last) && goes_on(note) => {
                let mut pieces = note.0.clone();
                pieces[0].text = format!(" {}", pieces[0].text);
                last.0.extend(pieces);
            }
            _ => out.push(note.clone()),
        }
    }
    out
}

/// The list action a key in a note stands for: `↵` starts, a letter is its action's own key.
pub(crate) fn key_action(key: &str) -> Option<Action> {
    let code = match key {
        "↵" => KeyCode::Enter,
        _ => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => return None,
            }
        }
    };
    Action::from_key(KeyEvent::from(code))
}

/// Core's notes, a paragraph each, at most [`form::PARA_W`] wide. A key a note names is a keycap
/// button that does what the key does, disabled with the reason when it cannot run. Returns the
/// action a click asked for.
pub(crate) fn notes(ui: &mut egui::Ui, app: &App, notes: &[Note]) -> Option<Action> {
    let mut clicked = None;
    for note in paragraphs(notes) {
        if note.is_blank() {
            ui.add_space(6.0);
            continue;
        }
        ui.scope(|ui| {
            ui.set_max_width(ui.available_width().min(form::PARA_W));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for p in &note.0 {
                    match (p.kind, key_action(&p.text)) {
                        (PieceKind::Key, Some(action)) => {
                            if keycap(ui, app, &p.text, action) {
                                clicked = Some(action);
                            }
                        }
                        // A plain piece's `code` is code; a value is in the monospace face
                        // already.
                        (PieceKind::Plain, _) => {
                            let weak = ui.visuals().weak_text_color();
                            let text = theme::with_code(ui, &p.text, |t| t.color(weak));
                            ui.add(egui::Label::new(text).wrap());
                        }
                        (kind, _) => {
                            ui.add(egui::Label::new(piece(ui, kind, &p.text)).wrap());
                        }
                    }
                }
            });
        });
    }
    clicked
}

/// A key drawn as a key — framed, in the monospace face — that is also the action's button.
fn keycap(ui: &mut egui::Ui, app: &App, key: &str, action: Action) -> bool {
    let state = actions::enabled(app, action, false);
    let cap = egui::Button::new(RichText::new(key).monospace().strong()).small();
    let response = ui.add_enabled(state.is_ok(), cap).on_hover_text(format!(
        "{} — {}",
        actions::title(app, action),
        action.summary()
    ));
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, action.title()));
    match state {
        Ok(()) => response.clicked(),
        Err(why) => {
            response.on_disabled_hover_text(theme::with_code(ui, &why, |t| t));
            false
        }
    }
}

/// Labelled facts, a label in a column of its own and the value beside it, as the Overview
/// writes its rows: the proxy and the policy over the Network tab's rules.
pub(crate) fn rows(ui: &mut egui::Ui, rows: Vec<(&str, Vec<RichText>)>) {
    const LABEL_W: f32 = 96.0;
    let value_w = (ui.available_width() - LABEL_W - 10.0).max(120.0);
    for (label, value) in rows {
        ui.horizontal_top(|ui| {
            let line = egui::vec2(LABEL_W, ui.spacing().interact_size.y);
            let layout = egui::Layout::left_to_right(egui::Align::Center);
            ui.allocate_ui_with_layout(line, layout, |ui| {
                ui.set_min_size(line);
                ui.label(weak(ui, label));
            });
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.set_max_width(value_w);
                ui.horizontal_wrapped(|ui| {
                    for text in value {
                        ui.add(egui::Label::new(text).wrap());
                    }
                });
            });
        });
    }
}

/// A fetch's error, drawn above whatever is still shown.
fn error(ui: &mut egui::Ui, error: Option<&str>) {
    if let Some(err) = error {
        let color = theme::danger(ui.visuals().dark_mode);
        ui.add(egui::Label::new(coloured(err, color)).wrap());
    }
}

fn pending(ui: &mut egui::Ui, kind: Kind, loading: bool) {
    let text = weak(ui, insight::pending_text(kind, loading));
    ui.label(text);
}

/// A note that starts with a key — `p  publish another   u  unpublish` — is a line of keys to
/// press, which the window offers as buttons where the list is, named as the actions are.
pub(crate) fn key_line(note: &Note) -> bool {
    note.0.first().is_some_and(|p| p.kind == PieceKind::Key)
}

/// Core's notes without their key lines ([`key_line`]), and with no blank line at either end or
/// two in a row where a key line went.
pub(crate) fn prose(notes: &[Note]) -> Vec<Note> {
    let mut out: Vec<Note> = Vec::new();
    for note in notes.iter().filter(|n| !key_line(n)) {
        if note.is_blank() && out.last().is_none_or(Note::is_blank) {
            continue;
        }
        out.push(note.clone());
    }
    while out.last().is_some_and(Note::is_blank) {
        out.pop();
    }
    out
}

/// The actions the lines of keys among `notes` name ([`key_line`]), in their order, as a row of
/// buttons titled as the actions are — each greyed out with the reason when it cannot run.
/// Returns the action one asked for.
pub(crate) fn key_buttons(ui: &mut egui::Ui, app: &App, notes: &[Note]) -> Option<Action> {
    let keys = (notes.iter().filter(|n| key_line(n)))
        .flat_map(|n| &n.0)
        .filter(|p| p.kind == PieceKind::Key);
    let actions: Vec<Action> = keys.filter_map(|p| key_action(&p.text)).collect();
    if actions.is_empty() {
        return None;
    }
    let mut clicked = None;
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        for action in actions {
            let state = actions::enabled(app, action, false);
            let response = ui
                .add_enabled(
                    state.is_ok(),
                    egui::Button::new(actions::title(app, action)),
                )
                .on_hover_text(format!(
                    "{} ({})",
                    action.summary(),
                    actions::key_label(action)
                ));
            match state {
                Ok(()) if response.clicked() => clicked = Some(action),
                Ok(()) => {}
                Err(why) => {
                    response.on_disabled_hover_text(theme::with_code(ui, &why, |t| t));
                }
            }
        }
    });
    clicked
}

/// Core's prose under a table: a little apart from it, a paragraph a note.
fn captions(ui: &mut egui::Ui, app: &App, list: &[Note]) -> Option<Action> {
    let list = prose(list);
    if list.is_empty() {
        return None;
    }
    ui.add_space(8.0);
    notes(ui, app, &list)
}

/// Core's word for a tag, as a badge writes it: its first letter capitalised.
pub(crate) fn tag(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

// --- tabs ---------------------------------------------------------------------------------------

/// The folders the sandbox sees, then those added with `sbx mount`, then the skills store — each a
/// table of folders with their access as a badge.
fn mounts(ui: &mut egui::Ui, app: &App, sb: &Sandbox) -> Option<Action> {
    let about = insight::mount_notes(sb);
    let quiet = ui.visuals().weak_text_color();
    let mut clicked = None;
    let dark = ui.visuals().dark_mode;
    let folders: Vec<Row> = (insight::mount_rows(sb).into_iter())
        .map(|mount| {
            let row = Row::new(vec![Cell::mono(mount.host)]);
            let row = match &mount.target {
                Some(target) => row.sub(format!("at {target} in the sandbox")),
                None => row,
            };
            let row = match mount.kind {
                MountKind::Workspace => {
                    row.badge("Workspace", quiet, "the folder the sandbox is built on")
                }
                MountKind::Skills => row.badge("Skills store", quiet, insight::SKILLS_STORE),
                MountKind::Added => {
                    row.badge("Added", quiet, "added with sbx mount after creation")
                }
                MountKind::Folder => row,
            };
            let row = row.badge(tag(&mount.access), quiet, "what the sandbox may do in it");
            match mount.missing {
                true => row.badge("Missing", theme::danger(dark), "gone from this host"),
                false => row,
            }
        })
        .collect();
    if !folders.is_empty() {
        table::heading(ui, app, "Folders", "", None, &[]);
        table::table(ui, "mounts", &[], &folders, "");
    }
    clicked = clicked.or(captions(ui, app, &about.about));
    if let Some(text) = insight::skills_row(sb) {
        table::heading(ui, app, "Skills store", "", None, &[]);
        let row = Row::new(vec![Cell::text(tag(insight::SKILLS_STORE))]).sub(text);
        table::table(ui, "mounts-skills", &[], &[row], "");
    }
    if !sb.runtime_mounts.is_empty() && sb.host_mounts.is_none() {
        let added: Vec<Row> = (sb.runtime_mounts.iter())
            .map(|mount| {
                let host = model::tilde(Path::new(&mount.host_path));
                let row = Row::new(vec![Cell::mono(host)]);
                let row = match mount.target() != mount.host_path {
                    true => row.sub(format!("at {} in the sandbox", mount.target())),
                    false => row,
                };
                let access = insight::runtime_mount_access(mount);
                row.badge(tag(access), quiet, "what the sandbox may do in it")
            })
            .collect();
        let count = table::count(added.len(), "folder", "folders");
        table::heading(ui, app, "Added with sbx mount", &count, None, &[]);
        table::table(ui, "mounts-added", &[], &added, "");
    }
    let foot: Vec<Note> = about.added.into_iter().chain(about.missing).collect();
    clicked.or(captions(ui, app, &foot))
}

/// The published ports: a table of what the host reaches and where it lands, each port with its
/// own *Unpublish*, under a heading that publishes another.
fn ports(ui: &mut egui::Ui, app: &App, sb: &Sandbox) -> Option<Clicked> {
    let cell = &app.insight.ports;
    error(ui, cell.error.as_deref());
    let Some(list) = &cell.value else {
        pending(ui, Kind::Ports, cell.is_loading());
        return None;
    };
    let dark = ui.visuals().dark_mode;
    let quiet = ui.visuals().weak_text_color();
    // The title names what is counted.
    let count = match list.len() {
        0 => String::new(),
        n => n.to_string(),
    };
    let heading = table::heading(
        ui,
        app,
        "Published ports",
        &count,
        None,
        &[Action::PublishPort],
    );
    let mut clicked = heading.map(Clicked::Action);
    if !list.is_empty() {
        let rows: Vec<Row> = (list.iter())
            .map(|port| {
                let row = Row::new(vec![
                    Cell::mono(port.url()),
                    Cell::mono(port.sandbox_port.to_string()),
                    Cell::text(insight::port_protocol(port)).colored(quiet),
                ])
                .button(Button::of(app, Action::UnpublishPort, "Unpublish"));
                match port.ephemeral {
                    true => row.badge(
                        tag(insight::EPHEMERAL),
                        theme::warn(dark),
                        "this host port is not stable across restarts",
                    ),
                    false => row,
                }
            })
            .collect();
        let columns = ["On the host", "In the sandbox", "Protocol"];
        if let Some(i) = table::table(ui, "ports", &columns, &rows, "") {
            clicked = Some(Clicked::Unpublish(list[i].clone()));
        }
    }
    let notes = captions(ui, app, &insight::port_notes(sb, list));
    clicked.or(notes.map(Clicked::Action))
}

fn network(ui: &mut egui::Ui, app: &App, sb: &Sandbox) -> Option<Action> {
    let mut list = Vec::new();
    if !sb.proxy.is_empty() {
        let value = vec![RichText::new(&sb.proxy), weak(ui, insight::PROXY_GLOSS)];
        list.push(("Proxy", value));
    }
    if !sb.policy_scope.is_empty() {
        let scope = RichText::new(format!("{} scope", sb.policy_scope));
        list.push(("Policy", vec![scope]));
    }
    if !sb.network.is_empty() {
        list.push(("Network", vec![weak(ui, &sb.network)]));
    }
    rows(ui, list);
    let rules = app.insight.network_rules();
    // The tab's one line of keys (`insight::network_check_note`) is this button on the first
    // heading.
    let check = [Action::CheckHost];
    let clicked = rule_tables(ui, app, sb, &rules, insight::NO_NETWORK_RULES, &check);
    if let Some(warning) = insight::network_warning(sb) {
        ui.add_space(8.0);
        let color = theme::danger(ui.visuals().dark_mode);
        ui.add(egui::Label::new(coloured(warning, color)).wrap());
    }
    clicked
}

fn filesystem(ui: &mut egui::Ui, app: &App, sb: &Sandbox) -> Option<Action> {
    let clicked = notes(ui, app, &[Note::plain(insight::FILESYSTEM_ABOUT)]);
    let rules = app.insight.filesystem_rules();
    clicked.or(rule_tables(
        ui,
        app,
        sb,
        &rules,
        insight::NO_FILESYSTEM_RULES,
        &[],
    ))
}

/// `rules` in two tables whose columns line up, what they block and then what they allow, as
/// core lists them; a rule a row ([`rule_row`]). `buttons` go on the first heading. Returns the
/// action a button asked for.
fn rule_tables(
    ui: &mut egui::Ui,
    app: &App,
    sb: &Sandbox,
    rules: &[&PolicyRule],
    empty: &str,
    buttons: &[Action],
) -> Option<Action> {
    let cell = &app.insight.rules;
    error(ui, cell.error.as_deref());
    if rules.is_empty() {
        let clicked = table::heading(ui, app, "Rules", "", None, buttons);
        let text = match cell.is_loading() {
            true => insight::pending_text(Kind::Policy, true),
            false => empty,
        };
        table::table(ui, "rules", &[], &[], text);
        return clicked;
    }
    let rows = |allows: bool| -> Vec<Row> {
        (rules.iter())
            .filter(|r| r.allows() == allows)
            .map(|rule| rule_row(ui, sb, rule))
            .collect()
    };
    let (blocked, allowed) = (rows(false), rows(true));
    let dark = ui.visuals().dark_mode;
    let mut clicked = None;
    let mut buttons = buttons;
    for (title, marker, rows) in [
        ("Blocked", theme::danger(dark), &blocked),
        ("Allowed", theme::running(dark), &allowed),
    ] {
        if rows.is_empty() {
            continue;
        }
        let count = table::count(rows.len(), "rule", "rules");
        let asked = table::heading(ui, app, title, &count, Some(marker), buttons);
        clicked = clicked.or(asked);
        buttons = &[];
        let id = format!("rules-{title}");
        table::aligned(ui, &id, &[], rows, "", &[&blocked, &allowed]);
    }
    clicked
}

/// A rule: what it matches, its type, and as badges who put it there, whom it reaches beyond
/// `sb` and whether it is switched off.
fn rule_row(ui: &egui::Ui, sb: &Sandbox, rule: &PolicyRule) -> Row {
    let dark = ui.visuals().dark_mode;
    let quiet = ui.visuals().weak_text_color();
    let kind = match insight::rule_protocols(rule) {
        Some(protocols) => format!("{} {protocols}", rule.resource_type),
        None => rule.resource_type.clone(),
    };
    let row = Row::new(vec![
        Cell::mono(rule.targets().join("   ")),
        Cell::text(kind).colored(quiet),
    ]);
    let source = rule.source();
    let tone = match insight::source_tone(source) {
        SourceTone::Accent => theme::accent(dark),
        SourceTone::Warn => theme::warn(dark),
        SourceTone::Quiet => quiet,
    };
    let mut row = row.badge(tag(source), tone, "who put the rule there");
    if let Some(reach) = insight::rule_reach(rule, &sb.name) {
        let tip = "the rule is not this sandbox's alone";
        row = row.badge(tag(&reach), theme::accent(dark), tip);
    }
    if let Some(status) = insight::rule_inactive(rule) {
        row = row.badge(tag(status), theme::warn(dark), "present, but switched off");
    }
    row
}

/// The requests the proxy logged: a table of the blocked hosts, each with why, then one of those
/// let through — host, how it went, how often and when last.
fn activity(ui: &mut egui::Ui, app: &App) -> Option<Action> {
    let cell = &app.insight.log;
    error(ui, cell.error.as_deref());
    let Some(log) = &cell.value else {
        pending(ui, Kind::Log, cell.is_loading());
        return None;
    };
    if log.is_empty() {
        ui.label(weak(ui, insight::NO_REQUESTS));
        return None;
    }
    let dark = ui.visuals().dark_mode;
    let rows = |hosts: &[LogHost], blocked| -> Vec<Row> {
        (hosts.iter())
            .map(|host| host_row(ui, host, blocked))
            .collect()
    };
    let (blocked, allowed) = (rows(&log.blocked, true), rows(&log.allowed, false));
    let mut clicked = None;
    for (title, marker, rows) in [
        ("Blocked", theme::danger(dark), &blocked),
        ("Allowed", theme::running(dark), &allowed),
    ] {
        if rows.is_empty() {
            continue;
        }
        let count = table::count(rows.len(), "host", "hosts");
        table::heading(ui, app, title, &count, Some(marker), &[]);
        let columns = ["Host", "Via", "Requests", "Last seen"];
        table::aligned(ui, title, &columns, rows, "", &[&blocked, &allowed]);
        if title == "Blocked" && insight::asks_approval(log) {
            clicked = clicked.or(captions(ui, app, &[insight::approval_note()]));
        }
    }
    clicked
}

/// A logged host: its name, the proxy's way, the count and the time, and under them why.
fn host_row(ui: &egui::Ui, host: &LogHost, blocked: bool) -> Row {
    let quiet = ui.visuals().weak_text_color();
    Row::new(vec![
        Cell::mono(&host.host),
        Cell::text(&host.proxy_type).colored(quiet),
        Cell::number(host.count.to_string()),
        Cell::mono(insight::short_time(&host.last_seen)).colored(quiet),
    ])
    .sub(insight::log_reason(host, blocked))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_lines_flow_into_paragraphs_until_a_sentence_ends() {
        let mut sb = model::sample("s", Some("claude-anthropic"), &[]);
        sb.clone = true;
        let joined: Vec<String> = paragraphs(&insight::mount_notes(&sb).about)
            .iter()
            .map(Note::text)
            .collect();
        assert_eq!(
            joined,
            [
                "Clone mode: the agent works on its own Git clone. Get its commits with git fetch \
              sandbox-s"
            ]
        );
        let stopped = paragraphs(&insight::resources_stopped_notes());
        let texts: Vec<String> = stopped.iter().map(Note::text).collect();
        assert_eq!(texts.len(), 3, "{texts:?}");
        assert_eq!(texts[0], "The sandbox is stopped.");
        assert_eq!(stopped[2], insight::resources_stopped_notes()[2]);
        let mut sb = model::sample("s", Some("claude-anthropic"), &[]);
        sb.running = true;
        let ports = [aisbx_core::sbx::Port {
            host_ip: "127.0.0.1".into(),
            host_port: 8080,
            sandbox_port: 3000,
            protocol: "tcp".into(),
            ephemeral: false,
        }];
        let notes = paragraphs(&insight::port_notes(&sb, &ports));
        let texts: Vec<String> = notes.iter().map(Note::text).collect();
        assert!(
            texts.contains(&"p  publish another   u  unpublish".to_string()),
            "{texts:?}"
        );
        let last = notes.last().unwrap();
        assert_eq!(
            last.text(),
            "It stops after its last session, and its ports with it — b keeps it running."
        );
        assert!(last
            .0
            .iter()
            .any(|p| p.kind == PieceKind::Key && p.text == "b"));
    }
}
