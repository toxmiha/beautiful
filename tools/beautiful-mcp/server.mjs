#!/usr/bin/env node
/**
 * Beautiful Diagnostics MCP
 * Exposes cargo check/test/build so the agent can see compile errors
 * instead of guessing.
 *
 * Logs go to stderr only — stdout is reserved for MCP JSON-RPC.
 */

import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PROJECT_ROOT = path.resolve(__dirname, "..", "..");

/** Cursor MCP processes often miss user PATH — inject rustup bins. */
function toolchainEnv() {
  const home = process.env.USERPROFILE || process.env.HOME || "";
  const cargoBin = path.join(home, ".cargo", "bin");
  const rustupBin = path.join(home, ".rustup", "toolchains");
  const sep = process.platform === "win32" ? ";" : ":";
  const pathParts = [cargoBin, process.env.PATH || ""].filter(Boolean);
  return {
    ...process.env,
    PATH: pathParts.join(sep),
    CARGO_HOME: process.env.CARGO_HOME || path.join(home, ".cargo"),
    RUSTUP_HOME: process.env.RUSTUP_HOME || path.join(home, ".rustup"),
    CARGO_TERM_COLOR: "never",
    RUST_BACKTRACE: "0",
    // silence unused for linters that scan this file
    BEAUTIFUL_RUSTUP_TOOLCHAINS: rustupBin,
  };
}

function cargoExecutable() {
  const home = process.env.USERPROFILE || process.env.HOME || "";
  const candidate = path.join(home, ".cargo", "bin", process.platform === "win32" ? "cargo.exe" : "cargo");
  return candidate;
}

function runCargo(args, { timeoutMs = 180_000 } = {}) {
  return new Promise((resolve) => {
    const cargo = cargoExecutable();
    const child = spawn(cargo, args, {
      cwd: PROJECT_ROOT,
      env: toolchainEnv(),
      shell: false,
      windowsHide: true,
    });

    let stdout = "";
    let stderr = "";
    let killed = false;

    const timer = setTimeout(() => {
      killed = true;
      child.kill();
    }, timeoutMs);

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString("utf8");
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString("utf8");
    });

    child.on("close", (code) => {
      clearTimeout(timer);
      resolve({
        code: killed ? 124 : code ?? 1,
        stdout,
        stderr,
        timedOut: killed,
      });
    });

    child.on("error", (err) => {
      clearTimeout(timer);
      resolve({
        code: 1,
        stdout,
        stderr: `${stderr}\n${err.message}`,
        timedOut: false,
      });
    });
  });
}

function parseCargoJsonMessages(stdout) {
  const diagnostics = [];
  const lines = stdout.split(/\r?\n/).filter(Boolean);

  for (const line of lines) {
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      continue;
    }
    if (msg.reason !== "compiler-message" || !msg.message) continue;

    const m = msg.message;
    const level = m.level; // error | warning | note | help
    if (level !== "error" && level !== "warning") continue;

    const span = (m.spans || []).find((s) => s.is_primary) || m.spans?.[0];
    diagnostics.push({
      level,
      message: m.message,
      code: m.code?.code ?? null,
      file: span?.file_name ?? null,
      line: span?.line_start ?? null,
      column: span?.column_start ?? null,
      endLine: span?.line_end ?? null,
      endColumn: span?.column_end ?? null,
      rendered: m.rendered?.trim() ?? null,
    });
  }

  return diagnostics;
}

function formatDiagnostics(diagnostics) {
  if (!diagnostics.length) return "No compiler errors or warnings.";

  const errors = diagnostics.filter((d) => d.level === "error");
  const warnings = diagnostics.filter((d) => d.level === "warning");

  const lines = [
    `Summary: ${errors.length} error(s), ${warnings.length} warning(s)`,
    "",
  ];

  for (const d of diagnostics) {
    const loc =
      d.file != null
        ? `${d.file}:${d.line ?? "?"}:${d.column ?? "?"}`
        : "(no location)";
    const code = d.code ? ` [${d.code}]` : "";
    lines.push(`${d.level.toUpperCase()}${code} ${loc}`);
    lines.push(`  ${d.message}`);
    if (d.rendered) {
      lines.push(d.rendered);
    }
    lines.push("");
  }

  return lines.join("\n");
}

