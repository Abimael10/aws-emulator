// SPDX-License-Identifier: CC0-1.0

//! The DECISION a connection function takes, as `TestConnectionFunction` reports it, end to
//! end over HTTP.
//!
//! The rule: a test of a connection function reports what the run produced — the connection
//! as the code left it, carrying the decision the code took on it — so two runs that decide
//! DIFFERENTLY about the same connection report different outputs, and a run that raised
//! reports the engine's message for that raise rather than a clean result. The code reaches
//! the connection the way the runtime hands it over: as the global `connection` its body
//! reads, and as the argument of the function its code declares, which the runtime calls.
//!
//! Why it matters: a test is the only way to validate a connection function before publishing
//! it, so a caller has to be able to tell an allowing function from a denying one, and a
//! broken function from a working one.
//!
//! The rows differ along what the rule generalises over: WHICH decision the code takes
//! (allow, deny, none), HOW the code is written (deciding in its own body, declaring the
//! function the runtime calls — under the runtime's name, under another name, bound to a
//! `var`, or as the function its body is — deciding from inside a promise), whether it raises
//! (in its body or in the function it declared), and what it did to the connection before
//! deciding. The harness is the sibling connection function suites', reused rather than
//! copied.

use ::serde_json::Value as Json;

use crate::connection_function_test_run::{
    create_one, result_logs, result_text, test_connection_function, CONNECTION_OBJECT,
};
use crate::distribution_lifecycle::emulator;

/// One run: the code the connection function was stored with, and what the result must
/// report about the decision it took.
struct Decision {
    case: &'static str,
    code: &'static str,
    /// The connection object the request carries, as the JSON text a client submits.
    connection_object: &'static str,
    /// The decision the output document must name, or `None` for a run that took none.
    decision: Option<&'static str>,
    /// WHERE the output document records that decision — on the connection object itself, or
    /// on the connection it wraps — as a JSON pointer.
    decision_at: &'static str,
    /// A fragment the reported error message must contain, or `None` where the run must report
    /// no error at all.
    error: Option<&'static str>,
    /// The log lines the result must report, in order.
    logs: Vec<&'static str>,
}

/// Where a decision taken on the connection object itself is recorded.
const DECISION_ON_THE_OBJECT: &str = "/decision";

