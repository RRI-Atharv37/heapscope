//! Turns the raw return addresses in a trace into function, file and line.
//!
//! Resolution happens after capture, from the module list the capture recorded: each module is
//! loaded into DbgHelp at the base address it had in the target process, so the addresses in the
//! trace can be looked up unchanged. Symbols come from the PDB next to the image or from
//! `_NT_SYMBOL_PATH` (a symbol server works if the user configured one). Results are cached on
//! disk per PDB identity, so a second analysis of the same build does not touch DbgHelp at all.
//!
//! A frame that cannot be resolved is not an error: it is reported as `module+offset`.

mod cache;
mod dbghelp;

use std::collections::HashMap;
use std::path::PathBuf;

pub use cache::default_cache_dir;

/// A module that was loaded in the profiled process, as recorded in the trace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleInfo {
    pub base: u64,
    pub size: u64,
    /// The `RSDS` GUID bytes exactly as stored in the image (all zero: the image has no PDB).
    pub pdb_guid: [u8; 16],
    pub pdb_age: u32,
    pub path: String,
}

impl ModuleInfo {
    /// File name without the directory.
    pub fn name(&self) -> &str {
        self.path.rsplit(['\\', '/']).next().unwrap_or(&self.path)
    }

    fn contains(&self, addr: u64) -> bool {
        addr >= self.base && addr - self.base < self.size
    }
}

/// One resolved stack frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// The address from the trace (a return address).
    pub address: u64,
    /// File name of the module containing the address, if any module does.
    pub module: Option<String>,
    /// Offset of the address from the module base.
    pub offset: u64,
    pub function: Option<String>,
    pub file: Option<String>,
    /// 0 when unknown.
    pub line: u32,
}

impl Frame {
    /// `function`, else `module+0xoffset`, else the bare address.
    pub fn display_name(&self) -> String {
        if let Some(f) = &self.function {
            return f.clone();
        }
        match &self.module {
            Some(m) => format!("{m}+0x{:x}", self.offset),
            None => format!("0x{:x}", self.address),
        }
    }

    pub fn is_resolved(&self) -> bool {
        self.function.is_some()
    }
}

/// Anything that can turn an address into a frame. Analysis takes this so its tests need no PDBs.
pub trait FrameResolver {
    fn resolve(&mut self, address: u64) -> Frame;
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Where results are cached; `None` disables the cache.
    pub cache_dir: Option<PathBuf>,
    /// DbgHelp search path. `None` leaves DbgHelp's default (which honours `_NT_SYMBOL_PATH`).
    pub symbol_path: Option<String>,
}

impl Options {
    /// The cache in `%LOCALAPPDATA%\HeapScope\symcache` and `_NT_SYMBOL_PATH`.
    pub fn standard() -> Self {
        Self {
            cache_dir: default_cache_dir(),
            symbol_path: std::env::var("_NT_SYMBOL_PATH")
                .ok()
                .filter(|s| !s.is_empty()),
        }
    }
}

pub struct Symbolizer {
    modules: Vec<ModuleInfo>,
    options: Options,
    session: Option<dbghelp::Session>,
    caches: HashMap<usize, cache::ModuleCache>,
    memo: HashMap<u64, Frame>,
}

impl Symbolizer {
    /// `modules` may list the same address range more than once (a DLL unloaded and another
    /// loaded at its base); the module listed last wins, which is the one loaded most recently.
    pub fn new(modules: Vec<ModuleInfo>, options: Options) -> Self {
        Self {
            modules,
            options,
            session: None,
            caches: HashMap::new(),
            memo: HashMap::new(),
        }
    }

    fn module_index(&self, addr: u64) -> Option<usize> {
        self.modules.iter().rposition(|m| m.contains(addr))
    }

    /// Write newly resolved frames to the disk cache. Also done on drop; call it to see errors.
    pub fn flush_cache(&mut self) -> std::io::Result<()> {
        let mut result = Ok(());
        for cache in self.caches.values_mut() {
            if let Err(e) = cache.save() {
                result = Err(e);
            }
        }
        result
    }

    fn resolve_uncached(&mut self, address: u64) -> Frame {
        let mut frame = Frame {
            address,
            ..Frame::default()
        };
        let Some(i) = self.module_index(address) else {
            return frame;
        };
        let module = self.modules[i].clone();
        frame.module = Some(module.name().to_string());
        frame.offset = address - module.base;
        // A return address points after the call; the call itself is one byte earlier, which
        // matters when the call is the last instruction of a function or of a line.
        let lookup = address.saturating_sub(1).max(module.base);
        let rva = lookup - module.base;

        let use_cache = module.pdb_guid != [0; 16] && self.options.cache_dir.is_some();
        if use_cache {
            let dir = self.options.cache_dir.clone().unwrap_or_default();
            let cache = self
                .caches
                .entry(i)
                .or_insert_with(|| cache::ModuleCache::load(&dir, &module));
            if let Some(hit) = cache.get(rva) {
                frame.function = Some(hit.function.clone());
                frame.file = hit.file.clone();
                frame.line = hit.line;
                return frame;
            }
        }

        let path = self.options.symbol_path.as_deref();
        let session = self
            .session
            .get_or_insert_with(|| dbghelp::Session::new(path));
        if let Some(sym) = session.lookup(&module, lookup) {
            frame.function = Some(sym.function.clone());
            frame.file = sym.file.clone();
            frame.line = sym.line;
            if use_cache {
                if let Some(cache) = self.caches.get_mut(&i) {
                    cache.insert(rva, sym);
                }
            }
        }
        frame
    }
}

impl FrameResolver for Symbolizer {
    fn resolve(&mut self, address: u64) -> Frame {
        if let Some(f) = self.memo.get(&address) {
            return f.clone();
        }
        let frame = self.resolve_uncached(address);
        self.memo.insert(address, frame.clone());
        frame
    }
}

impl Drop for Symbolizer {
    fn drop(&mut self) {
        let _ = self.flush_cache();
    }
}

/// What DbgHelp (or the cache) knows about one address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Symbol {
    pub function: String,
    pub file: Option<String>,
    pub line: u32,
}
