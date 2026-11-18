// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the `ModelPackageStatus` every projection of a model package
//! reports: the ONE value stored on that package's record.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so what
//! these tests exercise is what a real SDK client gets. The harness and the
//! create/describe helpers are reused from `model_package_registry`.
//!
//! The RULE, asserted over a TABLE of inputs rather than one recorded call: a model
//! package's status is a per-package stored value — set when `CreateModelPackage` answers
//! to the registration work that call left outstanding, and advanced to the terminal
//! status at the instant the clock owes it — and every operation that projects it
//! (`DescribeModelPackage`, `BatchDescribeModelPackage`, `ListModelPackages` and the
//! `Search` over `ModelPackage` records) reports that stored value for that package, so no
//! projection reports a constant, one package's status never stands in for another's in a
//! batch, and the instant the transition is recorded at is the instant it was DUE —
//! whichever projection happened to materialise it, and whenever it was read.
//!
//! `BatchDescribeModelPackage` is the batch-read half of that rule: its
//! `ModelPackageSummaries` entry for a registration with nothing left to validate reports
//! the terminal status from the first call onwards, so a client that batch-polls model
//! packages and waits for the terminal status before approving or deploying them
//! terminates.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sagemaker_sagemaker::state;

use crate::model_package_registry::{
    container, status_at_registration, str_field, validation_specification, Emulator,
};

/// How long a poll below gives the clock to settle a registration: far longer than
/// `state::MODEL_PACKAGE_COMPLETED_AFTER`, so a loaded host still settles well inside it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How often such a poll looks again. Only the loop's condition decides when it stops.
const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The terminal `ModelPackageStatus` a registration reaches once it has no validation work
/// left: a member of the modelled `ModelPackageStatus` enum
/// (`smithy_query(query="shape",shape="ModelPackageStatus")`, values `Pending`,
/// `InProgress`, `Completed`, `Failed`, `Deleting`).
const COMPLETED: &str = "Completed";

/// The non-terminal `ModelPackageStatus` a registration with validation work still to run
/// is in, from the same modelled enum.
const PENDING: &str = "Pending";

/// The group every registration below joins, so each one is a VERSIONED package — which is
/// what `BatchDescribeModelPackage` ("this action batch describes a list of versioned model
/// packages") and `ListModelPackages`' group listing report.
const GROUP: &str = "status-projection";

/// How far two epoch-second timestamps of one answer may differ from the span between the
/// instants they stand for: the precision a JSON epoch-second float keeps at present-day
/// instants, not a timing budget — both instants are stored exactly, so nothing here
/// depends on how fast the host runs.
const TIMESTAMP_TOLERANCE: f64 = 0.01;

/// Every operation that projects a model package's `ModelPackageStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reader {
    Describe,
    Batch,
    List,
    Search,
}

/// The readers each case reads a package through.
const READERS: &[Reader] = &[Reader::Describe, Reader::Batch, Reader::List, Reader::Search];

/// An `InferenceSpecification` carrying `containers` containers.
fn inference_specification(containers: usize) -> Value {
    let containers: Vec<Value> = (0..containers)
        .map(|index| container(&format!("123.dkr.ecr.us-east-1.amazonaws.com/image{index}:1")))
        .collect();
    json!({"Containers": containers})
}

/// One `BatchDescribeModelPackage` over `arns`, as the map it answers with.
async fn batch_summaries(
    emulator: &Emulator,
    case: &str,
    arns: &[String],
) -> serde_json::Map<String, Value> {
    let output = emulator
        .ok(
            case,
            "BatchDescribeModelPackage",
            &json!({"ModelPackageArnList": arns}),
        )
        .await;
    output
        .get("ModelPackageSummaries")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected ModelPackageSummaries, got {output}"))
}

/// The `BatchDescribeModelPackage` summary for the one ARN `arn`.
async fn batch_summary(emulator: &Emulator, case: &str, arn: &str) -> Value {
    batch_summaries(emulator, case, &[arn.to_string()])
        .await
        .get(arn)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a batch summary for {arn}"))
}

/// The `ListModelPackages` entry for `arn`, listed through the group it belongs to.
async fn listed(emulator: &Emulator, case: &str, arn: &str) -> Value {
    let output = emulator
        .ok(
            case,
            "ListModelPackages",
            &json!({"ModelPackageGroupName": GROUP}),
        )
        .await;
    output
        .get("ModelPackageSummaryList")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected a summary list, got {output}"))
        .iter()
        .find(|entry| entry.get("ModelPackageArn") == Some(&json!(arn)))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected {arn} to be listed, got {output}"))
}

