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
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::Manager;
use torrent::{TorrentPollerState, TorrentSessionState};

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app
                .path_resolver()
                .app_data_dir()
                .expect("could not resolve app data dir");

            let conn = db::init_db(dir).expect("failed to initialise database");

            // Read default_folder for the torrent session output directory.
            let torrent_output: PathBuf = db::get_setting(&conn, "default_folder")
                .ok()
                .flatten()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));

            // Initialise the librqbit torrent session on Tauri's Tokio runtime.
            // Session::new() is async; block_on bridges the sync setup closure.
            let torrent_session = tauri::async_runtime::block_on(
                librqbit::Session::new(torrent_output),
            )
            .expect("failed to initialise torrent session");

            app.manage(
                reqwest::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(10))
                    .build()
                    .expect("failed to build HTTP client"),
            );
            app.manage(DbState(Mutex::new(conn)));
            app.manage(LifecycleState(Mutex::new(HashMap::new())));
            app.manage(QueueState(Mutex::new(VecDeque::new())));
            app.manage(TorrentSessionState(torrent_session));
            app.manage(TorrentPollerState(Mutex::new(HashMap::new())));

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
        ])
        .run(tauri::generate_context!())
        .expect("error while running ReLay");
}
