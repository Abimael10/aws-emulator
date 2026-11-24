// SPDX-License-Identifier: CC0-1.0

//! The predefined WORKER TYPES a Glue request may name, and how a value outside that set is
//! refused.
//!
//! RULE: a member that names a predefined worker type admits exactly the value set the
//! service runs — on every operation that takes one — and a value outside that set is refused
//! 400 in AWS's own constraint envelope, naming the value the caller submitted, the member
//! path it was sent at and the whole set it had to come from, with nothing stored.
//!
//! The set the service runs is LARGER than this crate's Smithy model declares, so
//! [`ACCEPTED`] is written out here independently of the service: its first seven values are
//! `com.amazonaws.glue#WorkerType` as the model declares it
//! (`smithy_query(query="shape",shape="WorkerType")`), and the rest are the worker families a
//! recorded refusal of an unknown worker type enumerates beside them (corpus
//! `simparity-wire-aws-v1`, fixture `glue-gjwr-traj-hand-written-job-definition-refusals`
//! step `task#4`). Only the MEMBERSHIP of the set is asserted, never the order the service
//! lists it in: the clause's set is parsed out and compared as a set.
//!
//! The rows differ along what the rule generalises over: the worker family, its size, whether
//! it is a Graviton variant, which operation carries the member, and how a submitted value
//! misses the set — a case variant, an unknown size, an unknown family, a padded value, the
//! empty string.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ml_transforms::create_body as ml_transform_body;
use crate::wire::{call, constraint_message, emulator, error_code, ok};

/// Every worker type the service accepts. See the module note for the two sources.
const ACCEPTED: [&str; 23] = [
    "Standard",
    "G.025X",
    "G.1X",
    "G.2X",
    "G.4X",
    "G.8X",
    "G.12X",
    "G.16X",
    "Z.2X",
    "R.1X",
    "R.2X",
    "R.4X",
    "R.8X",
    "G.1X.GRAVITON",
    "G.2X.GRAVITON",
    "G.4X.GRAVITON",
    "G.8X.GRAVITON",
    "G.12X.GRAVITON",
    "G.16X.GRAVITON",
    "R.1X.GRAVITON",
    "R.2X.GRAVITON",
    "R.4X.GRAVITON",
    "R.8X.GRAVITON",
];

/// The seven values this crate's model declares, which the accepted set has to hold as well:
/// a set the service accepts cannot drop a value the model publishes.
const MODEL_DECLARED: [&str; 7] = [
    "Standard", "G.1X", "G.2X", "G.025X", "G.4X", "G.8X", "Z.2X",
];

/// Values outside the set, each missing it in a different way.
const OUTSIDE: [(&str, &str); 6] = [
    ("a case variant of a value in the set", "G.1x"),
    ("a size the service does not run", "G.32X"),
    ("a family the service does not run", "P.1X"),
    ("a Graviton variant in lower case", "g.2x.graviton"),
    ("a value in the set with a trailing space", "G.1X "),
    ("the empty string", ""),
];

/// The IAM role every definition staged here names.
const ROLE: &str = "arn:aws:iam::000000000000:role/sim2real-glue-worker-types";

/// How one row of the cross-operation table builds its request: the worker type it names, and
/// a suffix that keeps the resources two rows create apart.
type RequestFor = fn(&str, &str) -> Value;

/// One row of that table: the case, the operation, the request builder and the member path a
/// clause about its worker type names.
type OperationRow<'a> = (&'a str, &'a str, RequestFor, &'a str);

/// The `JobCommand` every job definition staged here runs.
fn command() -> Value {
    json!({ "Name": "glueetl", "ScriptLocation": "s3://etl/x.py" })
}

/// The whole message a constraint refusal of one worker type must carry, with the admissible
/// set left to the caller to compare as a SET: everything up to the set, and everything after
/// it.
fn clause_around_the_set(value: &str, member_path: &str) -> (String, String) {
    (
        format!(
            "1 validation error detected: Value '{value}' at '{member_path}' failed to satisfy \
             constraint: Member must satisfy enum value set: ["
        ),
        "]".to_string(),
    )
}

