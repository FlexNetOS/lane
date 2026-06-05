//! `lane completions <shell>` — emit a shell completion script.

use clap::CommandFactory;

use super::{Cli, CompletionsArgs};

/// Generate a shell completion script for the given shell and write it to
/// stdout.
///
/// The completion script is a machine artifact meant to be `eval`'d (like
/// `version --json`), so the raw script is written to `std::io::stdout()`,
/// intentionally bypassing `crate::term`.
pub(crate) fn run(args: &CompletionsArgs) -> anyhow::Result<()> {
    let mut cmd = Cli::command();
    let bin_name = cmd.get_name().to_string();
    clap_complete::generate(args.shell, &mut cmd, bin_name, &mut std::io::stdout());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap_complete::Shell;

    #[test]
    fn generates_bash_completion_mentioning_lane_and_subcommand() {
        let mut buf: Vec<u8> = Vec::new();
        clap_complete::generate(Shell::Bash, &mut Cli::command(), "lane", &mut buf);
        let script = String::from_utf8(buf).expect("completion script is valid UTF-8");

        assert!(!script.is_empty(), "completion script should not be empty");
        assert!(
            script.contains("lane"),
            "completion script should mention the binary name"
        );
        assert!(
            script.contains("start"),
            "completion script should mention a known subcommand"
        );
    }

    #[test]
    fn command_tree_is_valid() {
        // Validates the entire clap command tree, including the new
        // `completions` variant.
        Cli::command().debug_assert();
    }
}
