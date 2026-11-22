// SPDX-License-Identifier: CC0-1.0

//! `TestConnectionFunction` end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The rules under test are:
//!
//! * "Tests a connection function": a connection function's code runs with the connection it
//!   decides about in scope as the global `connection` — and is handed that connection where
//!   it declares a `handler` — so the result is what the STORED code of the addressed stage DID
//!   with the submitted connection object: the DECISION it took (`connection.allow()` or
//!   `connection.deny()`, reported as the JSON of the connection as the code left it, carrying
//!   the decision that was taken, since
//!   `ConnectionFunctionTestResult$ConnectionFunctionOutput` is a plain string the model
//!   states no structure for), the lines it logged in the order it logged them, and — when it
//!   failed — the error message of that failure, beside the summary of
//!   the stage that ran and the share of the allowed execution time the run took. Code that
//!   takes no decision and fails at nothing reports the connection as it left it and no
//!   error;
//! * a result that is served at all reports BOTH halves of the run: the output member is
//!   present on every 200 and carries the empty JSON object for a run that produced no
//!   connection at all, and the error-message member is present on every 200 and carries the
//!   EMPTY STRING for a run that reported no error;
//! * the stage decides WHICH stored record runs: a request naming no stage tests the stage a
//!   connection function is created in, `LIVE` tests what a publish copied there, and a stage
//!   no stored connection function has — the `LIVE` stage of one nothing published among
//!   them — is an entity that was not found (`EntityNotFound`, HTTP 404), as is an id no
//!   connection function answers to;
//! * `IfMatch` is "The current version (ETag value) of the connection function", checked
//!   against the version of the STAGE being tested: a header that is no version token at all
//!   is missing or not valid (`InvalidIfMatchVersion`, HTTP 400) and a well-formed token that
//!   is not that stage's current one is a failed precondition (`PreconditionFailed`,
//!   HTTP 412);
//! * a connection object that is no JSON object describes no connection a function can be
//!   handed, so the test cannot be carried out (`TestFunctionFailed`, HTTP 500) and nothing
//!   runs;
//! * a test REPORTS, so it persists nothing: the connection function, its version tokens and
//!   its code stand exactly as they did before it.
//!
//! The table rows differ along everything the first rule generalises over — what the code
//! DOES (allows the connection it was handed, allows it after changing it, computes what it
//! adds from the connection, walks the connection it was handed, allows from inside a
//! promise, logs, raises, decides nothing, does not compile, never returns) and which
//! connection object it is handed — because the result is a property of the RUN rather than
//! of any one script.

use ::serde_json::Value as Json;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::MAX_OUTPUT_BYTES;
use smithy_server_runtime::MultiServiceRouter;

use crate::connection_function_lifecycle::{
    create_connection_function, describe_connection_function, member, Create,
};
// The publish call path, from the suite that owns the publish surface: one copy of it rather
// than a second here.
use crate::connection_function_publish::{
    awaited_publish, publish_connection_function, STATUS_UNASSOCIATED,
};
use crate::distribution_lifecycle::{call, emulator, Answer, Node, API_VERSION};
use crate::function_lifecycle::base64;

/// The two values the modelled `FunctionStage` enum admits (`smithy_query(query="shape",
/// shape="FunctionStage")`), spelled out here so a wrong constant in the crate cannot make
/// these assertions pass.
const STAGE_DEVELOPMENT: &str = "DEVELOPMENT";
const STAGE_LIVE: &str = "LIVE";

/// A version token that is well formed — `E` + 13 characters of the shape this service mints
/// — but that no connection function in a fresh emulator holds, so a precondition carrying
/// it is stale rather than malformed.
const UNHELD_VERSION_TOKEN: &str = "E1ABCDEFGHIJKL";

/// The document a result reports for a run that produced none: the empty JSON object. A
/// literal here rather than the crate's own constant, so a wrong constant in the crate cannot
/// make these assertions pass.
const EMPTY_OUTPUT_DOCUMENT: &str = "{}";

