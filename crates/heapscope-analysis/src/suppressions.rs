//! Suppressions: call paths whose live-at-exit memory is expected and should not be reported as a
//! leak candidate. Built-in rules cover C runtime and loader start-up; a user file adds the
//! program's own intentional exit-time "leaks" (singletons, caches that live for the whole run).
//!
//! A rule matches a call path when any frame of it matches. File format, one rule per line:
//!
//! ```text
//! # a comment
//! fn:cv::getInitializationMutex   # reason shown in the report
//! module:opencv_world*.dll
//! file:*\third_party\*
//! ```
//!
//! Patterns are case-insensitive and understand `*` (any run of characters) and `?` (one
//! character). `fn:` matches the function name as printed in the report, `module:` the module
//! file name, and `file:` the source path (either slash).

use std::path::Path;

use crate::model::{CallTree, FrameInfo, NodeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Function,
    Module,
    File,
}

impl Target {
    fn keyword(self) -> &'static str {
        match self {
            Self::Function => "fn",
            Self::Module => "module",
            Self::File => "file",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuppressionRule {
    pub target: Target,
    pub pattern: String,
    /// Why this is expected, shown next to the suppressed entry.
    pub reason: String,
    pub builtin: bool,
}

impl SuppressionRule {
    fn matches(&self, frame: &FrameInfo) -> bool {
        match self.target {
            Target::Function => glob(&self.pattern, &frame.name),
            Target::Module => frame
                .module
                .as_deref()
                .is_some_and(|m| glob(&self.pattern, m)),
            Target::File => frame
                .file
                .as_deref()
                .is_some_and(|f| glob(&self.pattern.replace('\\', "/"), &f.replace('\\', "/"))),
        }
    }

    /// The rule as it is written in a suppression file.
    pub fn spec(&self) -> String {
        format!("{}:{}", self.target.keyword(), self.pattern)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Suppressions {
    rules: Vec<SuppressionRule>,
}

/// Start-up work that is allocated once and released by process teardown, never by the program.
const BUILTIN: &[(Target, &str, &str)] = &[
    (
        Target::Function,
        "*dynamic initializer for*",
        "global object constructed before main; destroyed (or not) at process exit",
    ),
    (
        Target::Function,
        "*dynamic atexit destructor for*",
        "global object teardown at process exit",
    ),
    (
        Target::Function,
        "_initterm*",
        "C runtime static initialisation",
    ),
    (Target::Function, "_cinit", "C runtime start-up"),
    (
        Target::Function,
        "__acrt_initialize*",
        "universal C runtime start-up",
    ),
    (
        Target::Function,
        "__vcrt_initialize*",
        "Visual C++ runtime start-up",
    ),
    (
        Target::Function,
        "__scrt_initialize_*",
        "Visual C++ runtime start-up",
    ),
    (
        Target::Function,
        "dllmain_crt_process_attach",
        "C runtime initialisation when a DLL loads",
    ),
    (
        Target::Function,
        "__dyn_tls_init",
        "thread-local storage initialisation",
    ),
    (
        Target::Function,
        "std::_Init_locks::*",
        "standard library lock tables, kept for the life of the process",
    ),
    (
        Target::Function,
        "std::ios_base::Init::*",
        "standard stream objects, kept for the life of the process",
    ),
    (
        Target::Function,
        "std::locale::*Init*",
        "standard library locale tables, kept for the life of the process",
    ),
    (
        Target::Function,
        "Ldrp*",
        "Windows loader bookkeeping for loaded modules",
    ),
    (
        Target::Function,
        "LdrInitializeThunk",
        "Windows loader bookkeeping for loaded modules",
    ),
    (
        Target::Function,
        "LdrLoadDll",
        "Windows loader bookkeeping for loaded modules",
    ),
];

impl Suppressions {
    pub fn none() -> Self {
        Self::default()
    }

    /// The rules shipped with HeapScope.
    pub fn builtin() -> Self {
        Self {
            rules: BUILTIN
                .iter()
                .map(|&(target, pattern, reason)| SuppressionRule {
                    target,
                    pattern: pattern.into(),
                    reason: reason.into(),
                    builtin: true,
                })
                .collect(),
        }
    }

    pub fn rules(&self) -> &[SuppressionRule] {
        &self.rules
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Rules of `other` are tried after the ones already here.
    pub fn extend(&mut self, other: Self) {
        self.rules.extend(other.rules);
    }

    /// Parse the text of a suppression file. The error names the offending line.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut rules = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let (spec, comment) = match raw.find('#') {
                Some(at) => (&raw[..at], raw[at + 1..].trim()),
                None => (raw, ""),
            };
            let spec = spec.trim();
            if spec.is_empty() {
                continue;
            }
            let line = i + 1;
            let Some((keyword, pattern)) = spec.split_once(':') else {
                return Err(format!(
                    "line {line}: expected 'fn:', 'module:' or 'file:' followed by a pattern, \
                     found '{spec}'"
                ));
            };
            let target = match keyword.trim() {
                "fn" => Target::Function,
                "module" => Target::Module,
                "file" => Target::File,
                other => {
                    return Err(format!(
                        "line {line}: unknown rule type '{other}' (use fn, module or file)"
                    ))
                }
            };
            let pattern = pattern.trim();
            if pattern.is_empty() {
                return Err(format!("line {line}: the pattern is empty"));
            }
            rules.push(SuppressionRule {
                target,
                pattern: pattern.into(),
                reason: if comment.is_empty() {
                    format!("suppression file, line {line}")
                } else {
                    comment.into()
                },
                builtin: false,
            });
        }
        Ok(Self { rules })
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read suppression file {}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The first rule that matches a frame of the call path ending at `node`.
    pub fn matching(&self, tree: &CallTree, node: NodeId) -> Option<&SuppressionRule> {
        if self.rules.is_empty() {
            return None;
        }
        let path = tree.path(node);
        self.rules
            .iter()
            .find(|r| path.iter().any(|f| r.matches(f)))
    }
}

/// Case-insensitive match of `text` against a pattern with `*` and `?`.
fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0, 0);
    // Where the last `*` was, and how much text it has swallowed so far.
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(name: &str, module: &str, file: &str) -> FrameInfo {
        FrameInfo {
            name: name.into(),
            module: Some(module.into()),
            file: (!file.is_empty()).then(|| file.into()),
            line: 1,
        }
    }

    fn tree_of(frames: &[FrameInfo]) -> (CallTree, NodeId) {
        let mut t = CallTree::new();
        let mut n = crate::ROOT;
        for f in frames {
            n = t.child(n, f.clone());
        }
        (t, n)
    }

    #[test]
    fn globs() {
        assert!(glob("abc", "ABC"));
        assert!(glob("a*c", "abbbc"));
        assert!(glob(
            "*dynamic initializer for*",
            "`dynamic initializer for 'g_x''"
        ));
        assert!(glob("a?c", "abc"));
        assert!(glob("*", ""));
        assert!(!glob("a?c", "ac"));
        assert!(!glob("a*c", "abd"));
        assert!(!glob("abc", "abcd"));
        assert!(glob("a*b*c", "axxbxxc"));
    }

    #[test]
    fn user_file_parses_with_reasons_and_reports_bad_lines() {
        let s = Suppressions::parse(
            "# header\n\nfn:cv::getInitializationMutex  # OpenCV singleton\nmodule:opencv*.dll\nfile:*/third_party/*\n",
        )
        .unwrap();
        assert_eq!(s.rules().len(), 3);
        assert_eq!(s.rules()[0].reason, "OpenCV singleton");
        assert_eq!(s.rules()[0].spec(), "fn:cv::getInitializationMutex");
        assert!(!s.rules()[0].builtin);
        assert!(s.rules()[1].reason.contains("line 4"));

        let e = Suppressions::parse("fn:ok\nbogus\n").unwrap_err();
        assert!(e.contains("line 2"), "{e}");
        let e = Suppressions::parse("what:x\n").unwrap_err();
        assert!(e.contains("unknown rule type"), "{e}");
        assert!(Suppressions::parse("fn:  \n").is_err());
    }

    #[test]
    fn a_rule_matches_any_frame_of_the_path() {
        let (tree, leaf) = tree_of(&[
            frame("main", "app.exe", "C:\\src\\main.cpp"),
            frame("cv::getInitializationMutex", "opencv_world.dll", ""),
            frame("make_mutex", "opencv_world.dll", ""),
        ]);
        let s = Suppressions::parse("fn:cv::getInit*\n").unwrap();
        assert!(s.matching(&tree, leaf).is_some());
        let s = Suppressions::parse("module:OPENCV*.dll\n").unwrap();
        assert!(s.matching(&tree, leaf).is_some());
        let s = Suppressions::parse("file:c:/src/*\n").unwrap();
        assert!(
            s.matching(&tree, leaf).is_some(),
            "either slash style matches"
        );
        let s = Suppressions::parse("fn:other\n").unwrap();
        assert!(s.matching(&tree, leaf).is_none());
        assert!(Suppressions::none().matching(&tree, leaf).is_none());
    }

    #[test]
    fn builtin_rules_hide_startup_but_not_ordinary_user_code() {
        let b = Suppressions::builtin();
        let (tree, leaf) = tree_of(&[
            frame("_initterm", "app.exe", ""),
            frame("`dynamic initializer for 'g_cache''", "app.exe", ""),
            frame("build_cache", "app.exe", ""),
        ]);
        assert!(b.matching(&tree, leaf).unwrap().builtin);

        let (tree, leaf) = tree_of(&[
            frame("RtlUserThreadStart", "ntdll.dll", ""),
            frame("worker", "app.exe", ""),
            frame("leaky", "app.exe", ""),
        ]);
        assert!(b.matching(&tree, leaf).is_none());
        let (tree, leaf) = tree_of(&[
            frame("main", "app.exe", ""),
            frame("leak_site_a", "app.exe", ""),
        ]);
        assert!(b.matching(&tree, leaf).is_none());
    }
}
