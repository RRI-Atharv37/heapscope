//! Frame trimming: hide the allocator and C runtime frames at the inner end of a stack, so a call
//! path is keyed by the first frame that belongs to the program (`malloc` and `operator new` are
//! never the interesting part of a leak report).

use heapscope_symbols::Frame;

/// Modules whose frames are allocator plumbing when they are all that is left at the inner end of
/// a stack. This is what hides the frames of a system DLL whose symbols are not available.
fn is_runtime_module(module: &str) -> bool {
    let m = module.to_ascii_lowercase();
    matches!(
        m.as_str(),
        "ntdll.dll"
            | "kernelbase.dll"
            | "kernel32.dll"
            | "ucrtbase.dll"
            | "ucrtbased.dll"
            | "heapscope_capture.dll"
    ) || m.starts_with("vcruntime")
        || m.starts_with("msvcr")
        || m.starts_with("api-ms-win-crt-")
}

/// Function name without its parameter list or template arguments' trailing call syntax.
fn base_name(name: &str) -> &str {
    let name = name.trim();
    // `operator new(unsigned __int64)` -> `operator new`; leave `operator()` alone.
    match name.find('(') {
        Some(i) if i > 0 && !name[..i].ends_with("operator") => &name[..i],
        _ => name,
    }
}

/// Is `name` an allocation or deallocation entry point of the Windows heap or the C/C++ runtime?
pub fn is_allocator_function(name: &str) -> bool {
    let n = base_name(name);
    const EXACT: &[&str] = &[
        "malloc",
        "calloc",
        "realloc",
        "free",
        "_expand",
        "_msize",
        "malloc_base",
        "calloc_base",
        "realloc_base",
        "free_base",
        "HeapAlloc",
        "HeapReAlloc",
        "HeapFree",
        "LocalAlloc",
        "LocalReAlloc",
        "LocalFree",
        "GlobalAlloc",
        "GlobalReAlloc",
        "GlobalFree",
        "CoTaskMemAlloc",
        "CoTaskMemRealloc",
        "SysAllocString",
        "SysAllocStringLen",
    ];
    if EXACT.contains(&n) {
        return true;
    }
    const PREFIXES: &[&str] = &[
        "operator new",
        "operator delete",
        "Rtl",
        "_malloc",
        "_calloc",
        "_realloc",
        "_recalloc",
        "_free",
        "_aligned_",
        "_heap_alloc",
        "heap_alloc",
        "_nh_malloc",
        "new_scalar",
        "new_array",
    ];
    PREFIXES.iter().any(|p| n.starts_with(p))
}

pub fn is_allocator_frame(frame: &Frame) -> bool {
    frame.function.as_deref().is_some_and(is_allocator_function)
        || frame.module.as_deref().is_some_and(is_runtime_module)
}

/// The part of `frames` (innermost first) that starts at the first frame not belonging to the
/// allocator. If every frame is allocator plumbing the stack is returned whole, so the memory is
/// still attributed somewhere.
pub fn trim(frames: &[Frame]) -> &[Frame] {
    match frames.iter().position(|f| !is_allocator_frame(f)) {
        Some(i) => &frames[i..],
        None => frames,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(module: &str, function: Option<&str>) -> Frame {
        Frame {
            address: 1,
            module: Some(module.into()),
            offset: 0x10,
            function: function.map(Into::into),
            file: None,
            line: 0,
        }
    }

    #[test]
    fn names_of_the_allocator_are_recognised() {
        for n in [
            "malloc",
            "malloc_base",
            "_malloc_base",
            "_malloc_dbg",
            "operator new",
            "operator new[]",
            "operator new(unsigned __int64)",
            "RtlAllocateHeap",
            "RtlFreeHeap",
            "_aligned_malloc",
            "HeapAlloc",
        ] {
            assert!(is_allocator_function(n), "{n}");
        }
        for n in [
            "main",
            "leak_site_a",
            "std::vector<int>::push_back",
            "operator()",
            "Renderer::Rtl",
            "mallocator",
        ] {
            assert!(!is_allocator_function(n), "{n}");
        }
    }

    #[test]
    fn trimming_stops_at_the_first_user_frame() {
        let frames = [
            f("leak.exe", Some("malloc_base")),
            f("leak.exe", Some("operator new")),
            f("leak.exe", Some("leak_site_a")),
            f("leak.exe", Some("main")),
        ];
        let t = trim(&frames);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].function.as_deref(), Some("leak_site_a"));
    }

    #[test]
    fn unresolved_system_frames_are_trimmed_by_module_but_user_frames_are_not() {
        let frames = [
            f("ntdll.dll", None),
            f("ucrtbase.dll", None),
            f("app.exe", None),
            f("ntdll.dll", None), // outer system frame (thread start) stays
        ];
        let t = trim(&frames);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].module.as_deref(), Some("app.exe"));
    }

    #[test]
    fn a_stack_made_only_of_plumbing_is_kept_whole() {
        let frames = [f("ntdll.dll", None), f("kernelbase.dll", None)];
        assert_eq!(trim(&frames).len(), 2);
    }
}
