//! The sandbox page's Overview under the map ([`crate::boundary`]): four sections of rows, each a
//! label and a value with its gloss. Core decides which rows show and what each says; where a row
//! names something to do, it carries that action as a button.

use aisbx_core::actions::Action;
use aisbx_core::app::App;
use aisbx_core::model::{self, Auth, FolderRelation, Hue, Lifecycle, Sandbox, Skills};
use aisbx_core::sbx::{self, SkillsMode};
use aisbx_core::state;
use eframe::egui::{self, Color32, RichText};

use crate::{actions, theme};

/// The least width of the label column, and the room between the two columns.
const LABEL_W: f32 = 96.0;
const GAP: f32 = 10.0;

/// How a piece of text is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Strong,
    Weak,
    Warn,
    Danger,
    Running,
    /// A kit's or an agent's colour family.
    Hue(Hue),
}

/// One piece of a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub text: String,
    pub tone: Tone,
}

fn part(tone: Tone, text: impl Into<String>) -> Part {
    let text = text.into();
    Part { text, tone }
}

fn plain(text: impl Into<String>) -> Part {
    part(Tone::Plain, text)
}

fn weak(text: impl Into<String>) -> Part {
    part(Tone::Weak, text)
}

fn warn(text: impl Into<String>) -> Part {
    part(Tone::Warn, text)
}

fn danger(text: impl Into<String>) -> Part {
    part(Tone::Danger, text)
}

/// One row: its label, the value's pieces side by side, the gloss after them in the weak colour,
/// and what to do about it — an action and its button's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub label: &'static str,
    pub value: Vec<Part>,
    pub gloss: Option<Part>,
    pub action: Option<(Action, &'static str)>,
}

fn row(label: &'static str, value: Vec<Part>) -> Row {
    Row {
        label,
        value,
        gloss: None,
        action: None,
    }
}

impl Row {
    /// This row with a button for `action`.
    fn acting(self, action: Action, label: &'static str) -> Row {
        let action = Some((action, label));
        Row { action, ..self }
    }
}

fn glossed(label: &'static str, value: Vec<Part>, gloss: Part) -> Row {
    let gloss = Some(gloss);
    Row {
        gloss,
        ..row(label, value)
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What a group of the Overview's rows is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// Who works in the sandbox, and how it reaches its model.
    Agent,
    /// What it works on, and how you reach it.
    Workspace,
    /// When it runs, and whether it stops by itself.
    Lifecycle,
    /// What sbx built it from, and the names sbx knows it by.
    Build,
}

impl Section {
    pub const ALL: [Section; 4] = [
        Section::Agent,
        Section::Workspace,
        Section::Lifecycle,
        Section::Build,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Agent => "Agent",
            Section::Workspace => "Workspace",
            Section::Lifecycle => "Lifecycle",
            Section::Build => "Build",
        }
    }
}

/// The Overview of `sb`: every section that has a row, in [`Section::ALL`]'s order, each row only
/// where it applies.
pub fn sections(app: &App, sb: &Sandbox) -> Vec<(Section, Vec<Row>)> {
    (Section::ALL.into_iter())
        .map(|section| {
            let rows = match section {
                Section::Agent => agent_rows(app, sb),
                Section::Workspace => workspace_rows(app, sb),
                Section::Lifecycle => lifecycle_rows(sb),
                Section::Build => build_rows(app, sb),
            };
            (section, rows)
        })
        .filter(|(_, rows)| !rows.is_empty())
        .collect()
}

/// Every row of the Overview of `sb`, section after section.
#[cfg(test)]
pub fn rows(app: &App, sb: &Sandbox) -> Vec<Row> {
    sections(app, sb)
        .into_iter()
        .flat_map(|(_, rows)| rows)
        .collect()
}

