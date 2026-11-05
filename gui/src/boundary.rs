//! The map at the top of the Overview: what the agent inside the sandbox can reach, drawn as the
//! sandbox's wall and what crosses it, the host on the left and the network on the right. Each
//! crossing is a button for the tab that lists it.

use std::path::PathBuf;

use aisbx_core::app::App;
use aisbx_core::create;
use aisbx_core::insight::{self, Tab};
use aisbx_core::model::{self, Sandbox, Skills, MOUNTLESS_WORKDIR};
use aisbx_core::sbx::SkillsMode;
use eframe::egui::{self, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, Vec2};

use crate::theme;

/// Which way a crossing goes, as its arrow points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    /// Both ways: a folder the agent reads and writes.
    Both,
    /// Into the sandbox only: a folder it reads, a port the host reaches it on, a copy.
    In,
    /// Out of the sandbox: the network.
    Out,
    /// Nothing crosses — no folder, no port: said, and drawn without an arrow.
    None,
}

/// How a crossing is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    /// Nothing crosses here.
    Weak,
    /// Wider than it has to be, or waiting on you: every host, a store other sandboxes load.
    Warn,
    /// Broken: the folder is gone.
    Danger,
}

/// One thing that crosses the sandbox's wall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crossing {
    /// What it is, on its side of the wall: a folder, a port, the network.
    pub what: String,
    /// How it crosses, under it.
    pub how: String,
    pub way: Way,
    pub tone: Tone,
    /// The tab that lists it, which a click opens.
    pub tab: Tab,
    /// Its tooltip: what the mode means, every folder of a group.
    pub tip: String,
}

/// The map of one sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Map {
    /// The agent, inside the wall, and its kits under it: each id with its label, as the list's
    /// badges name it.
    pub agent: String,
    pub kits: Vec<(String, String)>,
    /// The host's side, top to bottom: the workspace, other folders, the skills store, ports.
    pub host: Vec<Crossing>,
    /// The network's side.
    pub out: Crossing,
}

fn crossing(what: impl Into<String>, how: impl Into<String>, way: Way, tab: Tab) -> Crossing {
    Crossing {
        what: what.into(),
        how: how.into(),
        way,
        tone: Tone::Plain,
        tab,
        tip: String::new(),
    }
}

impl Crossing {
    fn tone(self, tone: Tone) -> Self {
        Crossing { tone, ..self }
    }

    fn tip(self, tip: impl Into<String>) -> Self {
        let tip = tip.into();
        Crossing { tip, ..self }
    }
}

/// What `sb` reaches, for the map.
pub fn map(app: &App, sb: &Sandbox) -> Map {
    let agent = match app.backend_of(sb) {
        Some(backend) => create::harness_label(backend),
        None => sb.agent.clone(),
    };
    let kits = sb.kit_badges();
    let mut host = vec![workspace(sb)];
    host.extend(folders(sb));
    host.extend(skills(sb));
    host.push(ports(sb));
    let out = network(app, sb);
    Map {
        agent,
        kits,
        host,
        out,
    }
}

/// The folder the sandbox was made for, and how the agent works on it.
fn workspace(sb: &Sandbox) -> Crossing {
    if sb.mountless() {
        let how = format!("it works in {MOUNTLESS_WORKDIR}");
        return crossing("No folder mounted", how, Way::None, Tab::Mounts)
            .tone(Tone::Weak)
            .tip("Its files live only in the sandbox: deleting the sandbox deletes them.");
    }
    let what = sb.folder_label();
    if sb.workspace_missing {
        return crossing(what, "folder missing", Way::Both, Tab::Mounts)
            .tone(Tone::Danger)
            .tip("This folder no longer exists on this computer.");
    }
    // How sbx mounted it, in the Mounts tab's words.
    let how = sb.mounts().first().map_or("read-write", |(_, mode)| *mode);
    match sb.clone {
        true => crossing(what, how, Way::In, Tab::Mounts).tip(
            "Clone mode: the agent works on its own Git clone, and your files stay unchanged.",
        ),
        false => crossing(what, how, Way::Both, Tab::Mounts)
            .tip("What the agent changes here changes your files."),
    }
}

/// The other host folders it sees: the kits' read-only caches and what `sbx mount` added, one
/// crossing per access.
fn folders(sb: &Sandbox) -> Vec<Crossing> {
    let added = |read_only: bool| {
        (sb.runtime_mounts.iter())
            .filter(move |m| m.read_only == read_only)
            .map(|m| PathBuf::from(&m.host_path))
    };
    let rw: Vec<PathBuf> = sb.rw_mounts.iter().cloned().chain(added(false)).collect();
    let ro: Vec<PathBuf> = sb.ro_mounts.iter().cloned().chain(added(true)).collect();
    let mut out = Vec::new();
    for (paths, how, way) in [(rw, "read-write", Way::Both), (ro, "read-only", Way::In)] {
        let names: Vec<String> = paths.iter().map(|p| model::tilde(p)).collect();
        let what = match names.as_slice() {
            [] => continue,
            [one] => one.clone(),
            many => format!("{} folders", many.len()),
        };
        out.push(crossing(what, how, way, Tab::Mounts).tip(names.join("\n")));
    }
    out
}

