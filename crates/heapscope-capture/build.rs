fn main() {
    println!("cargo:rerun-if-changed=capture.rc");
    println!("cargo:rerun-if-changed=src/shim.c");
    embed_resource::compile("capture.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
    cc::Build::new().file("src/shim.c").compile("hs_shim");
}
