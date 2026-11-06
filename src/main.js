const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open } = window.__TAURI__.dialog;

const urlInput = document.getElementById("url");
const fetchBtn = document.getElementById("fetch-btn");
const addQueueBtn = document.getElementById("add-queue-btn");
const downloadBtn = document.getElementById("download-btn");
const chooseDirBtn = document.getElementById("choose-dir-btn");
const outDirLabel = document.getElementById("out-dir");
const chooseCookiesBtn = document.getElementById("choose-cookies-btn");
const cookieFileLabel = document.getElementById("cookies-file");
const qualitySelect = document.getElementById("quality-select");
const browserSelect = document.getElementById("browser-select");
const filenameTemplateInput = document.getElementById("filename-template");
const playlistItemsInput = document.getElementById("playlist-items");
const optSubs = document.getElementById("opt-subs");
const optSubsEmbed = document.getElementById("opt-subs-embed");
const optEmbed = document.getElementById("opt-embed");
const optArchive = document.getElementById("opt-archive");

const infoCard = document.getElementById("info-card");
const infoThumb = document.getElementById("info-thumb");
const infoTitle = document.getElementById("info-title");
const infoMeta = document.getElementById("info-meta");
const infoPlaylist = document.getElementById("info-playlist");
const playlistCountEl = document.getElementById("playlist-count");
const playlistToggle = document.getElementById("playlist-toggle");

const startQueueBtn = document.getElementById("start-queue-btn");
const queueListEl = document.getElementById("queue-list");
const queueCountEl = document.getElementById("queue-count");
const queueEmptyEl = document.getElementById("queue-empty");
const statusEl = document.getElementById("status");
const updateDlpBtn = document.getElementById("update-dlp-btn");

let outDir = null;
let cookieFile = null;
let currentInfo = null; // { url, title, thumbnail, is_playlist, playlist_count }
let queue = []; // { id, url, title, outDir, quality, isPlaylist, filenameTemplate, status, percent, speed, eta, message }
let isProcessingQueue = false;

function setStatus(msg, isError = false) {
  statusEl.textContent = msg;
  statusEl.title = msg;
  statusEl.classList.toggle("is-error", isError);
  statusEl.parentElement?.classList.toggle("is-error", isError);
}

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

// YouTube báo "Sign in to confirm your age / you're not a bot" khi request
// không có cookies — hướng dẫn người dùng chọn trình duyệt ở ô "Xác thực".
function applyErrorHint(msg) {
  let hint =
    "Lưu ý: nếu là video YouTube giới hạn tuổi, chọn trình duyệt ở ô 'Xác thực (cookies)' rồi Kiểm tra lại.";
  if (/failed to decrypt|dpapi|could not copy/i.test(msg)) {
    hint =
      "Trình duyệt mã hoá cookies kiểu mới (Chrome/Edge 127+), yt-dlp không đọc nổi. Cách chắc chắn: bên cạnh ô Cookies.txt, cài extension 'Get cookies.txt' → xuất file cookies.txt cho youtube.com → bấm nút Cookies.txt… để chọn file đó.";
  } else if (/sign in to confirm|not a bot|age|cookies/i.test(msg)) {
    hint =
      "Lưu ý: chọn trình duyệt bạn đã đăng nhập ở ô 'Xác thực (cookies từ trình duyệt)' rồi Kiểm tra lại. Nếu vẫn lỗi (Chrome/Edge mã hoá cookies DPAPI), hãy dùng file cookies.txt qua nút Cookies.txt…";
  }
  setStatus(`${msg} — ${hint}`, true);
}

function resetQualityOptions() {
  qualitySelect.innerHTML = `
    <option value="best">Tốt nhất (mp4)</option>
    <option value="audio">Chỉ âm thanh (mp3)</option>
  `;
}

function populateQualityOptions(formats) {
  resetQualityOptions();
  const bestOpt = qualitySelect.querySelector('option[value="best"]');
  for (const f of formats) {
    const opt = document.createElement("option");
    opt.value = String(f.height);
    opt.textContent = f.label;
    bestOpt.after(opt);
  }
}

function updateActionButtons() {
  const hasInfo = !!currentInfo;
  addQueueBtn.disabled = !hasInfo;
  downloadBtn.disabled = !(hasInfo && outDir);
}

// Phụ đề chỉ có ý nghĩa khi tải file video (mp3 thì chỉ tải .srt riêng).
function syncOptState() {
  const isAudio = qualitySelect.value === "audio";
  optSubsEmbed.disabled = isAudio || !optSubs.checked;
  if (optSubsEmbed.disabled) optSubsEmbed.checked = false;
}

qualitySelect.addEventListener("change", syncOptState);
optSubs.addEventListener("change", syncOptState);

urlInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") fetchBtn.click();
});

