// Lõi tải video: bọc quanh binary yt-dlp (và ffmpeg khi cần mux) qua subprocess.
// Không tự implement lại extractor — dựa vào yt-dlp vì các nền tảng
// (YouTube/TikTok/Instagram) đổi cấu trúc chống bot liên tục.
//
// Mọi lệnh gọi subprocess (blocking I/O) đều được chạy trong
// tauri::async_runtime::spawn_blocking để không chiếm luồng xử lý IPC/event
// loop của Tauri — nếu không UI sẽ báo "Not Responding" khi mạng chậm.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use tauri::{path::BaseDirectory, AppHandle, Emitter, Manager};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Theo dõi các tiến trình yt-dlp đang chạy, khoá theo id tải, để có thể hủy.
#[derive(Default)]
pub struct DownloadRegistry(pub Mutex<HashMap<String, Child>>);

/// Tìm binary theo thứ tự ưu tiên:
/// 1. Resource dir của bản build đã đóng gói (binaries/<name>.exe, khai báo trong tauri.conf.json)
/// 2. Cạnh exe của app (tiện cho dev: đặt thủ công vào target/debug)
/// 3. PATH hệ thống
fn resolve_binary(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    let exe_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };

    if let Ok(path) = app
        .path()
        .resolve(format!("binaries/{exe_name}"), BaseDirectory::Resource)
    {
        if path.is_file() {
            return Ok(path);
        }
    }

    if let Ok(exe_dir) = std::env::current_exe() {
        if let Some(dir) = exe_dir.parent() {
            let candidate = dir.join(&exe_name);
            if candidate.is_file() {
                return Ok(candidate);
            }
            let candidate = dir.join("binaries").join(&exe_name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    which::which(&exe_name)
        .or_else(|_| which::which(name))
        .map_err(|_| format!("Không tìm thấy '{name}'. Hãy cài đặt hoặc đặt binary cạnh YTFlash.exe"))
}

fn base_command(bin: &PathBuf) -> Command {
    let mut cmd = Command::new(bin);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FormatOption {
    pub height: i64,
    pub label: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VideoInfo {
    pub title: String,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
    pub uploader: Option<String>,
    pub extractor: Option<String>,
    pub is_playlist: bool,
    pub playlist_count: Option<i64>,
    pub formats: Vec<FormatOption>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ProgressEvent {
    pub id: String,
    pub percent: f32,
    pub speed: Option<String>,
    pub eta: Option<String>,
    pub stage: String, // "downloading" | "merging" | "done" | "error" | "canceled"
    pub message: Option<String>,
}

/// Chạy một closure blocking trên threadpool riêng, không chặn tokio runtime của Tauri.
async fn run_blocking<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("Lỗi nội bộ (task panic): {e}"))?
}

#[tauri::command]
pub async fn check_yt_dlp(app: AppHandle) -> Result<String, String> {
    run_blocking(move || {
        let bin = resolve_binary(&app, "yt-dlp")?;
        let out = base_command(&bin)
            .arg("--version")
            .output()
            .map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    })
    .await
}

fn fetch_metadata(bin: &PathBuf, url: &str) -> Result<serde_json::Value, String> {
    let out = base_command(bin)
        .args(["-j", "--no-playlist", "--no-warnings", url])
        .output()
        .map_err(|e| e.to_string())?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("yt-dlp lỗi: {err}"));
    }

    serde_json::from_slice(&out.stdout).map_err(|e| format!("Parse JSON lỗi: {e}"))
}

/// Dò nhanh xem URL có phải playlist không, dùng --flat-playlist để không tải metadata từng video.
fn detect_playlist(bin: &PathBuf, url: &str) -> (bool, Option<i64>) {
    let out = base_command(bin)
        .args(["-J", "--flat-playlist", "--no-warnings", url])
        .output();

    let Ok(out) = out else {
        return (false, None);
    };
    if !out.status.success() {
        return (false, None);
    }
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return (false, None);
    };

    match json.get("entries").and_then(|e| e.as_array()) {
        Some(entries) if entries.len() > 1 => (true, Some(entries.len() as i64)),
        _ => (false, None),
    }
}

