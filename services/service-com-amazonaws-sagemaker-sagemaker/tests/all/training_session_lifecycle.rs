// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for SageMaker's training sessions: `CreateTrainingSession` and
//! the `DescribeTrainingSession` that reads one back.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so
//! what these tests exercise is what a real SDK client gets.
//!
//! The tests assert RULES over a TABLE of inputs rather than one recorded call: the
//! create/describe round trip for every optional member present and absent and for both
//! published LoRA bounds, the ARN form for every signed region and name shape, the
//! already-exists and not-found refusals, the unresolvable model-package-group reference,
//! the constraint refusals that must not reach the store, and the `Created` -> `Running`
//! transition the clock owes.

use std::time::Duration;

use serde_json::{json, Value};

// The crate's ONE wire harness, DEFINED and exported by
// `tests/all/inference_deployment_lifecycle.rs` and consumed here: the same
// `MultiServiceRouter` the production router assembles, the signed call paths, the
// error-code and message readers and the account id every emulator ARN carries.
use super::inference_deployment_lifecycle::{error_code, message, Emulator, ACCOUNT};

/// The two `TrainingSessionStatus` members a created session passes through, from
/// `smithy_query(query="shape",shape="TrainingSessionStatus")`.
const STATUS_CREATED: &str = "Created";
/// See [`STATUS_CREATED`].
const STATUS_RUNNING: &str = "Running";

/// How long a poll for the clock-owed `Created` -> `Running` transition keeps looking
/// before it fails. Far above the span itself (3 s), so a loaded or slow host still
/// passes: the test waits for the CONDITION, not for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll loop waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A role ARN inside `RoleArn`'s modelled pattern
/// (`^arn:aws[a-z\-]*:iam::\d{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$`).
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/training-session")
}

/// A base model ARN inside `TrainingSessionModelConfig$BaseModelArn`'s modelled pattern,
/// which admits a hub content of this account or of the `aws` public hub.
const BASE_MODEL_ARN: &str =
    "arn:aws:sagemaker:us-east-1:aws:hub-content/SageMakerPublicHub/Model/llama-3-8b/1.0.0";

/// A KMS key ARN inside `TrainingSessionS3Location$KmsKeyArn`'s modelled pattern
/// (`^arn:aws[a-z\-]*:kms:[a-z0-9\-]+:[0-9]{12}:key/(mrk-)?[a-f0-9\-]+$`).
fn kms_key_arn() -> String {
    format!("arn:aws:kms:us-east-1:{ACCOUNT}:key/1234abcd-12ab-34cd-56ef-1234567890ab")
}

/// The `S3Location` every row shares.
const OUTPUT_PATH: &str = "s3://session-outputs/run";

/// A create request for `name` with the required members, plus whatever `extra` names.
fn create_request(name: &str, extra: Value) -> Value {
    let mut body = json!({
        "TrainingSessionName": name,
        "LoraConfig": { "Rank": 8, "Alpha": 16 },
        "RoleArn": role_arn(),
        "ModelConfig": { "BaseModelArn": BASE_MODEL_ARN, "AcceptEula": true },
        "OutputConfig": { "S3Location": { "Path": OUTPUT_PATH } },
    });
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        body[member] = value;
    }
    body
}

/// The `arn:aws:sagemaker:<region>:<account>:training-session/<name>` a create mints, as
/// `TrainingSessionArn`'s modelled pattern spells it.
fn session_arn(region: &str, name: &str) -> String {
    format!("arn:aws:sagemaker:{region}:{ACCOUNT}:training-session/{name}")
}

/// A model-package-group ARN of this account in `region`, as `CreateModelPackageGroup`
/// mints one.
fn group_arn(region: &str, name: &str) -> String {
    format!("arn:aws:sagemaker:{region}:{ACCOUNT}:model-package-group/{name}")
}

/// A member of a response body, with JSON `null` read as the absence it stands for.
fn field(body: &Value, member: &str) -> Value {
    match body.get(member) {
        None | Some(Value::Null) => Value::Null,
        Some(value) => value.clone(),
    }
}

/// The `TrainingSessionDetail` a `DescribeTrainingSession` answer carries.
fn detail(body: &Value) -> Value {
    field(body, "TrainingSessionDetail")
}

