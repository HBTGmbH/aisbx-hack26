use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("core/ sits in the repository root")
        .to_path_buf()
}

fn pages(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.join("README.md")];
    for entry in fs::read_dir(root.join("docs")).unwrap().map(Result::unwrap) {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
    out.sort();
    out
}

fn prose_lines(text: &str) -> Vec<&str> {
    let mut fence = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fence = !fence;
        } else if !fence {
            out.push(line);
        }
    }
    out
}

fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

fn anchors(text: &str) -> HashSet<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out = HashSet::new();
    for line in prose_lines(text) {
        let hashes = line.chars().take_while(|&c| c == '#').count();
        if !(1..=6).contains(&hashes) || !line[hashes..].starts_with(' ') {
            continue;
        }
        let base = slug(&line[hashes + 1..]);
        let n = seen.entry(base.clone()).or_insert(0);
        out.insert(if *n == 0 {
            base.clone()
        } else {
            format!("{base}-{n}")
        });
        *n += 1;
    }
    out
}

fn links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in prose_lines(text) {
        let mut rest = line;
        while let Some(i) = rest.find("](") {
            rest = &rest[i + 2..];
            if let Some(end) = rest.find(')') {
                out.push(rest[..end].to_string());
                rest = &rest[end..];
            }
        }
    }
    out
}

fn broken_links(root: &Path) -> Vec<String> {
    let mut texts: HashMap<PathBuf, String> = HashMap::new();
    for page in pages(root) {
        let text = fs::read_to_string(&page).unwrap();
        texts.insert(page.canonicalize().unwrap(), text);
    }
    let mut broken = Vec::new();
    for page in pages(root) {
        let here = page.canonicalize().unwrap();
        for link in links(&texts[&here]) {
            if link.contains("://") || link.starts_with("mailto:") {
                continue;
            }
            let (file, anchor) = link.split_once('#').unwrap_or((&link, ""));
            let target = if file.is_empty() {
                here.clone()
            } else {
                match page.parent().unwrap().join(file).canonicalize() {
                    Ok(path) => path,
                    Err(_) => {
                        broken.push(format!("{}: {link} (no such file)", page.display()));
                        continue;
                    }
                }
            };
            if anchor.is_empty() {
                continue;
            }
            let text = match texts.get(&target) {
                Some(text) => text.clone(),
                None => fs::read_to_string(&target).unwrap_or_default(),
            };
            if !anchors(&text).contains(anchor) {
                broken.push(format!("{}: {link} (no such heading)", page.display()));
            }
        }
    }
    broken
}

#[test]
fn every_link_in_the_documentation_resolves() {
    let root = repo_root();
    assert!(pages(&root).len() > 5, "the docs pages are missing");
    let broken = broken_links(&root);
    assert!(broken.is_empty(), "broken links: {broken:#?}");
}

#[test]
fn a_heading_slug_follows_github() {
    assert_eq!(
        slug("Headless use (`aisbx up`, `aisbx run`, …)"),
        "headless-use-aisbx-up-aisbx-run-"
    );
    assert_eq!(slug("Ports (`p` / `u`)"), "ports-p--u");
    assert_eq!(
        slug("Host folders (`F`) and `companion.json`"),
        "host-folders-f-and-companionjson"
    );
    let text = "# A\n```\n# B\n```\n## A\n";
    let found = anchors(text);
    assert!(found.contains("a") && found.contains("a-1") && !found.contains("b"));
}

fn images(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in prose_lines(text) {
        let mut rest = line;
        while let Some(i) = rest.find("![") {
            rest = &rest[i + 2..];
            let Some(mid) = rest.find("](") else { break };
            let alt = rest[..mid].to_string();
            rest = &rest[mid + 2..];
            if let Some(end) = rest.find(')') {
                out.push((alt, rest[..end].to_string()));
                rest = &rest[end..];
            }
        }
    }
    out
}

fn image_problems(root: &Path) -> Vec<String> {
    let mut used = HashSet::new();
    let mut problems = Vec::new();
    for page in pages(root) {
        let text = fs::read_to_string(&page).unwrap();
        for (alt, link) in images(&text) {
            if alt.trim().is_empty() {
                problems.push(format!("{}: {link} has no ALT text", page.display()));
            }
            if let Ok(path) = page.parent().unwrap().join(&link).canonicalize() {
                used.insert(path);
            }
        }
    }
    let folder = root.join("docs").join("images");
    if let Ok(entries) = fs::read_dir(&folder) {
        for path in entries.map(|e| e.unwrap().path()) {
            if path.extension().is_some_and(|e| e == "png")
                && !used.contains(&path.canonicalize().unwrap())
            {
                problems.push(format!("{} is in no page", path.display()));
            }
        }
    }
    problems.sort();
    problems
}

#[test]
fn every_image_has_alt_text_and_every_png_is_used() {
    let problems = image_problems(&repo_root());
    assert!(problems.is_empty(), "image problems: {problems:#?}");
}

#[test]
fn an_image_link_gives_its_alt_text() {
    let text = "![A map](images/a.png) and ![](b.png)\n```\n![x](c.png)\n```\n";
    assert_eq!(
        images(text),
        [
            ("A map".to_string(), "images/a.png".to_string()),
            (String::new(), "b.png".to_string())
        ]
    );
}