/// A connection object a client submits: the connection a connection function decides about,
/// carrying the viewer and the TLS details of it. Its content is the CALLER's, so what a row
/// proves is that the script is handed exactly this document.
///
/// `pub(crate)` for the reason [`test_connection_function`] is.
pub(crate) const CONNECTION_OBJECT: &str = r#"{"version":"1.0",
     "context":{"distributionDomainName":"d111111abcdef8.cloudfront.net",
     "distributionId":"EDFDVBD6EXAMPLE","connectionId":"c-1"},
     "viewer":{"ip":"198.51.100.11","tls":{"version":"TLSv1.3",
     "cipher":"TLS_AES_128_GCM_SHA256","sni":"example.com"}}}"#;

/// A second connection object, differing in every value the rows below read off one, so no
/// assertion can pass on a document this suite only submitted once.
const OTHER_CONNECTION_OBJECT: &str = r#"{"version":"1.0",
     "context":{"distributionDomainName":"d222222abcdef8.cloudfront.net",
     "distributionId":"E2CDEFGHEXAMPLE","connectionId":"c-2"},
     "viewer":{"ip":"203.0.113.7","tls":{"version":"TLSv1.2",
     "cipher":"ECDHE-RSA-AES128-GCM-SHA256","sni":"shop.example.com"}}}"#;

// ---------------------------------------------------------------------------
// Calling the test
// ---------------------------------------------------------------------------

/// `POST /2020-05-31/connection-function/{Id}/test`, carrying the connection object as the
/// base64 blob member a restXml request puts it in, the stage when the request names one,
/// and `if_match` as the `If-Match` header the operation binds its required `IfMatch` member
/// to.
///
/// `pub(crate)` so a sibling suite that tests a connection function goes over this one wire
/// path rather than growing a copy of it.
pub(crate) async fn test_connection_function(
    router: &MultiServiceRouter,
    id: &str,
    stage: Option<&str>,
    connection_object: &[u8],
    if_match: &str,
) -> Answer {
    let stage_element = match stage {
        Some(stage) => format!("<Stage>{stage}</Stage>"),
        None => String::new(),
    };
    let body = format!(
        "<TestConnectionFunctionRequest \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         {stage_element}<ConnectionObject>{}</ConnectionObject>\
         </TestConnectionFunctionRequest>",
        base64(connection_object)
    );
    call(
        router,
        "POST",
        &format!("/{API_VERSION}/connection-function/{id}/test"),
        Some(&body),
        Some(if_match),
    )
    .await
}

/// What one create handed back: the id it minted and the version token of the `DEVELOPMENT`
/// stage it stored, which is the version a test of that stage is checked against.
pub(crate) struct Created {
    pub(crate) id: String,
    pub(crate) etag: String,
}

/// Create the connection function `name` with `code` and answer what the create minted.
pub(crate) async fn create_one(router: &MultiServiceRouter, name: &str, code: &str) -> Created {
    let create = Create {
        case: "a connection function under test",
        name: name.to_string(),
        comment: "runs under test".to_string(),
        runtime: "cloudfront-js-2.0",
        code: code.as_bytes().to_vec(),
        associations: Vec::new(),
        tags: Vec::new(),
    };
    let answer = create_connection_function(router, &create.xml()).await;
    assert_eq!(
        answer.status, 201,
        "the create of {name} must answer HTTP 201, got {} with {}",
        answer.status, answer.raw
    );
    Created {
        id: member(answer.root(), "Id", create.case, &answer.raw),
        etag: answer.header("etag").to_string(),
    }
}

/// The text of one member of the `ConnectionFunctionTestResult` document, or `None` when it
/// carries none.
pub(crate) fn result_text<'a>(result: &'a Node, name: &str) -> Option<&'a str> {
    result.text_at(name)
}

