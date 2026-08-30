# Pack Beautiful-Alpha folder (no zip).
# Usage: powershell -ExecutionPolicy Bypass -File tools/pack-alpha.ps1
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$Alpha = Join-Path $Root "dist\Beautiful-Alpha"

New-Item -ItemType Directory -Force -Path $Alpha | Out-Null

$Exe = Join-Path $Root "target\release\beautiful.exe"
if (-not (Test-Path $Exe)) {
    $Exe = Join-Path $Root "dist\beautiful.exe"
}
if (-not (Test-Path $Exe)) {
    throw "No Windows beautiful.exe found. Build release first."
}

Copy-Item -Force $Exe (Join-Path $Alpha "Beautiful.exe")
& (Join-Path $PSScriptRoot "ensure-python-embed.ps1") -DestDirs @(
    $Alpha,
    (Join-Path $Root "dist")
)
try {
    Copy-Item -Force $Exe (Join-Path $Root "dist\beautiful.exe")
} catch {
    Write-Warning "dist\beautiful.exe locked (app running?) — Alpha copy is OK"
}

@'
@echo off
cd /d "%~dp0"
start "" "Beautiful.exe"
'@ | Set-Content -Encoding ASCII (Join-Path $Alpha "START.bat")

$LinuxDrop = Join-Path $Root "dist\Beautiful-Linux\beautiful"
$Linux7z = Join-Path $Root "dist\beautiful-linux.7z"
$linuxNote = if (Test-Path $Linux7z) {
    @"
Linux / Steam Deck
  Архив: dist\beautiful-linux.7z
  Внутри папка: beautiful + libpython3.12.so + lib/python3.12.
  Распакуй на Deck и запусти ./beautiful из этой папки.
"@
} elseif (Test-Path $LinuxDrop) {
    @"
Linux / Steam Deck
  Файл: dist\Beautiful-Linux\ (beautiful + libpython*.so)
  На Deck: chmod +x beautiful && ./beautiful
"@
} else {
    @"
Linux / Steam Deck
  Собери: wsl env CARGO_TARGET_DIR=/home/crab3/beautiful-target bash /mnt/c/modding/beautiful/tools/build-linux.sh
"@
}

@"
Beautiful — alpha
=================

КАК ЗАПУСТИТЬ (Windows)
  1. Дважды кликни START.bat
     или Beautiful.exe

RUST / PYTHON НЕ НУЖНЫ
  Это уже собранная программа. CPython — DLL рядом с exe
  (python3.dll), как у Blender. Скидывай всю папку, не один файл.
  ffmpeg не входит в сборку: радио / экспорт MP4 — если ffmpeg в PATH
  или в папке ffmpeg\ рядом с exe.

СИСТЕМА (Windows)
  - Windows 10 / 11 (64-bit)
  - Видеокарта с DirectX 12 / Vulkan
  - Стилус: Windows Ink в драйвере

$linuxNote

НАСТРОЙКИ / ГАЛЕРЕЯ
  Windows: %APPDATA%\Beautiful\

Сборка без zip — папка dist\Beautiful-Alpha.

Версия: alpha 0.4.9
"@ | Set-Content -Encoding UTF8 (Join-Path $Alpha "README.txt")

# Prefer folder over zip for distribution
Get-ChildItem (Join-Path $Root "dist") -Filter "Beautiful-*-win.zip" -ErrorAction SilentlyContinue |
    Remove-Item -Force -ErrorAction SilentlyContinue

Write-Host "==> Packed: $Alpha"
Get-ChildItem $Alpha -Recurse | Format-Table Name, Length, LastWriteTime -AutoSize
