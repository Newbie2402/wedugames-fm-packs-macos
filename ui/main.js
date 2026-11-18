const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);

const ICONS = {
  all: '<rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/>',
  faces: '<circle cx="12" cy="8" r="4"/><path d="M4 21c1-5 4-7 8-7s7 2 8 7"/>',
  logos: '<path d="M12 3l8 3v6c0 5-4 8-8 9-4-1-8-4-8-9V6z"/>',
  kits: '<path d="M8 3L3 6l2 5 3-1v11h8V10l3 1 2-5-5-3c-1 2-7 2-8 0z"/>',
  other: '<path d="M7 4h10v5a5 5 0 01-10 0zM7 6H4c0 3 1 5 3 5M17 6h3c0 3-1 5-3 5M12 14v4M8 21h8"/>',
};

const CATEGORIES = [
  { id: "all", label: "Semua pack" },
  { id: "faces", label: "Facepack", hint: "pilih satu" },
  { id: "logos", label: "Logo pack", hint: "pilih satu" },
  { id: "kits", label: "Kit pack", hint: "pilih satu 2D dan satu 3D" },
  { id: "other", label: "Lainnya", hint: "bisa dipasang bareng" },
];

let plans = [];
let view = "all";
let busy = false;
let gateAction = null;
let lastGame = null;
const selected = new Set();
const covers = new Map();

// Kits are split into kits-2d and kits-3d groups so one of each can sit side by side.
const categoryOf = (plan) => (plan.group?.startsWith("kits") ? "kits" : plan.group ?? "other");
const needsWork = (plan) => plan.status === "updateAvailable" || plan.status === "needsRepair";
const installed = (plan) => Boolean(plan.installedVersion);

function formatBytes(bytes) {
  if (bytes >= 1e9) return `${(bytes / 1e9).toLocaleString("id-ID", { maximumFractionDigits: 1 })} GB`;
  return `${Math.max(1, Math.ceil(bytes / 1e6)).toLocaleString("id-ID")} MB`;
}

const formatCount = (n) => n.toLocaleString("id-ID");

function showToast(text, isError = false) {
  const toast = $("toast");
  toast.textContent = text;
  toast.classList.toggle("error", isError);
  toast.hidden = false;
  clearTimeout(showToast.timer);
  showToast.timer = setTimeout(() => (toast.hidden = true), isError ? 9000 : 6000);
}

function renderNav() {
  $("nav").replaceChildren(
    ...CATEGORIES.filter((c) => c.id === "all" || plans.some((p) => categoryOf(p) === c.id)).map((category) => {
      const items = plans.filter((p) => category.id === "all" || categoryOf(p) === category.id);
      const button = document.createElement("button");
      button.type = "button";
      button.className = `nav-item${view === category.id ? " active" : ""}`;
      if (view === category.id) button.setAttribute("aria-current", "true");
      button.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true">${ICONS[category.id]}</svg>`;
      const label = Object.assign(document.createElement("span"), { className: "nav-label", textContent: category.label });
      button.append(label);
      const pending = items.filter(needsWork);
      if (pending.length) {
        const dot = document.createElement("span");
        dot.className = `pulse${pending.every((p) => p.status === "needsRepair") ? " repair" : ""}`;
        dot.title = `${pending.length} update`;
        button.append(dot);
      } else {
        button.append(Object.assign(document.createElement("span"), { className: "nav-count", textContent: items.length }));
      }
      button.addEventListener("click", () => {
        view = category.id;
        render();
      });
      return button;
    }),
  );
}