/// The log lines the result reports, in the order they appear in it.
pub(crate) fn result_logs(result: &Node) -> Vec<String> {
    result
        .at("ConnectionFunctionExecutionLogs")
        .map(|logs| {
            logs.children
                .iter()
                .map(|line| line.text().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// `text` as JSON, or a panic naming the case: the output member carries a JSON document, so
/// it is compared as a document rather than as text.
fn json(text: &str, case: &str) -> Json {
    ::serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("case {case}: expected JSON, got {text:?} ({error})"))
}

// ---------------------------------------------------------------------------
// The runs under test
// ---------------------------------------------------------------------------

/// One run: the code a connection function was stored with, the connection object it is
/// handed, and what the result must report.
struct Run {
    case: &'static str,
    code: &'static str,
    /// The connection object the request carries, as the JSON text a client submits.
    connection_object: &'static str,
    /// The JSON of the CONNECTION the `ConnectionFunctionOutput` member must report: the
    /// document carries the decision the run took where it took one ([`reported`]).
    output: Option<&'static str>,
    /// Whether the run reported its output as a DECISION it took, rather than as the
    /// connection it was handed and left as it stands.
    decided: bool,
    /// The log lines the result must report, in order.
    logs: Vec<&'static str>,
    /// A fragment the reported error message must contain, or `Some("")` for a run that must
    /// report SOME error message whose wording is the engine's own. `None` where the run must
    /// report no error at all.
    error: Option<&'static str>,
}

/// The output document a result reports for a run over the connection `connection`: the
/// connection as the code left it, carrying the decision that was taken where `decided` says
/// the run allowed it.
fn reported(connection: &str, decided: bool) -> String {
    let mut document: Json = ::serde_json::from_str(connection)
        .expect("every row of this table submits a JSON connection object");
    if decided {
        document["decision"] = Json::String("allow".to_string());
    }
    document.to_string()
}

/// Every rule below is asserted over these runs.
fn run_table() -> Vec<Run> {
    vec![
        Run {
            case: "a script that only allows the connection it was handed",
            code: "connection.allow();\n",
            connection_object: CONNECTION_OBJECT,
            output: Some(CONNECTION_OBJECT),
            decided: true,
            logs: Vec::new(),
            error: None,
        },
        Run {
            case: "a script that allows the other connection object",
            code: "connection.allow();\n",
            connection_object: OTHER_CONNECTION_OBJECT,
            output: Some(OTHER_CONNECTION_OBJECT),
            decided: true,
            logs: Vec::new(),
            error: None,
        },
        Run {
            case: "a script that changes the connection before allowing it",
            code: "connection.decided = { by: 'tls', value: connection.viewer.tls.version };\n\
                   connection.allow();\n",
            connection_object: CONNECTION_OBJECT,
            output: Some(
                r#"{"version":"1.0",
                 "context":{"distributionDomainName":"d111111abcdef8.cloudfront.net",
                 "distributionId":"EDFDVBD6EXAMPLE","connectionId":"c-1"},
                 "viewer":{"ip":"198.51.100.11","tls":{"version":"TLSv1.3",
                 "cipher":"TLS_AES_128_GCM_SHA256","sni":"example.com"}},
                 "decided":{"by":"tls","value":"TLSv1.3"}}"#,
            ),
            decided: true,
            logs: Vec::new(),
            error: None,
        },
        Run {
            case: "a script whose change is computed from the connection it was handed",
            code: "var names = Object.keys(connection.viewer.tls).sort();\n\
                   connection.seen = names.length + ':' + names.join('-');\n\
                   connection.allow();\n",
            connection_object: OTHER_CONNECTION_OBJECT,
            output: Some(
                r#"{"version":"1.0",
                 "context":{"distributionDomainName":"d222222abcdef8.cloudfront.net",
                 "distributionId":"E2CDEFGHEXAMPLE","connectionId":"c-2"},
                 "viewer":{"ip":"203.0.113.7","tls":{"version":"TLSv1.2",
                 "cipher":"ECDHE-RSA-AES128-GCM-SHA256","sni":"shop.example.com"}},
                 "seen":"3:cipher-sni-version"}"#,
            ),
            decided: true,
            logs: Vec::new(),
            error: None,
        },
        Run {
            case: "a script that computes over a loop, which the step budget leaves room for",
            code: "var total = 0;\n\
                   for (var index = 0; index < 10000; index++) { total += index; }\n\
                   connection.total = total;\n\
                   connection.allow();\n",
            connection_object: CONNECTION_OBJECT,
            output: Some(
                r#"{"version":"1.0",
                 "context":{"distributionDomainName":"d111111abcdef8.cloudfront.net",
                 "distributionId":"EDFDVBD6EXAMPLE","connectionId":"c-1"},
                 "viewer":{"ip":"198.51.100.11","tls":{"version":"TLSv1.3",
                 "cipher":"TLS_AES_128_GCM_SHA256","sni":"example.com"}},
                 "total":49995000}"#,
            ),
            decided: true,
            logs: Vec::new(),
            error: None,
        },
        Run {
            case: "a script that walks the connection it was handed, which carries only the \
                   submitted members",
            code: "connection.walked = Object.keys(connection).join(',');\n\
                   connection.allow();\n",
            connection_object: CONNECTION_OBJECT,
            output: Some(
                r#"{"version":"1.0",
                 "context":{"distributionDomainName":"d111111abcdef8.cloudfront.net",
                 "distributionId":"EDFDVBD6EXAMPLE","connectionId":"c-1"},
                 "viewer":{"ip":"198.51.100.11","tls":{"version":"TLSv1.3",
                 "cipher":"TLS_AES_128_GCM_SHA256","sni":"example.com"}},
                 "walked":"version,context,viewer"}"#,
            ),
            decided: true,
            logs: Vec::new(),
            error: None,
        },
        Run {
            case: "a script that allows the connection from inside a promise",
            code: "Promise.resolve('TLSv1.3').then(function (version) {\n\
                   \x20   connection.awaited = version === connection.viewer.tls.version;\n\
                   \x20   connection.allow();\n\
                   });\n",
            connection_object: CONNECTION_OBJECT,
            output: Some(
                r#"{"version":"1.0",
                 "context":{"distributionDomainName":"d111111abcdef8.cloudfront.net",
                 "distributionId":"EDFDVBD6EXAMPLE","connectionId":"c-1"},
                 "viewer":{"ip":"198.51.100.11","tls":{"version":"TLSv1.3",
                 "cipher":"TLS_AES_128_GCM_SHA256","sni":"example.com"}},
                 "awaited":true}"#,
            ),
            decided: true,
            logs: Vec::new(),
            error: None,
        },
        Run {
            case: "lines written through the console, in the order they were written",
            code: "console.log('ip', connection.viewer.ip);\n\
                   console.log(connection.viewer.tls);\n\
                   console.error('deciding');\n\
                   connection.allow();\n",
            connection_object: CONNECTION_OBJECT,
            output: Some(CONNECTION_OBJECT),
            decided: true,
            logs: vec![
                "ip 198.51.100.11",
                "{\"version\":\"TLSv1.3\",\"cipher\":\"TLS_AES_128_GCM_SHA256\",\
                 \"sni\":\"example.com\"}",
                "deciding",
            ],
            error: None,
        },
        Run {
            case: "a script that raises, after logging",
            code: "console.log('about to fail');\n\
                   throw new Error('no route for ' + connection.viewer.tls.sni);\n",
            connection_object: CONNECTION_OBJECT,
            output: None,
            decided: false,
            logs: vec!["about to fail"],
            error: Some("Error: no route for example.com"),
        },
        Run {
            case: "a script that decides nothing about the connection, which reports the \
                   connection as it left it",
            code: "var unused = connection.viewer.ip;\n",
            connection_object: CONNECTION_OBJECT,
            output: Some(CONNECTION_OBJECT),
            decided: false,
            logs: Vec::new(),
            error: None,
        },
        Run {
            case: "code that does not compile",
            code: "connection.allow(;\n",
            connection_object: CONNECTION_OBJECT,
            output: None,
            decided: false,
            logs: Vec::new(),
            error: Some("SyntaxError"),
        },
        Run {
            case: "a script that never returns, which a bound stops",
            code: "while (true) { }\n",
            connection_object: CONNECTION_OBJECT,
            output: None,
            decided: false,
            logs: Vec::new(),
            error: Some("stopped"),
        },
    ]
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/// The result is what the stored code DID with the submitted connection object: the decision
/// it took, as the JSON document naming it; the lines it logged, in order; and the error
/// message of a run that failed, with no output beside it. Every result reports a compute
/// utilization that is a whole-number percentage of the allowed execution time, and the
/// summary of the stage that ran.
#[tokio::test]
async fn a_test_reports_what_the_stored_code_did_with_the_connection_object() {
    let router = emulator();

    for (index, run) in run_table().into_iter().enumerate() {
        let case = run.case;
        let name = format!("connection_run_{index}");
        let created = create_one(&router, &name, run.code).await;

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
        assert_eq!(
            result.name(),
            "ConnectionFunctionTestResult",
            "case {case}: expected the ConnectionFunctionTestResult payload, got {}",
            answer.raw
        );

        // "The connection function summary": the stage that ran, projected from the stored
        // record.
        assert_eq!(
            result.text_at("ConnectionFunctionSummary/Name"),
            Some(name.as_str()),
            "case {case}: expected the tested connection function's name in the summary, got {}",
            answer.raw
        );
        assert_eq!(
            result.text_at("ConnectionFunctionSummary/Id"),
            Some(created.id.as_str()),
            "case {case}: expected the Id the create minted in the summary, got {}",
            answer.raw
        );
        assert_eq!(
            result.text_at("ConnectionFunctionSummary/Stage"),
            Some(STAGE_DEVELOPMENT),
            "case {case}: expected the summary of the stage that ran, got {}",
            answer.raw
        );

        // "The connection function compute utilization" — a percentage of the allowed
        // execution time, so a whole number no greater than 100.
        let utilization = result_text(result, "ComputeUtilization").unwrap_or_else(|| {
            panic!(
                "case {case}: the result carries no ComputeUtilization: {}",
                answer.raw
            )
        });
        let utilization: u32 = utilization.parse().unwrap_or_else(|error| {
            panic!("case {case}: expected a whole-number percentage, got {utilization:?} ({error})")
        });
        assert!(
            utilization <= 100,
            "case {case}: expected a percentage of the allowed time, got {utilization}"
        );

        // Both halves of the run are reported on every result that is served at all: the
        // decision it took — the empty JSON object for a run that took none — and the
        // error message, which is the empty string for a run that reported none.
        let output = result_text(result, "ConnectionFunctionOutput").unwrap_or_else(|| {
            panic!(
                "case {case}: the result carries no ConnectionFunctionOutput: {}",
                answer.raw
            )
        });
        assert_eq!(
            json(output, case),
            json(
                &run.output.map_or_else(
                    || EMPTY_OUTPUT_DOCUMENT.to_string(),
                    |connection| reported(connection, run.decided)
                ),
                case
            ),
            "case {case}: expected the connection the run reported, got {output}"
        );

        let message = result_text(result, "ConnectionFunctionErrorMessage").unwrap_or_else(|| {
            panic!(
                "case {case}: the result carries no ConnectionFunctionErrorMessage: {}",
                answer.raw
            )
        });
        match run.error {
            Some(fragment) => assert!(
                !message.is_empty() && message.contains(fragment),
                "case {case}: expected an error message carrying {fragment:?}, got {message:?}"
            ),
            None => assert_eq!(
                message, "",
                "case {case}: a run that reported no error reports the empty string, got \
                 {message:?}"
            ),
        }

        let logs = result_logs(result);
        assert_eq!(
            logs, run.logs,
            "case {case}: expected the lines the function logged, got {logs:?}"
        );
    }
}

