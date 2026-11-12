# Create GitHub release v0.1.0 + upload zip + set repo description/topics. ASCII-only.
$info = "protocol=https`nhost=github.com`n`n" | git credential fill
$token = ($info | Select-String -Pattern "^password=(.+)$").Matches[0].Groups[1].Value
$h = @{ Authorization = "Bearer $token"; Accept = "application/vnd.github+json"; "X-GitHub-Api-Version" = "2022-11-28" }
$repo = "Achernar046/overlay"

# 1) repo description + topics
$meta = @{ description = "Windows system monitor in Rust - realtime egui dashboard, click-through game overlay, tray daemon, SQLite history, named-pipe IPC"; homepage = "" } | ConvertTo-Json
Invoke-RestMethod -Uri "https://api.github.com/repos/$repo" -Method Patch -Headers $h -Body $meta -ContentType "application/json" | Out-Null
$topics = @{ names = @("rust", "system-monitor", "egui", "overlay", "performance", "windows") } | ConvertTo-Json
Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/topics" -Method Put -Headers $h -Body $topics -ContentType "application/json" | Out-Null
Write-Output "repo description + topics set"

# 2) release
$body = @{
    tag_name         = "v0.1.0"
    name             = "v0.1.0"
    body             = "First tagged release. All 6 phases of the plan plus extras:`n`n- Realtime dashboard (egui): CPU/temp per core, RAM, network, GPU (D3DKMT + NVML), disks`n- Click-through game overlay with global hotkeys (Ctrl+Alt+O / Ctrl+Alt+P)`n- Tray daemon with autostart and flush-before-exit`n- SQLite history with rollups (1m/1h) and retention`n- Named-pipe IPC: daemon streams snapshots to ui/overlay (postcard frames)`n- Process monitor: search, sort CPU/RAM/Disk IO, kill with confirmation`n- TUI terminal dashboard`n`nMeasured performance in README (daemon 0.09% CPU of machine, db ~100 MB steady state).`n`nWindows x64 portable zip attached - unzip and run."
    draft            = $false
    prerelease       = $false
} | ConvertTo-Json
$rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases" -Method Post -Headers $h -Body $body -ContentType "application/json"
Write-Output "release created: $($rel.html_url)"

# 3) upload asset
$zip = "dist\sysmon-0.1.0-windows-x64.zip"
$bytes = [System.IO.File]::ReadAllBytes($zip)
$up = @{ Authorization = "Bearer $token"; Accept = "application/vnd.github+json" }
Invoke-RestMethod -Uri "$($rel.upload_url.Split("{")[0])?name=sysmon-0.1.0-windows-x64.zip" -Method Post -Headers $up -Body $bytes -ContentType "application/zip" | Out-Null
Write-Output "asset uploaded"
