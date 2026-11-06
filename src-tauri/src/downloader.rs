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

/// Tiến trình bgutil PO Token provider server (node, port 4416) — khởi động
/// khi app chạy, dọn sạch khi app thoát. Cho yt-dlp xin PO Token qua HTTP
/// để client `web` trả format tới 1080p trên tài khoản bị ép SABR.
#[derive(Default)]
pub struct PotServerState(pub Mutex<Option<Child>>);

const POT_ENDPOINT: &str = "127.0.0.1:4416";

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

/// yt-dlp cập nhật liên tục để theo kịp các thay đổi chống bot của YouTube,
/// bản bundle đi kèm app có thể đã cũ → tự cập nhật bằng --update của nó.
/// Chỉ hoạt động với bản yt-dlp đóng gói tự chạy (exe/macos); bản cài qua pip
/// sẽ trả về Err với hướng dẫn pip install -U yt-dlp.
#[tauri::command]
pub async fn update_yt_dlp(app: AppHandle) -> Result<String, String> {
    run_blocking(move || {
        let bin = resolve_binary(&app, "yt-dlp")?;
        let out = base_command(&bin)
            .arg("--update")
            .output()
            .map_err(|e| e.to_string())?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
        .trim()
        .to_string();
        if out.status.success() {
            Ok(text)
        } else {
            Err(format!("Cập nhật yt-dlp thất bại: {text}"))
        }
    })
    .await
}

/// Thư mục chứa plugin PO Token provider (bgutil-ytdlp-pot-provider), nằm
/// cạnh yt-dlp: <dir yt-dlp>/plugins. Cấu trúc chuẩn: plugins/<tên>/
/// yt_dlp_plugins/<category>/<file>.py (đây là đường dẫn truyền vào
/// --plugin-dirs của yt-dlp).
fn resolve_plugins_dir(yt_dlp: &PathBuf) -> Option<PathBuf> {
    let dir = yt_dlp.parent()?.join("plugins");
    let candidate = dir.join("bgutil-ytdlp-pot-provider");
    (candidate.is_dir()).then_some(dir)
}

/// Thư mục bgutil provider server (build/main.js + node_modules) cạnh yt-dlp.
fn resolve_server_dir(app: &AppHandle, yt_dlp: &PathBuf) -> Option<PathBuf> {
    if let Some(dir) = yt_dlp.parent() {
        let candidate = dir.join("bgutil-provider");
        if candidate.join("build").join("main.js").is_file() {
            return Some(candidate);
        }
    }
    let candidate = app
        .path()
        .resolve("binaries/bgutil-provider", BaseDirectory::Resource)
        .ok()?;
    if candidate.join("build").join("main.js").is_file() {
        return Some(candidate);
    }
    None
}

fn pot_server_reachable() -> bool {
    use std::net::TcpStream;
    TcpStream::connect(POT_ENDPOINT)
        .map(|s| {
            drop(s);
            true
        })
        .unwrap_or(false)
}

/// Đảm bảo bgutil provider server đang chạy (spawn nếu rỗng, chờ port mở
/// tối đa ~8 giây). Trả về true nếu plugin có thể xin PO Token qua HTTP.
fn ensure_pot_server(app: &AppHandle, yt_dlp: &PathBuf) -> bool {
    if pot_server_reachable() {
        // Server có thể do app cũ nơi khác hoặc chạy thủ công — cứ dùng.
        return true;
    }
    if !which::which("node").is_ok() {
        return false;
    }
    let Some(server_dir) = resolve_server_dir(app, yt_dlp) else {
        return false;
    };

    let state = app.state::<PotServerState>();
    let mut guard = state.0.lock().unwrap();
    if guard.is_some() {
        // Đã spawn app-launch nhưng chưa kịp nghe port -> chờ thêm đã.
        return wait_pot_ready_guarded(&guard);
    }
    let node_bin = PathBuf::from("node");
    let mut cmd = base_command(&node_bin);
    cmd.current_dir(&server_dir);
    cmd.arg("build").arg("main.js");
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    match cmd.spawn() {
        Ok(child) => {
            *guard = Some(child);
            wait_pot_ready_guarded(&guard)
        }
        Err(_) => false,
    }
}