fetchBtn.addEventListener("click", async () => {
  const url = urlInput.value.trim();
  if (!url) return;
  setStatus("Đang lấy thông tin video...");
  currentInfo = null;
  infoCard.classList.add("hidden");
  updateActionButtons();
  try {
    const info = await invoke("fetch_info", {
      url,
      cookiesFromBrowser: browserSelect.value || null,
      cookiesFile: cookieFile,
    });
    currentInfo = { url, ...info };

    infoTitle.textContent = info.title;
    const parts = [];
    if (info.uploader) parts.push(info.uploader);
    if (info.duration) parts.push(`${Math.round(info.duration)}s`);
    if (info.extractor) parts.push(info.extractor);
    infoMeta.textContent = parts.join(" · ");
    infoThumb.src = info.thumbnail || "";

    populateQualityOptions(info.formats || []);

    if (info.is_playlist) {
      infoPlaylist.classList.remove("hidden");
      playlistCountEl.textContent = info.playlist_count ?? "?";
      playlistToggle.checked = false;
    } else {
      infoPlaylist.classList.add("hidden");
    }

    infoCard.classList.remove("hidden");
    updateActionButtons();
    let readyMsg = "Sẵn sàng tải.";
    const auth = [];
    if (cookieFile) auth.push("cookies.txt");
    if (browserSelect.value) auth.push(browserSelect.value);
    if (auth.length) readyMsg = `Cookies từ ${auth.join(" + ")}. ${readyMsg}`;
    setStatus(readyMsg);
  } catch (e) {
    applyErrorHint(String(e));
  }
});

chooseCookiesBtn.addEventListener("click", async () => {
  const file = await open({
    multiple: false,
    filters: [{ name: "Cookies.txt", extensions: ["txt", "sqlite"] }],
  });
  if (file) {
    cookieFile = file.path || file;
    cookieFileLabel.textContent = cookieFile;
  }
});

chooseDirBtn.addEventListener("click", async () => {
  const dir = await open({ directory: true, multiple: false });
  if (dir) {
    outDir = dir;
    outDirLabel.textContent = dir;
    updateActionButtons();
  }
});

function currentSettingsSnapshot() {
  return {
    url: currentInfo.url,
    title: currentInfo.title,
    outDir,
    quality: qualitySelect.value,
    isPlaylist: currentInfo.is_playlist && playlistToggle.checked,
    filenameTemplate: filenameTemplateInput.value.trim() || null,
    cookiesFromBrowser: browserSelect.value || null,
    cookiesFile: cookieFile,
    playlistItems: playlistItemsInput.value.trim() || null,
    writeSubs: optSubs.checked,
    subsEmbed: optSubs.checked && optSubsEmbed.checked,
    embedMetadata: optEmbed.checked,
    useArchive: optArchive.checked,
  };
}

function addToQueue() {
  const snap = currentSettingsSnapshot();
  const item = {
    id: crypto.randomUUID(),
    ...snap,
    status: "pending", // pending | downloading | merging | done | error | canceled
    percent: 0,
    speed: null,
    eta: null,
    message: null,
  };
  queue.push(item);
  renderQueue();
  startQueueBtn.disabled = queue.every((i) => i.status !== "pending");
  return item;
}

addQueueBtn.addEventListener("click", () => {
  if (!currentInfo) return;
  addToQueue();
  setStatus("Đã thêm vào hàng đợi.");
});

downloadBtn.addEventListener("click", () => {
  if (!currentInfo || !outDir) return;
  addToQueue();
  processQueue();
});

startQueueBtn.addEventListener("click", () => {
  processQueue();
});

function renderQueue() {
  queueListEl.innerHTML = "";
  if (queueCountEl) queueCountEl.textContent = String(queue.length);
  if (queueEmptyEl) queueEmptyEl.hidden = queue.length > 0;
  for (const item of queue) {
    const li = document.createElement("li");
    li.className = `queue-item status-${item.status}`;
    li.dataset.id = item.id;

    const badge = {
      pending: "Chờ",
      downloading: "Đang tải",
      merging: "Đang ghép",
      done: "Xong",
      error: "Lỗi",
      canceled: "Đã hủy",
    }[item.status];

    const qualityLabel =
      item.quality === "audio" ? "MP3" : item.quality === "best" ? "MP4 tốt nhất" : `${item.quality}p`;
    const meta = [
      qualityLabel,
      item.isPlaylist ? "Playlist" : null,
      item.playlistItems ? `mục ${item.playlistItems}` : null,
      item.writeSubs ? (item.subsEmbed ? "phụ đề nhúng" : "phụ đề srt") : null,
      item.embedMetadata ? "thumbnail" : null,
      item.useArchive ? "archive" : null,
      item.cookiesFile ? "cookies.txt" : null,
      item.cookiesFromBrowser || null,
    ]
      .filter(Boolean)
      .join(" · ");

    li.innerHTML = `
      <div class="queue-row-top">
        <span class="queue-title" title="${escapeHtml(item.url)}">${escapeHtml(item.title)}</span>
        <span class="queue-badge">${badge}</span>
      </div>
      <div class="queue-row-meta">${escapeHtml(meta)}</div>
      <div class="progress-bar"><div class="progress-fill" style="width:${item.percent}%"></div></div>
      <div class="queue-row-bottom">
        <span class="queue-progress-label">${queueProgressLabel(item)}</span>
        <span class="queue-actions"></span>
      </div>
    `;

    const actions = li.querySelector(".queue-actions");
    if (item.status === "pending") {
      const removeBtn = document.createElement("button");
      removeBtn.textContent = "Xóa";
      removeBtn.className = "mini-btn";
      removeBtn.addEventListener("click", () => {
        queue = queue.filter((i) => i.id !== item.id);
        renderQueue();
        startQueueBtn.disabled = queue.every((i) => i.status !== "pending");
      });
      actions.appendChild(removeBtn);
    } else if (item.status === "downloading" || item.status === "merging") {
      const cancelBtn = document.createElement("button");
      cancelBtn.textContent = "Hủy";
      cancelBtn.className = "mini-btn danger";
      cancelBtn.addEventListener("click", async () => {
        try {
          await invoke("cancel_download", { id: item.id });
        } catch (e) {
          setStatus(String(e), true);
        }
      });
      actions.appendChild(cancelBtn);
    }

    queueListEl.appendChild(li);
  }
}