/// The `Search` record for `arn`, out of the `ModelPackage` records the search answers.
async fn searched(emulator: &Emulator, case: &str, arn: &str) -> Value {
    let output = emulator
        .ok(case, "Search", &json!({"Resource": "ModelPackage"}))
        .await;
    output
        .get("Results")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected a Results list, got {output}"))
        .iter()
        .filter_map(|record| record.get("ModelPackage"))
        .find(|package| package.get("ModelPackageArn") == Some(&json!(arn)))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected {arn} in the results, got {output}"))
}

/// The `ModelPackageStatus` `reader` reports for the package `arn` names.
async fn status_through(emulator: &Emulator, case: &str, reader: Reader, arn: &str) -> String {
    let projected = match reader {
        Reader::Describe => emulator.describe(case, arn).await,
        Reader::Batch => batch_summary(emulator, case, arn).await,
        Reader::List => listed(emulator, case, arn).await,
        Reader::Search => searched(emulator, case, arn).await,
    };
    str_field(&projected, "ModelPackageStatus")
}

/// Read `arn` through `reader` until it reports the terminal status, or the deadline
/// passes. What is under test is that the transition is reported at all, not when the host
/// gets round to it.
async fn poll_until_completed(emulator: &Emulator, case: &str, reader: Reader, arn: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let status = status_through(emulator, case, reader, arn).await;
        if status == COMPLETED {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?} read through {reader:?}: expected {COMPLETED} within \
             {SETTLE_DEADLINE:?}, got {status:?}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// A member of one answer that carries an epoch-second timestamp.
fn seconds(projected: &Value, member: &str) -> f64 {
    projected
        .get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("expected a numeric {member} in {projected}"))
}

/// One row: how a registration is made, and therefore what status it is in when
/// `CreateModelPackage` answers.
struct Row {
    case: &'static str,
    /// The members the registration carries beyond `ModelPackageGroupName`.
    extras: Value,
}

/// The rows the rule is asserted over, spanning what it generalises over: whether the
/// registration leaves validation work outstanding (no specification at all, a
/// specification with no profile, a profile waived by `SkipModelValidation`, a profile
/// still to run), how many profiles it names, and how much else it carries beside them.
///
/// The row that leaves work outstanding comes LAST, so the clock owing IT the transition
/// starts at the last create the case makes: the batch read that then observes it still
/// settling is the very next request, rather than one made several round trips later.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no validation specification",
            extras: json!({"InferenceSpecification": inference_specification(1)}),
        },
        Row {
            case: "a validation specification naming no profile",
            extras: json!({
                "InferenceSpecification": inference_specification(2),
                "ValidationSpecification": validation_specification(0),
            }),
        },
        Row {
            case: "a validation profile waived by SkipModelValidation",
            extras: json!({
                "InferenceSpecification": inference_specification(1),
                "ValidationSpecification": validation_specification(1),
                "SkipModelValidation": "All",
            }),
        },
        Row {
            case: "a non-ASCII description, an approval decision and three containers",
            extras: json!({
                "InferenceSpecification": inference_specification(3),
                "ModelPackageDescription": "fraud detector — Détection ✓ 日本語",
                "ModelApprovalStatus": "Approved",
                "ModelPackageRegistrationType": "Registered",
            }),
        },
        Row {
            case: "a validation profile still to run",
            extras: json!({
                "InferenceSpecification": inference_specification(1),
                "ValidationSpecification": validation_specification(1),
            }),
        },
    ]
}

