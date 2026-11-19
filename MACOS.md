# Wedugames FM Packs — versi macOS

Folder ini adalah salinan proyek yang sudah disesuaikan untuk macOS. Proyek
Windows asli (`..`) tidak disentuh sama sekali. Semua yang berubah ada di
folder ini saja.

## Bisa dibikin? Ya.

Inti logika aplikasi — download + resume, verifikasi checksum, ekstrak zip,
backup/rollback, dan license — memang sudah ditulis cross-platform. Yang
bergantung Windows cuma beberapa titik kecil, dan semuanya sudah diberi cabang
per platform di folder ini.

Satu batasan teknis yang tidak bisa dihindari: **biner `.app` dan `.dmg` hanya
bisa dihasilkan di dalam macOS** (atau CI macOS), karena butuh toolchain Apple
(Xcode, codesign, hdiutil). Dari Windows tidak bisa. Jadi langkah build akhir
dilakukan di Mac — atau gratis lewat GitHub Actions, lihat bagian bawah.

## Apa saja yang diubah dari proyek asli

| File | Perubahan |
| --- | --- |
| `crates/core/src/detect.rs` | Deteksi Steam di `~/Library/Application Support/Steam`; folder user FM di `~/Library/Application Support/Sports Interactive/Football Manager 27` (macOS tidak pakai Documents); folder database juga dicari di dalam bundle `*.app/Contents/Resources/shared/data/database/db`; cek game berjalan pakai `pgrep` |
| `crates/core/src/license.rs` | HWID macOS dari `ioreg` (IOPlatformUUID) — macOS tidak punya `/etc/machine-id`; tiap Mac terhitung satu PC sendiri di kuota license |
| `crates/core/src/install.rs` | Pesan izin menulis menyesuaikan platform (bukan lagi "jalankan sebagai Administrator") |
| `Cargo.toml` | Member `uploader` dihapus — FM Uploader tetap aplikasi Windows khusus admin |
| `src-tauri/tauri.conf.json` | `bundle.active: true`, target `app` + `dmg`, ikon menyertakan `icon.icns` |
| `src-tauri/icons/icon.icns` | Dibuat dari logo yang sama (scripts/make_icns.py) |
| `ui/main.js` | Placeholder/hint folder menyesuaikan platform (Documents vs Library/Application Support) |

## Prasyarat di Mac

```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
python3 -m pip install --user pillow numpy
npm install --no-audit --no-fund   # di folder ini, untuk terser
```

## Build di Mac

```bash
cd macos-build
./scripts/build-macos.sh
```

Hasilnya:
- `target/release/bundle/macos/Wedugames FM Packs.app`
- `target/release/bundle/dmg/Wedugames FM Packs.dmg` (dan salinannya di `releases/`)

> Catatan: `ui/dist` di folder ini masih salinan Windows. `./scripts/build-macos.sh`
> otomatis membangun ulang dari `ui/main.js` versi macOS (placeholder & hint folder
> sudah menyesuaikan platform), jadi selalu build lewat script tersebut.

Build universal (Intel + Apple Silicon jadi satu app):

```bash
npx @tauri-apps/cli@2 build --target universal-apple-darwin
```

## Tanpa Mac: GitHub Actions

Folder ini sudah berisi `.github/workflows/build-macos.yml`. Dorong ke
repository GitHub sendiri, lalu di tab Actions jalankan workflow "Build macOS"
(atau push tag `v*`). Artifact `wedugames-fm-packs-macos` berisi `.app` +
`.dmg`.

## Menandatangani & notarisasi (penting untuk distribusi)

Tanpa tanda tangan Developer ID, Mac pengguna dapat peringatan Gatekeeper saat
pertama membuka app (masih bisa dibuka lewat klik kanan → Open, tapi tidak
nyaman).

Kalau punya Apple Developer account:
1. Buat sertifikat **Developer ID Application** di developer.apple.com →
   Certificates, export sebagai `.p12`.
