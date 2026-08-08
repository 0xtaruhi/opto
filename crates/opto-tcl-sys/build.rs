// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Builds pinned Tcl and embeds its script library in Opto's virtual filesystem.
//!
//! The build verifies the exact vendored Tcl patch level, generates a stable
//! index for bundled library files, and links the platform-specific static
//! interpreter plus the VFS adapter.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TCL_VERSION: &str = "8.6.11";

fn main() {
    let manifest_dir = PathBuf::from(required_env("CARGO_MANIFEST_DIR"));
    let source_dir = manifest_dir.join("../../third_party/tcl");
    let out_dir = PathBuf::from(required_env("OUT_DIR"));

    verify_source(&source_dir);
    generate_library_index(&source_dir.join("library"), &out_dir);
    let target_os = required_env("CARGO_CFG_TARGET_OS");
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target = required_env("TARGET");
    let host = required_env("HOST");
    build_tcl(
        &source_dir,
        &out_dir,
        &target_os,
        &target_env,
        &target,
        &host,
    );
    build_adapter(&manifest_dir, &source_dir, &target_os);

    println!("cargo:rerun-if-changed={}", source_dir.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("native/opto_tcl_vfs.c").display()
    );
}

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("required environment variable {name} is missing"))
}

fn verify_source(source_dir: &Path) {
    let header = fs::read_to_string(source_dir.join("generic/tcl.h"))
        .expect("vendored Tcl header is missing");
    assert!(
        header.lines().any(|line| {
            line.starts_with("#define TCL_PATCH_LEVEL")
                && line.ends_with(&format!("\"{TCL_VERSION}\""))
        }),
        "vendored Tcl must be exactly {TCL_VERSION}"
    );
}

fn generate_library_index(library_dir: &Path, out_dir: &Path) {
    let mut paths = Vec::new();
    visit(library_dir, &mut paths);
    paths.sort();

    let mut generated = String::from(
        "pub(crate) static EMBEDDED_ENTRIES: &[EmbeddedEntry] = &[\n\
         \x20   EmbeddedEntry::directory(b\"opto:/\\0\"),\n\
         \x20   EmbeddedEntry::directory(b\"opto:/tcl8.6\\0\"),\n",
    );
    for path in paths {
        let relative = path
            .strip_prefix(library_dir)
            .expect("embedded library path escaped its root");
        let relative = slash_path(relative);
        let virtual_path = format!("opto:/tcl8.6/{relative}");
        if path.is_dir() {
            writeln!(
                generated,
                "    EmbeddedEntry::directory(b\"{}\\0\"),",
                escape_rust_bytes(&virtual_path)
            )
            .expect("writing generated Tcl entry to a string cannot fail");
        } else {
            writeln!(
                generated,
                "    EmbeddedEntry::file(b\"{}\\0\", include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../third_party/tcl/library/{}\"))),",
                escape_rust_bytes(&virtual_path),
                escape_rust_string(&relative)
            )
            .expect("writing generated Tcl entry to a string cannot fail");
        }
    }
    generated.push_str("];\n");
    fs::write(out_dir.join("library_index.rs"), generated)
        .expect("failed to write embedded Tcl library index");
}

