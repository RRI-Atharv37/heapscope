# Builds the release CLI and capture DLL and packs heapscope-<version>.zip into build\dist:
# heapscope.exe, heapscope_capture.dll, include\, docs\, README.md, CHANGELOG.md, LICENSE,
# THIRD-PARTY-NOTICES.txt.
# Prints the SHA-256 that goes into the CI templates (HEAPSCOPE_SHA256).
#   pwsh scripts\package-beta.ps1 [-Version 0.1.0-beta]
param([string]$Version = '0.1.0-beta')
$ErrorActionPreference = 'Continue'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    cargo build --release --workspace --locked
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    $bin = Join-Path $root 'target\x86_64-pc-windows-msvc\release'
    if (-not (Test-Path (Join-Path $bin 'heapscope.exe'))) { $bin = Join-Path $root 'target\release' }

    $stage = Join-Path $root "build\dist\heapscope-$Version"
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Path $stage | Out-Null
    Copy-Item (Join-Path $bin 'heapscope.exe'), (Join-Path $bin 'heapscope_capture.dll') $stage
    Copy-Item -Recurse include, docs $stage
    Copy-Item README.md, CHANGELOG.md, LICENSE, THIRD-PARTY-NOTICES.txt $stage

    $zip = Join-Path $root "build\dist\heapscope-$Version.zip"
    Remove-Item $zip -ErrorAction SilentlyContinue
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
    # The zip must carry the licence files the third-party code requires.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead($zip)
    try { $entries = $archive.Entries | ForEach-Object { $_.FullName } } finally { $archive.Dispose() }
    foreach ($required in 'THIRD-PARTY-NOTICES.txt', 'LICENSE', 'heapscope.exe', 'heapscope_capture.dll') {
        if ($entries -notcontains $required) { throw "$zip does not contain $required" }
    }
    "{0}  {1}" -f (Get-FileHash $zip -Algorithm SHA256).Hash, $zip
}
finally { Pop-Location }
