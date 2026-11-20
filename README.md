# deruPDF

A small, fast PDF reader that stays on top of other windows. Rust + egui for the window,
PDFium (Chrome's PDF engine) for rendering and search.

## Features

- Always on top (toggle with **Pin on top** or `Ctrl+T`)
- Continuous scroll, renders only visible pages at exact screen resolution
- Find: `Ctrl+F`, `Enter` / `Shift+Enter` (or `F3` / `Shift+F3`) for next/previous, `Esc` to close
- Zoom: `Ctrl +` / `Ctrl -` / `Ctrl+0` (fit width), or `Ctrl` + mouse wheel
- `PageUp` / `PageDown` / `Home` / `End` to jump between pages
- Open with `Ctrl+O`, drag-and-drop, or `deruPDF.exe file.pdf` (so "Open with…" works)

## Build on Windows

1. Install Rust from <https://rustup.rs> (accept the Visual Studio C++ Build Tools prompt).
2. In this folder, fetch PDFium:
   ```powershell
   powershell -ExecutionPolicy Bypass -File get-pdfium.ps1
   ```
   (Or download `pdfium-win-x64.tgz` from <https://github.com/bblanchon/pdfium-binaries/releases>
   and copy `bin\pdfium.dll` next to the exe.)
3. Build and run:
   ```powershell
   cargo run --release
   ```
   The app is `target\release\deruPDF.exe`. To share it, ship the exe together with `pdfium.dll`.

## Linux / macOS

Same steps. Download `pdfium-linux-x64.tgz` or `pdfium-mac-arm64.tgz` / `pdfium-mac-x64.tgz`
from the same releases page and put `lib/libpdfium.so` or `lib/libpdfium.dylib` next to the binary.
On Linux you may need dev packages for the windowing libraries
(e.g. `sudo apt install libxkbcommon-dev libgtk-3-dev`).

Note: on Wayland, compositors decide whether "always on top" is honored; GNOME ignores it
for regular apps. X11, KDE and most tiling WMs respect it.

## Code layout

Everything is in `src/main.rs` (~700 lines):
`App::pages` handles layout, lazy rendering and the texture cache; `App::run_search` uses
PDFium's text search and stores match rectangles for highlighting.

README made by Claude.