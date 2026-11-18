# Wedugames FM Packs

Dulu bernama FM Updater. Nama itu masih dipakai di bagian internal: bucket
`fm-updater`, Worker, dan folder `%LOCALAPPDATA%\FMUpdater`.

Aplikasi Windows buat pasang dan update facepack, logo pack, kit pack, dan Real
Name Fix untuk Football Manager 27. File disimpan di Cloudflare R2. Customer
tinggal pilih pack, lalu klik Update. Pack di-upload pakai
FM Uploader, aplikasi terpisah khusus admin.

| Folder | Isi |
| --- | --- |
| `crates/core` | Deteksi game, download, install, backup, rollback |
| `crates/packer` | Bikin zip dasar dan patch, tanda tangan manifest, CLI keygen |
| `crates/publisher` | Upload ke R2 dan penerbitan update |
| `src-tauri`, `ui` | Aplikasi customer |
| `uploader` | FM Uploader, aplikasi admin |
| `worker` | Cloudflare Worker: cek license dan layani file dari R2 |
| `config` | URL Worker dan public key, ikut ke-compile ke aplikasi |

## Setup sekali

`keys/signing.key` sudah dibuat dan public key-nya sudah ada di
`config/public_key.hex`. Backup file itu di tempat aman dan jangan pernah
dibagikan. Kalau hilang, aplikasi yang sudah beredar nggak bisa nerima update lagi.

1. Login dan bikin bucket serta KV. Salin `id` KV ke `worker/wrangler.toml`.

   ```bash
   cd worker
   npx wrangler login
   npx wrangler r2 bucket create fm-updater
   npx wrangler kv namespace create LICENSES
   npx wrangler deploy
   ```

2. Tulis URL hasil deploy ke `config/api_url.txt`.
3. Build aplikasi customer. Hasilnya `target/release/wedugames-fm-packs.exe`.
   Ikon dan simbol W di app dibuat dari `assets/wedugames-logo.webp` dengan
   `python scripts/make_icon.py`.

   ```bash
   npx @tauri-apps/cli@2 build --no-bundle
   ```

4. Di dashboard Cloudflare, buka R2 lalu bikin API token dengan akses Object
   Read & Write ke bucket `fm-updater`.
5. Buka `target/release/fm-uploader.exe`, klik Pengaturan, lalu isi token dan
   pilih file `keys/signing.key`.

FM Uploader jangan dibagikan ke customer. Token disimpan sebagai teks biasa di
`%LOCALAPPDATA%\FMUploader`, jadi cuma pakai di PC sendiri.

## Upload pack

1. Taruh semua arsip satu pack dalam satu folder, misalnya
   `D:\Master Megapack\Logopack\FMG Steel`. Nomor versi di nama file, seperti
   `2026.05`, dipakai buat urutan extract. Pack lain harus di folder lain.
2. Di FM Uploader, klik Tambah pack, lalu isi nama, kategori, folder itu, dan
   gambar cover (opsional, 640x400, png, jpg, atau webp, maksimal 2 MB).
3. Cek kolom folder tujuan. Defaultnya `logos\<nama pack>` atau
   `faces\<nama pack>` di dalam `graphics`. Kit 2D langsung ke `kits`, dan
   Kit 3D ke `kits\3d kits\<nama pack>`.
4. Pilih pack, lalu klik Upload.

