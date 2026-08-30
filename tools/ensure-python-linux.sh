#!/usr/bin/env bash
# Cache portable CPython for Linux and copy libpython3.12.so next to Beautiful.
# Same model as Windows python3.dll — the ELF does not contain CPython.
#
# Usage:
#   bash tools/ensure-python-linux.sh
#   bash tools/ensure-python-linux.sh dist/Beautiful-Linux target/x86_64-unknown-linux-gnu/release
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# python-build-standalone install_only_stripped (glibc, x86_64).
REL="${PYTHON_STANDALONE_REL:-20260310}"
PYVER="${PYTHON_STANDALONE_VER:-3.12.13}"
TAR_NAME="cpython-${PYVER}+${REL}-x86_64-unknown-linux-gnu-install_only_stripped.tar.gz"
URL="https://github.com/astral-sh/python-build-standalone/releases/download/${REL}/${TAR_NAME}"
VENDOR="$ROOT/vendor/python-linux"
CACHE="$ROOT/.cache/$TAR_NAME"

mkdir -p "$ROOT/.cache" "$VENDOR"

# Copy a python-build-standalone tree without share/terminfo (case collisions
# and symlinks break on WSL /mnt/c / NTFS).
install_vendor_from() {
  local src="$1"
  mkdir -p "$VENDOR/bin" "$VENDOR/lib" "$VENDOR/include"
  cp -a "$src/bin/." "$VENDOR/bin/"
  if [[ -d "$src/include" ]]; then
    cp -a "$src/include/." "$VENDOR/include/"
  fi
  cp -a "$src/lib"/libpython3.12.so* "$VENDOR/lib/" 2>/dev/null || true
  if [[ -f "$src/lib/libpython3.so" ]]; then
    cp -a "$src/lib/libpython3.so" "$VENDOR/lib/"
  fi
  if [[ -d "$src/lib/pkgconfig" ]]; then
    mkdir -p "$VENDOR/lib/pkgconfig"
    cp -a "$src/lib/pkgconfig/." "$VENDOR/lib/pkgconfig/"
  fi
  if [[ -d "$src/lib/python3.12" ]]; then
    rm -rf "$VENDOR/lib/python3.12"
    cp -a "$src/lib/python3.12" "$VENDOR/lib/python3.12"
  fi
}

need_extract=0
if [[ ! -f "$VENDOR/lib/libpython3.12.so.1.0" ]]; then
  need_extract=1
fi

if [[ "$need_extract" -eq 1 ]]; then
  if [[ ! -s "$CACHE" ]]; then
    echo "==> Downloading $TAR_NAME"
    curl -fsSL "$URL" -o "$CACHE"
  fi
  echo "==> Extracting $TAR_NAME"
  TMP="$(mktemp -d)"
  tar -xzf "$CACHE" -C "$TMP"
  # install_only layout: python/{bin,lib,include,...}
  SRC="$TMP/python"
  if [[ ! -d "$SRC/lib" ]]; then
    echo "error: unexpected archive layout (no python/lib)" >&2
    rm -rf "$TMP"
    exit 1
  fi
  install_vendor_from "$SRC"
  rm -rf "$TMP"
fi

# Unversioned sonames: prefer a real file on DrvFS (ln -s often fails on /mnt/c).
(
  cd "$VENDOR/lib"
  if [[ -f libpython3.12.so.1.0 ]]; then
    if [[ ! -e libpython3.12.so ]]; then
      ln -s libpython3.12.so.1.0 libpython3.12.so 2>/dev/null \
        || cp -f libpython3.12.so.1.0 libpython3.12.so
    fi
    if [[ ! -e libpython3.so ]]; then
      ln -s libpython3.12.so.1.0 libpython3.so 2>/dev/null \
        || cp -f libpython3.12.so.1.0 libpython3.so
    fi
  fi
)

if [[ ! -f "$VENDOR/lib/libpython3.12.so.1.0" ]]; then
  echo "error: vendor/python-linux missing libpython3.12.so.1.0" >&2
  exit 1
fi

copy_file_or_link() {
  local src="$1"
  local dest="$2"
  if [[ -L "$src" ]]; then
    local target
    target="$(readlink "$src")"
    ln -s "$target" "$dest" 2>/dev/null || cp -L "$src" "$dest"
  else
    cp -a "$src" "$dest"
  fi
}

copy_runtime() {
  local dest="$1"
  [[ -z "$dest" ]] && return 0
  mkdir -p "$dest"
  for n in libpython3.12.so.1.0 libpython3.12.so libpython3.so; do
    if [[ -e "$VENDOR/lib/$n" ]]; then
      rm -f "$dest/$n"
      copy_file_or_link "$VENDOR/lib/$n" "$dest/$n"
    fi
  done
  mkdir -p "$dest/lib"
  rm -rf "$dest/lib/python3.12"
  cp -a "$VENDOR/lib/python3.12" "$dest/lib/python3.12"
  echo "==> Python runtime -> $dest"
}

if [[ "$#" -eq 0 ]]; then
  copy_runtime "$ROOT/dist/Beautiful-Linux"
else
  for d in "$@"; do
    copy_runtime "$d"
  done
fi

echo "==> Bundled CPython $PYVER (libpython3.12.so sidecar) ready"