/// The value set a constraint clause states, as a sorted list — so the assertion is about
/// membership and not about the order the service lists the set in.
fn stated_set(case: &str, message: &str, value: &str, member_path: &str) -> Vec<String> {
    let (opening, closing) = clause_around_the_set(value, member_path);
    let rest = message.strip_prefix(&opening).unwrap_or_else(|| {
        panic!("case {case:?}: expected a clause opening {opening:?}, got {message:?}")
    });
    let inside = rest.strip_suffix(&closing).unwrap_or_else(|| {
        panic!("case {case:?}: expected a clause closing {closing:?}, got {message:?}")
    });
    let mut stated: Vec<String> = inside.split(", ").map(str::to_string).collect();
    stated.sort();
    stated
}

/// [`ACCEPTED`] as a sorted list, to compare a stated set against.
fn accepted_set() -> Vec<String> {
    let mut accepted: Vec<String> = ACCEPTED.iter().map(|value| (*value).to_string()).collect();
    accepted.sort();
    accepted
}

/// Assert that `operation` refused `request` as a worker type outside the set at
/// `member_path`, stating the whole set.
async fn assert_refused(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
    member_path: &str,
    value: &str,
) {
    let (status, body) = call(router, operation, request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: {operation} must refuse the worker type {value:?}, got {status} with \
         {body}"
    );
    assert!(
        error_code(&body).ends_with("ValidationException"),
        "case {case:?}: expected the validator's ValidationException, got {} in {body}",
        error_code(&body)
    );
    let message = constraint_message(&body);
    assert_eq!(
        stated_set(case, &message, value, member_path),
        accepted_set(),
        "case {case:?}: the clause must state every worker type the service accepts, got \
         {message:?}"
    );
}

/// Every worker type the service accepts is stored by a job create and read back by `GetJob`
/// — including the families this crate's model does not declare.
#[tokio::test]
async fn every_accepted_worker_type_is_stored_and_read_back() {
    let router = emulator();

    for (index, worker_type) in ACCEPTED.iter().enumerate() {
        let name = format!("worker-{index}");
        let created = ok(
            &router,
            "CreateJob",
            &json!({
                "Name": &name,
                "Role": ROLE,
                "Command": command(),
                "WorkerType": worker_type,
                "NumberOfWorkers": 2,
            }),
        )
        .await;
        assert_eq!(
            created.get("Name").and_then(Value::as_str),
            Some(name.as_str()),
            "case {worker_type:?}: the create must report the stored name, got {created}"
        );

        let read = ok(&router, "GetJob", &json!({ "JobName": &name })).await;
        assert_eq!(
            read["Job"].get("WorkerType").and_then(Value::as_str),
            Some(*worker_type),
            "case {worker_type:?}: the stored definition must report the worker type it was \
             created with, got {read}"
        );
    }
}

/// The accepted set holds every value this crate's model declares: what the service runs is a
/// superset of what the model publishes, never a different set.
#[test]
fn the_accepted_set_holds_every_worker_type_the_model_declares() {
    for declared in MODEL_DECLARED {
        assert!(
            ACCEPTED.contains(&declared),
            "case {declared:?}: a worker type the model declares must be accepted, got \
             {ACCEPTED:?}"
        );
    }
}

/// A worker type outside the set is refused by the job create, in AWS's constraint envelope
/// naming the submitted value and the whole admissible set — and the create stores nothing.
#[tokio::test]
async fn a_worker_type_outside_the_set_is_refused_and_stores_nothing() {
    let router = emulator();

    for (index, (case, worker_type)) in OUTSIDE.iter().enumerate() {
        let name = format!("refused-{index}");
        assert_refused(
            &router,
            case,
            "CreateJob",
            &json!({
                "Name": &name,
                "Role": ROLE,
                "Command": command(),
                "WorkerType": worker_type,
                "NumberOfWorkers": 2,
            }),
            "workerType",
            worker_type,
        )
        .await;

        // A refused create stored nothing, so the read that follows misses.
        let (status, body) = call(&router, "GetJob", &json!({ "JobName": &name })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create must store no definition, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("EntityNotFoundException"),
            "case {case:?}: expected the miss, got {} in {body}",
            error_code(&body)
        );
    }
}