/// The agent, the connection or account it signs in with, and what it is given: model pins,
/// skills, secrets, MCP servers — and a credential it needs that is not stored.
fn agent_rows(app: &App, sb: &Sandbox) -> Vec<Row> {
    let mut out = vec![match app.backend_of(sb) {
        Some(b) => glossed(
            "Harness",
            vec![part(
                Tone::Hue(model::agent_hue(&b.agent)),
                &b.kit.display_name,
            )],
            weak(b.setup_hint()),
        ),
        None => {
            let agent = vec![warn(format!("{} (not from this repo's kits)", sb.agent))];
            match sb.kits_unknown {
                true => glossed("Harness", agent, weak("created outside aisbx")),
                false => row("Harness", agent),
            }
        }
    }];
    if let Some(conn) = &sb.litellm {
        let mut value = vec![plain(conn)];
        if app.litellm_key_missing(conn) {
            let fix = "⚠ key missing — aisbx litellm add <file> --key-stdin --replace";
            value.push(danger(fix));
        }
        out.push(row("Connection", value));
    }
    if let Some(auth) = &sb.auth {
        let users = auth.token_name().map(|t| app.token_users(t)).unwrap_or(0);
        let row = account(auth, users);
        out.push(match auth {
            Auth::Token { stored: false, .. } => row.acting(Action::Auth, "Store a token"),
            _ => row,
        });
    }
    let pinned = app.pinned_models(sb);
    if !pinned.is_empty() {
        let value = pinned
            .into_iter()
            .flat_map(|(alias, name)| [part(Tone::Strong, alias), weak(name)])
            .collect();
        out.push(row("Models", value));
    }
    if let Some(skills) = sb.skills {
        let (value, gloss) = match skills {
            Skills::Known(SkillsMode::Readonly) => {
                ("read-only", "the host store, which it cannot change")
            }
            Skills::Known(SkillsMode::Readwrite) => (
                "read-write",
                "the host store, and it can rewrite what other sandboxes load",
            ),
            Skills::Known(SkillsMode::Off) => {
                ("not shared", "created with --skills=off; fixed at creation")
            }
            Skills::Unrecorded => ("shared", "mode not recorded — created outside aisbx"),
        };
        out.push(glossed("Skills", vec![plain(value)], weak(gloss)));
    }
    let secrets = sb.injected_secrets();
    if !secrets.is_empty() {
        let list: Vec<String> = secrets
            .into_iter()
            .map(|(name, source)| match name {
                None => format!("placeholder only ({source})"),
                Some(name) => format!("{name} ({source})"),
            })
            .collect();
        let gloss = weak("values stay on the host");
        out.push(glossed("Secrets", vec![weak(list.join("  "))], gloss));
    }
    if sb.mcp_gateway {
        let servers = app.registry.static_mcp(&sb.name);
        let mut value = vec![weak("gateway enabled")];
        if !servers.is_empty() {
            value.push(weak(servers.join(" ")));
        }
        out.push(row("MCP", value));
    }
    for service in app.missing_credentials(sb) {
        let text = format!("{service} is not stored — run  sbx secret set {service}");
        out.push(row("⚠ secret", vec![danger(text)]));
    }
    out
}

/// The folder it works on, the one the agent opens in, and the SSH host an editor reaches it by.
fn workspace_rows(app: &App, sb: &Sandbox) -> Vec<Row> {
    let relation = match sb.folder_relation(&app.cwd) {
        FolderRelation::Here => weak("this folder"),
        FolderRelation::Elsewhere => warn("another folder"),
        FolderRelation::Mountless => weak(format!(
            "the agent works in {} inside the sandbox",
            model::MOUNTLESS_WORKDIR
        )),
    };
    let mut out = vec![glossed("Folder", vec![plain(sb.folder_label())], relation)];
    if let Some(dir) = &sb.start_dir {
        let dir = dir.display().to_string();
        out.push(match sb.start_dir_missing() {
            true => glossed(
                "Opens in",
                vec![warn(dir)],
                warn("⚠ gone — the agent starts in the folder above instead"),
            )
            .acting(Action::StartDir, "Pick another"),
            false => glossed(
                "Opens in",
                vec![plain(dir)],
                weak("the agent's working directory, inside the folder above"),
            ),
        });
    }
    out.push(match app.ssh_ready {
        true => glossed(
            "SSH",
            vec![plain(sbx::ssh_host(&sb.name))],
            weak("an editor or a shell connects to it"),
        )
        .acting(Action::Connect, "Connect"),
        false => glossed(
            "SSH",
            vec![plain(sbx::ssh_host(&sb.name))],
            warn("⚠ SSH is not set up on this host yet (sbx setup ssh)"),
        )
        .acting(Action::Connect, "Set up"),
    });
    out
}

