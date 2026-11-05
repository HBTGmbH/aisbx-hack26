//! The main list: which rows it has and in what order, what a sandbox's row says beyond the
//! facts [`Sandbox`] already words, and how its three columns share the width.

use std::path::{Path, PathBuf, MAIN_SEPARATOR};

use crate::app::{App, Cell};
use crate::model::{CardMarker, CardNote, Sandbox};

/// One row of the main list, in the order [`App::roster`] lists them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// The current folder's heading: `This folder  ~/proj  3 sandboxes`.
    Here,
    /// A sandbox, by its cell in [`App::cells`].
    Sandbox(usize),
    /// The "+ New sandbox" row that closes the current folder's section, by its cell.
    New(usize),
    /// The other folders' heading, which `a` (or a click on it) folds away and back.
    Other,
    /// A folder heading inside the other folders' section, over the sandboxes that mount it.
    Folder(FolderHead),
}

/// A folder heading of the other folders' section — see [`Row::Folder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderHead {
    /// The workspace the sandboxes under it mount; empty for the mountless ones, which share one
    /// heading.
    pub path: PathBuf,
    /// The folder is gone from the host, which the heading says once for every row under it.
    pub missing: bool,
    /// How many sandboxes stand under the heading.
    pub sandboxes: usize,
}

impl FolderHead {
    /// The sandboxes under it mount no host folder at all.
    pub fn mountless(&self) -> bool {
        self.path.as_os_str().is_empty()
    }
}

impl App {
    /// The main list's rows, top to bottom: the current folder's heading, its sandboxes and the
    /// "+ New sandbox" row, then the other folders' heading and, while it is expanded, one folder
    /// heading over each folder's sandboxes.
    ///
    /// [`App::cells`] already sorts the second section by folder, so each folder gets one heading.
    pub fn roster(&self) -> Vec<Row> {
        let mut rows = vec![Row::Here];
        rows.extend((0..self.here).map(Row::Sandbox));
        rows.push(Row::New(self.here));
        if self.elsewhere == 0 {
            return rows;
        }
        rows.push(Row::Other);
        let mut head: Option<usize> = None;
        for cell in self.here + 1..self.cells.len() {
            let Some(sb) = self.sandbox_at(cell) else {
                continue;
            };
            let same = |row: &Row| matches!(row, Row::Folder(f) if f.path == sb.workspace);
            match head.filter(|at| same(&rows[*at])) {
                Some(at) => {
                    if let Row::Folder(f) = &mut rows[at] {
                        f.sandboxes += 1;
                    }
                }
                None => {
                    head = Some(rows.len());
                    rows.push(Row::Folder(FolderHead {
                        path: sb.workspace.clone(),
                        missing: sb.workspace_missing,
                        sandboxes: 1,
                    }));
                }
            }
            rows.push(Row::Sandbox(cell));
        }
        rows
    }

    /// How many folders the other folders' sandboxes mount, whether or not the section is
    /// expanded. The mountless ones count as one.
    pub fn other_folders(&self) -> usize {
        let here: Vec<usize> = self.cells[..self.here.min(self.cells.len())]
            .iter()
            .filter_map(|cell| match cell {
                Cell::Sandbox(i) => Some(*i),
                Cell::New => None,
            })
            .collect();
        let mut folders: Vec<&Path> = Vec::new();
        for (i, sb) in self.sandboxes.iter().enumerate() {
            if !here.contains(&i) && !folders.contains(&sb.workspace.as_path()) {
                folders.push(&sb.workspace);
            }
        }
        folders.len()
    }

    /// How many of the host's sandboxes run, and how many there are, folded section included;
    /// `None` while the host has none.
    pub fn host_running(&self) -> Option<(usize, usize)> {
        let all = self.sandboxes.len();
        let running = self.sandboxes.iter().filter(|sb| sb.running).count();
        (all > 0).then_some((running, all))
    }
}

