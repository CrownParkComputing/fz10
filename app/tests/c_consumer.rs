//! Build the smallest C application against include/retro3do.h and the Rust
//! static library, then run it. Proves the header is plain, self-sufficient C
//! and that the library exports every symbol it declares with matching types.
//! Skipped (with a note) when no C compiler or no static library is at hand.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

/// The static library cargo built alongside this test: target/<profile>/.
fn static_library() -> Option<PathBuf> {
    let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .parent()?
        .to_path_buf();
    let exe = std::env::current_exe().ok()?;
    let mut candidates: Vec<PathBuf> = ["debug", "release"]
        .iter()
        .map(|p| target.join(p).join("libretro3do.a"))
        .collect();
    // Prefer the profile directory this test binary lives under.
    candidates.sort_by_key(|c| !c.parent().is_some_and(|dir| exe.starts_with(dir)));
    candidates.into_iter().find(|c| c.exists())
}

#[test]
fn a_c_application_builds_against_the_header_and_runs() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let Some(library) = static_library() else {
        eprintln!("skipped: libretro3do.a not built");
        return;
    };
    let compiler = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let output = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("abi_c_consumer");
    let built = Command::new(&compiler)
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-I"])
        .arg(manifest.join("include"))
        .arg(manifest.join("tests/c/abi_c_consumer.c"))
        .arg(&library)
        .args(["-lpthread", "-ldl", "-lm", "-o"])
        .arg(&output)
        .output();
    let built = match built {
        Ok(built) => built,
        Err(error) => {
            eprintln!("skipped: no C compiler ({compiler}): {error}");
            return;
        }
    };
    assert!(
        built.status.success(),
        "C build failed:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );

    let ran = Command::new(&output).output().expect("the C program runs");
    let stdout = String::from_utf8_lossy(&ran.stdout);
    assert!(ran.status.success(), "C program failed: {stdout}");
    assert!(stdout.starts_with("ok: 640x480 frame, ABI 1"), "{stdout}");
}
