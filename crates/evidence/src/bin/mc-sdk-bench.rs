//! Operator-only fixture preparation and evidence capture; never a gameplay policy.
mod bench;
fn main() {
    if bench::run(std::env::args_os().skip(1).collect()).is_err() {
        // Tool stderr stays in private evidence. Never print credentials or tool responses.
        eprintln!("mc-sdk-bench failed; inspect the private case evidence (no success implied)");
        std::process::exit(1);
    }
}
