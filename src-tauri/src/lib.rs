mod downloader;

use tauri::{Manager, RunEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(downloader::DownloadRegistry::default())
        .manage(downloader::PotServerState::default())
        .invoke_handler(tauri::generate_handler![
            downloader::check_yt_dlp,
            downloader::update_yt_dlp,
            downloader::fetch_info,
            downloader::download_video,
            downloader::cancel_download,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");
    app.run(|_app, event| {
        if let RunEvent::Exit = event {
            // Dọn tiến trình bgutil PO Token provider server (con của app).
            let state = _app.state::<downloader::PotServerState>();
            downloader::kill_pot_server(&state);
        }
    });
}
