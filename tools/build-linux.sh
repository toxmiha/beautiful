#!/usr/bin/env bash
# Build Beautiful for SteamOS / Steam Deck (x86_64 Linux + Vulkan).
# Run on Linux (Steam Deck desktop mode, Distrobox, or any x86_64 Ubuntu/Arch).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

TARGET="${TARGET:-x86_64-unknown-linux-gnu}"
OUT_DIR="${OUT_DIR:-dist/Beautiful-Alpha/linux}"

echo "==> Installing Rust target (if needed)"
rustup target add "$TARGET"

# Debian/Ubuntu-ish deps (Steam Deck: use Distrobox Ubuntu, or install via pacman on SteamOS).
if command -v apt-get >/dev/null 2>&1; then
  echo "==> Ensuring build packages (apt)"
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    build-essential pkg-config curl \
    libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxi-dev libxrandr-dev \
    libssl-dev libgtk-3-dev \
    mesa-vulkan-drivers libvulkan-dev
elif command -v pacman >/dev/null 2>&1; then
  echo "==> Ensuring build packages (pacman)"
  sudo pacman -Syu --needed --noconfirm \
    base-devel pkgconf \
    libxkbcommon wayland libx11 libxcursor libxi libxrandr \
    openssl gtk3 vulkan-icd-loader vulkan-radeon mesa
fi

echo "==> cargo build --release --target $TARGET"
cargo build --release -p beautiful-app --target "$TARGET"

mkdir -p "$OUT_DIR"
BIN="target/${TARGET}/release/beautiful"
cp -f "$BIN" "$OUT_DIR/beautiful"
chmod +x "$OUT_DIR/beautiful"

# Lightweight runner for Deck Game Mode / desktop
cat > "$OUT_DIR/run-beautiful.sh" <<'EOF'
#!/usr/bin/env bash
DIR="$(cd "$(dirname "$0")" && pwd)"
export WGPU_BACKEND="${WGPU_BACKEND:-vulkan}"
# Gamescope / Deck: prefer Vulkan
exec "$DIR/beautiful" "$@"
EOF
chmod +x "$OUT_DIR/run-beautiful.sh"

cat > "$OUT_DIR/README.txt" <<'EOF'
Beautiful — Linux / Steam Deck build
===================================

Requires: Vulkan (Mesa/RADV on Deck is fine), glibc.

Desktop mode:
  ./run-beautiful.sh

Steam (non-Steam game):
  Target: /path/to/run-beautiful.sh
  Launch options: WGPU_BACKEND=vulkan %command%

If it fails to start under Gamescope, try desktop mode first.
EOF

echo "==> Done: $OUT_DIR/beautiful"
ls -lh "$OUT_DIR/beautiful"
file "$OUT_DIR/beautiful" || true
