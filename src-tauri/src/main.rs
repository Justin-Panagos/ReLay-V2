// Prevents an additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod db;
mod download;
mod icp;
mod pro;
mod shield;
mod torrent;

use db::DbState;
use download::lifecycle::{LifecycleState, QueueState};
use icp::ConfigState;
use pro::{LicenceCache, LicenceCacheState};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::Manager;
use torrent::{TorrentPollerState, TorrentSessionState};

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app
                .path_resolver()
                .app_data_dir()
                .expect("could not resolve app data dir");

            let conn = db::init_db(dir.clone()).expect("failed to initialise database");
            let icp_config = match icp::config::load_config(&dir) {
                Ok(cfg) => cfg,
                Err(msg) => {
                    tauri::api::dialog::blocking::message(
                        None::<&tauri::Window>,
                        "Configuration Error",
                        msg,
                    );
                    std::process::exit(1);
                }
            };

            // Read default_folder for the torrent session output directory.
            let torrent_output: PathBuf = db::get_setting(&conn, "default_folder")
                .ok()
                .flatten()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));

            // Initialise the librqbit torrent session on Tauri's Tokio runtime.
            // Session::new_with_opts() is async; block_on bridges the sync setup closure.
            // UPnP port forwarding and a fixed listen port range are enabled so the
            // router maps an inbound port, allowing peers to dial in directly.
            // defer_writes_up_to buffers piece writes in memory (32 MB) so disk I/O
            // does not bottleneck fast connections.
            let torrent_session = tauri::async_runtime::block_on(
                librqbit::Session::new_with_opts(
                    torrent_output,
                    librqbit::SessionOptions {
                        enable_upnp_port_forwarding: true,
                        listen_port_range: Some(6881..6891),
                        defer_writes_up_to: Some(32),
                        ..Default::default()
                    },
                ),
            )
            .expect("failed to initialise torrent session");

            // Seed the licence cache from the last persisted ICP-verified value.
            // The cache is marked stale (86401s ago) so run_startup() always triggers
            // a live ICP check; the seed merely avoids a blank UI during that check.
            let seed_status = db::get_setting(&conn, "licence_status")
                .ok()
                .flatten()
                .unwrap_or_else(|| "free".to_string());
            let seed_expiry = db::get_setting(&conn, "licence_expiry")
                .ok()
                .flatten()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);

            app.manage(ConfigState(icp_config.clone()));
            app.manage(
                reqwest::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(10))
                    .build()
                    .expect("failed to build HTTP client"),
            );
            app.manage(LicenceCacheState(Mutex::new(LicenceCache {
                status: seed_status,
                expiry: seed_expiry,
                // Force stale so run_startup() immediately issues a live ICP check.
                verified_at: Instant::now() - Duration::from_secs(86_401),
            })));
            app.manage(DbState(Mutex::new(conn)));
            app.manage(LifecycleState(Mutex::new(HashMap::new())));
            app.manage(QueueState(Mutex::new(VecDeque::new())));
            app.manage(TorrentSessionState(torrent_session));
            app.manage(TorrentPollerState(Mutex::new(HashMap::new())));

            // Spawn ICP startup: device registration, licence sync, pattern sync loop.
            let app_handle = app.handle();
            let config_for_startup = icp_config.clone();
            tauri::async_runtime::spawn(async move {
                icp::sync::run_startup(app_handle, config_for_startup).await;
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::db::get_setting,
            commands::db::set_setting,
            commands::download::start_download,
            commands::download::get_downloads,
            commands::download::get_download_by_id,
            commands::download::reset_stale_downloads,
            commands::download::pause_download,
            commands::download::resume_download,
            commands::download::cancel_download,
            commands::torrent::add_magnet,
            commands::torrent::list_torrent_files,
            commands::torrent::add_torrent_file,
            commands::torrent::pause_torrent,
            commands::torrent::resume_torrent,
            commands::torrent::cancel_torrent,
            commands::torrent::get_torrents,
            commands::shield::get_quarantine,
            commands::shield::restore_quarantine,
            commands::shield::delete_quarantine,
            commands::icp::submit_zero_day,
            commands::icp::get_proposals,
            commands::icp::get_reputation,
            commands::icp::vote_proposal,
            commands::pro::get_pro_status,
            commands::pro::open_upgrade_page,
            commands::pro::cancel_subscription,
            commands::pro::recheck_licence,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ReLay");
}
