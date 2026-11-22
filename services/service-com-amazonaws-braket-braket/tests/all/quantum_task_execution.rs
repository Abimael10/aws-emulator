// SPDX-License-Identifier: CC0-1.0

//! A quantum task is EXECUTED: its program runs, its result document is written where a
//! client reads it, and the task reports what that run produced.
//!
//! The rule these suites pin: `CreateQuantumTask` on an online gate-model device runs the
//! program its `action` carries and writes the run's result document under the task's
//! `outputS3Directory` in the caller's bucket, so `GetQuantumTask` then reports the task
//! `COMPLETED` with one measurement per shot readable from Amazon S3; a request the
//! service cannot serve — an output bucket S3 holds no bucket for, an `action` that is not
//! a Braket IR document, a `shots` value outside the device's published range — is
//! refused with `ValidationException` (400) before any task is minted; and a task that
//! has reached a terminal status can no longer be cancelled (`ConflictException`, 409).
//!
//! Each suite asserts that over a TABLE spanning what the rule generalises over: the
//! circuit (its gates, its qubit count, which qubits it measures), the shot count, and
//! the member of the request that is out of range. The recorded Bell circuit at 100 shots
//! is ONE row of the first table.
//!
//! The last two suites pin the OBJECT and the metadata the document it holds reports:
//! Amazon S3 serves a stored object as the write that stored it declared it, so a read of
//! a result object reports the store's own default media type (the write declares none)
//! and the CRC32 whole-object checksum that write carries; and the document's
//! `taskMetadata` reports the task as it was submitted, including the device-parameters
//! document the task ran under.
//!
//! Everything goes over HTTP through the production router, with Amazon S3 — the owner of
//! the output location — mounted beside this service on the same engine, so a result
//! object exists here only because S3 answered for it.

use std::time::Duration;

use serde_json::{json, Value};

use crate::quantum_task_loop::{member, Braket, Reply};

/// The bucket the suites write their results to, named as Amazon Braket's own
/// documentation names an output bucket.
const BUCKET: &str = "amazon-braket-emulator-results";

/// The key prefix every create in these suites asks for.
const PREFIX: &str = "results";

/// The managed state-vector simulator of the seeded inventory — the device the recorded
/// run used, and the one these suites submit to.
const SV1: &str = "arn:aws:braket:::device/quantum-simulator/amazon/sv1";

/// The tensor-network simulator, whose published `shotsRange` starts at 1 rather than 0:
/// the device a shot count BELOW the range is asserted against.
const TN1: &str = "arn:aws:braket:::device/quantum-simulator/amazon/tn1";

/// The result object a finished task's results are read from, under its
/// `outputS3Directory`.
const RESULTS_OBJECT: &str = "results.json";

/// The `braketSchemaHeader` name of a gate-model result document.
const RESULT_SCHEMA: &str = "braket.task_result.gate_model_task_result";

/// An OpenQASM program action document, as a client sends it.
fn openqasm_action(source: &str) -> String {
    json!({
        "braketSchemaHeader": { "name": "braket.ir.openqasm.program", "version": "1" },
        "source": source,
    })
    .to_string()
}

/// The body of a `CreateQuantumTask` call, with every required member filled in.
fn create_body(
    client_token: &str,
    device_arn: &str,
    shots: i64,
    bucket: &str,
    action: &str,
) -> Value {
    json!({
        "clientToken": client_token,
        "deviceArn": device_arn,
        "shots": shots,
        "outputS3Bucket": bucket,
        "outputS3KeyPrefix": PREFIX,
        "action": action,
    })
}

/// Create the suite's bucket through Amazon S3's own API.
///
/// The harness signs for a Region outside `us-east-1`, which S3 serves a create for only
/// with the matching location constraint.
async fn create_bucket(s3: &::aws_sdk_s3::Client) {
    s3.create_bucket()
        .bucket(BUCKET)
        .create_bucket_configuration(
            ::aws_sdk_s3::types::CreateBucketConfiguration::builder()
                .location_constraint(::aws_sdk_s3::types::BucketLocationConstraint::from(
                    crate::quantum_task_loop::REGION,
                ))
                .build(),
        )
        .send()
        .await
        .unwrap_or_else(|error| panic!("the bucket {BUCKET} must be created, got {error:?}"));
}

