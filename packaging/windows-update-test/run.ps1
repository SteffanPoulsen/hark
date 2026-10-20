# kuulo's Windows update test, started by run-test.cmd beside it. It installs the 0.0.1 test
# build, serves the signed 0.0.2 setup on localhost as an update feed, and walks through two
# updates: one from the window, and one from kuulo-cli, which has to refuse while the window
# is open and install once it is closed. Then it offers to uninstall. What it sees and what
# you answer go to report-<time>.txt beside this script.
#
# To make the kit, copy this folder to a USB stick and add the test versions from the release
# workflow's version input: the 0.0.1 setup in install-0.0.1\, and the 0.0.2 setup with its
# .minisig in feed\.
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$app = Join-Path $env:LOCALAPPDATA 'Programs\kuulo'
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\dk.steffanpoulsen.kuulo_is1'
$feedUrl = 'http://localhost:8765/latest.json'
$report = Join-Path $here ('report-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.txt')

function Note([string]$text) {
    Write-Host $text
    Add-Content -LiteralPath $report -Value $text
}
function Step([string]$title) {
    Note ''
    Note "== $title"
}
function Answer([string]$question) {
    $reply = Read-Host $question
    Add-Content -LiteralPath $report -Value "$question`r`n  answer: $reply"
    $reply
}
function Installed { (Get-ItemProperty -Path $uninstallKey -ErrorAction SilentlyContinue).DisplayVersion }
function State {
    Note "installed version (Apps list): $(Installed)"
    if (Test-Path "$app\kuulo-cli.exe") { Note "kuulo-cli version: $(& "$app\kuulo-cli.exe" version)" }
    if (Test-Path "$app\kuulo.exe") { Note "kuulo.exe file version: $((Get-Item "$app\kuulo.exe").VersionInfo.FileVersion)" }
    Note "kuulo windows running: $(@(Get-Process kuulo -ErrorAction SilentlyContinue).Count)"
    $userPath = (Get-ItemProperty -Path 'HKCU:\Environment').Path
    Note "times kuulo's folder is on the user PATH: $(@($userPath -split ';' | Where-Object { $_ -ieq $app }).Count)"
}
function InstallOld {
    Note 'Installing kuulo 0.0.1 with its setup (/SILENT)...'
    Start-Process -Wait -FilePath (Join-Path $here 'install-0.0.1\kuulo-0.0.1-windows-x86_64-setup.exe') -ArgumentList '/SILENT', '/NORESTART'
}
function CloseKuulo {
    foreach ($p in @(Get-Process kuulo -ErrorAction SilentlyContinue)) { [void]$p.CloseMainWindow() }
    for ($i = 0; $i -lt 20 -and (Get-Process kuulo -ErrorAction SilentlyContinue); $i++) { Start-Sleep -Milliseconds 500 }
    Get-Process kuulo -ErrorAction SilentlyContinue | Stop-Process -Force
}
function WaitFor([scriptblock]$done, [int]$seconds) {
    for ($i = 0; $i -lt $seconds -and -not (& $done); $i++) { Start-Sleep 1 }
}
# Runs kuulo-cli with its output in files: read straight from a native program, Windows
# PowerShell 5.1 turns error output into errors that stop this script.
function RunKuuloCli([string]$arguments) {
    $out = Join-Path $env:TEMP 'kuulo-cli-out.txt'
    $err = Join-Path $env:TEMP 'kuulo-cli-err.txt'
    $process = Start-Process -FilePath "$app\kuulo-cli.exe" -ArgumentList $arguments -NoNewWindow -Wait -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
    foreach ($line in @(Get-Content $out) + @(Get-Content $err)) { if ($line) { Note "  kuulo-cli: $line" } }
    Note "  kuulo-cli exit code: $($process.ExitCode)"
}

New-Item -ItemType File -Path $report -Force | Out-Null
Step 'This machine'
$nt = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
Note "Windows $($nt.DisplayVersion), build $($nt.CurrentBuild).$($nt.UBR)"
$sac = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy' -ErrorAction SilentlyContinue).VerifiedAndReputablePolicyState
$sacText = switch ($sac) { 0 { 'off' } 1 { 'on' } 2 { 'evaluation' } default { 'not reported' } }
Note "Smart App Control: $sacText"
Note "kuulo installed before the test: $(Installed)"
if (Get-Process kuulo -ErrorAction SilentlyContinue) { Read-Host 'kuulo is running; close it, then press Enter' | Out-Null }

Step 'Serving the 0.0.2 update on localhost:8765'
$server = Start-Process powershell -WindowStyle Hidden -PassThru -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$(Join-Path $here 'serve.ps1')`"", '-Root', "`"$(Join-Path $here 'feed')`""
try {
    $offer = $null
    for ($i = 0; $i -lt 20 -and -not $offer; $i++) {
        try { $offer = Invoke-RestMethod -Uri $feedUrl -UseBasicParsing } catch { Start-Sleep -Milliseconds 500 }
    }
    if (-not $offer) { throw 'the feed did not start on localhost:8765' }
    Note "The feed offers $($offer.tag_name)."

    Step '1 of 2: install from the window'
    InstallOld
    State
    $env:KUULO_UPDATE_FEED = $feedUrl
    Start-Process (Join-Path $app 'kuulo.exe') -ArgumentList '--settings', 'about'
    Note 'kuulo 0.0.1 is opening on Settings > About.'
    Note "Click Check for updates, then Install 0.0.2. kuulo should close, then open again by itself with its What's new card."
    Read-Host 'Press Enter once kuulo is back, or once it clearly is not coming back' | Out-Null
    WaitFor { (Installed) -eq '0.0.2' } 30
    State
    Answer "Did kuulo come back by itself, showing What's new with 'Updated from 0.0.1'? Did anything else appear on the way? (describe)" | Out-Null
    CloseKuulo

    Step '2 of 2: install from kuulo-cli'
    InstallOld
    State
    Start-Process (Join-Path $app 'kuulo.exe')
    Read-Host 'kuulo 0.0.1 is opening. Press Enter once its window is up' | Out-Null
    Note 'Running kuulo-cli update with the window open. It should refuse and ask for kuulo to be closed.'
    RunKuuloCli 'update'
    State
    Answer 'Is the kuulo window still open and working, with nothing else on screen? (describe)' | Out-Null
    Note 'Closing kuulo, then running kuulo-cli update again. It should install with nothing on screen.'
    CloseKuulo
    RunKuuloCli 'update'
    WaitFor { (Installed) -eq '0.0.2' } 90
    State
    Answer 'Did any message or window appear this time, such as files in use? (describe)' | Out-Null

    Step 'Uninstall'
    if ((Answer 'Uninstall kuulo now? (y/n)') -eq 'y') {
        Start-Process -Wait -FilePath (Join-Path $app 'unins000.exe') -ArgumentList '/VERYSILENT'
        WaitFor { -not (Test-Path (Join-Path $app 'kuulo.exe')) } 30
        State
        Note "kuulo's folder still there: $(Test-Path $app)"
        Note "Start menu entry still there: $(Test-Path (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\kuulo.lnk'))"
    }
    Note ''
    Note 'Done.'
} catch {
    Note "The test stopped: $_"
} finally {
    Stop-Process -Id $server.Id -ErrorAction SilentlyContinue
    Note "Report: $report"
}