/// Describe `name` in `region`, requiring a 200, and answer the detail it reports.
async fn describe(emulator: &Emulator, region: &str, name: &str) -> Value {
    let (status, parsed) = emulator
        .call_in(
            region,
            "DescribeTrainingSession",
            &json!({ "TrainingSessionName": name }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {name}: expected HTTP 200 from the describe, got {status} with {parsed}"
    );
    detail(&parsed)
}

/// The detail members this emulator never fills, each for a reason the handler states: a
/// session that is provisioning or running has not ended and nothing failed it, and the
/// sequence numbers and token counts are the runtime data plane's own records.
const ALWAYS_ABSENT: [&str; 4] = [
    "EndTime",
    "ErrorDetails",
    "LastReceivedSequenceNumber",
    "BillableTokenUsage",
];

/// Describe `name` until its status is `want`, or fail once [`SETTLE_DEADLINE`] passes.
async fn poll_until_status(emulator: &Emulator, name: &str, want: &str) -> Value {
    let started = std::time::Instant::now();
    loop {
        let reported = describe(emulator, "us-east-1", name).await;
        if field(&reported, "TrainingSessionStatus") == json!(want) {
            return reported;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {name}: expected TrainingSessionStatus {want} before the deadline, \
             got {reported}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a training session reads back exactly what its create stored — every member the
/// request carried, the empty tag list for one that carried none, and nothing for the
/// members no call has produced a value for.
#[tokio::test]
async fn describe_replays_what_create_stored() {
    let emulator = Emulator::new();
    // The group a row references has to exist for the reference to resolve, so it is
    // created through the operation that mints one.
    emulator
        .ok(
            "CreateModelPackageGroup",
            &json!({ "ModelPackageGroupName": "checkpoints" }),
        )
        .await;

    // `TrainingSessionName` is `len 1..63, pattern ^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}$`, so
    // the rows span a single character, a hyphenated name and the published upper bound.
    // `LoraConfig$Rank` is `range 1..256` and `$Alpha` is `range 1..`
    // (smithy_query(query="shape",shape="LoraConfig")), so the rows also carry both
    // published rank bounds and the least legal alpha. Each row varies the region it is
    // signed in and the optional members it carries.
    let longest_name = format!("a{}", "b".repeat(62));
    let vpc = json!({
        "SecurityGroupIds": ["sg-0123456789abcdef0"],
        "Subnets": ["subnet-0123456789abcdef0", "subnet-0fedcba9876543210"],
    });
    let tags = json!([
        { "Key": "team", "Value": "research" },
        { "Key": "stage", "Value": "dev" },
    ]);
    let rows: Vec<(&str, &str, String, Value)> = vec![
        ("only-the-required-members", "us-east-1", "minimal".to_string(), json!({})),
        (
            "every-optional-member",
            "us-east-1",
            "complete".to_string(),
            json!({
                "ModelPackageGroupArn": group_arn("us-east-1", "checkpoints"),
                "LoraConfig": { "Rank": 1, "Alpha": 1 },
                "VpcConfig": vpc,
                "ModelConfig": { "BaseModelArn": BASE_MODEL_ARN, "AcceptEula": false },
                "OutputConfig": {
                    "S3Location": { "Path": OUTPUT_PATH, "KmsKeyArn": kms_key_arn() },
                },
                "Tags": tags,
            }),
        ),
        (
            "shortest-legal-name-in-another-region",
            "eu-west-2",
            "a".to_string(),
            json!({ "LoraConfig": { "Rank": 256, "Alpha": 512 } }),
        ),
        (
            "longest-legal-name",
            "ap-southeast-2",
            longest_name,
            json!({ "Tags": [] }),
        ),
    ];

    for (case, region, name, extra) in &rows {
        let request = create_request(name, extra.clone());
        let (status, created) = emulator
            .call_in(region, "CreateTrainingSession", &request)
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200, got {status} with {created}"
        );
        // The ARN carries the region the caller signed for and the session's own name.
        assert_eq!(
            field(&created, "TrainingSessionArn"),
            json!(session_arn(region, name)),
            "case {case}: TrainingSessionArn, got {created}"
        );

        let reported = describe(&emulator, region, name).await;
        assert_eq!(
            field(&reported, "TrainingSessionArn"),
            json!(session_arn(region, name)),
            "case {case}: the described ARN must be the one the create answered, \
             got {reported}"
        );
        assert_eq!(
            field(&reported, "TrainingSessionName"),
            json!(name),
            "case {case}: TrainingSessionName, got {reported}"
        );
        // Every member the request carried reads back as it was sent, and the three the
        // request may leave out read back as the session holds them: no group reference,
        // no VPC placement, and the EMPTY tag list — the member is how a session reports
        // what it holds, not what the request mentioned.
        for member in [
            "RoleArn",
            "LoraConfig",
            "ModelConfig",
            "OutputConfig",
            "ModelPackageGroupArn",
            "VpcConfig",
            "Tags",
        ] {
            let want = match request.get(member) {
                Some(sent) => sent.clone(),
                None if member == "Tags" => json!([]),
                None => Value::Null,
            };
            assert_eq!(
                field(&reported, member),
                want,
                "case {case}: {member}, got {reported}"
            );
        }
        for member in ALWAYS_ABSENT {
            assert_eq!(
                field(&reported, member),
                Value::Null,
                "case {case}: {member} must be absent, got {reported}"
            );
        }
        // Both timestamps are reported, and a session nothing has modified yet reports
        // the instant it was created as both.
        let created_at = field(&reported, "CreationTime")
            .as_f64()
            .unwrap_or_else(|| panic!("case {case}: CreationTime is a timestamp"));
        let modified_at = field(&reported, "LastModifiedTime")
            .as_f64()
            .unwrap_or_else(|| panic!("case {case}: LastModifiedTime is a timestamp"));
        assert!(
            created_at > 0.0 && modified_at >= created_at,
            "case {case}: expected LastModifiedTime at or after CreationTime, \
             got {modified_at} and {created_at}"
        );
    }

    // Every session is its own record: each row still reads back its own configuration
    // after all the other rows were created.
    for (case, region, name, extra) in &rows {
        let request = create_request(name, extra.clone());
        let reported = describe(&emulator, region, name).await;
        assert_eq!(
            field(&reported, "LoraConfig"),
            request["LoraConfig"],
            "case {case}: LoraConfig must be unchanged by the sibling creates, \
             got {reported}"
        );
        assert_eq!(
            field(&reported, "OutputConfig"),
            request["OutputConfig"],
            "case {case}: OutputConfig must be unchanged by the sibling creates, \
             got {reported}"
        );
    }
}

/// RULE: a session stays in the non-terminal status its create left it in until the clock
/// owes the next one, and a read moves it neither earlier nor later than that.
#[tokio::test]
async fn the_clock_moves_a_created_session_to_running() {
    let emulator = Emulator::new();
    emulator
        .ok("CreateTrainingSession", &create_request("waiter", json!({})))
        .await;

    // The span is never zero, so a caller that describes the session straight after
    // creating it still observes the transient status.
    let creating = describe(&emulator, "us-east-1", "waiter").await;
    assert_eq!(
        field(&creating, "TrainingSessionStatus"),
        json!(STATUS_CREATED),
        "case immediately-after-create: TrainingSessionStatus, got {creating}"
    );

    let settled = poll_until_status(&emulator, "waiter", STATUS_RUNNING).await;
    let created_at = field(&settled, "CreationTime")
        .as_f64()
        .expect("CreationTime is a timestamp");
    let modified_at = field(&settled, "LastModifiedTime")
        .as_f64()
        .expect("LastModifiedTime is a timestamp");
    assert!(
        modified_at > created_at,
        "case after-the-due-span: the transition must move LastModifiedTime past \
         CreationTime, got {modified_at} and {created_at}"
    );

    // A read does not change what a later caller sees: the status stays where the clock
    // put it, and so does the instant it moved at.
    let again = describe(&emulator, "us-east-1", "waiter").await;
    assert_eq!(
        field(&again, "TrainingSessionStatus"),
        json!(STATUS_RUNNING),
        "case read-again: the status must stay where the clock put it, got {again}"
    );
    assert_eq!(
        field(&again, "LastModifiedTime"),
        field(&settled, "LastModifiedTime"),
        "case read-again: a read must not move LastModifiedTime, got {again}"
    );
}

/// RULE: a create under a name already in use is refused with the modelled
/// `ResourceInUse`, and the stored session stays as the first create left it.
#[tokio::test]
async fn creating_a_name_already_in_use_is_refused_and_keeps_the_stored_session() {
    let emulator = Emulator::new();
    let first = create_request("taken", json!({}));
    emulator.ok("CreateTrainingSession", &first).await;

    // Rows differing along what the second create asks for: the same configuration, and
    // one differing in every member a session carries.
    let rows: Vec<(&str, Value)> = vec![
        ("identical-request", first.clone()),
        (
            "every-member-different",
            create_request(
                "taken",
                json!({
                    "LoraConfig": { "Rank": 32, "Alpha": 64 },
                    "RoleArn": format!("arn:aws:iam::{ACCOUNT}:role/other"),
                    "OutputConfig": {
                        "S3Location": { "Path": OUTPUT_PATH, "KmsKeyArn": kms_key_arn() },
                    },
                    "Tags": [{ "Key": "team", "Value": "other" }],
                }),
            ),
        ),
    ];

    for (case, body) in &rows {
        let (status, parsed) = emulator.call("CreateTrainingSession", body).await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceInUse",
            "case {case}: expected the modelled ResourceInUse, got {parsed}"
        );
        assert_eq!(
            message(&parsed),
            "Training session names must be unique within an Amazon Web Services account \
             and Region, and \"taken\" already exists.",
            "case {case}: the refusal must name the session and the uniqueness rule, \
             got {parsed}"
        );
    }

    // The first create's session is what the store still holds.
    let stored = describe(&emulator, "us-east-1", "taken").await;
    for member in ["LoraConfig", "RoleArn", "OutputConfig", "Tags"] {
        let want = match first.get(member) {
            Some(sent) => sent.clone(),
            None => json!([]),
        };
        assert_eq!(
            field(&stored, member),
            want,
            "a refused create must not overwrite the stored session: {member}, \
             got {stored}"
        );
    }
}

/// RULE: a reader refuses a name no create stored, with the modelled `ResourceNotFound`
/// at HTTP 400 naming the identifier the request carried, and the refusal does not bring
/// the session into being.
#[tokio::test]
async fn describing_an_unknown_training_session_is_refused_with_resource_not_found() {
    let emulator = Emulator::new();
    emulator
        .ok(
            "CreateTrainingSession",
            &create_request("present", json!({})),
        )
        .await;

    // Rows: a name never created, one differing from a stored name only in case, one
    // differing by a single character, and the longest legal name.
    let longest = format!("z{}", "y".repeat(62));
    let rows: Vec<(&str, String)> = vec![
        ("never-created", "absent".to_string()),
        ("case-differs-from-a-stored-name", "Present".to_string()),
        ("one-character-off", "presen".to_string()),
        ("longest-legal-name", longest),
    ];

    for (case, name) in &rows {
        let (status, parsed) = emulator
            .call(
                "DescribeTrainingSession",
                &json!({ "TrainingSessionName": name }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceNotFound",
            "case {case}: expected the modelled ResourceNotFound, got {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!("Could not find training session \"{name}\"."),
            "case {case}: the refusal must quote the name the request carried, got {parsed}"
        );
    }

    // The stored session is untouched by every refusal above.
    let described = describe(&emulator, "us-east-1", "present").await;
    assert_eq!(
        field(&described, "TrainingSessionName"),
        json!("present"),
        "a refused read must leave the stored session readable, got {described}"
    );
}

/// RULE: a create whose `ModelPackageGroupArn` names a group of this account that the
/// account does not hold is refused with the modelled `ResourceNotFound`; an ARN this
/// service did not mint names no record here and is left alone.
#[tokio::test]
async fn creating_against_an_unresolvable_model_package_group_is_refused() {
    let emulator = Emulator::new();
    emulator
        .ok(
            "CreateModelPackageGroup",
            &json!({ "ModelPackageGroupName": "held" }),
        )
        .await;

    // Rows: the group this account holds (served), a group of this account it does not
    // hold (refused), and two ARNs this service did not mint — another account's and
    // another Region's — which name no record here and so are not resolved.
    let rows: Vec<(&str, String, bool)> = vec![
        ("group-held-by-this-account", group_arn("us-east-1", "held"), false),
        (
            "group-this-account-does-not-hold",
            group_arn("us-east-1", "missing"),
            true,
        ),
        (
            "group-of-another-account",
            "arn:aws:sagemaker:us-east-1:999999999999:model-package-group/elsewhere".to_string(),
            false,
        ),
        ("group-of-another-region", group_arn("eu-west-2", "held"), false),
    ];

    for (case, group, refused) in &rows {
        let name = case.replace('-', "");
        let body = create_request(&name, json!({ "ModelPackageGroupArn": group }));
        let (status, parsed) = emulator.call("CreateTrainingSession", &body).await;
        if *refused {
            assert_eq!(
                status, 400,
                "case {case}: expected HTTP 400, got {status} with {parsed}"
            );
            assert_eq!(
                error_code(&parsed),
                "ResourceNotFound",
                "case {case}: expected the modelled ResourceNotFound, got {parsed}"
            );
            assert_eq!(
                message(&parsed),
                format!("ModelPackageGroup {group} does not exist."),
                "case {case}: the refusal must name the group the request referenced, \
                 got {parsed}"
            );
            // A refused create writes nothing.
            let (status, parsed) = emulator
                .call(
                    "DescribeTrainingSession",
                    &json!({ "TrainingSessionName": name }),
                )
                .await;
            assert_eq!(
                status, 400,
                "case {case}: a refused create must write nothing, got {status} with {parsed}"
            );
            assert_eq!(
                error_code(&parsed),
                "ResourceNotFound",
                "case {case}: a refused create must write nothing, got {parsed}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case}: expected HTTP 200, got {status} with {parsed}"
            );
            let described = describe(&emulator, "us-east-1", &name).await;
            assert_eq!(
                field(&described, "ModelPackageGroupArn"),
                json!(group),
                "case {case}: the stored reference must be the one the request sent, \
                 got {described}"
            );
        }
    }
}

/// RULE: a request outside a modelled constraint is refused as a constraint violation —
/// never as a not-implemented operation — and reaches no store.
#[tokio::test]
async fn constraint_violations_are_refused_and_reach_no_store() {
    let emulator = Emulator::new();

    // `LoraConfig$Rank` is `range 1..256` and `$Alpha` is `range 1..`, so the rows carry
    // one value past each published bound, beside a missing required member and a role
    // ARN outside its pattern. The last row is the reader, with a name its own pattern
    // does not admit.
    let mut rows: Vec<(&str, &str, Value)> = vec![
        (
            "rank-below-the-published-minimum",
            "CreateTrainingSession",
            create_request("ranklow", json!({ "LoraConfig": { "Rank": 0, "Alpha": 16 } })),
        ),
        (
            "rank-above-the-published-maximum",
            "CreateTrainingSession",
            create_request("rankhigh", json!({ "LoraConfig": { "Rank": 257, "Alpha": 16 } })),
        ),
        (
            "alpha-below-the-published-minimum",
            "CreateTrainingSession",
            create_request("alphalow", json!({ "LoraConfig": { "Rank": 8, "Alpha": 0 } })),
        ),
        (
            "role-arn-outside-its-pattern",
            "CreateTrainingSession",
            create_request("badrole", json!({ "RoleArn": "not-an-arn" })),
        ),
        (
            "describe-with-an-illegal-name",
            "DescribeTrainingSession",
            json!({ "TrainingSessionName": "not a session name" }),
        ),
    ];
    // A request missing a required member, built by removing one from a complete request.
    let mut without_output = create_request("nooutput", json!({}));
    without_output
        .as_object_mut()
        .expect("the request is a JSON object")
        .remove("OutputConfig");
    rows.push((
        "missing-the-required-output-config",
        "CreateTrainingSession",
        without_output,
    ));

    for (case, target, body) in &rows {
        let (status, parsed) = emulator.call(target, body).await;
        assert_eq!(
            status, 400,
            "case {case}: {target} must be refused, got {status} with {parsed}"
        );
        // The code as well as the status: a constraint refusal is a `ValidationException`,
        // so a row answered by some other 400 (a `ResourceNotFound` for the describe row,
        // which would mean the illegal name reached the store) fails here.
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case}: {target} must be refused as a constraint violation, got {parsed}"
        );
    }

    for name in ["ranklow", "rankhigh", "alphalow", "badrole", "nooutput"] {
        let (status, parsed) = emulator
            .call(
                "DescribeTrainingSession",
                &json!({ "TrainingSessionName": name }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {name}: a refused create must write nothing, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceNotFound",
            "case {name}: a refused create must write nothing, got {parsed}"
        );
    }
}