/// The body of one object, or `None` when the object store serves no such object.
async fn object_body(s3: &::aws_sdk_s3::Client, key: &str) -> Option<String> {
    let object = s3.get_object().bucket(BUCKET).key(key).send().await.ok()?;
    let bytes = object.body.collect().await.ok()?.into_bytes();
    String::from_utf8(bytes.to_vec()).ok()
}

/// Poll `GetQuantumTask` until the task reaches a terminal status, and answer what it
/// then reports.
///
/// The terminal transition is owed by the clock, so the poll waits it out rather than
/// sleeping for a fixed span; a task that never gets there fails the suite instead of
/// hanging it.
async fn poll_until_terminal(braket: &Braket, arn: &str) -> Value {
    for _ in 0..100 {
        let reply = braket.get_task(arn).await;
        assert_eq!(
            reply.status, 200,
            "case poll {arn}: expected 200, got {} {}",
            reply.status, reply.body
        );
        if matches!(
            member(&reply.body, "status"),
            "COMPLETED" | "FAILED" | "CANCELLED"
        ) {
            return reply.body;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("case poll {arn}: the task never reached a terminal status");
}

/// What the measurements of a run must satisfy, which is what each row of the table
/// below differs along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// Every shot reports exactly this row.
    Exactly(&'static [u8]),
    /// Every shot reports a row whose bits all agree, and over the run both the all-zero
    /// and the all-one outcome appear — the entangled case.
    Correlated,
}

/// A run writes one measurement per shot, over the whole class of gate-model circuits:
/// a circuit whose outcome is certain reports it on every shot, and an entangled one
/// reports only the outcomes its state allows.
#[tokio::test]
async fn a_quantum_task_runs_its_program_and_writes_its_measurements() {
    let (braket, s3) = Braket::with_s3();
    create_bucket(&s3).await;

    // (case, source, shots, measured qubits, what the shots must satisfy)
    let cases: Vec<(&str, &str, i64, Vec<u64>, Expected)> = vec![
        (
            // The recorded run: a two-qubit Bell circuit at 100 shots.
            "a Bell circuit at 100 shots",
            "OPENQASM 3.0; bit[2] b; qubit[2] q; h q[0]; cnot q[0], q[1]; \
             b[0] = measure q[0]; b[1] = measure q[1];",
            100,
            vec![0, 1],
            Expected::Correlated,
        ),
        (
            "a one-qubit circuit at one shot",
            "OPENQASM 3.0; qubit[1] q; x q[0];",
            1,
            vec![0],
            Expected::Exactly(&[1]),
        ),
        (
            "a three-qubit GHZ circuit",
            "OPENQASM 3.0; qubit[3] q; h q[0]; cnot q[0], q[1]; cnot q[1], q[2];",
            64,
            vec![0, 1, 2],
            Expected::Correlated,
        ),
        (
            "a circuit of rotations only",
            "OPENQASM 3.0; qubit[2] q; rx(pi) q[0]; ry(2 * pi) q[1];",
            5,
            vec![0, 1],
            Expected::Exactly(&[1, 0]),
        ),
        (
            "a circuit that measures one of its qubits",
            "OPENQASM 3.0; bit b; qubit[2] q; x q[1]; b = measure q[1];",
            7,
            vec![1],
            Expected::Exactly(&[1]),
        ),
    ];

    for (index, (case, source, shots, measured_qubits, expected)) in cases.iter().enumerate() {
        let created = braket
            .create_task(create_body(
                &format!("exec-token-{index}"),
                SV1,
                *shots,
                BUCKET,
                &openqasm_action(source),
            ))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: expected 200, got {} {}",
            created.status, created.body
        );
        let arn = created.str_member("quantumTaskArn");
        let task = poll_until_terminal(&braket, &arn).await;
        assert_eq!(
            member(&task, "status"),
            "COMPLETED",
            "case {case}: expected COMPLETED, got {task}"
        );
        assert!(
            task.get("failureReason").is_none(),
            "case {case}: a completed task carries no failureReason, got {task}"
        );
        assert_eq!(
            task.get("numSuccessfulShots").and_then(Value::as_i64),
            Some(*shots),
            "case {case}: expected {shots} successful shots, got {task}"
        );

        // The results live where the task says they do, in the caller's own bucket.
        let directory = member(&task, "outputS3Directory").to_string();
        assert_eq!(
            member(&task, "outputS3Bucket"),
            BUCKET,
            "case {case}: expected the bucket the create named, got {task}"
        );
        let key = format!("{directory}/{RESULTS_OBJECT}");
        let body = object_body(&s3, &key)
            .await
            .unwrap_or_else(|| panic!("case {case}: expected a result object at {key}"));
        let document: Value = serde_json::from_str(&body)
            .unwrap_or_else(|error| panic!("case {case}: the result object is JSON: {error}"));
        assert_eq!(
            document["braketSchemaHeader"]["name"].as_str(),
            Some(RESULT_SCHEMA),
            "case {case}: expected the gate-model result schema, got {document}"
        );
        assert_eq!(
            document["measuredQubits"],
            json!(measured_qubits),
            "case {case}: expected the measured qubits {measured_qubits:?}, got {}",
            document["measuredQubits"]
        );
        assert_eq!(
            document["taskMetadata"]["id"].as_str(),
            Some(arn.as_str()),
            "case {case}: expected the task's own ARN in its metadata, got {}",
            document["taskMetadata"]
        );
        assert_eq!(
            document["taskMetadata"]["deviceId"].as_str(),
            Some(SV1),
            "case {case}: expected the device that ran it, got {}",
            document["taskMetadata"]
        );
        let rows: Vec<Vec<u64>> = document["measurements"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: measurements is a list, got {document}"))
            .iter()
            .map(|row| {
                row.as_array()
                    .unwrap_or_else(|| panic!("case {case}: a measurement row is a list"))
                    .iter()
                    .map(|bit| {
                        bit.as_u64()
                            .unwrap_or_else(|| panic!("case {case}: a measured bit is a number"))
                    })
                    .collect()
            })
            .collect();
        assert_eq!(
            rows.len(),
            usize::try_from(*shots).expect("a shot count fits a usize"),
            "case {case}: expected one row per shot, got {} rows",
            rows.len()
        );
        for row in &rows {
            assert_eq!(
                row.len(),
                measured_qubits.len(),
                "case {case}: expected one bit per measured qubit, got {row:?}"
            );
            assert!(
                row.iter().all(|bit| *bit <= 1),
                "case {case}: expected measured bits, got {row:?}"
            );
        }
        match expected {
            Expected::Exactly(wanted) => {
                let wanted: Vec<u64> = wanted.iter().map(|bit| u64::from(*bit)).collect();
                for row in &rows {
                    assert_eq!(
                        row, &wanted,
                        "case {case}: expected every shot to report {wanted:?}, got {row:?}"
                    );
                }
            }
            Expected::Correlated => {
                let mut zeros = 0;
                let mut ones = 0;
                for row in &rows {
                    assert!(
                        row.iter().all(|bit| *bit == row[0]),
                        "case {case}: expected correlated bits, got {row:?}"
                    );
                    if row[0] == 0 {
                        zeros += 1;
                    } else {
                        ones += 1;
                    }
                }
                assert!(
                    zeros > 0 && ones > 0,
                    "case {case}: expected both outcomes over {shots} shots, got {zeros} \
                     all-zero and {ones} all-one"
                );
            }
        }
    }
}

/// A program the simulator cannot run ends its task in the modelled failure status
/// carrying the reason — never in `COMPLETED`, and never by taking the emulator down.
///
/// The rows span the ways a program can be unrunnable: text past the length the service
/// reads, nesting past the constants it folds (in brackets and spelled without them), a
/// state larger than the simulator holds, a statement form it does not run, and a gate it
/// has no matrix for.
#[tokio::test]
async fn a_program_the_simulator_cannot_run_fails_its_task_with_the_reason() {
    let (braket, s3) = Braket::with_s3();
    create_bucket(&s3).await;

    // (case, source, a fragment of the reason the task must carry)
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "program text past the length the service reads",
            format!("OPENQASM 3.0; qubit[1] q;{}", " ".repeat(16_384)),
            "more than the",
        ),
        (
            "a prefix-operator chain nested without brackets",
            format!("OPENQASM 3.0; qubit[1] q; rx({}1) q[0];", "-".repeat(512)),
            "not a constant the emulator could evaluate",
        ),
        (
            "a binary-operator chain nested without brackets",
            format!("OPENQASM 3.0; qubit[1] q; rx(1{}) q[0];", "+1".repeat(512)),
            "not a constant the emulator could evaluate",
        ),
        (
            "brackets nested deeper than the constants it folds",
            format!(
                "OPENQASM 3.0; qubit[1] q; rx({}1{}) q[0];",
                "(".repeat(256),
                ")".repeat(256)
            ),
            "not a constant the emulator could evaluate",
        ),
        (
            "a state larger than the simulator holds",
            "OPENQASM 3.0; qubit[40] q; h q[0];".to_string(),
            "qubits this emulator's simulator holds",
        ),
        (
            "a statement form the simulator does not run",
            "OPENQASM 3.0; qubit[1] q; for int i in [0:1] { h q[0]; }".to_string(),
            "does not run the OpenQASM statement",
        ),
        (
            "a gate the simulator has no matrix for",
            "OPENQASM 3.0; qubit[1] q; frobnicate q[0];".to_string(),
            "does not apply the gate",
        ),
    ];

    for (index, (case, source, fragment)) in cases.iter().enumerate() {
        let created = braket
            .create_task(create_body(
                &format!("unrunnable-token-{index}"),
                SV1,
                4,
                BUCKET,
                &openqasm_action(source),
            ))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: the create is accepted, got {} {}",
            created.status, created.body
        );
        let arn = created.str_member("quantumTaskArn");
        let task = poll_until_terminal(&braket, &arn).await;
        assert_eq!(
            member(&task, "status"),
            "FAILED",
            "case {case}: expected the modelled failure status, got {task}"
        );
        assert!(
            member(&task, "failureReason").contains(fragment),
            "case {case}: expected a reason carrying {fragment:?}, got {:?}",
            member(&task, "failureReason")
        );
        assert!(
            task.get("numSuccessfulShots").is_none(),
            "case {case}: a task that produced no result reports no successful shots, \
             got {task}"
        );
    }

    // The emulator is still serving: the control request after the table completes.
    let after = braket
        .create_task(create_body(
            "unrunnable-control",
            SV1,
            4,
            BUCKET,
            &openqasm_action("OPENQASM 3.0; qubit[1] q; x q[0];"),
        ))
        .await;
    assert_eq!(
        after.status, 200,
        "case the control after the table: expected 200, got {} {}",
        after.status, after.body
    );
    let task = poll_until_terminal(&braket, &after.str_member("quantumTaskArn")).await;
    assert_eq!(
        member(&task, "status"),
        "COMPLETED",
        "case the control after the table: expected COMPLETED, got {task}"
    );
}