/// The host's shared agent-skills store, where the sandbox is given it.
fn skills(sb: &Sandbox) -> Option<Crossing> {
    let skills = sb.skills?;
    let (how, way, tone, tip) = match skills {
        Skills::Known(SkillsMode::Off) => return None,
        Skills::Known(SkillsMode::Readonly) => (
            "read-only",
            Way::In,
            Tone::Plain,
            "The shared skills; the agent cannot change them.",
        ),
        Skills::Known(SkillsMode::Readwrite) => (
            "read-write",
            Way::Both,
            Tone::Warn,
            "It can change the skills every other sandbox loads.",
        ),
        Skills::Unrecorded => (
            "mode not recorded",
            Way::In,
            Tone::Plain,
            "Created outside aisbx.",
        ),
    };
    let what = "Shared skills";
    Some(crossing(what, how, way, Tab::Mounts).tone(tone).tip(tip))
}

/// The ports the host reaches it on. A dual-stack port is listed twice by sbx, once per
/// address family: it is one port here.
fn ports(sb: &Sandbox) -> Crossing {
    let mut pairs: Vec<(String, u16, u16)> = Vec::new();
    for p in &sb.ports {
        if !pairs
            .iter()
            .any(|(_, h, s)| (*h, *s) == (p.host_port, p.sandbox_port))
        {
            let host = match p.host_ip.as_str() {
                "127.0.0.1" | "::1" | "" => "localhost".to_string(),
                ip => ip.to_string(),
            };
            pairs.push((host, p.host_port, p.sandbox_port));
        }
    }
    let tip = |pairs: &[(String, u16, u16)]| -> String {
        let lines: Vec<String> = (pairs.iter())
            .map(|(host, h, s)| format!("{host}:{h} on your computer → port {s} in the sandbox"))
            .collect();
        lines.join("\n")
    };
    match pairs.as_slice() {
        [] if !sb.running => crossing("Ports", "listed while it runs", Way::None, Tab::Ports)
            .tone(Tone::Weak)
            .tip("A stopped sandbox lists no ports; the published ones come back when it starts."),
        [] => crossing(
            "No ports published",
            "nothing on your computer reaches in",
            Way::None,
            Tab::Ports,
        )
        .tone(Tone::Weak),
        [(host, h, s)] => {
            let how = format!("to port {s} inside");
            crossing(format!("{host}:{h}"), how, Way::In, Tab::Ports).tip(tip(&pairs))
        }
        many => crossing(
            format!("{} ports", many.len()),
            "published on your computer",
            Way::In,
            Tab::Ports,
        )
        .tip(tip(many)),
    }
}

/// The way out: every host with the web kit, else the hosts its policy allows — and the requests
/// it holds for an answer, which is what it waits on.
fn network(app: &App, sb: &Sandbox) -> Crossing {
    let (what, tone, tip) = match insight::network_warning(sb) {
        Some(_) => (
            "Every host",
            Tone::Warn,
            "The web kit lets this sandbox reach every host on the internet.",
        ),
        None => (
            "Allowed hosts",
            Tone::Plain,
            "Its kits' hosts and the rules on the Network tab; anything else is blocked, or held \
             for an answer.",
        ),
    };
    let out = crossing(what, "through the sbx proxy", Way::Out, Tab::Network).tip(tip);
    match app.approvals_for(&sb.name) {
        0 => out.tone(tone),
        n => {
            let requests = if n == 1 { "request" } else { "requests" };
            let how = format!("⚑ {n} {requests} waiting");
            Crossing { how, ..out }.tone(Tone::Warn)
        }
    }
}

// --- drawing -------------------------------------------------------------------------------

/// A crossing's two lines, and the room above and below the crossings.
const ROW_H: f32 = 40.0;
const PAD: f32 = 6.0;
/// An arrow's head, along the line and across it.
const HEAD: Vec2 = Vec2::new(7.0, 4.5);
/// The room between a label column and its arrow, and between an arrow and the wall.
const GAP: f32 = 10.0;

/// Where the map's parts go in `rect`: the host's labels, the wall, the network's labels.
struct Geometry {
    host: Rect,
    wall: Rect,
    out: Rect,
}

/// The shortest arrow.
const ARROW: f32 = 36.0;

impl Geometry {
    /// `label` is what the host's widest label needs, at most a third of the map; `out` what the
    /// network's label needs. The wall stands in the middle where both sides leave room for it,
    /// the host's labels against the left edge and the network's against the right, the arrows
    /// across what is between.
    fn new(rect: Rect, label: f32, out: f32) -> Self {
        let w = rect.width();
        let label = label.clamp(120.0, w * 0.34);
        let wall_w = wall_width(w);
        let earliest = rect.left() + label + 2.0 * GAP + ARROW;
        let latest = rect.right() - out - 2.0 * GAP - ARROW - wall_w;
        let centred = rect.center().x - wall_w / 2.0;
        let wall_x = centred.min(latest).max(earliest);
        let wall = Rect::from_min_max(
            Pos2::new(wall_x, rect.top() + 2.0),
            Pos2::new(wall_x + wall_w, rect.bottom() - 2.0),
        );
        let host = Rect::from_min_max(rect.min, Pos2::new(rect.left() + label, rect.bottom()));
        let out_x = (rect.right() - out).max(wall.right() + 2.0 * GAP + ARROW);
        let out = Rect::from_min_max(Pos2::new(out_x, rect.top()), rect.max);
        Geometry { host, wall, out }
    }
}

