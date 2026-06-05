//! OS-level port forwarding (80/443 -> the proxy's ports).
//!
//! Faithful port of `internal/system/portfwd.go`, `portfwd_linux.go`, and
//! `portfwd_darwin.go`. Only the host platform's concrete implementation is
//! compiled, but every pure string-matching helper (iptables message parsing,
//! pf token parsing) is compiled on all targets so its unit tests run
//! everywhere.

use anyhow::Result;

/// Cross-platform port-forwarding controller.
pub trait PortForwarder {
    /// Install the redirect rules (80/443 -> proxy ports).
    fn enable(&self) -> Result<()>;
    /// Remove the redirect rules.
    fn disable(&self) -> Result<()>;
    /// Report whether the OUTPUT jump (Linux) / anchor file (Darwin) is present.
    fn is_enabled(&self) -> bool;
    /// Report whether the rules are actually loaded in the running firewall.
    fn is_loaded(&self) -> bool;
    /// Repair/load the rules, reapplying any missing wiring.
    fn ensure_loaded(&self) -> Result<()>;
    /// Classify the forwarding rule's presence, distinguishing "cannot
    /// determine without privilege" ([`ForwardingStatus::Unknown`]) from
    /// genuinely "absent". Used by the read-only `doctor` check so a
    /// permission-denied probe is reported honestly instead of as a false
    /// "not configured". The default mirrors [`PortForwarder::is_enabled`]:
    /// `Present` when enabled, `Absent` otherwise (platforms that can always
    /// determine presence without privilege never return `Unknown`).
    fn forwarding_status(&self) -> ForwardingStatus {
        if self.is_enabled() {
            ForwardingStatus::Present
        } else {
            ForwardingStatus::Absent
        }
    }
}

/// Three-way result of probing whether the port-forwarding rule is installed.
///
/// Unlike [`PortForwarder::is_enabled`]'s `bool` (which collapses "absent" and
/// "could-not-check" into `false`), this separates the two so a read-only
/// caller can warn instead of falsely reporting the rule missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardingStatus {
    /// The rule is confirmed installed (check exited 0).
    Present,
    /// The rule is confirmed absent (check ran with enough privilege and the
    /// rule was missing).
    Absent,
    /// The rule's presence could not be determined (e.g. the probe failed with
    /// permission-denied because it ran without root).
    Unknown,
}

/// Construct the port forwarder for the current platform.
#[cfg(target_os = "linux")]
pub fn new_port_forwarder() -> Box<dyn PortForwarder> {
    Box::new(LinuxPortFwd::new())
}

/// Construct the port forwarder for the current platform.
#[cfg(target_os = "macos")]
pub fn new_port_forwarder() -> Box<dyn PortForwarder> {
    Box::new(DarwinPortFwd)
}

/// Construct the port forwarder for the current platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn new_port_forwarder() -> Box<dyn PortForwarder> {
    Box::new(UnsupportedPortFwd)
}

// ===========================================================================
// Pure matcher helpers — compiled on ALL targets (Go: lower-cased substring
// matches). Kept platform-independent so their unit tests run everywhere.
// ===========================================================================

/// iptables `-N` chain creation already-exists detection.
///
/// Go: `strings.Contains(msg, "chain already exists") || strings.Contains(msg, "file exists")`
/// on the lower-cased, trimmed output.
pub fn iptables_chain_already_exists(output: &[u8]) -> bool {
    let msg = String::from_utf8_lossy(output).trim().to_lowercase();
    msg.contains("chain already exists") || msg.contains("file exists")
}

/// iptables chain-missing detection (used to tolerate flush/delete of an
/// already-removed chain).
///
/// Go: matches "no chain/target/match by that name", "does a matching rule
/// exist", or "not found" on the lower-cased, trimmed output.
pub fn iptables_chain_missing(output: &[u8]) -> bool {
    let msg = String::from_utf8_lossy(output).trim().to_lowercase();
    msg.contains("no chain/target/match by that name")
        || msg.contains("does a matching rule exist")
        || msg.contains("not found")
}

/// `-C` rule-check "rule absent" detection: a non-zero exit whose (lower-cased,
/// trimmed) output matches one of these phrases means the rule does not exist
/// rather than a hard failure.
///
/// Go (inline in `ruleExists`): "bad rule" || "no chain/target/match by that
/// name" || "does a matching rule exist" || "not found".
pub fn iptables_rule_absent(output: &[u8]) -> bool {
    let msg = String::from_utf8_lossy(output).trim().to_lowercase();
    msg.contains("bad rule")
        || msg.contains("no chain/target/match by that name")
        || msg.contains("does a matching rule exist")
        || msg.contains("not found")
}

