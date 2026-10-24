//! On-disk cache of resolved addresses, one text file per PDB identity (GUID + age).
//!
//! Lines are `rva<TAB>line<TAB>function<TAB>file`, with the file empty when unknown. Only
//! successful lookups are stored: a failed one may succeed later once the user adds the PDB.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{ModuleInfo, Symbol};

/// `%LOCALAPPDATA%\HeapScope\symcache`, or `None` when the variable is not set.
pub fn default_cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(Path::new(&base).join("HeapScope").join("symcache"))
}

pub(crate) struct ModuleCache {
    file: PathBuf,
    entries: HashMap<u64, Symbol>,
    new: Vec<u64>,
}

fn file_name(m: &ModuleInfo) -> String {
    let mut s = String::with_capacity(40);
    for b in m.pdb_guid {
        s.push_str(&format!("{b:02x}"));
    }
    s.push_str(&format!("-{:x}.txt", m.pdb_age));
    s
}

fn clean(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ")
}

impl ModuleCache {
    pub(crate) fn load(dir: &Path, module: &ModuleInfo) -> Self {
        let file = dir.join(file_name(module));
        let mut entries = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(&file) {
            for line in text.lines() {
                let mut parts = line.splitn(4, '\t');
                let (Some(rva), Some(ln), Some(func), Some(path)) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                else {
                    continue; // a torn last line from a killed run: ignore it
                };
                let (Ok(rva), Ok(ln)) = (rva.parse::<u64>(), ln.parse::<u32>()) else {
                    continue;
                };
                entries.insert(
                    rva,
                    Symbol {
                        function: func.to_string(),
                        file: (!path.is_empty()).then(|| path.to_string()),
                        line: ln,
                    },
                );
            }
        }
        Self {
            file,
            entries,
            new: Vec::new(),
        }
    }

    pub(crate) fn get(&self, rva: u64) -> Option<&Symbol> {
        self.entries.get(&rva)
    }

    pub(crate) fn insert(&mut self, rva: u64, sym: Symbol) {
        if self.entries.insert(rva, sym).is_none() {
            self.new.push(rva);
        }
    }

    pub(crate) fn save(&mut self) -> std::io::Result<()> {
        if self.new.is_empty() {
            return Ok(());
        }
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut out = String::new();
        for rva in &self.new {
            let s = &self.entries[rva];
            out.push_str(&format!(
                "{rva}\t{}\t{}\t{}\n",
                s.line,
                clean(&s.function),
                clean(s.file.as_deref().unwrap_or(""))
            ));
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)?
            .write_all(out.as_bytes())?;
        self.new.clear();
        Ok(())
    }
}
