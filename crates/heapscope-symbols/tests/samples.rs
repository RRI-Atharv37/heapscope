//! Resolves the frames of a real capture of `samples/leak` to the exact source lines.
//!
//! Needs the built sample programs (cmake, RelWithDebInfo) and the workspace's `heapscope.exe`;
//! skips when they are missing unless `HEAPSCOPE_REQUIRE_SAMPLES` is set (CI sets it).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use heapscope_symbols::{Frame, FrameResolver, ModuleInfo, Options, Symbolizer};
use heapscope_trace::{Event, TraceReader};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn skip_or_fail(what: &str) -> bool {
    assert!(
        std::env::var_os("HEAPSCOPE_REQUIRE_SAMPLES").is_none(),
        "{what} not found; build the workspace and the samples first"
    );
    eprintln!("skipped: {what} not found");
    true
}

/// `(heapscope.exe, leak.exe)`, or `None` to skip.
fn tools() -> Option<(PathBuf, PathBuf)> {
    let test_exe = std::env::current_exe().ok()?;
    let cli = test_exe.parent()?.parent()?.join("heapscope.exe");
    if !cli.is_file() {
        skip_or_fail("heapscope.exe");
        return None;
    }
    let mut dirs = Vec::new();
    if let Some(d) = std::env::var_os("HEAPSCOPE_SAMPLES") {
        dirs.push(PathBuf::from(d));
    }
    dirs.push(workspace_root().join("build/samples-RelWithDebInfo"));
    dirs.push(workspace_root().join("build/samples/RelWithDebInfo"));
    match dirs
        .into_iter()
        .map(|d| d.join("leak.exe"))
        .find(|p| p.is_file())
    {
        Some(leak) => Some((cli, leak)),
        None => {
            skip_or_fail("leak.exe");
            None
        }
    }
}

struct Capture {
    modules: Vec<ModuleInfo>,
    /// Stack of the allocation site that made `count` allocations of `size` bytes.
    stacks: Vec<Vec<u64>>,
    site_of: HashMap<(u64, u64), u32>,
}

fn capture_leak(cli: &Path, leak: &Path, name: &str) -> Capture {
    let dir = std::env::temp_dir().join(format!("heapscope-symbols-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let trace = dir.join("leak.hstrace");
    let out = Command::new(cli)
        .arg("run")
        .arg("-o")
        .arg(&trace)
        .arg("--")
        .arg(leak)
        .arg("--expected")
        .arg(dir.join("expected.json"))
        .output()
        .expect("cannot start heapscope (Smart App Control may block a fresh build)");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut r = TraceReader::new(std::io::BufReader::new(
        std::fs::File::open(&trace).unwrap(),
    ))
    .unwrap();
    let mut modules = Vec::new();
    let mut counts: HashMap<(u64, u32), u64> = HashMap::new();
    while let Some(e) = r.next_event().unwrap() {
        match e {
            Event::ModuleLoad {
                base,
                size,
                pdb_guid,
                pdb_age,
                path,
                ..
            } => modules.push(ModuleInfo {
                base,
                size,
                pdb_guid,
                pdb_age,
                path,
            }),
            Event::Alloc { size, stack, .. } => *counts.entry((size, stack)).or_default() += 1,
            _ => {}
        }
    }
    let mut site_of = HashMap::new();
    for ((size, stack), n) in counts {
        site_of.insert((size, n), stack);
    }
    let stacks = (1..=r.stacks().len() as u32)
        .map(|i| r.stacks().get(i).unwrap().to_vec())
        .collect();
    Capture {
        modules,
        stacks,
        site_of,
    }
}

/// The first frame of `stack` that resolves into leak's own source file.
fn user_frame(sym: &mut Symbolizer, stack: &[u64]) -> Frame {
    stack
        .iter()
        .map(|&a| sym.resolve(a))
        .find(|f| {
            f.file
                .as_deref()
                .is_some_and(|p| p.replace('\\', "/").ends_with("samples/leak/main.cpp"))
        })
        .unwrap_or_else(|| panic!("no frame of the stack reaches samples/leak/main.cpp"))
}

/// 1-based line of the first line of `main.cpp` containing `needle`.
fn line_of(needle: &str) -> u32 {
    let src = std::fs::read_to_string(workspace_root().join("samples/leak/main.cpp")).unwrap();
    1 + src.lines().position(|l| l.contains(needle)).unwrap() as u32
}

fn options(cache: &Path) -> Options {
    Options {
        cache_dir: Some(cache.to_path_buf()),
        symbol_path: None,
    }
}

#[test]
fn leak_frames_resolve_to_the_exact_source_lines_and_are_cached() {
    let Some((cli, leak)) = tools() else { return };
    let cap = capture_leak(&cli, &leak, "exact");
    let cache = std::env::temp_dir().join(format!("heapscope-symcache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);

    let site_a = cap.site_of[&(1024, 100)];
    let site_b = cap.site_of[&(65536, 10)];
    let stack_a = &cap.stacks[site_a as usize - 1];
    let stack_b = &cap.stacks[site_b as usize - 1];

    let (fa, fb) = {
        let mut sym = Symbolizer::new(cap.modules.clone(), options(&cache));
        (user_frame(&mut sym, stack_a), user_frame(&mut sym, stack_b))
    };
    assert!(
        fa.function.as_deref().unwrap().contains("leak_site_a"),
        "{fa:?}"
    );
    assert_eq!(fa.line, line_of("::operator new(1024)"));
    assert!(
        fb.function.as_deref().unwrap().contains("leak_site_b"),
        "{fb:?}"
    );
    assert_eq!(fb.line, line_of("std::malloc(65536)"));
    assert_eq!(fa.module.as_deref(), Some("leak.exe"));

    // The cache was written, and a second run resolves the same frames without the image: its
    // path is made unreachable, so only the cache can answer.
    assert!(
        std::fs::read_dir(&cache).unwrap().count() > 0,
        "no cache file written"
    );
    let gone: Vec<ModuleInfo> = cap
        .modules
        .iter()
        .map(|m| ModuleInfo {
            path: format!("Z:\\no\\such\\dir\\{}", m.name()),
            ..m.clone()
        })
        .collect();
    let mut sym = Symbolizer::new(gone, options(&cache));
    let (ca, cb) = (user_frame(&mut sym, stack_a), user_frame(&mut sym, stack_b));
    assert_eq!(
        (ca.function, ca.file, ca.line),
        (fa.function, fa.file, fa.line)
    );
    assert_eq!((cb.function, cb.line), (fb.function, fb.line));
}

#[test]
fn frames_without_symbols_fall_back_to_module_and_offset() {
    let Some((cli, leak)) = tools() else { return };
    let cap = capture_leak(&cli, &leak, "fallback");
    let leak_mod = cap
        .modules
        .iter()
        .find(|m| m.name().eq_ignore_ascii_case("leak.exe"))
        .unwrap();
    let hidden = ModuleInfo {
        path: "Z:\\no\\such\\dir\\leak.exe".into(),
        ..leak_mod.clone()
    };
    let mut sym = Symbolizer::new(vec![hidden], Options::default());
    let f = sym.resolve(leak_mod.base + 0x1234);
    assert!(!f.is_resolved());
    assert_eq!(f.display_name(), "leak.exe+0x1234");
    // An address in no module is just an address.
    let f = sym.resolve(0x10);
    assert_eq!(f.display_name(), "0x10");
}