Butuh [7-Zip](https://7-zip.org) terpasang. Uploader extract arsip berurutan,
membuang folder pembungkus yang bernama versi, menimpa hasilnya, lalu menghapus
hasil extract setelah di-zip. Folder yang sudah di-extract juga bisa dipilih.

Facepack, logo pack, Kit 2D, dan Kit 3D saling menggantikan per kategori:
customer cuma bisa pasang satu dari tiap kategori, jadi satu Kit 2D dan satu
Kit 3D bisa dipasang bareng. Pack kits sortitoutsi membungkus filenya dalam
`<versi>\graphics\kits`, padahal uploader cuma membuang satu folder
pembungkus. Jadi isi `graphics\kits` di-extract dulu ke folder sendiri, lalu
folder itu yang dipilih. Kategori Lainnya bisa dipasang bareng apa saja. Real Name
Fix otomatis masuk ke semua folder database, sesuai panduan pembuatnya.

## Update pack

Taruh arsip update baru di folder pack itu, lalu pilih pack dan klik Upload.
Arsip yang sudah pernah di-upload dilewati, jadi yang diproses cuma yang baru.
File yang baru atau berubah dibungkus jadi satu patch, dan versinya ikut nomor
di nama arsip. Customer lama cuma download patch itu. Customer baru download zip
dasar lalu semua patch.

Kalau upload putus, klik Upload lagi. File yang sudah masuk R2 nggak dikirim
ulang, dan customer baru lihat update setelah semua file lengkap.

Satu aturan: **jangan hapus atau ganti nama arsip yang sudah di-upload.** Arsip
yang namanya berubah dianggap update baru. Patch cuma menambah dan menimpa, jadi
file yang dihilangkan dari pack tetap ada di komputer customer.

Tarik di FM Uploader menghapus pack dari aplikasi customer, tapi customer yang
sudah pasang tetap punya filenya. Bersihkan file lama menghapus permanen file R2
yang nggak dipakai lagi dan umurnya lebih dari 7 hari.

Update bisa dipublish kapan saja tanpa rilis aplikasi baru, karena aplikasi
ngecek manifest tiap dibuka. Rilis ulang cuma perlu kalau kode aplikasinya berubah.

## License

Pakai sistem license WeduGames yang sama dengan RTE4FM 27, dengan produk
`fmpacks27`.

1. Admin bikin key di portal keygen WeduGames. Pilih produk "Wedugames FM
   Packs", dan key-nya berawalan `FMP-`.
2. Saat pertama dibuka, app minta License Key, WhatsApp, atau email. App
   bertanya ke Worker license (`rte4fm-license`, kodenya di
   `FMRWG\RTE4FM-Keygen`), lalu dapat tiket yang ditandatangani untuk PC ini.
   Key-nya diingat di `%LOCALAPPDATA%\FMUpdater\license.json`.
3. **Wajib online, tanpa toleransi offline.** Setiap kali dibuka, app bertanya
   ke server. Kalau server tidak bisa dihubungi, layar register muncul dengan
   pesan "cek koneksi internet". Kalau server menolak (diblokir, expired, atau
   salah produk), key dilupakan dan layar register muncul lagi.
4. Tiket berlaku 24 jam di Worker pack. App mengambil tiket baru saat dibuka,
   sebelum setiap download, dan tiap 6 jam selama app terbuka, jadi customer
   tidak merasakan apa-apa. Tiket yang disalin keluar dari app mati dalam 24
   jam.
5. Kuota PC dipilih per key di portal keygen (defaultnya 3). Kalau penuh,
   muncul tombol "Pindahkan license ke PC ini" dengan cooldown 180 hari. Aturan
   ini ada di Worker license dan berlaku untuk semua produk WeduGames.
6. Worker pack (`worker/`) cuma melayani request yang membawa tiket valid
   (`REQUIRE_LICENSE = "true"`). Worker pack cuma menyimpan public key, tanpa
   secret dan tanpa data akun.

ID PC dihitung sama seperti RTE4FM (`crates/core/src/license.rs`), jadi akun
dengan akses semua produk menghitung satu PC sekali saja. Tiket diberi tag
`FMPACKS27` dan jam terbit (RTE4FM cuma pakai tanggal), jadi tiket RTE4FM
tidak bisa membuka FM Packs.

## Build dan tes

```bash
cargo test -p fmup-core -p publisher
```

Tesnya pakai server lokal dan folder sementara, jadi nggak nyentuh instalasi FM.

Build FM Uploader dari folder `uploader`. Hasilnya `target/release/fm-uploader.exe`.

```bash
npx @tauri-apps/cli@2 build --no-bundle
```
