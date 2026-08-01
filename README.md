# Beautiful

Lightweight digital painting app in Rust.

**Status:** Phase A done; save/undo/warp/filters/gallery shipped. Live tracker: [`ROADMAP.md`](ROADMAP.md).

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

Draw with pen or mouse (left button). Pressure bar shown in the status line.

## Roadmap (high level)

- [x] Phase A — canvas, brush, stabilizer, layers, UI
- [x] Phase B — save/load (TXMH/PNG/JPEG/PSD), undo _(WinTab still open)_
- [ ] Phase C — animation timeline + onion skin
- [x] Phase D — mesh / distort / free transform _(polish)_
- [~] Phase E — custom features _(see `ROADMAP.md` P0–P4)_

## Project layout

```
crates/
  beautiful-core/   document, layers, brush engine, stabilizer
  beautiful-app/    eframe desktop app
```

## Diagnostics MCP

Project MCP server `beautiful-diagnostics` lets the agent run compile checks without guessing.

| Tool | What it does |
|------|----------------|
| `cargo_check` | Structured Rust errors with `file:line` |
| `cargo_build` | Full build + linker errors |
| `cargo_test` | Unit tests |
| `project_info` | Toolchain + workspace crates |

Config: `.cursor/mcp.json`  
Server: `tools/beautiful-mcp/server.mjs`

After adding/changing MCP config, reload MCP in Cursor (**Settings → MCP → refresh**, or restart Cursor).

## Tablet setup (XP-Pen Artist 16 Pro Gen 2)

1. Install the latest driver from [xp-pen.com](https://www.xp-pen.com/)
2. Enable **Windows Ink** in driver settings if pressure is missing
3. Calibrate the display in the driver panel
4. Map tablet buttons to `B`, `E`, `Ctrl+Z` in the driver
