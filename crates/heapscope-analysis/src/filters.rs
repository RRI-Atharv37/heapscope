//! Filters that restrict an analysis to part of the traffic.

use heapscope_symbols::Frame;

#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// Keep allocations whose stack passes through one of these modules (file names, any case).
    pub modules: Vec<String>,
    /// Keep allocations made on one of these thread ids.
    pub threads: Vec<u32>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// Keep allocations whose stack has a frame whose function, file or module contains this
    /// text (any case).
    pub text: Option<String>,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
            && self.threads.is_empty()
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.text.is_none()
    }

    /// Thread and size are properties of single allocations, which `--min-lifetime` totals no
    /// longer have, so those totals cannot be filtered by them.
    pub fn cannot_filter_summaries(&self) -> bool {
        !self.threads.is_empty() || self.min_size.is_some() || self.max_size.is_some()
    }

    pub(crate) fn allows_op(&self, thread: u32, size: u64) -> bool {
        (self.threads.is_empty() || self.threads.contains(&thread))
            && self.min_size.is_none_or(|m| size >= m)
            && self.max_size.is_none_or(|m| size <= m)
    }

    /// Does the (untrimmed) stack pass the module and text filters?
    pub fn allows_stack(&self, frames: &[Frame]) -> bool {
        if !self.modules.is_empty()
            && !frames.iter().any(|f| {
                f.module
                    .as_deref()
                    .is_some_and(|m| self.modules.iter().any(|w| w.eq_ignore_ascii_case(m)))
            })
        {
            return false;
        }
        if let Some(text) = &self.text {
            let needle = text.to_ascii_lowercase();
            let hit = |s: Option<&str>| s.is_some_and(|s| s.to_ascii_lowercase().contains(&needle));
            if !frames.iter().any(|f| {
                hit(f.function.as_deref()) || hit(f.file.as_deref()) || hit(f.module.as_deref())
            }) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(module: &str, function: &str) -> Frame {
        Frame {
            address: 1,
            module: Some(module.into()),
            offset: 0,
            function: Some(function.into()),
            file: Some(format!("C:\\src\\{function}.cpp")),
            line: 1,
        }
    }

    #[test]
    fn module_filter_matches_any_frame_case_insensitively() {
        let f = Filter {
            modules: vec!["Render.DLL".into()],
            ..Default::default()
        };
        assert!(f.allows_stack(&[frame("app.exe", "main"), frame("render.dll", "draw")]));
        assert!(!f.allows_stack(&[frame("app.exe", "main")]));
    }

    #[test]
    fn text_filter_searches_function_file_and_module() {
        let f = Filter {
            text: Some("TEXTURE".into()),
            ..Default::default()
        };
        assert!(f.allows_stack(&[frame("app.exe", "load_texture")]));
        assert!(!f.allows_stack(&[frame("app.exe", "load_mesh")]));
    }

    #[test]
    fn thread_and_size_bounds_apply_to_single_operations() {
        let f = Filter {
            threads: vec![7],
            min_size: Some(100),
            max_size: Some(200),
            ..Default::default()
        };
        assert!(f.allows_op(7, 100) && f.allows_op(7, 200));
        assert!(!f.allows_op(8, 150) && !f.allows_op(7, 99) && !f.allows_op(7, 201));
        assert!(f.cannot_filter_summaries());
        assert!(Filter::default().is_empty());
    }
}
