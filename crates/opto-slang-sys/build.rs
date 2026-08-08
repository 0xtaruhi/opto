// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Builds the pinned slang/fmt compatibility set and Opto's native C bridge.
//!
//! The build is deliberately offline: source trees must already exist at their
//! pinned vendor locations. A deterministic source fingerprint is exported to
//! Rust so checkpoints can identify the exact native frontend implementation.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, ffi::OsString, fs};

fn main() {
    println!("cargo:rerun-if-env-changed=OPTO_SLANG_VENDOR_DIR");
    println!("cargo:rerun-if-env-changed=OPTO_FMT_VENDOR_DIR");
    println!("cargo:rerun-if-env-changed=CXX");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=native/CMakeLists.txt");
    println!("cargo:rerun-if-changed=native/opto_slang_bridge.cpp");
    println!("cargo:rerun-if-changed=native/opto_slang_bridge.h");
    println!("cargo:rerun-if-changed=native/opto_slang_internal.h");
    println!("cargo:rerun-if-changed=native/opto_slang_lower_internal.h");
    println!("cargo:rerun-if-changed=native/opto_slang_lower_support.cpp");
    println!("cargo:rerun-if-changed=native/opto_slang_lower_expr.cpp");
    println!("cargo:rerun-if-changed=native/opto_slang_lower_process.cpp");
    println!("cargo:rerun-if-changed=native/opto_slang_lower_hierarchy.cpp");
    println!("cargo:rerun-if-changed=native/opto_slang_views.cpp");
    println!("cargo:rerun-if-changed=../../third_party/slang");
    println!("cargo:rerun-if-changed=../../third_party/fmt");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.join("../..");
    let vendor_dir = env::var_os("OPTO_SLANG_VENDOR_DIR")
        .map_or_else(|| workspace_root.join("third_party/slang"), PathBuf::from);
    let fmt_vendor_dir = env::var_os("OPTO_FMT_VENDOR_DIR")
        .map_or_else(|| workspace_root.join("third_party/fmt"), PathBuf::from);

    require_slang_vendor(&vendor_dir);
    require_fmt_vendor(&fmt_vendor_dir);
    let native_fingerprint = source_tree_fingerprint(&[
        ("bridge", manifest_dir.join("native")),
        ("slang/include", vendor_dir.join("include")),
        ("slang/source", vendor_dir.join("source")),
        ("slang/CMakeLists.txt", vendor_dir.join("CMakeLists.txt")),
    ]);
    println!("cargo:rustc-env=OPTO_SLANG_NATIVE_FINGERPRINT={native_fingerprint:016x}");
    println!(
        "cargo:rustc-env=OPTO_SLANG_VENDOR_DIR_RESOLVED={}",
        vendor_dir.display()
    );

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let build_dir = out_dir.join("opto-slang-native");
    let archive_dir = build_dir.join("lib");
    configure_native_build(
        &manifest_dir,
        &vendor_dir,
        &fmt_vendor_dir,
        &build_dir,
        &archive_dir,
    );
    build_native_bridge(&build_dir);

    println!("cargo:rustc-link-search=native={}", archive_dir.display());
    println!(
        "cargo:rustc-link-search=native={}",
        build_dir.join("slang/lib").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        build_dir.join("slang/lib64").display()
    );
    println!("cargo:rustc-link-lib=static=opto_slang_bridge");
    println!("cargo:rustc-link-lib=static=svlang");
    println!("cargo:rustc-link-lib=static=fmt");
    let cxx = cmake_cxx_compiler(&build_dir)
        .unwrap_or_else(|| fail("CMake did not record the selected C++ compiler"));
    link_cpp_standard_library(&cxx);
}

fn source_tree_fingerprint(roots: &[(&str, PathBuf)]) -> u64 {
    let mut files = Vec::new();
    for (label, root) in roots {
        let mut root_files = Vec::new();
        collect_source_files(root, &mut root_files);
        for path in root_files {
            let relative = if root.is_file() {
                String::new()
            } else {
                normalized_relative_path(root, &path)
            };
            files.push((format!("{label}/{relative}"), path));
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));

    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for (stable_path, path) in files {
        hash_bytes(&mut hash, &(stable_path.len() as u64).to_le_bytes());
        hash_bytes(&mut hash, stable_path.as_bytes());
        let bytes = fs::read(&path).unwrap_or_else(|err| {
            fail(format!(
                "failed to fingerprint native source '{}': {err}",
                path.display()
            ))
        });
        hash_bytes(&mut hash, &(bytes.len() as u64).to_le_bytes());
        hash_bytes(&mut hash, &bytes);
    }
    hash
}

fn normalized_relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("collected source belongs to its fingerprint root")
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn collect_source_files(path: &Path, files: &mut Vec<PathBuf>) {
    if path.is_file() {
        files.push(path.to_path_buf());
        return;
    }
    let entries = fs::read_dir(path).unwrap_or_else(|err| {
        fail(format!(
            "failed to enumerate native source '{}': {err}",
            path.display()
        ))
    });
    for entry in entries {
        let entry = entry
            .unwrap_or_else(|err| fail(format!("failed to enumerate native source entry: {err}")));
        collect_source_files(&entry.path(), files);
    }
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
}

