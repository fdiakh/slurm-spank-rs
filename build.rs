use std::env;
use std::path::PathBuf;

fn main() {
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    if env::var("DOCS_RS").is_ok() || env::var("SKIP_SLURM_BINDINGS").is_ok() {
        // Use pre-generated bindings when building the documentation
        let source_bindings =
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo manifest dir is empty"))
                .join("build/bindings.rs");
        let dest_bindings = out_path.join("bindings.rs");

        std::fs::copy(source_bindings, dest_bindings)
            .expect("Failed to copy pre-generated bindings");
        return;
    }

    let bindings = bindgen::Builder::default()
        .rust_target("1.72.0".parse().unwrap())
        .header("wrapper.h")
        // We define spank_option manually to indicate that string pointers are const
        .blocklist_type("spank_option")
        .generate()
        .expect("Unable to generate bindings");

    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}
