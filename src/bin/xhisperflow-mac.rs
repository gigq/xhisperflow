#[cfg(target_os = "macos")]
fn main() {
    if let Err(err) = xhisperflow::macos_app::run() {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("xhisperflow-mac is only available on macOS");
    std::process::exit(1);
}