2. Build lokal: set env berikut lalu jalankan `./scripts/build-macos.sh`:
   `APPLE_SIGN_IDENTITY`, `APPLE_ID`, `APPLE_TEAM_ID`,
   `APPLE_APP_SPECIFIC_PASSWORD`.
3. CI: tambahkan secrets dengan nama yang sama (lihat workflow); langkah
   signing otomatis jalan kalau secrets ada.

App ID di Apple Developer bisa pakai `com.wedugames.fmpacks`, sama dengan
`identifier` di `src-tauri/tauri.conf.json`.

## Perilaku di macOS yang perlu diketahui

- **Folder data app**: `~/Library/Application Support/FMUpdater`
  (license.json, meta.json, cache, backup). Di Windows lokasinya
  `%LOCALAPPDATA%\FMUpdater`.
- **Deteksi game**: Steam Mac di
  `~/Library/Application Support/Steam/steamapps/libraryfolders.vdf`. FM 27
  versi macOS harus terpasang lewat Steam. (Konfirmasi sekali di Steam bahwa
  FM 27 kamu memang punya versi macOS, seperti FM-FM sebelumnya.)
- **Folder user FM**: `~/Library/Application Support/Sports
  Interactive/Football Manager 27` — di macOS FM menyimpan data di
  Application Support, bukan Documents seperti di Windows.
- **Real Name Fix (Database)**: folder `db/<versi>` berada di dalam bundle
  game, tepatnya di plugin bundle bersarang:
  `fm.app/Contents/PlugIns/game_plugin.bundle/Contents/Resources/shared/data/database/db`
  (diverifikasi di Mac: install Steam FM 27 pakai `fm.app`, bukan
  `Football Manager 27.app`). `resolve_db_dir()` di `crates/core/src/detect.rs`
  mencari `Contents/Resources/...` langsung maupun di dalam `PlugIns/*.bundle`.
  Menulis ke dalam bundle membuat Steam menganggap file game berubah — sama
  seperti perilaku di Windows, dan umumnya aman. Kalau Gatekeeper/Steam
  protes: Steam → kanan → Properties → Installed Files → Verify integrity of
  game files, lalu pasang ulang Real Name Fix. (Letak persis folder DB di
  dalam bundle dicek pertama kali di Mac; kalau beda, sesuaikan
  `resolve_db_dir()` di `crates/core/src/detect.rs`.)
- **License**: tiap Mac dihitung sebagai satu PC (ID dari IOPlatformUUID via
  `ioreg`). Aktivasi pertama di Mac memakai satu slot kuota key — itu wajar.
  Server license (`rte4fm-license`) dan Worker pack (`worker/`) **tidak perlu
  diubah**.
- **Cek game berjalan**: pakai `pgrep` dengan nama proses `fm` / `football`.
  Kalau nama proses FM versi Mac ternyata lain, tambahkan di `game_is_running()`
  di `crates/core/src/detect.rs`.
- **FM Uploader & publish tetap Windows**: uploader butuh 7-Zip dan khusus
  admin, jadi tidak dibawa ke macOS. Update pack tetap dipublish dari PC
  Windows seperti biasa.

## Verifikasi dari Windows (tanpa Mac)

Kode jalur macOS bisa dicek tipe-nya dari Windows (target Apple, tanpa perlu
Mac):

```bash
rustup target add aarch64-apple-darwin
cargo check -p fmup-core --target aarch64-apple-darwin
```

Build dan bundling final tetap perlu macOS.

## Catatan

- `keys/signing.key` **sengaja tidak ikut** di folder ini supaya tidak
  tersebar. Manifest tetap ditandatangani dari PC Windows; app macOS hanya
  membaca public key dari `config/public_key.hex` (ikut ter-copy).
- `worker/` ikut ter-copy apa adanya, tidak ada perubahan.