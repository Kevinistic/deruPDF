# Downloads the latest prebuilt PDFium for Windows x64 and puts pdfium.dll
# where `cargo run` and the release build will find it.
# Usage (from the project folder):  powershell -ExecutionPolicy Bypass -File get-pdfium.ps1

$ErrorActionPreference = "Stop"
$url = "https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-win-x64.tgz"
$tmp = Join-Path $env:TEMP "pdfium-download"

New-Item -ItemType Directory -Force $tmp | Out-Null
Write-Host "Downloading $url"
Invoke-WebRequest $url -OutFile "$tmp\pdfium.tgz"
tar -xzf "$tmp\pdfium.tgz" -C $tmp

foreach ($dest in @(".", "target\debug", "target\release")) {
    New-Item -ItemType Directory -Force $dest | Out-Null
    Copy-Item "$tmp\bin\pdfium.dll" $dest -Force
}
Write-Host "pdfium.dll copied to project root, target\debug and target\release."
