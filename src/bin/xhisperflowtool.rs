fn main() {
    if let Err(err) = xhisperflow::app::run_xhisperflowtool_main() {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}
