// SPDX-License-Identifier: CC0-1.0

//! The RUNTIME API this emulator serves a running handler, at the address
//! `AWS_LAMBDA_RUNTIME_API` names.
//!
//! "AWS_LAMBDA_RUNTIME_API – … The host and port of the runtime API" is the interface a
//! managed runtime takes its next invocation from and reports a result to. This emulator
//! hands the invocation to the handler IN PROCESS rather than over that interface, so the
//! invocation half of it is served by nothing here — but the address is in the environment of
//! every run, and a runtime, an extension or a handler that reads it ADDRESSES it. What such
//! a request reaches is this module: the paths of that interface this emulator can answer
//! truthfully, served from the host ([`answered`]), and nothing else — a path no answer here
//! covers reaches no service at all, exactly as any other address this emulator does not
//! serve does ([`super::guest_http`]).
//!
//! One path is ANSWERED from the host's own state. `GET /2018-06-01/runtime/restore/next` is
//! where a SnapStart-aware runtime waits for the snapshot it is being restored from: AWS
//! answers it `404` when there is no snapshot being restored, which every invocation this
//! emulator runs is — handlers run from their own deployment package, and no execution
//! environment here is ever resumed from a snapshot. So the answer is that `404`, which is
//! what a runtime probing the endpoint reads, instead of a failure inside the sandbox that no
//! AWS runtime would see.
//!
//! Every OTHER path of the interface is answered as a path it does not ROUTE: the invocation
//! half of it is served by nothing here, so a caller reads that answer from the interface
//! rather than a resolution failure from underneath it, and nothing is answered for work
//! nothing did. (Serving the extension and telemetry paths is its own open report.)

use super::guest_http::Answered;

/// The address `AWS_LAMBDA_RUNTIME_API` names, which is the one the interface is addressed
/// at: the loopback interface and the port a managed runtime's environment publishes.
///
/// ONE spelling, shared with the environment every run is given
/// (`super::run_environment`), so the address a handler is told to use and the address this
/// module answers on cannot drift.
pub const RUNTIME_API_ADDRESS: &str = "127.0.0.1:9001";

/// The path a SnapStart-aware runtime waits for its restore on.
const RESTORE_NEXT_PATH: &str = "/2018-06-01/runtime/restore/next";

/// The status that path answers while no snapshot is being restored.
const NO_SNAPSHOT_STATUS: u16 = 404;

/// The status every other path of the interface answers: a path this interface does not
/// route, which is what every path of it but the one above is here.
const NOT_ROUTED_STATUS: u16 = 404;

/// The answer the runtime API gives `method` `url`, or `None` when the URL addresses
/// something other than the runtime API — which is an address this module says nothing
/// about.
#[must_use]
pub fn answered(method: &str, url: &str) -> Option<Answered> {
    let (authority, target) = url
        .split_once("://")
        .map(|(_scheme, rest)| match rest.find(['/', '?', '#']) {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, "/"),
        })?;
    if authority != RUNTIME_API_ADDRESS {
        return None;
    }
    let path = target.split(['?', '#']).next().unwrap_or(target);
    // A method is case-sensitive (RFC 9110 §9.1), and the interface binds this path to a
    // read: another method on it is another request, and one this interface does not route.
    let status = if method == "GET" && path == RESTORE_NEXT_PATH {
        NO_SNAPSHOT_STATUS
    } else {
        NOT_ROUTED_STATUS
    };
    Some(Answered {
        status,
        headers: Vec::new(),
        body: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The restore endpoint answers "no snapshot" on the runtime API's own address, every
    /// other path of that interface is answered as one it does not route, and no other
    /// address is answered here at all. The rows span what the reading generalises over: the
    /// path, the method, the address, and a URL that is no URL at all.
    #[test]
    fn the_runtime_apis_restore_endpoint_answers_no_snapshot() {
        let served = format!("http://{RUNTIME_API_ADDRESS}{RESTORE_NEXT_PATH}");
        // (case, the method, the URL, the status answered, if any)
        let cases: &[(&str, &str, &str, Option<u16>)] = &[
            ("the restore endpoint", "GET", &served, Some(404)),
            (
                "the restore endpoint with a query",
                "GET",
                &format!("{served}?probe=1"),
                Some(404),
            ),
            ("another method on it", "POST", &served, Some(404)),
            (
                "another path of the interface",
                "GET",
                &format!("http://{RUNTIME_API_ADDRESS}/2018-06-01/runtime/invocation/next"),
                Some(404),
            ),
            (
                "the interface's own root",
                "GET",
                &format!("http://{RUNTIME_API_ADDRESS}"),
                Some(404),
            ),
            (
                "the same path on another address",
                "GET",
                &format!("http://127.0.0.1:9297{RESTORE_NEXT_PATH}"),
                None,
            ),
            ("a value that is no URL", "GET", RESTORE_NEXT_PATH, None),
        ];
        for (case, method, url, expected) in cases {
            let answer = answered(method, url).map(|answer| answer.status);
            assert_eq!(
                answer, *expected,
                "case {case:?}: expected {expected:?}, got {answer:?}"
            );
        }
    }
}