function renderHero() {
  const pending = plans.filter(needsWork);
  const repairs = plans.filter((p) => p.status === "needsRepair");
  const updates = plans.filter((p) => p.status === "updateAvailable");
  const hasWork = pending.length > 0;
  $("hero").classList.toggle("has-update", hasWork);
  $("hero").classList.toggle("has-repair", repairs.length > 0);

  const eyebrow = $("hero-eyebrow");
  if (repairs.length > 0 && updates.length > 0) {
    eyebrow.textContent = "Perlu perbaikan & update";
  } else if (repairs.length > 0) {
    eyebrow.textContent = "Perlu perbaikan";
  } else if (hasWork) {
    eyebrow.textContent = "Update tersedia";
  } else {
    eyebrow.textContent = "Status";
  }

  if (hasWork) {
    const pulseKind = `pulse${repairs.length > 0 ? " repair" : ""}`;
    eyebrow.prepend(Object.assign(document.createElement("span"), { className: pulseKind }));
  }

  if (repairs.length > 0 && updates.length > 0) {
    $("hero-title").textContent = `${repairs.length} pack perlu diperbaiki, ${updates.length} update`;
    $("update-all").textContent = "Perbaiki & Update semua";
  } else if (repairs.length > 0) {
    $("hero-title").textContent = `${repairs.length} pack perlu diperbaiki`;
    $("update-all").textContent = "Perbaiki semua";
  } else if (hasWork) {
    $("hero-title").textContent = `${updates.length} pack siap di-update`;
    $("update-all").textContent = "Update semua";
  } else {
    $("hero-title").textContent = "Semua pack sudah terbaru";
  }

  const size = pending.reduce((sum, p) => sum + p.downloadBytes, 0);
  $("hero-sub").textContent = hasWork
    ? `${pending.map((p) => p.name).join(", ")} · ${formatBytes(size)}`
    : `${plans.filter(installed).length} pack terpasang di FM 27`;

  $("update-all").hidden = !hasWork;
  $("update-all").disabled = busy;
}

function replacementFor(plan) {
  if (!plan.group || !selected.has(plan.id) || installed(plan)) return null;
  return plans.find((p) => p.id !== plan.id && p.group === plan.group && installed(p));
}

function badgeFor(plan) {
  const badge = document.createElement("span");
  const set = (kind, text) => {
    badge.className = `badge ${kind}`;
    badge.textContent = text;
  };
  if (plan.status === "updateAvailable") set("update", "Update");
  else if (plan.status === "needsRepair") set("repair", "Perlu diperbaiki");
  else if (plan.status === "incompatible") set("none", "Tidak cocok");
  else if (installed(plan)) set("installed", "Terpasang");
  else set("none", "Belum terpasang");
  if (needsWork(plan)) {
    badge.prepend(Object.assign(document.createElement("span"), {
      className: `pulse${plan.status === "needsRepair" ? " repair" : ""}`,
    }));
  }
  return badge;
}

