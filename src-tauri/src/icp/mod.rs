pub mod agent;
pub mod config;
pub mod sync;

pub use config::{AppConfig, ConfigState};

/// Tauri-managed state: PEM bytes of the device's Ed25519 identity keypair.
/// Generated once on first launch and persisted to {app_data_dir}/device_identity.pem.
/// Passed to `agent::build_agent` on every ICP call so each call is authenticated.
pub struct DeviceIdentityState(pub Vec<u8>);
