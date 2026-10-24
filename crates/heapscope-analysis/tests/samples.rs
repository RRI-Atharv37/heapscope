//! Analysis of real captures of the sample programs, with real symbols. Skips when the samples or
//! `heapscope.exe` are missing unless `HEAPSCOPE_REQUIRE_SAMPLES` is set.

use std::path::{Path, PathBuf};
use std::process::Command;

use heapscope_analysis::{analyze, Analysis, NodeId, Options, Trace};
use heapscope_symbols::{Options as SymOptions, Symbolizer};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn analyze_sample(name: &str) -> Option<Analysis> {
    let missing = |what: &str| {
        assert!(
            std::env::var_os("HEAPSCOPE_REQUIRE_SAMPLES").is_none(),
            "{what} not found"
        );
        eprintln!("skipped: {what} not found");
    };
    let cli = std::env::current_exe()
        .ok()?
        .parent()?
        .parent()?
        .join("heapscope.exe");
    if !cli.is_file() {
        missing("heapscope.exe");
        return None;
    }
    let mut dirs = vec![
        root().join("build/samples-RelWithDebInfo"),
        root().join("build/samples/RelWithDebInfo"),
    ];
    if let Some(d) = std::env::var_os("HEAPSCOPE_SAMPLES") {
        dirs.insert(0, PathBuf::from(d));
    }
    let Some(exe) = dirs
        .iter()
        .map(|d| d.join(format!("{name}.exe")))
        .find(|p| p.is_file())
    else {
        missing(&format!("{name}.exe"));
        return None;
    };
    let dir =
        std::env::temp_dir().join(format!("heapscope-analysis-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let trace_path = dir.join("t.hstrace");
    let out = Command::new(cli)
        .arg("run")
        .arg("-o")
        .arg(&trace_path)
        .arg("--")
        .arg(exe)
        .arg("--expected")
        .arg(dir.join("expected.json"))
        .output()
        .expect("cannot start heapscope");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let trace = Trace::open(&trace_path).unwrap();
    let mut sym = Symbolizer::new(
        trace.modules.clone(),
        SymOptions {
            cache_dir: Some(dir.join("symcache")),
            symbol_path: None,
        },
    );
    Some(analyze(&trace, &mut sym, &Options::default()))
}

fn site(a: &Analysis, function: &str) -> NodeId {
    (1..a.tree.len() as NodeId)
        .find(|&i| a.tree.node(i).frame.name.contains(function) && a.stats[i as usize].allocs > 0)
        .unwrap_or_else(|| panic!("no allocation site {function}"))
}

#[test]
fn leak_sample_is_attributed_to_its_three_sites_with_the_expected_totals() {
    let Some(a) = analyze_sample("leak") else {
        return;
    };
    let (sa, sb, bal) = (
        a.stats[site(&a, "leak_site_a") as usize],
        a.stats[site(&a, "leak_site_b") as usize],
        a.stats[site(&a, "balanced_site") as usize],
    );
    assert_eq!(
        (sa.allocs, sa.alloc_bytes, sa.live_allocs, sa.live_bytes),
        (100, 102_400, 100, 102_400)
    );
    assert_eq!(
        (sb.allocs, sb.alloc_bytes, sb.live_allocs, sb.live_bytes),
        (10, 655_360, 10, 655_360)
    );
    assert_eq!((bal.allocs, bal.frees, bal.live_allocs), (100, 100, 0));

    // The site is the user's function, not operator new / malloc.
    let leaf = a.tree.node(site(&a, "leak_site_a"));
    assert!(leaf
        .frame
        .file
        .as_deref()
        .unwrap()
        .replace('\\', "/")
        .ends_with("samples/leak/main.cpp"));
    assert!(leaf.frame.line > 0);
    assert!(a.peak.live_bytes >= 102_400 + 655_360);
}
