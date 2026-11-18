// SPDX-License-Identifier: CC0-1.0

//! The id an Athena answer names its request by, as a Step Functions `Task` state RELAYS
//! it — driven across the three services the question spans (Athena answers the call, IAM
//! holds the role the call is made as, Step Functions reports what came back), mounted on
//! ONE engine exactly as `core/router/src/lib.rs` mounts them.
//!
//! RULE: every AWS response names the request it answered, and an AWS SDK renders a
//! service exception as the service's own sentence followed by the metadata that answer
//! carried, whose key names, order and separators are fixed text of the client that made
//! the call — the current AWS SDK through an `arn:aws:states:::aws-sdk:<service>:<action>`
//! `Resource` (`<message> (Service: <sdk name>, Status Code: <status>, Request ID: <id>)
//! (SDK Attempt Count: <n>)`) and the earlier generation of client through an optimized
//! one (`<message> (Service: <client>; Status Code: <status>; Error Code: <code>; Request
//! ID: <id>; Proxy: null)`). Step Functions puts that rendering in a failed task's
//! `cause`, so the `Request ID: <id>` segment is there for every refusal a task relays,
//! and a succeeded task reports the same id in the `SdkResponseMetadata.RequestId` of the
//! SDK response it answers with. Each call is named by its OWN id, so no two of them
//! repeat one.
//!
//! Two crates have to agree for that to hold, so the suite lives here rather than in
//! either of them. The OTHER place the same id is observable — the `x-amzn-RequestId` of
//! Athena's own answer, which is what this relay reads it out of — is pinned by
//! `services/service-com-amazonaws-athena-amazonathena/tests/all/response_request_id.rs`,
//! so this suite asks only what that one cannot: what the RELAY reports.
//!
//! The rows differ along what the rule generalises over: which Athena operation the task
//! called (`GetQueryExecution`, `StopQueryExecution`, `GetQueryResults`), which integration
//! family the `Resource` is written in (the optimized `arn:aws:states:::athena:<action>`
//! and the AWS SDK `arn:aws:states:::aws-sdk:athena:<action>` form), whether the id the
//! call carries is written into the `Parameters` or read out of the execution's own input,
//! and whether the call was REFUSED or SUCCEEDED — a success reports its id in the result's
//! own metadata rather than in a cause.
//!
//! Everything but the id itself is fixed text written out here, and the id is read out of
//! the answer and checked against the FORM alone: it identifies one call, so no particular
//! value is part of the sentence.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The harness the Step Functions execution-role suites build: the role with its trust
// document, the state machine, the execution reader and the awsJson caller for Step
// Functions itself. `tests/all/main.rs` is one crate root, so this suite borrows them
// rather than carrying a second copy.
use crate::sfn_task_target_authorization::{
    allow, logs_only, machine, masked, ran, ran_with_input, refusal_metadata, role, string,
    task_failure, REGION,
};

/// How long a poll waits for the condition it is waiting on — a query leaving `RUNNING`.
/// Generous: the loop stops as soon as the condition holds, so this bounds only a failure.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long a poll waits between looks.
const GLANCE: Duration = Duration::from_millis(25);

/// The marker that separates the invoked service's own sentence from the metadata its SDK
/// client appends to it.
const METADATA_MARKER: &str = " (Service: ";

/// The name the client each integration family is served by gives Athena, which is what
/// its rendering of an exception reports the service under: the earlier client's own name,
/// and the name the current AWS SDK's `Athena` client carries.
const SDK_CLIENT: &str = "AmazonAthena";
/// See [`SDK_CLIENT`].
const CURRENT_SDK_NAME: &str = "Athena";

/// The refusal class a read of an execution Athena never minted comes back under, and the
/// status that class is answered with:
/// `smithy_query(query="shape",shape="InvalidRequestException",model_path="services/
/// service-com-amazonaws-athena-amazonathena/smithy-model/
/// com-amazonaws-athena-amazonathena.json")` reports `httpError` 400.
const REFUSAL_CODE: &str = "InvalidRequestException";
/// See [`REFUSAL_CODE`].
const REFUSAL_STATUS: u16 = 400;

/// The error name a failed task reports for that class: Athena as Step Functions names it
/// in a task error, and the SDK exception the code is deserialized into.
const REFUSAL_ERROR: &str = "Athena.InvalidRequestException";

/// A query execution id nothing here ever minted, which is what the refusal rows read.
const UNMINTED_EXECUTION: &str = "3f6b0f52-2d4d-4a0e-9f5c-1b7a1d2e3f40";

/// Where the workgroup the positive control runs in writes its results. Nothing here reads
/// the objects: the location is what lets a query run at all.
const RESULTS_LOCATION: &str = "s3://sfn-relayed-athena-results/results/";

/// The statement the positive control's query is made of, which needs no table.
const QUERY: &str = "SELECT 'marker' AS marker";