/// A request the service cannot serve is refused with `ValidationException` (400) and the
/// message of the member that failed, and NO task is minted for it.
#[tokio::test]
async fn a_create_the_service_cannot_serve_is_refused_before_a_task_is_minted() {
    let (braket, s3) = Braket::with_s3();
    create_bucket(&s3).await;
    let bell = openqasm_action(
        "OPENQASM 3.0; bit[2] b; qubit[2] q; h q[0]; cnot q[0], q[1]; \
         b[0] = measure q[0]; b[1] = measure q[1];",
    );

    // (case, device, shots, bucket, action, the message the refusal carries)
    let cases: Vec<(&str, &str, i64, &str, String, String)> = vec![
        (
            // The recorded request: a syntactically valid bucket nobody created.
            "an output bucket Amazon S3 holds no bucket for",
            SV1,
            100,
            "amazon-braket-never-created",
            bell.clone(),
            "Caller doesn't have access to bucket or it doesn't exist. Learn more on our \
             developer guide: \
             https://docs.aws.amazon.com/braket/latest/developerguide/braket-slr.html"
                .to_string(),
        ),
        (
            "an action that is not a Braket IR document",
            SV1,
            100,
            BUCKET,
            json!({ "foo": "bar" }).to_string(),
            "Provided action is not valid. Please refer to the github repo \
             amazon-braket-schemas-python for examples"
                .to_string(),
        ),
        (
            "an action document carrying no schema header",
            SV1,
            100,
            BUCKET,
            json!({ "source": "OPENQASM 3.0; qubit[1] q;" }).to_string(),
            "Provided action is not valid. Please refer to the github repo \
             amazon-braket-schemas-python for examples"
                .to_string(),
        ),
        (
            "a shot count above the device's published maximum",
            SV1,
            100_001,
            BUCKET,
            bell.clone(),
            format!("Shot count should be between 0 and 100000 on {SV1}, but found 100001."),
        ),
        (
            "a shot count below the device's published minimum",
            TN1,
            0,
            BUCKET,
            bell.clone(),
            format!("Shot count should be between 1 and 1000 on {TN1}, but found 0."),
        ),
    ];

    for (index, (case, device, shots, bucket, action, expected)) in cases.iter().enumerate() {
        let reply = braket
            .create_task(create_body(
                &format!("refused-token-{index}"),
                device,
                *shots,
                bucket,
                action,
            ))
            .await;
        assert_eq!(
            reply.status, 400,
            "case {case}: expected 400, got {} {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ValidationException"),
            "case {case}: expected ValidationException, got {:?}",
            reply.error
        );
        assert_eq!(
            reply.message(),
            expected,
            "case {case}: expected the message {expected:?}, got {:?}",
            reply.message()
        );
        assert!(
            reply.body.get("quantumTaskArn").is_none(),
            "case {case}: a refused create mints no task, got {}",
            reply.body
        );
    }

    // The positive control: the same request with every member in range is accepted.
    let accepted = braket
        .create_task(create_body("accepted-token", SV1, 10, BUCKET, &bell))
        .await;
    assert_eq!(
        accepted.status, 200,
        "case the control request: expected 200, got {} {}",
        accepted.status, accepted.body
    );
}

