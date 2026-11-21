// SPDX-License-Identifier: CC0-1.0

//! The RUNTIME API this emulator serves a running handler, at the address
//! `AWS_LAMBDA_RUNTIME_API` names.
//!
//! "AWS_LAMBDA_RUNTIME_API – … The host and port of the runtime API" is the interface a
//! managed runtime takes its next invocation from and reports a result to, and the one an
//! EXTENSION registers with the execution environment at. This emulator hands the invocation
//! to the handler IN PROCESS rather than over that interface, so the invocation half of it is
//! served by nothing here — but the address is in the environment of every run, and a
//! runtime, an extension or a handler that reads it ADDRESSES it. What such a request reaches
//! is this module: the paths of that interface this emulator can answer truthfully, served
//! from the host and from the invocation's own state ([`answered`]), and nothing else — a path
//! no answer here covers reaches no service at all, exactly as any other address this emulator
//! does not serve does ([`super::guest_http`]).
//!
//! What is SERVED here:
//!
//! * The EXTENSIONS API ([`extensions`]): an extension registers, is minted the identifier its
//!   later requests carry, and reports its own failures. The registration is state of the
//!   invocation whose sandbox it was made in ([`sandbox::Sandbox`]), so the identifier one call
//!   mints is what the next call is read against.
//! * The TELEMETRY API ([`telemetry`]): a registered extension subscribes to the environment's
//!   telemetry streams. The subscribe body is judged against every published rule of its
//!   members, and a body that breaks them is answered with the status and the error document
//!   AWS words for that class.
//! * `GET /2018-06-01/runtime/restore/next`, where a SnapStart-aware runtime waits for the
//!   snapshot it is being restored from: AWS answers it `404` when there is no snapshot being
//!   restored, which every invocation this emulator runs is — handlers run from their own
//!   deployment package, and no execution environment here is ever resumed from a snapshot. So
//!   the answer is that `404`, which is what a runtime probing the endpoint reads, instead of a
//!   failure inside the sandbox that no AWS runtime would see.
//!
//! Every OTHER path of the interface is answered as a path it does not ROUTE: the invocation
//! half of it is served by nothing here, so a caller reads that answer from the interface
//! rather than a resolution failure from underneath it, and nothing is answered for work
//! nothing did.

use ::serde_json::{json, Value};

use super::guest_http::Answered;

pub mod extensions;
pub mod sandbox;
pub mod telemetry;

pub use sandbox::{FunctionFacts, Sandbox};

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
/// route, which is what every path of it this module does not serve is here.
const NOT_ROUTED_STATUS: u16 = 404;

/// ONE request addressed at the runtime API, as the host is handed it.
#[derive(Debug)]
pub struct Requested<'a> {
    /// The method it was made with, upper-cased: a method is case-sensitive (RFC 9110 §9.1),
    /// and the interface binds each of its paths to one method.
    pub method: &'a str,
    /// The URL it addressed.
    pub url: &'a str,
    /// The headers it carried, as name/value pairs.
    pub headers: &'a [(String, String)],
    /// The body it carried.
    pub body: &'a [u8],
    /// The state of the execution environment it was made from: the registrations of the
    /// invocation whose sandbox the caller is in. `None` for a request made from no sandbox
    /// of this emulator at all.
    pub sandbox: Option<&'a Sandbox>,
}

/// What ONE request addressed at this interface came to.
#[derive(Debug)]
pub enum Reached {
    /// The interface answered it, with this answer.
    Answered(Answered),
    /// The request reached the interface and is never answered: a long poll for an event the
    /// caller is owed none of (see [`extensions`]). It is not an answer withheld — there is
    /// no answer to give — so it is reported as a request that does not complete.
    NeverAnswered,
    /// The URL addresses something other than this interface, which this module says nothing
    /// about.
    Elsewhere,
}

/// What the runtime API answers `request`.
#[must_use]
pub fn answered(request: &Requested<'_>) -> Reached {
    let Some((authority, target)) = request.url.split_once("://").map(|(_scheme, rest)| {
        match rest.find(['/', '?', '#']) {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, "/"),
        }
    }) else {
        return Reached::Elsewhere;
    };
    if authority != RUNTIME_API_ADDRESS {
        return Reached::Elsewhere;
    }
    let path = target.split(['?', '#']).next().unwrap_or(target);
    if (request.method, path) == ("GET", RESTORE_NEXT_PATH) {
        return Reached::Answered(Answered {
            status: NO_SNAPSHOT_STATUS,
            headers: Vec::new(),
            body: Vec::new(),
        });
    }
    match extensions::answered(request, path) {
        Reached::Elsewhere => {}
        reached => return reached,
    }
    match telemetry::answered(request, path) {
        Reached::Elsewhere => {}
        reached => return reached,
    }
    Reached::Answered(Answered {
        status: NOT_ROUTED_STATUS,
        headers: Vec::new(),
        body: Vec::new(),
    })
}