/// Whether it stops by itself, and when it was created and last used. The state pill says whether
/// it runs, and the head's callout and the map show a waiting request.
fn lifecycle_rows(sb: &Sandbox) -> Vec<Row> {
    let mut out = Vec::new();
    match sb.lifecycle() {
        Some(Lifecycle::KeepsRunning) => out.push(
            glossed(
                "Lifecycle",
                vec![plain("keeps running after its sessions end")],
                weak("detached mode, until it is stopped; going back is a rebuild"),
            )
            .acting(Action::KeepRunning, "Auto-stop"),
        ),
        Some(Lifecycle::StopsWithPorts) => out.push(
            glossed(
                "Lifecycle",
                vec![plain("stops itself after its last session")],
                weak("and its ports stop answering"),
            )
            .acting(Action::KeepRunning, "Keep running"),
        ),
        None => {}
    }
    out.push(match sb.created {
        Some(epoch) => glossed(
            "Created",
            vec![plain(state::format_local(epoch))],
            weak(state::ago(epoch)),
        ),
        None => row("Created", vec![weak("—")]),
    });
    if let Some(epoch) = sb.last_used {
        let value = vec![plain(state::format_local(epoch))];
        out.push(glossed("Last used", value, weak(state::ago(epoch))));
    }
    out
}

/// The name sbx knows it by, its kits — and whether they changed since — its limits and
/// volumes, its image and how sbx signs it in.
fn build_rows(app: &App, sb: &Sandbox) -> Vec<Row> {
    let mut out = vec![row("sbx name", vec![plain(&sb.name)]), kits(app, sb)];
    if let Some(dir) = &sb.other_checkout {
        let text = format!(
            "matched by name — this sandbox points at {}; a rebuild takes them from here",
            model::tilde(dir)
        );
        out.push(row("⚠ Kits", vec![warn(text)]).acting(Action::Rebuild, "Rebuild"));
    }
    if let Some(drift) = &sb.drift {
        out.push(row("⟳ Update", vec![warn(drift.reason())]).acting(Action::Rebuild, "Rebuild"));
    }
    if sb.cpus.is_some() || sb.memory.is_some() {
        let mut parts = Vec::new();
        if let Some(cpus) = sb.cpus {
            parts.push(plural(cpus as usize, "CPU", "CPUs"));
        }
        if let Some(memory) = &sb.memory {
            parts.push(format!("{memory} memory"));
        }
        let gloss = weak("recorded at creation; a rebuild keeps them");
        out.push(glossed("Limits", vec![plain(parts.join(" · "))], gloss));
    }
    if let Some((value, gloss)) = sb.volumes_note() {
        out.push(glossed("Volumes", vec![plain(value)], weak(gloss)));
    }
    if !sb.image.is_empty() {
        let mut value = vec![weak(&sb.image)];
        value.extend(sb.short_digest().map(weak));
        out.push(row("Image", value));
    }
    if !sb.auth_mode.is_empty() {
        out.push(row("sbx auth", vec![weak(&sb.auth_mode)]));
    }
    out.push(row("Location", vec![weak(sb.location())]));
    out
}

/// Which Anthropic account, and — where it is not the one you would think — what to do about it.
fn account(auth: &Auth, users: usize) -> Row {
    let (value, gloss) = match auth {
        Auth::Login => (
            plain("/login (OAuth)"),
            weak("host-global — shared with every other /login sandbox"),
        ),
        Auth::Token {
            name,
            stored: false,
            ..
        } => (
            danger(format!("setup token “{name}” — not stored")),
            danger("it cannot authenticate until one is stored"),
        ),
        Auth::Token { name, .. } => {
            let others = match users {
                0 | 1 => "no other sandbox uses it yet".to_string(),
                n => format!(
                    "{} uses it too",
                    plural(n - 1, "other sandbox", "other sandboxes")
                ),
            };
            (
                part(Tone::Running, format!("setup token “{name}”")),
                weak(format!("independent of the /login; {others}")),
            )
        }
    };
    glossed("Account", vec![value], gloss)
}

fn kits(app: &App, sb: &Sandbox) -> Row {
    let mut value = Vec::new();
    for name in &sb.addons {
        let display = app.catalog.addon(name).map(|a| a.kit.display_name.clone());
        let display = display.unwrap_or_else(|| aisbx_core::kits::short_kit_label(name));
        value.push(part(Tone::Hue(model::kit_hue(name)), display));
    }
    value.extend(sb.foreign_kits.iter().map(warn));
    if value.is_empty() {
        value.push(weak("none"));
    }
    row("Kits", value)
}

