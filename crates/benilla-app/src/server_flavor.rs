//! Which server family the logon speaks to ([`ServerFlavor`]), decided from the install.
//!
//! A Turtle WoW install lists its own `Turtle_*` FrameXML files in its `FrameXML.toc`, which no
//! stock 1.12.1 chain does, so the chain names its server: Turtle data logs in as 1.18.1.7272.
//! `$WOW_FLAVOR` (`turtle`/`twow` or `vanilla`) overrides the detection for the session.

use bevy::prelude::*;

use benilla_assets::{LockRecover, WorldAssets};
use benilla_protocol::ServerFlavor;

/// The flavor the next login attempt carries on its [`crate::net::LoginRequest`].
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Flavor {
    pub(crate) flavor: ServerFlavor,
    /// Set by `$WOW_FLAVOR` or by a finished detection; either way the chain is not read again.
    settled: bool,
}

impl Default for Flavor {
    fn default() -> Self {
        match std::env::var("WOW_FLAVOR").ok().as_deref().and_then(parse) {
            Some(flavor) => Flavor {
                flavor,
                settled: true,
            },
            None => Flavor {
                flavor: ServerFlavor::Vanilla,
                settled: false,
            },
        }
    }
}

/// `$WOW_FLAVOR`'s spellings, any case.
fn parse(value: &str) -> Option<ServerFlavor> {
    match value.trim().to_ascii_lowercase().as_str() {
        "turtle" | "twow" => Some(ServerFlavor::Turtle),
        "vanilla" | "stock" => Some(ServerFlavor::Vanilla),
        _ => None,
    }
}

/// Whether a `FrameXML.toc` is Turtle's: it lists `Turtle_`-prefixed interface files.
fn is_turtle_toc(toc: &str) -> bool {
    toc.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .any(|l| l.to_ascii_lowercase().contains("turtle_"))
}

/// Read the chain's `FrameXML.toc` once it is open, and settle the flavor from it.
fn detect(assets: Option<Res<WorldAssets>>, mut flavor: ResMut<Flavor>) {
    if flavor.settled {
        return;
    }
    let Some(assets) = assets else { return };
    flavor.settled = true;
    let toc = assets
        .chain
        .lock_recover()
        .read("Interface\\FrameXML\\FrameXML.toc")
        .ok();
    if toc.is_some_and(|t| is_turtle_toc(&String::from_utf8_lossy(&t))) {
        flavor.flavor = ServerFlavor::Turtle;
        info!(
            "server flavor: Turtle WoW install — logging in as build {}",
            benilla_protocol::TWOW_CLIENT_BUILD
        );
    }
}

pub(crate) struct ServerFlavorPlugin;

impl Plugin for ServerFlavorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Flavor>().add_systems(Update, detect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_spellings() {
        assert_eq!(parse("TWoW"), Some(ServerFlavor::Turtle));
        assert_eq!(parse(" turtle "), Some(ServerFlavor::Turtle));
        assert_eq!(parse("vanilla"), Some(ServerFlavor::Vanilla));
        assert_eq!(parse(""), None);
    }

    #[test]
    fn a_turtle_toc_names_its_own_files() {
        assert!(is_turtle_toc(
            "## Interface: 11200\nGlobalStrings.lua\nTurtle_TransmogUI.xml\n"
        ));
        assert!(!is_turtle_toc(
            "## Interface: 11200\nGlobalStrings.lua\n# Turtle_Commented.xml\nUIParent.xml\n"
        ));
    }
}
