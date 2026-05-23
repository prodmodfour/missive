use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if let Ok(target) = std::env::var("TARGET") {
        println!("cargo:rustc-env=MISSIVE_BUILD_TARGET={target}");
    }
    if let Ok(profile) = std::env::var("PROFILE") {
        println!("cargo:rustc-env=MISSIVE_BUILD_PROFILE={profile}");
    }

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    if let Ok(output) = Command::new(rustc).arg("--version").output() {
        if output.status.success() {
            if let Ok(version) = String::from_utf8(output.stdout) {
                println!(
                    "cargo:rustc-env=MISSIVE_BUILD_RUSTC_VERSION={}",
                    version.trim()
                );
            }
        }
    }
}
