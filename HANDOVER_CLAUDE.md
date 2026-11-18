# Handover Document: Wedugames FM Packs (FM 27 Project)

> **Document Version:** 3.0  
> **Last Updated:** 12 November 2026 (Reflecting work up to 8–12 November 2026)  
> **Target Audience:** Claude (or any AI / software engineer taking over the project)  
> **Repository Root:** `C:\Users\dell\Documents\FM 27 Project`

---

## 1. Executive Summary

**Wedugames FM Packs** (sebelumnya bernama *FM Updater*) adalah ekosistem aplikasi desktop Windows untuk mendistribusikan, menginstal, dan memperbarui megapack grafis (facepacks, logos, kits 2D/3D, trophies) serta patch perbaikan lisensi (*Real Name Fix*) untuk **Football Manager 2027**.

### Komponen Utama:
1. **Customer Desktop App** (`src-tauri/`, `ui/`):
   - Client aplikasi ringan berbasis **Tauri v2** + **Rust** + Vanilla JS/CSS (Terminifikasi via Terser).
   - Rilis customer: [`releases/WeduGames-FM-Packs.exe`](file:///c:/Users/dell/Documents/FM%2027%20Project/releases/WeduGames-FM-Packs.exe) (juga didistribusikan sebagai `releases/WeduGames-FM-Packs v.3.exe` dan `releases/WeduGames-FM-Packs v.3.zip`).
2. **Admin Publishing App** (`uploader/`):
   - Desktop app admin (`releases/fm-uploader.exe`) untuk memproses arsip zip/rar dari kreator megapack, membuat chunk & delta patch, menandatangani manifest (Ed25519), dan mengupload ke Cloudflare R2.
3. **Cloudflare Worker & R2 Storage** (`worker/`):
   - Bucket R2: `fm-updater`.
   - Endpoint: `https://fm-updater.wedugamez.workers.dev` (tercantum di `config/api_url.txt`).
   - Worker memvalidasi tiket lisensi ECDSA P-256 (24-hour expiring ticket) sebelum melayani chunk/zip dari R2.
4. **License System**:
   - Terintegrasi dengan sistem lisensi WeduGames (`rte4fm-license`, produk `fmpacks27`).
   - Format lisensi customer: `FMP-XXXX-XXXX-XXXX` (di-generate via portal admin keygen).

---

## 2. Status Terkini & Masalah Kritis yang Telah Diselesaikan (8–12 Nov 2026)

Tiga masalah kritis terakhir yang dilaporkan user telah diselidiki dan diperbaiki 100% tuntas:

### A. Masalah Crash / Listrik Padam Saat Install (`expected value at line 1 column 1`)
- **Gejala:** Customer mengalami PC mati tiba-tiba saat proses instalasi pack sedang berjalan. Saat aplikasi dibuka kembali, muncul dialog error `expected value at line 1 column 1`.
- **Penyebab:** File `meta.json` atau `%LOCALAPPDATA%\FMUpdater\packages\<id>\state.json` terpotong (0-byte atau corrupt) saat listrik padam mendadak, membuat `serde_json::from_slice` gagal parse.
- **Solusi yang Diterapkan (`crates/core/src/state.rs`):**
  - Fungsi `read<T>` diperbaiki: file kosong (0 byte) atau JSON korup ditoleransi dengan anggapan belum terpasang (`Ok(None)`) alih-alih crash.
  - Fungsi `write<T>` diubah menjadi **Atomic Write**: data ditulis dulu ke file temporary `.tmp`, dipaksa flush ke disk via `file.sync_all()?`, lalu di-rename ke nama file target. Listrik padam tidak akan pernah lagi meninggalkan file setengah jadi.

### B. Fitur Perbaikan / Repair Pack (Jika Folder Dihapus)
- **Kebutuhan:** Customer kerap menghapus folder `graphics` secara manual saat troubleshooting, lalu bingung cara memasang ulang.
- **Solusi yang Diterapkan (`crates/core/src/engine.rs`, `src-tauri/src/main.rs`, `ui/`):**
  - Deteksi folder hilang instan (< 1 ms): `engine::plan` otomatis mendeteksi jika `(!root.exists() || root_is_empty(root))` atau chunk file hilang melalui `is_chunk_missing`, dan menandai status pack sebagai `Status::NeedsRepair`.
  - Menambahkan backend command `plan_repair` dan `repair`.
  - Menambahkan tombol UI:
    - **"Perbaiki Semua Pack"** di sidebar kiri (dengan dialog konfirmasi).
    - Tombol **"Perbaiki"** pada kartu masing-masing pack yang rusak.

### C. Masalah Startup Hang / Layar Blank Selama 2.5 Menit (580k File Stat)
- **Gejala:** Gambar cover dan kartu pack tidak muncul saat aplikasi dibuka. Layar tampak kosong/freeze selama 2.5 hingga 3 menit.
- **Penyebab:**
  - Steam melakukan update FM 27 ke `Build 26340721`, sedangkan file meta lokal mencatat `Build 26227640`.
  - Di `src-tauri/src/main.rs`, perbedaan build id memicu `force_verify = true` ke semua pack.
  - `engine.rs` menjalankan `broken_chunks` yang memanggil `std::fs::metadata` secara sequential ke **seluruh 580.000 file** (pack facepack sendiri berisi 412.000 file!).
  - Frontend menunggu invoke `check` selesai sebelum memanggil `render()`, sehingga window tampak kosong.
- **Solusi yang Diterapkan:**
  1. **Backend Decoupling (`crates/core/src/engine.rs` & `src-tauri/src/main.rs`):**
     - Update game Steam **hanya menyentuh file database** (`Target::Database` / `real-name-fix` yang hanya berisi 12 file di folder game Steam).
     - Steam **tidak pernah menyentuh folder user documents** (`Target::Graphics`).
     - Jika terdeteksi update Steam (`game_patched`), verifikasi deep scan dibatasi **hanya** untuk `Target::Database` (< 5 ms). Pack grafis tetap menggunakan deteksi cepat `< 1 ms`.
     - Deep scan 580.000 file sekarang **hanya** berjalan jika user sengaja mengklik tombol **"Cek Kelengkapan File"** (`force_verify = true`).
     - Setelah Database pack valid, `meta.last_build_id` otomatis diperbarui ke build game saat ini agar verifikasi tidak berulang.
  2. **Frontend Immediate Feedback (`ui/main.js` & `ui/style.css`):**
     - Sejak milidetik pertama dibuka, Hero langsung menampilkan teks: *"Menyiapkan pack FM 27… Menghubungi server dan memeriksa pack…"*.
     - Container `#grid` langsung menampilkan spinner animasi putar (*loading state*), sehingga layar tidak pernah kosong.
  3. **Pengujian E2E:**
     - Ditambahkan test E2E di `crates/core/tests/e2e.rs`: `steam_patch_checks_only_database_and_skips_graphics`.
     - Seluruh **42+ tests di workspace lulus 100% (0 fail)**.

---

## 3. Struktur Direktori Workspace

```
FM 27 Project/
├── Cargo.toml                  # Workspace manifest (core, packer, publisher, src-tauri, uploader)
├── Cargo.lock
├── config/
│   ├── api_url.txt             # URL Worker Cloudflare (https://fm-updater.wedugamez.workers.dev)
│   ├── public_key.hex          # Ed25519 Public Key untuk verifikasi manifest
│   └── update-check.json       # Pola pengecekan versi baru di situs kreator (admin side)
├── crates/
│   ├── core/                   # Engine utama: detect, download, engine, install, license, manifest, state
│   │   ├── src/
│   │   └── tests/              # e2e integration tests (14 tests)
│   ├── packer/                 # Pembuat chunk zip, hashing, signing manifest
│   └── publisher/              # CLI fmup-publish (upload ke Cloudflare R2)
├── src-tauri/                  # Tauri v2 backend wrapper untuk customer app
│   ├── src/main.rs             # Tauri commands (overview, check, repair, run, cancel, uninstall, license)
│   └── Cargo.toml
├── ui/                         # Frontend customer app (Vanilla JS, CSS, SVG, HTML)
│   ├── index.html
│   ├── main.js
│   ├── style.css
│   ├── art.js                  # Algorithmic cover fallback jika image cover tidak ada
│   └── dist/                   # File hasil minifikasi terser (yang di-embed ke dalam release binary)
├── uploader/                   # FM Uploader (Admin tool desktop GUI)
├── worker/                     # Cloudflare Worker code (validasi ECDSA P-256 + R2 streaming)
│   ├── src/index.js
│   └── wrangler.toml
├── scripts/
│   ├── build_app.ps1           # SCRIPT BUILD RESMI: Minify UI -> Cargo release -> Copy ke releases
│   ├── build_ui.ps1            # Minifikasi main.js dan art.js menggunakan terser ke ui/dist
│   ├── make_icon.py            # Pembuat icon Tauri
│   └── fmup-publish.exe        # Tool admin CLI untuk audit & publish R2
└── releases/
    ├── WeduGames-FM-Packs.exe  # Binary release resmi hasil build scripts/build_app.ps1
    ├── WeduGames-FM-Packs v.3.exe # Salinan build terbaru v.3 (8 Nov 2026)
    ├── WeduGames-FM-Packs v.3.zip # Zip bundle siap kirim ke customer
    └── fm-uploader.exe         # Admin tool binary
```

---

## 4. Invarian & Aturan Teknis Penting (JANGAN DIRUBAH!)

1. **Nama Internal & Path Persisten:**
   - Folder data lokal di komputer user: `%LOCALAPPDATA%\FMUpdater`. **Jangan diubah** karena menyimpan file lisensi dan state pack pengguna yang sudah ada.
   - Nama R2 bucket: `fm-updater`.
   - Nama Worker: `fm-updater`.
2. **Aturan Target Path Game:**
   - `Target::Graphics`: `%USERPROFILE%\Documents\Sports Interactive\Football Manager 27\graphics`.
   - `Target::Database`: Folder install Steam FM 27 `<Steam>\shared\data\database\db\<version>\lnc`.
3. **Sistem Lisensi (Strict Online):**
   - Validasi tiket menggunakan ECDSA P-256 curve (tiket berlaku 24 jam).
   - Tidak ada toleransi offline (*zero offline grace*). Customer wajib online saat membuka aplikasi dan mengunduh pack.
4. **Build & Release Protocol:**
   - **Selalu** gunakan script [`scripts/build_app.ps1`](file:///c:/Users/dell/Documents/FM%2027%20Project/scripts/build_app.ps1) untuk meng-compile binary customer.
   - Script ini menjalankan Terser untuk mem-mangle JS ke `ui/dist`, lalu menjalankan `cargo build --release -p wedugames-fm-packs` dengan optimasi `lto = "fat"`, `codegen-units = 1`, dan `strip = "symbols"`.
   - **DILARANG** mengaktifkan `--features demo` pada binary rilis customer.

---

## 5. Cheat Sheet Perintah untuk Claude

### Menjalankan Unit & Integration Test
```powershell
cargo test
```
*Catatan: Semua 42+ test harus lulus tanpa error.*

### Menjalankan Test E2E Tertentu (misal patch game Steam)
```powershell
cargo test --test e2e steam_patch_checks_only_database_and_skips_graphics
```

### Menjalankan Build UI (Terser Minification)
```powershell
powershell -ExecutionPolicy Bypass -File scripts/build_ui.ps1
```

### Menjalankan Full Release Build
```powershell
powershell -ExecutionPolicy Bypass -File scripts/build_app.ps1
```
*Hasil binary akan otomatis disalin ke `releases\WeduGames-FM-Packs.exe`.*

### Menjalankan UI Preview Server
```powershell
python -m http.server 8765 --bind 127.0.0.1 --directory ui
```

---

## 6. Daftar Pack yang Sedang Live di Cloudflare R2 (17 Megapacks)

| No | Nama Pack | Kategori | Target Folder | Catatan |
|---|---|---|---|---|
| 1 | Real Name Fix | Database | `<game>\shared\data\database\db\*\lnc` | Patch semua versi DB aktif |
| 2 | Cut-Out Player Faces | Faces | `graphics\faces\cut-out-player-faces` | ~412.000 file (v2027.00) |
| 3 | DF11 Classic | Faces | `graphics\faces\df11` | 16.9 GB |
| 4 | FMSite Facepack | Faces | `graphics\faces\fmsite` | 26.9 GB |
| 5 | sortitoutsi SS Kits | Kits (2D) | `graphics\kits` | Ekstrak rata ke root kits |
| 6 | 3D Kits | Kits (3D) | `graphics\kits\3d kits` | Pilihan alternatif 3D |
| 7 | 3D Unity Kits | Kits (3D) | `graphics\kits\3d kits` | v2027.02 (Unity format) |
| 8 | TCM Logos | Logos | `graphics\logos\tcm` | |
| 9 | FMG Standard | Logos | `graphics\logos\fmg-standard` | |
| 10 | FMV Logos | Logos | `graphics\logos\fmv` | |
| 11 | Metallic Logos | Logos | `graphics\logos\metallic` | |
| 12 | Plastic Logos | Logos | `graphics\logos\plastic` | |
| 13 | Grunge Logos | Logos | `graphics\logos\grunge` | |
| 14 | FMG Logos | Logos | `graphics\logos\fmg-logos` | |
| 15 | Steel Logos | Logos | `graphics\logos\steel` | |
| 16 | Vapour Logos | Logos | `graphics\logos\vapour` | |
| 17 | Trophies Pack | Other | `graphics\trophies` | |

---

## 7. Catatan Keamanan & Tugas Lanjutan untuk Masa Depan

1. **Drive D:**
   - Driver hardware Drive D (`D:\Master Megapack`) dilaporkan bermasalah (*Full Repair Needed* / hardware error `c0000483`). Jangan menulis ke drive D. Arsip lokal sebaiknya disimpan di Drive C jika diperlukan.
2. **Rotasi Signing Key Sebelum Rilis Publik Terbuka:**
   - Jika `keys/signing.key` lama dirasa perlu diganti, jalankan `packer keygen`, perbarui `config/public_key.hex`, tanda tangani ulang manifest dengan `fmup-publish`, dan build ulang aplikasi customer.
3. **Code Signing Certificate:**
   - Binary exe saat ini belum memiliki sertifikat Code Signing EV/OV, sehingga Windows SmartScreen mungkin memunculkan peringatan pada download pertama di komputer baru. Hal ini normal untuk binary yang belum ditandatangani sertifikat Microsoft.

---
*Dokumen ini dibuat otomatis sebagai panduan komprehensif bagi Claude untuk langsung melanjutkan pengembangan tanpa kehilangan konteks teknis atau historis.*
