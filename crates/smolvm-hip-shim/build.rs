fn main() {
    // The library must announce itself as libamdhip64.so.7 so the dynamic
    // linker satisfies programs linked against (or dlopen-ing) the real HIP
    // runtime's soname. The build artifact is libamdhip64.so; installers
    // symlink/rename to libamdhip64.so.7 (ROCm 7.x's soname on this host).
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("linux") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libamdhip64.so.7");
    }
}
