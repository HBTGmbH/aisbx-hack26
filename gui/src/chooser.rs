//! The folder chooser: which folder's sandboxes the grid shows first. Pick a folder the host's
//! sandboxes use (`App::folders`), type a path, or browse (`startdir::children`). Opening one is
//! `App::set_folder`: nothing is stored, and the process keeps its own working directory.

use std::path::{Component, Path, PathBuf};

use aisbx_core::app::App;
use aisbx_core::{model, startdir};
use eframe::egui::{self, Key, RichText, ScrollArea};

use crate::theme;

/// The chooser's state while it is open.
pub struct Chooser {
    /// What the path field holds.
    pub path: String,
    /// The folder the browser shows.
    pub browse: PathBuf,
    /// Its subfolders, read when the browser moves rather than on every frame.
    children: Vec<String>,
    /// Why the last folder picked could not be opened.
    pub error: Option<String>,
}

impl Chooser {
    /// Open on the folder the grid shows now.
    pub fn new(cwd: &Path) -> Self {
        let mut chooser = Self {
            path: cwd.display().to_string(),
            browse: PathBuf::new(),
            children: Vec::new(),
            error: None,
        };
        chooser.browse_to(cwd.to_path_buf());
        chooser
    }

    fn browse_to(&mut self, dir: PathBuf) {
        self.children = startdir::children(&dir);
        self.browse = dir;
    }
}

/// The folder a typed path means: a leading `~` is the home folder, a relative path is taken
/// against `cwd` (the folder the grid shows), and `.` / `..` are folded away.
pub fn resolve(input: &str, cwd: &Path, home: Option<&Path>) -> PathBuf {
    let input = input.trim();
    let tilde_rest = input
        .strip_prefix('~')
        .filter(|rest| rest.is_empty() || rest.starts_with(['/', std::path::MAIN_SEPARATOR]));
    let joined = match (tilde_rest, home) {
        (Some(rest), Some(home)) => {
            home.join(rest.trim_start_matches(['/', std::path::MAIN_SEPARATOR]))
        }
        _ => cwd.join(input),
    };
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Does the window open on the chooser? Only when no FOLDER was given and the process was
/// started at a filesystem root, which is what Finder, the Dock and desktop menus hand a GUI.
pub fn opens_at_start(folder_given: bool, cwd: &Path) -> bool {
    !folder_given && cwd.parent().is_none()
}

/// Draw the chooser as a modal window — disabled while `covered` by another. Returns whether it
/// is still open.
pub fn show(ctx: &egui::Context, app: &mut App, ch: &mut Chooser, covered: bool) -> bool {
    let dark = ctx.global_style().visuals.dark_mode;
    let home = model::home_dir();
    let mut open: Option<PathBuf> = None;
    let mut browse: Option<PathBuf> = None;
    let mut cancel = false;

    let modal = egui::Modal::new(egui::Id::unique("folder-chooser")).show(ctx, |ui| {
        if covered {
            ui.disable();
        }
        ui.set_width(560.0);
        ui.heading("Show a folder's sandboxes");
        let gist = "Its sandboxes come first; the others stay under Other folders.";
        ui.label(RichText::new(gist).weak());

        ui.add_space(10.0);
        ui.strong("Folders with sandboxes");
        ScrollArea::vertical()
            .id_salt("chooser-folders")
            .max_height(160.0)
            .show(ui, |ui| {
                for folder in app.folders() {
                    ui.horizontal(|ui| {
                        let mut text = model::tilde(&folder.path);
                        if folder.current {
                            text.push_str("   (shown now)");
                        }
                        if ui
                            .selectable_label(folder.current, text)
                            .on_hover_text("Show this folder")
                            .clicked()
                        {
                            open = Some(folder.path.clone());
                        }
                        let count = match folder.sandboxes {
                            1 => "1 sandbox".to_string(),
                            n => format!("{n} sandboxes"),
                        };
                        ui.label(RichText::new(count).weak());
                    });
                }
            });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let label = ui.strong("Path");
            let field = ui
                .add(
                    egui::TextEdit::singleline(&mut ch.path)
                        .hint_text("~/projects/app")
                        .desired_width(400.0),
                )
                .labelled_by(label.id)
                .on_hover_text(
                    "~ is your home folder; a relative path starts at the folder shown now",
                );
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if ui.button("Open").clicked() || entered {
                open = Some(resolve(&ch.path, &app.cwd, home.as_deref()));
            }
        });
        if let Some(error) = &ch.error {
            ui.label(RichText::new(error).color(theme::danger(dark)));
        }

        ui.add_space(10.0);
        ui.strong("Browse");
        ui.horizontal(|ui| {
            let parent = ch.browse.parent().map(Path::to_path_buf);
            let up = ui
                .add_enabled(parent.is_some(), egui::Button::new("⬆ Up"))
                .on_hover_text("The folder above")
                .on_disabled_hover_text("This is the top of the filesystem");
            if up.clicked() {
                browse = parent;
            }
            ui.label(model::tilde(&ch.browse));
        });
        ScrollArea::vertical()
            .id_salt("chooser-browse")
            .max_height(200.0)
            .show(ui, |ui| {
                if ch.children.is_empty() {
                    ui.label(RichText::new("no subfolders").weak());
                }
                for name in &ch.children {
                    if ui
                        .selectable_label(false, format!("🗀 {name}"))
                        .on_hover_text("Go into this folder")
                        .clicked()
                    {
                        browse = Some(ch.browse.join(name));
                    }
                }
            });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Open this folder").clicked() {
                open = Some(ch.browse.clone());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Cancel").on_hover_text("Esc").clicked() {
                    cancel = true;
                }
            });
        });
    });

    if let Some(dir) = browse {
        ch.browse_to(dir);
    }
    if let Some(dir) = open {
        match app.set_folder(dir) {
            Ok(()) => return false,
            Err(e) => ch.error = Some(format!("{e:#}")),
        }
    }
    !(cancel || modal.should_close())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_path_is_home_relative_or_absolute() {
        let cwd = Path::new("/work/api");
        let home = Some(Path::new("/home/me"));
        assert_eq!(resolve("~", cwd, home), PathBuf::from("/home/me"));
        assert_eq!(
            resolve("~/src/x", cwd, home),
            PathBuf::from("/home/me/src/x")
        );
        assert_eq!(
            resolve("  ~/src  ", cwd, home),
            PathBuf::from("/home/me/src")
        );
        assert_eq!(resolve("ui", cwd, home), PathBuf::from("/work/api/ui"));
        assert_eq!(resolve("../ui", cwd, home), PathBuf::from("/work/ui"));
        assert_eq!(
            resolve("./a/./b", cwd, home),
            PathBuf::from("/work/api/a/b")
        );
        assert_eq!(resolve("/srv/app", cwd, home), PathBuf::from("/srv/app"));
        assert_eq!(resolve("", cwd, home), PathBuf::from("/work/api"));
        // `~name` is not the home folder, and without one `~` is just a name.
        assert_eq!(resolve("~bob", cwd, home), PathBuf::from("/work/api/~bob"));
        assert_eq!(resolve("~/x", cwd, None), PathBuf::from("/work/api/~/x"));
    }

    #[test]
    fn the_chooser_opens_by_itself_only_at_a_root_without_a_folder() {
        assert!(opens_at_start(false, Path::new("/")));
        assert!(!opens_at_start(true, Path::new("/")));
        assert!(!opens_at_start(false, Path::new("/work/api")));
        assert!(!opens_at_start(true, Path::new("/work/api")));
    }
}
