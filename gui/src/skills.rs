//! The shared skills store (`K`) and the Repo skills picker (*Repo skills* in `K`, or `i` on a
//! sandbox). Core decides the rows and every decision drawn; nothing here reads a file or calls
//! sbx.

use aisbx_core::app::{App, Hit, Screen, SkillRow};
use aisbx_core::input::{KeyCode, KeyEvent};
use aisbx_core::model;
use aisbx_core::repo_skills_ui::{RepoScope, RepoSkillRow, RepoSkillsForm};
use eframe::egui;

use crate::form::{self, Look, Rows};
use crate::page::{self, Item};

/// What a row of the store's page can do, as its buttons: a skill is removed (`d`), each of the
/// store's commands and the repo skills picker run by ↵, as the TUI's rows do.
pub fn row_actions(row: &SkillRow) -> Vec<(&'static str, KeyCode, Look)> {
    match row {
        SkillRow::Skill(_) => vec![("Remove", KeyCode::Char('d'), Look::Danger)],
        SkillRow::Import => vec![("Import…", KeyCode::Enter, Look::Plain)],
        SkillRow::AddRepo => vec![("Add…", KeyCode::Enter, Look::Plain)],
        SkillRow::Update => vec![("Update…", KeyCode::Enter, Look::Plain)],
        SkillRow::RepoSkills => vec![("Open", KeyCode::Enter, Look::Plain)],
        SkillRow::Close => Vec::new(),
    }
}

/// Draw the skills store: the skills in it, each with *Remove*, then the commands that change the
/// store and the repo skills picker, each with the button that runs it; a click on a row selects
/// it and says what it does.
pub fn show_store(ui: &mut egui::Ui, app: &mut App) {
    let Screen::Skills(skills) = &app.screen else {
        return;
    };
    let view: &App = app;
    let rows = view.skill_rows();
    let focus = skills.focus.min(rows.len() - 1);
    let mut list = Rows::new(ui, "skills", focus);
    let known = view.skills.is_some();
    let close = form::index_of(&rows, SkillRow::Close);
    let mut groups: Vec<(&'static str, Vec<usize>)> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let Some(heading) = App::skill_heading(row) else {
            continue;
        };
        match groups.last_mut() {
            Some((h, items)) if *h == heading => items.push(i),
            _ => groups.push((heading, vec![i])),
        }
    }
    let store = match view.skills_store_path() {
        Some(store) => format!("The store: {}.", model::tilde(&store)),
        None => "sbx does not report where the store is.".to_string(),
    };
    let subtitle = std::iter::once(store)
        .chain(view.skills_trust_lines())
        .collect::<Vec<_>>()
        .join(" ");
    let mut picked = None;
    let mut action = None;

    page::bar(ui, "skills", |ui| {
        list.buttons(ui, &[("Close", close, false)]);
    });
    page::body(ui, "skills", "Shared agent skills", &subtitle, |ui| {
        let weak = ui.visuals().weak_text_color();
        if !known {
            page::paragraph(ui, "Reading `sbx skills ls`…", weak);
        }
        // An empty store has no skill rows to put the first heading over, so it is said.
        if known && !rows.iter().any(|r| matches!(r, SkillRow::Skill(_))) {
            page::heading(ui, "Installed skills", "");
            page::paragraph(
                ui,
                "Nothing in the store yet — import this host's, or add from a repository.",
                weak,
            );
        }
        for (heading, items) in &groups {
            page::heading(ui, heading, "");
            page::group(ui, |ui| {
                for (n, i) in items.iter().enumerate() {
                    let row = &rows[*i];
                    let (name, note) = view.skill_view(row);
                    let actions = row_actions(row);
                    let labels: Vec<(&str, Look)> =
                        actions.iter().map(|(l, _, look)| (*l, *look)).collect();
                    let item = Item {
                        title: &name,
                        mark: None,
                        meta: &note,
                        state: None,
                        actions: &labels,
                        reserve: 0.0,
                        selected: list.is_focused(*i),
                        rule: n > 0,
                    };
                    let got = page::item(ui, item, list.follows(*i), |ui| {
                        page::notes(ui, &view.skill_notes(row).join(" "));
                    });
                    if got.item {
                        picked = Some(*i);
                    }
                    if let Some(a) = got.action {
                        action = Some((*i, actions[a].1));
                    }
                }
            });
        }
    });
    if let Some((i, code)) = action {
        app.click(Hit::FormFocus(i));
        app.on_key(KeyEvent::from(code));
    } else if let Some(i) = picked {
        app.click(Hit::FormFocus(i));
    } else if let Some(hit) = list.clicked {
        app.click(hit);
    }
}

