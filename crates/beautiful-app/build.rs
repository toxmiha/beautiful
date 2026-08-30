//! Copy sidecar CPython next to the binary (never statically in the exe).
//! Windows: python3.dll (embeddable). Linux: libpython3.12.so + lib/python3.12/.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_PYTHON");
    if env::var("CARGO_FEATURE_PYTHON").is_err() {
        return;
    }

    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo = manifest.join("../..");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let bin_dir = out.join("../../..");

    match os.as_str() {
        "windows" => windows_sidecar(&repo, &bin_dir),
        "linux" => linux_sidecar(&repo, &bin_dir),
        _ => {}
    }
}

fn windows_sidecar(repo: &Path, bin_dir: &Path) {
    let vendor = repo.join("vendor/python-windows");
    let dll = vendor.join("python3.dll");
    println!("cargo:rerun-if-changed={}", dll.display());

    if !dll.is_file() {
        let script = repo.join("tools/ensure-python-embed.ps1");
        println!("cargo:warning=fetching bundled CPython into vendor/python-windows");
        let status = Command::new("powershell")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                script.to_str().expect("utf-8 path"),
                "-DestDirs",
                vendor.to_str().expect("utf-8 path"),
            ])
            .status();
        if !matches!(status, Ok(s) if s.success()) || !dll.is_file() {
            println!(
                "cargo:warning=python3.dll missing — cargo run will need tools/ensure-python-embed.ps1"
            );
            return;
        }
    }

    if let Err(e) = copy_windows_runtime(&vendor, bin_dir) {
        println!("cargo:warning=could not copy bundled CPython next to exe: {e}");
    }
}

fn copy_windows_runtime(vendor: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(vendor)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == "python.exe"
            || name_str == "pythonw.exe"
            || name_str.ends_with("-embed-amd64.zip")
        {
            continue;
        }
        let dest_name = if name_str == "LICENSE.txt" {
            "PYTHON-LICENSE.txt".into()
        } else {
            name
        };
        fs::copy(&path, dest.join(dest_name))?;
    }
    Ok(())
}

fn linux_sidecar(repo: &Path, bin_dir: &Path) {
    // Loader finds libpython next to the ELF (same idea as python3.dll).
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");

    let vendor = repo.join("vendor/python-linux");
    let lib = vendor.join("lib");
    let so = lib.join("libpython3.12.so.1.0");
    println!("cargo:rerun-if-changed={}", so.display());

    if so.is_file() {
        println!("cargo:rustc-link-search=native={}", lib.display());
        println!("cargo:rustc-link-lib=dylib=python3.12");
        if let Err(e) = copy_linux_runtime(&vendor, bin_dir) {
            println!("cargo:warning=could not copy libpython next to ELF: {e}");
        }
    } else {
        println!(
            "cargo:warning=vendor/python-linux missing — run tools/ensure-python-linux.sh (sidecar .so)"
        );
    }
}

fn copy_linux_runtime(vendor: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    let lib = vendor.join("lib");
    for name in [
        "libpython3.12.so.1.0",
        "libpython3.12.so",
        "libpython3.so",
    ] {
        let src = lib.join(name);
        if src.is_file() {
            fs::copy(&src, dest.join(name))?;
        }
    }
    let stdlib_src = lib.join("python3.12");
    if stdlib_src.is_dir() {
        copy_dir(&stdlib_src, &dest.join("lib").join("python3.12"))?;
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
