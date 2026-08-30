**Language:** English | [Русский](README.ru.md)

# Beautiful

**Beautiful** is an open-source digital painting app ([GPL-3.0](LICENSE)).

> **Alpha** — not stable. Expect bugs and unfinished tools. Published on GitHub for **beta testers**.

A **beta** release is planned later.

## Download

Get the latest builds from **[Releases](https://github.com/toxmiha/beautiful/releases)**  
Windows: unpack `Beautiful-0.4.9-windows-x64.zip` and run `Beautiful.exe` **from that folder** (CPython DLLs must sit next to the exe — a lone `.exe` will not start). Linux x64 is attached when available. Source archives are attached to each release as usual.

## Status

| | |
|---|---|
| Stage | **Alpha 0.4.9** |
| Stability | Unstable — bugs are expected |
| Tools progress | About **~70%** of planned tools implemented |
| License | [GPL-3.0](LICENSE) |
| Audience | Early / beta testers |

## What it is

- A program for **drawing and painting**
- **Full UI customization** — rearrange and personalize the interface
- **Addons** supported
- **Open source** ([GPL-3.0](LICENSE))

## What’s new in 0.4.9

- Text tool (live layout + raster on the canvas)
- Python add-ons with sidecar CPython (Windows `python3.dll`, Linux `libpython3.12.so`) + Music Player add-on
- Gamepad / Steam Deck input, UI language (i18n)
- Preset library / browser, ABR + brush asset folders
- Display-tile present path (replaces the old full-canvas mip)
- Export Studio, demo record/replay
- PSD / file IO, filters, live transform, and multilayer composite work

## What’s new in 0.4.8

- Brush Engine v2 (opacity/flow, spacing, scatter, dynamics, dual brush path, node editor entry)
- Editable tablet pressure curves + mouse pressure emulation
- Filter Studio (stack filters with live preview, then Apply)
- Discord Rich Presence (session status; NSFW hides canvas name + preview)
- Boot splash progress, optional update check from GitHub Releases
- Paint-path and multilayer composite performance work

## What’s next

- Continue brush and performance work
- Grow the toolset toward a fuller painting workflow
- Move from alpha toward a **beta** release

## Requirements

**Windows**

- Windows 10/11 (64-bit)
- GPU with DirectX 12 / Vulkan
- Pen tablet optional (Windows Ink / WinTab); mouse works

**Linux**

- x86_64, Vulkan (Mesa is fine; Steam Deck / SteamOS compatible builds when provided)

## Build from source

```bash
cargo run -p beautiful-app --release
```

Needs [Rust](https://rustup.rs/) (stable).

| Layer | Tech |
|-------|------|
| Language | Rust 2021 |
| UI | egui + eframe |
| GPU | wgpu |

## Controls

| Action | Key |
|--------|-----|
| Pen tool | `B` |
| Eraser | `E` |
| New layer | `Ctrl+L` |

## License

GNU General Public License v3.0 or later — see [LICENSE](LICENSE).