fn visit(directory: &Path, paths: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", directory.display()))
        .map(|entry| {
            entry
                .expect("failed to read vendored Tcl directory entry")
                .path()
        })
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        paths.push(path.clone());
        if path.is_dir() {
            visit(&path, paths);
        }
    }
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn escape_rust_bytes(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn escape_rust_string(value: &str) -> String {
    escape_rust_bytes(value)
}

fn build_tcl(
    source_dir: &Path,
    out_dir: &Path,
    target_os: &str,
    target_env: &str,
    target: &str,
    host: &str,
) {
    match (target_os, target_env) {
        ("windows", "msvc") => build_tcl_windows_msvc(source_dir, out_dir, target),
        ("windows", _) => panic!("opto-tcl-sys supports Windows through the MSVC toolchain"),
        _ => build_tcl_unix(source_dir, out_dir, target_os, target, host),
    }
}

fn build_tcl_unix(source_dir: &Path, out_dir: &Path, target_os: &str, target: &str, host: &str) {
    let build_dir = out_dir.join("tcl-build");
    fs::create_dir_all(&build_dir).expect("failed to create Tcl build directory");
    let makefile = build_dir.join("Makefile");
    if !makefile.exists() {
        let configure = source_dir.join("unix/configure");
        let mut command = Command::new(&configure);
        command
            .current_dir(&build_dir)
            .arg("--disable-shared")
            .arg("--enable-threads")
            .arg("--disable-symbols")
            .arg(format!(
                "--prefix={}",
                out_dir.join("tcl-install").display()
            ));
        if target != host {
            command.arg(format!("--host={target}"));
        }
        if let Ok(compiler) = env::var("CC") {
            command.env("CC", compiler);
        }
        run(&mut command, "configure vendored Tcl");
    }

    let jobs = std::thread::available_parallelism().map_or(1, usize::from);
    let mut make = Command::new("make");
    make.current_dir(&build_dir)
        .arg(format!("-j{jobs}"))
        .arg("libtcl8.6.a");
    run(&mut make, "build vendored Tcl");

    println!("cargo:rustc-link-search=native={}", build_dir.display());
    println!("cargo:rustc-link-lib=static=tcl8.6");
    for library in ["pthread", "z", "m"] {
        println!("cargo:rustc-link-lib={library}");
    }
    if target_os == "macos" {
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
    } else {
        println!("cargo:rustc-link-lib=dl");
    }
}

fn build_tcl_windows_msvc(source_dir: &Path, out_dir: &Path, target: &str) {
    let build_dir = out_dir.join("tcl-build");
    let install_dir = out_dir.join("tcl-install");
    fs::create_dir_all(&build_dir).expect("failed to create Tcl build directory");
    fs::create_dir_all(&install_dir).expect("failed to create Tcl install directory");

    let nmake_tool = cc::windows_registry::find_tool(target, "nmake.exe")
        .expect("the MSVC toolchain does not provide nmake.exe");
    let vc_install_dir = nmake_tool
        .path()
        .ancestors()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("VC"))
        })
        .expect("nmake.exe is not inside a Visual Studio VC installation");
    let mut nmake = nmake_tool.to_command();
    nmake
        .env("VCINSTALLDIR", vc_install_dir)
        .current_dir(source_dir.join("win"))
        .arg("/f")
        .arg("makefile.vc")
        .arg("core")
        .arg("OPTS=static,msvcrt")
        .arg(format!("TMP_DIR={}", build_dir.display()))
        .arg(format!("OUT_DIR={}", build_dir.display()))
        .arg(format!("INSTALLDIR={}", install_dir.display()));
    run(&mut nmake, "build vendored Tcl");

    println!("cargo:rustc-link-search=native={}", build_dir.display());
    // Tcl 8.6's MSVC naming uses `t` for threads, `s` for a static library,
    // and `x` when that static library uses the dynamic MSVC runtime.
    // Keep Tcl's /GL archive intact instead of repacking it into this crate's
    // rlib. MSVC needs the original archive metadata when linking downstream
    // test and binary targets.
    println!("cargo:rustc-link-lib=static:-bundle=tcl86tsx");
    for library in ["advapi32", "netapi32", "user32", "userenv", "ws2_32"] {
        println!("cargo:rustc-link-lib={library}");
    }
}

fn build_adapter(manifest_dir: &Path, source_dir: &Path, target_os: &str) {
    let mut build = cc::Build::new();
    build
        .file(manifest_dir.join("native/opto_tcl_vfs.c"))
        .include(source_dir.join("generic"))
        .define("STATIC_BUILD", None)
        .warnings(true);
    if target_os == "windows" {
        build.define("_CRT_SECURE_NO_WARNINGS", None);
    }
    build.compile("opto_tcl_vfs");
}

fn run(command: &mut Command, action: &str) {
    let output = command
        .output()
        .unwrap_or_else(|err| panic!("failed to {action}: {err}"));
    if !output.status.success() {
        fail_command(action, command, &output);
    }
}

fn fail_command(action: &str, command: &Command, output: &Output) -> ! {
    panic!(
        "failed to {action} with {:?}:\nstdout:\n{}\nstderr:\n{}",
        command,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
