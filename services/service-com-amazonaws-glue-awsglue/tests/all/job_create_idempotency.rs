// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `CreateJob`'s idempotency on the job NAME.
//!
//! The rule under test: the `Name` a create submits is that create's idempotency
//! identifier ("It must be unique in your account" — `CreateJob$Name`), so what a repeat
//! create under a name the Region already holds a job under is answered with is decided by
//! the job DEFINITION it submits — the modelled `Job` shape a read of that name reports. A
//! repeat submitting the stored definition is the RETRY of the create that stored it: HTTP
//! 200 with the stored `Name`, and nothing written. Only a repeat submitting a DIFFERENT
//! definition has associated one identifier with two records, which is the operation's
//! modelled `IdempotentParameterMismatchException`: HTTP 400 with its fixed sentence, and
//! still nothing written, so the name's idempotency survives the refusal. `Tags` are no
//! part of that definition — they are the separately addressed surface `TagResource`,
//! `UntagResource` and `GetTags` serve — so a repeat sending other tags, or none at all,
//! is still the retry.
//!
//! Each suite is a table whose rows differ along what the rule generalises over: how a
//! repeat states the SAME definition (identically, with the tags dropped, emptied or
//! replaced, with its maps written in another order, and with every member the service
//! filled in spelled out), and WHICH member of the definition a differing repeat changes
//! (its role, its command and a member nested in it, its capacity stated either way, its
//! timeout, its retry budget, its concurrency, its arguments, its connections, its Glue
//! version, its mode and a member it adds).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::resource_tagging::{read_tags, resource_arn};
use crate::wire::{call, emulator, error_code, error_message, ok};

/// A role ARN a job definition can carry.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/sim2real-glue-job-role";

/// The sentence the idempotency mismatch carries, written out here as a literal rather
/// than read from the service's own constant, so the suite pins the wording instead of
/// restating it. It is entirely fixed text: it interpolates no name, value or limit.
const MISMATCH_MESSAGE: &str = "Job already submitted with different configuration.";

/// The definition the first create stores: a Python shell job whose capacity is stated as
/// a DPU figure, carrying a map-valued member and a tag set a row can change.
fn python_shell_job(name: &str) -> Value {
    json!({
        "Name": name,
        "Role": ROLE_ARN,
        "Command": {
            "Name": "pythonshell",
            "PythonVersion": "3.9",
            "ScriptLocation": "s3://curated-etl/scripts/transform.py",
        },
        "MaxCapacity": 0.0625,
        "Timeout": 10,
        "DefaultArguments": { "--job-language": "python", "--enable-metrics": "true" },
        "Tags": { "owner": "data-platform", "env": "test" },
    })
}

/// A second definition shape: a Spark job whose capacity is a WORKER CONFIGURATION, which
/// the service maps to a `MaxCapacity` of its own — so the comparison is over members the
/// request did not state as well as the ones it did.
fn spark_job(name: &str) -> Value {
    json!({
        "Name": name,
        "Role": ROLE_ARN,
        "Command": { "Name": "glueetl", "ScriptLocation": "s3://curated-etl/scripts/spark.py" },
        "GlueVersion": "4.0",
        "WorkerType": "G.1X",
        "NumberOfWorkers": 2,
        "Timeout": 120,
    })
}

/// `base` with every member of `patch` replaced, and every member `patch` maps to null
/// REMOVED — so one row can add a member, change one, or drop one.
fn patched(base: &Value, patch: &Value) -> Value {
    let mut body = base.clone();
    let members = body.as_object_mut().expect("the request is a structure");
    for (member, value) in patch.as_object().expect("the patch is a structure") {
        if value.is_null() {
            members.remove(member);
        } else {
            members.insert(member.clone(), value.clone());
        }
    }
    body
}

/// The `Job` a read of `name` reports — the stored definition, which is what a repeat
/// create is compared against and what a retry must leave alone.
async fn stored_job(router: &MultiServiceRouter, name: &str) -> Value {
    let read = ok(router, "GetJob", &json!({ "JobName": name })).await;
    read.get("Job")
        .cloned()
        .unwrap_or_else(|| panic!("GetJob must report a Job for {name}, got {read}"))
}