/// The map of the selected sandbox, as wide as the page. Returns the tab a click on a crossing
/// asked for.
pub fn show(ui: &mut egui::Ui, app: &App, sb: &Sandbox) -> Option<Tab> {
    let map = map(app, sb);
    let width = ui.available_width();
    // The map is at least as tall as the wall needs for the agent and every one of its kits.
    let inside = Inside::new(ui, &map, wall_width(width) - 2.0 * WALL_PAD);
    let rows = map.host.len().max(2) as f32;
    let height = (rows * ROW_H + 2.0 * PAD).max(inside.height + 2.0 * WALL_PAD + 4.0);
    let (field, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    drafting_field(ui, field);
    let rect = field.shrink2(Vec2::new(FIELD_PAD, 0.0));
    let out = label_width(ui, std::slice::from_ref(&map.out));
    let geo = Geometry::new(rect, label_width(ui, &map.host), out);
    wall(ui, sb, &inside, geo.wall);
    let live = sb.running;

    let mut clicked = None;
    // The host's crossings stand centred beside the wall, however tall its kits make it.
    let first = rect.center().y - map.host.len() as f32 * ROW_H / 2.0;
    for (i, c) in map.host.iter().enumerate() {
        let top = first + i as f32 * ROW_H;
        let row = Rect::from_min_size(Pos2::new(rect.left(), top), Vec2::new(0.0, ROW_H));
        let row = row.with_max_x(geo.wall.left() - 2.0);
        let arrow = (geo.host.right() + GAP, geo.wall.left() - GAP);
        if draw(ui, c, row, geo.host.right(), arrow, live) {
            clicked = Some(c.tab);
        }
    }
    let row = Rect::from_center_size(geo.wall.center(), Vec2::new(0.0, ROW_H));
    let row = Rect::from_x_y_ranges(geo.wall.right() + 2.0..=rect.right(), row.y_range());
    let label = Rect::from_x_y_ranges(geo.out.x_range(), row.y_range());
    let arrow = (geo.wall.right() + GAP, geo.out.left() - GAP);
    if draw_out(ui, &map.out, row, label, arrow, live) {
        clicked = Some(map.out.tab);
    }
    clicked
}

/// The room between the field's edge and the host's labels.
const FIELD_PAD: f32 = 14.0;

/// The distance between two dots of the field's grid.
const GRID: f32 = 16.0;

/// The ground the map is drawn on: the stripes' fill under a dot grid in the hairline colour,
/// as a drawing's paper.
fn drafting_field(ui: &egui::Ui, rect: Rect) {
    let dark = ui.visuals().dark_mode;
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, ui.visuals().faint_bg_color);
    let dot = theme::line(dark);
    let mut y = rect.top() + GRID / 2.0;
    while y < rect.bottom() {
        let mut x = rect.left() + GRID / 2.0;
        while x < rect.right() {
            painter.circle_filled(Pos2::new(x, y), 1.0, dot);
            x += GRID;
        }
        y += GRID;
    }
}

/// The width the host's labels need: the widest of their two lines, and the room before them.
fn label_width(ui: &egui::Ui, host: &[Crossing]) -> f32 {
    let width = |text: &str, size: f32| {
        let font = FontId::proportional(size);
        let galley = ui
            .painter()
            .layout_no_wrap(text.to_string(), font, Color32::WHITE);
        galley.size().x
    };
    (host.iter())
        .map(|c| width(&c.what, theme::BODY).max(width(&c.how, theme::SMALL)))
        .fold(0.0, f32::max)
        + 16.0
}

/// The colours of a crossing: its first line, its second, its arrow.
fn colours(ui: &egui::Ui, tone: Tone) -> (Color32, Color32, Color32) {
    let dark = ui.visuals().dark_mode;
    let (strong, weak) = (
        ui.visuals().strong_text_color(),
        ui.visuals().weak_text_color(),
    );
    match tone {
        Tone::Plain => (strong, weak, weak),
        Tone::Weak => (weak, weak, weak),
        Tone::Warn => (strong, theme::warn(dark), theme::warn(dark)),
        Tone::Danger => (
            theme::danger(dark),
            theme::danger(dark),
            theme::danger(dark),
        ),
    }
}

/// The wall's width in a map `width` wide.
fn wall_width(width: f32) -> f32 {
    (width * 0.22).clamp(130.0, 220.0)
}

/// The room between the wall and what it holds.
const WALL_PAD: f32 = 12.0;

