use warp_core::settings::macros::define_settings_group;
use warp_core::settings::{SupportedPlatforms, SyncToCloud};

define_settings_group!(SwarmKeySettings, settings: [
    prefix: SwarmPrefixKey {
        type: String,
        default: "off".to_string(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        storage_key: "SwarmPrefixKey",
        toml_path: "swarm.keys.prefix",
        description: "tmux-style prefix keys: \"off\", \"ctrl-b\" (as in tmux) or \"ctrl-a\" (as in screen).",
    },
]);

/// The prefix choices: the setting's value and how it's shown.
pub const PREFIXES: [(&str, &str); 3] = [("off", "Off"), ("ctrl-b", "Ctrl-B"), ("ctrl-a", "Ctrl-A")];
