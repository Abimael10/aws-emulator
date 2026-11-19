// SPDX-License-Identifier: CC0-1.0

//! The id an AWS Glue answer names its request by, as a Step Functions `Task` state RELAYS
//! it — driven across the three services the question spans (Glue serves and words the
//! refusal, IAM holds the role the call is made as, Step Functions renders the exception
//! around it), mounted on ONE engine as `core/router/src/lib.rs` mounts them. The id belongs
//! to the ONE in-process call the task made, so neither crate alone can say what reaches a
//! client's `cause`.
//!
//! RULE: a refusal a `Task` state relays is reported as the client that served the call
//! renders a service exception — the service's own sentence followed by the metadata that
//! answer carried — and the id of the refused request is one of those fields: the earlier
//! generation of client, which serves an optimized `Resource`, closes with
//! `(Service: <client>; Status Code: <status>; Error Code: <code>; Request ID: <id>;
//! Proxy: null)`, and the current AWS SDK, which serves an
//! `arn:aws:states:::aws-sdk:<service>:<action>` one, with
//! `(Service: <service>, Status Code: <status>, Request ID: <id>) (SDK Attempt Count: <n>)`.
//! Neither form may drop the `Request ID` field: every response an emulated service sends
//! names the request it answered, so the relay always has an id to carry — and a consumer
//! that parses that envelope out of a cause, or an operator correlating the failed task with
//! the underlying call, reads exactly that field.
//!
//! `sfn_relayed_athena_request_id.rs` asks the same question of Athena; this suite asks it of
//! the Glue integration, whose refusals are relayed through the same one rendering, and adds
//! what only a refused START can say: a task whose starting call was refused SUBMITTED no
//! job, so its history carries no `TaskSubmitted` event and Glue holds no run of the job.
//!
//! The rows differ along what the rule generalises over: the integration family that serves
//! the call (optimized and AWS SDK), the pattern the `Resource` asks for (request-response,
//! `.sync` and `.sync:2`), the error class the answer names (`EntityNotFoundException` and
//! `InvalidInputException`), and what makes Glue refuse the start (a job that does not
//! exist, a previous run that does not, and a timeout past the documented ceiling) — so one
//! row's sentence differs from another's under the same class.
//!
//! Nothing here asserts a request id's VALUE: the id belongs to the one call that was
//! refused, so each row holds the whole envelope AROUND it (through the harness's `masked`
//! and `refusal_metadata`), takes the FORM it must be spelled in from the id Glue's own
//! direct answer named its request by, and the suite holds every id it reads to being
//! distinct — a constant stitched into the envelope would fail that. No assertion reads the
//! clock and nothing sleeps for a span: an execution's outcome is polled for until it is
//! there, against a generous deadline.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The harness the Step Functions suites of this test crate build: the state machine of one
// `Task`, the execution reader, the awsJson caller for Step Functions itself, and the two
// readers that hold a relayed refusal to the text its client wrote. `tests/all/main.rs` is
// one crate root, so this suite borrows them rather than carrying a second copy.
use crate::sfn_task_target_authorization::{
    allow, logs_only, machine, masked, named_request, ran, refusal_metadata, role, sfn, string,
    ACCOUNT, REGION,
};

/// The header AWS names a response's request in, written out here rather than read from any
/// crate so the assertion is against what a client reads.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The header an `awsJson` refusal names its error class in.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The id a client reads as "no id at all": no relayed envelope may name it.
const PLACEHOLDER_ID: &str = "00000000-0000-0000-0000-000000000000";

/// The name the client that serves an OPTIMIZED Glue integration reports the service under,
/// which is the one the recorded cause carries.
const OPTIMIZED_CLIENT: &str = "AWSGlue";

/// The name the current AWS SDK's own `Glue` client reports it under, for the family served
/// by that SDK.
const SDK_SERVICE: &str = "Glue";

/// The name Step Functions writes for Glue in the error name of a failed task, which both
/// families' clients give the service.
const TASK_ERROR_PREFIX: &str = "Glue";

/// The sentence Glue answers a start naming a job whose metadata it cannot read with, which
/// a relayed cause carries whole.
const MISSING_METADATA: &str = "Failed to start job run due to missing metadata.";

/// The status Glue answers each of the classes below with
/// (`smithy_query(query="operation",operation="StartJobRun",model_path="services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json")`:
/// `EntityNotFoundException` and `InvalidInputException`, errorKind client, httpError 400).
const REFUSAL_STATUS: u16 = 400;

