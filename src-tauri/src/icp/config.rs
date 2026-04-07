use std::path::Path;

/// Canister IDs loaded from config.toml.
#[derive(serde::Deserialize, Clone)]
pub struct CanisterIds {
    pub pattern: String,
    pub governance: String,
    pub identity: String,
}

/// Top-level app config loaded from {app_data_dir}/config.toml.
#[derive(serde::Deserialize, Clone)]
pub struct AppConfig {
    pub canisters: CanisterIds,
    /// ICP network URL. Use "https://ic0.app" for mainnet or
    /// "http://127.0.0.1:4943" for a local dfx replica.
    pub icp_url: String,
    /// URL of the deployed Cloudflare Worker (e.g. "https://relay-worker.yourname.workers.dev").
    /// Used by open_upgrade_page to initialise a Paystack checkout session.
    /// Optional so existing config.toml files without this key still parse.
    #[serde(default)]
    pub worker_url: Option<String>,
}

/// Tauri managed state wrapping the loaded AppConfig.
pub struct ConfigState(pub AppConfig);

/// Loads and parses {app_data_dir}/config.toml.
///
/// Args:
///   app_data_dir: Path to the OS-specific app data directory.
///
/// Returns:
///   Parsed AppConfig. Panics with a clear message if the file is absent or malformed.
pub fn load_config(app_data_dir: &Path) -> AppConfig {
    let path = app_data_dir.join("config.toml");
    let contents = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "config.toml not found at {} — copy config.toml.example from the repo root \
             and fill in canister IDs before launching",
            path.display()
        )
    });
    toml::from_str(&contents).unwrap_or_else(|e| {
        panic!("failed to parse config.toml at {}: {e}", path.display())
    })
}
