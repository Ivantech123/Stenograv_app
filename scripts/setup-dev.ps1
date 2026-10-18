# Готовит машину к сборке Стенографа. Ничего не ставит в систему: всё в tools/.
#   libclang   — bindgen для whisper-rs (pip-пакет libclang, ~25 МБ)
#   Vulkan SDK — только для сборки с видеокартой (copy_only, ~300 МБ)
# Нужны заранее: Rust, Node.js, Python, Visual Studio Build Tools 2022 (C++), CMake.
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$tools = Join-Path $root "tools"
New-Item -ItemType Directory -Force -Path $tools | Out-Null

$libclang = Join-Path $tools "libclang\clang\native\libclang.dll"
if (-not (Test-Path $libclang)) {
    Write-Host "libclang..."
    python -m pip install --quiet --target (Join-Path $tools "libclang") libclang
}

$vulkan = Join-Path $tools "vulkan\Bin\glslc.exe"
if (-not (Test-Path $vulkan)) {
    Write-Host "Vulkan SDK (copy_only)..."
    $exe = Join-Path $tools "vulkan_sdk.exe"
    curl.exe -L --fail -o $exe "https://sdk.lunarg.com/sdk/download/latest/windows/vulkan_sdk.exe"
    & $exe --root (Join-Path $tools "vulkan") --accept-licenses --default-answer --confirm-command install copy_only=1
    Remove-Item $exe -Force
}

Push-Location $root
npm install --silent
Pop-Location

Write-Host ""
Write-Host "Готово. Сборка установщика (с видеокартой):"
Write-Host '  $env:CARGO_TARGET_DIR = "C:\stg"; npm run build:gpu'
Write-Host "Короткий CARGO_TARGET_DIR обязателен: шейдеры Vulkan собираются во вложенном"
Write-Host "CMake-проекте, и из папки проекта путь превышает лимит Windows в 260 символов."
