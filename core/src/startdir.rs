//! Where the agent opens, when that is not the top of the workspace. The mount does not change;
//! the session becomes `sbx exec -w <dir>` of the agent binary.

use std::path::{Component, Path, PathBuf};

/// The immediate subdirectories of `dir` without the hidden ones, sorted for a stable list. An
/// unreadable directory lists as empty.
pub fn children(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b)));
    names
}

/// The absolute directory a start folder means, given the workspace it is relative to. An empty
/// or absent `rel` is the workspace root. The sandbox mounts the workspace at the same path.
pub fn resolve(root: &Path, rel: Option<&Path>) -> PathBuf {
    match rel.filter(|r| !r.as_os_str().is_empty()) {
        Some(rel) => root.join(rel),
        None => root.to_path_buf(),
    }
}

/// Whether a stored start folder is gone from the host. No start folder is never missing: the
/// list checks the workspace root itself (`workspace_missing`).
pub fn missing(root: &Path, rel: Option<&Path>) -> bool {
    rel.filter(|r| !r.as_os_str().is_empty())
        .is_some_and(|rel| !root.join(rel).is_dir())
}

/// Reject anything that would climb out of the workspace, or is not a plain relative path. A user
/// can edit the registry, and the value ends up as `sbx exec -w`.
pub fn is_inside(rel: &Path) -> bool {
    !rel.as_os_str().is_empty()
        && rel
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// How a start folder reads next to the workspace it belongs to: `~/work › notes/drafts`.
pub fn describe(root: &Path, rel: Option<&Path>) -> String {
    match inside(rel) {
        Some(inside) => format!("{} {inside}", crate::model::tilde(root)),
        None => crate::model::tilde(root),
    }
}

/// The start folder alone, as the list's row writes it under the label: `› notes/drafts`.
/// `None` at the workspace root.
pub fn inside(rel: Option<&Path>) -> Option<String> {
    let rel = rel.filter(|r| !r.as_os_str().is_empty())?;
    Some(format!(
        "› {}",
        rel.display()
            .to_string()
            .replace(std::path::MAIN_SEPARATOR, "/")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_treats_no_start_folder_as_the_workspace_root() {
        let root = Path::new("/ws");
        assert_eq!(resolve(root, None), root);
        assert_eq!(resolve(root, Some(Path::new(""))), root);
        assert_eq!(resolve(root, Some(Path::new("a/b"))), Path::new("/ws/a/b"));
    }

    #[test]
    fn a_start_folder_may_not_climb_out_of_the_workspace() {
        assert!(is_inside(Path::new("a")));
        assert!(is_inside(Path::new("a/b/c")));
        assert!(!is_inside(Path::new("")));
        assert!(!is_inside(Path::new("..")));
        assert!(!is_inside(Path::new("a/../../b")));
        assert!(!is_inside(Path::new("/abs")));
    }

    #[test]
    fn describe_separates_what_is_mounted_from_where_the_agent_opens() {
        let root = Path::new("/ws/work");
        assert_eq!(describe(root, None), "/ws/work");
        assert_eq!(describe(root, Some(Path::new("notes"))), "/ws/work › notes");
        assert_eq!(inside(None), None);
        assert_eq!(inside(Some(Path::new(""))), None);
        let nested = Path::new("notes").join("drafts");
        assert_eq!(inside(Some(&nested)).as_deref(), Some("› notes/drafts"));
    }

    #[test]
    fn children_lists_visible_subdirectories_in_order() {
        let tmp = std::env::temp_dir().join(format!("aisbx-startdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for name in ["beta", "Alpha", ".hidden", "gamma"] {
            std::fs::create_dir_all(tmp.join(name)).unwrap();
        }
        std::fs::write(tmp.join("a-file.txt"), "x").unwrap();
        assert_eq!(children(&tmp), ["Alpha", "beta", "gamma"]);
        assert!(children(&tmp.join("a-file.txt")).is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_only_flags_a_start_folder_that_is_gone() {
        let root = std::env::temp_dir();
        assert!(!missing(&root, None));
        assert!(missing(&root, Some(Path::new("aisbx-definitely-not-here"))));
    }
}