/// What the wall holds, laid out: the agent, then its kits on as many lines as they take.
struct Inside {
    agent: std::sync::Arc<egui::Galley>,
    kits: Vec<std::sync::Arc<egui::Galley>>,
    /// The agent and the kits, from the agent's top to the last kit line's bottom.
    height: f32,
}

/// The room between the agent and its kits.
const AGENT_GAP: f32 = 4.0;

impl Inside {
    fn new(ui: &egui::Ui, map: &Map, room: f32) -> Self {
        let dark = ui.visuals().dark_mode;
        let strong = ui.visuals().strong_text_color();
        let font = FontId::new(theme::BODY + 1.0, theme::strong_family());
        let agent = fitted(ui, &map.agent, font, strong, room);
        let kits = kit_lines(ui, &map.kits, dark, room);
        let kits_h: f32 = kits.iter().map(|k| k.size().y).sum();
        let gap = if kits.is_empty() { 0.0 } else { AGENT_GAP };
        let height = agent.size().y + gap + kits_h;
        Inside {
            agent,
            kits,
            height,
        }
    }
}

/// The chamber's fill: a wash of the agent's colour while the sandbox runs, the page's raised
/// colour while it is stopped.
pub fn chamber(agent: &str, running: bool, dark: bool) -> Color32 {
    match running {
        true => theme::wash(theme::agent_color(agent, dark), dark),
        false => theme::raised(dark),
    }
}

/// The wall's colour: the agent's while the sandbox runs, the stopped grey while it does not.
fn wall_colour(agent: &str, running: bool, dark: bool) -> Color32 {
    match running {
        true => theme::agent_color(agent, dark),
        false => theme::stopped(dark),
    }
}

/// The wall's tooltip: what its look says.
fn wall_tip(running: bool) -> &'static str {
    match running {
        true => "Running: the agent works inside this wall, and what crosses it is in use now.",
        false => {
            "Stopped: the wall stands, and nothing crosses it until the sandbox starts. Its \
             files and kits stay."
        }
    }
}

/// The outer wall's stroke width.
const WALL_STROKE: f32 = 2.0;

/// How far inside the outer wall its dashed inner line runs.
const LINER: f32 = 5.0;

/// The sandbox: a containment wall, a solid outer line and a dashed inner one, around a chamber
/// with the agent and its kits inside, centred. While it runs, the wall is in the agent's colour
/// and the chamber tinted with it; while it is stopped, both are grey.
fn wall(ui: &egui::Ui, sb: &Sandbox, inside: &Inside, rect: Rect) {
    let dark = ui.visuals().dark_mode;
    let id = ui.scope_id().with(("boundary-wall", &sb.name));
    let label = format!("{} sandbox, {}", inside.agent.text(), sb.status_text());
    let response = ui.interact(rect, id, Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &label));
    response.on_hover_text(wall_tip(sb.running));
    let painter = ui.painter();
    let hue = wall_colour(&sb.agent, sb.running, dark);
    painter.rect_filled(rect, 3.0, chamber(&sb.agent, sb.running, dark));
    painter.rect_stroke(
        rect,
        3.0,
        Stroke::new(WALL_STROKE, hue),
        egui::StrokeKind::Inside,
    );
    let liner = rect.shrink(LINER);
    let corners = [
        liner.left_top(),
        liner.right_top(),
        liner.right_bottom(),
        liner.left_bottom(),
        liner.left_top(),
    ];
    let dash = Stroke::new(1.0, hue.gamma_multiply(0.7));
    painter.extend(egui::Shape::dashed_line(&corners, dash, 6.0, 4.0));
    let strong = ui.visuals().strong_text_color();
    let x = |w: f32| rect.center().x - w / 2.0;
    let mut y = rect.center().y - inside.height / 2.0;
    let agent = &inside.agent;
    painter.galley(Pos2::new(x(agent.size().x), y), agent.clone(), strong);
    y += agent.size().y + AGENT_GAP;
    for line in &inside.kits {
        painter.galley(Pos2::new(x(line.size().x), y), line.clone(), strong);
        y += line.size().y;
    }
}

/// The kits by label in their families' colours, as many to a line as fit in `room` and never one
/// cut in two: a line breaks between two kits. A kit wider than `room` on its own is cut with `…`.
fn kit_lines(
    ui: &egui::Ui,
    kits: &[(String, String)],
    dark: bool,
    room: f32,
) -> Vec<std::sync::Arc<egui::Galley>> {
    const SEP: &str = "  ";
    let font = FontId::proportional(theme::SMALL);
    let width = |text: &str| {
        let galley = (ui.painter()).layout_no_wrap(text.to_string(), font.clone(), Color32::WHITE);
        galley.size().x
    };
    let sep = width(SEP);
    let mut lines: Vec<Vec<&(String, String)>> = Vec::new();
    let mut used = 0.0;
    for kit in kits {
        let w = width(&kit.1);
        match lines.last_mut() {
            Some(line) if used + sep + w <= room => {
                line.push(kit);
                used += sep + w;
            }
            _ => {
                lines.push(vec![kit]);
                used = w;
            }
        }
    }
    (lines.into_iter())
        .map(|line| {
            let mut job = egui::text::LayoutJob::default();
            for (i, (kit, label)) in line.into_iter().enumerate() {
                let format = egui::TextFormat::simple(font.clone(), theme::kit_color(kit, dark));
                if i > 0 {
                    job.append(SEP, 0.0, format.clone());
                }
                job.append(label, 0.0, format);
            }
            job.wrap = egui::text::TextWrapping::truncate_at_width(room.max(1.0));
            ui.painter().layout_job(job)
        })
        .collect()
}