function renderCard(plan) {
  const card = document.createElement("article");
  const isSelected = selected.has(plan.id);
  card.className = "card";
  card.classList.toggle("selected", isSelected);
  card.classList.toggle("has-update", needsWork(plan));
  card.classList.toggle("needs-repair", plan.status === "needsRepair");
  card.classList.toggle("off", plan.status === "incompatible");

  const main = document.createElement("button");
  main.type = "button";
  main.className = "card-main";
  main.disabled = busy || plan.status === "incompatible";
  main.setAttribute("aria-pressed", String(isSelected));
  main.addEventListener("click", () => toggle(plan));

  const art = document.createElement("div");
  art.className = "card-art";
  const cover = covers.get(plan.id);
  if (cover) {
    art.append(Object.assign(document.createElement("img"), { src: cover, alt: "" }));
  } else {
    art.innerHTML = packArt(plan);
    loadCover(plan);
  }
  art.append(badgeFor(plan));
  const check = document.createElement("span");
  check.className = "check";
  check.innerHTML = '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 8.5l3.5 3.5L13 4.5"/></svg>';
  art.append(check);

  const body = document.createElement("div");
  body.className = "card-body";
  body.append(Object.assign(document.createElement("div"), { className: "card-name", textContent: plan.name }));
  const version = needsWork(plan) && plan.installedVersion && plan.installedVersion !== plan.latestVersion
    ? `${plan.installedVersion} → ${plan.latestVersion}`
    : plan.latestVersion;
  const meta = `${version} · ${formatBytes(plan.totalBytes)} · ${formatCount(plan.totalFiles)} file`;
  body.append(Object.assign(document.createElement("div"), { className: "card-meta", textContent: meta }));

  const replaced = replacementFor(plan);
  if (replaced) {
    body.append(Object.assign(document.createElement("div"), { className: "card-note", textContent: `Menggantikan ${replaced.name}` }));
  } else if (plan.status === "needsRepair") {
    const note = `File perlu diperbaiki · Download ${formatBytes(plan.downloadBytes)}`;
    body.append(Object.assign(document.createElement("div"), { className: "card-note repair", textContent: note }));
  } else if (needsWork(plan)) {
    const note = `Download ${formatBytes(plan.downloadBytes)}`;
    body.append(Object.assign(document.createElement("div"), { className: "card-note update", textContent: note }));
  }

  main.append(art, body);
  card.append(main);

  if (installed(plan)) {
    const actions = document.createElement("div");
    actions.className = "card-actions";

    const repairBtn = document.createElement("button");
    repairBtn.type = "button";
    repairBtn.className = `card-repair${plan.status === "needsRepair" ? " active" : ""}`;
    repairBtn.textContent = "Perbaiki";
    repairBtn.title = `Perbaiki atau pasang ulang ${plan.name}`;
    repairBtn.disabled = busy;
    repairBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      startRepair(plan);
    });
    actions.append(repairBtn);

    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "card-remove";
    remove.textContent = "Uninstall";
    remove.title = `Hapus ${plan.name} dari game`;
    remove.disabled = busy;
    remove.addEventListener("click", (e) => {
      e.stopPropagation();
      askUninstall(plan);
    });
    actions.append(remove);

    card.append(actions);
  }
  return card;
}

function sectionTitle(category) {
  const title = document.createElement("div");
  title.className = "section-title";
  title.textContent = category.label;
  if (category.hint) title.append(Object.assign(document.createElement("span"), { textContent: category.hint }));
  return title;
}

function renderGrid() {
  const shown = view === "all" ? CATEGORIES.slice(1) : CATEGORIES.filter((c) => c.id === view);
  $("grid").replaceChildren(
    ...shown.flatMap((category) => {
      const items = plans.filter((p) => categoryOf(p) === category.id);
      return items.length ? [sectionTitle(category), ...items.map(renderCard)] : [];
    }),
  );
}

function renderDock() {
  const chosen = plans.filter((p) => selected.has(p.id));
  const run = $("run");
  run.disabled = busy || chosen.length === 0;
  if (!chosen.length) {
    $("dock-title").textContent = "Pilih pack yang mau dipasang";
    $("dock-sub").textContent = "Klik kartu pack di atas";
    run.textContent = "Pasang";
    run.classList.remove("repair");
    return;
  }
  const size = chosen.reduce((sum, p) => sum + p.downloadBytes, 0);
  $("dock-title").textContent = chosen.map((p) => p.name).join(", ");
  $("dock-sub").textContent = `${chosen.length} pack dipilih · download ${formatBytes(size)}`;

  const hasRepair = chosen.some((p) => p.status === "needsRepair");
  const hasUpdate = chosen.some((p) => p.status === "updateAvailable");
  const allInstalled = chosen.every(installed);

  const isRepair = hasRepair || (allInstalled && !hasUpdate);
  run.classList.toggle("repair", isRepair);
  if (hasRepair && !hasUpdate) {
    run.textContent = `Perbaiki ${chosen.length} pack`;
  } else if (hasRepair && hasUpdate) {
    run.textContent = `Perbaiki & Update ${chosen.length} pack`;
  } else if (allInstalled && !hasUpdate) {
    run.textContent = `Perbaiki / Pasang ulang ${chosen.length} pack`;
  } else if (allInstalled) {
    run.textContent = `Update ${chosen.length} pack`;
  } else {
    run.textContent = `Pasang ${chosen.length} pack`;
  }
}