/// The error class a start naming a job, or a previous run, that does not exist is refused
/// under; see [`REFUSAL_STATUS`] for where it is read from.
const ENTITY_NOT_FOUND: &str = "EntityNotFoundException";

/// The error class a start whose own members Glue will not take is refused under; see
/// [`REFUSAL_STATUS`].
const INVALID_INPUT: &str = "InvalidInputException";

/// "Jobs must have timeout values less than 7 days or 10080 minutes" (`StartJobRun$Timeout`):
/// one minute past that documented ceiling is a value Glue will not start a run with.
const TIMEOUT_PAST_THE_CEILING: i64 = 10_081;

/// The event a task that submitted work for the run-a-job pattern writes
/// (`smithy_query(query="shape",shape="HistoryEventType")`), which a refused start writes
/// none of.
const TASK_SUBMITTED: &str = "TaskSubmitted";

/// The three services this question spans — Glue answers the call, IAM holds the role the
/// call is made as, Step Functions reports what came back — over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them.
///
/// IAM is mounted so each row's execution role is a role that really exists, with a grant
/// over the actions the start and the run-a-job pattern's own calls take: that is what makes
/// a row's failure the RELAY's and not the authorization gate's.
///
/// Request authentication is off, as in every other suite here; the in-process call Step
/// Functions makes signs its own request either way, and its credential scope is what routes
/// it to Glue.
fn emulator_with_glue() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        engine,
    ));
    router
}

/// What one Glue call came back with, read the way a CLIENT reads it: the status, the id the
/// answer named its request by, the class it named and its body.
struct Answered {
    status: u16,
    request_id: Option<String>,
    error_class: Option<String>,
    body: Value,
    text: String,
}

