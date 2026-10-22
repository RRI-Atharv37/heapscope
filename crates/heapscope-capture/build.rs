fn main() {
    println!("cargo:rerun-if-changed=capture.rc");
    embed_resource::compile("capture.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
