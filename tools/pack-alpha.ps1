# Pack Beautiful-Alpha folder (no zip).
# Usage: powershell -ExecutionPolicy Bypass -File tools/pack-alpha.ps1
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$Alpha = Join-Path $Root "dist\Beautiful-Alpha"
$LinuxOut = Join-Path $Alpha "linux"

New-Item -ItemType Directory -Force -Path $Alpha | Out-Null
New-Item -ItemType Directory -Force -Path $LinuxOut | Out-Null

$Exe = Join-Path $Root "target\release\beautiful.exe"
if (-not (Test-Path $Exe)) {
    $Exe = Join-Path $Root "dist\beautiful.exe"
}
if (-not (Test-Path $Exe)) {
    throw "No Windows beautiful.exe found. Build release first."
}

Copy-Item -Force $Exe (Join-Path $Alpha "Beautiful.exe")
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

$hasLinux = Test-Path (Join-Path $LinuxOut "beautiful")
$linuxNote = if ($hasLinux) {
    @"
Linux / Steam Deck
  Бинарник: linux/beautiful
  Запуск:   linux/run-beautiful.sh
  Нужен Vulkan (Mesa).
"@
} else {
    @"
Linux / Steam Deck
  Пока нет linux/beautiful — собери через WSL:
    wsl -e bash /mnt/c/modding/beautiful/tools/build-linux.sh
  затем снова запусти tools/pack-alpha.ps1
"@
}

@"
Beautiful — alpha
=================

КАК ЗАПУСТИТЬ (Windows)
  1. Дважды кликни START.bat
     или Beautiful.exe

RUST НЕ НУЖЕН
  Это уже собранная программа.

СИСТЕМА (Windows)
  - Windows 10 / 11 (64-bit)
  - Видеокарта с DirectX 12 / Vulkan
  - Стилус: Windows Ink в драйвере

$linuxNote

НАСТРОЙКИ / ГАЛЕРЕЯ
  Windows: %APPDATA%\Beautiful\

Сборка без zip — папка dist\Beautiful-Alpha.

Версия: alpha 0.4.8
"@ | Set-Content -Encoding UTF8 (Join-Path $Alpha "README.txt")

# Prefer folder over zip for distribution
Get-ChildItem (Join-Path $Root "dist") -Filter "Beautiful-*-win.zip" -ErrorAction SilentlyContinue |
    Remove-Item -Force -ErrorAction SilentlyContinue

Write-Host "==> Packed: $Alpha"
Get-ChildItem $Alpha -Recurse | Format-Table Name, Length, LastWriteTime -AutoSize
