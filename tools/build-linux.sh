#!/usr/bin/env bash
# Build Beautiful for SteamOS / Steam Deck (x86_64 Linux + Vulkan).
# Host may be a *newer* glibc (e.g. Ubuntu 26.04 / 2.43). We link with Zig
# against glibc 2.35 so the ELF runs on SteamOS 3.x (glibc 2.37–2.41) and
# Ubuntu 22.04+. Do not cargo-build on the host toolchain — that embeds GLIBC_2.43.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

TARGET="${TARGET:-x86_64-unknown-linux-gnu}"
# Minimum glibc we claim. SteamOS 3.7 is 2.41; 2.35 matches Ubuntu 22.04 / GitHub CI.
GLIBC_MIN="${GLIBC_MIN:-2.35}"
# User-facing drop: one ELF at archive root (not linux/linux/beautiful).
OUT_DIR="${OUT_DIR:-dist/Beautiful-Linux}"
ARCHIVE="${ARCHIVE:-dist/beautiful-linux.7z}"

if [[ -f "$HOME/.cargo/env" ]]; then
  # shellcheck disable=SC1090
  . "$HOME/.cargo/env"
fi

echo "==> Installing Rust target (if needed)"
rustup target add "$TARGET"

# Debian/Ubuntu-ish headers (link is Zig; we only need pkg-config + C headers).
# gilrs → libudev-sys needs libudev.pc; alsa/x11/wayland are usually already present.
if command -v apt-get >/dev/null 2>&1; then
  need=()
  dpkg -s libasound2-dev >/dev/null 2>&1 || need+=(libasound2-dev)
  dpkg -s pkg-config >/dev/null 2>&1 || need+=(pkg-config)
  dpkg -s libssl-dev >/dev/null 2>&1 || need+=(libssl-dev)
  dpkg -s libudev-dev >/dev/null 2>&1 || need+=(libudev-dev)
  if ((${#need[@]})); then
    echo "==> Ensuring build packages (apt): ${need[*]}"
    if command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
      sudo DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${need[@]}"
    elif [[ " ${need[*]} " == *" libudev-dev "* ]] && command -v apt-get >/dev/null 2>&1; then
      echo "==> no passwordless sudo — extracting libudev-dev into ~/.local"
      DEP="${UDEV_PREFIX:-$HOME/.local/beautiful-linux-deps}"
      mkdir -p "$DEP" /tmp/beautiful-debs
      ( cd /tmp/beautiful-debs && apt-get download libudev-dev )
      dpkg-deb -x /tmp/beautiful-debs/libudev-dev_*.deb "$DEP"
      PC="$(find "$DEP" -name libudev.pc -print -quit)"
      if [[ -z "$PC" ]]; then
        echo "error: libudev.pc missing after extracting libudev-dev" >&2
        exit 1
      fi
      # .pc from the deb points at /usr; rewrite to the extracted prefix.
      PREFIX="$(cd "$(dirname "$PC")/../../.." && pwd)"
      # debian multiarch: .../usr/lib/x86_64-linux-gnu/pkgconfig → prefix = .../usr
      if [[ "$PC" == *"/usr/lib/"* ]]; then
        PREFIX="$(cd "$(dirname "$PC")/../../.." && pwd)"
      fi
      cat > "$PC" <<EOF
prefix=$PREFIX
exec_prefix=\${prefix}
libdir=\${prefix}/lib/x86_64-linux-gnu
includedir=\${prefix}/include

Name: libudev
Description: udev library (user-prefix extract)
Version: 1
Libs: -L\${libdir} -ludev
Cflags: -I\${includedir}
EOF
      # Unversioned .so for the linker; the real SONAME lives on the system.
      LIBDIR="$PREFIX/lib/x86_64-linux-gnu"
      mkdir -p "$LIBDIR"
      if [[ ! -e "$LIBDIR/libudev.so" ]]; then
        if [[ -f /usr/lib/x86_64-linux-gnu/libudev.so.1 ]]; then
          ln -sf /usr/lib/x86_64-linux-gnu/libudev.so.1 "$LIBDIR/libudev.so"
        elif [[ -f /lib/x86_64-linux-gnu/libudev.so.1 ]]; then
          ln -sf /lib/x86_64-linux-gnu/libudev.so.1 "$LIBDIR/libudev.so"
        fi
      fi
      export PKG_CONFIG_PATH="${LIBDIR}/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
      export LIBRARY_PATH="${LIBDIR}${LIBRARY_PATH:+:$LIBRARY_PATH}"
      remaining=()
      for p in "${need[@]}"; do
        [[ "$p" == libudev-dev ]] && continue
        remaining+=("$p")
      done
      if ((${#remaining[@]})); then
        echo "Missing packages and no passwordless sudo: ${remaining[*]}" >&2
        echo "Install as root, then re-run." >&2
        exit 1
      fi
    else
      echo "Missing packages and no passwordless sudo: ${need[*]}" >&2
      echo "Install as root, then re-run." >&2
      exit 1
    fi
  fi
fi

# Re-use a previous user-prefix libudev even when the package list is otherwise complete.
if ! pkg-config --exists libudev 2>/dev/null; then
  DEP="${UDEV_PREFIX:-$HOME/.local/beautiful-linux-deps}"
  PC="$(find "$DEP" -name libudev.pc -print -quit 2>/dev/null || true)"
  if [[ -n "$PC" ]]; then
    export PKG_CONFIG_PATH="$(dirname "$PC")${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    export LIBRARY_PATH="$(cd "$(dirname "$PC")/.." && pwd)${LIBRARY_PATH:+:$LIBRARY_PATH}"
  fi
fi
if ! pkg-config --exists libudev 2>/dev/null; then
  echo "error: libudev not visible to pkg-config (need libudev-dev)" >&2
  exit 1
fi
echo "==> libudev: $(pkg-config --modversion libudev) ($(pkg-config --libs libudev))"

ZIG_VER="${ZIG_VER:-0.14.1}"
ZIG_DIR="${ZIG_DIR:-$HOME/.local/zig}"
if [[ ! -x "$ZIG_DIR/zig" ]]; then
  echo "==> Installing Zig $ZIG_VER (glibc versioned linker)"
  mkdir -p "$HOME/.local"
  ZIG_TAR="${ZIG_TAR:-$ROOT/.cache/zig-x86_64-linux-${ZIG_VER}.tar.xz}"
  if [[ ! -s "$ZIG_TAR" ]]; then
    mkdir -p "$(dirname "$ZIG_TAR")"
    curl -fsSL "https://ziglang.org/download/${ZIG_VER}/zig-x86_64-linux-${ZIG_VER}.tar.xz" -o "$ZIG_TAR"
  fi
  rm -rf "$ZIG_DIR" "$HOME/.local/zig-x86_64-linux-${ZIG_VER}"
  tar -xJf "$ZIG_TAR" -C "$HOME/.local"
  mv "$HOME/.local/zig-x86_64-linux-${ZIG_VER}" "$ZIG_DIR"
fi
export PATH="$ZIG_DIR:$PATH"
echo "==> zig $(zig version)"

if ! command -v cargo-zigbuild >/dev/null 2>&1; then
  echo "==> cargo install cargo-zigbuild"
  cargo install --locked cargo-zigbuild
fi

echo "==> sidecar CPython (libpython3.12.so, not inside the ELF)"
bash "$ROOT/tools/ensure-python-linux.sh"

if [[ "${SKIP_PYTHON:-0}" == "1" ]]; then
  echo "==> SKIP_PYTHON=1 — zigbuild --no-default-features (no add-ons)"
  PY_FLAGS=(--no-default-features)
else
  export PYO3_PYTHON="${PYO3_PYTHON:-$ROOT/vendor/python-linux/bin/python3}"
  if [[ ! -x "$PYO3_PYTHON" ]]; then
    echo "error: PYO3_PYTHON not executable: $PYO3_PYTHON" >&2
    echo "re-run tools/ensure-python-linux.sh or set SKIP_PYTHON=1" >&2
    exit 1
  fi
  PY_FLAGS=()
  echo "==> PYO3_PYTHON=$PYO3_PYTHON"
fi

echo "==> cargo zigbuild --release (glibc ${GLIBC_MIN}, python sidecar .so)"
# Suffix .${GLIBC_MIN} is cargo-zigbuild's glibc pin — required on Ubuntu 26.04 hosts.
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  cargo zigbuild --release -p beautiful-app "${PY_FLAGS[@]}" \
    --target "${TARGET}.${GLIBC_MIN}"
  BIN="${CARGO_TARGET_DIR}/${TARGET}/release/beautiful"
else
  cargo zigbuild --release -p beautiful-app "${PY_FLAGS[@]}" \
    --target "${TARGET}.${GLIBC_MIN}"
  BIN="target/${TARGET}/release/beautiful"
fi

if [[ ! -f "$BIN" ]]; then
  echo "error: expected ELF at $BIN" >&2
  exit 1
fi
chmod +x "$BIN"

max_glibc() {
  local f="$1"
  objdump -T "$f" 2>/dev/null | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sed 's/GLIBC_//' | sort -V | tail -1
}

MAX_VER="$(max_glibc "$BIN")"
echo "==> max GLIBC symbol: ${MAX_VER:-unknown}"
if [[ -z "$MAX_VER" ]]; then
  echo "warning: could not read GLIBC versions (stripped?)" >&2
else
  # 2.35 <= SteamOS 3.5+ and Ubuntu 22.04. Reject host leakage (2.42/2.43).
  highest="$(printf '%s\n%s\n' "$MAX_VER" "2.39" | sort -V | tail -1)"
  if [[ "$highest" != "2.39" ]]; then
    echo "error: ELF requires GLIBC_$MAX_VER — too new for SteamOS (need <= 2.39)" >&2
    objdump -T "$BIN" | grep GLIBC_ | sort -u | tail -20 >&2
    exit 1
  fi
fi

if objdump -p "$BIN" | grep -qi 'NEEDED.*python'; then
  echo "==> ELF NEEDED libpython (sidecar .so — expected)"
elif [[ "${SKIP_PYTHON:-0}" != "1" ]]; then
  echo "warning: ELF has no NEEDED libpython; add-ons need tools/ensure-python-linux.sh beside the binary" >&2
fi

mkdir -p "$OUT_DIR"
rm -f "$OUT_DIR/beautiful" "$OUT_DIR/run-beautiful.sh" "$OUT_DIR/README.txt"
cp -f "$BIN" "$OUT_DIR/beautiful"
chmod +x "$OUT_DIR/beautiful"

if [[ "${SKIP_PYTHON:-0}" != "1" ]]; then
  bash "$ROOT/tools/ensure-python-linux.sh" "$OUT_DIR"
fi

# Folder drop: ELF + libpython3.12.so + lib/python3.12 (not a lone binary).
mkdir -p "$(dirname "$ARCHIVE")"
STAGE="$(mktemp -d)"
cp -a "$OUT_DIR/." "$STAGE/"
chmod +x "$STAGE/beautiful"
rm -f "$ARCHIVE"
( cd "$STAGE" && 7z a -t7z -mx=9 "$ROOT/$ARCHIVE" . >/dev/null )
rm -rf "$STAGE"

echo "==> Done"
echo "    dir:     $OUT_DIR  (beautiful + libpython*.so + lib/python3.12)"
echo "    archive: $ARCHIVE"
ls -lh "$OUT_DIR/beautiful" "$ARCHIVE"
file "$OUT_DIR/beautiful" || true
echo "==> NEEDED"
objdump -p "$OUT_DIR/beautiful" | grep NEEDED || true
echo "==> copy $ARCHIVE to the Deck, extract the folder, run ./beautiful"