/// What may be cancelled depends on the status the task is in: a task still waiting is
/// accepted for cancellation, and one that has reached a terminal status is refused with
/// `ConflictException` (409) naming that status and the device that ran it.
#[tokio::test]
async fn a_task_in_a_terminal_status_can_no_longer_be_cancelled() {
    let (braket, s3) = Braket::with_s3();
    create_bucket(&s3).await;
    let action = openqasm_action("OPENQASM 3.0; qubit[2] q; h q[0]; cnot q[0], q[1];");

    // A task that has just been created is still cancellable: the positive control.
    let pending = braket
        .create_task(create_body("cancel-pending", SV1, 10, BUCKET, &action))
        .await;
    let pending_arn = pending.str_member("quantumTaskArn");
    let cancelled = braket.cancel_task(&pending_arn, "cancel-token-a").await;
    assert_eq!(
        cancelled.status, 200,
        "case cancel a pending task: expected 200, got {} {}",
        cancelled.status, cancelled.body
    );
    assert!(
        matches!(
            member(&cancelled.body, "cancellationStatus"),
            "CANCELLING" | "CANCELLED"
        ),
        "case cancel a pending task: expected a cancellation status, got {}",
        cancelled.body
    );

    // One that ran to completion is not: there is nothing left to cancel.
    let completed = braket
        .create_task(create_body("cancel-completed", SV1, 10, BUCKET, &action))
        .await;
    let completed_arn = completed.str_member("quantumTaskArn");
    let task = poll_until_terminal(&braket, &completed_arn).await;
    assert_eq!(
        member(&task, "status"),
        "COMPLETED",
        "case setup: the task must have completed, got {task}"
    );
    let conflict = braket.cancel_task(&completed_arn, "cancel-token-b").await;
    assert_eq!(
        conflict.status, 409,
        "case cancel a completed task: expected 409, got {} {}",
        conflict.status, conflict.body
    );
    assert_eq!(
        conflict.error.as_deref(),
        Some("ConflictException"),
        "case cancel a completed task: expected ConflictException, got {:?}",
        conflict.error
    );
    let expected = format!(
        "Amazon Braket cannot cancel a quantum task in the COMPLETED status for device ARN \
         {SV1}."
    );
    assert_eq!(
        conflict.message(),
        expected,
        "case cancel a completed task: expected {expected:?}, got {:?}",
        conflict.message()
    );
    // And the refusal changed nothing: the task is still the one that completed.
    let after = braket.get_task(&completed_arn).await;
    assert_eq!(
        member(&after.body, "status"),
        "COMPLETED",
        "case after the refused cancel: expected the status to stand, got {}",
        after.body
    );
}