/// pf "pf already enabled" detection.
pub fn is_pf_already_enabled_output(out: &str) -> bool {
    out.to_lowercase().contains("pf already enabled")
}

/// pf `-s info` "status: enabled" detection.
pub fn is_pf_enabled_info_output(out: &str) -> bool {
    out.to_lowercase().contains("status: enabled")
}

/// Parse the reference token from `pfctl -E` output.
///
/// Go: for each line containing "token" (case-insensitive), split on the first
/// ':' and return the trimmed right-hand side if non-empty.
pub fn parse_pf_enable_token(out: &str) -> String {
    for line in out.split('\n') {
        if !line.to_lowercase().contains("token") {
            continue;
        }
        match line.split_once(':') {
            Some((_, rhs)) => {
                let token = rhs.trim();
                if !token.is_empty() {
                    return token.to_string();
                }
            }
            None => continue,
        }
    }
    String::new()
}

/// Report whether `pfctl -s References` output references `token`.
///
/// Go: empty token -> false; else any line containing the token substring.
pub fn has_pf_reference_token(out: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    out.split('\n').any(|line| line.contains(token))
}

// ===========================================================================
// Unsupported platform stub.
// ===========================================================================

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
struct UnsupportedPortFwd;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl PortForwarder for UnsupportedPortFwd {
    fn enable(&self) -> Result<()> {
        anyhow::bail!("port forwarding is not supported on this platform")
    }
    fn disable(&self) -> Result<()> {
        Ok(())
    }
    fn is_enabled(&self) -> bool {
        false
    }
    fn is_loaded(&self) -> bool {
        false
    }
    fn ensure_loaded(&self) -> Result<()> {
        self.enable()
    }
}

