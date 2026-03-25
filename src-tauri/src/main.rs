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
            app.manage(DbState(Mutex::new(conn)));

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::db::get_setting,
            commands::db::set_setting,
            commands::download::start_download,
            commands::download::get_downloads,
            commands::download::reset_stale_downloads,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ReLay");
}
