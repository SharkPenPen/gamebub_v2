fn main() {
    embuild::espidf::sysenv::output();

    // Embed the current git commit so the boot banner can tell which revision is
    // actually flashed onto the device. Never fail the build if git is missing
    // (for example when building from a source tarball).
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|commit| !commit.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_COMMIT={}", commit);
    println!("cargo:rerun-if-changed=../../.git/HEAD");

    slint_build::compile_with_config(
        "res/ui/main.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
    )
    .unwrap();
}