/// Every rule below is asserted over these runs.
fn decision_table() -> Vec<Decision> {
    vec![
        Decision {
            case: "code that allows the connection in its own body",
            code: "connection.allow();\n",
            connection_object: CONNECTION_OBJECT,
            decision: Some("allow"),
            decision_at: DECISION_ON_THE_OBJECT,
            error: None,
            logs: Vec::new(),
        },
        Decision {
            case: "code that denies the connection in its own body",
            code: "connection.deny();\n",
            connection_object: CONNECTION_OBJECT,
            decision: Some("deny"),
            decision_at: DECISION_ON_THE_OBJECT,
            error: None,
            logs: Vec::new(),
        },
        Decision {
            case: "a handler that allows the connection it is handed",
            code: "function handler(connection) { connection.allow(); }\n",
            connection_object: CONNECTION_OBJECT,
            decision: Some("allow"),
            decision_at: DECISION_ON_THE_OBJECT,
            error: None,
            logs: Vec::new(),
        },
        Decision {
            case: "a handler that denies the connection it is handed",
            code: "function handler(connection) { connection.deny(); }\n",
            connection_object: CONNECTION_OBJECT,
            decision: Some("deny"),
            decision_at: DECISION_ON_THE_OBJECT,
            error: None,
            logs: Vec::new(),
        },
        Decision {
            case: "a handler bound to a var that denies the connection",
            code: "var handler = function (connection) { connection.deny(); };\n",
            connection_object: CONNECTION_OBJECT,
            decision: Some("deny"),
            decision_at: DECISION_ON_THE_OBJECT,
            error: None,
            logs: Vec::new(),
        },
        Decision {
            case: "the single function the code declares, under another name",
            code: "function onConnection(connection) { connection.deny(); }\n",
            connection_object: CONNECTION_OBJECT,
            decision: Some("deny"),
            decision_at: DECISION_ON_THE_OBJECT,
            error: None,
            logs: Vec::new(),
        },
        Decision {
            case: "a function the code declares beside a helper it calls",
            code: "function allowed(connection) { return connection.viewer.ip.length > 0; }\n\
                   function decide(connection) {\n\
                   \x20   if (allowed(connection)) { connection.allow(); } else { connection.deny(); }\n\
                   }\n\
                   globalThis.handler = decide;\n",
            connection_object: CONNECTION_OBJECT,
            decision: Some("allow"),
            decision_at: DECISION_ON_THE_OBJECT,
            error: None,
            logs: Vec::new(),
        },
        Decision {
            case: "code whose whole body is the function it declares",
            code: "(function (connection) { connection.allow(); })\n",
            connection_object: CONNECTION_OBJECT,
            decision: Some("allow"),
            decision_at: DECISION_ON_THE_OBJECT,
            error: None,
            logs: Vec::new(),
        },
        Decision {
            case: "a handler that decides from what the connection carries",
            code: "function handler(connection) {\n\
                   \x20   if (connection.viewer.tls.version === 'TLSv1.3') {\n\
                   \x20       connection.allow();\n\
                   \x20   } else {\n\
                   \x20       connection.deny();\n\
                   \x20   }\n\
                   }\n",
            connection_object: CONNECTION_OBJECT,
            decision: Some("allow"),
            decision_at: DECISION_ON_THE_OBJECT,
            error: None,
            logs: Vec::new(),
        },
        Decision {
            case: "a handler that denies what the connection carries",
            code: "function handler(connection) {\n\
                   \x20   if (connection.viewer.tls.sni === 'example.com') {\n\
                   \x20       connection.deny();\n\
                   \x20       return;\n\
                   \x20   }\n\
                   \x20   connection.allow();\n\
                   }\n",
            connection_object: CONNECTION_OBJECT,
            decision: Some("deny"),
            decision_at: DECISION_ON_THE_OBJECT,
            error: None,
            logs: Vec::new(),
        },
        Decision {
            case: "a handler that denies after logging",
            code: "function handler(connection) {\n\
                   \x20   console.log('denying', connection.viewer.ip);\n\
                   \x20   connection.deny();\n\
                   }\n",
            connection_object: CONNECTION_OBJECT,
            decision: Some("deny"),
            decision_at: DECISION_ON_THE_OBJECT,
            error: None,
            logs: vec!["denying 198.51.100.11"],
        },
        Decision {
            case: "a handler that denies from inside a promise",
            code: "function handler(connection) {\n\
                   \x20   return Promise.resolve().then(function () { connection.deny(); });\n\
                   }\n",
            connection_object: CONNECTION_OBJECT,
            decision: Some("deny"),
            decision_at: DECISION_ON_THE_OBJECT,
            error: None,
            logs: Vec::new(),
        },
        Decision {
            case: "a handler that raises before deciding",
            code: "function handler(connection) {\n\
                   \x20   throw new Error('no route for ' + connection.viewer.tls.sni);\n\
                   }\n",
            connection_object: CONNECTION_OBJECT,
            decision: None,
            decision_at: DECISION_ON_THE_OBJECT,
            error: Some("Error: no route for example.com"),
            logs: Vec::new(),
        },
        Decision {
            case: "a handler that raises after logging",
            code: "function handler(connection) {\n\
                   \x20   console.log('about to fail');\n\
                   \x20   throw new TypeError('the connection has no route');\n\
                   }\n",
            connection_object: CONNECTION_OBJECT,
            decision: None,
            decision_at: DECISION_ON_THE_OBJECT,
            error: Some("TypeError: the connection has no route"),
            logs: vec!["about to fail"],
        },
        Decision {
            case: "a handler whose promise rejects",
            code: "function handler(connection) {\n\
                   \x20   return Promise.reject(new Error('rejected the connection'));\n\
                   }\n",
            connection_object: CONNECTION_OBJECT,
            decision: None,
            decision_at: DECISION_ON_THE_OBJECT,
            error: Some("rejected the connection"),
            logs: Vec::new(),
        },
        Decision {
            case: "code that decides about the connection in scope",
            code: "connection.allow();\n",
            connection_object: CONNECTION_OBJECT,
            decision: Some("allow"),
            decision_at: DECISION_ON_THE_OBJECT,
            error: None,
            logs: Vec::new(),
        },
        Decision {
            case: "code that reaches a member the connection does not carry",
            code: "connection.missing.member = 1;\n",
            connection_object: CONNECTION_OBJECT,
            decision: None,
            decision_at: DECISION_ON_THE_OBJECT,
            error: Some("TypeError"),
            logs: Vec::new(),
        },
    ]
}