async function cargoCheck({ package: pkg, release = false } = {}) {
  const args = ["check", "--message-format=json", "--workspace"];
  if (pkg) {
    args.push("-p", pkg);
  }
  if (release) {
    args.push("--release");
  }

  const result = await runCargo(args);
  const diagnostics = parseCargoJsonMessages(result.stdout);
  const text = [
    `command: cargo ${args.join(" ")}`,
    `cwd: ${PROJECT_ROOT}`,
    `exit: ${result.code}${result.timedOut ? " (timed out)" : ""}`,
    "",
    formatDiagnostics(diagnostics),
  ];

  if (result.code !== 0 && diagnostics.length === 0) {
    text.push("", "--- stderr ---", result.stderr.slice(0, 8000));
    if (result.stdout.trim() && !result.stdout.includes('"reason"')) {
      text.push("", "--- stdout ---", result.stdout.slice(0, 4000));
    }
  }

  return {
    content: [{ type: "text", text: text.join("\n") }],
    isError: result.code !== 0,
  };
}

async function cargoTest({ package: pkg, filter } = {}) {
  const args = ["test", "--workspace", "--", "--nocapture"];
  if (pkg) {
    // replace --workspace with -p
    args.splice(1, 1, "-p", pkg);
  }
  if (filter) {
    args.push(filter);
  }

  const result = await runCargo(args, { timeoutMs: 300_000 });
  const combined = [result.stdout, result.stderr].filter(Boolean).join("\n");
  const text = [
    `command: cargo ${args.join(" ")}`,
    `cwd: ${PROJECT_ROOT}`,
    `exit: ${result.code}${result.timedOut ? " (timed out)" : ""}`,
    "",
    combined.slice(0, 12000) || "(no output)",
  ].join("\n");

  return {
    content: [{ type: "text", text }],
    isError: result.code !== 0,
  };
}

async function cargoBuild({ package: pkg, release = false } = {}) {
  const args = ["build", "--message-format=json", "--workspace"];
  if (pkg) {
    args.push("-p", pkg);
  }
  if (release) {
    args.push("--release");
  }

  const result = await runCargo(args, { timeoutMs: 300_000 });
  const diagnostics = parseCargoJsonMessages(result.stdout);
  const text = [
    `command: cargo ${args.join(" ")}`,
    `cwd: ${PROJECT_ROOT}`,
    `exit: ${result.code}${result.timedOut ? " (timed out)" : ""}`,
    "",
    formatDiagnostics(diagnostics),
  ];

  if (result.code !== 0 && diagnostics.length === 0) {
    text.push("", "--- stderr ---", result.stderr.slice(0, 8000));
  }

  return {
    content: [{ type: "text", text: text.join("\n") }],
    isError: result.code !== 0,
  };
}

async function projectInfo() {
  let members = [];
  try {
    const metaResult = await runCargo([
      "metadata",
      "--no-deps",
      "--format-version",
      "1",
    ]);
    const meta = JSON.parse(metaResult.stdout);
    members = (meta.workspace_members || []).map((id) => {
      const name = id.split(" ")[0];
      const pkg = (meta.packages || []).find((p) => p.id === id || p.name === name);
      return {
        name: pkg?.name ?? name,
        version: pkg?.version ?? null,
        path: pkg?.manifest_path ?? null,
      };
    });
  } catch (err) {
    members = [{ error: String(err) }];
  }

  const rustcOut = await new Promise((resolve) => {
    const home = process.env.USERPROFILE || process.env.HOME || "";
    const rustc = path.join(
      home,
      ".cargo",
      "bin",
      process.platform === "win32" ? "rustc.exe" : "rustc",
    );
    const child = spawn(rustc, ["--version"], {
      cwd: PROJECT_ROOT,
      env: toolchainEnv(),
      shell: false,
      windowsHide: true,
    });
    let out = "";
    child.stdout.on("data", (c) => (out += c.toString()));
    child.stderr.on("data", (c) => (out += c.toString()));
    child.on("close", () => resolve(out.trim()));
    child.on("error", (e) => resolve(e.message));
  });

  const cargoVer = await runCargo(["--version"]);

  const text = [
    `project_root: ${PROJECT_ROOT}`,
    `cargo_bin: ${cargoExecutable()}`,
    `rustc: ${rustcOut || "(not found)"}`,
    `cargo: ${cargoVer.stdout.trim() || cargoVer.stderr.trim() || "(not found)"}`,
    "",
    "workspace members:",
    ...members.map((m) =>
      m.error ? `  error: ${m.error}` : `  - ${m.name} ${m.version ?? ""} (${m.path ?? ""})`,
    ),
  ].join("\n");

  return { content: [{ type: "text", text }] };
}