function render() {
  renderNav();
  renderHero();
  renderGrid();
  renderDock();
  $("verify").disabled = busy;
  $("repair-all-side").disabled = busy;
  $("pick-temp").disabled = busy;
  $("pick-graphics").disabled = busy;
}

function showTempDir(path) {
  $("temp-path").textContent = path ?? "Otomatis";
  $("temp-path").title = path ?? "";
}

function showGraphics(path) {
  $("graphics-path").textContent = path ?? "Otomatis";
  $("graphics-path").title = path ?? "";
}

async function loadCover(plan) {
  if (!plan.cover || covers.has(plan.id)) return;
  covers.set(plan.id, null);
  try {
    const image = await invoke("cover_image", { id: plan.id });
    if (image) {
      covers.set(plan.id, image);
      renderGrid();
    }
  } catch {
    covers.delete(plan.id);
  }
}

function toggle(plan) {
  if (busy) return;
  const wasSelected = selected.has(plan.id);
  if (plan.group) {
    for (const peer of plans) if (peer.group === plan.group) selected.delete(peer.id);
  }
  if (wasSelected) selected.delete(plan.id);
  else selected.add(plan.id);
  render();
}

function selectPending() {
  selected.clear();
  for (const plan of plans) if (needsWork(plan)) selected.add(plan.id);
}

function setBusy(value) {
  busy = value;
  $("dock-idle").hidden = value;
  $("dock-busy").hidden = !value;
  if (value) {
    $("bar-fill").style.width = "0";
    $("progress-percent").textContent = "0%";
    $("progress-label").textContent = "Menyiapkan…";
    $("progress-detail").textContent = "";
  }
  render();
}

function openGate({ title, text, label, placeholder, error, onSubmit }) {
  $("gate-title").textContent = title;
  $("gate-text").textContent = text;
  $("gate-label").textContent = label;
  $("gate-input").value = "";
  $("gate-input").placeholder = placeholder;
  $("gate-error").textContent = error ?? "";
  gateAction = onSubmit;
  $("gate").hidden = false;
  $("gate-input").focus();
}

const isMac = /Mac|iPhone|iPad/.test(navigator.platform);

function askGameFolder(error) {
  openGate({
    title: "FM 27 belum ketemu",
    text: "Masukkan folder instalasi Football Manager 27 kamu.",
    label: "Folder game",
    placeholder: isMac
      ? "~/Library/Application Support/Steam/steamapps/common/Football Manager 27"
      : "D:\\SteamLibrary\\steamapps\\common\\Football Manager 27",
    error,
    onSubmit: async (value) => {
      await invoke("set_game_dir", { path: value });
      await start();
    },
  });
}

async function refresh(forceVerify = false) {
  if (forceVerify) {
    $("hero-sub").textContent = "Memindai integritas seluruh file…";
  }
  try {
    plans = await invoke("check", { forceVerify });
    selectPending();
    $("gate").hidden = true;
    render();
    if (forceVerify) {
      const repairs = plans.filter((p) => p.status === "needsRepair");
      const updates = plans.filter((p) => p.status === "updateAvailable");
      if (repairs.length > 0) {
        showToast(`${repairs.length} pack ditemukan bermasalah / file hilang. Klik 'Perbaiki' untuk memperbaikinya.`);
      } else if (updates.length > 0) {
        showToast(`${updates.length} update pack tersedia.`);
      } else {
        showToast("Semua file pack aman dan lengkap.");
      }
    }
  } catch (error) {
    showToast(String(error), true);
  }
}

async function startRepair(plan) {
  if (busy) return;
  setBusy(true);
  try {
    const updated = await invoke("repair", { id: plan.id });
    const idx = plans.findIndex((p) => p.id === plan.id);
    if (idx !== -1) plans[idx] = updated;
    selected.clear();
    selected.add(plan.id);

    const touchesDatabase = plan.target === "database";
    await invoke("run", { ids: [plan.id] });
    setBusy(false);
    await refresh();
    showToast(touchesDatabase
      ? `Selesai! ${plan.name} berhasil diperbaiki.`
      : `Selesai! ${plan.name} berhasil diperbaiki. Di FM, clear cache lalu reload skin.`);
  } catch (error) {
    setBusy(false);
    showToast(String(error), true);
  }
}

