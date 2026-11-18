# Handoff Wedugames FM Packs (dulu FM Updater)

> **PENTING UNTUK CLAUDE / DEVELOPER BARU:**  
> Dokumen handover terbaru dan terlengkap (mencakup perbaikan crash listrik padam, fitur repair, dan optimasi freeze startup Steam 8-12 Nov 2026) telah didokumentasikan secara rinci di **[`HANDOVER_CLAUDE.md`](file:///c:/Users/dell/Documents/FM%2027%20Project/HANDOVER_CLAUDE.md)**. Silakan baca file tersebut sebagai acuan utama.

Ditulis 31 Oktober 2026 sore (diperbarui 12 November 2026). Baca ini dan `HANDOVER_CLAUDE.md` dulu sebelum lanjut. Panduan pemakaian ada di `README.md`.

## Apa ini

- **Wedugames FM Packs** (`src-tauri`, `ui`): aplikasi customer, tanpa license untuk sementara.
- **FM Uploader** (`uploader`): aplikasi admin, `releases/fm-uploader.exe`.
- **R2:** bucket `fm-updater`. Patch system, install dengan backup, cover, dan OneDrive sudah ada.
- Cek status R2: `scripts\fmup-publish.exe --check`. Ukur isi bucket: `--audit`. Hapus sisa upload terputus (lebih dari 6 jam): `--abort-unfinished`
- Cek versi baru di situs kreator: `scripts\fmup-publish.exe --updates` (baca `config/update-check.json`, tidak sentuh R2, admin saja). Setelah upload versi baru: `--mark-seen <id> <versi>` biar berhenti ditandai.

## Status

| Hal | Status |
|---|---|
| Live di R2 | 17 pack, semua dengan cover: Real Name Fix, Trophies, TCM, FMG Standard, FMV, Metallic, Plastic, Grunge, FMG Logos, Steel, Vapour, Cut-Out Player Faces (v2027.00), sortitoutsi SS Kits, 3D Kits, 3D Unity Kits (v2027.02), DF11 Classic (16,9 GB), dan FMSite Facepack (26,9 GB) |
| DF11, FMSite | **Live** 1 Nov 2026. User sudah memastikan izin. Isinya hanya `.png` dan `.xml`, tanpa `.exe` |
| Kits sortitoutsi | Ketiganya live. SS (`kits-2d`, ke `kits`), 3D dan 3D Unity (`kits-3d`, pilih satu, ke `kits\3d kits`). Zip sortitoutsi berisi `<versi>\graphics\kits\*`, sedangkan packer hanya membuang satu folder pembungkus. Jadi isi `graphics\kits` di-extract rata ke `C:\FMWork\src\<id>` dan folder itu yang jadi `src`. 2D dan 3D sama-sama punya `config.xml` dan `all_config.xml`, sehingga harus beda folder |
| App kits 2D + 3D | Kode `ui/` dan `uploader/ui` sudah diubah, **exe belum di-build ulang** |
| Worker pack | **Live** 31 Okt 2026 di `https://fm-updater.wedugamez.workers.dev` (sudah di `config/api_url.txt`). Wajib tiket license, tanpa tiket dapat 401 |
| License | **Live** 31 Okt 2026: Worker license `rte4fm-license` (produk `fmpacks27`, sudah dicek RTE4FM tetap normal) dan portal `wedugames-keygen.vercel.app` (pilihan FM Packs, key `FMP-`). Backup sumber portal: `TSR PROJECT\Keygen WeduGames.html.bak-20261031` |
| Exe customer | `releases/WeduGames-FM-Packs.exe` (atau `-v2` bila yang lama terkunci), build 1 Nov sudah **dikeraskan** (lihat bawah). **Aktivasi dan download belum dites** dengan key `FMP-` asli |
| Auto-cek versi (admin) | Ditambah 3 Nov. `fmup-publish --updates` memindai situs kreator (pola dari catalogue FM27 Graphics Manager, di `config/update-check.json`) dan menandai pack yang ada versi lebih baru dari yang kita publish. Hanya sisi admin/seller, customer tidak tahu. 10 dari 17 pack ter-cover; sisanya (RNF, Trophies, TCM, Metallic, FMG Logos, FMSite, 3D Kits non-unity) dicek manual. Tes 3 Nov: DF11 ada update v2, 6 logo FMG ada update bulanan v202611 |
| Deteksi OneDrive | Diperbaiki 1 Nov. `detect.rs` kini memindai **semua** folder `OneDrive*` di profil user (bukan cuma dari nama env var), dan client bisa pilih folder graphics manual (tombol "Folder graphics FM" → `set_user_dir`, menerima folder FM / induk Documents / `Sports Interactive` / `graphics`). Path graphics ditampilkan di sidebar. Dipicu karena pack tak kebaca di PC client yang Documents-nya di OneDrive |
| Pengamanan exe | Release Rust: LTO penuh + `codegen-units = 1` + `strip = "symbols"` di `Cargo.toml`. Frontend di-minify/mangle oleh terser ke `ui/dist` (`scripts/build_ui.ps1`), dan Tauri mengompres aset itu (brotli) di dalam exe, jadi JS tidak terbaca sebagai teks. **Build final pakai `scripts/build_app.ps1`** (minify → compile → salin ke `releases`). Belum ada code signing cert, jadi SmartScreen masih akan warning |
| Tes di FM 27 asli | **Belum pernah.** Jalur `graphics\logos` dan `graphics\faces` belum terbukti |

## Bahaya yang belum selesai

1. **PC kemungkinan kena malware.** `Windows Driver Foundation (WDF).exe` (693 MB, tanpa tanda tangan) dan `wzone.exe` ada di `%LOCALAPPDATA%\google\chrome\user data`, masuk Exclusions Defender, dan terhubung ke `51.79.138.6:443`. Muncul 30 Oktober 23:33, 4 menit setelah `uTorrentPro.3.6.0.47390.kuyhAa.7z` diunduh. Prosesnya sudah dihentikan sekali, filenya masih ada dan bisa jalan lagi.
   - User harus: hapus dua Exclusion, jalankan Defender Offline scan, scan Malwarebytes.
   - Dari HP: ganti password Google, Cloudflare, Football Manager Graphics, dan akun lain di Chrome. Nyalakan 2FA. Hapus token R2 lama, lalu bikin baru setelah PC bersih.
   - Jangan login akun penting di PC ini dulu. Install ulang Windows tetap opsi paling aman.
2. **Drive D rusak.** Status "Full Repair Needed", `chkdsk` gagal dengan error hardware `c0000483`. Jangan menulis ke D. Arsip di `D:\Master Megapack` hanya boleh dibaca. Jangan pakai untuk kerja berat lagi.
3. **`signing.key` dianggap bocor.** Bikin key baru sebelum rilis: `packer keygen`, update `config/public_key.hex`, tanda tangan ulang manifest, build ulang exe customer.
4. **Izin gambar:** wajah Messi dan logo klub di cover perlu izin sendiri sebelum dijual.
5. **License:** kodenya sudah selesai, tapi baru melindungi setelah ketiga Worker/portal di-deploy dan exe di-build ulang. Aturan dari user: wajib online tanpa toleransi offline, dan tiket download berlaku 24 jam (app memperbaruinya otomatis). Tiket yang bocor masih bisa dipakai paling lama 24 jam.

## Urutan lanjut

1. Nama app sudah **Wedugames FM Packs** dan exe sudah di-build ulang + dikeraskan 1 Nov (`scripts/build_app.ps1`). Nama internal (bucket `fm-updater`, Worker, `%LOCALAPPDATA%\FMUpdater`) jangan diubah.
2. Cover kits bisa dibuat ulang dengan `scripts/make_kits.py` (mode `2d`/`3d`).
3. User membersihkan PC dan mengganti token.
4. Ganti `signing.key` dan tanda tangan ulang manifest.
5. Bikin key `FMP-` di portal, lalu tes register dan download di exe baru.
6. Tes pasang pack di FM 27 asli, termasuk uninstall.
7. Sign exe dan urus izin gambar.

## Catatan teknis

- Arsip zip disalin tanpa extract (`plan_zips` di `crates/packer`). `.rar` di-extract dengan 7-Zip di `C:\FMWork`.
- Real Name Fix masuk ke semua folder database, sesuai panduannya, dan kepakai di save lama.
- File dengan nama tidak valid di Windows dilewati dan dilaporkan.
- `scripts/rescue_pack.ps1`: salin dari D ke C, tes arsip, upload, hapus salinan, dengan prioritas Idle.
- Cover dibuat dari `images` sesi lama lewat `scripts/make_*.py`. Hasil final ada di `covers/`, jadi tidak perlu dibuat ulang.
- Build ulang butuh sekitar 10 menit karena folder `target` pernah dikosongkan.