/// A well-formed ARN no task is held for is reported as a missing resource, with the
/// message Amazon Braket words that class with — on every quantum-task door that takes
/// one.
#[tokio::test]
async fn a_well_formed_arn_no_task_is_held_for_is_a_missing_resource() {
    let (braket, _s3) = Braket::with_s3();
    let ghosts = [
        "arn:aws:braket:eu-west-1:000000000000:quantum-task/11111111-1111-4111-8111-111111111111",
        "arn:aws:braket:::quantum-task/never-created",
    ];
    for ghost in ghosts {
        let read: Reply = braket.get_task(ghost).await;
        let cancel: Reply = braket.cancel_task(ghost, "ghost-token").await;
        for (operation, reply) in [("GetQuantumTask", read), ("CancelQuantumTask", cancel)] {
            assert_eq!(
                reply.status, 404,
                "case {operation} on {ghost}: expected 404, got {} {}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error.as_deref(),
                Some("ResourceNotFoundException"),
                "case {operation} on {ghost}: expected ResourceNotFoundException, got {:?}",
                reply.error
            );
            assert_eq!(
                reply.message(),
                "Quantum task not found",
                "case {operation} on {ghost}: expected the service's own sentence, got {:?}",
                reply.message()
            );
        }
    }
}

/// Amazon S3 reports a stored object exactly as the write that stored it declared it, so a
/// read of a finished task's result object reports the store's own default media type —
/// the Braket write declares none — and the CRC32 whole-object checksum that write
/// carries, beside the standard metadata of any stored object.
///
/// The rows differ along the run that produced the document, and so along its size and its
/// bytes: the properties a read reports are the write's, not the document's. The recorded
/// deterministic circuit at 10 shots is ONE row.
#[tokio::test]
async fn a_result_object_is_read_back_with_the_properties_its_write_stored() {
    let (braket, s3) = Braket::with_s3();
    create_bucket(&s3).await;

    // (case, source, shots)
    let cases: Vec<(&str, &str, i64)> = vec![
        (
            // The recorded run of this fixture: an X on the first of two measured qubits.
            "the recorded deterministic circuit at 10 shots",
            "OPENQASM 3.0; bit[2] b; qubit[2] q; x q[0]; b[0] = measure q[0]; \
             b[1] = measure q[1];",
            10,
        ),
        (
            "a one-qubit circuit at a single shot",
            "OPENQASM 3.0; qubit[1] q; x q[0];",
            1,
        ),
        (
            // Many more shots over more qubits: a document some hundreds of rows long.
            "a three-qubit circuit at 250 shots",
            "OPENQASM 3.0; qubit[3] q; h q[0]; cnot q[0], q[1]; cnot q[1], q[2];",
            250,
        ),
    ];

    for (index, (case, source, shots)) in cases.iter().enumerate() {
        let created = braket
            .create_task(create_body(
                &format!("object-token-{index}"),
                SV1,
                *shots,
                BUCKET,
                &openqasm_action(source),
            ))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: expected 200, got {} {}",
            created.status, created.body
        );
        let arn = created.str_member("quantumTaskArn");
        let task = poll_until_terminal(&braket, &arn).await;
        assert_eq!(
            member(&task, "status"),
            "COMPLETED",
            "case {case}: expected COMPLETED, got {task}"
        );
        let key = format!("{}/{RESULTS_OBJECT}", member(&task, "outputS3Directory"));

        let object = s3
            .get_object()
            .bucket(BUCKET)
            .key(&key)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case}: expected the object at {key}, got {error:?}")
            });
        let content_type = object.content_type().map(ToString::to_string);
        let checksum = object.checksum_crc32().map(ToString::to_string);
        let checksum_type = object
            .checksum_type()
            .map(|value| value.as_str().to_string());
        let accept_ranges = object.accept_ranges().map(ToString::to_string);
        let etag = object.e_tag().map(ToString::to_string);
        let last_modified = object.last_modified().copied();
        let encryption = object
            .server_side_encryption()
            .map(|value| value.as_str().to_string());
        let bytes = object
            .body
            .collect()
            .await
            .unwrap_or_else(|error| panic!("case {case}: the object's body reads, got {error:?}"))
            .into_bytes();

        assert_eq!(
            content_type.as_deref(),
            // The store's own default for an object stored with no stated media type,
            // named where Amazon S3 holds it rather than copied as a value.
            Some(::service_com_amazonaws_s3_amazons3::state::DEFAULT_CONTENT_TYPE),
            "case {case}: expected the store's default media type, got {content_type:?}"
        );
        // The CRC32 of the bytes the read served, Base64 encoded the way every `Checksum*`
        // member carries a checksum.
        let mut digest = ::crc32fast::Hasher::new();
        digest.update(&bytes);
        let expected = ::aws_smithy_types::base64::encode(digest.finalize().to_be_bytes());
        assert_eq!(
            checksum.as_deref(),
            Some(expected.as_str()),
            "case {case}: expected the CRC32 of the {} bytes served ({expected}), got \
             {checksum:?}",
            bytes.len()
        );
        assert_eq!(
            checksum_type.as_deref(),
            Some("FULL_OBJECT"),
            "case {case}: expected a whole-object checksum, got {checksum_type:?}"
        );
        assert_eq!(
            accept_ranges.as_deref(),
            Some("bytes"),
            "case {case}: expected ranged reads to be offered, got {accept_ranges:?}"
        );
        assert!(
            etag.as_deref().is_some_and(|tag| !tag.is_empty()),
            "case {case}: expected an entity tag, got {etag:?}"
        );
        assert!(
            last_modified.is_some(),
            "case {case}: expected a last-modified instant, got {last_modified:?}"
        );
        assert!(
            encryption.is_some(),
            "case {case}: expected the stored encryption to be reported, got {encryption:?}"
        );

        // The document itself is the run's, and it is the whole of the object: one
        // measurement row per shot, read off the bytes the checksum above describes.
        let document: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("case {case}: the result object is JSON: {error}"));
        assert_eq!(
            document["braketSchemaHeader"]["name"].as_str(),
            Some(RESULT_SCHEMA),
            "case {case}: expected the gate-model result schema, got {document}"
        );
        assert_eq!(
            document["measurements"].as_array().map(Vec::len),
            Some(usize::try_from(*shots).expect("a shot count fits a usize")),
            "case {case}: expected one measurement row per shot, got {}",
            document["measurements"]
        );

        // A HEAD of the same key reports the same stored properties: both readers answer
        // about one object. The checksum is asked for explicitly, which is what a HEAD has
        // to do to be told it ("to retrieve the checksum, this parameter must be enabled").
        let head = s3
            .head_object()
            .bucket(BUCKET)
            .key(&key)
            .checksum_mode(::aws_sdk_s3::types::ChecksumMode::Enabled)
            .send()
            .await
            .unwrap_or_else(|error| panic!("case {case}: expected a HEAD of {key}, got {error:?}"));
        assert_eq!(
            head.content_type().map(ToString::to_string),
            content_type,
            "case {case}: expected the HEAD to report the stored media type, got {:?}",
            head.content_type()
        );
        assert_eq!(
            head.checksum_crc32().map(ToString::to_string),
            checksum,
            "case {case}: expected the HEAD to report the stored checksum, got {:?}",
            head.checksum_crc32()
        );
        assert_eq!(
            head.e_tag().map(ToString::to_string),
            etag,
            "case {case}: expected the HEAD to report the stored entity tag, got {:?}",
            head.e_tag()
        );
    }
}

