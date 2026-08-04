**Language:** English | [Русский](README.ru.md)

# Beautiful

**Beautiful** is an open-source digital painting app.

> **Alpha** — not stable. Expect bugs and unfinished tools. This build is published on GitHub for **beta testers** who want to try early builds and help with feedback.

A **beta** release is planned later.

## Status

| | |
|---|---|
| Stage | **Alpha** |
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

## Stack

| Layer | Tech |
|-------|------|
| Language | Rust 2021 |
| UI | egui + eframe |
| GPU | wgpu (via eframe) |
| Pen input | Windows Ink / egui pointer force (WinTab planned) |

## Requirements

- [Rust toolchain](https://rustup.rs/) (stable)
- Windows 10/11 (primary target)
- XP-Pen / Wacom / any Windows Ink or WinTab tablet

## Run

```bash
cargo run -p beautiful-app
```

Release build (faster canvas updates):

```bash
cargo run -p beautiful-app --release
```

## Controls

| Action | Key |
|--------|-----|
| Pen tool | `B` |
| Eraser | `E` |
| New layer | `Ctrl+L` |

Draw with pen or mouse (left button). Pressure is shown in the status line.

## Project layout

```
crates/
  beautiful-core/   document, layers, brush engine, stabilizer
  beautiful-app/    eframe desktop app
```

More detail: [`ROADMAP.md`](ROADMAP.md).

## License

MIT — see [LICENSE](LICENSE).
