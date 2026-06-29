use std::fs;

#[test]
fn test_ci_workflow_includes_windows_latest() {
    let ci = fs::read_to_string(".github/workflows/ci.yml").unwrap();
    assert!(ci.contains("windows-latest"));
}

#[test]
fn test_release_workflow_includes_windows_msvc_zip_build() {
    let release = fs::read_to_string(".github/workflows/release.yml").unwrap();
    assert!(release.contains("x86_64-pc-windows-msvc"));
    assert!(release.contains("windows-latest"));
    assert!(release.contains("squad.exe"));
    assert!(release.contains(".zip"));
}

#[test]
fn test_release_workflow_keeps_unix_archives_enabled() {
    let release = fs::read_to_string(".github/workflows/release.yml").unwrap();
    assert!(release.contains("target: aarch64-apple-darwin"));
    assert!(release.contains("target: x86_64-apple-darwin"));
    assert!(release.contains("target: x86_64-unknown-linux-musl"));
    assert_eq!(release.matches("archive-format: tar.gz").count(), 3);
}

#[test]
fn test_release_workflow_checksums_cover_zip_and_tarballs() {
    let release = fs::read_to_string(".github/workflows/release.yml").unwrap();
    assert!(release.contains("*.tar.gz"));
    assert!(release.contains("*.zip"));
}