/// Every Braket device-parameters schema is named after the device family it belongs to and
/// ends with this segment — the shape of the name the service's own resolution builds
/// (`crate::state::device_parameters::parameters_schema` in the service crate), asserted
/// rather than listed so no one device's schema name is written out here.
const PARAMETERS_SCHEMA_SUFFIX: &str = "_device_parameters";

/// The schema of the gate-model paradigm parameters a caller nests in its own
/// device-parameters document, as the Braket device schemas publish it. It travels as the
/// CALLER's input here: these documents are what the create sent, not a catalog this suite
/// owns.
const GATE_MODEL_PARAMETERS_SCHEMA: &str = "braket.device_schema.gate_model_parameters";

/// A gate-model result document's `taskMetadata` reports the task as it was SUBMITTED: the
/// device-parameters document the task RAN UNDER is one of its members — the document the
/// create carried, nested as the document it is, or the device's own default document for
/// the program that ran when the create carried none — and it is the same value the API
/// read of the task reports, because both report one stored document.
///
/// The rows differ along what the rule generalises over: whether the create carried a
/// parameters document at all, and the document it carried (a full simulator parameters
/// document, one that pins the program's qubits, an empty one). The recorded two-qubit
/// document is ONE row. Each row also pins the media type the object is served under: the
/// write declares none, so the store's own default is what a read reports.
#[tokio::test]
async fn a_result_documents_task_metadata_reports_the_parameters_the_task_ran_under() {
    let (braket, s3) = Braket::with_s3();
    create_bucket(&s3).await;

    // (case, the qubits the program declares, its source, the document the create carries)
    let cases: Vec<(&str, u64, &str, Option<Value>)> = vec![
        (
            // The recorded run of this fixture: its circuit and its parameters document.
            "the recorded simulator parameters document",
            2,
            "OPENQASM 3.0; bit[2] b; qubit[2] q; x q[0]; b[0] = measure q[0]; \
             b[1] = measure q[1];",
            Some(json!({
                "braketSchemaHeader": {
                    "name": "braket.device_schema.simulators.\
                             gate_model_simulator_device_parameters",
                    "version": "1",
                },
                "paradigmParameters": {
                    "braketSchemaHeader": {
                        "name": GATE_MODEL_PARAMETERS_SCHEMA,
                        "version": "1",
                    },
                    "qubitCount": 2,
                    "disableQubitRewiring": false,
                },
            })),
        ),
        (
            "a document that pins the program's qubits",
            3,
            "OPENQASM 3.0; qubit[3] q; h q[0]; cnot q[0], q[1]; cnot q[1], q[2];",
            Some(json!({
                "paradigmParameters": {
                    "braketSchemaHeader": {
                        "name": GATE_MODEL_PARAMETERS_SCHEMA,
                        "version": "1",
                    },
                    "qubitCount": 3,
                    "disableQubitRewiring": true,
                },
            })),
        ),
        (
            "an empty document the create carried",
            1,
            "OPENQASM 3.0; qubit[1] q; x q[0];",
            Some(json!({})),
        ),
        (
            "no document at all",
            2,
            "OPENQASM 3.0; qubit[2] q; h q[0]; cnot q[0], q[1];",
            None,
        ),
    ];

    for (index, (case, qubits, source, submitted)) in cases.iter().enumerate() {
        let mut body = create_body(
            &format!("parameters-token-{index}"),
            SV1,
            2,
            BUCKET,
            &openqasm_action(source),
        );
        if let Some(document) = submitted {
            // `deviceParameters` travels as JSON TEXT on the API, which is how a client
            // sends the document it wants the task to run under.
            body["deviceParameters"] = json!(document.to_string());
        }
        let created = braket.create_task(body).await;
        assert_eq!(
            created.status, 200,
            "case {case}: expected 200, got {} {}",
            created.status, created.body
        );
        let arn = created.str_member("quantumTaskArn");
        let task = poll_until_terminal(&braket, &arn).await;
        assert_eq!(
            member(&task, "status"),
            "COMPLETED",
            "case {case}: expected COMPLETED, got {task}"
        );
        let key = format!("{}/{RESULTS_OBJECT}", member(&task, "outputS3Directory"));

        let object = s3
            .get_object()
            .bucket(BUCKET)
            .key(&key)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case}: expected the object at {key}, got {error:?}")
            });
        let content_type = object.content_type().map(ToString::to_string);
        let bytes = object
            .body
            .collect()
            .await
            .unwrap_or_else(|error| panic!("case {case}: the object's body reads, got {error:?}"))
            .into_bytes();
        assert_eq!(
            content_type.as_deref(),
            // The store's own default for an object stored with no stated media type,
            // named where Amazon S3 holds it rather than copied as a value.
            Some(::service_com_amazonaws_s3_amazons3::state::DEFAULT_CONTENT_TYPE),
            "case {case}: expected the store's default media type, got {content_type:?}"
        );

        let document: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("case {case}: the result object is JSON: {error}"));
        let metadata = &document["taskMetadata"];
        assert_eq!(
            metadata["id"].as_str(),
            Some(arn.as_str()),
            "case {case}: expected the task's own ARN as the document's id, got {metadata}"
        );
        assert_eq!(
            metadata["deviceId"].as_str(),
            Some(SV1),
            "case {case}: expected the device that ran it, got {metadata}"
        );
        let reported = &metadata["deviceParameters"];
        match submitted {
            Some(expected) => assert_eq!(
                reported, expected,
                "case {case}: expected the submitted document as it was sent, got {reported}"
            ),
            None => {
                let schema = metadata["deviceParameters"]["braketSchemaHeader"]["name"]
                    .as_str()
                    .unwrap_or_default();
                assert!(
                    schema.starts_with("braket.device_schema.")
                        && schema.ends_with(PARAMETERS_SCHEMA_SUFFIX),
                    "case {case}: expected the device's own parameters schema, got {reported}"
                );
                assert_eq!(
                    reported["paradigmParameters"]["qubitCount"].as_u64(),
                    Some(*qubits),
                    "case {case}: expected the program's {qubits} qubits, got {reported}"
                );
                assert_eq!(
                    reported["paradigmParameters"]["disableQubitRewiring"].as_bool(),
                    Some(false),
                    "case {case}: expected rewiring to stay enabled, got {reported}"
                );
            }
        }

        // The API read and the result document report ONE value: the document the task ran
        // under, carried as JSON text on the API and nested as a document in the object.
        let read = member(&task, "deviceParameters");
        let api: Value = serde_json::from_str(read)
            .unwrap_or_else(|error| panic!("case {case}: {read} is not JSON: {error}"));
        assert_eq!(
            &api, reported,
            "case {case}: expected the API's own {api} in the document, got {reported}"
        );
    }
}