async function runSelected() {
  const ids = [...selected];
  if (!ids.length) return;
  setBusy(true);
  try {
    for (const id of ids) {
      const p = plans.find((x) => x.id === id);
      if (p && (p.status === "upToDate" || !p.actions || p.actions.length === 0)) {
        const repaired = await invoke("repair", { id });
        const idx = plans.findIndex((x) => x.id === id);
        if (idx !== -1) plans[idx] = repaired;
      }
    }
    const touchesDatabase = plans.some((p) => ids.includes(p.id) && p.target === "database");
    await invoke("run", { ids });
    setBusy(false);
    await refresh();
    showToast(touchesDatabase
      ? "Selesai! Real Name Fix langsung kepakai di save baru maupun save lama."
      : "Selesai! Di FM, clear cache lalu reload skin.");
  } catch (error) {
    setBusy(false);
    showToast(String(error), true);
  }
}

function askUninstall(plan) {
  $("confirm-title").textContent = `Uninstall ${plan.name}?`;
  $("confirm-text").textContent = "File pack dihapus dan file asli game dikembalikan.";
  $("confirm-yes").onclick = async () => {
    $("confirm").close();
    try {
      await invoke("uninstall", { id: plan.id });
      await refresh();
      showToast(`${plan.name} sudah dihapus.`);
    } catch (error) {
      showToast(String(error), true);
    }
  };
  $("confirm").showModal();
}

let speedSample = null;

function onProgress({ phase, done, total }) {
  const ratio = total ? Math.min(done / total, 1) : 0;
  $("bar-fill").style.width = `${ratio * 100}%`;
  $("progress-percent").textContent = `${Math.floor(ratio * 100)}%`;
  if (phase !== "downloading") {
    $("progress-label").textContent = "Memasang file";
    $("progress-detail").textContent = `${formatCount(done)} dari ${formatCount(total)} file`;
    return;
  }
  $("progress-label").textContent = "Mendownload";
  const now = performance.now();
  if (!speedSample || now - speedSample.at > 1000) {
    const speed = speedSample ? ((done - speedSample.done) / (now - speedSample.at)) * 1000 : 0;
    speedSample = { at: now, done, speed };
  }
  const rate = speedSample.speed > 0 ? ` · ${formatBytes(speedSample.speed)}/s` : "";
  $("progress-detail").textContent = `${formatBytes(done)} dari ${formatBytes(total)}${rate}`;
}

function showRegister(reason) {
  $("register-error").textContent = reason ?? "";
  $("register-move").hidden = true;
  $("register").hidden = false;
  $("register-input").focus();
}

async function activate(transfer) {
  const identifier = $("register-input").value.trim();
  if (!identifier) {
    $("register-error").textContent = "Isi License Key, WhatsApp, atau email dulu.";
    return;
  }
  $("register-error").textContent = "Menghubungi server license…";
  $("register-ok").disabled = $("register-move").disabled = true;
  try {
    const result = await invoke("license_activate", { identifier, transfer });
    if (result.ok) {
      $("register").hidden = true;
      $("register-input").value = "";
      showToast(result.status?.name ? `License aktif. Selamat datang, ${result.status.name}!` : "License aktif. Selamat datang!");
      await start();
      return;
    }
    $("register-error").textContent = !result.reached
      ? "Tidak bisa menghubungi server license. Cek koneksi internet lalu coba lagi."
      : result.message || "License tidak valid.";
    $("register-move").hidden = !result.canTransfer;
  } catch (error) {
    $("register-error").textContent = String(error);
  } finally {
    $("register-ok").disabled = $("register-move").disabled = false;
  }
}

