use warp_core::settings::macros::define_settings_group;
use warp_core::settings::{SupportedPlatforms, SyncToCloud};

define_settings_group!(SwarmUpdateSettings, settings: [
    download_in_background: SwarmDownloadUpdatesInBackground {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::MAC,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        storage_key: "SwarmDownloadUpdatesInBackground",
        toml_path: "swarm.updates.download_in_background",
        description: "Whether Swarm downloads a new version by itself and then offers to restart into it.",
    },
    install_when_idle: SwarmInstallUpdatesWhenIdle {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::MAC,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        storage_key: "SwarmInstallUpdatesWhenIdle",
        toml_path: "swarm.updates.install_when_idle",
        description: "Whether Swarm installs a downloaded update and restarts while you're in another app.",
    },
]);