/// RULE: each projection reports the status stored on the package it is about — so the
/// summaries of ONE batch covering registrations in different statuses report each
/// package's own status, and the single read, the listing and the search agree with that
/// batch package by package. A registration with no validation work outstanding is
/// reported in the terminal status from the first read onwards; one that has work
/// outstanding is reported in the non-terminal status until the clock owes the transition,
/// and in the terminal status afterwards.
#[tokio::test]
async fn every_projection_reports_the_stored_status_of_its_own_package() {
    let emulator = Emulator::new();
    emulator.create_group(state::DEFAULT_REGION, GROUP).await;

    let rows = rows();
    let mut arns = Vec::new();
    let mut expected = Vec::new();
    for row in &rows {
        let mut body = row.extras.clone();
        body["ModelPackageGroupName"] = json!(GROUP);
        arns.push(emulator.create(row.case, &body).await);
        expected.push(status_at_registration(&body));
    }

    // ONE batch covering every registration: a per-item read reports each package's own
    // stored status, so the rows that registered complete and the row still settling are
    // reported differently inside the same answer.
    let summaries = batch_summaries(&emulator, "every registration in one batch", &arns).await;
    for ((row, arn), expected) in rows.iter().zip(&arns).zip(&expected) {
        let case = row.case;
        let summary = summaries
            .get(arn)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected a summary keyed by {arn}"));
        let status = str_field(&summary, "ModelPackageStatus");
        assert_eq!(
            status, *expected,
            "case {case:?}: expected the batch summary to report {expected}, got {status:?}"
        );
    }

    // Once the clock has nothing left to owe, every projection reports the terminal status
    // for every row — including the one that was registered with work outstanding, and
    // still including the ones that were complete when they were registered.
    for (row, arn) in rows.iter().zip(&arns) {
        poll_until_completed(&emulator, row.case, Reader::Describe, arn).await;
    }
    for (row, arn) in rows.iter().zip(&arns) {
        let case = row.case;
        for reader in READERS {
            let status = status_through(&emulator, case, *reader, arn).await;
            assert_eq!(
                status, COMPLETED,
                "case {case:?} read through {reader:?}: expected {COMPLETED} once nothing is \
                 outstanding, got {status:?}"
            );
        }
    }

    // Reading changed nothing a later caller sees: the same batch answers the same way
    // twice over.
    let after = batch_summaries(&emulator, "the same batch again", &arns).await;
    let again = batch_summaries(&emulator, "and once more", &arns).await;
    assert_eq!(
        after, again,
        "case \"the same batch again\": a batch read must not change what it reports, got \
         {again:?} after {after:?}"
    );
}

/// RULE: each projection reports the stored non-terminal status of a registration that
/// still has validation work outstanding, and the transition that registration's status
/// then makes is recorded at the instant it was DUE — a fixed span after the registering
/// call — so the `LastModifiedTime` a settled package reports is independent of which
/// projection first materialised the transition and of how long after the due instant it
/// was read, and stays put on every later read.
#[tokio::test]
async fn the_settled_transition_is_recorded_at_the_instant_it_was_due() {
    let span = state::MODEL_PACKAGE_COMPLETED_AFTER.as_secs_f64();

    // One store per row, so the projection the row names is the only one that could have
    // materialised that package's transition.
    for reader in READERS {
        let case = format!("settled through {reader:?}");
        let case = case.as_str();
        let emulator = Emulator::new();
        emulator.create_group(state::DEFAULT_REGION, GROUP).await;
        let arn = emulator
            .create(
                case,
                &json!({
                    "ModelPackageGroupName": GROUP,
                    "InferenceSpecification": inference_specification(1),
                    // The profile is what leaves the registration outstanding, so this
                    // package has a transition to make at all.
                    "ValidationSpecification": validation_specification(1),
                }),
            )
            .await;

        // The caller's very next request, so the registration is still settling: the
        // projection reports the stored non-terminal status rather than anticipating the
        // transition.
        let status = status_through(&emulator, case, *reader, &arn).await;
        assert_eq!(
            status, PENDING,
            "case {case:?}: expected the stored {PENDING} while the validation profile is \
             still to run, got {status:?}"
        );

        poll_until_completed(&emulator, case, *reader, &arn).await;

        let described = emulator.describe(case, &arn).await;
        assert_eq!(
            str_field(&described, "ModelPackageStatus"),
            COMPLETED,
            "case {case:?}: expected {COMPLETED} once the transition is materialised, \
             got {described}"
        );
        let creation = seconds(&described, "CreationTime");
        let modified = seconds(&described, "LastModifiedTime");
        let offset = modified - creation;
        assert!(
            (offset - span).abs() <= TIMESTAMP_TOLERANCE,
            "case {case:?}: expected the transition to be recorded {span} s after the \
             registration, got {offset} s"
        );

        // A later read reports the same instant: the value is the one the clock owed, not
        // the one some read happened at.
        let reread = emulator.describe(case, &arn).await;
        assert_eq!(
            reread.get("LastModifiedTime"),
            described.get("LastModifiedTime"),
            "case {case:?}: expected a stable LastModifiedTime, got {:?} after {:?}",
            reread.get("LastModifiedTime"),
            described.get("LastModifiedTime")
        );
    }
}