/// `text` on one line in `font`, cut with `…` where it is wider than `room`.
fn fitted(
    ui: &egui::Ui,
    text: &str,
    font: FontId,
    colour: Color32,
    room: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, colour);
    job.wrap = egui::text::TextWrapping::truncate_at_width(room.max(1.0));
    ui.painter().layout_job(job)
}

/// The press and hover of a crossing's row: a pointer, an outline under the pointer, its
/// tooltip, and one announcement for a screen reader. Never focused, as no list row is: the
/// list keeps the keyboard.
fn button(ui: &egui::Ui, c: &Crossing, row: Rect) -> bool {
    let id = ui.scope_id().with(("boundary", &c.what, c.tab.index()));
    let response = ui.interact(row, id, Sense::CLICK);
    let label = format!("{}, {} — {}", c.what, c.how, c.tab.title());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    if response.hovered() {
        let dark = ui.visuals().dark_mode;
        let outline = Stroke::new(1.0, theme::line(dark));
        ui.painter()
            .rect_stroke(row, 6.0, outline, egui::StrokeKind::Inside);
    }
    let tip = match c.tip.is_empty() {
        true => format!("Opens the {} tab", c.tab.title()),
        false => format!("{}\n\nOpens the {} tab", c.tip, c.tab.title()),
    };
    let response = response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(tip);
    response.clicked()
}

/// A crossing on the host's side: its two lines in the label column, right of `row.left()` and
/// left of `label_right`, its arrow between `arrow.0` and `arrow.1` toward the wall.
fn draw(
    ui: &egui::Ui,
    c: &Crossing,
    row: Rect,
    label_right: f32,
    arrow: (f32, f32),
    live: bool,
) -> bool {
    let clicked = button(ui, c, row);
    let label = Rect::from_x_y_ranges(row.left() + 8.0..=label_right, row.y_range());
    lines(ui, c, label);
    line(ui, c, arrow, row.center().y, live);
    clicked
}

/// The crossing on the network's side: the arrow from the wall, then its two lines.
fn draw_out(
    ui: &egui::Ui,
    c: &Crossing,
    row: Rect,
    label: Rect,
    arrow: (f32, f32),
    live: bool,
) -> bool {
    let clicked = button(ui, c, row);
    line(ui, c, arrow, row.center().y, live);
    lines(ui, c, label);
    clicked
}

/// What a crossing is over how it crosses, both cut to `rect`'s width.
fn lines(ui: &egui::Ui, c: &Crossing, rect: Rect) {
    let (first, second, _) = colours(ui, c.tone);
    let room = rect.width() - 8.0;
    let what = fitted(ui, &c.what, FontId::proportional(theme::BODY), first, room);
    let how = fitted(ui, &c.how, FontId::proportional(theme::SMALL), second, room);
    let height = what.size().y + how.size().y;
    let top = rect.center().y - height / 2.0;
    let painter = ui.painter();
    painter.galley(Pos2::new(rect.left(), top), what.clone(), first);
    painter.galley(Pos2::new(rect.left(), top + what.size().y), how, second);
}

/// The dash and the gap of an arrow while the sandbox is stopped.
const IDLE_DASH: (f32, f32) = (4.0, 4.0);