/// The three services this question spans, over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(engine),
    );
    router
}

/// Call one Athena operation, requiring a 200.
///
/// `awsJson1_1` under the target prefix `AmazonAthena`, from that service's model's
/// `service_info` (`sigv4Name` `athena`).
async fn athena(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("athena.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonAthena.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/athena/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {operation}: expected 200 from Athena, got {status} with {parsed:?}"
    );
    parsed
}

/// Whether `id` is spelled the way Athena spells a request id: the hyphenated 8-4-4-4-12
/// groups of LOWERCASE hex digits of a version-4 UUID, the form almost every AWS service
/// reports.
///
/// Written out here rather than read off either crate, so what is pinned is the form a
/// client parses out of the relayed sentence.
fn names_an_athena_request(id: &str) -> bool {
    let mut groups = id.split('-');
    for width in [8usize, 4, 4, 4, 12] {
        match groups.next() {
            Some(group) => {
                let hex = group.len() == width
                    && group
                        .bytes()
                        .all(|digit| digit.is_ascii_hexdigit() && !digit.is_ascii_uppercase());
                if !hex {
                    return false;
                }
            }
            None => return false,
        }
    }
    groups.next().is_none()
}

/// The id the relayed sentence names the refused request by, with the WHOLE sentence
/// pinned around it.
///
/// Everything but the id is fixed text this suite writes out — the service's own sentence,
/// the client the call was made with, the status, and whatever else that client's rendering
/// carries — so the id is the one part read out of the answer, and it is judged by its
/// FORM alone.
fn relayed_request_id(case: &str, cause: &str, sentence: &str, resource: &str) -> String {
    let (stated, metadata) = cause.split_once(METADATA_MARKER).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the invoked service's sentence followed by \
             {METADATA_MARKER:?}, got {cause:?}"
        )
    });
    assert_eq!(
        stated, sentence,
        "case {case:?}: expected the sentence {sentence:?}, got {stated:?}"
    );
    const MARKER: &str = "Request ID: ";
    let at = metadata.find(MARKER).unwrap_or_else(|| {
        panic!("case {case:?}: expected the relayed metadata to name the request, got {cause:?}")
    });
    // The id runs up to whichever delimiter closes its field.
    let id: String = metadata[at + MARKER.len()..]
        .chars()
        .take_while(|character| !";)".contains(*character))
        .collect();
    let service = match resource.contains(":aws-sdk:") {
        true => CURRENT_SDK_NAME,
        false => SDK_CLIENT,
    };
    let expected = format!(
        "{stated}{}",
        refusal_metadata(resource, service, REFUSAL_CODE, REFUSAL_STATUS)
    );
    let reported = masked(cause);
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the metadata Athena's own client appends, got {cause:?}"
    );
    assert!(
        names_an_athena_request(&id),
        "case {case:?}: expected the relayed request id in Athena's own form — the hyphenated \
         8-4-4-4-12 hex groups of a UUID — got {id:?}"
    );
    id
}

/// One row: the case, the `Resource` the `Task` names, the parameters it calls with, the
/// execution input those parameters may read the id out of, and the sentence Athena's
/// refusal states.
struct Row {
    case: &'static str,
    resource: &'static str,
    parameters: Value,
    input: Option<String>,
    sentence: String,
}

