//! Stable call-path identity across runs. Addresses move with ASLR and builds shift line numbers,
//! so a path is identified by the module and function names of its frames only (an unresolved
//! frame is `module+0xoffset`, which is stable for the same build).

use std::collections::BTreeMap;

use crate::model::{CallTree, FrameInfo, NodeId};

/// How many frames from the allocation site outwards identify a path by default. Deeper frames
/// vary with recursion and with who happened to call, which would split one site into many.
pub const DEFAULT_DEPTH: usize = 12;

/// One frame as `module!function`, lower-cased where the platform ignores case.
pub fn frame_key(frame: &FrameInfo) -> String {
    match &frame.module {
        Some(m) => format!("{}!{}", m.to_ascii_lowercase(), frame.name),
        None => frame.name.clone(),
    }
}

/// The identity of the call path ending at `node`: its innermost `depth` frames, site first.
pub fn path_key(tree: &CallTree, node: NodeId, depth: usize) -> String {
    tree.path(node)
        .iter()
        .take(depth.max(1))
        .map(|f| frame_key(f))
        .collect::<Vec<_>>()
        .join(" <- ")
}

/// Tree nodes grouped by identity, so statistics of paths that only differ below `depth` merge.
pub fn group_nodes(tree: &CallTree, depth: usize) -> BTreeMap<String, Vec<NodeId>> {
    let mut out: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();
    for id in 1..tree.len() as NodeId {
        out.entry(path_key(tree, id, depth)).or_default().push(id);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(name: &str, module: &str, line: u32) -> FrameInfo {
        FrameInfo {
            name: name.into(),
            module: Some(module.into()),
            file: Some("x.cpp".into()),
            line,
        }
    }

    fn chain(frames: &[FrameInfo]) -> (CallTree, NodeId) {
        let mut t = CallTree::new();
        let mut n = crate::ROOT;
        for f in frames {
            n = t.child(n, f.clone());
        }
        (t, n)
    }

    #[test]
    fn lines_and_module_case_do_not_change_identity() {
        let (a, na) = chain(&[frame("main", "App.exe", 10), frame("site", "App.exe", 20)]);
        let (b, nb) = chain(&[frame("main", "app.exe", 11), frame("site", "app.exe", 25)]);
        assert_eq!(path_key(&a, na, 8), path_key(&b, nb, 8));
        assert_eq!(path_key(&a, na, 8), "app.exe!site <- app.exe!main");
    }

    #[test]
    fn different_functions_differ_and_depth_truncates_from_the_outer_end() {
        let (a, na) = chain(&[frame("main", "app.exe", 1), frame("site", "app.exe", 1)]);
        let (b, nb) = chain(&[frame("start", "app.exe", 1), frame("site", "app.exe", 1)]);
        assert_ne!(path_key(&a, na, 8), path_key(&b, nb, 8));
        assert_eq!(path_key(&a, na, 1), path_key(&b, nb, 1));
        assert_eq!(
            path_key(&a, na, 0),
            path_key(&a, na, 1),
            "depth is at least one frame"
        );
    }

    #[test]
    fn unresolved_frames_keep_their_offset() {
        let f = FrameInfo {
            name: "app.exe+0x1a2b".into(),
            module: Some("app.exe".into()),
            file: None,
            line: 0,
        };
        assert_eq!(frame_key(&f), "app.exe!app.exe+0x1a2b");
    }
}
