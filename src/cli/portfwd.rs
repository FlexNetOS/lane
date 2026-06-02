//! Port-forwarding reload helpers — placeholder; implemented in the CLI layer.

pub(crate) fn ingress_ports_reachable() -> bool {
    false
}

pub(crate) fn should_reload_port_forwarding(
    _pf: &dyn crate::system::PortForwarder,
    _daemon_running: bool,
) -> bool {
    false
}
