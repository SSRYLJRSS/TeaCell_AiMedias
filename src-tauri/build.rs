fn main() {
    // Phase 2 F02：MSVC STL ABI 垫片。msvc_stl_shim.cpp 会根据 _MSC_VER
    // 自动跳过 14.45+（这些版本已内置符号），只为旧版 Build Tools 补齐 ABI。
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && env == "msvc" {
        cc::Build::new()
            .cpp(true)
            .file("msvc_stl_shim.cpp")
            .compile("msvc_stl_shim");
    }
    tauri_build::build()
}