/// An Athena failure a `Task` state relays names the refused request, and an Athena success
/// relays one too — each call by its own id.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relayed_athena_answer_names_the_request_it_answered() {
    let router = emulator();
    // Every integration call below is one Athena read or stop; none of them is gated on a
    // target resource, so the role carries a grant over the service's own actions beside
    // the log writes every state machine makes.
    let role_arn = role(
        &router,
        "RelayedAthenaRequestIdRole",
        &json!({
            "Version": "2012-10-17",
            "Statement": [logs_only(), allow("athena:*", "*")],
        }),
    )
    .await;

    let not_found = format!("QueryExecution {UNMINTED_EXECUTION} was not found");
    let rows = [
        Row {
            case: "the optimized getQueryExecution on an id Athena never minted",
            resource: "arn:aws:states:::athena:getQueryExecution",
            parameters: json!({ "QueryExecutionId": UNMINTED_EXECUTION }),
            input: None,
            sentence: not_found.clone(),
        },
        Row {
            // The same call, with the id taken out of the execution's own input rather
            // than written into the state: the relayed sentence is the same either way.
            case: "the optimized getQueryExecution reading the id out of the input",
            resource: "arn:aws:states:::athena:getQueryExecution",
            parameters: json!({ "QueryExecutionId.$": "$.qid" }),
            input: Some(json!({ "qid": UNMINTED_EXECUTION }).to_string()),
            sentence: not_found.clone(),
        },
        Row {
            // The other family makes the same call, so the relayed id has to be there
            // through it as well.
            case: "the same read through the AWS SDK integration",
            resource: "arn:aws:states:::aws-sdk:athena:getQueryExecution",
            parameters: json!({ "QueryExecutionId": UNMINTED_EXECUTION }),
            input: None,
            sentence: not_found.clone(),
        },
        Row {
            // A second operation, and a mutating one, so the segment is a property of the
            // ANSWER rather than of one handler.
            case: "the optimized stopQueryExecution on an id Athena never minted",
            resource: "arn:aws:states:::athena:stopQueryExecution",
            parameters: json!({ "QueryExecutionId": UNMINTED_EXECUTION }),
            input: None,
            sentence: not_found.clone(),
        },
        Row {
            case: "the optimized getQueryResults on an id Athena never minted",
            resource: "arn:aws:states:::athena:getQueryResults",
            parameters: json!({ "QueryExecutionId": UNMINTED_EXECUTION }),
            input: None,
            sentence: not_found.clone(),
        },
    ];

    // Every id any of these calls was named by: each call is its own request, so no two of
    // them may repeat one.
    let mut named: BTreeSet<String> = BTreeSet::new();

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let machine_arn = machine(
            &router,
            &format!("relayed-athena-{index}"),
            row.resource,
            &row.parameters,
            &role_arn,
        )
        .await;
        let (execution_arn, described) = match &row.input {
            Some(document) => {
                ran_with_input(&router, &machine_arn, Some(document.as_str()), case).await
            }
            None => ran(&router, &machine_arn, case).await,
        };
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: expected FAILED, got {described:?}"
        );
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            Some(REFUSAL_ERROR),
            "case {case:?}: expected the error {REFUSAL_ERROR:?}, got {described:?}"
        );
        let cause = string(&described, "cause");
        let id = relayed_request_id(case, &cause, &row.sentence, row.resource);
        assert!(
            named.insert(id.clone()),
            "case {case:?}: every call is named by its own request id, and {id} was already \
             relayed"
        );
        // The execution's own history reports the same failure, so a `Catch` path and a
        // history reader see one sentence.
        let details = task_failure(&router, &execution_arn, case).await;
        assert_eq!(
            string(&details, "cause"),
            cause,
            "case {case:?}: expected the history to carry the same cause, got {details:?}"
        );
    }

    // A call that SUCCEEDS relays its id too, in the response metadata of the result the
    // task reports rather than in a cause — and the execution it reported is the one that
    // really ran, which is what says the call was served.
    let case = "the optimized getQueryExecution on an execution Athena did mint";
    athena(
        &router,
        "CreateWorkGroup",
        &json!({
            "Name": "sfn-relayed-athena",
            "Configuration": {
                "ResultConfiguration": { "OutputLocation": RESULTS_LOCATION },
            },
        }),
    )
    .await;
    let submitted = athena(
        &router,
        "StartQueryExecution",
        &json!({ "QueryString": QUERY, "WorkGroup": "sfn-relayed-athena" }),
    )
    .await;
    let query_id = string(&submitted, "QueryExecutionId");
    let began = Instant::now();
    loop {
        let reported = athena(
            &router,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": query_id }),
        )
        .await;
        let state = reported["QueryExecution"]["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if !matches!(state.as_str(), "QUEUED" | "RUNNING") {
            assert_eq!(
                state, "SUCCEEDED",
                "case {case:?}: expected Athena to run the query, got {reported:?}"
            );
            break;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected Athena to settle the query within {PATIENCE:?}, got \
             {reported:?}"
        );
        ::tokio::time::sleep(GLANCE).await;
    }

    let machine_arn = machine(
        &router,
        "relayed-athena-success",
        "arn:aws:states:::athena:getQueryExecution",
        &json!({ "QueryExecutionId": query_id }),
        &role_arn,
    )
    .await;
    let (_, described) = ran(&router, &machine_arn, case).await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected SUCCEEDED, got {described:?}"
    );
    let output: Value = serde_json::from_str(&string(&described, "output"))
        .unwrap_or_else(|why| panic!("case {case:?}: expected a JSON result, got {why}"));
    assert_eq!(
        output["QueryExecution"]["QueryExecutionId"].as_str(),
        Some(query_id.as_str()),
        "case {case:?}: expected the result to report the execution that ran, got {output}"
    );
    let id = output["SdkResponseMetadata"]["RequestId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the SDK response metadata to name the request it \
                 answered, got {output}"
            )
        })
        .to_string();
    assert!(
        names_an_athena_request(&id),
        "case {case:?}: expected the relayed request id in Athena's own form — the hyphenated \
         8-4-4-4-12 hex groups of a UUID — got {id:?}"
    );
    assert!(
        named.insert(id.clone()),
        "case {case:?}: every call is named by its own request id, and {id} was already relayed"
    );
}