/// A crossing's arrow at height `y` from `x.0` to `x.1`, its heads where it points: at the wall
/// for what comes in, at the far end for what goes out, at both for what goes both ways. Solid
/// while the sandbox runs (`live`), dashed while nothing crosses.
fn line(ui: &egui::Ui, c: &Crossing, x: (f32, f32), y: f32, live: bool) {
    if c.way == Way::None || x.1 - x.0 < 2.0 * HEAD.x {
        return;
    }
    let (_, _, colour) = colours(ui, c.tone);
    let painter = ui.painter();
    let (from, to) = (Pos2::new(x.0, y), Pos2::new(x.1, y));
    let stroke = Stroke::new(1.5, colour);
    match live {
        true => {
            painter.line_segment([from, to], stroke);
        }
        false => {
            let (dash, gap) = IDLE_DASH;
            painter.extend(egui::Shape::dashed_line(&[from, to], stroke, dash, gap));
        }
    }
    let head = |tip: Pos2, dir: f32| {
        let back = Pos2::new(tip.x - dir * HEAD.x, tip.y);
        let points = vec![
            tip,
            Pos2::new(back.x, back.y - HEAD.y),
            Pos2::new(back.x, back.y + HEAD.y),
        ];
        painter.add(egui::Shape::convex_polygon(points, colour, Stroke::NONE));
    };
    match c.way {
        // Into the sandbox, and out of it to the network: both point right.
        Way::In | Way::Out => head(to, 1.0),
        Way::Both => {
            head(to, 1.0);
            head(from, -1.0);
        }
        Way::None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aisbx_core::kits::Catalog;
    use aisbx_core::sbx;

    fn app(sb: &Sandbox) -> App {
        App::for_test(Catalog::load().unwrap(), vec![sb.clone()])
    }

    fn port(host_ip: &str, host_port: u16, sandbox_port: u16) -> sbx::Port {
        sbx::Port {
            host_ip: host_ip.into(),
            host_port,
            sandbox_port,
            ..Default::default()
        }
    }

    fn summary(c: &Crossing) -> (&str, &str, Way, Tone, Tab) {
        (c.what.as_str(), c.how.as_str(), c.way, c.tone, c.tab)
    }

    #[test]
    fn a_plain_sandbox_reaches_its_folder_the_skills_and_allowed_hosts() {
        let mut sb = model::sample("box", Some("claude-anthropic"), &["jdk25", "rtk"]);
        sb.running = true;
        let map = map(&app(&sb), &sb);
        assert_eq!(map.agent, "Claude Code");
        let kits: Vec<_> = map
            .kits
            .iter()
            .map(|(id, label)| (id.as_str(), label.as_str()))
            .collect();
        assert_eq!(kits, [("jdk25", "JDK25"), ("rtk", "RTK")]);
        let host: Vec<_> = map.host.iter().map(summary).collect();
        assert_eq!(
            host,
            [
                ("/proj", "read-write", Way::Both, Tone::Plain, Tab::Mounts),
                (
                    "Shared skills",
                    "read-only",
                    Way::In,
                    Tone::Plain,
                    Tab::Mounts
                ),
                (
                    "No ports published",
                    "nothing on your computer reaches in",
                    Way::None,
                    Tone::Weak,
                    Tab::Ports
                ),
            ]
        );
        assert_eq!(
            summary(&map.out),
            (
                "Allowed hosts",
                "through the sbx proxy",
                Way::Out,
                Tone::Plain,
                Tab::Network
            )
        );
    }

    #[test]
    fn no_crossing_calls_your_computer_the_host() {
        let base = model::sample("box", Some("claude-anthropic"), &[]);
        let running = Sandbox {
            running: true,
            ..base.clone()
        };
        let mut cases = vec![base.clone(), running.clone()];
        for ports in [
            vec![port("127.0.0.1", 8080, 3000)],
            vec![port("127.0.0.1", 8080, 3000), port("0.0.0.0", 9090, 4000)],
        ] {
            cases.push(Sandbox {
                ports,
                ..running.clone()
            });
        }
        cases.push(model::sample("box", Some("claude-anthropic"), &["web"]));
        let mut clone = base.clone();
        clone.clone = true;
        clone.skills = Some(Skills::Known(SkillsMode::Readwrite));
        cases.push(clone);
        let mut gone = base.clone();
        gone.workspace_missing = true;
        cases.push(gone);
        let mut bare = base;
        bare.workspace = PathBuf::new();
        cases.push(bare);
        for sb in &cases {
            let map = map(&app(sb), sb);
            for c in map.host.iter().chain([&map.out]) {
                let said = format!("{} | {} | {}", c.what, c.how, c.tip);
                for inner in [
                    "on the host",
                    "this host",
                    "host's",
                    "host folder",
                    "**",
                    "Bind-mount",
                ] {
                    assert!(!said.contains(inner), "{inner}: {said}");
                }
            }
        }
    }

    #[test]
    fn the_workspace_crossing_follows_how_it_is_mounted() {
        let mut sb = model::sample("box", Some("claude-anthropic"), &[]);
        sb.clone = true;
        let c = workspace(&sb);
        assert_eq!((c.how.as_str(), c.way), ("read-only (clone mode)", Way::In));
        sb.clone = false;
        sb.workspace_missing = true;
        assert_eq!(workspace(&sb).tone, Tone::Danger);
        sb.workspace = PathBuf::new();
        let c = workspace(&sb);
        assert_eq!((c.what.as_str(), c.way), ("No folder mounted", Way::None));
    }

    #[test]
    fn other_folders_cross_once_per_access() {
        let mut sb = model::sample("box", Some("claude-anthropic"), &[]);
        sb.ro_mounts = vec!["/caches/m2".into(), "/caches/gradle".into()];
        sb.runtime_mounts = vec![sbx::RuntimeMount {
            host_path: "/data".into(),
            container_target: "/data".into(),
            read_only: false,
        }];
        let crossings: Vec<_> = folders(&sb);
        let got: Vec<_> = crossings.iter().map(summary).collect();
        assert_eq!(
            got,
            [
                ("/data", "read-write", Way::Both, Tone::Plain, Tab::Mounts),
                ("2 folders", "read-only", Way::In, Tone::Plain, Tab::Mounts),
            ]
        );
        assert_eq!(crossings[1].tip, "/caches/m2\n/caches/gradle");
    }

    #[test]
    fn ports_cross_in_and_a_dual_stack_port_counts_once() {
        let mut sb = model::sample("box", Some("claude-anthropic"), &[]);
        sb.running = true;
        sb.ports = vec![port("127.0.0.1", 8080, 3000), port("::1", 8080, 3000)];
        let c = ports(&sb);
        assert_eq!(
            summary(&c),
            (
                "localhost:8080",
                "to port 3000 inside",
                Way::In,
                Tone::Plain,
                Tab::Ports
            )
        );
        sb.ports.push(port("0.0.0.0", 9090, 9090));
        assert_eq!(ports(&sb).what, "2 ports");
        assert!(ports(&sb).tip.contains("0.0.0.0:9090"));
        sb.ports.clear();
        sb.running = false;
        assert_eq!(ports(&sb).how, "listed while it runs");
    }

    #[test]
    fn what_is_wider_than_it_has_to_be_or_waits_is_in_the_warning_colour() {
        let mut sb = model::sample("box", Some("claude-anthropic"), &["web"]);
        sb.skills = Some(Skills::Known(SkillsMode::Readwrite));
        let mut app = app(&sb);
        let map = map(&app, &sb);
        assert_eq!(
            (map.out.what.as_str(), map.out.tone),
            ("Every host", Tone::Warn)
        );
        let store = map.host.iter().find(|c| c.what == "Shared skills").unwrap();
        assert_eq!((store.way, store.tone), (Way::Both, Tone::Warn));

        sb.addons.clear();
        app.approvals = vec![sbx::Approval {
            id: "network:1".into(),
            sandbox: "box".into(),
            title: "example.org:443".into(),
            detail: String::new(),
            options: Vec::new(),
        }];
        let out = network(&app, &sb);
        assert_eq!(
            (out.what.as_str(), out.how.as_str(), out.tone),
            ("Allowed hosts", "⚑ 1 request waiting", Tone::Warn)
        );
        sb.skills = Some(Skills::Known(SkillsMode::Off));
        assert!(skills(&sb).is_none(), "no store, no crossing");
    }

    /// The galleys a map `width` wide draws for `sb`, each with where it is, and the wall.
    fn drawn(sb: &Sandbox, width: f32) -> (Vec<(std::sync::Arc<egui::Galley>, Rect)>, Rect) {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let app = app(sb);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 900.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            show(ui, &app, sb);
        });
        output.textures_delta.clear();
        let mut galleys = Vec::new();
        let mut wall = None;
        fn walk(
            shape: &egui::Shape,
            galleys: &mut Vec<(std::sync::Arc<egui::Galley>, Rect)>,
            wall: &mut Option<Rect>,
        ) {
            match shape {
                egui::Shape::Text(text) => {
                    let rect = text.galley.rect.translate(text.pos.to_vec2());
                    galleys.push((text.galley.clone(), rect));
                }
                egui::Shape::Rect(r) if r.stroke.width == WALL_STROKE => *wall = Some(r.rect),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, galleys, wall)),
                _ => {}
            }
        }
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut galleys, &mut wall);
        }
        (galleys, wall.expect("the wall is drawn"))
    }

    /// The map is a drawing: on a dotted field, a solid wall with a dashed liner inside it.
    #[test]
    fn the_wall_is_drawn_as_containment_on_a_field() {
        let sb = model::sample("box", Some("claude-anthropic"), &["rtk"]);
        let (_, wall) = drawn(&sb, 900.0);
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let app = app(&sb);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 900.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            show(ui, &app, &sb);
        });
        output.textures_delta.clear();
        let (mut dashes, mut dots) = (0, 0);
        fn walk(shape: &egui::Shape, wall: Rect, dashes: &mut usize, dots: &mut usize) {
            match shape {
                egui::Shape::LineSegment { points, .. }
                    if points.iter().all(|p| wall.shrink(LINER - 0.5).contains(*p)) =>
                {
                    *dashes += 1
                }
                egui::Shape::Circle(c) if !wall.contains(c.center) => *dots += 1,
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, wall, dashes, dots)),
                _ => {}
            }
        }
        for clipped in &output.shapes {
            walk(&clipped.shape, wall, &mut dashes, &mut dots);
        }
        assert!(dashes >= 8, "a dashed liner inside the wall: {dashes}");
        assert!(dots >= 20, "a dotted field around it: {dots}");
    }

    /// The shapes a map `width` wide draws for `sb`.
    fn shapes(sb: &Sandbox, width: f32) -> Vec<egui::Shape> {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let app = app(sb);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 900.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            show(ui, &app, sb);
        });
        output.textures_delta.clear();
        fn flat(shape: egui::Shape, out: &mut Vec<egui::Shape>) {
            match shape {
                egui::Shape::Vec(shapes) => shapes.into_iter().for_each(|s| flat(s, out)),
                other => out.push(other),
            }
        }
        let mut out = Vec::new();
        for clipped in output.shapes {
            flat(clipped.shape, &mut out);
        }
        out
    }

    /// While it runs, the wall is in the agent's colour around a chamber tinted with it, and the
    /// arrows are solid; while it is stopped, the wall is grey and every arrow dashed.
    #[test]
    fn the_wall_and_the_arrows_show_whether_it_runs() {
        let dark = egui::Context::default().theme() == egui::Theme::Dark;
        for running in [true, false] {
            let mut sb = model::sample("box", Some("claude-anthropic"), &["rtk"]);
            sb.running = running;
            let shapes = shapes(&sb, 900.0);
            let (_, wall) = drawn(&sb, 900.0);
            let stroke = (shapes.iter())
                .find_map(|s| match s {
                    egui::Shape::Rect(r) if r.stroke.width == WALL_STROKE => Some(r.stroke.color),
                    _ => None,
                })
                .unwrap();
            let fill = (shapes.iter())
                .find_map(|s| match s {
                    egui::Shape::Rect(r) if r.rect == wall && r.fill.a() > 0 => Some(r.fill),
                    _ => None,
                })
                .unwrap();
            let hue = theme::agent_color(&sb.agent, dark);
            let (want_stroke, want_fill) = match running {
                true => (hue, theme::wash(hue, dark)),
                false => (theme::stopped(dark), theme::raised(dark)),
            };
            assert_eq!(
                (stroke, fill),
                (want_stroke, want_fill),
                "running {running}"
            );
            // The arrows: line segments outside the wall, in the map's row band.
            let long = (shapes.iter())
                .filter_map(|s| match s {
                    egui::Shape::LineSegment { points, .. } if !wall.contains(points[0]) => {
                        Some((points[1] - points[0]).length())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert!(!long.is_empty(), "running {running}: arrows drawn");
            let solid = long.iter().filter(|l| **l > IDLE_DASH.0 + 0.5).count();
            match running {
                true => assert!(solid >= 3, "solid arrows: {long:?}"),
                false => assert_eq!(solid, 0, "only dashes: {long:?}"),
            }
        }
    }

    /// On a wide page the wall stands in the middle, the host's labels left of it and the
    /// network's right of it; on the narrowest it still leaves both their room.
    #[test]
    fn the_wall_stands_in_the_middle_where_there_is_room() {
        let sb = model::sample("box", Some("claude-anthropic"), &["rtk"]);
        let (galleys, wall) = drawn(&sb, 900.0);
        let middle = 900.0 / 2.0;
        assert!((wall.center().x - middle).abs() < 2.0, "{wall:?}");
        let (narrow, wall) = drawn(&sb, 560.0);
        let at = |galleys: &[(std::sync::Arc<egui::Galley>, Rect)], text: &str| {
            (galleys.iter())
                .find(|(g, _)| g.text() == text)
                .map(|(_, r)| *r)
                .unwrap()
        };
        for galleys in [&galleys, &narrow] {
            let _ = at(galleys, "Allowed hosts");
        }
        assert!(at(&narrow, "listed while it runs").right() < wall.left());
        assert!(at(&narrow, "through the sbx proxy").left() > wall.right());
        assert!(at(&narrow, "through the sbx proxy").right() <= 560.0);
    }

    /// Also at the narrowest page: the kits by label on as many lines as they take, under the
    /// agent.
    #[test]
    fn the_wall_names_every_kit_whole() {
        let kits = [
            "web",
            "host-gradle",
            "host-maven",
            "host-npm",
            "jdk25",
            "statusline",
        ];
        let mut sb = model::sample("box", Some("opencode-litellm"), &kits);
        sb.agent = "opencode".into();
        let labels: Vec<String> = kits.map(aisbx_core::kits::short_kit_label).to_vec();
        for width in [560.0, 900.0] {
            let (galleys, wall) = drawn(&sb, width);
            let inside: Vec<_> = (galleys.iter())
                .filter(|(_, r)| wall.contains_rect(*r))
                .collect();
            let lines: Vec<&str> = (inside.iter())
                .filter(|(g, _)| labels.iter().any(|k| g.text().contains(k.as_str())))
                .map(|(g, _)| g.text())
                .collect();
            assert!(lines.len() > 1, "{width}: more than one line: {lines:?}");
            for kit in &labels {
                let whole = |line: &&str| line.split("  ").any(|piece| piece == kit);
                assert!(lines.iter().any(whole), "{width}: {kit} in {lines:?}");
            }
            for (g, r) in &inside {
                assert!(!g.elided, "{width}: {:?} is cut at {r:?}", g.text());
            }
            let agent = (inside.iter())
                .find(|(g, _)| g.text() == "OpenCode")
                .map(|(_, r)| *r)
                .expect("the agent is in the wall");
            for (g, r) in &inside {
                if labels.iter().any(|k| g.text().contains(k.as_str())) {
                    assert!(
                        r.top() >= agent.bottom(),
                        "{width}: {:?} under the agent",
                        g.text()
                    );
                }
            }
        }
    }
}
