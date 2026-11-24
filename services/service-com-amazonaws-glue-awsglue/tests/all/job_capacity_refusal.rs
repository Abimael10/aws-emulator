// SPDX-License-Identifier: CC0-1.0

//! The refusals every operation that takes a job's CAPACITY answers for a capacity it cannot
//! make sense of: `CreateJob`, `UpdateJob` and `StartJobRun`.
//!
//! Two rules, both over the capacity members:
//!
//! 1. "do not set `MaxCapacity` if using `WorkerType` and `NumberOfWorkers`"
//!    (`CreateJobRequest$MaxCapacity`) — a request carrying `MaxCapacity` together with either
//!    of the worker members is refused with HTTP 400 `InvalidInputException` and ONE fixed
//!    sentence of the service's own, the same sentence whichever of the three operations
//!    refused it and whatever values the request carried.
//! 2. "Instead, you should specify a `Worker type` and the `Number of workers`" (the same
//!    member's documentation) — a whole job DEFINITION (`CreateJob`, `UpdateJob`, each of
//!    which states the definition the job runs under) that expresses its capacity as workers
//!    states BOTH halves of it, so one of them alone is refused with the service's own
//!    sentence for that rule, and it allocates at least as many workers as the service admits
//!    for that worker type — a floor the refusal names beside the type, and which is two,
//!    because a job's documented capacity range starts at two DPUs ("you can allocate from 2
//!    to 100 DPUs"). A RUN (`StartJobRun`) states an override over a stored definition that
//!    already holds a whole capacity, so a run naming one of them, or fewer workers, is
//!    served.
//! 3. A worker configuration belongs to a command the service SIZES in workers. A Python
//!    shell job's capacity is a DPU figure ("When you specify a Python shell job
//!    (`JobCommand.Name`="pythonshell"), you can allocate either 0.0625 or 1 DPU") and the
//!    worker types are documented for Spark and Ray jobs, so a definition that gives a Python
//!    shell job a worker type is refused with the sentence naming the command it asked for —
//!    while every other command name, documented or not, carries one.
//!
//! A request naming its capacity one admissible way is served.
//!
//! Each suite is a TABLE whose rows differ along what the rules generalise over: which
//! worker member accompanies `MaxCapacity`, how many of them do, which half of the worker
//! pair a definition states, the capacity and worker values themselves, and which operation
//! the request was sent to. The recorded cases — a create carrying `MaxCapacity` beside
//! `WorkerType` and `NumberOfWorkers`, and a create stating one half of the worker pair — are
//! each one row.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok};

/// The role ARN every job definition below is created with.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/sim2real-glue-job-role";

/// The whole sentence the service refuses a doubly-named capacity with: fixed text with no
/// variable part, stating the two capacity members in the service's prose spelling and
/// ending in a period. Shared by `CreateJob`, `UpdateJob` and `StartJobRun`, which is what
/// the rows below assert of each.
const CAPACITY_REFUSAL: &str =
    "Please do not set Max Capacity if using Worker Type and Number of Workers.";

/// The whole sentence the service refuses a definition stating HALF a worker capacity with:
/// fixed text with no variable part, naming the two members in the same prose spelling
/// [`CAPACITY_REFUSAL`] names them in. Shared by the two writes that carry a whole job
/// definition, which is what the rows below assert of each.
const WORKER_PAIR_REFUSAL: &str = "Please set both Worker Type and Number of Workers.";

/// The fewest workers of any type a job definition may allocate, which the refusal below
/// names beside the type. See rule 2 in the module note for where the floor comes from.
const MIN_WORKERS: i64 = 2;

/// The whole sentence a definition allocating fewer than [`MIN_WORKERS`] workers is refused
/// with: fixed text around the worker type the definition named and the floor the service
/// publishes for it.
fn below_the_floor(worker_type: &str) -> String {
    format!(
        "To use Worker Type {worker_type}, minimum allowed value of Number of Workers is \
         {MIN_WORKERS}."
    )
}