// --- GUI MCP: talk to Beautiful control plane (127.0.0.1) ---

const DEFAULT_MCP_PORT = Number(process.env.BEAUTIFUL_MCP_PORT || 8765);
let appChild = null;
let appPort = DEFAULT_MCP_PORT;

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

async function appCmd(cmd, extra = {}, { timeoutMs = 120_000 } = {}) {
  const body = JSON.stringify({ cmd, ...extra });
  const ac = new AbortController();
  const t = setTimeout(() => ac.abort(), timeoutMs);
  try {
    const res = await fetch(`http://127.0.0.1:${appPort}/cmd`, {
      method: "POST",
      headers: { "Content-Type": "application/json", "Content-Length": String(Buffer.byteLength(body)) },
      body,
      signal: ac.signal,
    });
    const text = await res.text();
    let json;
    try {
      json = JSON.parse(text);
    } catch {
      json = { ok: false, error: text.slice(0, 500) };
    }
    return {
      content: [{ type: "text", text: JSON.stringify(json, null, 2) }],
      isError: json.ok === false,
    };
  } catch (err) {
    return {
      content: [
        {
          type: "text",
          text: JSON.stringify(
            { ok: false, error: String(err?.message || err), port: appPort },
            null,
            2,
          ),
        },
      ],
      isError: true,
    };
  } finally {
    clearTimeout(t);
  }
}

async function appLaunch({ release = true, port } = {}) {
  if (port) appPort = Number(port) || DEFAULT_MCP_PORT;
  const exe = path.join(
    PROJECT_ROOT,
    "dist",
    process.platform === "win32" ? "beautiful.exe" : "beautiful",
  );
  const alt = path.join(
    PROJECT_ROOT,
    "target",
    release ? "release" : "debug",
    process.platform === "win32" ? "beautiful.exe" : "beautiful",
  );
  const bin = (await import("node:fs")).existsSync(exe) ? exe : alt;
  if (!(await import("node:fs")).existsSync(bin)) {
    return {
      content: [
        {
          type: "text",
          text: JSON.stringify(
            { ok: false, error: `exe not found: ${exe} or ${alt}` },
            null,
            2,
          ),
        },
      ],
      isError: true,
    };
  }
  if (appChild && !appChild.killed) {
    try {
      appChild.kill();
    } catch {
      /* ignore */
    }
    appChild = null;
    await sleep(500);
  }
  appChild = spawn(bin, ["--mcp"], {
    cwd: PROJECT_ROOT,
    env: {
      ...process.env,
      BEAUTIFUL_MCP: "1",
      BEAUTIFUL_PERF: "1",
      BEAUTIFUL_MCP_PORT: String(appPort),
    },
    detached: false,
    windowsHide: false,
  });
  appChild.on("exit", () => {
    appChild = null;
  });
  // Wait for ping
  for (let i = 0; i < 60; i++) {
    await sleep(500);
    const ping = await appCmd("ping", {}, { timeoutMs: 2000 });
    if (!ping.isError) {
      return {
        content: [
          {
            type: "text",
            text: JSON.stringify(
              { ok: true, pid: appChild?.pid, port: appPort, bin, ping: JSON.parse(ping.content[0].text) },
              null,
              2,
            ),
          },
        ],
      };
    }
  }
  return {
    content: [
      {
        type: "text",
        text: JSON.stringify(
          { ok: false, error: "timeout waiting for MCP ping", port: appPort, bin },
          null,
          2,
        ),
      },
    ],
    isError: true,
  };
}

async function appQuit() {
  const r = await appCmd("quit", {}, { timeoutMs: 5000 });
  await sleep(300);
  if (appChild && !appChild.killed) {
    try {
      appChild.kill();
    } catch {
      /* ignore */
    }
    appChild = null;
  }
  return r;
}

async function waitIdle({ maxFrames = 40, settleMs = 50 } = {}) {
  const frames = [];
  for (let i = 0; i < maxFrames; i++) {
    await appCmd("wait_frames", { n: 1 });
    await sleep(settleMs);
    const snap = await appCmd("perf_snapshot");
    let j;
    try {
      j = JSON.parse(snap.content[0].text);
    } catch {
      j = { ok: false };
    }
    frames.push({
      pending: j.pending,
      dirty: j.dirty,
      offscreen: j.offscreen,
      frame_ms: j.frame_ms,
    });
    if (j.ok && j.pending === false && j.offscreen === false) {
      return {
        content: [
          {
            type: "text",
            text: JSON.stringify({ ok: true, settled: true, frames: i + 1, last: j }, null, 2),
          },
        ],
      };
    }
  }
  return {
    content: [
      {
        type: "text",
        text: JSON.stringify({ ok: true, settled: false, frames: maxFrames, trail: frames.slice(-5) }, null, 2),
      },
    ],
  };
}