#[tauri::command]
pub async fn fetch_info(app: AppHandle, url: String) -> Result<VideoInfo, String> {
    let bin = run_blocking(move || resolve_binary(&app, "yt-dlp")).await?;

    // Hai lệnh yt-dlp (metadata single-video + dò playlist) chạy song song
    // trên 2 thread riêng thay vì tuần tự, để giảm ~50% thời gian chờ mạng.
    // spawn_blocking() bắt đầu chạy ngay lập tức trên threadpool, nên gọi cả
    // hai trước rồi mới await tuần tự vẫn cho concurrency thật sự.
    let bin1 = bin.clone();
    let url1 = url.clone();
    let metadata_handle =
        tauri::async_runtime::spawn_blocking(move || fetch_metadata(&bin1, &url1));

    let bin2 = bin.clone();
    let url2 = url.clone();
    let playlist_handle =
        tauri::async_runtime::spawn_blocking(move || detect_playlist(&bin2, &url2));

    let json = metadata_handle
        .await
        .map_err(|e| format!("Lỗi nội bộ (task panic): {e}"))??;
    let (is_playlist, playlist_count) = playlist_handle
        .await
        .unwrap_or((false, None));

    // Rút danh sách độ phân giải khả dụng (duy nhất, giảm dần) để hiện dropdown chất lượng.
    let mut heights: Vec<i64> = json
        .get("formats")
        .and_then(|f| f.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f| f.get("height").and_then(|h| h.as_i64()))
                .collect()
        })
        .unwrap_or_default();
    heights.sort_unstable_by(|a, b| b.cmp(a));
    heights.dedup();
    let formats: Vec<FormatOption> = heights
        .into_iter()
        .map(|h| FormatOption {
            height: h,
            label: format!("{h}p"),
        })
        .collect();

    Ok(VideoInfo {
        title: json
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string(),
        duration: json.get("duration").and_then(|v| v.as_f64()),
        thumbnail: json
            .get("thumbnail")
            .and_then(|v| v.as_str())
            .map(String::from),
        uploader: json
            .get("uploader")
            .and_then(|v| v.as_str())
            .map(String::from),
        extractor: json
            .get("extractor")
            .and_then(|v| v.as_str())
            .map(String::from),
        is_playlist,
        playlist_count,
        formats,
    })
}