// ===========================================================================
// Linux implementation (iptables nat chain LANE).
// ===========================================================================

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{anyhow, Result};

    use super::ForwardingStatus;
    use super::PortForwarder;
    use super::{iptables_chain_already_exists, iptables_chain_missing, iptables_rule_absent};
    use crate::config;
    use crate::osutil;

    /// iptables nat chain name.
    pub(super) const LINUX_CHAIN_NAME: &str = "LANE";

    /// Combined output + exit result from a privileged iptables invocation.
    type RunResult = (Vec<u8>, Result<()>);

    /// Seam type for `osutil::command_exists`.
    type CommandExistsFn = Box<dyn Fn(&str) -> bool + Send + Sync>;
    /// Seam type for `osutil::run_privileged` (combined output + exit result).
    type RunPrivilegedFn = Box<dyn Fn(&str, &[&str]) -> RunResult + Send + Sync>;
    /// Seam type for the `iptables -C` check. Returns a three-way
    /// [`ForwardingStatus`] derived from the check's exit code so callers can
    /// tell "rule absent" apart from "could not check (permission denied)".
    /// (Go modelled this as `execCommandFn(...).Run()==nil`, a bool; we widen
    /// it so the read-only doctor probe is honest about permission-denied.)
    type CheckCommandFn = Box<dyn Fn(&str, &[&str]) -> ForwardingStatus + Send + Sync>;

    /// Linux port forwarder. Holds injectable seams mirroring Go's package-level
    /// function pointers (`commandExistsLinuxFn`, `runPrivilegedLinuxFn`,
    /// `execCommandLinuxFn`) so the behavioral unit tests can drive a mock.
    pub struct LinuxPortFwd {
        command_exists: CommandExistsFn,
        run_privileged: RunPrivilegedFn,
        /// Classifies the `iptables -C` OUTPUT-jump check into
        /// [`ForwardingStatus`]: `Present` (exit 0), `Unknown`
        /// (permission-denied, exit 4) or `Absent` (any other non-zero). Drives
        /// both `is_enabled` (`== Present`) and `forwarding_status`.
        check_command_status: CheckCommandFn,
    }

    impl Default for LinuxPortFwd {
        fn default() -> Self {
            Self::new()
        }
    }

    impl LinuxPortFwd {
        pub fn new() -> Self {
            LinuxPortFwd {
                command_exists: Box::new(osutil::command_exists),
                run_privileged: Box::new(osutil::run_privileged),
                check_command_status: Box::new(default_check_command_status),
            }
        }

        fn run(&self, args: &[&str]) -> RunResult {
            (self.run_privileged)("iptables", args)
        }

        fn ensure_chain(&self) -> Result<()> {
            let (output, res) = self.run(&["-t", "nat", "-N", LINUX_CHAIN_NAME]);
            if let Err(e) = res {
                if !iptables_chain_already_exists(&output) {
                    return Err(anyhow!(
                        "creating chain {}: {}: {}",
                        LINUX_CHAIN_NAME,
                        trimmed(&output),
                        e
                    ));
                }
            }

            let (output, res) = self.run(&["-t", "nat", "-F", LINUX_CHAIN_NAME]);
            if let Err(e) = res {
                return Err(anyhow!(
                    "flushing chain {}: {}: {}",
                    LINUX_CHAIN_NAME,
                    trimmed(&output),
                    e
                ));
            }
            Ok(())
        }

        fn ensure_redirect_rule(&self, from_port: u16, to_port: u16) -> Result<()> {
            let from = from_port.to_string();
            let to = to_port.to_string();
            let args = [
                "-t",
                "nat",
                "-A",
                LINUX_CHAIN_NAME,
                "-p",
                "tcp",
                "-d",
                "127.0.0.1/32",
                "--dport",
                from.as_str(),
                "-j",
                "REDIRECT",
                "--to-ports",
                to.as_str(),
            ];
            let (output, res) = self.run(&args);
            if let Err(e) = res {
                return Err(anyhow!(
                    "adding redirect rule {}->{}: {}: {}",
                    from_port,
                    to_port,
                    trimmed(&output),
                    e
                ));
            }
            Ok(())
        }

        /// Port of `ruleExists`: returns Ok(true) when present, Ok(false) when a
        /// recognized "absent" message comes back, Err on an unexpected failure.
        fn rule_exists(&self, chain: &str, rule_args: &[&str]) -> Result<bool> {
            let mut args: Vec<&str> = vec!["-t", "nat", "-C", chain];
            args.extend_from_slice(rule_args);
            let (output, res) = self.run(&args);
            if res.is_ok() {
                return Ok(true);
            }
            // res is Err here; recover the underlying error for the message.
            let err = res.unwrap_err();
            if iptables_rule_absent(&output) {
                return Ok(false);
            }
            Err(anyhow!(
                "checking iptables rule: {}: {}",
                trimmed(&output),
                err
            ))
        }
    }

    impl PortForwarder for LinuxPortFwd {
        fn enable(&self) -> Result<()> {
            if !(self.command_exists)("iptables") {
                return Err(anyhow!("iptables not found (install iptables)"));
            }

            self.ensure_chain()?;
            self.ensure_redirect_rule(80, config::PROXY_HTTP_PORT)?;
            self.ensure_redirect_rule(443, config::PROXY_HTTPS_PORT)?;

            let exists =
                self.rule_exists("OUTPUT", &["-o", "lo", "-p", "tcp", "-j", LINUX_CHAIN_NAME])?;
            if !exists {
                let (output, res) = self.run(&[
                    "-t",
                    "nat",
                    "-I",
                    "OUTPUT",
                    "1",
                    "-o",
                    "lo",
                    "-p",
                    "tcp",
                    "-j",
                    LINUX_CHAIN_NAME,
                ]);
                if let Err(e) = res {
                    return Err(anyhow!(
                        "installing OUTPUT jump rule: {}: {}",
                        trimmed(&output),
                        e
                    ));
                }
            }
            Ok(())
        }

        fn disable(&self) -> Result<()> {
            if !(self.command_exists)("iptables") {
                return Ok(());
            }

            loop {
                let exists =
                    self.rule_exists("OUTPUT", &["-o", "lo", "-p", "tcp", "-j", LINUX_CHAIN_NAME])?;
                if !exists {
                    break;
                }
                let (output, res) = self.run(&[
                    "-t",
                    "nat",
                    "-D",
                    "OUTPUT",
                    "-o",
                    "lo",
                    "-p",
                    "tcp",
                    "-j",
                    LINUX_CHAIN_NAME,
                ]);
                if let Err(e) = res {
                    return Err(anyhow!(
                        "removing OUTPUT jump rule: {}: {}",
                        trimmed(&output),
                        e
                    ));
                }
            }

            let (output, res) = self.run(&["-t", "nat", "-F", LINUX_CHAIN_NAME]);
            if let Err(e) = res {
                if !iptables_chain_missing(&output) {
                    return Err(anyhow!(
                        "flushing chain {}: {}: {}",
                        LINUX_CHAIN_NAME,
                        trimmed(&output),
                        e
                    ));
                }
            }

            let (output, res) = self.run(&["-t", "nat", "-X", LINUX_CHAIN_NAME]);
            if let Err(e) = res {
                if !iptables_chain_missing(&output) {
                    return Err(anyhow!(
                        "deleting chain {}: {}: {}",
                        LINUX_CHAIN_NAME,
                        trimmed(&output),
                        e
                    ));
                }
            }
            Ok(())
        }

        fn is_loaded(&self) -> bool {
            self.is_enabled()
        }

        fn is_enabled(&self) -> bool {
            self.forwarding_status() == ForwardingStatus::Present
        }

        fn forwarding_status(&self) -> ForwardingStatus {
            if !(self.command_exists)("iptables") {
                // No iptables at all: the rule cannot be installed, so it is
                // genuinely absent (mirrors the old `is_enabled` -> false).
                return ForwardingStatus::Absent;
            }
            (self.check_command_status)(
                "iptables",
                &[
                    "-t",
                    "nat",
                    "-C",
                    "OUTPUT",
                    "-o",
                    "lo",
                    "-p",
                    "tcp",
                    "-j",
                    LINUX_CHAIN_NAME,
                ],
            )
        }

        fn ensure_loaded(&self) -> Result<()> {
            self.enable()
        }
    }

    /// Default for `check_command_status`: spawn the `iptables -C` check and
    /// classify its exit code.
    ///
    /// iptables exit-code mapping (`man iptables` / `xtables`):
    /// * 0 -> rule present ([`ForwardingStatus::Present`]).
    /// * 4 -> resource/permission problem; unprivileged `iptables` prints
    ///   "Permission denied (you must be root)" and exits 4, so we cannot tell
    ///   whether the rule exists ([`ForwardingStatus::Unknown`]). Doctor is a
    ///   read-only diagnostic and must NOT escalate with sudo, so this is the
    ///   honest answer.
    /// * any other non-zero (typically 1 / 2 -> rule absent or bad argument), or
    ///   a spawn failure -> [`ForwardingStatus::Absent`].
    fn default_check_command_status(name: &str, args: &[&str]) -> ForwardingStatus {
        use std::process::{Command, Stdio};
        match Command::new(name)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => ForwardingStatus::Present,
            Ok(status) if status.code() == Some(4) => ForwardingStatus::Unknown,
            Ok(_) => ForwardingStatus::Absent,
            Err(_) => ForwardingStatus::Absent,
        }
    }

    /// `strings.TrimSpace(string(output))`.
    fn trimmed(output: &[u8]) -> String {
        String::from_utf8_lossy(output).trim().to_string()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::{Arc, Mutex};

        /// Mirror of Go's `iptablesMock`: records commands and tracks the
        /// chain/jump state to answer `-C` checks the way real iptables would.
        #[derive(Default)]
        struct MockState {
            commands: Vec<Vec<String>>,
            chain_exists: bool,
            output_jump: bool,
        }

        #[derive(Clone)]
        struct IptablesMock {
            state: Arc<Mutex<MockState>>,
        }

        impl IptablesMock {
            fn new(chain_exists: bool, output_jump: bool) -> Self {
                IptablesMock {
                    state: Arc::new(Mutex::new(MockState {
                        commands: Vec::new(),
                        chain_exists,
                        output_jump,
                    })),
                }
            }

            fn run(&self, name: &str, args: &[&str]) -> RunResult {
                let mut st = self.state.lock().unwrap();
                let mut cmd = vec![name.to_string()];
                cmd.extend(args.iter().map(|s| s.to_string()));
                st.commands.push(cmd);

                if args.len() < 4 || args[0] != "-t" || args[1] != "nat" {
                    return (b"invalid".to_vec(), Err(anyhow!("invalid command")));
                }

                let match_prefix = |action: &str, chain: &str| -> bool {
                    args.len() >= 4 && args[2] == action && args[3] == chain
                };

                if match_prefix("-C", "OUTPUT") {
                    if st.output_jump {
                        return (Vec::new(), Ok(()));
                    }
                    return (b"Bad rule".to_vec(), Err(anyhow!("exit 1")));
                }
                if match_prefix("-N", LINUX_CHAIN_NAME) {
                    if st.chain_exists {
                        return (b"Chain already exists".to_vec(), Err(anyhow!("exit 1")));
                    }
                    st.chain_exists = true;
                    return (Vec::new(), Ok(()));
                }
                if match_prefix("-F", LINUX_CHAIN_NAME) {
                    return (Vec::new(), Ok(()));
                }
                if match_prefix("-A", LINUX_CHAIN_NAME) {
                    return (Vec::new(), Ok(()));
                }
                if match_prefix("-I", "OUTPUT") {
                    st.output_jump = true;
                    return (Vec::new(), Ok(()));
                }
                if match_prefix("-D", "OUTPUT") {
                    if !st.output_jump {
                        return (b"Bad rule".to_vec(), Err(anyhow!("exit 1")));
                    }
                    st.output_jump = false;
                    return (Vec::new(), Ok(()));
                }
                if match_prefix("-X", LINUX_CHAIN_NAME) {
                    if !st.chain_exists {
                        return (
                            b"No chain/target/match by that name".to_vec(),
                            Err(anyhow!("exit 1")),
                        );
                    }
                    st.chain_exists = false;
                    return (Vec::new(), Ok(()));
                }
                (Vec::new(), Ok(()))
            }

            fn count_command(&self, action: &str, chain: &str) -> usize {
                let st = self.state.lock().unwrap();
                let needle = format!(" {} {}", action, chain);
                st.commands
                    .iter()
                    .filter(|cmd| cmd.join(" ").contains(&needle))
                    .count()
            }
        }

        /// Build a `LinuxPortFwd` whose seams are driven by `mock`, with
        /// `iptables` reported present.
        fn pf_with_mock(mock: IptablesMock) -> LinuxPortFwd {
            let m = mock.clone();
            LinuxPortFwd {
                command_exists: Box::new(|name| name == "iptables"),
                run_privileged: Box::new(move |name, args| m.run(name, args)),
                check_command_status: Box::new(|_, _| ForwardingStatus::Absent),
            }
        }

        #[test]
        fn enable_twice_installs_output_jump_once() {
            // Port of TestLinuxPortForwardEnableTwiceInstallsOutputJumpOnce.
            let mock = IptablesMock::new(false, false);
            let pf = pf_with_mock(mock.clone());

            pf.enable().expect("first Enable");
            pf.enable().expect("second Enable");

            assert_eq!(
                mock.count_command("-I", "OUTPUT"),
                1,
                "expected one OUTPUT jump insert"
            );
            assert_eq!(
                mock.count_command("-A", LINUX_CHAIN_NAME),
                4,
                "expected four redirect appends (2 per enable)"
            );
        }

        #[test]
        fn disable_removes_rules() {
            // Port of TestLinuxPortForwardDisableRemovesRules.
            let mock = IptablesMock::new(true, true);
            let pf = pf_with_mock(mock.clone());

            pf.disable().expect("Disable");

            assert_eq!(
                mock.count_command("-D", "OUTPUT"),
                1,
                "expected one OUTPUT jump removal"
            );
            assert_eq!(
                mock.count_command("-F", LINUX_CHAIN_NAME),
                1,
                "expected one chain flush"
            );
            assert_eq!(
                mock.count_command("-X", LINUX_CHAIN_NAME),
                1,
                "expected one chain delete"
            );
        }

        #[test]
        fn enable_fails_when_iptables_missing() {
            // Port of TestLinuxPortForwardEnableFailsWhenIptablesMissing.
            let pf = LinuxPortFwd {
                command_exists: Box::new(|_| false),
                run_privileged: Box::new(|_, _| (Vec::new(), Ok(()))),
                check_command_status: Box::new(|_, _| ForwardingStatus::Absent),
            };
            assert!(
                pf.enable().is_err(),
                "expected Enable to fail when iptables is missing"
            );
        }

        #[test]
        fn is_enabled_check() {
            // Port of TestLinuxPortForwardIsEnabledCheck.
            let pf_ok = LinuxPortFwd {
                command_exists: Box::new(|name| name == "iptables"),
                run_privileged: Box::new(|_, _| (Vec::new(), Ok(()))),
                check_command_status: Box::new(|_, _| ForwardingStatus::Present),
            };
            assert!(
                pf_ok.is_enabled(),
                "expected IsEnabled true when iptables check command succeeds"
            );

            let pf_fail = LinuxPortFwd {
                command_exists: Box::new(|name| name == "iptables"),
                run_privileged: Box::new(|_, _| (Vec::new(), Ok(()))),
                check_command_status: Box::new(|_, _| ForwardingStatus::Absent),
            };
            assert!(
                !pf_fail.is_enabled(),
                "expected IsEnabled false when iptables check command fails"
            );
        }

        #[test]
        fn forwarding_status_three_way() {
            // exit 0 -> Present, permission-denied (exit 4) -> Unknown,
            // rule-absent (other non-zero) -> Absent. Drives the doctor
            // Pass / Warn / Fail mapping.
            let present = LinuxPortFwd {
                command_exists: Box::new(|name| name == "iptables"),
                run_privileged: Box::new(|_, _| (Vec::new(), Ok(()))),
                check_command_status: Box::new(|_, _| ForwardingStatus::Present),
            };
            assert_eq!(present.forwarding_status(), ForwardingStatus::Present);
            assert!(present.is_enabled());

            let unknown = LinuxPortFwd {
                command_exists: Box::new(|name| name == "iptables"),
                run_privileged: Box::new(|_, _| (Vec::new(), Ok(()))),
                check_command_status: Box::new(|_, _| ForwardingStatus::Unknown),
            };
            assert_eq!(unknown.forwarding_status(), ForwardingStatus::Unknown);
            // Unknown must NOT read as enabled (no false Pass).
            assert!(!unknown.is_enabled());

            let absent = LinuxPortFwd {
                command_exists: Box::new(|name| name == "iptables"),
                run_privileged: Box::new(|_, _| (Vec::new(), Ok(()))),
                check_command_status: Box::new(|_, _| ForwardingStatus::Absent),
            };
            assert_eq!(absent.forwarding_status(), ForwardingStatus::Absent);
            assert!(!absent.is_enabled());

            // No iptables binary -> Absent (cannot be installed).
            let no_iptables = LinuxPortFwd {
                command_exists: Box::new(|_| false),
                run_privileged: Box::new(|_, _| (Vec::new(), Ok(()))),
                check_command_status: Box::new(|_, _| ForwardingStatus::Present),
            };
            assert_eq!(no_iptables.forwarding_status(), ForwardingStatus::Absent);
        }

        #[test]
        fn default_check_command_status_maps_exit_codes() {
            // `true` exits 0 -> Present; `false` exits 1 -> Absent; a missing
            // binary -> Absent. (Exit-4 -> Unknown is covered behaviorally by
            // the injected-seam test above, since exit 4 is hard to provoke
            // portably from a stock shell utility.)
            assert_eq!(
                default_check_command_status("true", &[]),
                ForwardingStatus::Present
            );
            assert_eq!(
                default_check_command_status("false", &[]),
                ForwardingStatus::Absent
            );
            assert_eq!(
                default_check_command_status("definitely-not-a-real-binary-xyz", &[]),
                ForwardingStatus::Absent
            );
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::LinuxPortFwd;

// ===========================================================================
// Darwin implementation (pf anchor com.lane).
// ===========================================================================

#[cfg(target_os = "macos")]
mod darwin {
    use std::fs;
    use std::process::Command;

    use anyhow::{anyhow, Result};

    use super::{
        has_pf_reference_token, is_pf_already_enabled_output, is_pf_enabled_info_output,
        parse_pf_enable_token, PortForwarder,
    };
    use crate::config;
    use crate::system::elevated::write_file_elevated;

    const ANCHOR_NAME: &str = "com.lane";
    const ANCHOR_FILE: &str = "/etc/pf.anchors/com.lane";

    fn pf_rules() -> String {
        format!(
            "rdr pass on lo0 inet proto tcp from any to 127.0.0.1 port 80 -> 127.0.0.1 port {}\nrdr pass on lo0 inet proto tcp from any to 127.0.0.1 port 443 -> 127.0.0.1 port {}\n",
            config::PROXY_HTTP_PORT,
            config::PROXY_HTTPS_PORT
        )
    }

    pub struct DarwinPortFwd;

    /// Run `sudo <args...>` and return (combined output, exit result).
    fn sudo_combined(args: &[&str]) -> (Vec<u8>, Result<()>) {
        let out = Command::new("sudo").args(args).output();
        match out {
            Ok(o) => {
                let mut combined = o.stdout;
                combined.extend_from_slice(&o.stderr);
                let res = if o.status.success() {
                    Ok(())
                } else {
                    match o.status.code() {
                        Some(code) => Err(anyhow!("exit status {}", code)),
                        None => Err(anyhow!("signal: killed")),
                    }
                };
                (combined, res)
            }
            Err(e) => (Vec::new(), Err(anyhow::Error::new(e))),
        }
    }

    fn trimmed(output: &[u8]) -> String {
        String::from_utf8_lossy(output).trim().to_string()
    }

    fn read_pf_reference_token() -> Result<String> {
        let data = fs::read(config::pf_token_path())?;
        Ok(String::from_utf8_lossy(&data).trim().to_string())
    }

    fn is_pf_reference_token_active(token: &str) -> bool {
        if token.is_empty() {
            return false;
        }
        let (output, res) = sudo_combined(&["pfctl", "-s", "References"]);
        if res.is_err() {
            return false;
        }
        has_pf_reference_token(&String::from_utf8_lossy(&output), token)
    }

    fn ensure_pf_enabled() -> Result<()> {
        let (output, res) = sudo_combined(&["pfctl", "-e"]);
        if res.is_err() {
            let out = trimmed(&output);
            if !is_pf_already_enabled_output(&out) {
                return Err(anyhow!("enabling pfctl: {}: {}", out, res.unwrap_err()));
            }
        }
        Ok(())
    }

    fn ensure_pf_enabled_with_reference() -> Result<()> {
        let token = read_pf_reference_token().unwrap_or_default();
        if !token.is_empty() && is_pf_reference_token_active(&token) {
            return ensure_pf_enabled();
        }

        let (output, res) = sudo_combined(&["pfctl", "-E"]);
        let out = trimmed(&output);
        if res.is_err() {
            return ensure_pf_enabled();
        }

        let token = parse_pf_enable_token(&out);
        if !token.is_empty() {
            let _ = fs::write(config::pf_token_path(), format!("{}\n", token));
        } else {
            let _ = fs::remove_file(config::pf_token_path());
        }
        Ok(())
    }

    fn release_pf_reference_token() -> Result<()> {
        let token = match read_pf_reference_token() {
            Ok(t) if !t.is_empty() => t,
            _ => return Ok(()),
        };

        let (output, res) = sudo_combined(&["pfctl", "-X", &token]);
        if res.is_err() {
            let out = String::from_utf8_lossy(&output).trim().to_lowercase();
            if !out.contains("token") {
                return Err(anyhow!(
                    "releasing pf token: {}: {}",
                    trimmed(&output),
                    res.unwrap_err()
                ));
            }
        }

        let _ = fs::remove_file(config::pf_token_path());
        Ok(())
    }

    impl PortForwarder for DarwinPortFwd {
        fn enable(&self) -> Result<()> {
            write_file_elevated(ANCHOR_FILE, &pf_rules())
                .map_err(|e| anyhow!("writing pf anchor: {}", e))?;

            let pf_conf =
                fs::read("/etc/pf.conf").map_err(|e| anyhow!("reading pf.conf: {}", e))?;
            let mut conf = String::from_utf8_lossy(&pf_conf).into_owned();

            let anchor_load = format!("rdr-anchor \"{}\"", ANCHOR_NAME);
            let anchor_rule = format!("load anchor \"{}\" from \"{}\"", ANCHOR_NAME, ANCHOR_FILE);

            let mut needs_update = false;
            if !conf.contains(&anchor_load) {
                let lines: Vec<&str> = conf.split('\n').collect();
                let mut updated: Vec<String> = Vec::new();
                let mut inserted = false;
                for line in lines {
                    updated.push(line.to_string());
                    if !inserted && line.starts_with("rdr-anchor") {
                        updated.push(anchor_load.clone());
                        inserted = true;
                    }
                }
                if !inserted {
                    updated.insert(0, anchor_load.clone());
                }
                conf = updated.join("\n");
                needs_update = true;
            }
            if !conf.contains(&anchor_rule) {
                conf = format!("{}\n{}\n", conf.trim_end_matches('\n'), anchor_rule);
                needs_update = true;
            }

            if needs_update {
                write_file_elevated("/etc/pf.conf", &conf)
                    .map_err(|e| anyhow!("writing pf.conf: {}", e))?;
            }

            ensure_pf_enabled_with_reference()?;

            let (output, res) = sudo_combined(&["pfctl", "-f", "/etc/pf.conf"]);
            if res.is_err() {
                return Err(anyhow!(
                    "loading pfctl rules: {}: {}",
                    trimmed(&output),
                    res.unwrap_err()
                ));
            }
            Ok(())
        }

        fn ensure_loaded(&self) -> Result<()> {
            // Reuse full enable flow so missing anchor wiring in pf.conf is
            // repaired, not just reloaded.
            self.enable()
        }

        fn disable(&self) -> Result<()> {
            release_pf_reference_token()?;

            let (output, res) = sudo_combined(&["rm", "-f", ANCHOR_FILE]);
            if res.is_err() {
                return Err(anyhow!(
                    "removing pf anchor: {}: {}",
                    trimmed(&output),
                    res.unwrap_err()
                ));
            }

            let pf_conf = match fs::read("/etc/pf.conf") {
                Ok(c) => c,
                Err(_) => return Ok(()),
            };
            let mut conf = String::from_utf8_lossy(&pf_conf).into_owned();

            let anchor_load = format!("rdr-anchor \"{}\"", ANCHOR_NAME);
            let anchor_rule = format!("load anchor \"{}\" from \"{}\"", ANCHOR_NAME, ANCHOR_FILE);

            conf = conf.replace(&format!("{}\n", anchor_load), "");
            conf = conf.replace(&format!("{}\n", anchor_rule), "");

            write_file_elevated("/etc/pf.conf", &conf)
                .map_err(|e| anyhow!("writing pf.conf: {}", e))?;

            let (output, res) = sudo_combined(&["pfctl", "-f", "/etc/pf.conf"]);
            if res.is_err() {
                return Err(anyhow!(
                    "reloading pfctl: {}: {}",
                    trimmed(&output),
                    res.unwrap_err()
                ));
            }
            Ok(())
        }

        fn is_enabled(&self) -> bool {
            fs::metadata(ANCHOR_FILE).is_ok()
        }

        fn is_loaded(&self) -> bool {
            let (info_output, res) = sudo_combined(&["pfctl", "-s", "info"]);
            if res.is_err() {
                return false;
            }
            if !is_pf_enabled_info_output(&String::from_utf8_lossy(&info_output)) {
                return false;
            }

            let (output, res) = sudo_combined(&["pfctl", "-a", ANCHOR_NAME, "-s", "nat"]);
            if res.is_err() {
                return false;
            }
            let out = String::from_utf8_lossy(&output);
            let out = out.trim();
            out.contains("rdr pass") && out.contains("port = 443")
        }
    }
}

#[cfg(target_os = "macos")]
pub use darwin::DarwinPortFwd;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_pf_already_enabled_output_cases() {
        // Port of TestIsPFAlreadyEnabledOutput.
        assert!(is_pf_already_enabled_output(
            "No ALTQ support in kernel\npfctl: pf already enabled"
        ));
        assert!(is_pf_already_enabled_output("PF Already Enabled"));
        assert!(!is_pf_already_enabled_output("pfctl: syntax error"));
    }

    #[test]
    fn is_pf_enabled_info_output_cases() {
        // Port of TestIsPFEnabledInfoOutput.
        assert!(is_pf_enabled_info_output(
            "Status: Enabled for 0 days 00:12:34           Debug: Urgent"
        ));
        assert!(is_pf_enabled_info_output("status: enabled"));
        assert!(!is_pf_enabled_info_output("Status: Disabled"));
        assert!(!is_pf_enabled_info_output("No ALTQ support in kernel"));
    }

    #[test]
    fn parse_pf_enable_token_cases() {
        // Port of TestParsePFEnableToken.
        assert_eq!(
            parse_pf_enable_token("pf enabled\nToken : 1272727272727272727"),
            "1272727272727272727"
        );
        assert_eq!(
            parse_pf_enable_token("Status: Enabled\nToken:   9999"),
            "9999"
        );
        assert_eq!(parse_pf_enable_token("pf already enabled"), "");
        assert_eq!(parse_pf_enable_token("Token 12345"), "");
    }

    #[test]
    fn has_pf_reference_token_cases() {
        // Port of TestHasPFReferenceToken.
        assert!(has_pf_reference_token(
            "PID 1234 token 55555\nPID 8888 token 99999",
            "55555"
        ));
        assert!(!has_pf_reference_token(
            "PID 1234 token 55555\nPID 8888 token 99999",
            "11111"
        ));
        assert!(!has_pf_reference_token("PID 1234 token 55555", ""));
    }

    #[test]
    fn iptables_chain_already_exists_cases() {
        assert!(iptables_chain_already_exists(
            b"iptables: Chain already exists."
        ));
        assert!(iptables_chain_already_exists(b"File exists"));
        assert!(!iptables_chain_already_exists(b"some other error"));
    }

    #[test]
    fn iptables_chain_missing_cases() {
        assert!(iptables_chain_missing(
            b"iptables: No chain/target/match by that name."
        ));
        assert!(iptables_chain_missing(b"does a matching rule exist"));
        assert!(iptables_chain_missing(b"not found"));
        assert!(!iptables_chain_missing(b"chain already exists"));
    }

    #[test]
    fn iptables_rule_absent_cases() {
        assert!(iptables_rule_absent(
            b"Bad rule (does a matching rule exist in that chain?)"
        ));
        assert!(iptables_rule_absent(b"No chain/target/match by that name"));
        assert!(iptables_rule_absent(b"not found"));
        assert!(!iptables_rule_absent(b"permission denied"));
    }
}
