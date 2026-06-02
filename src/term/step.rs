//! Sequential "step" runner with check/cross/warn output.
//!
//! Faithful port of `internal/term/step.go`. The Go code used
//! `charmbracelet/huh/spinner` for non-interactive steps; here we use an
//! `indicatif` spinner. Interactive steps print a dim `· {name}` line and run
//! without a spinner (so the step can read from stdin / prompt the user).

use std::time::Duration;

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};

use super::{check_mark, cross_mark, dim, warn_mark};

/// A single unit of work in a [`run_steps`] sequence.
///
/// Mirrors Go's `Step` struct. `run` returns a status string: a value starting
/// with `"skipped"` renders as a warning line, anything else renders as a
/// success line.
pub struct Step {
    /// Human-readable label shown next to the spinner / mark.
    pub name: String,
    /// The work to perform. Returns a status string on success.
    pub run: Box<dyn FnOnce() -> Result<String>>,
    /// When `true`, no spinner is shown (the step may prompt on stdin).
    pub interactive: bool,
}

/// Run each step in order, stopping at the first error.
///
/// Mirrors Go's `RunSteps`: returns the first step's error, or `Ok(())` when
/// all steps succeed.
pub fn run_steps(steps: Vec<Step>) -> Result<()> {
    for step in steps {
        run_step(step)?;
    }
    Ok(())
}

/// Render the trailing line after a successful run, mirroring Go's shared
/// branch: a `"skipped"`-prefixed result is a warn line, otherwise a check.
fn print_result(name: &str, result: &str) {
    if result.starts_with("skipped") {
        println!("{} {} ({})", warn_mark(), name, result);
    } else {
        println!("{} {}", check_mark(), name);
    }
}

fn run_step(step: Step) -> Result<()> {
    let Step {
        name,
        run,
        interactive,
    } = step;

    if interactive {
        // Interactive: print the dim marker, then run with no spinner so the
        // step body can read from stdin / prompt.
        println!("{}", dim(format!("· {name}")));
        match run() {
            Ok(result) => {
                print_result(&name, &result);
                Ok(())
            }
            Err(err) => {
                println!("{} {}", cross_mark(), name);
                Err(err)
            }
        }
    } else {
        // Non-interactive: show a spinner titled `name` while the work runs.
        let spinner = ProgressBar::new_spinner();
        spinner.set_message(name.clone());
        if let Ok(style) = ProgressStyle::with_template("{spinner} {msg}") {
            spinner.set_style(style);
        }
        spinner.enable_steady_tick(Duration::from_millis(80));

        let run_result = run();

        spinner.finish_and_clear();

        match run_result {
            Ok(result) => {
                print_result(&name, &result);
                Ok(())
            }
            Err(err) => {
                println!("{} {}", cross_mark(), name);
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use anyhow::anyhow;

    use super::*;

    #[test]
    fn run_steps_runs_all_in_order() {
        let order = Rc::new(Cell::new(0u32));
        let mut steps = Vec::new();
        for _ in 0..3 {
            let order = Rc::clone(&order);
            steps.push(Step {
                name: "step".to_string(),
                run: Box::new(move || {
                    order.set(order.get() + 1);
                    Ok(String::new())
                }),
                interactive: true,
            });
        }
        run_steps(steps).unwrap();
        assert_eq!(order.get(), 3);
    }

    #[test]
    fn run_steps_stops_at_first_error() {
        let later_ran = Rc::new(Cell::new(false));
        let later_flag = Rc::clone(&later_ran);
        let steps = vec![
            Step {
                name: "ok".to_string(),
                run: Box::new(|| Ok(String::new())),
                interactive: true,
            },
            Step {
                name: "boom".to_string(),
                run: Box::new(|| Err(anyhow!("kaboom"))),
                interactive: true,
            },
            Step {
                name: "never".to_string(),
                run: Box::new(move || {
                    later_flag.set(true);
                    Ok(String::new())
                }),
                interactive: true,
            },
        ];
        let err = run_steps(steps).unwrap_err();
        assert_eq!(err.to_string(), "kaboom");
        assert!(!later_ran.get(), "steps after an error must not run");
    }

    #[test]
    fn run_steps_handles_skipped_result() {
        // A "skipped"-prefixed result must not be treated as an error.
        let steps = vec![Step {
            name: "maybe".to_string(),
            run: Box::new(|| Ok("skipped (already done)".to_string())),
            interactive: true,
        }];
        assert!(run_steps(steps).is_ok());
    }
}