fn format_selector(quality: &str) -> Option<Vec<String>> {
    match quality {
        "audio" => None, // xử lý riêng bằng -x
        "best" => Some(vec![
            "-f".into(),
            "bv*[ext=mp4]+ba[ext=m4a]/b[ext=mp4]/b".into(),
        ]),
        h => {
            // h là chuỗi số chiều cao, vd "1080", "720"
            Some(vec![
                "-f".into(),
                format!("bv*[height<={h}][ext=mp4]+ba[ext=m4a]/b[height<={h}][ext=mp4]/b[height<={h}]"),
            ])
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn download_video(
    app: AppHandle,
    id: String,
    url: String,
    out_dir: String,
    quality: String,       // "best" | "audio" | "1080" | "720" | "480" | "360" ...
    is_playlist: bool,     // true nếu người dùng muốn tải cả playlist
    filename_template: Option<String>, // cú pháp output template của yt-dlp, không kèm thư mục
) -> Result<(), String> {
    run_blocking(move || {
        run_download_blocking(app, id, url, out_dir, quality, is_playlist, filename_template)
    })
    .await
}

#[allow(clippy::too_many_arguments)]
fn run_download_blocking(
    app: AppHandle,
    id: String,
    url: String,
    out_dir: String,
    quality: String,
    is_playlist: bool,
    filename_template: Option<String>,
) -> Result<(), String> {
    let bin = resolve_binary(&app, "yt-dlp")?;
    let ffmpeg_path = resolve_binary(&app, "ffmpeg").ok();
    let registry = app.state::<DownloadRegistry>();

    let template = filename_template
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| {
            if is_playlist {
                "%(playlist_index)03d - %(title).150B [%(id)s].%(ext)s".to_string()
            } else {
                "%(title).150B [%(id)s].%(ext)s".to_string()
            }
        });
    let out_template = format!("{out_dir}/{template}");

    let mut cmd = base_command(&bin);
    cmd.args(["--newline", "--no-warnings", "-o", &out_template]);

    if let Some(ffmpeg) = &ffmpeg_path {
        // Chỉ cần khi ffmpeg không có sẵn trong PATH của máy người dùng —
        // trỏ thẳng tới bản đã bundle cùng app để mux mp4 hoạt động độc lập.
        cmd.arg("--ffmpeg-location");
        cmd.arg(ffmpeg);
    }

    if is_playlist {
        cmd.arg("--yes-playlist");
    } else {
        cmd.arg("--no-playlist");
    }

    if quality == "audio" {
        cmd.args(["-x", "--audio-format", "mp3"]);
    } else if let Some(args) = format_selector(&quality) {
        cmd.args(args);
    }

    cmd.arg(&url);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("Không khởi chạy được yt-dlp: {e}"))?;

    let stdout = child.stdout.take().ok_or("Không lấy được stdout")?;
    let stderr = child.stderr.take().ok_or("Không lấy được stderr")?;

    // Lưu child vào registry để lệnh cancel_download có thể kill sau này.
    registry.0.lock().unwrap().insert(id.clone(), child);

    let progress_re =
        Regex::new(r"\[download\]\s+(\d+(?:\.\d+)?)%.*?(?:at\s+(\S+))?.*?(?:ETA\s+(\S+))?")
            .unwrap();

    let app_out = app.clone();
    let id_out = id.clone();
    let out_handle = std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(caps) = progress_re.captures(&line) {
                let percent: f32 = caps
                    .get(1)
                    .and_then(|m| m.as_str().parse().ok())
                    .unwrap_or(0.0);
                let speed = caps.get(2).map(|m| m.as_str().to_string());
                let eta = caps.get(3).map(|m| m.as_str().to_string());
                let _ = app_out.emit(
                    "download-progress",
                    ProgressEvent {
                        id: id_out.clone(),
                        percent,
                        speed,
                        eta,
                        stage: "downloading".into(),
                        message: None,
                    },
                );
            } else if line.contains("Merging formats") || line.contains("[Merger]") {
                let _ = app_out.emit(
                    "download-progress",
                    ProgressEvent {
                        id: id_out.clone(),
                        percent: 99.0,
                        speed: None,
                        eta: None,
                        stage: "merging".into(),
                        message: None,
                    },
                );
            }
        }
    });

    let mut err_buf = String::new();
    {
        let mut reader = BufReader::new(stderr);
        use std::io::Read;
        let _ = reader.read_to_string(&mut err_buf);
    }

    // Chờ tiến trình kết thúc (thành công, lỗi, hoặc bị kill do cancel).
    let status = {
        let mut guard = registry.0.lock().unwrap();
        match guard.get_mut(&id) {
            Some(child) => child.wait(),
            None => {
                // Đã bị cancel_download loại khỏi registry trước khi wait — coi như đã hủy.
                drop(guard);
                let _ = out_handle.join();
                let _ = app.emit(
                    "download-progress",
                    ProgressEvent {
                        id: id.clone(),
                        percent: 0.0,
                        speed: None,
                        eta: None,
                        stage: "canceled".into(),
                        message: None,
                    },
                );
                return Err("Đã hủy tải".to_string());
            }
        }
    };
    registry.0.lock().unwrap().remove(&id);
    let status = status.map_err(|e| e.to_string())?;
    let _ = out_handle.join();

    if status.success() {
        let _ = app.emit(
            "download-progress",
            ProgressEvent {
                id,
                percent: 100.0,
                speed: None,
                eta: None,
                stage: "done".into(),
                message: None,
            },
        );
        Ok(())
    } else {
        let _ = app.emit(
            "download-progress",
            ProgressEvent {
                id: id.clone(),
                percent: 0.0,
                speed: None,
                eta: None,
                stage: "error".into(),
                message: Some(err_buf.clone()),
            },
        );
        Err(format!("yt-dlp thất bại: {err_buf}"))
    }
}

#[tauri::command]
pub fn cancel_download(app: AppHandle, id: String) -> Result<(), String> {
    let registry = app.state::<DownloadRegistry>();
    let mut guard = registry.0.lock().unwrap();
    if let Some(mut child) = guard.remove(&id) {
        kill_process_tree(&mut child);
        Ok(())
    } else {
        Err("Không tìm thấy tiến trình đang tải với id này (có thể đã xong).".to_string())
    }
}

/// Kill cả cây tiến trình: yt-dlp có thể đã spawn ffmpeg con, chỉ kill() cha
/// (mặc định của std::process::Child) sẽ để ffmpeg mồ côi vẫn chạy tiếp.
fn kill_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let pid = child.id();
        let _ = base_command(&PathBuf::from("taskkill"))
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}
