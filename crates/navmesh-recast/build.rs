use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(recast_cpp)");
    let recast_src = PathBuf::from("vendor/recastnavigation/Recast/Source");
    let recast_inc = "vendor/recastnavigation/Recast/Include";

    // The C++ recastnavigation backend is OPT-IN (FUFLO_BACKEND=cpp at runtime). When its source isn't
    // vendored, skip the C++ build so the default pure-Rust `rerecast` backend compiles standalone — the
    // `recast_cpp` cfg gates the FFI in lib.rs, and `build()` falls back to an error if cpp is requested.
    if !recast_src.is_dir() {
        println!(
            "cargo:warning=recastnavigation not vendored (navmesh-recast/vendor/recastnavigation) — C++ \
             Recast backend disabled; using pure-Rust rerecast. Vendor it before FUFLO_BACKEND=cpp."
        );
        return;
    }

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .include(recast_inc)
        .include("wrapper")
        .warnings(false);

    for entry in std::fs::read_dir(&recast_src)
        .expect("Recast/Source not found — is recastnavigation vendored?")
    {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("cpp") {
            build.file(&path);
        }
    }
    build.file("wrapper/recast_wrapper.cpp");
    build.compile("recast_wrapper");

    println!("cargo:rustc-cfg=recast_cpp");
    println!("cargo:rerun-if-changed=wrapper/recast_wrapper.cpp");
    println!("cargo:rerun-if-changed=wrapper/recast_wrapper.h");
}