/// The stage decides which stored record runs, and each stage is tested at its OWN version:
/// the `LIVE` stage of a connection function nothing published is not found, after a publish
/// it runs the copied code under the version token the copy carries, and the token of the
/// other stage is a failed precondition against it.
#[tokio::test]
async fn the_stage_the_request_names_decides_which_record_runs() {
    let router = emulator();
    let code = "connection.ran = '1';\n\
                connection.allow();\n";
    let created = create_one(&router, "staged_connection_run", code).await;

    // The LIVE stage of a connection function nothing has published does not exist, so there
    // is no code to run there and no version to check against.
    let unpublished = test_connection_function(
        &router,
        &created.id,
        Some(STAGE_LIVE),
        CONNECTION_OBJECT.as_bytes(),
        &created.etag,
    )
    .await;
    assert_eq!(
        unpublished.status, 404,
        "the LIVE stage of an unpublished connection function must be not found, got {} with {}",
        unpublished.status, unpublished.raw
    );
    assert_eq!(
        unpublished.error_code(),
        "EntityNotFound",
        "expected EntityNotFound for an unpublished stage, got {}",
        unpublished.raw
    );

    let publish = publish_connection_function(&router, &created.id, &created.etag).await;
    assert_eq!(
        publish.status, 200,
        "the publish must answer HTTP 200, got {} with {}",
        publish.status, publish.raw
    );
    // The version of the stage the publish wrote, read back from the describe of it rather
    // than assumed — and read once the status the clock owes that publish has settled, so the
    // token belongs to the stage as it now stands.
    let live_etag = awaited_publish(
        &router,
        "the published stage",
        &created.id,
        STATUS_UNASSOCIATED,
    )
    .await
    .header("etag")
    .to_string();

    for (case, stage, if_match, expected_stage) in [
        (
            "no stage named",
            None,
            created.etag.clone(),
            STAGE_DEVELOPMENT,
        ),
        (
            "DEVELOPMENT named",
            Some(STAGE_DEVELOPMENT),
            created.etag.clone(),
            STAGE_DEVELOPMENT,
        ),
        (
            "LIVE named",
            Some(STAGE_LIVE),
            live_etag.clone(),
            STAGE_LIVE,
        ),
    ] {
        let answer = test_connection_function(
            &router,
            &created.id,
            stage,
            CONNECTION_OBJECT.as_bytes(),
            &if_match,
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected HTTP 200 from the test, got {} with {}",
            answer.status, answer.raw
        );
        let reported_stage = answer
            .root()
            .text_at("ConnectionFunctionSummary/Stage")
            .unwrap_or_default();
        assert_eq!(
            reported_stage, expected_stage,
            "case {case}: expected the summary of the stage that ran, got {}",
            answer.raw
        );
        let output = result_text(answer.root(), "ConnectionFunctionOutput").unwrap_or_else(|| {
            panic!(
                "case {case}: the result carries no ConnectionFunctionOutput: {}",
                answer.raw
            )
        });
        let allowed = json(output, case);
        assert_eq!(
            allowed.pointer("/ran").and_then(Json::as_str),
            Some("1"),
            "case {case}: expected the code of that stage to run, got {output}"
        );
    }

    // Each stage is checked against its own version token, so the other stage's token is a
    // well-formed token that is not the current one: a failed precondition.
    for (case, stage, if_match) in [
        (
            "the LIVE token against DEVELOPMENT",
            Some(STAGE_DEVELOPMENT),
            live_etag.clone(),
        ),
        (
            "the DEVELOPMENT token against LIVE",
            Some(STAGE_LIVE),
            created.etag.clone(),
        ),
    ] {
        let answer = test_connection_function(
            &router,
            &created.id,
            stage,
            CONNECTION_OBJECT.as_bytes(),
            &if_match,
        )
        .await;
        assert_eq!(
            answer.status, 412,
            "case {case}: expected HTTP 412, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "PreconditionFailed",
            "case {case}: expected PreconditionFailed, got {}",
            answer.raw
        );
    }
}

