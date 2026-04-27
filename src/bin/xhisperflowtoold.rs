fn main() {
    if let Err(err) = xhisperflow::app::run_xhisperflowtoold_main() {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}