const server = new Server(
  {
    name: "beautiful-diagnostics",
    version: "1.1.0",
  },
  {
    capabilities: {
      tools: {},
    },
  },
);

server.setRequestHandler(ListToolsRequestSchema, async () => ({
  tools: [
    {
      name: "cargo_check",
      description:
        "Run `cargo check` on the Beautiful workspace and return structured Rust compiler errors/warnings with file:line. Use after editing Rust code.",
      inputSchema: {
        type: "object",
        properties: {
          package: {
            type: "string",
            description: "Optional crate name, e.g. beautiful-app or beautiful-core",
          },
          release: {
            type: "boolean",
            description: "If true, check --release profile",
          },
        },
      },
    },
    {
      name: "cargo_build",
      description:
        "Run `cargo build` and return structured compiler diagnostics. Heavier than cargo_check; use when linking errors matter.",
      inputSchema: {
        type: "object",
        properties: {
          package: {
            type: "string",
            description: "Optional crate name",
          },
          release: {
            type: "boolean",
            description: "If true, build --release",
          },
        },
      },
    },
    {
      name: "cargo_test",
      description: "Run `cargo test` and return test output.",
      inputSchema: {
        type: "object",
        properties: {
          package: {
            type: "string",
            description: "Optional crate name",
          },
          filter: {
            type: "string",
            description: "Optional test name filter",
          },
        },
      },
    },
    {
      name: "project_info",
      description:
        "Show Rust/cargo versions and workspace crate members for Beautiful.",
      inputSchema: {
        type: "object",
        properties: {},
      },
    },
    {
      name: "app_launch",
      description:
        "Launch Beautiful with BEAUTIFUL_MCP=1 control plane and wait until ping succeeds.",
      inputSchema: {
        type: "object",
        properties: {
          release: { type: "boolean", description: "Prefer release exe (default true)" },
          port: { type: "number", description: "MCP port (default 8765)" },
        },
      },
    },
    {
      name: "app_quit",
      description: "Ask Beautiful to quit via MCP and kill the process if needed.",
      inputSchema: { type: "object", properties: {} },
    },
    {
      name: "open_document",
      description: "Open a document path in the running Beautiful (txmh/psd/raster).",
      inputSchema: {
        type: "object",
        properties: {
          path: { type: "string", description: "Absolute file path" },
        },
        required: ["path"],
      },
    },
    {
      name: "open_library_match",
      description:
        "Open first library.json entry whose name/path contains query (e.g. gangle).",
      inputSchema: {
        type: "object",
        properties: {
          query: { type: "string" },
        },
        required: ["query"],
      },
    },
    {
      name: "list_layers",
      description: "List layers in the open document.",
      inputSchema: { type: "object", properties: {} },
    },
    {
      name: "set_layer_visible",
      description: "Toggle layer visibility (same path as the eye button).",
      inputSchema: {
        type: "object",
        properties: {
          idx: { type: "number" },
          visible: { type: "boolean" },
        },
        required: ["idx", "visible"],
      },
    },
    {
      name: "toggle_layer_burst",
      description:
        "Rapid eye-click stress: flip one layer visibility N times in one UI tick. sync_each=true runs full composite sync after each flip (simulates fast clicking load).",
      inputSchema: {
        type: "object",
        properties: {
          idx: { type: "number" },
          times: { type: "number", description: "Flips (default 20, max 500)" },
          sync_each: {
            type: "boolean",
            description: "Composite sync after each toggle (default true)",
          },
        },
        required: ["idx"],
      },
    },
    {
      name: "draw_stroke",
      description:
        "Paint a polyline on the canvas in document space for CPU probing. Pass points [[x,y,p],...] or x0/y0/x1/y1/steps.",
      inputSchema: {
        type: "object",
        properties: {
          points: {
            type: "array",
            items: { type: "array", items: { type: "number" } },
          },
          x0: { type: "number" },
          y0: { type: "number" },
          x1: { type: "number" },
          y1: { type: "number" },
          steps: { type: "number" },
          pressure: { type: "number" },
          brush_size: { type: "number" },
          sync: { type: "boolean", description: "sync_display after stroke (default true)" },
        },
      },
    },
    {
      name: "spam_repaint",
      description: "Force N continuous repaint frames (idle wake / hover CPU probe).",
      inputSchema: {
        type: "object",
        properties: {
          n: { type: "number", description: "frames (default 60)" },
        },
      },
    },
    {
      name: "show_profiler",
      description: "Open/close the F12 microprofiler window (Hud mode).",
      inputSchema: {
        type: "object",
        properties: {
          open: { type: "boolean" },
        },
      },
    },
    {
      name: "caps",
      description: "Perf schema + supported cmds/categories/modes (agent discovery).",
      inputSchema: { type: "object", properties: {} },
    },
    {
      name: "bench_begin",
      description:
        "Start a passive Bench run: reset counters, sample memory, Mode=Bench (no HUD wake).",
      inputSchema: {
        type: "object",
        properties: {
          action: { type: "string", description: "Label for the run" },
        },
      },
    },
    {
      name: "bench_end",
      description:
        "Finish Bench run → wall/peak/sticky/categories/memory delta/events (same core as F12).",
      inputSchema: { type: "object", properties: {} },
    },
    {
      name: "perf_snapshot",
      description:
        "Unified perf snapshot (schema v2): categories, ring, events, memory, spans.",
      inputSchema: { type: "object", properties: {} },
    },
    {
      name: "perf_reset",
      description: "Reset microprofiler accumulators (keeps memory baseline).",
      inputSchema: { type: "object", properties: {} },
    },
    {
      name: "wait_idle",
      description: "Wait until composite has no pending/offscreen dirty (or timeout).",
      inputSchema: {
        type: "object",
        properties: {
          maxFrames: { type: "number" },
          settleMs: { type: "number" },
        },
      },
    },
    {
      name: "get_view",
      description: "Zoom, revision, path, screen.",
      inputSchema: { type: "object", properties: {} },
    },
  ],
}));