fn color(tone: Tone, ui: &egui::Ui) -> Option<Color32> {
    let dark = ui.visuals().dark_mode;
    match tone {
        Tone::Plain | Tone::Strong => None,
        Tone::Weak => Some(ui.visuals().weak_text_color()),
        Tone::Warn => Some(theme::warn(dark)),
        Tone::Danger => Some(theme::danger(dark)),
        Tone::Running => Some(theme::running(dark)),
        Tone::Hue(hue) => Some(theme::hue(hue, dark)),
    }
}

fn text(p: &Part, ui: &egui::Ui) -> RichText {
    let mut text = RichText::new(&p.text);
    // A value to read first, or a problem, in the strong weight; an agent's name keeps the
    // regular one, its colour marks it already.
    match p.tone {
        Tone::Strong | Tone::Warn | Tone::Danger => text = theme::strong(text),
        Tone::Hue(_) => text = text.strong(),
        _ => {}
    }
    match color(p.tone, ui) {
        Some(color) => text.color(color),
        None => text,
    }
}

/// The room above a section's heading, and between two rows.
const SECTION_GAP: f32 = 18.0;
const ROW_GAP: f32 = 0.0;

/// The selected sandbox's Overview, section by section: a heading, then a row per fact — the
/// label in a column of its own, the value, its gloss and its button on one line that wraps under
/// the value. Returns the action a row's button asked for.
pub fn panel(ui: &mut egui::Ui, app: &App) -> Option<Action> {
    let sb = app.selected()?;
    let value_w = (ui.available_width() - LABEL_W - GAP).max(120.0);
    let mut clicked = None;
    for (i, (section, rows)) in sections(app, sb).into_iter().enumerate() {
        if i > 0 {
            ui.add_space(SECTION_GAP);
        }
        let heading = theme::strong(RichText::new(section.title()).size(theme::SECTION));
        ui.label(heading);
        ui.add_space(2.0);
        for r in rows {
            ui.horizontal_top(|ui| {
                let line = egui::vec2(LABEL_W, ui.spacing().interact_size.y);
                let layout = egui::Layout::left_to_right(egui::Align::Center);
                ui.allocate_ui_with_layout(line, layout, |ui| {
                    ui.set_min_size(line);
                    ui.label(RichText::new(r.label).weak());
                });
                ui.add_space(GAP);
                ui.vertical(|ui| {
                    ui.set_max_width(value_w);
                    ui.horizontal_wrapped(|ui| {
                        for p in &r.value {
                            ui.add(egui::Label::new(text(p, ui)).wrap());
                        }
                        if let Some(gloss) = &r.gloss {
                            ui.add_space(4.0);
                            ui.add(egui::Label::new(text(gloss, ui)).wrap());
                        }
                        if let Some((action, label)) = r.action {
                            if button(ui, app, action, label) {
                                clicked = Some(action);
                            }
                        }
                    });
                });
            });
            ui.add_space(ROW_GAP);
        }
    }
    clicked
}

