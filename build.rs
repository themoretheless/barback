fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    // SkyLight lives in /System/Library/PrivateFrameworks and exports the CGS*
    // symbols Ice (and we) rely on. CoreGraphics pulls it in transitively, but
    // we link it explicitly so the extern "C" declarations resolve cleanly.
    println!("cargo:rustc-link-search=framework=/System/Library/PrivateFrameworks");
    println!("cargo:rustc-link-lib=framework=SkyLight");

    // TCC refuses calendar access to an executable that carries no usage
    // description, and it does not merely deny: it kills the process. A bare
    // cargo binary has no bundle to read one from, so embed the plist directly
    // into the Mach-O. The `-bins` form keeps the section off build scripts and
    // test harnesses, which must not carry it.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set");
    println!("cargo:rerun-if-changed=Info.plist");
    println!(
        "cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{manifest_dir}/Info.plist"
    );
}