/// The operation addresses the resource by "The connection function ID" the create minted, so
/// every other identifier — the name it was created under among them — names no connection
/// function to test: HTTP 404 `EntityNotFound`.
#[tokio::test]
async fn an_id_no_connection_function_answers_to_is_not_found() {
    let router = emulator();
    let name = "present_connection_function";
    let created = create_one(
        &router,
        name,
        "connection.allow();\n",
    )
    .await;

    let unknown = [
        ("the name the create was given", name.to_string()),
        ("an id of the minted shape nothing holds", {
            let mut other = created.id.clone();
            // Flip the last character of a real id, so the value has the shape this service
            // mints and still names nothing.
            other.pop();
            format!("{other}0")
        }),
        ("a value of no particular shape", "missing".to_string()),
    ];

    for (case, id) in unknown {
        let answer = test_connection_function(
            &router,
            &id,
            None,
            CONNECTION_OBJECT.as_bytes(),
            &created.etag,
        )
        .await;
        assert_eq!(
            answer.status, 404,
            "case {case}: expected HTTP 404 testing {id:?}, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "EntityNotFound",
            "case {case}: expected EntityNotFound, got {}",
            answer.raw
        );
    }
}

/// The test only runs the version its caller read. A header carrying the current version
/// token of the stage addressed — bare, quoted as an HTTP entity tag, or quoted as a weak one
/// — runs; anything that is no version token at all is missing or not valid; a well-formed
/// token that belongs to another version is a failed precondition.
#[tokio::test]
async fn a_test_needs_the_current_version_of_the_stage_it_runs() {
    let router = emulator();
    let code = "connection.allow();\n";
    let created = create_one(&router, "precondition_connection_run", code).await;
    // A version token this service really minted that belongs to another connection function,
    // so one stale case is a token the store issued rather than a made-up string.
    let foreign = create_one(&router, "foreign_version_source", code).await;

    for (case, if_match, refusal) in [
        ("the current version, bare", created.etag.clone(), None),
        (
            "the current version, quoted as an entity tag",
            format!("\"{}\"", created.etag),
            None,
        ),
        (
            "the current version, quoted as a weak entity tag",
            format!("W/\"{}\"", created.etag),
            None,
        ),
        (
            "a header that is no version token at all",
            "not-a-version".to_string(),
            Some((400, "InvalidIfMatchVersion")),
        ),
        (
            "an empty header",
            String::new(),
            Some((400, "InvalidIfMatchVersion")),
        ),
        (
            "a token of the right shape in the wrong case",
            created.etag.to_lowercase(),
            Some((400, "InvalidIfMatchVersion")),
        ),
        (
            "a token whose body is too short",
            "E123".to_string(),
            Some((400, "InvalidIfMatchVersion")),
        ),
        (
            "a well-formed token no connection function holds",
            UNHELD_VERSION_TOKEN.to_string(),
            Some((412, "PreconditionFailed")),
        ),
        (
            "the version token of another connection function",
            foreign.etag.clone(),
            Some((412, "PreconditionFailed")),
        ),
    ] {
        let answer = test_connection_function(
            &router,
            &created.id,
            None,
            CONNECTION_OBJECT.as_bytes(),
            &if_match,
        )
        .await;
        match refusal {
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case {case}: expected the test to run, got {} with {}",
                    answer.status, answer.raw
                );
                assert!(
                    result_text(answer.root(), "ConnectionFunctionOutput").is_some(),
                    "case {case}: a test that ran reports the connection it allowed, got {}",
                    answer.raw
                );
            }
            Some((status, code)) => {
                assert_eq!(
                    answer.status, status,
                    "case {case}: expected HTTP {status}, got {} with {}",
                    answer.status, answer.raw
                );
                assert_eq!(
                    answer.error_code(),
                    code,
                    "case {case}: expected {code}, got {}",
                    answer.raw
                );
            }
        }
    }
}