/// The first value of the header `name` a request carries, or `None` for one it does not.
///
/// A field name is case-insensitive (RFC 9110 §5.1), so a caller that spells
/// `Lambda-Extension-Identifier` in any case names the same header. It is the ONE lookup over
/// a request's name/value pairs this crate keeps: a function URL's event reads the headers of
/// its own request through it too (`crate::service::function_url::event`), so no two readers
/// of a header list can disagree about which header a name names.
pub(crate) fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(held, _value)| held.eq_ignore_ascii_case(name))
        .map(|(_held, value)| value.as_str())
}

/// An answer carrying `document` as its body, under the media type a JSON document is served
/// as.
fn json_answer(status: u16, document: &Value) -> Answered {
    Answered {
        status,
        headers: vec![("Content-Type".to_string(), "application/json".to_string())],
        body: ::serde_json::to_vec(document).unwrap_or_default(),
    }
}

/// The answer a request this interface refuses gets: its status, and the error document the
/// interface words for that class — the two members every error of it carries.
fn errored(status: u16, error_type: &str, message: &str) -> Answered {
    json_answer(
        status,
        &json!({ "errorType": error_type, "errorMessage": message }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request addressed at the interface, carrying nothing and made from no sandbox.
    fn probing<'a>(method: &'a str, url: &'a str) -> Requested<'a> {
        Requested {
            method,
            url,
            headers: &[],
            body: &[],
            sandbox: None,
        }
    }

    /// The status the interface answered, or `None` where the URL addresses something else
    /// and `Some(0)` for a request it never answers.
    fn status(reached: Reached) -> Option<u16> {
        match reached {
            Reached::Answered(answer) => Some(answer.status),
            Reached::NeverAnswered => Some(0),
            Reached::Elsewhere => None,
        }
    }

    /// The restore endpoint answers "no snapshot" on the runtime API's own address, every
    /// path of that interface this module does not serve is answered as one it does not
    /// route, and no other address is answered here at all. The rows span what the reading
    /// generalises over: the path, the method, the address, and a URL that is no URL at all.
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
            let answer = status(answered(&probing(method, url)));
            assert_eq!(
                answer, *expected,
                "case {case:?}: expected {expected:?}, got {answer:?}"
            );
        }
    }

    /// The Extensions and Telemetry API are served on that same address: a register call is
    /// answered with the identifier it minted and the function's own facts, every later call
    /// is read against that registration, and a call from an extension this sandbox minted no
    /// identifier for is refused.
    #[test]
    fn an_extension_registers_and_is_read_back_by_its_identifier() {
        let sandbox = Sandbox::new(FunctionFacts {
            function_name: "probe".to_string(),
            function_version: "$LATEST".to_string(),
            handler: "lambda_function.lambda_handler".to_string(),
            account_id: "000000000000".to_string(),
        });
        let registering = Requested {
            method: "POST",
            url: &format!("http://{RUNTIME_API_ADDRESS}{}", extensions::REGISTER_PATH),
            headers: &[(
                "Lambda-Extension-Name".to_string(),
                "telemetry-probe".to_string(),
            )],
            body: br#"{"events":["INVOKE","SHUTDOWN"]}"#,
            sandbox: Some(&sandbox),
        };
        let Reached::Answered(registered) = answered(&registering) else {
            panic!("case \"register\": expected an answer, got none");
        };
        assert_eq!(
            registered.status, 200,
            "case \"register\": expected 200, got {}",
            registered.status
        );
        let identifier = registered
            .headers
            .iter()
            .find(|(name, _value)| name == extensions::IDENTIFIER_HEADER)
            .map(|(_name, value)| value.clone())
            .expect("the answer must name the identifier it minted");
        assert_eq!(
            ::serde_json::from_slice::<Value>(&registered.body).expect("a JSON body"),
            json!({
                "functionName": "probe",
                "functionVersion": "$LATEST",
                "handler": "lambda_function.lambda_handler",
                "accountId": "000000000000",
            }),
            "case \"register\": expected the function's own facts, got {}",
            String::from_utf8_lossy(&registered.body)
        );
        assert_eq!(
            sandbox
                .registration(&identifier)
                .map(|held| held.name)
                .as_deref(),
            Some("telemetry-probe"),
            "case \"register\": expected the registration to be held, got none"
        );

        let body = ::serde_json::to_vec(&json!({
            "schemaVersion": "2022-12-13",
            "types": ["platform"],
            "destination": { "protocol": "HTTP", "URI": "http://sandbox.localdomain:8080/x" },
        }))
        .expect("the body must serialize");
        let url = format!("http://{RUNTIME_API_ADDRESS}{}", telemetry::SUBSCRIBE_PATH);
        // (case, the identifier the subscribe carries, the status it is answered with)
        let rows: &[(&str, &str, u16)] = &[
            ("the identifier the register minted", &identifier, 200),
            ("an identifier nothing minted", "not-an-identifier", 403),
        ];
        for (case, carried, expected) in rows {
            let headers = vec![(
                extensions::IDENTIFIER_HEADER.to_string(),
                (*carried).to_string(),
            )];
            let answer = status(answered(&Requested {
                method: "PUT",
                url: &url,
                headers: &headers,
                body: &body,
                sandbox: Some(&sandbox),
            }));
            assert_eq!(
                answer,
                Some(*expected),
                "case {case:?}: expected {expected:?}, got {answer:?}"
            );
        }
        assert!(
            sandbox
                .registration(&identifier)
                .and_then(|held| held.telemetry)
                .is_some(),
            "case \"the subscribed registration\": expected its subscription to be held, got \
             none"
        );
    }

    /// Every path of these two APIs is routed, and each is read against the registration the
    /// request names itself with: an error report of a registered extension is accepted, the
    /// event pump of one is a request that is never answered (`Some(0)` below), and a request
    /// naming no registration is refused whichever path it is on.
    ///
    /// The rows differ along the path, the method it is bound to, and whether the caller is an
    /// extension this sandbox registered.
    #[test]
    fn every_path_of_the_two_apis_is_read_against_the_registration_it_names() {
        let sandbox = Sandbox::new(FunctionFacts {
            function_name: "probe".to_string(),
            function_version: "$LATEST".to_string(),
            handler: "lambda_function.lambda_handler".to_string(),
            account_id: "000000000000".to_string(),
        });
        let registered = sandbox.register("telemetry-probe", vec!["INVOKE".to_string()]);
        let class = (
            "Lambda-Extension-Function-Error-Type".to_string(),
            "Extension.TestError".to_string(),
        );
        // (case, the method, the path, the identifier the request carries, the status answered)
        let rows: &[(&str, &str, &str, &str, u16)] = &[
            (
                "the event pump of a registered extension",
                "GET",
                extensions::EVENT_NEXT_PATH,
                &registered,
                0,
            ),
            (
                "the event pump of an extension nothing registered",
                "GET",
                extensions::EVENT_NEXT_PATH,
                "not-an-identifier",
                403,
            ),
            (
                "an initialization failure reported by a registered extension",
                "POST",
                extensions::INIT_ERROR_PATH,
                &registered,
                202,
            ),
            (
                "an exit failure reported by a registered extension",
                "POST",
                extensions::EXIT_ERROR_PATH,
                &registered,
                202,
            ),
            (
                "an exit failure reported by an extension nothing registered",
                "POST",
                extensions::EXIT_ERROR_PATH,
                "not-an-identifier",
                403,
            ),
            (
                "another method on the event pump",
                "POST",
                extensions::EVENT_NEXT_PATH,
                &registered,
                404,
            ),
        ];
        for (case, method, path, carried, expected) in rows {
            let headers = vec![
                (
                    extensions::IDENTIFIER_HEADER.to_string(),
                    (*carried).to_string(),
                ),
                class.clone(),
            ];
            let answer = status(answered(&Requested {
                method,
                url: &format!("http://{RUNTIME_API_ADDRESS}{path}"),
                headers: &headers,
                body: &[],
                sandbox: Some(&sandbox),
            }));
            assert_eq!(
                answer,
                Some(*expected),
                "case {case:?}: expected {expected:?}, got {answer:?}"
            );
        }
        // And an error report that states no class of failure is refused as one.
        let headers = vec![(
            extensions::IDENTIFIER_HEADER.to_string(),
            registered.clone(),
        )];
        let answer = status(answered(&Requested {
            method: "POST",
            url: &format!("http://{RUNTIME_API_ADDRESS}{}", extensions::INIT_ERROR_PATH),
            headers: &headers,
            body: &[],
            sandbox: Some(&sandbox),
        }));
        assert_eq!(
            answer,
            Some(400),
            "case \"a failure report naming no error type\": expected 400, got {answer:?}"
        );
        // A request made from no sandbox at all reaches an environment that cannot serve it.
        let answer = status(answered(&probing(
            "POST",
            &format!("http://{RUNTIME_API_ADDRESS}{}", extensions::REGISTER_PATH),
        )));
        assert_eq!(
            answer,
            Some(500),
            "case \"a register call from no sandbox\": expected 500, got {answer:?}"
        );
    }
}
