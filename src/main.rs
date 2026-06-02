use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    lane::install_crypto_provider();

    // When re-executed as the detached daemon, run the proxy in the foreground
    // of the child process instead of the CLI.
    if lane::daemon::is_child() {
        if let Err(e) = lane::daemon::run_foreground().await {
            lane::daemon::write_startup_error(&e);
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }

    match lane::cli::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Root error presentation mirrors slim's `cmd.Execute`.
            eprintln!("\n{} {e:#}", lane::term::red("Error:"));
            ExitCode::FAILURE
        }
    }
}
