const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open } = window.__TAURI__.dialog;

const urlInput = document.getElementById("url");
const fetchBtn = document.getElementById("fetch-btn");
const addQueueBtn = document.getElementById("add-queue-btn");
const downloadBtn = document.getElementById("download-btn");
const chooseDirBtn = document.getElementById("choose-dir-btn");
const outDirLabel = document.getElementById("out-dir");
const qualitySelect = document.getElementById("quality-select");
const filenameTemplateInput = document.getElementById("filename-template");

const infoCard = document.getElementById("info-card");
const infoThumb = document.getElementById("info-thumb");
const infoTitle = document.getElementById("info-title");
const infoMeta = document.getElementById("info-meta");
const infoPlaylist = document.getElementById("info-playlist");
const playlistCountEl = document.getElementById("playlist-count");
const playlistToggle = document.getElementById("playlist-toggle");

const startQueueBtn = document.getElementById("start-queue-btn");
const queueListEl = document.getElementById("queue-list");
const statusEl = document.getElementById("status");

let outDir = null;
let currentInfo = null; // { url, title, thumbnail, is_playlist, playlist_count }
let queue = []; // { id, url, title, outDir, quality, isPlaylist, filenameTemplate, status, percent, speed, eta, message }
let isProcessingQueue = false;

function setStatus(msg, isError = false) {
  statusEl.textContent = msg;
  statusEl.style.color = isError ? "#e5484d" : "";
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

fetchBtn.addEventListener("click", async () => {
  const url = urlInput.value.trim();
  if (!url) return;
  setStatus("Đang lấy thông tin video...");
  currentInfo = null;
  infoCard.classList.add("hidden");
  updateActionButtons();
  try {
    const info = await invoke("fetch_info", { url });
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
    setStatus("Sẵn sàng tải.");
  } catch (e) {
    setStatus(String(e), true);
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
  for (const item of queue) {
    const li = document.createElement("li");
    li.className = `queue-item status-${item.status}`;
    li.dataset.id = item.id;

    const badge = {
      pending: "⏳ Chờ",
      downloading: "⬇️ Đang tải",
      merging: "🔧 Đang ghép",
      done: "✅ Xong",
      error: "❌ Lỗi",
      canceled: "🚫 Đã hủy",
    }[item.status];

    li.innerHTML = `
      <div class="queue-row-top">
        <span class="queue-title" title="${item.url}">${item.title}</span>
        <span class="queue-badge">${badge}</span>
      </div>
      <div class="queue-row-meta">
        ${item.quality === "audio" ? "🎵 mp3" : `🎞 ${item.quality === "best" ? "tốt nhất" : item.quality + "p"}`}
        ${item.isPlaylist ? " · playlist" : ""}
      </div>
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
      });
      item.status = "done";
      item.percent = 100;
    } catch (e) {
      const msg = String(e);
      item.status = msg.includes("Đã hủy") ? "canceled" : "error";
      item.message = msg;
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

// Kiểm tra yt-dlp có sẵn không khi khởi động
invoke("check_yt_dlp")
  .then((v) => setStatus(`yt-dlp ${v} sẵn sàng.`))
  .catch(() => setStatus("Chưa tìm thấy yt-dlp. Hãy cài đặt hoặc đặt yt-dlp.exe cạnh YTFlash.exe.", true));