/// Call one Glue operation (`awsJson1_1` under the target prefix `AWSGlue`, from that
/// service's model's `service_info`) and report what it answered, refusal and all.
///
/// The harness's own caller requires a 200 and drops the response's headers, and the headers
/// are the point here: this suite reads the id Glue's answer names its request by out of
/// them.
async fn glue(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answered {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("glue.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSGlue.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/glue/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, payload) = response.into_parts();
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let text = String::from_utf8_lossy(&payload).to_string();
    let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let error_class = header(ERROR_TYPE_HEADER)
        .or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .map(|named| bare_class(&named));
    Answered {
        status: parts.status.as_u16(),
        request_id: header(REQUEST_ID_HEADER),
        error_class,
        body,
        text,
    }
}

/// The CLASS name out of the discriminator an `awsJson` refusal carries: a shape id's
/// namespace, and the URL some services decorate it with, are no part of the class.
fn bare_class(named: &str) -> String {
    named
        .split(':')
        .next()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Call one Glue operation, requiring a 200.
async fn glue_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let answered = glue(router, operation, body).await;
    assert_eq!(
        answered.status, 200,
        "{operation}: expected 200 from Glue, got {} with {}",
        answered.status, answered.text
    );
    answered.body
}

/// Create the Glue job `name`, so a row that needs an existing job has one of its own.
async fn create_job(router: &MultiServiceRouter, name: &str) {
    let created = glue_ok(
        router,
        "CreateJob",
        &json!({
            "Name": name,
            "Role": format!("arn:aws:iam::{ACCOUNT}:role/GlueJobRole"),
            "Command": {
                "Name": "pythonshell",
                "ScriptLocation": format!("s3://xsvc-sfn-glue-relay/{name}.py")
            }
        }),
    )
    .await;
    assert_eq!(
        created.get("Name").and_then(Value::as_str),
        Some(name),
        "setup: CreateJob must answer with the name it stored, got {created:?}"
    );
}

/// The runs Glue holds for the job `name`, which a refused start adds none to.
async fn job_runs(router: &MultiServiceRouter, name: &str) -> Vec<Value> {
    let answered = glue_ok(router, "GetJobRuns", &json!({ "JobName": name })).await;
    answered
        .get("JobRuns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// How many events of type `TaskSubmitted` an execution's history carries.
async fn submissions(router: &MultiServiceRouter, execution_arn: &str) -> usize {
    let history = sfn(
        router,
        "GetExecutionHistory",
        &json!({ "executionArn": execution_arn, "maxResults": 100 }),
    )
    .await;
    history
        .get("events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter(|event| event.get("type").and_then(Value::as_str) == Some(TASK_SUBMITTED))
                .count()
        })
        .unwrap_or_default()
}

/// How the `Resource` of a row names the call, and what Glue is asked to start.
struct Relayed {
    case: &'static str,
    /// The `Resource` the `Task` names, written whole so each row states the family and the
    /// pattern it asks for.
    resource: &'static str,
    /// Whether the job the start names is one this suite created.
    job_exists: bool,
    /// The id of a previous run the start asks to retry, which this suite never creates.
    retries: Option<&'static str>,
    /// The timeout the start carries, in minutes.
    timeout: Option<i64>,
    /// The class Glue's answer must name, which is the error half of the task's error name.
    code: &'static str,
    /// The sentence that answer must carry, where AWS states it as fixed text; `None` for a
    /// row that holds the relay to carrying whatever sentence Glue worded.
    sentence: Option<&'static str>,
}

/// The parameters a row's `Task` builds, which ARE the `StartJobRun` request.
fn parameters(row: &Relayed, job_name: &str) -> Value {
    let mut request = json!({ "JobName": job_name });
    if let Some(previous) = row.retries {
        request["JobRunId"] = json!(previous);
    }
    if let Some(timeout) = row.timeout {
        request["Timeout"] = json!(timeout);
    }
    request
}

/// Whether `id` is spelled the way `pattern` is: the same hyphen-separated group widths over
/// the same character class.
///
/// NOT a statement of which form that is — `pattern` is the id GLUE's own direct answer
/// named its request by, read in the same row — so what is held is that the relay reports an
/// id of the serving service's own spelling, whatever spelling that service uses.
fn spelled_like(id: &str, pattern: &str) -> bool {
    let widths = |value: &str| -> Vec<usize> { value.split('-').map(str::len).collect() };
    widths(id) == widths(pattern)
        && id
            .chars()
            .all(|character| character == '-' || character.is_ascii_hexdigit())
}

/// The id the relayed sentence names the refused request by, with the WHOLE sentence pinned
/// around it: the service's own message, then the metadata the client of `resource`'s family
/// closes with, with the id masked ([`masked`]).
fn relayed_request_id(case: &str, cause: &str, message: &str, resource: &str, code: &str) -> String {
    let client = match resource.contains(":aws-sdk:") {
        true => SDK_SERVICE,
        false => OPTIMIZED_CLIENT,
    };
    let expected = format!(
        "{message}{}",
        refusal_metadata(resource, client, code, REFUSAL_STATUS)
    );
    let reported = masked(cause);
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the metadata Glue's own client appends, got {cause:?}"
    );
    named_request(cause).unwrap_or_else(|| {
        panic!("case {case:?}: expected the relayed metadata to name the request, got {cause:?}")
    })
}

/// RULE: the cause a relayed Glue refusal reaches a client as is the exception the serving
/// client renders, and it names the request the refused call answered.
///
/// Each row makes the SAME call twice: once directly against Glue, which is where the status,
/// the class and the sentence the envelope must carry come from, and once through a `Task`
/// state, whose cause must be that same refusal rendered by the client the `Resource`'s
/// family is served by — with the id field carrying an id of this call's own. No job run is
/// submitted for a start that was refused, so the history carries no `TaskSubmitted` event
/// and Glue holds no run of the job.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relayed_glue_refusal_names_the_request_it_was_answered_for() {
    let rows = [
        Relayed {
            case: "the run-a-job pattern over a job name that does not exist",
            resource: "arn:aws:states:::glue:startJobRun.sync",
            job_exists: false,
            retries: None,
            timeout: None,
            code: ENTITY_NOT_FOUND,
            sentence: Some(MISSING_METADATA),
        },
        Relayed {
            case: "the request-response form of that same refused start",
            resource: "arn:aws:states:::glue:startJobRun",
            job_exists: false,
            retries: None,
            timeout: None,
            code: ENTITY_NOT_FOUND,
            sentence: Some(MISSING_METADATA),
        },
        Relayed {
            case: "the second revision of the run-a-job pattern, retrying a run that does \
                   not exist",
            resource: "arn:aws:states:::glue:startJobRun.sync:2",
            job_exists: true,
            retries: Some("jr_0000000000000000000000000000000000000000000000000000000000000000"),
            timeout: None,
            code: ENTITY_NOT_FOUND,
            sentence: None,
        },
        Relayed {
            case: "a timeout past the documented ceiling, which is another class entirely",
            resource: "arn:aws:states:::glue:startJobRun.sync",
            job_exists: true,
            retries: None,
            timeout: Some(TIMEOUT_PAST_THE_CEILING),
            code: INVALID_INPUT,
            sentence: None,
        },
        Relayed {
            case: "the AWS SDK integration family, whose client renders the same refusal its \
                   own way",
            resource: "arn:aws:states:::aws-sdk:glue:startJobRun",
            job_exists: false,
            retries: None,
            timeout: None,
            code: ENTITY_NOT_FOUND,
            sentence: Some(MISSING_METADATA),
        },
    ];

    let router = emulator_with_glue();
    // Every integration call below is one Glue start, and the run-a-job rows report and stop
    // the run they start, so the role carries a grant over that service's own actions beside
    // the log writes every state machine makes.
    let role_arn = role(
        &router,
        "RelayedGlueRefusalRole",
        &json!({
            "Version": "2012-10-17",
            "Statement": [logs_only(), allow("glue:*", "*")],
        }),
    )
    .await;
    // Every id this suite reads: each call is its own request, so no two of them may repeat
    // one — which an envelope carrying a constant could not satisfy.
    let mut named: BTreeSet<String> = BTreeSet::new();

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let job_name = format!("xsvc-sfn-glue-relay-{index}");
        if row.job_exists {
            create_job(&router, &job_name).await;
        }
        let request = parameters(row, &job_name);

        // What GLUE answers this very start: the status, the class and the sentence the
        // relayed envelope has to carry, read from the service rather than written here.
        let direct = glue(&router, "StartJobRun", &request).await;
        assert_eq!(
            direct.status, REFUSAL_STATUS,
            "case {case:?}: expected Glue to refuse the start with HTTP {REFUSAL_STATUS}, got \
             {} with {}",
            direct.status, direct.text
        );
        assert_eq!(
            direct.error_class.as_deref(),
            Some(row.code),
            "case {case:?}: expected the class {:?}, got {:?} in {}",
            row.code,
            direct.error_class,
            direct.text
        );
        let message = direct
            .body
            .get("Message")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a Message in Glue's refusal {}", direct.text)
            })
            .to_string();
        if let Some(expected) = row.sentence {
            assert_eq!(
                message, expected,
                "case {case:?}: expected the sentence {expected:?}, got {message:?}"
            );
        }
        let direct_id = direct.request_id.clone().unwrap_or_else(|| {
            panic!(
                "case {case:?}: every response names the request it answered, got none with {}",
                direct.text
            )
        });
        assert!(
            !direct_id.is_empty() && direct_id != PLACEHOLDER_ID,
            "case {case:?}: expected Glue's own answer to name a real request, got {direct_id:?}"
        );
        assert!(
            named.insert(direct_id.clone()),
            "case {case:?}: expected an answer of this call's own, got {direct_id:?}, which \
             another call already reported"
        );

        // The same start, made by a `Task` state: it fails, under the error name the class
        // resolves to, with that refusal as its cause.
        let machine_arn = machine(
            &router,
            &format!("xsvc-sfn-glue-relay-{index}"),
            row.resource,
            &request,
            &role_arn,
        )
        .await;
        let (execution_arn, described) = ran(&router, &machine_arn, case).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: Glue refused the start, so the task must fail, got {described:?}"
        );
        let expected_error = format!("{TASK_ERROR_PREFIX}.{}", row.code);
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            Some(expected_error.as_str()),
            "case {case:?}: expected the error name {expected_error:?}, got {described:?}"
        );

        // The cause is the exception the serving client renders, and the id field in it is
        // this call's own.
        let cause = string(&described, "cause");
        let relayed_id = relayed_request_id(case, &cause, &message, row.resource, row.code);
        assert!(
            !relayed_id.is_empty() && relayed_id != PLACEHOLDER_ID,
            "case {case:?}: expected the relayed envelope to name a real request, got \
             {relayed_id:?} in {cause:?}"
        );
        assert!(
            spelled_like(&relayed_id, &direct_id),
            "case {case:?}: expected an id spelled as Glue's own answers spell one \
             ({direct_id:?}), got {relayed_id:?} in {cause:?}"
        );
        assert!(
            named.insert(relayed_id.clone()),
            "case {case:?}: expected the refused call's own id, got {relayed_id:?}, which \
             another call already reported"
        );

        // Nothing was submitted for a start that was refused: the history carries no
        // submission and Glue holds no run of the job.
        let submitted = submissions(&router, &execution_arn).await;
        assert_eq!(
            submitted, 0,
            "case {case:?}: no job run was started, so the history carries no \
             {TASK_SUBMITTED}, got {submitted} of them"
        );
        if row.job_exists {
            let runs = job_runs(&router, &job_name).await;
            assert!(
                runs.is_empty(),
                "case {case:?}: a refused start leaves the job with no run, got {runs:?}"
            );
        }
    }
}