/// `text` as JSON, or a panic naming the case.
fn json(text: &str, case: &str) -> Json {
    ::serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("case {case}: expected JSON, got {text:?} ({error})"))
}

/// A test reports the decision the code took and the connection it took it on; a run that
/// raised reports the engine's message for that raise and no decision at all.
#[tokio::test]
async fn a_test_reports_the_decision_the_code_took() {
    let router = emulator();
    let mut outputs: Vec<(&str, &str, Option<&str>, String)> = Vec::new();

    for (index, run) in decision_table().into_iter().enumerate() {
        let case = run.case;
        let created = create_one(&router, &format!("decided_{index}"), run.code).await;
        let answer = test_connection_function(
            &router,
            &created.id,
            None,
            run.connection_object.as_bytes(),
            &created.etag,
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected HTTP 200 from the test, got {} with {}",
            answer.status, answer.raw
        );
        let result = answer.root();
        let output = result_text(result, "ConnectionFunctionOutput")
            .unwrap_or_else(|| panic!("case {case}: the result carries no output: {}", answer.raw))
            .to_string();
        let message = result_text(result, "ConnectionFunctionErrorMessage")
            .unwrap_or_else(|| {
                panic!("case {case}: the result carries no error message: {}", answer.raw)
            })
            .to_string();

        match run.decision {
            Some(decision) => {
                let document = json(&output, case);
                assert_eq!(
                    document.pointer(run.decision_at).and_then(Json::as_str),
                    Some(decision),
                    "case {case}: expected the decision {decision:?} at {}, got {output}",
                    run.decision_at
                );
                // The connection the decision was taken on is reported as the code left it:
                // every member the request submitted is still in it.
                assert_eq!(
                    document.pointer("/viewer/ip").and_then(Json::as_str),
                    Some("198.51.100.11"),
                    "case {case}: expected the connection the code decided about, got {output}"
                );
                assert_eq!(
                    message, "",
                    "case {case}: a run that decided and failed at nothing reports no error \
                     message, got {message:?}"
                );
            }
            None => {
                assert_eq!(
                    output, "{}",
                    "case {case}: a run that took no decision reports no decision document, \
                     got {output}"
                );
            }
        }

        match run.error {
            Some(fragment) => assert!(
                message.contains(fragment),
                "case {case}: expected an error message carrying {fragment:?}, got {message:?}"
            ),
            None => assert_eq!(
                message, "",
                "case {case}: expected no error message, got {message:?}"
            ),
        }

        let logs = result_logs(result);
        assert_eq!(
            logs, run.logs,
            "case {case}: expected the lines the run logged {:?}, got {logs:?}",
            run.logs
        );
        outputs.push((case, run.connection_object, run.decision, output));
    }

    // The whole point of the two decisions: a run that ALLOWED and a run that DENIED the SAME
    // connection are distinguishable, and neither reports the empty document.
    for (case, object, decision, output) in &outputs {
        assert!(
            !output.is_empty(),
            "case {case}: expected an output document, got {output:?}"
        );
        if decision.is_none() {
            continue;
        }
        for (other_case, other_object, other_decision, other_output) in &outputs {
            if other_decision.is_none() || other_decision == decision || other_object != object {
                continue;
            }
            assert_ne!(
                output, other_output,
                "case {case} against case {other_case}: a run that decided {decision:?} and one \
                 that decided {other_decision:?} about the same connection must report \
                 different outputs, both reported {output}"
            );
        }
    }
}