/// The same rule on every other operation that takes a worker type: one of the families the
/// model does not declare is served, and a value outside the set is refused with the clause at
/// that operation's own member path.
#[tokio::test]
async fn the_rule_holds_on_every_operation_that_takes_a_worker_type() {
    let router = emulator();

    // A job definition and a machine learning transform the updates below address.
    ok(
        &router,
        "CreateJob",
        &json!({ "Name": "staged-job", "Role": ROLE, "Command": command() }),
    )
    .await;
    let transform = ok(
        &router,
        "CreateMLTransform",
        &ml_transform_body("staged-transform", json!({})),
    )
    .await;
    let transform_id = transform
        .get("TransformId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the create must report a TransformId, got {transform}"))
        .to_string();

    // (case, operation, the request with the worker type the row names filled in, the member
    // path a clause about it names). `R.2X.GRAVITON` is a family the model does not declare,
    // so a 200 on it is what shows the set the operation admits is the service's.
    let accepted = "R.2X.GRAVITON";
    let rows: Vec<OperationRow<'_>> = vec![
        (
            "the job update's definition",
            "UpdateJob",
            |worker_type, _| {
                json!({
                    "JobName": "staged-job",
                    "JobUpdate": {
                        "Role": ROLE,
                        "Command": { "Name": "glueetl", "ScriptLocation": "s3://etl/x.py" },
                        "WorkerType": worker_type,
                        // A definition that states its capacity as workers states both
                        // halves of it (`crate::job_capacity_refusal`).
                        "NumberOfWorkers": 2,
                    },
                })
            },
            "jobUpdate.workerType",
        ),
        (
            "the job run's own capacity",
            "StartJobRun",
            |worker_type, _| {
                json!({
                    "JobName": "staged-job",
                    "WorkerType": worker_type,
                    "NumberOfWorkers": 2,
                })
            },
            "workerType",
        ),
        (
            "the session create",
            "CreateSession",
            |worker_type, suffix| {
                json!({
                    "Id": format!("session-{suffix}"),
                    "Role": ROLE,
                    "Command": { "Name": "glueetl" },
                    "WorkerType": worker_type,
                    "NumberOfWorkers": 2,
                })
            },
            "workerType",
        ),
        (
            "the machine learning transform create",
            "CreateMLTransform",
            |worker_type, suffix| {
                ml_transform_body(
                    &format!("transform-{suffix}"),
                    json!({ "WorkerType": worker_type, "NumberOfWorkers": 2 }),
                )
            },
            "workerType",
        ),
    ];

    for (case, operation, request, member_path) in &rows {
        // The value outside the set is refused, at this operation's own member path.
        let outside = "G.1x";
        assert_refused(
            &router,
            case,
            operation,
            &request(outside, "refused"),
            member_path,
            outside,
        )
        .await;

        // And a family the model does not declare is served.
        ok(&router, operation, &request(accepted, "served")).await;
    }

    // The transform update takes one too, and it is the stored transform it addresses.
    let update = |worker_type: &str| {
        json!({
            "TransformId": &transform_id,
            "WorkerType": worker_type,
            "NumberOfWorkers": 2,
        })
    };
    assert_refused(
        &router,
        "the machine learning transform update",
        "UpdateMLTransform",
        &update("G.1x"),
        "workerType",
        "G.1x",
    )
    .await;
    ok(&router, "UpdateMLTransform", &update(accepted)).await;
    let read = ok(
        &router,
        "GetMLTransform",
        &json!({ "TransformId": &transform_id }),
    )
    .await;
    assert_eq!(
        read.get("WorkerType").and_then(Value::as_str),
        Some(accepted),
        "the update must store the worker type it was given, got {read}"
    );
}
