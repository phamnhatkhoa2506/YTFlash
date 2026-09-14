mod downloader;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(downloader::DownloadRegistry::default())
        .invoke_handler(tauri::generate_handler![
            downloader::check_yt_dlp,
            downloader::fetch_info,
            downloader::download_video,
            downloader::cancel_download,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