/// A `CreateJob` body: the `@required` members plus whatever `extra` adds.
fn create_body(name: &str, extra: &Value) -> Value {
    create_body_running(name, "glueetl", extra)
}

/// The same, for a definition naming the command `command_name`.
fn create_body_running(name: &str, command_name: &str, extra: &Value) -> Value {
    let mut body = json!({
        "Name": name,
        "Role": ROLE_ARN,
        "Command": { "Name": command_name, "ScriptLocation": "s3://etl/x.py" },
    });
    merge(&mut body, extra);
    body
}

/// A `JobUpdate` carrying the definition's `@required` members plus whatever `extra` adds,
/// which is what `UpdateJob` submits as the whole definition.
fn update_body(name: &str, extra: &Value) -> Value {
    update_body_running(name, "glueetl", extra)
}

/// The same, for a definition naming the command `command_name`.
fn update_body_running(name: &str, command_name: &str, extra: &Value) -> Value {
    let mut update = json!({
        "Role": ROLE_ARN,
        "Command": { "Name": command_name, "ScriptLocation": "s3://etl/x.py" },
    });
    merge(&mut update, extra);
    json!({ "JobName": name, "JobUpdate": update })
}

/// A `StartJobRun` body for the job `name`, carrying whatever `extra` adds.
fn start_body(name: &str, extra: &Value) -> Value {
    let mut body = json!({ "JobName": name });
    merge(&mut body, extra);
    body
}

/// Copy every member of `extra` onto `body`.
fn merge(body: &mut Value, extra: &Value) {
    let members = extra.as_object().expect("the capacity members are an object");
    for (member, value) in members {
        body[member] = value.clone();
    }
}

/// The capacity members a request may carry two ways at once. Each row names
/// `MaxCapacity` beside at least one worker member, which is the ambiguity the rule
/// refuses; the values differ across rows so nothing can be keyed to one of them.
fn ambiguous_capacities() -> Vec<(&'static str, Value)> {
    vec![
        (
            // The recorded case: both worker members beside a maximum capacity.
            "a maximum capacity beside a worker type and a worker count",
            json!({ "MaxCapacity": 2.0, "WorkerType": "G.1X", "NumberOfWorkers": 2 }),
        ),
        (
            "a maximum capacity beside a worker type alone",
            json!({ "MaxCapacity": 10.0, "WorkerType": "G.2X" }),
        ),
        (
            "a maximum capacity beside a worker count alone",
            json!({ "MaxCapacity": 0.0625, "NumberOfWorkers": 17 }),
        ),
        (
            "a fractional maximum capacity beside the largest worker type",
            json!({ "MaxCapacity": 1.0, "WorkerType": "G.8X", "NumberOfWorkers": 1 }),
        ),
    ]
}

/// Assert that `operation` refused `body` with `expected` as the whole sentence.
async fn assert_refused_with(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
    expected: &str,
) {
    let (status, answer) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?} / {operation}: expected 400, got {status} with {answer}"
    );
    assert!(
        error_code(&answer).contains("InvalidInputException"),
        "case {case:?} / {operation}: expected InvalidInputException, got {answer}"
    );
    let message = error_message(&answer);
    assert_eq!(
        message, expected,
        "case {case:?} / {operation}: expected the service's own sentence {expected:?}, got \
         {message:?}"
    );
}

/// Assert that `operation` refused `body` with the one sentence the service words the
/// doubly-named capacity refusal as.
async fn assert_refused(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) {
    assert_refused_with(router, case, operation, body, CAPACITY_REFUSAL).await;
}

