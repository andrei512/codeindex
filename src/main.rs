fn main() -> std::process::ExitCode {
    codeindex::adapters::cli::run(
        std::env::args_os().skip(1),
        std::io::stdout().lock(),
        std::io::stderr().lock(),
    )
}