/// A repeat create that submits the stored job definition is the retry of the create that
/// stored it: 200 with the stored `Name`, and the stored definition — its tag set
/// included — left exactly as the first create wrote it.
///
/// The rows differ along how the repeat states that same definition: identically, with the
/// tags dropped, emptied or replaced (no part of the definition), and with its maps written
/// in another order (a map is a set of entries).
#[tokio::test]
async fn a_repeat_create_job_submitting_the_stored_definition_answers_the_stored_name() {
    let router = emulator();

    let rows: Vec<(&str, Value)> = vec![
        ("the identical definition", json!({})),
        ("no tags at all", json!({ "Tags": Value::Null })),
        ("an empty tag map", json!({ "Tags": {} })),
        (
            "other tags than the stored job carries",
            json!({ "Tags": { "owner": "someone-else" } }),
        ),
        (
            "its maps written in another order",
            json!({
                "DefaultArguments": { "--enable-metrics": "true", "--job-language": "python" },
                "Tags": { "env": "test", "owner": "data-platform" },
            }),
        ),
    ];

    for (index, (case, patch)) in rows.iter().enumerate() {
        let name = format!("retried-job-{index}");
        ok(&router, "CreateJob", &python_shell_job(&name)).await;
        let before = stored_job(&router, &name).await;
        let arn = resource_arn("job", &name);
        let tags_before = read_tags(&router, case, &arn).await;

        let repeat = patched(&python_shell_job(&name), patch);
        let (status, answer) = call(&router, "CreateJob", &repeat).await;
        assert_eq!(
            status, 200,
            "case {case}: a repeat submitting the stored definition must answer 200, got \
             {status} with {answer}"
        );
        assert_eq!(
            answer.get("Name").and_then(Value::as_str),
            Some(name.as_str()),
            "case {case}: the answer must name the stored job, got {answer}"
        );

        let after = stored_job(&router, &name).await;
        assert_eq!(
            after, before,
            "case {case}: the retry must write nothing; expected {before}, got {after}"
        );
        let tags_after = read_tags(&router, case, &arn).await;
        assert_eq!(
            tags_after, tags_before,
            "case {case}: the retry must leave the tag set as it was; expected {tags_before:?}, \
             got {tags_after:?}"
        );
    }
}

/// The retry stays a retry however often it is repeated, and whichever of the definition's
/// members it spells out: the members the SERVICE filled in are part of the definition, so
/// a client that read the job back and resubmitted what the read reports is still retrying.
#[tokio::test]
async fn a_retry_is_still_the_retry_when_repeated_and_when_it_spells_the_defaults_out() {
    let router = emulator();
    let name = "spelled-out-retry";
    ok(&router, "CreateJob", &python_shell_job(name)).await;
    let before = stored_job(&router, name).await;

    // Every member below is read off the stored definition rather than written out here, so
    // the case states no default of its own.
    let spelled_out = json!({
        "Name": name,
        "Role": before["Role"],
        "Command": before["Command"],
        "JobMode": before["JobMode"],
        "JobRunQueuingEnabled": before["JobRunQueuingEnabled"],
        "GlueVersion": before["GlueVersion"],
        "ExecutionProperty": before["ExecutionProperty"],
        "MaxRetries": before["MaxRetries"],
        "MaxCapacity": before["MaxCapacity"],
        "Timeout": before["Timeout"],
        "DefaultArguments": before["DefaultArguments"],
    });

    let repeats: Vec<(&str, Value)> = vec![
        ("the first retry", python_shell_job(name)),
        ("a second retry", python_shell_job(name)),
        ("the definition as the read reports it", spelled_out),
        (
            "a further retry after the spelled-out one",
            python_shell_job(name),
        ),
    ];

    for (case, repeat) in repeats {
        let (status, answer) = call(&router, "CreateJob", &repeat).await;
        assert_eq!(
            status, 200,
            "case {case}: a repeat submitting the stored definition must answer 200, got \
             {status} with {answer}"
        );
        assert_eq!(
            answer.get("Name").and_then(Value::as_str),
            Some(name),
            "case {case}: the answer must name the stored job, got {answer}"
        );
        let after = stored_job(&router, name).await;
        assert_eq!(
            after, before,
            "case {case}: the retry must write nothing; expected {before}, got {after}"
        );
    }
}