fn require_slang_vendor(vendor_dir: &Path) {
    if !vendor_dir.join("CMakeLists.txt").is_file() {
        fail(format!(
            "opto-slang-sys requires SystemVerilog slang source at '{}' or OPTO_SLANG_VENDOR_DIR",
            vendor_dir.display()
        ));
    }
}

fn require_fmt_vendor(vendor_dir: &Path) {
    if !vendor_dir.join("CMakeLists.txt").is_file() {
        fail(format!(
            "opto-slang-sys requires fmt source at '{}' or OPTO_FMT_VENDOR_DIR",
            vendor_dir.display()
        ));
    }
}

fn configure_native_build(
    manifest_dir: &Path,
    vendor_dir: &Path,
    fmt_vendor_dir: &Path,
    build_dir: &Path,
    archive_dir: &Path,
) {
    let native_dir = manifest_dir.join("native");
    let selected_cxx = selected_cxx_compiler();
    reset_for_compiler_change(build_dir, selected_cxx.as_deref());
    let mut command = Command::new("cmake");
    command
        .arg("-S")
        .arg(&native_dir)
        .arg("-B")
        .arg(build_dir)
        .arg(format!("-DOPTO_SLANG_VENDOR_DIR={}", vendor_dir.display()))
        .arg(format!(
            "-DOPTO_FMT_VENDOR_DIR={}",
            fmt_vendor_dir.display()
        ))
        .arg(format!(
            "-DCMAKE_ARCHIVE_OUTPUT_DIRECTORY={}",
            archive_dir.display()
        ))
        .arg(format!(
            "-DCMAKE_ARCHIVE_OUTPUT_DIRECTORY_RELEASE={}",
            archive_dir.display()
        ))
        .arg("-DCMAKE_BUILD_TYPE=Release");
    if let Some(cxx) = selected_cxx {
        command.arg(cmake_definition("CMAKE_CXX_COMPILER", &cxx));
    }
    let status = command
        .status()
        .unwrap_or_else(|err| fail(format!("failed to run cmake configure: {err}")));
    if !status.success() {
        fail(format!(
            "cmake configure failed for opto-slang-sys native bridge with status {status}"
        ));
    }
}

fn reset_for_compiler_change(build_dir: &Path, requested: Option<&std::ffi::OsStr>) {
    let Some(requested) = requested else {
        return;
    };
    let Some(cached) = cmake_cxx_compiler(build_dir) else {
        return;
    };
    let requested = Path::new(requested);
    let matches = if requested.components().count() == 1 {
        cached.file_name() == requested.file_name()
    } else {
        cached == requested
    };
    if !matches {
        fs::remove_dir_all(build_dir).unwrap_or_else(|err| {
            fail(format!(
                "failed to reset CMake build after C++ compiler changed: {err}"
            ))
        });
    }
}

fn selected_cxx_compiler() -> Option<OsString> {
    if let Some(cxx) = env::var_os("CXX") {
        return Some(cxx);
    }
    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("windows") => None,
        _ => Some(OsString::from("clang++")),
    }
}

fn cmake_definition(name: &str, value: &OsString) -> OsString {
    let mut definition = OsString::from(format!("-D{name}="));
    definition.push(value);
    definition
}

fn build_native_bridge(build_dir: &Path) {
    let status = Command::new("cmake")
        .arg("--build")
        .arg(build_dir)
        .arg("--target")
        .arg("opto_slang_bridge")
        .arg("--config")
        .arg("Release")
        .arg("--parallel")
        .status()
        .unwrap_or_else(|err| fail(format!("failed to run cmake build: {err}")));
    if !status.success() {
        fail(format!(
            "cmake build failed for opto-slang-sys native bridge with status {status}"
        ));
    }
}

fn cmake_cxx_compiler(build_dir: &Path) -> Option<PathBuf> {
    let cache = fs::read_to_string(build_dir.join("CMakeCache.txt")).ok()?;
    for line in cache.lines() {
        if let Some((key, value)) = line.split_once('=')
            && key.starts_with("CMAKE_CXX_COMPILER:")
        {
            return Some(PathBuf::from(value));
        }
    }
    None
}

fn link_cpp_standard_library(cxx: &Path) {
    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos" | "ios") => println!("cargo:rustc-link-lib=c++"),
        Ok("windows") => {}
        _ => {
            add_cpp_runtime_search_path(cxx, "libstdc++fs.a");
            add_cpp_runtime_search_path(cxx, "libstdc++.a");
            println!("cargo:rustc-link-lib=static=stdc++fs");
            println!("cargo:rustc-link-lib=static=stdc++");
        }
    }
}

fn add_cpp_runtime_search_path(cxx: &Path, library_name: &str) {
    let output = Command::new(cxx)
        .arg(format!("-print-file-name={library_name}"))
        .output();
    let Ok(output) = output else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let path = PathBuf::from(path);
    if path.is_file()
        && let Some(parent) = path.parent()
    {
        println!("cargo:rustc-link-search=native={}", parent.display());
    }
}

fn fail(message: impl AsRef<str>) -> ! {
    eprintln!("error: {}", message.as_ref());
    std::process::exit(1);
}