/// Draw the Repo skills picker and pass a click on a row or a button to core.
pub fn show_repo(ui: &mut egui::Ui, app: &mut App) {
    let Screen::RepoSkills(repo) = &app.screen else {
        return;
    };
    let view: &App = app;
    let rows = repo.rows();
    let focus = repo.focus.min(rows.len() - 1);
    let mut list = Rows::new(ui, "repo-skills", focus);
    let index = |row: RepoSkillRow| form::index_of(&rows, row);
    let dark = ui.visuals().dark_mode;

    page::bar(ui, "repo-skills", |ui| {
        if repo.dirty {
            form::warn(ui, "Not saved yet.");
        }
        list.buttons(
            ui,
            &[
                ("Save", index(RepoSkillRow::Save), false),
                ("Close", index(RepoSkillRow::Close), false),
            ],
        );
    });
    let title = match &repo.scope {
        RepoScope::Global => "Repo skills · all sandboxes".to_string(),
        RepoScope::Sandbox(name) => format!("Repo skills · {}", view.sandbox_label(name)),
    };
    page::body(ui, "repo-skills", &title, "", |ui| {
        if let Some(why) = &repo.error {
            form::warn(ui, &format!("The selection cannot be read: {why}"));
            form::warn(
                ui,
                "Nothing can be saved until the file is fixed or removed; it is left as it is.",
            );
        } else if repo.repos.is_empty() {
            form::para(
                ui,
                "No skill repo is configured. Add one with `aisbx skill-repo add SOURCE`, then \
                 `aisbx skill-repo fetch`.",
            );
        }
        let mut shown: Option<&str> = None;
        for (i, row) in rows.iter().enumerate() {
            let id = match row {
                RepoSkillRow::All(id) | RepoSkillRow::Skill { repo: id, .. } => Some(id.as_str()),
                RepoSkillRow::SyncNow | RepoSkillRow::Save | RepoSkillRow::Close => None,
            };
            if id != shown {
                footer(ui, repo, shown);
                if let Some(r) = id.and_then(|id| repo.repos.iter().find(|r| r.id == id)) {
                    let commit = match &r.commit {
                        Some(c) => format!("at {}", &c[..c.len().min(12)]),
                        None => "not fetched".to_string(),
                    };
                    form::section(ui, &r.id, &format!("{}  ·  {commit}", r.source));
                }
                shown = id;
            }
            let job = match row {
                RepoSkillRow::All(id) => {
                    tick_row(ui, repo, row, "all skills", repo.locked(id, None))
                }
                RepoSkillRow::Skill { name, locked, .. } => tick_row(ui, repo, row, name, *locked),
                RepoSkillRow::SyncNow => form::with_hint(
                    ui,
                    "▸ Sync now",
                    Some(crate::theme::accent(dark)),
                    "else it applies at the next start",
                ),
                // In the bottom panel.
                RepoSkillRow::Save | RepoSkillRow::Close => continue,
            };
            list.row(ui, Some(i), job);
            if list.is_focused(i) {
                let notes = repo.notes(row);
                if !notes.is_empty() {
                    ui.indent(("repo-notes", i), |ui| form::para(ui, &notes.join(" ")));
                }
            }
        }
        footer(ui, repo, shown);
    });
    if let Some(hit) = list.clicked {
        app.click(hit);
    }
}

/// One tick row: the box, the name, and `global` — greyed — where a global pick locks it.
fn tick_row(
    ui: &egui::Ui,
    repo: &RepoSkillsForm,
    row: &RepoSkillRow,
    name: &str,
    locked: bool,
) -> egui::text::LayoutJob {
    let tick = if repo.ticked(row) { "☑" } else { "☐" };
    let color = locked.then(|| ui.visuals().weak_text_color());
    form::with_hint(
        ui,
        &format!("{tick} {name}"),
        color,
        if locked { "global" } else { "" },
    )
}

/// Under a repo's rows: why it has no cache and the folders it skipped.
fn footer(ui: &mut egui::Ui, repo: &RepoSkillsForm, id: Option<&str>) {
    let Some(r) = id.and_then(|id| repo.repos.iter().find(|r| r.id == id)) else {
        return;
    };
    if let Some(problem) = &r.problem {
        form::para(ui, problem);
    }
    for (name, why) in &r.skipped {
        form::para(ui, &format!("☐ {}   skipped: {why}", name.escape_debug()));
    }
}
