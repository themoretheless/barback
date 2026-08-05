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
    //
    // The plist is generated rather than checked in so that the version cannot
    // drift away from Cargo.toml. tools/make-app.sh substitutes the same two
    // placeholders for the bundle copy.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set");
    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is set");
    let build = std::env::var("BARBACK_BUILD").unwrap_or_else(|_| version.clone());

    println!("cargo:rerun-if-changed=Info.plist.in");
    println!("cargo:rerun-if-env-changed=BARBACK_BUILD");

    let template = std::fs::read_to_string(format!("{manifest_dir}/Info.plist.in"))
        .expect("Info.plist.in is readable");
    let plist = template
        .replace("@VERSION@", &version)
        .replace("@BUILD@", &build);
    let plist_path = format!("{out_dir}/Info.plist");
    std::fs::write(&plist_path, plist).expect("the generated plist is writable");

    println!("cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{plist_path}");
}