async function start() {
  $("hero-eyebrow").textContent = "Memuat…";
  $("hero-title").textContent = "Menyiapkan pack FM 27…";
  $("hero-sub").textContent = "Menghubungi server dan memeriksa pack…";
  $("grid").innerHTML = `
    <div class="loading-state">
      <div class="spinner"></div>
      <div class="loading-text">Memuat daftar pack…</div>
    </div>
  `;

  const license = await invoke("license_status");
  if (!license.licensed) {
    showRegister(license.reason);
    return;
  }
  const { game, tempDir } = await invoke("overview");
  showTempDir(tempDir);
  if (game.Err) {
    $("game-detail").textContent = "Tidak ketemu";
    askGameFolder(game.Err);
    return;
  }
  lastGame = game.Ok;
  const build = lastGame.buildId ? `Build ${lastGame.buildId}` : "Folder manual";
  $("game-detail").textContent = `${build} · DB ${lastGame.dbVersions.at(-1) ?? "-"}`;
  $("game-dot").classList.add("ok");
  showGraphics(lastGame.graphicsDir);
  if (lastGame.inOnedrive) {
    showToast("Folder FM kamu ada di OneDrive. Kalau graphics tak muncul di game, cek path di bawah dan ganti kalau salah.", true);
  }
  await refresh();
}

$("gate-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    await gateAction?.($("gate-input").value);
  } catch (error) {
    $("gate-error").textContent = String(error);
  }
});

$("register-form").addEventListener("submit", (event) => {
  event.preventDefault();
  activate(false);
});
$("register-move").addEventListener("click", () => activate(true));

$("run").addEventListener("click", runSelected);
$("update-all").addEventListener("click", () => {
  selectPending();
  runSelected();
});
$("verify").addEventListener("click", async () => {
  if (busy) return;
  showToast("Memeriksa kelengkapan file game & pack…");
  await refresh(true);
});
$("repair-all-side").addEventListener("click", () => {
  if (busy) return;
  const installedPacks = plans.filter(installed);
  if (!installedPacks.length) {
    showToast("Belum ada pack yang terpasang untuk diperbaiki.");
    return;
  }
  $("confirm-title").textContent = "Perbaiki Semua Pack?";
  $("confirm-text").textContent = `Download dan pasang ulang ${installedPacks.length} pack yang terpasang? Cocok jika folder graphics terhapus atau file bermasalah.`;
  $("confirm-yes").onclick = async () => {
    $("confirm").close();
    selected.clear();
    for (const p of installedPacks) selected.add(p.id);
    await runSelected();
  };
  $("confirm").showModal();
});
$("pick-temp").addEventListener("click", async () => {
  const path = await window.__TAURI__.dialog.open({ directory: true });
  if (!path) return;
  await invoke("set_temp_dir", { path });
  showTempDir(path);
  showToast("Folder download sementara disimpan. File di sini dihapus otomatis setelah terpasang.");
});
$("pick-graphics").addEventListener("click", async () => {
  const path = await window.__TAURI__.dialog.open({
    directory: true,
    title: isMac
      ? "Pilih folder Football Manager 27 di Library/Application Support (yang ada folder 'games')"
      : "Pilih folder Football Manager 27 di Documents (yang ada folder 'games')",
  });
  if (!path) return;
  try {
    await invoke("set_user_dir", { path });
    showToast("Folder graphics disetel. Graphics akan dipasang ke folder ini.");
    await start();
  } catch (error) {
    showToast(String(error), true);
  }
});
$("cancel").addEventListener("click", () => invoke("cancel"));
$("confirm-no").addEventListener("click", () => $("confirm").close());

const appWindow = window.__TAURI__.window.getCurrentWindow();
$("win-min").addEventListener("click", () => appWindow.minimize());
$("win-max").addEventListener("click", () => appWindow.toggleMaximize());
$("win-close").addEventListener("click", () => appWindow.close());

listen("progress", ({ payload }) => onProgress(payload));

start().catch((error) => showToast(String(error), true));
