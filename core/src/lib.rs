//! aisbx-core: what the aisbx frontends share, from every `sbx` call ([`sbx`]) to the application
//! state machine ([`app`]). No UI library: a frontend draws [`app::App`] and feeds it [`input`]
//! events. Guards that keep tests off the host use `cfg(any(test, feature = "test-support"))`.

pub mod actions;
pub mod app;
pub mod bindings;
pub mod create;
#[cfg(test)]
mod create_golden;
pub mod find;
pub mod folders;
pub mod geom;
pub mod headless;
pub mod input;
pub mod insight;
pub mod job;
pub mod kits;
pub mod litellm;
pub mod model;
pub mod model_fetch;
pub mod model_picks;
pub mod models;
pub mod remote;
pub mod repo_skills_ui;
pub mod roster;
pub mod sbx;
pub mod skill_repos;
pub mod skill_select;
pub mod skill_source;
pub mod startdir;
pub mod state;
pub mod stats;
pub mod terminal;
#[cfg(any(test, feature = "test-support"))]
pub mod testkit;
pub mod tmpfs;
pub mod tokens;

/// The aisbx version: the sbx version it needs, then a counter (`0.50.1.0`). See docs/development.md,
/// *aisbx version*.
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").replacen('+', ".", 1)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_version_is_the_required_sbx_version_plus_a_counter() {
        let version = super::version();
        let counter = version
            .strip_prefix(crate::sbx::REQUIRED_VERSION)
            .and_then(|rest| rest.strip_prefix('.'))
            .unwrap_or_else(|| panic!("{version} does not start with the required sbx version"));
        assert!(counter.parse::<u32>().is_ok(), "{version}");
    }
}
