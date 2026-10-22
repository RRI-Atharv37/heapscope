fn main() {
    println!("cargo:rerun-if-changed=heapscope.rc");
    embed_resource::compile("heapscope.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
