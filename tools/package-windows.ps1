# Windows: портативний .zip (slicenv.exe, slicenv-mcp.exe, ліцензії) і, якщо є Inno Setup
# (iscc), інсталятор .exe — у dist\. Без підпису (див. packaging\README.md).
# Нічого не публікує й не встановлює. CI: .github/workflows/package-cross.yml.
#   pwsh tools/package-windows.ps1              # cargo build --release + пакування
#   $env:SLICENV_SKIP_BUILD=1; pwsh tools/package-windows.ps1
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Root = Resolve-Path (Join-Path $PSScriptRoot '..')
$Dist = if ($env:DIST) { $env:DIST } else { Join-Path $Root 'dist' }
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root 'target' }
$Version = (Select-String -Path (Join-Path $Root 'Cargo.toml') -Pattern '^version = "(.+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$Arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
$Base = "slicenv-$Version-$Arch-windows"

if (-not $env:SLICENV_SKIP_BUILD) {
    Write-Host '==> cargo build --release (slicenv, slicenv-mcp)'
    Push-Location $Root
    cargo build --release --locked -p app -p mcp
    if ($LASTEXITCODE -ne 0) { throw "cargo build: $LASTEXITCODE" }
    Pop-Location
}
$Bin = Join-Path $TargetDir 'release'
foreach ($b in 'slicenv.exe', 'slicenv-mcp.exe') {
    if (-not (Test-Path (Join-Path $Bin $b))) { throw "немає $Bin\$b" }
}

$Stage = Join-Path $Dist $Base
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
Copy-Item (Join-Path $Bin 'slicenv.exe'), (Join-Path $Bin 'slicenv-mcp.exe') $Stage
Copy-Item (Join-Path $Root 'LICENSE') (Join-Path $Stage 'LICENSE.txt')
Copy-Item (Join-Path $Root 'THIRD-PARTY-NOTICES.md') $Stage
Copy-Item (Join-Path $Root 'crates\app\assets\app\slicenv.svg') $Stage
@"
Slicenv $Version (портативна збірка, без підпису)

slicenv.exe      — застосунок
slicenv-mcp.exe  — MCP-сервер (stdio)

Налаштування: %APPDATA%\slicenv, дані й кеш: %LOCALAPPDATA%\slicenv.
Рушій: встановіть Bambu Studio або OrcaSlicer звичайним інсталятором — Slicenv
знайде його в Program Files або в PATH; інакше задайте BAMBU_STUDIO / ORCA_SLICER.
SmartScreen попередить про невідомого видавця: «Докладніше» → «Однаково запустити».
"@ | Set-Content -Encoding utf8 (Join-Path $Stage 'README.txt')

$Zip = Join-Path $Dist "$Base.zip"
if (Test-Path $Zip) { Remove-Item -Force $Zip }
Write-Host "==> zip → $Zip"
Compress-Archive -Path $Stage -DestinationPath $Zip
(Get-FileHash -Algorithm SHA256 $Zip).Hash.ToLower() + "  $Base.zip" |
    Set-Content -Encoding ascii "$Zip.sha256"

# Inno Setup (є на образах windows-latest GitHub Actions). Без нього — лише zip.
$Iscc = Get-Command iscc -ErrorAction SilentlyContinue
if (-not $Iscc) {
    $Candidate = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
    if (Test-Path $Candidate) { $Iscc = Get-Item $Candidate }
}
if ($Iscc) {
    Write-Host '==> Inno Setup → інсталятор'
    & $Iscc.Source "/DAppVersion=$Version" "/DSourceDir=$Stage" "/DOutputDir=$Dist" `
        "/DOutputBase=$Base-setup" (Join-Path $Root 'packaging\windows\slicenv.iss')
    if ($LASTEXITCODE -ne 0) { throw "iscc: $LASTEXITCODE" }
    $Setup = Join-Path $Dist "$Base-setup.exe"
    (Get-FileHash -Algorithm SHA256 $Setup).Hash.ToLower() + "  $Base-setup.exe" |
        Set-Content -Encoding ascii "$Setup.sha256"
} else {
    Write-Host 'попередження: Inno Setup не знайдено — лише портативний zip'
}
Write-Host "==> готово: $Dist"
