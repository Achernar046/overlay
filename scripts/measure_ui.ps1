# Compare overlay vs ui RAM (60s each). ASCII-only.
$u = Start-Process -FilePath "target\release\sysmon-overlay.exe" -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 3
$s = @()
for ($i = 0; $i -lt 6; $i++) {
    Start-Sleep -Seconds 5
    $p = Get-Process -Id $u.Id -ErrorAction SilentlyContinue
    if ($p) { $s += [pscustomobject]@{ pm = $p.PrivateMemorySize64 / 1MB; ws = $p.WorkingSet64 / 1MB } }
}
Stop-Process -Id $u.Id -Force -ErrorAction SilentlyContinue
$pm = $s | Measure-Object pm -Average -Maximum
$ws = $s | Measure-Object ws -Average -Maximum
"overlay RAM: PM avg {0:N1} max {1:N1} | WS avg {2:N1} max {3:N1}" -f $pm.Average, $pm.Maximum, $ws.Average, $ws.Maximum

# rerun ui to check variance
$u2 = Start-Process -FilePath "target\release\sysmon-ui.exe" -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 3
$s2 = @()
for ($i = 0; $i -lt 6; $i++) {
    Start-Sleep -Seconds 5
    $p = Get-Process -Id $u2.Id -ErrorAction SilentlyContinue
    if ($p) { $s2 += [pscustomobject]@{ pm = $p.PrivateMemorySize64 / 1MB; ws = $p.WorkingSet64 / 1MB } }
}
Stop-Process -Id $u2.Id -Force -ErrorAction SilentlyContinue
$pm2 = $s2 | Measure-Object pm -Average -Maximum
$ws2 = $s2 | Measure-Object ws -Average -Maximum
"ui RAM (rerun): PM avg {0:N1} max {1:N1} | WS avg {2:N1} max {3:N1}" -f $pm2.Average, $pm2.Maximum, $ws2.Average, $ws2.Maximum
