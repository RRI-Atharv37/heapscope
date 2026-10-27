use std::path::PathBuf;

/// `0.1.0-beta` -> `0,1,0,0` for the numeric VERSIONINFO fields.
fn numeric_version(v: &str) -> String {
    let core = v.split(['-', '+']).next().unwrap_or(v);
    let mut parts: Vec<&str> = core.split('.').collect();
    parts.resize(4, "0");
    parts[..4].join(",")
}

fn main() {
    println!("cargo:rerun-if-changed=heapscope.rc");
    // The resource template carries @VERSION@ and @NUMERIC_VERSION@ so the version shown in
    // Explorer is the crate version, never a second copy to keep in step.
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let template = std::fs::read_to_string("heapscope.rc").unwrap();
    let rc = template
        .replace("@NUMERIC_VERSION@", &numeric_version(&version))
        .replace("@VERSION@", &version);
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("heapscope.rc");
    std::fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