impl Sandbox {
    /// The markers a row draws after its note (`⚑ ⟳`): the requests waiting and the kit drift,
    /// whichever the note does not already name.
    pub fn row_markers(&self, waiting: usize) -> Vec<CardMarker> {
        let note = self.card_note(waiting);
        let mut out = Vec::new();
        if waiting > 0 && !matches!(note, Some(CardNote::Approvals(_))) {
            out.push(CardMarker::Approvals);
        }
        if self.kit_drift() && !matches!(note, Some(CardNote::KitsChanged(_))) {
            out.push(CardMarker::KitsChanged);
        }
        out
    }

    /// Where the agent opens inside the workspace, as the row writes it under the label:
    /// `› api`. `None` at the workspace root, and for a sandbox that mounts nothing.
    pub fn start_inside(&self) -> Option<String> {
        if self.mountless() {
            return None;
        }
        crate::startdir::inside(self.start_dir.as_deref())
    }

    /// Does the row need its second line for a start folder, a kit, a note or a marker?
    pub fn second_line(&self, waiting: usize) -> bool {
        self.start_inside().is_some()
            || !self.kit_badges().is_empty()
            || self.card_note(waiting).is_some()
            || !self.row_markers(waiting).is_empty()
    }
}

/// The heading of the current folder's section.
pub const HERE_HEADING: &str = "This folder";

/// The heading of the second section, which `a` folds away and back.
pub const OTHER_HEADING: &str = "Sandboxes in other folders";

/// What a folder heading of the second section says for the sandboxes that mount no folder.
pub const NO_FOLDER: &str = "without a folder";

/// What a folder heading says after a folder that is gone from the host.
pub const FOLDER_GONE: &str = "folder not found";

/// `1 sandbox`, `3 sandboxes`.
pub fn count(n: usize) -> String {
    format!("{n} sandbox{}", if n == 1 { "" } else { "es" })
}

/// The other folders' heading count: `6 sandboxes in 5 folders`, `1 sandbox in 1 folder`.
pub fn other_count(sandboxes: usize, folders: usize) -> String {
    format!(
        "{} in {folders} folder{}",
        count(sandboxes),
        if folders == 1 { "" } else { "s" }
    )
}

/// A folder as a heading draws it: the path up to its last part (`~/projects/`), then the
/// last part (`aisbx`), which the heading sets in bold.
pub fn folder_parts(path: &Path) -> (String, String) {
    let shown = crate::model::tilde(path);
    match shown.rfind([MAIN_SEPARATOR, '/']) {
        Some(at) if at + 1 < shown.len() => (shown[..=at].to_string(), shown[at + 1..].to_string()),
        _ => (String::new(), shown),
    }
}

/// How wide the list's three columns are — name, runs, state — out of `avail`, in whatever unit
/// the frontend counts (characters, points). `want` is what each column's widest entry needs:
/// for the middle one the agent and its connection, not the kits.
pub fn columns(avail: f32, want: [f32; 3]) -> [f32; 3] {
    let avail = avail.max(0.0);
    let agent = want[1].clamp(0.0, avail * 2.0 / 5.0);
    let state = want[2].clamp(0.0, avail * 3.0 / 10.0);
    let name = want[0].clamp(0.0, (avail - agent - state).max(0.0));
    [name, avail - name - state, state]
}

