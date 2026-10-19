//! Compiles the Swift status overlay (macos/overlay.swift) into `wispr-overlay` next to the
//! `wispr` binary. If `swiftc` is unavailable the build still succeeds; wispr just runs
//! without the on-screen indicator.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=macos/overlay.swift");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let Some(out_dir) = env::var_os("OUT_DIR").map(PathBuf::from) else {
        return;
    };
    // OUT_DIR is target/<profile>/build/<pkg>-<hash>/out; the binaries live in target/<profile>.
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };
    let dest = profile_dir.join("wispr-overlay");

    let status = Command::new("swiftc")
        .args(["-O", "-suppress-warnings", "macos/overlay.swift", "-o"])
        .arg(&dest)
        .status();
    if !status.is_ok_and(|s| s.success()) {
        println!(
            "cargo:warning=swiftc failed or is missing; the on-screen indicator is disabled (install Xcode command line tools)"
        );
    }
}
