//! Port-occupancy pre-flight checks.
//!
//! Checked before `start` invokes Docker Compose (or launches native
//! processes) so an operator gets an actionable "port already in use"
//! error up front, rather than a more cryptic failure surfaced later by
//! Docker or the underlying service itself.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};

/// The ports this bundle's services bind to on the host by default:
/// Collector OTLP/HTTP, Tempo query API, Grafana.
pub const DEFAULT_PORTS: &[(&str, u16)] = &[
    ("collector (OTLP/HTTP)", 4318),
    ("tempo (query API)", 3200),
    ("grafana", 3000),
];

/// Return `true` if `port` is free to bind on all interfaces -- both the
/// IPv4 and IPv6 wildcard addresses. Checking only IPv4 (as this used to)
/// would miss a service already bound solely to `[::]:port`, incorrectly
/// reporting the port as free.
pub fn is_port_free(port: u16) -> bool {
    let v4_free = TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))).is_ok();
    let v6_free = TcpListener::bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, port))).is_ok();
    v4_free && v6_free
}

/// Check every port in `ports`, returning the names/ports already occupied.
pub fn find_occupied_ports(ports: &[(&str, u16)]) -> Vec<(String, u16)> {
    ports
        .iter()
        .filter(|(_, port)| !is_port_free(*port))
        .map(|(name, port)| (name.to_string(), *port))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_free_ephemeral_port_is_reported_free() {
        // Bind to port 0 to get an OS-assigned free port, then release it
        // immediately before checking -- there's a small window where
        // another process could grab it, but that's true of any
        // port-availability check and not specific to this function.
        let listener = TcpListener::bind("0.0.0.0:0").expect("should bind an ephemeral port");
        let port = listener
            .local_addr()
            .expect("should have a local addr")
            .port();
        drop(listener);

        assert!(is_port_free(port));
    }

    #[test]
    fn a_bound_port_is_reported_occupied() {
        let listener = TcpListener::bind("0.0.0.0:0").expect("should bind an ephemeral port");
        let port = listener
            .local_addr()
            .expect("should have a local addr")
            .port();

        assert!(!is_port_free(port));
        drop(listener);
    }

    #[test]
    fn a_port_occupied_only_on_ipv6_is_reported_occupied() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)))
            .expect("should bind an ephemeral IPv6 port");
        let port = listener
            .local_addr()
            .expect("should have a local addr")
            .port();

        assert!(!is_port_free(port));
        drop(listener);
    }

    #[test]
    fn find_occupied_ports_reports_only_the_bound_one() {
        let listener = TcpListener::bind("0.0.0.0:0").expect("should bind an ephemeral port");
        let occupied_port = listener
            .local_addr()
            .expect("should have a local addr")
            .port();
        let free_listener = TcpListener::bind("0.0.0.0:0").expect("should bind an ephemeral port");
        let free_port = free_listener
            .local_addr()
            .expect("should have a local addr")
            .port();
        drop(free_listener);

        let result = find_occupied_ports(&[("occupied", occupied_port), ("free", free_port)]);
        assert_eq!(result, vec![("occupied".to_string(), occupied_port)]);
    }
}