/// Rendering the connection the code left is the code's own work too — a `toJSON` it put on
/// the connection is called to render it — so it is held to the same bounds the rest of the run
/// is: code whose `toJSON` never returns is reported as a run a bound STOPPED, with no
/// connection to report and the engine left to serve the next caller.
#[tokio::test]
async fn a_renderer_the_code_installed_is_held_to_the_run_s_bounds() {
    let router = emulator();
    let created = create_one(
        &router,
        "diverging_renderer",
        "connection.toJSON = function () { while (true) { } };\n",
    )
    .await;

    let answer = test_connection_function(
        &router,
        &created.id,
        None,
        CONNECTION_OBJECT.as_bytes(),
        &created.etag,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "expected the test to be answered, got {} with {}",
        answer.status, answer.raw
    );
    let output = result_text(answer.root(), "ConnectionFunctionOutput").unwrap_or_default();
    assert_eq!(
        output, "{}",
        "a run a bound stopped reports no connection, got {output}"
    );
    let message = result_text(answer.root(), "ConnectionFunctionErrorMessage").unwrap_or_default();
    assert!(
        message.contains("stopped"),
        "expected the run to report the bound that stopped it, got {message:?}"
    );

    // And the engine is still there for the next caller: a second connection function answers
    // its own decision rather than a run that did not happen.
    let next = create_one(
        &router,
        "after_a_diverging_renderer",
        "connection.allow();\n",
    )
    .await;
    let after = test_connection_function(
        &router,
        &next.id,
        None,
        CONNECTION_OBJECT.as_bytes(),
        &next.etag,
    )
    .await;
    assert_eq!(
        after.status, 200,
        "expected the next test to be answered, got {} with {}",
        after.status, after.raw
    );
    let decided = result_text(after.root(), "ConnectionFunctionOutput").unwrap_or_default();
    assert_eq!(
        json(decided, "after a diverging renderer")
            .pointer(DECISION_ON_THE_OBJECT)
            .and_then(Json::as_str),
        Some("allow"),
        "expected the next run to report its own decision, got {decided}"
    );
}

/// The FIRST decision the code takes is the run's: a function that denies and then tries to
/// allow the same connection reports the deny, so no later statement can overturn a decision
/// already taken.
#[tokio::test]
async fn the_first_decision_the_code_takes_is_the_one_reported() {
    let router = emulator();

    // (case, the code, the decision the result must name).
    let rows: [(&str, &str, &str); 2] = [
        (
            "deny before allow",
            "function handler(connection) {\n\
             \x20   connection.deny();\n\
             \x20   connection.allow();\n\
             }\n",
            "deny",
        ),
        (
            "allow before deny",
            "function handler(connection) {\n\
             \x20   connection.allow();\n\
             \x20   connection.deny();\n\
             }\n",
            "allow",
        ),
    ];

    for (index, (case, code, expected)) in rows.into_iter().enumerate() {
        let created = create_one(&router, &format!("first_decision_{index}"), code).await;
        let answer = test_connection_function(
            &router,
            &created.id,
            None,
            CONNECTION_OBJECT.as_bytes(),
            &created.etag,
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected HTTP 200 from the test, got {} with {}",
            answer.status, answer.raw
        );
        let output = result_text(answer.root(), "ConnectionFunctionOutput")
            .unwrap_or_else(|| panic!("case {case}: the result carries no output: {}", answer.raw))
            .to_string();
        assert_eq!(
            json(&output, case).pointer("/decision").and_then(Json::as_str),
            Some(expected),
            "case {case}: expected the first decision {expected:?}, got {output}"
        );
    }
}