server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const name = request.params.name;
  const args = request.params.arguments ?? {};

  try {
    switch (name) {
      case "cargo_check":
        return await cargoCheck(args);
      case "cargo_build":
        return await cargoBuild(args);
      case "cargo_test":
        return await cargoTest(args);
      case "project_info":
        return await projectInfo();
      case "app_launch":
        return await appLaunch(args);
      case "app_quit":
        return await appQuit();
      case "open_document":
        return await appCmd("open_path", { path: args.path });
      case "open_library_match":
        return await appCmd("open_library_match", { query: args.query });
      case "list_layers":
        return await appCmd("list_layers");
      case "set_layer_visible":
        return await appCmd("set_layer_visible", {
          idx: args.idx,
          visible: args.visible,
        });
      case "toggle_layer_burst":
        return await appCmd("toggle_layer_burst", {
          idx: args.idx,
          times: args.times,
          sync_each: args.sync_each,
        });
      case "draw_stroke":
        return await appCmd("draw_stroke", {
          points: args.points,
          x0: args.x0,
          y0: args.y0,
          x1: args.x1,
          y1: args.y1,
          steps: args.steps,
          pressure: args.pressure,
          brush_size: args.brush_size,
          sync: args.sync,
        });
      case "spam_repaint":
        return await appCmd("spam_repaint", { n: args.n });
      case "show_profiler":
        return await appCmd("show_profiler", { open: args.open !== false });
      case "caps":
        return await appCmd("caps");
      case "bench_begin":
        return await appCmd("bench_begin", { action: args.action || "bench" });
      case "bench_end":
        return await appCmd("bench_end");
      case "perf_snapshot":
        return await appCmd("perf_snapshot");
      case "perf_reset":
        return await appCmd("perf_reset");
      case "wait_idle":
        return await waitIdle(args);
      case "get_view":
        return await appCmd("get_view");
      default:
        return {
          content: [{ type: "text", text: `Unknown tool: ${name}` }],
          isError: true,
        };
    }
  } catch (err) {
    return {
      content: [
        {
          type: "text",
          text: `MCP tool failed: ${err?.stack || err}`,
        },
      ],
      isError: true,
    };
  }
});

async function main() {
  const transport = new StdioServerTransport();
  await server.connect(transport);
  console.error(`[beautiful-diagnostics] ready · root=${PROJECT_ROOT}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
