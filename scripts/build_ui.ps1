# Produces ui\dist: the frontend that gets embedded in the exe, with JS minified
# and local names mangled so the shipped app is hard to read. Source stays in ui\.
# Top-level names are left intact on purpose: art.js and main.js share globals
# (both are classic <script defer>), so mangling them would break the app.
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$src = Join-Path $root "ui"
$dist = Join-Path $src "dist"
$terser = Join-Path $root "node_modules\.bin\terser.cmd"

if (-not (Test-Path $terser)) { throw "terser belum terpasang. Jalankan: npm install" }
if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
New-Item -ItemType Directory -Path $dist | Out-Null

foreach ($js in "art.js", "main.js") {
    & $terser (Join-Path $src $js) --compress --mangle --comments false -o (Join-Path $dist $js)
    if ($LASTEXITCODE -ne 0) { throw "terser gagal pada $js" }
    Write-Host "minified $js"
}

# HTML and CSS hold no secrets; copy them as-is next to the minified JS.
Copy-Item (Join-Path $src "index.html") $dist
Copy-Item (Join-Path $src "style.css") $dist
Copy-Item (Join-Path $src "brand.png") $dist
Write-Host "ui\dist ready"
