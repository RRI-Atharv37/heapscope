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

fn write_trace(path: &std::path::Path, markers: &[(u64, &str)], allocs: &[(u64, u64, u64)]) {
    use heapscope_trace::{AllocKind, CaptureMode, Dropped, Event, Header, TraceWriter};
    let header = Header {
        tool_version: "test".into(),
        pid: 1,
        exe_path: "app.exe".into(),
        command_line: "app.exe".into(),
        pointer_size: 8,
        start_unix_ns: 0,
        capture_mode: CaptureMode::Full,
        min_lifetime_ms: 0,
        attached: false,
    };
    let mut w = TraceWriter::new(Vec::new(), &header).unwrap();
    let a = w.intern_stack(&[0x1000, 0x2000]);
    let b = w.intern_stack(&[0x1100, 0x2000]);
    for &(ts_ns, addr, size) in allocs {
        w.write_event(&Event::Alloc {
            ts_ns,
            thread: 1,
            kind: AllocKind::Heap,
            addr,
            size,
            requested: size,
            stack: if size >= 1000 { b } else { a },
        })
        .unwrap();
    }
    for &(ts_ns, label) in markers {
        w.write_event(&Event::Marker {
            ts_ns,
            thread: 1,
            label: label.into(),
        })
        .unwrap();
    }
    std::fs::write(path, w.finish(Dropped::default()).unwrap()).unwrap();
}

#[test]
fn diff_lists_grown_and_new_sites_between_two_traces() {
    let dir = std::env::temp_dir().join(format!("heapscope-cli-diff-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (a, b) = (dir.join("a.hstrace"), dir.join("b.hstrace"));
    write_trace(&a, &[], &[(10, 0x100, 64)]);
    write_trace(
        &b,
        &[],
        &[(10, 0x100, 64), (20, 0x200, 64), (30, 0x300, 5000)],
    );

    let out = heapscope().arg("diff").arg(&a).arg(&b).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("1 grew, 1 new, 0 shrank, 0 gone"), "{text}");
    assert!(text.contains("+4.9 KiB"), "{text}");

    let same = heapscope().arg("diff").arg(&a).arg(&a).output().unwrap();
    assert!(String::from_utf8_lossy(&same.stdout).contains("No allocation site changed"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn diff_compares_two_markers_of_one_trace_and_rejects_bad_arguments() {
    let dir = std::env::temp_dir().join(format!("heapscope-cli-diffm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let t = dir.join("m.hstrace");
    write_trace(
        &t,
        &[(15, "start"), (45, "stop")],
        &[(10, 0x100, 64), (20, 0x200, 128), (30, 0x300, 5000)],
    );

    let out = heapscope()
        .arg("diff")
        .arg(&t)
        .args(["--markers", "start", "stop"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("marker 'start' -> 'stop'"), "{text}");
    assert!(text.contains("1 grew, 1 new"), "{text}");

    let bad = heapscope()
        .arg("diff")
        .arg(&t)
        .args(["--markers", "start", "nope"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(1));
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(
        err.contains("no marker 'nope'") && err.contains("'start'"),
        "{err}"
    );

    let alone = heapscope().arg("diff").arg(&t).output().unwrap();
    assert_eq!(alone.status.code(), Some(1));
    std::fs::remove_dir_all(&dir).ok();
}
