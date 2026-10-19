#[test]
fn compiled_lighting_policy_exercises_community_records_and_negative_readbacks() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let binary =
        std::env::temp_dir().join(format!("asense-lighting-policy-{}", std::process::id()));
    let output = std::process::Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg("-I")
        .arg(root.join("kernel"))
        .arg(root.join("tests/kernel_lighting_policy.c"))
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("native C compiler for kernel policy tests");
    assert!(
        output.status.success(),
        "C policy compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status = std::process::Command::new(&binary)
        .status()
        .expect("run actual lighting policy");
    std::fs::remove_file(binary).expect("remove owned policy test executable");
    assert!(status.success(), "lighting policy regression");
}
