// SPDX-License-Identifier: CC0-1.0

//! The CHECK one `HealthCheck$Target` describes: which port of the instance a check connects
//! to, what it asks for there, and what counts as an answer.
//!
//! `HealthCheckTarget` is a plain string in the model, so its grammar and its meaning are
//! documented rather than modelled, in one paragraph of `HealthCheck$Target`:
//!
//! * "The protocol is either TCP, HTTP, HTTPS, or SSL. The range of valid ports is one (1)
//!   through 65535."
//! * "TCP is the default, specified as a TCP: port pair, for example \"TCP:5000\". In this
//!   case, a health check simply attempts to open a TCP connection to the instance on the
//!   specified port. Failure to connect within the configured timeout is considered
//!   unhealthy." — "SSL is also specified as SSL: port pair".
//! * "For HTTP/HTTPS, you must include a ping path in the string… for example
//!   \"HTTP:80/weather/us/wa/seattle\". In this case, a HTTP GET request is issued to the
//!   instance on the given port and path. Any answer other than \"200 OK\" within the timeout
//!   period is considered unhealthy."
//! * "The total length of the HTTP ping target must be 1024 16-bit Unicode characters or
//!   less."
//!
//! ONE reading of that paragraph, taken by both halves of the service: `ConfigureHealthCheck`
//! refuses a target that is not one of these ([`rejection`] states which rule it broke), and
//! each periodic check of a registered instance performs the check this one describes
//! ([`super::instance_checks`]). So a target the service accepted is a target it knows how to
//! perform, and the grammar cannot be read two ways.

/// The protocols a health-check target may name, from `HealthCheck$Target`: "The protocol
/// is either TCP, HTTP, HTTPS, or SSL." Listed in the order this service's own refusal
/// names them, which is what [`protocol_refusal`] states.
pub const TARGET_PROTOCOLS: &[&str] = &["HTTP", "TCP", "HTTPS", "SSL"];

/// The ports a health-check target may name, from the same paragraph: "The range of valid
/// ports is one (1) through 65535."
pub const TARGET_PORTS: (i32, i32) = (1, 65535);

/// The longest HTTP/HTTPS target: "The total length of the HTTP ping target must be 1024
/// 16-bit Unicode characters or less."
pub const MAX_HTTP_TARGET_CHARS: usize = 1024;

/// The only answer an HTTP or HTTPS check counts as a success: "Any answer other than
/// \"200 OK\" within the timeout period is considered unhealthy."
pub const HEALTH_CHECK_OK_STATUS: u16 = 200;

/// The method an HTTP or HTTPS check asks with: "a HTTP GET request is issued to the instance
/// on the given port and path."
pub const HEALTH_CHECK_METHOD: &str = "GET";

/// What ONE health-check target says a check of an instance does.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum HealthCheckTarget {
    /// A `TCP:` or `SSL:` port pair: the check "simply attempts to open a TCP connection to
    /// the instance on the specified port", so the port is the whole of it.
    Connect { port: i32 },
    /// An `HTTP:`/`HTTPS:` port and ping path: the check issues "a HTTP GET request … on the
    /// given port and path", and only a `200` answer is a success
    /// ([`HEALTH_CHECK_OK_STATUS`]).
    Request {
        port: i32,
        /// The ping path, as a request path — the `/`-prefixed path the target names after
        /// its port.
        path: String,
    },
}

impl HealthCheckTarget {
    /// The port a check of this target connects to.
    pub fn port(&self) -> i32 {
        match self {
            Self::Connect { port } | Self::Request { port, .. } => *port,
        }
    }

    /// The check `target` describes, or `None` when it is not a health-check target at all
    /// ([`rejection`] says why). A stored target has been accepted by `ConfigureHealthCheck`
    /// or seeded by the create, so `None` is a value nothing of this service produced.
    pub fn of(target: &str) -> Option<Self> {
        parsed(target).ok()
    }
}

/// Why `target` is not a health-check target, or the check it describes.
///
/// The message names the member and the rule it broke, which is what a caller fixes the
/// request from.
pub fn rejection(target: &str) -> Result<HealthCheckTarget, String> {
    parsed(target)
}

/// The refusal a target that does not begin with one of [`TARGET_PROTOCOLS`] earns: a
/// target is "a protocol:port pair", so a value that names no protocol at all and one that
/// names something else both break the same rule, and the sentence states that rule with
/// the protocols a target may begin with.
pub fn protocol_refusal() -> String {
    format!(
        "HealthCheck Target must begin with one of {}",
        TARGET_PROTOCOLS.join(", ")
    )
}