fn wait_pot_ready_guarded(_child: &Option<Child>) -> bool {
    for _ in 0..40 {
        if pot_server_reachable() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    false
}

/// Kill server PO Token khi app thoát.
pub fn kill_pot_server(state: &PotServerState) {
    if let Some(mut child) = state.0.lock().unwrap().take() {
        kill_process_tree(&mut child);
    }
}

/// Video giới hạn tuổi / tài khoản bị YouTube ép SABR (bỏ stream URL truyền
/// thống): các client web trả về có mỗi storyboard, không tải được. Nghiệm
/// pháp đã kiểm chứng (yt-dlp SABR fork + cookies):
/// - Có PO Token provider: client tv,web + formats=duplicate → TV fallback
///   chắc chắn có (360-720p), web mở khoá SABR tới 1080p+ qua PO Token.
/// - Không có: ép player_client=tv để lấy format trực tiếp (tới ~720p).
/// --js-runtimes node để giải n-challenge (node được dò theo PATH; yt-dlp
/// mặc định chỉ tìm deno).
/// Một số video client `tv` trả về response player cũ của YouTube
/// ("The page needs to be reloaded") — khi đó bỏ tv và thử lại chỉ với web.
fn add_youtube_fixes(
    cmd: &mut Command,
    has_cookies: bool,
    plugins_dir: Option<&PathBuf>,
    pot_enabled: bool,
    client_override: Option<&str>,
) {
    if which::which("node").is_ok() {
        cmd.args(["--js-runtimes", "node"]);
    }
    if let Some(dir) = plugins_dir {
        cmd.arg("--plugin-dirs");
        cmd.arg(dir);
    }
    if pot_enabled {
        let clients = client_override.unwrap_or("tv,web");
        cmd.args([
            "--extractor-args",
            &format!("youtube:player_client={clients};formats=duplicate"),
        ]);
    } else if has_cookies {
        let clients = client_override.unwrap_or("tv");
        cmd.args(["--extractor-args", &format!("youtube:player_client={clients}")]);
    }
}

/// YouTube/TikTok chặn request không có đăng nhập ("Sign in to confirm you're
/// not a bot") — yt-dlp khắc phục bằng cách mượn cookies của trình duyệt.
/// Ưu tiên file cookies.txt (--cookies) nếu được cung cấp: Chrome/Edge 127+
/// mã hoá cookies kiểu App-Bound khiến --cookies-from-browser thất bại vì
/// DPAPI (và DB của trình duyệt đang mở cũng bị khoá), file là cách chắc chắn.
/// Chỉ cho phép một trong hai phía trên — yt-dlp không nhận cả hai cùng lúc.
fn add_cookies_args(
    cmd: &mut Command,
    cookies_from_browser: Option<&str>,
    cookies_file: Option<&str>,
) -> bool {
    let file = cookies_file.map(str::trim).filter(|f| !f.is_empty());
    let browser = cookies_from_browser
        .map(str::trim)
        .filter(|b| !b.is_empty());

    let has_cookies = file.is_some() || browser.is_some();
    if let Some(f) = file {
        cmd.args(["--cookies", f]);
    } else if let Some(b) = browser {
        cmd.args(["--cookies-from-browser", b]);
    }
    has_cookies
}

fn fetch_metadata(
    bin: &PathBuf,
    url: &str,
    cookies_from_browser: Option<&str>,
    cookies_file: Option<&str>,
    plugins_dir: Option<PathBuf>,
    pot_enabled: bool,
) -> Result<serde_json::Value, String> {
    let duplicate_fixes =
        |cmd: &mut Command, client_override: Option<&str>| -> Result<serde_json::Value, String> {
            cmd.args(["-j", "--no-playlist", "--no-warnings"]);
            let has_cookies = add_cookies_args(cmd, cookies_from_browser, cookies_file);
            add_youtube_fixes(cmd, has_cookies, plugins_dir.as_ref(), pot_enabled, client_override);
            cmd.arg(url);
            let out = cmd.output().map_err(|e| e.to_string())?;

            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr);
                return Err(format!("yt-dlp lỗi: {err}"));
            }

            serde_json::from_slice(&out.stdout).map_err(|e| format!("Parse JSON lỗi: {e}"))
        };

    let mut first = base_command(bin);
    match duplicate_fixes(&mut first, None) {
        Ok(json) => Ok(json),
        Err(e) => {
            // Client tv trả stale response → thử lại chỉ với web (cần PO Token).
            if e.contains("The page needs to be reloaded") && pot_enabled {
                let mut retry = base_command(bin);
                duplicate_fixes(&mut retry, Some("web"))
            } else {
                Err(e)
            }
        }
    }
}

