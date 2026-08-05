// The meeting-selection rules are plain Rust and are compiled (and tested)
// everywhere; only the AppKit and CoreGraphics shell is macOS specific.
mod meeting;

#[cfg(target_os = "macos")]
mod app;
#[cfg(target_os = "macos")]
mod calendar;
#[cfg(target_os = "macos")]
mod cgs;
#[cfg(target_os = "macos")]
mod menu_bar_items;

#[cfg(target_os = "macos")]
fn main() {
    app::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("barback only runs on macOS");
    std::process::exit(1);
}
