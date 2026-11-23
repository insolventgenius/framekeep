fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        panic!("Framekeep is a native macOS application");
    }

    println!("cargo:rerun-if-changed=native/host.m");
    println!("cargo:rerun-if-changed=native/icon.m");
    println!("cargo:rerun-if-changed=ui");
    println!("cargo:rustc-link-lib=sqlite3");
    println!(
        "cargo:rustc-env=FRAMEKEEP_SOURCE={}",
        env!("CARGO_MANIFEST_DIR")
    );
    cc::Build::new()
        .file("native/host.m")
        .file("native/icon.m")
        .flag("-fobjc-arc")
        .flag("-fblocks")
        .flag("-mmacosx-version-min=13.0")
        .compile("framekeep_host");
    for framework in [
        "AppKit",
        "Foundation",
        "WebKit",
        "ImageIO",
        "CoreGraphics",
        "CoreImage",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