/// Dò nhanh xem URL có phải playlist không, dùng --flat-playlist để không tải
/// metadata từng video (video trong playlist sẽ được truy cập riêng lúc tải).
/// Với URL thuần playlist, lệnh -j thường sẽ quét từng video (chậm, dễ chết
/// theo mạng) — nên JSON của lệnh này cũng được dùng làm metadata thay thế
/// khi metadata single-video thất bại.
fn detect_playlist(
    bin: &PathBuf,
    url: &str,
    cookies_from_browser: Option<&str>,
    cookies_file: Option<&str>,
    plugins_dir: Option<PathBuf>,
    pot_enabled: bool,
) -> Option<serde_json::Value> {
    let mut cmd = base_command(bin);
    cmd.args(["-J", "--flat-playlist", "--no-warnings"]);
    let has_cookies = add_cookies_args(&mut cmd, cookies_from_browser, cookies_file);
    add_youtube_fixes(&mut cmd, has_cookies, plugins_dir.as_ref(), pot_enabled, None);
    cmd.arg(url);
    let out = cmd.output();

    let Ok(out) = out else {
        return None;
    };
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice::<serde_json::Value>(&out.stdout).ok()
}

#[tauri::command]
pub async fn fetch_info(
    app: AppHandle,
    url: String,
    cookies_from_browser: Option<String>, // tên trình duyệt để mượn cookies, rỗng/None = không dùng
    cookies_file: Option<String>, // đường dẫn file cookies.txt (định dạng Netscape) để xuất bypass DPAPI
) -> Result<VideoInfo, String> {
    let app2 = app.clone();
    let bin = run_blocking(move || resolve_binary(&app, "yt-dlp")).await?;
    // Đảm bảo bgutil PO Token provider server đang chạy (spawn 1 lần) rồi
    // nhìn thư mục plugin để truyền --plugin-dirs cho yt-dlp.
    let plugins_bin = bin.clone();
    let _ = run_blocking(move || Ok(ensure_pot_server(&app2, &plugins_bin))).await;
    let plugins_dir = resolve_plugins_dir(&bin);
    let pot_enabled = plugins_dir.is_some() && pot_server_reachable();

    // Hai lệnh yt-dlp (metadata single-video + dò playlist) chạy song song
    // trên 2 thread riêng thay vì tuần tự, để giảm ~50% thời gian chờ mạng.
    // spawn_blocking() bắt đầu chạy ngay lập tức trên threadpool, nên gọi cả
    // hai trước rồi mới await tuần tự vẫn cho concurrency thật sự.
    let bin1 = bin.clone();
    let url1 = url.clone();
    let cookies1 = cookies_from_browser.clone();
    let file1 = cookies_file.clone();
    let plugins1 = plugins_dir.clone();
    let metadata_handle = tauri::async_runtime::spawn_blocking(move || {
        fetch_metadata(
            &bin1,
            &url1,
            cookies1.as_deref(),
            file1.as_deref(),
            plugins1,
            pot_enabled,
        )
    });

    let bin2 = bin.clone();
    let cookies2 = cookies_from_browser.clone();
    let file2 = cookies_file.clone();
    let playlist_handle = tauri::async_runtime::spawn_blocking(move || {
        detect_playlist(
            &bin2,
            &url,
            cookies2.as_deref(),
            file2.as_deref(),
            plugins_dir,
            pot_enabled,
        )
    });

    let meta_result: Result<serde_json::Value, String> = match metadata_handle.await {
        Ok(r) => r,
        Err(e) => Err(format!("Lỗi nội bộ (task panic): {e}")),
    };
    let playlist_json = playlist_handle.await.unwrap_or(None);

    let (is_playlist, playlist_count) = playlist_json
        .as_ref()
        .and_then(|j| j.get("entries"))
        .and_then(|e| e.as_array())
        .map(|arr| (arr.len() > 0, Some(arr.len() as i64)))
        .unwrap_or((false, None));

    // Fallback riêng biệt: nếu URL là playlist thuần, -j metadata sẽ trả lỗi
    // quét từng video; dùng JSON của -J --flat-playlist thay thế (title của
    // playlist, không có format/conver/ duration của từng video bên trong).
    let json = match meta_result {
        Ok(j) => {
            if j.get("_type").and_then(|t| t.as_str()) == Some("playlist") {
                playlist_json.clone().unwrap_or(j)
            } else {
                j
            }
        }
        Err(e) => {
            if let Some(pj) = playlist_json.clone() {
                if is_playlist {
                    pj
                } else {
                    return Err(e);
                }
            } else {
                return Err(e);
            }
        }
    };

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
    cookies_from_browser: Option<String>, // tên trình duyệt để mượn cookies (chrome/edge/firefox/...)
    cookies_file: Option<String>, // đường dẫn file cookies.txt, sạch hơn và bypass lỗi DPAPI
    playlist_items: Option<String>, // dải item playlist theo cú pháp yt-dlp: "1-5,8,12-"
    write_subs: bool,      // tải subtitle (vi/en + auto caption) và --convert-subs srt
    subs_embed: bool,      // --embed-subs ghép phụ đề vào file video
    embed_metadata: bool,  // --embed-thumbnail + --embed-metadata + --embed-chapters
    use_archive: bool,     // --download-archive: bỏ qua item đã từng tải xong
) -> Result<(), String> {
    run_blocking(move || {
        run_download_blocking(
            app,
            id,
            url,
            out_dir,
            quality,
            is_playlist,
            filename_template,
            cookies_from_browser,
            cookies_file,
            playlist_items,
            write_subs,
            subs_embed,
            embed_metadata,
            use_archive,
        )
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
    cookies_from_browser: Option<String>,
    cookies_file: Option<String>,
    playlist_items: Option<String>,
    write_subs: bool,
    subs_embed: bool,
    embed_metadata: bool,
    use_archive: bool,
) -> Result<(), String> {
    let bin = resolve_binary(&app, "yt-dlp")?;
    ensure_pot_server(&app, &bin);
    let plugins_dir = resolve_plugins_dir(&bin);
    let pot_enabled = plugins_dir.is_some() && pot_server_reachable();
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

    // Lần 1: cấu hình chuẩn (client tv,web khi có PO Token). Nếu client tv
    // trả về "The page needs to be reloaded" (response player cũ của YouTube),
    // lần 2 thử lại chỉ với client web — video dạng đó thường tải được qua web.
    let attempts: [Option<&str>; 2] = [None, Some("web")];
    for client_override in attempts {
        let mut cmd = base_command(&bin);
        cmd.args(["--newline", "--no-warnings", "-o", &out_template]);
        let has_cookies = add_cookies_args(&mut cmd, cookies_from_browser.as_deref(), cookies_file.as_deref());
        add_youtube_fixes(&mut cmd, has_cookies, plugins_dir.as_ref(), pot_enabled, client_override);

        if let Some(ffmpeg) = &ffmpeg_path {
            // Chỉ cần khi ffmpeg không có sẵn trong PATH của máy người dùng —
            // trỏ thẳng tới bản đã bundle cùng app để mux mp4 hoạt động độc lập.
            cmd.arg("--ffmpeg-location");
            cmd.arg(ffmpeg);
        }

        if is_playlist {
            cmd.arg("--yes-playlist");
            // Dải video cụ thể muốn tải (nếu người dùng nhập) — cú pháp yt-dlp:
            // "1" / "1-5" / "12-" / "1-5,8,12-" — để trống là tải toàn bộ.
            if let Some(items) = playlist_items
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                cmd.args(["--playlist-items", &items]);
            }
        } else {
            cmd.arg("--no-playlist");
        }

        if write_subs {
            // Phụ đề thủ công lẫn auto-caption, ưu tiên tiếng Việt rồi tiếng Anh,
            // convert sang srt để dễ embed/phát lại.
            cmd.args([
                "--write-subs",
                "--write-auto-subs",
                "--sub-langs",
                "vi.*,vi,en.*,en",
                "--convert-subs",
                "srt",
            ]);
        }
        if subs_embed {
            cmd.arg("--embed-subs");
        }
        if embed_metadata {
            cmd.args(["--embed-metadata", "--embed-chapters", "--embed-thumbnail"]);
        }
        if use_archive {
            // File archive chỉ mang tính cục bộ trong thư mục hiện tại — yt-dlp
            // dùng nó để nhận diện như video đã tải xong rồi bỏ qua khi lặp lại.
            let archive = PathBuf::from(&out_dir).join("ytflash-archive.txt");
            cmd.arg("--download-archive");
            cmd.arg(&archive);
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
            return Ok(());
        }

        // Client tv trả stale response → thử lại 1 lần chỉ với client web.
        let should_retry = client_override.is_none()
            && pot_enabled
            && err_buf.contains("The page needs to be reloaded");
        if should_retry {
            let _ = app.emit(
                "download-progress",
                ProgressEvent {
                    id: id.clone(),
                    percent: 0.0,
                    speed: None,
                    eta: None,
                    stage: "downloading".into(),
                    message: Some("YouTube trả lỗi ở lần đầu — tự động thử lại bằng cấu hình khác…".into()),
                },
            );
            continue;
        }

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
        return Err(format!("yt-dlp thất bại: {err_buf}"));
    }
    unreachable!("danh sách attempt không được rỗng")
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
