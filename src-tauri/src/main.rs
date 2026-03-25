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
use std::sync::Mutex;
use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app
                .path_resolver()
                .app_data_dir()
                .expect("could not resolve app data dir");

            let conn = db::init_db(dir).expect("failed to initialise database");
            app.manage(
                reqwest::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(10))
                    .build()
                    .expect("failed to build HTTP client"),
            );
            app.manage(DbState(Mutex::new(conn)));
            app.manage(LifecycleState(Mutex::new(HashMap::new())));
            app.manage(QueueState(Mutex::new(VecDeque::new())));

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::db::get_setting,
            commands::db::set_setting,
            commands::download::start_download,
            commands::download::get_downloads,
            commands::download::reset_stale_downloads,
            commands::download::pause_download,
            commands::download::resume_download,
            commands::download::cancel_download,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ReLay");
}
