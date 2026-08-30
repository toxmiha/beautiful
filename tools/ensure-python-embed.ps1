# Cache official Windows embeddable CPython and copy it next to Beautiful.exe.
# PyO3 links python3.dll at load time — without these files Windows shows
# "python3.dll was not found" before main(). Same model as Blender: ship CPython.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File tools/ensure-python-embed.ps1
#   powershell -ExecutionPolicy Bypass -File tools/ensure-python-embed.ps1 -DestDirs dist, target\release
param(
    [string[]]$DestDirs = @()
)

$ErrorActionPreference = "Stop"
$Version = "3.12.10"
$ZipName = "python-$Version-embed-amd64.zip"
$Url = "https://www.python.org/ftp/python/$Version/$ZipName"
$Root = Split-Path -Parent $PSScriptRoot
$Vendor = Join-Path $Root "vendor\python-windows"
$ZipPath = Join-Path $Vendor $ZipName

New-Item -ItemType Directory -Force -Path $Vendor | Out-Null

if (-not (Test-Path (Join-Path $Vendor "python3.dll"))) {
    if (-not (Test-Path $ZipPath)) {
        Write-Host "==> Downloading $ZipName"
        Invoke-WebRequest -Uri $Url -OutFile $ZipPath -UseBasicParsing
    }
    Write-Host "==> Extracting $ZipName"
    Expand-Archive -Path $ZipPath -DestinationPath $Vendor -Force
}

if (-not (Test-Path (Join-Path $Vendor "python3.dll"))) {
    throw "embeddable CPython missing python3.dll in $Vendor"
}

function Copy-PythonRuntime([string]$Dest) {
    if (-not $Dest) { return }
    New-Item -ItemType Directory -Force -Path $Dest | Out-Null
    Get-ChildItem $Vendor -File | ForEach-Object {
        if ($_.Name -in @("python.exe", "pythonw.exe", $ZipName)) { return }
        $name = if ($_.Name -eq "LICENSE.txt") { "PYTHON-LICENSE.txt" } else { $_.Name }
        Copy-Item -Force $_.FullName (Join-Path $Dest $name)
    }
}

if ($DestDirs.Count -eq 0) {
    $DestDirs = @(
        (Join-Path $Root "dist"),
        (Join-Path $Root "target\release")
    )
} else {
    # Nested powershell / cargo may pass one comma-separated string.
    $DestDirs = @($DestDirs | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim().Trim('"') } | Where-Object { $_ })
}

foreach ($d in $DestDirs) {
    Copy-PythonRuntime $d
    Write-Host "==> Python runtime -> $d"
}

Write-Host "==> Bundled CPython $Version ready"