/// A create, an update and a start that each name a job's capacity both ways are refused
/// with the same status, the same modelled error and the same sentence — and the refused
/// write stores nothing.
#[tokio::test]
async fn a_capacity_named_both_ways_is_refused_with_one_sentence() {
    let router = emulator();
    // One stored definition for the update and the start to address, created with no
    // capacity members at all so the refusals below are about the submitted request.
    ok(&router, "CreateJob", &create_body("capacity", &json!({}))).await;
    let stored = ok(&router, "GetJob", &json!({ "JobName": "capacity" })).await;

    for (case, capacity) in ambiguous_capacities() {
        assert_refused(
            &router,
            case,
            "CreateJob",
            &create_body("created-by-this-row", &capacity),
        )
        .await;
        // The refused create stored nothing: the name it asked for is still free.
        let (status, answer) = call(
            &router,
            "GetJob",
            &json!({ "JobName": "created-by-this-row" }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create must store no job definition, got {status} with \
             {answer}"
        );

        assert_refused(&router, case, "UpdateJob", &update_body("capacity", &capacity)).await;
        assert_refused(&router, case, "StartJobRun", &start_body("capacity", &capacity)).await;

        // The refused update and start left the stored definition exactly as it was.
        let after = ok(&router, "GetJob", &json!({ "JobName": "capacity" })).await;
        assert_eq!(
            after, stored,
            "case {case:?}: a refused request must leave the stored definition untouched; \
             expected {stored}, got {after}"
        );
        let runs = ok(&router, "GetJobRuns", &json!({ "JobName": "capacity" })).await;
        let started = runs["JobRuns"].as_array().map_or(0, Vec::len);
        assert_eq!(
            started, 0,
            "case {case:?}: a refused start must record no run, got {runs}"
        );
    }
}

/// A capacity named only ONE admissible way is not these refusals' business: a maximum
/// capacity alone, and a worker type together with a worker count, are each served by all
/// three operations.
#[tokio::test]
async fn a_capacity_named_one_way_is_served() {
    // (case, the capacity members the request carries)
    let admitted: Vec<(&str, Value)> = vec![
        ("a maximum capacity alone", json!({ "MaxCapacity": 2.0 })),
        (
            "a worker type with a worker count",
            json!({ "WorkerType": "G.1X", "NumberOfWorkers": 2 }),
        ),
        (
            "the largest worker type at the worker floor",
            json!({ "WorkerType": "G.8X", "NumberOfWorkers": MIN_WORKERS }),
        ),
    ];

    for (index, (case, capacity)) in admitted.iter().enumerate() {
        let router = emulator();
        let name = format!("served-{index}");
        let created = ok(&router, "CreateJob", &create_body(&name, capacity)).await;
        assert_eq!(
            created["Name"],
            json!(name),
            "case {case:?}: expected the create to report the job's name, got {created}"
        );

        let updated = ok(&router, "UpdateJob", &update_body(&name, capacity)).await;
        assert_eq!(
            updated["JobName"],
            json!(name),
            "case {case:?}: expected the update to report the job's name, got {updated}"
        );

        let started = ok(&router, "StartJobRun", &start_body(&name, capacity)).await;
        assert!(
            started["JobRunId"].as_str().is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected the start to report the run it recorded, got {started}"
        );
    }
}

/// A job DEFINITION that expresses its capacity as workers has to express both halves of it:
/// a create or an update naming one of them alone is refused with the service's own sentence
/// for that rule and writes nothing, while a RUN — which overrides the capacity of a stored
/// definition that already holds both halves — is served.
///
/// The rows differ along which half the definition states and the value it gives it.
#[tokio::test]
async fn a_definition_stating_half_a_worker_capacity_is_refused() {
    let router = emulator();
    // One stored definition for the update and the start to address, created with a whole
    // worker capacity so the refusals below are about the submitted request — and with room
    // for one run per row, since each row starts one ("the maximum number of concurrent runs
    // allowed for this job", `ExecutionProperty$MaxConcurrentRuns`).
    let whole = json!({
        "WorkerType": "G.1X",
        "NumberOfWorkers": 2,
        "ExecutionProperty": { "MaxConcurrentRuns": 8 },
    });
    ok(&router, "CreateJob", &create_body("worker-pair", &whole)).await;
    let stored = ok(&router, "GetJob", &json!({ "JobName": "worker-pair" })).await;

    // (case, the half of the pair the definition states)
    let halves: Vec<(&str, Value)> = vec![
        ("a worker type alone", json!({ "WorkerType": "G.2X" })),
        (
            "another worker type alone",
            json!({ "WorkerType": "G.025X" }),
        ),
        ("a worker count alone", json!({ "NumberOfWorkers": 5 })),
        ("a single worker, counted alone", json!({ "NumberOfWorkers": 1 })),
    ];

    for (case, half) in &halves {
        assert_refused_with(
            &router,
            case,
            "CreateJob",
            &create_body("created-by-this-row", half),
            WORKER_PAIR_REFUSAL,
        )
        .await;
        // The refused create stored nothing: the name it asked for is still free.
        let (status, answer) = call(
            &router,
            "GetJob",
            &json!({ "JobName": "created-by-this-row" }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create must store no job definition, got {status} with \
             {answer}"
        );

        assert_refused_with(
            &router,
            case,
            "UpdateJob",
            &update_body("worker-pair", half),
            WORKER_PAIR_REFUSAL,
        )
        .await;

        // The refused update left the stored definition exactly as it was.
        let after = ok(&router, "GetJob", &json!({ "JobName": "worker-pair" })).await;
        assert_eq!(
            after, stored,
            "case {case:?}: a refused update must leave the stored definition untouched; \
             expected {stored}, got {after}"
        );

        // A RUN of that definition overrides half of a capacity the definition already
        // states whole, so it is served.
        let started = ok(&router, "StartJobRun", &start_body("worker-pair", half)).await;
        assert!(
            started["JobRunId"].as_str().is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected the start to report the run it recorded, got {started}"
        );
    }
}

/// A job DEFINITION allocates at least as many workers as the service admits for the worker
/// type it names: a create or an update below that floor is refused with the sentence naming
/// the type and the floor, and writes nothing — while the floor itself, and anything above it,
/// is served.
///
/// The rows differ along the worker type, how far below the floor the count is (one worker,
/// none, a negative count — `NumberOfWorkers` is a `NullableInteger` with no modelled range,
/// so each of them reaches the service) and, on the other side of the rule, how far above it.
#[tokio::test]
async fn a_definition_below_the_worker_floor_is_refused() {
    let router = emulator();
    let whole = json!({
        "WorkerType": "G.1X",
        "NumberOfWorkers": 2,
        "ExecutionProperty": { "MaxConcurrentRuns": 8 },
    });
    ok(&router, "CreateJob", &create_body("worker-floor", &whole)).await;
    let stored = ok(&router, "GetJob", &json!({ "JobName": "worker-floor" })).await;

    // (case, the worker type the definition names, the count it allocates)
    let below: Vec<(&str, &str, i64)> = vec![
        ("the recorded worker type with one worker", "G.1X", 1),
        ("the smallest worker type with one worker", "G.025X", 1),
        ("a Graviton worker type with one worker", "G.2X.GRAVITON", 1),
        ("a memory-optimised worker type with none", "R.1X", 0),
        ("the largest worker type with a negative count", "G.8X", -1),
    ];

    for (case, worker_type, workers) in &below {
        let capacity = json!({ "WorkerType": worker_type, "NumberOfWorkers": workers });
        assert_refused_with(
            &router,
            case,
            "CreateJob",
            &create_body("created-by-this-row", &capacity),
            &below_the_floor(worker_type),
        )
        .await;
        let (status, answer) = call(
            &router,
            "GetJob",
            &json!({ "JobName": "created-by-this-row" }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create must store no job definition, got {status} with \
             {answer}"
        );

        assert_refused_with(
            &router,
            case,
            "UpdateJob",
            &update_body("worker-floor", &capacity),
            &below_the_floor(worker_type),
        )
        .await;
        let after = ok(&router, "GetJob", &json!({ "JobName": "worker-floor" })).await;
        assert_eq!(
            after, stored,
            "case {case:?}: a refused update must leave the stored definition untouched; \
             expected {stored}, got {after}"
        );
    }

    // The floor itself and counts above it are served, which is what makes the refusals
    // above refusals of the count and not of the worker type.
    for (index, (case, worker_type, workers)) in [
        ("the floor itself", "G.1X", MIN_WORKERS),
        ("one worker above the floor", "G.025X", MIN_WORKERS + 1),
        ("a Graviton worker type at the floor", "G.2X.GRAVITON", MIN_WORKERS),
        ("many workers", "R.1X", 100),
    ]
    .iter()
    .enumerate()
    {
        let name = format!("floor-{index}");
        let capacity = json!({ "WorkerType": worker_type, "NumberOfWorkers": workers });
        ok(&router, "CreateJob", &create_body(&name, &capacity)).await;
        let read = ok(&router, "GetJob", &json!({ "JobName": &name })).await;
        assert_eq!(
            read["Job"]["NumberOfWorkers"],
            json!(workers),
            "case {case:?}: the stored definition must report the count it was created with, \
             got {read}"
        );
    }
}

/// A worker configuration belongs to a command the service sizes in workers: a definition
/// that gives a Python shell job one is refused with the sentence naming that command and
/// writes nothing, while every other command name carries one.
///
/// The rows differ along the worker type and count the definition names, and — on the other
/// side of the rule — along the command: the two Spark commands, the Ray command, and names
/// the documentation does not list at all.
#[tokio::test]
async fn a_worker_configuration_on_a_command_sized_in_dpus_is_refused() {
    let router = emulator();
    // The sentence names the command the definition asked for, and nothing else.
    let unsupported = |command_name: &str| {
        format!("Worker Type is not supported for Job Command {command_name}")
    };
    let stored_running_workers = json!({ "WorkerType": "G.1X", "NumberOfWorkers": 2 });
    ok(
        &router,
        "CreateJob",
        &create_body("sized-in-workers", &stored_running_workers),
    )
    .await;
    let stored = ok(&router, "GetJob", &json!({ "JobName": "sized-in-workers" })).await;

    // (case, the worker configuration the Python shell definition names)
    let configurations: Vec<(&str, Value)> = vec![
        (
            "a whole worker configuration",
            json!({ "WorkerType": "G.1X", "NumberOfWorkers": 2 }),
        ),
        (
            "a worker type the model does not declare",
            json!({ "WorkerType": "R.2X", "NumberOfWorkers": 4 }),
        ),
        (
            "the worker type a Ray job takes",
            json!({ "WorkerType": "Z.2X", "NumberOfWorkers": 2 }),
        ),
    ];

    for (case, capacity) in &configurations {
        assert_refused_with(
            &router,
            case,
            "CreateJob",
            &create_body_running("created-by-this-row", "pythonshell", capacity),
            &unsupported("pythonshell"),
        )
        .await;
        let (status, answer) = call(
            &router,
            "GetJob",
            &json!({ "JobName": "created-by-this-row" }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create must store no job definition, got {status} with \
             {answer}"
        );

        assert_refused_with(
            &router,
            case,
            "UpdateJob",
            &update_body_running("sized-in-workers", "pythonshell", capacity),
            &unsupported("pythonshell"),
        )
        .await;
        let after = ok(&router, "GetJob", &json!({ "JobName": "sized-in-workers" })).await;
        assert_eq!(
            after, stored,
            "case {case:?}: a refused update must leave the stored definition untouched; \
             expected {stored}, got {after}"
        );
    }

    // Every other command carries a worker configuration, the documented names and the
    // names the documentation does not list alike.
    for (index, command_name) in ["glueetl", "gluestreaming", "glueray", "spark", "sparkmagic"]
        .iter()
        .enumerate()
    {
        let name = format!("worker-command-{index}");
        ok(
            &router,
            "CreateJob",
            &create_body_running(&name, command_name, &stored_running_workers),
        )
        .await;
        let read = ok(&router, "GetJob", &json!({ "JobName": &name })).await;
        assert_eq!(
            read["Job"]["WorkerType"],
            json!("G.1X"),
            "case {command_name:?}: the stored definition must report the worker type it was \
             created with, got {read}"
        );
    }

    // And a Python shell job sized the way the service documents for it — a DPU figure — is
    // served, so what the refusals above are about is the worker configuration.
    ok(
        &router,
        "CreateJob",
        &create_body_running("sized-in-dpus", "pythonshell", &json!({ "MaxCapacity": 1.0 })),
    )
    .await;
}
