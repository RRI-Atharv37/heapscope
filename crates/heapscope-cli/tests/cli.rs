use std::process::Command;

fn heapscope() -> Command {
    Command::new(env!("CARGO_BIN_EXE_heapscope"))
}

#[test]
fn version_flag_prints_package_version() {
    let out = heapscope().arg("--version").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
}

#[test]
fn stub_subcommands_fail_cleanly() {
    let out = heapscope().args(["analyze", "x.hstrace"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not implemented"));
}
