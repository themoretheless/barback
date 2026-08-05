fn main() {
    // SkyLight lives in /System/Library/PrivateFrameworks and exports the CGS*
    // symbols Ice (and we) rely on. CoreGraphics pulls it in transitively, but
    // we link it explicitly so the extern "C" declarations resolve cleanly.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-search=framework=/System/Library/PrivateFrameworks");
        println!("cargo:rustc-link-lib=framework=SkyLight");
    }
}
