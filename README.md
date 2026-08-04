**Language:** English | [Русский](README.ru.md)

# Beautiful

**Beautiful** is an open-source digital painting app.

> **Alpha** — not stable. Expect bugs and unfinished tools. Published on GitHub for **beta testers**.

A **beta** release is planned later.

## Download

Get the latest Windows build from **[Releases](https://github.com/toxmiha/beautiful/releases)**  
(e.g. `Beautiful-0.4.7.exe`). Source archives are attached to each release as usual.

## Status

| | |
|---|---|
| Stage | **Alpha 0.4.7** |
| Stability | Unstable — bugs are expected |
| Tools progress | About **~7%** of planned tools implemented |
| License | [MIT](LICENSE) |
| Audience | Early / beta testers |

## What it is

- A program for **drawing and painting**
- **Full UI customization** — rearrange and personalize the interface
- **Addons** supported
- **Open source** (MIT)

## What’s next

- Finish and rework the **brush system**
- Continue **performance / optimization** work
- Grow the toolset toward a fuller painting workflow
- Move from alpha toward a **beta** release

## Requirements (Windows build)

- Windows 10/11 (64-bit)
- GPU with DirectX 12 / Vulkan
- XP-Pen / Wacom / any Windows Ink or WinTab tablet (optional; mouse works)

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

MIT — see [LICENSE](LICENSE).
