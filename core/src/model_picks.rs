//! The model choices of one LiteLLM connection and harness: which model each slot of the agent
//! is on ([`crate::models::Slot::of`]) and which of the gateway's models are switched off. Kept in
//! `<litellm dir>/<connection>/picks-<agent>.json`; the create wizard opens on them and saves
//! them back, and every sandbox on the connection gets them at its next start.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use crate::litellm::Agent;
use crate::models::{valid_id, Model, Overrides, Slot};

/// The file's format version.
const VERSION: u64 = 1;

/// The most switched-off models a file may hold; the gateway's list is capped the same way.
const MAX_DISABLED: usize = crate::models::MAX_HARVEST_MODELS;

/// One connection's choices for one agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Picks {
    /// The agent's slots only; an unset slot is the agent's own choice.
    pub slots: Overrides,
    /// Model ids the agent is not offered.
    pub disabled: BTreeSet<String>,
}

impl Picks {
    /// The models the agent is offered, in the catalog's order.
    pub fn enabled<'a>(&self, models: &'a [Model]) -> Vec<&'a Model> {
        models
            .iter()
            .filter(|m| !self.disabled.contains(&m.id))
            .collect()
    }

    /// The slots of `agent` that point at a model the agent is offered: a pick of a switched-off
    /// model, or of one the gateway no longer lists, is the agent's own choice again.
    pub fn live_slots(&self, agent: Agent, models: &[Model]) -> Overrides {
        let mut out = Overrides::default();
        for slot in Slot::of(agent) {
            let id = self
                .slots
                .get(*slot)
                .filter(|id| !self.disabled.contains(*id) && models.iter().any(|m| m.id == **id));
            out.set(*slot, id.map(str::to_string));
        }
        out
    }
}

/// Where a connection's choices for `agent` live.
pub fn path_in(root: &Path, conn: &str, agent: Agent) -> PathBuf {
    root.join(conn)
        .join(format!("picks-{}.json", agent.as_str()))
}

/// The choices on file, or none: a missing or invalid file is no choice at all, and an id that
/// fails [`valid_id`] or a slot of another agent is dropped.
pub fn load_in(root: &Path, conn: &str, agent: Agent) -> Picks {
    fs::read_to_string(path_in(root, conn, agent))
        .ok()
        .and_then(|text| parse(&text, agent))
        .unwrap_or_default()
}

fn parse(text: &str, agent: Agent) -> Option<Picks> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("version")?.as_u64()? != VERSION {
        return None;
    }
    let mut picks = Picks::default();
    if let Some(slots) = value.get("slots").and_then(Value::as_object) {
        for (key, id) in slots {
            let Some(slot) = Slot::from_key(key).filter(|s| Slot::of(agent).contains(s)) else {
                continue;
            };
            if let Some(id) = id.as_str().filter(|id| valid_id(id)) {
                picks.slots.set(slot, Some(id.to_string()));
            }
        }
    }
    if let Some(off) = value.get("disabled").and_then(Value::as_array) {
        picks.disabled = (off.iter())
            .filter_map(Value::as_str)
            .filter(|id| valid_id(id))
            .take(MAX_DISABLED)
            .map(str::to_string)
            .collect();
    }
    Some(picks)
}

/// The file's text for `picks`: the agent's slots and the switched-off ids, valid ones only.
fn render(picks: &Picks, agent: Agent) -> String {
    let mut slots = Map::new();
    for slot in Slot::of(agent) {
        if let Some(id) = picks.slots.get(*slot).filter(|id| valid_id(id)) {
            slots.insert(slot.key().to_string(), id.into());
        }
    }
    let disabled: Vec<&String> = (picks.disabled.iter())
        .filter(|id| valid_id(id))
        .take(MAX_DISABLED)
        .collect();
    let doc = json!({ "version": VERSION, "slots": slots, "disabled": disabled });
    format!(
        "{}\n",
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    )
}

/// Store a connection's choices for `agent`, through a temp file and a rename. Nothing is written
/// when the file already says the same.
pub fn save_in(root: &Path, conn: &str, agent: Agent, picks: &Picks) -> Result<()> {
    let path = path_in(root, conn, agent);
    let text = render(picks, agent);
    if fs::read_to_string(&path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    let dir = path.parent().context("the picks file has no folder")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let written = fs::write(&tmp, text)
        .with_context(|| format!("writing {}", tmp.display()))
        .and_then(|()| {
            fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))
        });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aisbx-picks-{tag}-{}-{}",
            std::process::id(),
            crate::state::now_epoch()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn model(id: &str) -> Model {
        Model {
            id: id.into(),
            name: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn picks_round_trip_per_connection_and_agent() {
        let root = scratch("round");
        let mut picks = Picks::default();
        picks.slots.set(Slot::Main, Some("gpt-x".into()));
        picks.slots.set(Slot::Small, Some("mini".into()));
        picks.disabled.insert("old".into());
        save_in(&root, "acme", Agent::Opencode, &picks).unwrap();
        assert_eq!(load_in(&root, "acme", Agent::Opencode), picks);
        assert_eq!(load_in(&root, "acme", Agent::Pi), Picks::default());
        assert_eq!(load_in(&root, "other", Agent::Opencode), Picks::default());
        assert!(path_in(&root, "acme", Agent::Opencode).ends_with("acme/picks-opencode.json"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_keeps_only_valid_ids_and_the_agents_own_slots() {
        let text = r#"{"version": 1,
            "slots": {"opus": "claude-x", "main": "a b", "small": "mini", "nope": "x"},
            "disabled": ["ok", "not ok", 3]}"#;
        let picks = parse(text, Agent::Opencode).unwrap();
        assert_eq!(
            picks.slots.get(Slot::Opus),
            None,
            "a Claude slot on OpenCode"
        );
        assert_eq!(picks.slots.get(Slot::Main), None, "an invalid id");
        assert_eq!(picks.slots.get(Slot::Small), Some("mini"));
        assert_eq!(picks.disabled.iter().collect::<Vec<_>>(), ["ok"]);
        assert_eq!(parse(r#"{"version": 2}"#, Agent::Pi), None);
        assert_eq!(parse("not json", Agent::Pi), None);
    }

    #[test]
    fn a_disabled_or_vanished_model_is_no_live_pick() {
        let models = [model("a"), model("b"), model("c")];
        let mut picks = Picks::default();
        picks.slots.set(Slot::Opus, Some("a".into()));
        picks.slots.set(Slot::Sonnet, Some("b".into()));
        picks.slots.set(Slot::Haiku, Some("gone".into()));
        picks.disabled.insert("b".into());
        let live = picks.live_slots(Agent::Claude, &models);
        assert_eq!(live.get(Slot::Opus), Some("a"));
        assert_eq!(live.get(Slot::Sonnet), None);
        assert_eq!(live.get(Slot::Haiku), None);
        let ids: Vec<&str> = picks
            .enabled(&models)
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, ["a", "c"]);
    }

    #[test]
    fn saving_the_same_picks_again_writes_nothing() {
        let root = scratch("same");
        let picks = Picks::default();
        save_in(&root, "acme", Agent::Pi, &picks).unwrap();
        let path = path_in(&root, "acme", Agent::Pi);
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        save_in(&root, "acme", Agent::Pi, &picks).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before);
        let _ = fs::remove_dir_all(&root);
    }
}