function queueProgressLabel(item) {
  if (item.status === "downloading") {
    return `${item.percent.toFixed(1)}%${item.speed ? " · " + item.speed : ""}${item.eta ? " · ETA " + item.eta : ""}`;
  }
  if (item.status === "merging") return "Đang ghép audio/video...";
  if (item.status === "error") return item.message || "Có lỗi xảy ra";
  if (item.status === "done") return "Hoàn tất";
  if (item.status === "canceled") return "Đã hủy bởi người dùng";
  return "Đang chờ trong hàng đợi...";
}

function updateQueueItemDom(item) {
  const li = queueListEl.querySelector(`li[data-id="${item.id}"]`);
  if (!li) return;
  li.className = `queue-item status-${item.status}`;
  const fill = li.querySelector(".progress-fill");
  if (fill) fill.style.width = `${item.percent}%`;
  const label = li.querySelector(".queue-progress-label");
  if (label) label.textContent = queueProgressLabel(item);
}

async function processQueue() {
  if (isProcessingQueue) return;
  isProcessingQueue = true;
  startQueueBtn.disabled = true;

  while (true) {
    const item = queue.find((i) => i.status === "pending");
    if (!item) break;

    item.status = "downloading";
    renderQueue();
    setStatus(`Đang tải: ${item.title}`);

    try {
      await invoke("download_video", {
        id: item.id,
        url: item.url,
        outDir: item.outDir,
        quality: item.quality,
        isPlaylist: item.isPlaylist,
        filenameTemplate: item.filenameTemplate,
        cookiesFromBrowser: item.cookiesFromBrowser,
        cookiesFile: item.cookiesFile,
        playlistItems: item.playlistItems,
        writeSubs: item.writeSubs,
        subsEmbed: item.subsEmbed,
        embedMetadata: item.embedMetadata,
        useArchive: item.useArchive,
      });
      item.status = "done";
      item.percent = 100;
    } catch (e) {
      const msg = String(e);
      item.status = msg.includes("Đã hủy") ? "canceled" : "error";
      item.message = msg;
      applyErrorHint(msg);
    }
    renderQueue();
  }

  isProcessingQueue = false;
  startQueueBtn.disabled = queue.every((i) => i.status !== "pending");
  setStatus("Hàng đợi đã xử lý xong.");
}

listen("download-progress", (event) => {
  const p = event.payload;
  const item = queue.find((i) => i.id === p.id);
  if (!item) return;

  item.percent = p.percent;
  item.speed = p.speed;
  item.eta = p.eta;
  if (p.stage === "downloading") item.status = "downloading";
  else if (p.stage === "merging") item.status = "merging";
  // "done" / "error" / "canceled" được set trực tiếp trong processQueue() sau khi invoke resolve/reject,
  // tránh race giữa event cuối cùng và kết quả trả về của invoke.

  updateQueueItemDom(item);
});

updateDlpBtn.addEventListener("click", async () => {
  updateDlpBtn.disabled = true;
  setStatus("Đang cập nhật yt-dlp...");
  try {
    const result = await invoke("update_yt_dlp");
    if (/already up-to-date|updated/i.test(result)) {
      setStatus(result);
    } else {
      setStatus(result);
    }
  } catch (e) {
    setStatus(String(e), true);
  } finally {
    invoke("check_yt_dlp")
      .then((v) => setStatus(`yt-dlp ${v} sẵn sàng.`))
      .catch(() => {});
    updateDlpBtn.disabled = false;
  }
});

// Kiểm tra yt-dlp có sẵn không khi khởi động
invoke("check_yt_dlp")
  .then((v) => setStatus(`yt-dlp ${v} sẵn sàng.`))
  .catch(() => setStatus("Chưa tìm thấy yt-dlp. Hãy cài đặt hoặc đặt yt-dlp.exe cạnh YTFlash.exe.", true));
