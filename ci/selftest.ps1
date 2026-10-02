# Runs scr8's self-test on a Windows CI machine (see ci/selftest.sh).
param([string]$Exe, [string]$Out)
$data = Join-Path $env:RUNNER_TEMP "scr8-data"
$shots = Join-Path $data "shots"
New-Item -ItemType Directory -Force $shots, $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$config = @{
  binds = @(@{ id = 1; name = "CI"; hotkey = @{ ctrl = $true; alt = $true; shift = $false; meta = $false; key = "F9" };
               region = @{ x = 100; y = 100; w = 400; h = 300 }; folder = $shots; enabled = $true })
  png_level = "Fast"; autostart = $false; keep_settings_open = $true; next_id = 1
}
$config | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8 (Join-Path $data "config.json")
$env:SCR8_DATA_DIR = $data
$env:SCR8_SELFTEST = $Out
$p = Start-Process -FilePath $Exe -ArgumentList "--hidden" -PassThru
if (-not $p.WaitForExit(120000)) { $p.Kill() }
Copy-Item -Recurse $shots (Join-Path $Out "shots")
Get-Process scr8 -ErrorAction SilentlyContinue | Out-File (Join-Path $Out "processes.txt")
Get-Process scr8 -ErrorAction SilentlyContinue | Stop-Process -Force
"report:"
Get-Content (Join-Path $Out "report.json") -ErrorAction SilentlyContinue
