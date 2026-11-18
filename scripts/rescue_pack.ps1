param([string[]]$Ids)
# Copy each pack off the failing drive, verify it, publish it, drop the copy.
$ErrorActionPreference = 'Continue'
$z = 'C:\Program Files\7-Zip\7z.exe'
$tool = 'C:\Users\dell\Documents\FM 27 Project\scripts\fmup-publish.exe'
$packs = Get-Content "$env:LOCALAPPDATA\FMUploader\packs.json" -Raw | ConvertFrom-Json
$today = (Get-Date).ToString('yyyy.MM.dd')
$failed = @()
$Ids = $Ids -split ','
# Everything started from here inherits this, so the PC stays usable.
[Diagnostics.Process]::GetCurrentProcess().PriorityClass = 'Idle'

foreach ($id in $Ids) {
    $pack = $packs | Where-Object id -eq $id
    if (-not $pack) { "[$id] not registered"; $failed += $id; continue }
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $in = "C:\FMWork\in\$id"
    New-Item -ItemType Directory -Force $in | Out-Null
    $bad = $null

    foreach ($f in Get-ChildItem -LiteralPath $pack.src -File | Sort-Object Name) {
        $dest = Join-Path $in $f.Name
        if ((Test-Path -LiteralPath $dest) -and (Get-Item -LiteralPath $dest).Length -eq $f.Length) { continue }
        try {
            [IO.File]::Copy($f.FullName, $dest, $true)
            if ((Get-Item -LiteralPath $dest).Length -ne $f.Length) { throw 'size differs' }
        } catch { $bad = "copy $($f.Name): $($_.Exception.Message)"; break }
    }
    "[$id] copied in $([math]::Round($clock.Elapsed.TotalMinutes,1)) min"

    if (-not $bad) {
        foreach ($f in Get-ChildItem -LiteralPath $in -File | Sort-Object Name) {
            & $z t -bso0 -bsp0 $f.FullName
            if ($LASTEXITCODE -ne 0) { $bad = "archive damaged: $($f.Name)"; break }
        }
        "[$id] verified at $([math]::Round($clock.Elapsed.TotalMinutes,1)) min"
    }
    if (-not $bad) {
        & $tool --id $id --src $in --today $today
        if ($LASTEXITCODE -ne 0) { $bad = "publish failed (exit $LASTEXITCODE)" }
    }

    if ($bad) {
        "[$id] FAILED: $bad"
        $failed += $id
    } else {
        "[$id] DONE in $([math]::Round($clock.Elapsed.TotalMinutes,1)) min"
    }
    Remove-Item -LiteralPath $in -Recurse -Force -Confirm:$false -ErrorAction SilentlyContinue
    "[$id] C free: $([math]::Round((Get-PSDrive C).Free/1GB,1)) GB"
}
"=== live on R2"
& $tool --check
"=== failed: $($failed -join ', ')"