/// The one reading of the grammar: the check `target` describes, or the rule it broke.
fn parsed(target: &str) -> Result<HealthCheckTarget, String> {
    let Some((protocol, rest)) = target.split_once(':') else {
        return Err(protocol_refusal());
    };
    let protocol = protocol.to_ascii_uppercase();
    if !TARGET_PROTOCOLS.contains(&protocol.as_str()) {
        return Err(protocol_refusal());
    }

    // An HTTP or HTTPS target carries a ping path after the port; a TCP or SSL target is
    // the port alone, so a '/' in it is not a path but a malformed port.
    let is_http = matches!(protocol.as_str(), "HTTP" | "HTTPS");
    let (port, path) = match rest.split_once('/') {
        Some((port, path)) => (port, Some(path)),
        None => (rest, None),
    };
    match (is_http, path) {
        (true, None) => {
            return Err(format!(
                "HealthCheck.Target must include a ping path for {protocol}, as in \
                 '{protocol}:80/index.html'; got '{target}'"
            ))
        }
        (false, Some(_)) => {
            return Err(format!(
                "HealthCheck.Target must be '{protocol}:<port>' with no ping path; got \
                 '{target}'"
            ))
        }
        _ => {}
    }
    if is_http && target.chars().count() > MAX_HTTP_TARGET_CHARS {
        return Err(format!(
            "HealthCheck.Target must be at most {MAX_HTTP_TARGET_CHARS} characters; got {}",
            target.chars().count()
        ));
    }

    let (low, high) = TARGET_PORTS;
    let port = match port.parse::<i32>() {
        Ok(port) if (low..=high).contains(&port) => port,
        _ => {
            return Err(format!(
                "HealthCheck.Target port must be a number from {low} through {high}; got \
                 '{port}'"
            ))
        }
    };
    Ok(match path {
        // The target names the ping path after the port, and a request path begins at the
        // root: "HTTP:80/weather/us/wa/seattle" asks for `/weather/us/wa/seattle`.
        Some(path) => HealthCheckTarget::Request {
            port,
            path: format!("/{path}"),
        },
        None => HealthCheckTarget::Connect { port },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a target names a protocol of [`TARGET_PROTOCOLS`] and a port in
    /// [`TARGET_PORTS`], with a ping path for HTTP and HTTPS and none for TCP and SSL — and
    /// the check it describes is a connection to that port, or a GET of that path on it.
    ///
    /// The rows span the four protocols, either case of one, both published port bounds and
    /// one past each, a missing and a surplus ping path, and a path with several segments.
    #[test]
    fn a_target_describes_the_check_its_protocol_and_port_name() {
        let rows: &[(&str, &str, Option<HealthCheckTarget>)] = &[
            (
                "a TCP port pair",
                "TCP:5000",
                Some(HealthCheckTarget::Connect { port: 5000 }),
            ),
            (
                "an SSL port pair",
                "SSL:443",
                Some(HealthCheckTarget::Connect { port: 443 }),
            ),
            (
                "a lower-case protocol",
                "tcp:80",
                Some(HealthCheckTarget::Connect { port: 80 }),
            ),
            (
                "the lowest valid port",
                "TCP:1",
                Some(HealthCheckTarget::Connect { port: 1 }),
            ),
            (
                "the highest valid port",
                "TCP:65535",
                Some(HealthCheckTarget::Connect { port: 65535 }),
            ),
            ("one below the lowest valid port", "TCP:0", None),
            ("one above the highest valid port", "TCP:65536", None),
            (
                "an HTTP target with a multi-segment ping path",
                "HTTP:80/weather/us/wa/seattle",
                Some(HealthCheckTarget::Request {
                    port: 80,
                    path: "/weather/us/wa/seattle".to_string(),
                }),
            ),
            (
                "an HTTPS target",
                "HTTPS:8443/ping",
                Some(HealthCheckTarget::Request {
                    port: 8443,
                    path: "/ping".to_string(),
                }),
            ),
            (
                "an HTTP target whose ping path is the root",
                "HTTP:80/",
                Some(HealthCheckTarget::Request {
                    port: 80,
                    path: "/".to_string(),
                }),
            ),
            ("an HTTP target with no ping path", "HTTP:80", None),
            ("a TCP target carrying a ping path", "TCP:80/ping", None),
            ("a target naming another protocol", "UDP:80", None),
            ("a target naming no protocol at all", "80", None),
        ];
        for (case, target, expected) in rows {
            let described = HealthCheckTarget::of(target);
            assert_eq!(
                described, *expected,
                "case {case:?}: expected {target:?} to describe {expected:?}, got \
                 {described:?}"
            );
        }
    }
}
