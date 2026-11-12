# Measure daemon CPU/RAM (release build) + DB growth. ASCII-only for PS 5.1.
$procs = [System.Environment]::ProcessorCount
Write-Output "logical cores: $procs"

$d = Start-Process -FilePath "target\release\sysmon-daemon.exe" -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 5
$c0 = (Get-Process -Id $d.Id).CPU
$t0 = Get-Date
$samples = @()
for ($i = 0; $i -lt 30; $i++) {
    Start-Sleep -Seconds 2
    $p = Get-Process -Id $d.Id -ErrorAction SilentlyContinue
    if (-not $p) { break }
    $samples += [pscustomobject]@{ cpu_sec = $p.CPU; pm = $p.PrivateMemorySize64 / 1MB; ws = $p.WorkingSet64 / 1MB }
}
$t1 = Get-Date
$dt = ($t1 - $t0).TotalSeconds
$cpu_per_core = (($samples[-1].cpu_sec - $c0) / $dt) * 100
$pm = $samples | Measure-Object pm -Average -Maximum
$ws = $samples | Measure-Object ws -Average -Maximum
"daemon CPU: {0:N3}% of 1 core / {1:N4}% of all {2} cores" -f $cpu_per_core, ($cpu_per_core / $procs), $procs
"daemon RAM: PM avg {0:N1} MB max {1:N1} MB | WS avg {2:N1} MB max {3:N1} MB" -f $pm.Average, $pm.Maximum, $ws.Average, $ws.Maximum

# db growth: two points 30s apart
$db1 = (Get-Item sysmon.db).Length
Start-Sleep -Seconds 30
$db2 = (Get-Item sysmon.db).Length
"db: {0:N0} KB -> {1:N0} KB in 30s ({2:N1} KB/min)" -f ($db1 / 1KB), ($db2 / 1KB), ((($db2 - $db1) / 1KB) / 0.5)

Stop-Process -Id $d.Id -Force
Start-Sleep -Seconds 1
Get-ChildItem sysmon.db* | ForEach-Object { "{0}  {1:N0} KB" -f $_.Name, ($_.Length / 1KB) }