/// How many of the kit badges fit in `room`, in order, with `gap` between two of them. When not
/// all do, room is kept for the `+N` badge that counts the rest, whose width `more(n)` gives.
/// `None` when not even the count fits, and the cell stays empty.
pub fn fit_badges(
    widths: &[f32],
    room: f32,
    gap: f32,
    more: impl Fn(usize) -> f32,
) -> Option<usize> {
    let used = |k: usize| widths[..k].iter().sum::<f32>() + gap * k.saturating_sub(1) as f32;
    if used(widths.len()) <= room {
        return Some(widths.len());
    }
    (0..widths.len()).rev().find(|&k| {
        let hidden = widths.len() - k;
        let before = if k == 0 { 0.0 } else { gap };
        used(k) + before + more(hidden) <= room
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kits::Catalog;
    use crate::model;

    fn app(sandboxes: Vec<Sandbox>) -> App {
        App::for_test(Catalog::load().unwrap(), sandboxes)
    }

    #[test]
    fn the_host_count_takes_every_folder_and_none_without_sandboxes() {
        let mut sandboxes = mixed();
        sandboxes[0].running = true;
        sandboxes[4].running = true;
        let all = sandboxes.len();
        let mut app = app(sandboxes);
        app.show_other = false;
        assert_eq!(app.host_running(), Some((2, all)));
        assert_eq!(self::app(Vec::new()).host_running(), None);
    }

    /// Two sandboxes here, three in two other folders, one mounting nothing.
    fn mixed() -> Vec<Sandbox> {
        let mut out = vec![
            model::sample("here-1", Some("claude-anthropic"), &[]),
            model::sample("here-2", Some("claude-anthropic"), &[]),
        ];
        for (i, dir) in ["/work/api", "/work/api", "/work/ui", ""]
            .iter()
            .enumerate()
        {
            let mut sb = model::sample(&format!("far-{i}"), Some("claude-anthropic"), &[]);
            sb.workspace = PathBuf::from(dir);
            out.push(sb);
        }
        out
    }

    fn folder(path: &str, sandboxes: usize) -> Row {
        Row::Folder(FolderHead {
            path: PathBuf::from(path),
            missing: false,
            sandboxes,
        })
    }

    #[test]
    fn the_roster_groups_the_other_sandboxes_under_their_folders() {
        let mut app = app(mixed());
        assert_eq!((app.here, app.elsewhere), (2, 4));
        assert_eq!(
            app.roster(),
            [
                Row::Here,
                Row::Sandbox(0),
                Row::Sandbox(1),
                Row::New(2),
                Row::Other,
                folder("", 1),
                Row::Sandbox(3),
                folder("/work/api", 2),
                Row::Sandbox(4),
                Row::Sandbox(5),
                folder("/work/ui", 1),
                Row::Sandbox(6),
            ]
        );
        assert_eq!(app.other_folders(), 3);
        assert!(matches!(&app.roster()[5], Row::Folder(f) if f.mountless()));

        app.show_other = false;
        app.refresh_cells();
        assert_eq!(app.roster().last(), Some(&Row::Other));
        assert_eq!(
            app.other_folders(),
            3,
            "the folded heading still counts them"
        );

        let mut empty = self::app(mixed());
        empty.cwd = PathBuf::from("/elsewhere");
        empty.refresh_cells();
        assert_eq!(empty.roster()[..3], [Row::Here, Row::New(0), Row::Other]);
        let alone = self::app(vec![model::sample("a", Some("claude-anthropic"), &[])]);
        assert_eq!(alone.roster(), [Row::Here, Row::Sandbox(0), Row::New(1)]);
    }

    #[test]
    fn a_missing_folder_is_marked_on_its_heading() {
        let mut gone = model::sample("gone", None, &[]);
        gone.workspace = PathBuf::from("/gone");
        gone.workspace_missing = true;
        let app = app(vec![gone]);
        let heads: Vec<FolderHead> = app
            .roster()
            .into_iter()
            .filter_map(|row| match row {
                Row::Folder(head) => Some(head),
                _ => None,
            })
            .collect();
        assert_eq!(
            heads,
            [FolderHead {
                path: PathBuf::from("/gone"),
                missing: true,
                sandboxes: 1,
            }]
        );
    }

    #[test]
    fn the_counts_read_as_words() {
        assert_eq!(count(1), "1 sandbox");
        assert_eq!(count(3), "3 sandboxes");
        assert_eq!(other_count(1, 1), "1 sandbox in 1 folder");
        assert_eq!(other_count(6, 5), "6 sandboxes in 5 folders");
    }

    #[test]
    fn a_folder_splits_before_its_last_part() {
        let nested = Path::new("/work").join("projects").join("aisbx");
        let (above, last) = folder_parts(&nested);
        assert_eq!(last, "aisbx");
        assert_eq!(format!("{above}{last}"), crate::model::tilde(&nested));
        assert_eq!(
            folder_parts(Path::new("/gone")),
            ("/".into(), "gone".into())
        );
        assert_eq!(folder_parts(Path::new("/")), (String::new(), "/".into()));
    }

    #[test]
    fn a_row_marks_only_what_its_note_does_not_say() {
        let mut sb = model::sample("a", Some("claude-anthropic"), &[]);
        assert!(sb.row_markers(0).is_empty());
        sb.drift = Some(crate::model::Drift::Changed(vec!["Statusline".into()]));
        assert_eq!(
            sb.card_note(0),
            Some(CardNote::KitsChanged(crate::model::DriftKind::Changed))
        );
        assert!(sb.row_markers(0).is_empty(), "the note says it");
        assert_eq!(sb.row_markers(2), [CardMarker::KitsChanged]);
        sb.workspace_missing = true;
        assert_eq!(
            sb.row_markers(2),
            [CardMarker::KitsChanged],
            "the folder's heading says it is missing"
        );
        sb.unresponsive = true;
        assert_eq!(sb.card_note(2), Some(CardNote::Unresponsive));
        assert_eq!(
            sb.row_markers(2),
            [CardMarker::Approvals, CardMarker::KitsChanged]
        );
    }

    #[test]
    fn a_row_has_a_second_line_only_with_something_on_it() {
        let mut sb = model::sample("a", Some("claude-anthropic"), &[]);
        assert!(
            !sb.second_line(0),
            "no kit, no start folder, nothing to say"
        );
        assert!(sb.second_line(1), "a request waits");
        sb.start_dir = Some(PathBuf::from("api"));
        assert_eq!(sb.start_inside().as_deref(), Some("› api"));
        assert!(sb.second_line(0));
        sb.start_dir = None;
        sb.addons = vec!["rtk".into()];
        assert!(sb.second_line(0));
        let mut bare = model::sample("b", Some("claude-anthropic"), &[]);
        bare.workspace = PathBuf::new();
        bare.start_dir = Some(PathBuf::from("api"));
        assert_eq!(bare.start_inside(), None);
    }

    #[test]
    fn the_columns_share_the_width_by_need() {
        assert_eq!(columns(100.0, [20.0, 25.0, 20.0]), [20.0, 60.0, 20.0]);
        assert_eq!(columns(100.0, [70.0, 40.0, 20.0]), [40.0, 40.0, 20.0]);
        assert_eq!(columns(100.0, [70.0, 20.0, 20.0]), [60.0, 20.0, 20.0]);
        assert_eq!(columns(80.0, [40.0, 27.0, 33.0]), [29.0, 27.0, 24.0]);
        assert_eq!(columns(100.0, [30.0, 60.0, 10.0]), [30.0, 60.0, 10.0]);
        assert_eq!(columns(100.0, [60.0, 60.0, 10.0]), [50.0, 40.0, 10.0]);
        assert_eq!(columns(100.0, [10.0, 10.0, 50.0]), [10.0, 60.0, 30.0]);
        let [a, b, c] = columns(30.0, [90.0, 90.0, 90.0]);
        assert!((a + b + c - 30.0).abs() < 1e-3, "{a} {b} {c}");
        assert_eq!(columns(-5.0, [10.0, 10.0, 10.0]), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn kit_badges_fit_in_order_with_a_count_for_the_rest() {
        let more = |_: usize| 2.0;
        assert_eq!(fit_badges(&[3.0, 3.0, 3.0], 11.0, 1.0, more), Some(3));
        assert_eq!(fit_badges(&[3.0, 3.0, 3.0], 10.0, 1.0, more), Some(2));
        assert_eq!(fit_badges(&[3.0, 3.0, 3.0], 6.0, 1.0, more), Some(1));
        assert_eq!(fit_badges(&[9.0, 3.0], 4.0, 1.0, more), Some(0));
        assert_eq!(fit_badges(&[9.0, 3.0], 1.0, 1.0, more), None);
        assert_eq!(fit_badges(&[], 0.0, 1.0, more), Some(0));
    }
}