/// "The connection object" is what the function is handed, so a payload that is no JSON
/// object describes no connection a function can be passed: the test cannot be carried out,
/// which is the modelled server-side test failure (`TestFunctionFailed`, HTTP 500), and
/// nothing runs.
#[tokio::test]
async fn a_connection_object_that_is_no_json_object_fails_the_test() {
    let router = emulator();
    let created = create_one(
        &router,
        "connection_object_shape",
        "connection.allow();\n",
    )
    .await;

    for (case, object) in [
        ("no bytes at all", &b""[..]),
        ("text that is not JSON", &b"not a connection"[..]),
        ("a truncated document", &b"{\"connection\":"[..]),
        ("a JSON array", &b"[{\"connection\":{}}]"[..]),
        ("a JSON string", &b"\"connection\""[..]),
        ("a JSON number", &b"42"[..]),
        ("a JSON null", &b"null"[..]),
        ("bytes that are not UTF-8", &[0x00, 0xff, 0xfe][..]),
    ] {
        let answer =
            test_connection_function(&router, &created.id, None, object, &created.etag).await;
        assert_eq!(
            answer.status, 500,
            "case {case}: expected HTTP 500, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "TestFunctionFailed",
            "case {case}: expected TestFunctionFailed, got {}",
            answer.raw
        );
    }
}