/// A repeat create that submits ANOTHER definition under a stored name is the idempotency
/// mismatch: 400 `IdempotentParameterMismatchException` with its fixed sentence. It writes
/// nothing, and it does not spend the name's idempotency either — the identical definition
/// is still answered with 200 afterwards.
///
/// The rows differ along which member of the definition the repeat changes: a required
/// member, a member nested inside the command, an optional member it adds, the capacity,
/// the timeout, the retry budget, the concurrency, a map, a list and the job's mode.
#[tokio::test]
async fn a_repeat_create_job_submitting_another_definition_is_the_idempotency_mismatch() {
    let router = emulator();

    let rows: Vec<(&str, Value)> = vec![
        (
            "another role",
            json!({ "Role": "arn:aws:iam::000000000000:role/other-glue-job-role" }),
        ),
        (
            "another script location",
            json!({
                "Command": {
                    "Name": "pythonshell",
                    "PythonVersion": "3.9",
                    "ScriptLocation": "s3://curated-etl/scripts/other.py",
                },
            }),
        ),
        (
            "another command kind",
            json!({
                "Command": {
                    "Name": "glueetl",
                    "PythonVersion": "3.9",
                    "ScriptLocation": "s3://curated-etl/scripts/transform.py",
                },
            }),
        ),
        (
            "another Python version nested in the command",
            json!({
                "Command": {
                    "Name": "pythonshell",
                    "PythonVersion": "3",
                    "ScriptLocation": "s3://curated-etl/scripts/transform.py",
                },
            }),
        ),
        (
            "a description the stored definition has not",
            json!({ "Description": "the second submission" }),
        ),
        ("another timeout", json!({ "Timeout": 20 })),
        ("another retry budget", json!({ "MaxRetries": 2 })),
        ("another capacity", json!({ "MaxCapacity": 1.0 })),
        (
            "another concurrency",
            json!({ "ExecutionProperty": { "MaxConcurrentRuns": 3 } }),
        ),
        (
            "another default argument value",
            json!({ "DefaultArguments": { "--job-language": "python", "--enable-metrics": "false" } }),
        ),
        (
            "one default argument fewer",
            json!({ "DefaultArguments": { "--job-language": "python" } }),
        ),
        (
            "a connection list the stored definition has not",
            json!({ "Connections": { "Connections": ["warehouse"] } }),
        ),
        ("another Glue version", json!({ "GlueVersion": "4.0" })),
        ("another job mode", json!({ "JobMode": "NOTEBOOK" })),
    ];

    for (index, (case, patch)) in rows.iter().enumerate() {
        let name = format!("mismatched-job-{index}");
        ok(&router, "CreateJob", &python_shell_job(&name)).await;
        let before = stored_job(&router, &name).await;

        let repeat = patched(&python_shell_job(&name), patch);
        let (status, body) = call(&router, "CreateJob", &repeat).await;
        assert_eq!(
            status, 400,
            "case {case}: a repeat submitting another definition must be refused with 400, got \
             {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("IdempotentParameterMismatchException"),
            "case {case}: expected IdempotentParameterMismatchException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            MISMATCH_MESSAGE,
            "case {case}: expected Message {MISMATCH_MESSAGE:?}, got {body}"
        );

        let after = stored_job(&router, &name).await;
        assert_eq!(
            after, before,
            "case {case}: the refused repeat must write nothing; expected {before}, got {after}"
        );

        // The refusal did not spend the name's idempotency: the stored definition is still
        // the retry.
        let (status, answer) = call(&router, "CreateJob", &python_shell_job(&name)).await;
        assert_eq!(
            status, 200,
            "case {case}: after the refusal the stored definition must still answer 200, got \
             {status} with {answer}"
        );
        assert_eq!(
            answer.get("Name").and_then(Value::as_str),
            Some(name.as_str()),
            "case {case}: the answer must name the stored job, got {answer}"
        );
        let after = stored_job(&router, &name).await;
        assert_eq!(
            after, before,
            "case {case}: the later retry must write nothing either; expected {before}, got \
             {after}"
        );
    }
}

/// The capacity the SERVICE derives from a worker configuration is part of the definition a
/// repeat is compared against: the same configuration repeated is the retry, and another
/// number of workers — which maps to another `MaxCapacity` — is the mismatch.
#[tokio::test]
async fn the_derived_capacity_is_part_of_the_definition_a_repeat_is_compared_against() {
    let router = emulator();
    let name = "worker-sized-job";
    ok(&router, "CreateJob", &spark_job(name)).await;
    let before = stored_job(&router, name).await;

    let (status, answer) = call(&router, "CreateJob", &spark_job(name)).await;
    assert_eq!(
        status, 200,
        "case the same worker configuration: expected 200, got {status} with {answer}"
    );
    assert_eq!(
        answer.get("Name").and_then(Value::as_str),
        Some(name),
        "case the same worker configuration: the answer must name the stored job, got {answer}"
    );
    assert_eq!(
        stored_job(&router, name).await,
        before,
        "case the same worker configuration: the retry must write nothing; expected {before}"
    );

    let (status, body) = call(
        &router,
        "CreateJob",
        &patched(&spark_job(name), &json!({ "NumberOfWorkers": 4 })),
    )
    .await;
    assert_eq!(
        status, 400,
        "case another number of workers: expected 400, got {status} with {body}"
    );
    assert!(
        error_code(&body).ends_with("IdempotentParameterMismatchException"),
        "case another number of workers: expected IdempotentParameterMismatchException, got {} \
         in {body}",
        error_code(&body)
    );
    assert_eq!(
        error_message(&body),
        MISMATCH_MESSAGE,
        "case another number of workers: expected Message {MISMATCH_MESSAGE:?}, got {body}"
    );
    assert_eq!(
        stored_job(&router, name).await,
        before,
        "case another number of workers: the refusal must write nothing; expected {before}"
    );
}
