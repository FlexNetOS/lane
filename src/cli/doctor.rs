//! `lane doctor` — run diagnostic checks and print a pass/fail/warn checklist.
//!
//! Faithful port of `cmd/doctor.go`. Runs [`crate::doctor::run`] and prints each
//! result as `"{icon}  {name:<22} {message}"`, where the icon is the green check,
//! yellow warn, or red cross mark for the result's [`Status`].

use anyhow::Result;

use crate::doctor::{self, Report, Status};
use crate::term;

/// Run the diagnostic checks and print the report.
///
/// Mirrors Go's `doctorCmd.RunE`: `report := doctorRunFn(); printReport(report)`.
pub async fn run() -> Result<()> {
    let report = doctor::run().await;
    print_report(&report);
    Ok(())
}

/// Print each check result as `"{icon}  {name:<22} {message}"`.
///
/// Mirrors Go's `printReport` (`fmt.Printf("%s  %-22s %s\n", ...)`).
fn print_report(report: &Report) {
    for r in &report.results {
        let icon = status_icon(r.status);
        println!("{icon}  {:<22} {}", r.name, r.message);
    }
}

/// Map a [`Status`] to its terminal icon string.
///
/// Mirrors Go's `statusIcon`: `Pass`→check, `Warn`→warn, `Fail`→cross. The Go
/// `default: "?"` arm is unreachable because [`Status`] is an exhaustive enum.
fn status_icon(s: Status) -> String {
    match s {
        Status::Pass => term::check_mark(),
        Status::Warn => term::warn_mark(),
        Status::Fail => term::cross_mark(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port of cmd/doctor_test.go::TestPrintReport — just verify it doesn't panic.
    #[test]
    fn test_print_report() {
        let report = Report {
            results: vec![
                crate::doctor::CheckResult {
                    name: "CA certificate".to_string(),
                    status: Status::Pass,
                    message: "valid".to_string(),
                },
                crate::doctor::CheckResult {
                    name: "Daemon".to_string(),
                    status: Status::Warn,
                    message: "not running".to_string(),
                },
                crate::doctor::CheckResult {
                    name: "Hosts".to_string(),
                    status: Status::Fail,
                    message: "missing".to_string(),
                },
            ],
        };
        print_report(&report);
    }

    // Port of cmd/doctor_test.go::TestStatusIcon. The Go test only asserts each
    // icon string is non-empty (it contains ANSI codes); we additionally assert
    // the underlying glyph for functional parity.
    #[test]
    fn test_status_icon() {
        let cases = [
            (Status::Pass, '✓'),
            (Status::Warn, '!'),
            (Status::Fail, '✗'),
        ];
        for (status, glyph) in cases {
            let got = status_icon(status);
            assert!(
                !got.is_empty(),
                "statusIcon({status:?}) returned empty string"
            );
            assert!(
                got.contains(glyph),
                "statusIcon({status:?}) = {got:?} should contain {glyph:?}"
            );
        }
    }

    // TODO(test-phase): TestDoctorRunFnInjectable — Go injected a `doctorRunFn`
    // package var to assert the command calls it; the Rust port calls
    // `doctor::run().await` directly (no injectable seam) and that pipeline is
    // exercised by the doctor module's own async/integration tests.
}