/// A test REPORTS: it runs the code and persists nothing. The connection function's
/// configuration, its status and its version token stand exactly as they did before, so a
/// second test at the same version runs and answers the same way.
#[tokio::test]
async fn a_test_persists_nothing() {
    let router = emulator();
    let code = "connection.stable = '1';\n\
                connection.allow();\n";
    let created = create_one(&router, "reported_only_connection", code).await;

    let before = describe_connection_function(&router, &created.id, None).await;

    let first = test_connection_function(
        &router,
        &created.id,
        None,
        CONNECTION_OBJECT.as_bytes(),
        &created.etag,
    )
    .await;
    assert_eq!(
        first.status, 200,
        "the first test must answer HTTP 200, got {} with {}",
        first.status, first.raw
    );
    let first_output = result_text(first.root(), "ConnectionFunctionOutput")
        .expect("the first test reports the connection the script allowed")
        .to_string();

    let after = describe_connection_function(&router, &created.id, None).await;
    assert_eq!(
        after.header("etag"),
        before.header("etag"),
        "a test must not mint a version: {}",
        after.raw
    );
    assert_eq!(
        after.raw, before.raw,
        "a test must leave the stored connection function exactly as it was"
    );
    // Nothing was published either: the LIVE stage is still not there.
    let live = describe_connection_function(&router, &created.id, Some(STAGE_LIVE)).await;
    assert_eq!(
        live.status, 404,
        "a test must publish nothing, got {} with {}",
        live.status, live.raw
    );

    // The version the first test used is therefore still the current one, and the second run
    // of the same code over the same connection object answers the same way.
    let second = test_connection_function(
        &router,
        &created.id,
        None,
        CONNECTION_OBJECT.as_bytes(),
        &created.etag,
    )
    .await;
    assert_eq!(
        second.status, 200,
        "the second test must answer HTTP 200, got {} with {}",
        second.status, second.raw
    );
    let second_output = result_text(second.root(), "ConnectionFunctionOutput")
        .expect("the second test reports the connection the script allowed")
        .to_string();
    assert_eq!(
        json(&second_output, "a repeated test"),
        json(&first_output, "a repeated test"),
        "the same code over the same connection object must answer the same value"
    );
}

/// One run cannot make an unbounded answer out of a small request: a connection past the
/// published output ceiling is reported as a run that FAILED, with the ceiling named and the
/// empty output document beside it.
#[tokio::test]
async fn an_allowed_connection_past_the_output_ceiling_is_a_failed_run() {
    let router = emulator();
    let verbose = format!(
        "connection.big = 'x'.repeat({});\n\
         connection.allow();\n",
        MAX_OUTPUT_BYTES + 1_000
    );
    let created = create_one(&router, "connection_output_ceiling", &verbose).await;

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
        "a run that allowed too large a connection is still a run, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        result_text(answer.root(), "ConnectionFunctionOutput").unwrap_or_default(),
        EMPTY_OUTPUT_DOCUMENT,
        "a connection past the ceiling must not be served: {}",
        answer.raw
    );
    let message =
        result_text(answer.root(), "ConnectionFunctionErrorMessage").unwrap_or_default();
    assert!(
        message.contains(&MAX_OUTPUT_BYTES.to_string()),
        "expected the reported error to name the {MAX_OUTPUT_BYTES} byte ceiling, got \
         {message:?}"
    );
}