/// A row's button: small, beside the value it is about, greyed out with the reason when the
/// action cannot run. Returns whether it was clicked.
fn button(ui: &mut egui::Ui, app: &App, action: Action, label: &str) -> bool {
    let state = actions::enabled(app, action, false);
    ui.add_space(8.0);
    let response = ui
        .add_enabled(state.is_ok(), egui::Button::new(label).small())
        .on_hover_text(format!(
            "{} ({})",
            action.summary(),
            actions::key_label(action)
        ));
    match state {
        Ok(()) => response.clicked(),
        Err(why) => {
            response.on_disabled_hover_text(theme::with_code(ui, &why, |t| t));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aisbx_core::kits::Catalog;
    use aisbx_core::models::Slot;

    /// A sandbox with every optional row switched on, and the app it is listed in, holding a
    /// request it waits on.
    fn everything() -> (App, Sandbox) {
        let mut sb = model::sample("gw", Some("claude-litellm"), &["web"]);
        sb.workspace = "/elsewhere".into();
        sb.start_dir = Some("sub".into());
        sb.last_used = Some(1_700_000_100);
        sb.keep_running = true;
        sb.cpus = Some(2);
        sb.tmpfs_volumes = true;
        sb.drift = Some(aisbx_core::model::Drift::Changed(vec!["Statusline".into()]));
        sb.other_checkout = Some("/other/kits".into());
        sb.models.set(Slot::Opus, Some("big-1".into()));
        sb.secrets = vec![("-".into(), "custom".into())];
        sb.mcp_gateway = true;
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb.clone()]);
        app.approvals = vec![sbx::Approval {
            id: "network:1".into(),
            sandbox: "gw".into(),
            title: "example.org:443".into(),
            detail: String::new(),
            options: Vec::new(),
        }];
        (app, sb)
    }

    #[test]
    fn every_optional_row_lands_in_its_section() {
        let (app, sb) = everything();
        let drawn: Vec<String> = (sections(&app, &sb).into_iter())
            .map(|(section, rows)| {
                let labels: Vec<&str> = rows.iter().map(|r| r.label).collect();
                format!("{}: {}", section.title(), labels.join(", "))
            })
            .collect();
        assert_eq!(
            drawn,
            [
                "Agent: Harness, Connection, Models, Skills, Secrets, MCP",
                "Workspace: Folder, Opens in, SSH",
                "Lifecycle: Lifecycle, Created, Last used",
                "Build: sbx name, Kits, ⚠ Kits, ⟳ Update, Limits, Volumes, Image, sbx auth, Location",
            ]
        );
        let rows = rows(&app, &sb);
        let folder = rows.iter().find(|r| r.label == "Folder").unwrap();
        assert_eq!(folder.gloss, Some(warn("another folder")));
        let secrets = rows.iter().find(|r| r.label == "Secrets").unwrap();
        assert_eq!(secrets.value[0].text, "placeholder only (custom)");
    }

    /// Each kit once, by its name in its family's colour: never the CLI id the badges hide.
    #[test]
    fn the_kits_row_names_each_kit_once() {
        let sb = model::sample("box", Some("claude-anthropic"), &["jdk25", "rtk"]);
        let app = App::for_test(Catalog::load().unwrap(), vec![sb.clone()]);
        let kits = kits(&app, &sb);
        let texts: Vec<&str> = kits.value.iter().map(|p| p.text.as_str()).collect();
        let names: Vec<String> = ["jdk25", "rtk"]
            .iter()
            .map(|k| app.catalog.addon(k).unwrap().kit.display_name.clone())
            .collect();
        assert_eq!(texts, names);
        let hues: Vec<Tone> = kits.value.iter().map(|p| p.tone).collect();
        assert_eq!(
            hues,
            [model::kit_hue("jdk25"), model::kit_hue("rtk")].map(Tone::Hue)
        );
    }

    /// `sbx` stays lowercase as the product's name, and a mark before the label is not a letter.
    #[test]
    fn every_label_is_in_sentence_case() {
        let (app, sb) = everything();
        for label in rows(&app, &sb).iter().map(|r| r.label) {
            let word = label.trim_start_matches(['⚑', '⚠', '⟳', ' ']);
            let first = word.chars().next().unwrap();
            assert!(
                first.is_uppercase() || word.starts_with("sbx "),
                "{label:?}"
            );
        }
    }

    #[test]
    fn the_heads_facts_are_no_rows_and_empty_sections_are_left_out() {
        let (app, sb) = everything();
        let labels: Vec<&str> = rows(&app, &sb).iter().map(|r| r.label).collect();
        for head in ["Name", "Status"] {
            assert!(!labels.contains(&head), "{head}: {labels:?}");
        }
        let plain = model::sample("plain", Some("claude-anthropic"), &[]);
        let app = App::for_test(Catalog::load().unwrap(), vec![plain.clone()]);
        let order: Vec<Section> = sections(&app, &plain).iter().map(|(s, _)| *s).collect();
        assert_eq!(order, Section::ALL, "every section has a row");
        let mut sb = plain;
        sb.created = None;
        sb.last_used = None;
        let order: Vec<Section> = sections(&app, &sb).iter().map(|(s, _)| *s).collect();
        assert!(
            order.contains(&Section::Lifecycle),
            "an unknown creation still says so: {order:?}"
        );
    }
}
