// SPDX-License-Identifier: CC0-1.0

//! WHAT a job that could not run as its own role reports, driven end to end over
//! HTTP against IAM, Amazon S3 and S3 Control on one shared engine.
//!
//! The rule under test: **the `FailureReason` a settled job reports is the
//! sentence of the failure CLASS its `FailureCode` names, with the values the
//! request named as its only variable parts — so a job whose role the account's
//! IAM denies S3 Batch Operations reports its `AccessDenied` failure as the
//! DENIAL that it is, naming the role the create named and the grant that role's
//! trust policy is missing, and never as an assume that has simply not succeeded
//! yet.** The distinction is the client's: a reason that reports the assume as
//! outstanding invites the client to submit the job again, while this one tells
//! it that the document IAM holds withholds the grant, so resubmitting the same
//! job cannot run it. Sources: `JobDescriptor$FailureReasons` ("if the specified
//! job failed, this field contains information describing the failure") and
//! `smithy_query(query="shape", shape="JobFailure")` for the two members and
//! their lengths.
//!
//! Where [`crate::batch_job_role_trust`] pins WHICH trust documents stop a job,
//! this suite pins the WHOLE sentence the stopped job reports, exactly, and the
//! rows span what composing it generalises over: how the role's document
//! withholds the grant (another service's principal, an explicit deny beside the
//! grant, the principal for a different action, an account principal rather than
//! a service one), how LONG the role ARN it is composed around is — at the root,
//! and under a path long enough that the explanation no longer fits the member's
//! `@length` — and, as the boundary of the class, a document that DOES grant the
//! assume, which reports no failure at all.
//!
//! The helpers and the fixed text are the ones [`crate::batch_job_role_trust`]
//! holds, so the two suites cannot disagree about what a trust document looks
//! like or about what the reason opens with.

use serde_json::{json, Value};

use service_com_amazonaws_s3control_awss3controlservicev20180820::state::{
    JOB_STATUS_COMPLETE, JOB_STATUS_FAILED,
};

use crate::access_point_lifecycle::REGION;
use crate::batch_job_execution::poll_terminal;
use crate::batch_job_role_trust::{
    create_job_body, create_role, failures, statement, trust, ASSUME_ROLE,
    ASSUME_ROLE_FAILURE_CODE, ASSUME_ROLE_FAILURE_TEXT, BATCH_OPERATIONS_PRINCIPAL,
    FAILURE_REASON_LENGTH_MAX, MANIFEST_KEY, TARGET_KEY,
};
use crate::object_store::ObjectStore;

/// One role the rule is asserted over.
struct Row {
    case: &'static str,
    /// The IAM path the role is created under: the longer it is, the longer the
    /// ARN the reason is composed around.
    path: String,
    /// The role name, unique within the row's own IAM store.
    role_name: String,
    /// The role's `AssumeRolePolicyDocument`.
    trust: Value,
    /// Whether that document grants the batch-operations principal the assume,
    /// and so whether the job runs rather than reporting this failure.
    grants: bool,
}

/// The role table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "another service's principal only",
            path: "/".to_string(),
            role_name: "denied-other-service".to_string(),
            trust: trust(json!([statement(
                "Allow",
                json!("ec2.amazonaws.com"),
                json!(ASSUME_ROLE)
            )])),
            grants: false,
        },
        Row {
            case: "an explicit deny beside the grant",
            path: "/".to_string(),
            role_name: "denied-explicitly".to_string(),
            trust: trust(json!([
                statement(
                    "Allow",
                    json!(BATCH_OPERATIONS_PRINCIPAL),
                    json!(ASSUME_ROLE)
                ),
                statement("Deny", json!(BATCH_OPERATIONS_PRINCIPAL), json!(ASSUME_ROLE)),
            ])),
            grants: false,
        },
        Row {
            case: "the principal, for an action that is not the assume",
            path: "/".to_string(),
            role_name: "denied-other-action".to_string(),
            trust: trust(json!([statement(
                "Allow",
                json!(BATCH_OPERATIONS_PRINCIPAL),
                json!("sts:TagSession")
            )])),
            grants: false,
        },
        Row {
            case: "an account principal rather than a service one",
            path: "/".to_string(),
            role_name: "denied-account-principal".to_string(),
            trust: trust(json!([{
                "Effect": "Allow",
                "Principal": {"AWS": "arn:aws:iam::123456789012:root"},
                "Action": ASSUME_ROLE
            }])),
            grants: false,
        },
        Row {
            // An IAM path is `@length(max: 512)` and a role name
            // `@length(max: 64)`, so a role IAM itself holds can carry an ARN
            // long enough that the explanation no longer fits beside it.
            case: "a role under a path long enough to crowd out the explanation",
            path: format!("/{}/{}/", "a".repeat(63), "b".repeat(63)),
            role_name: "c".repeat(64),
            trust: trust(json!([statement(
                "Allow",
                json!("ec2.amazonaws.com"),
                json!(ASSUME_ROLE)
            )])),
            grants: false,
        },
        Row {
            case: "a document that grants the principal the assume",
            path: "/".to_string(),
            role_name: "granted".to_string(),
            trust: trust(json!([statement(
                "Allow",
                json!(BATCH_OPERATIONS_PRINCIPAL),
                json!(ASSUME_ROLE)
            )])),
            grants: true,
        },
    ]
}

/// The reason the `AccessDenied` failure of the role `role_arn` reports: the
/// denial and the role it is about, then the grant that role's trust policy is
/// missing — which follows only while both fit inside the member's
/// `@length(max: 256)`, since a role ARN may be far longer than the reason.
fn expected_reason(role_arn: &str) -> String {
    let denial = format!("{ASSUME_ROLE_FAILURE_TEXT} {role_arn}.");
    let explained = format!(
        "{denial} Its trust policy must allow {BATCH_OPERATIONS_PRINCIPAL} to perform \
         {ASSUME_ROLE}."
    );
    if explained.chars().count() <= FAILURE_REASON_LENGTH_MAX {
        explained
    } else {
        denial
    }
}

/// RULE: a job denied the role it would have run as reports its `AccessDenied`
/// failure in that class's own sentence — the denial, the role the create named
/// and the grant its trust policy is missing — inside the length the model
/// declares for that member, and a job whose document grants the assume reports
/// no failure at all.
#[tokio::test]
async fn a_denied_job_role_is_reported_as_a_denial_naming_the_role_and_the_missing_grant() {
    for (row_at, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let (store, iam) = ObjectStore::with_iam();
        let bucket = format!("batch-denied-reason-{row_at}");
        store.create_bucket(case, REGION, &bucket).await;
        store
            .put_object(case, REGION, &bucket, TARGET_KEY, b"target-bytes")
            .await;
        let manifest = format!("{bucket},{TARGET_KEY}\n");
        let e_tag = store
            .put_object(case, REGION, &bucket, MANIFEST_KEY, manifest.as_bytes())
            .await;
        let role_arn = create_role(&iam, case, &row.path, &row.role_name, &row.trust).await;

        let created = store
            .s3control(
                REGION,
                "POST",
                "/v20180820/jobs",
                &create_job_body(
                    &format!("denied-reason-{row_at}"),
                    &bucket,
                    &e_tag,
                    &role_arn,
                ),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected CreateJob to answer 200, got {} with body {}",
            created.status, created.body
        );
        let job_id = created
            .text("JobId")
            .unwrap_or_else(|| panic!("case {case:?}: CreateJob answers a JobId: {}", created.body));

        let settled = poll_terminal(&store, case, &job_id).await;
        let status = settled.text("Status").unwrap_or_default();
        let reported = failures(&settled);

        if row.grants {
            assert_eq!(
                status, JOB_STATUS_COMPLETE,
                "case {case:?}: expected the job to run and settle {JOB_STATUS_COMPLETE}, got \
                 {status:?} with body {}",
                settled.body
            );
            assert!(
                reported.is_empty(),
                "case {case:?}: expected no FailureReasons on a job that ran, got {reported:?}"
            );
            continue;
        }

        assert_eq!(
            status, JOB_STATUS_FAILED,
            "case {case:?}: expected a job denied its role to settle {JOB_STATUS_FAILED}, got \
             {status:?} with body {}",
            settled.body
        );
        let expected = expected_reason(&role_arn);
        assert_eq!(
            reported,
            vec![(ASSUME_ROLE_FAILURE_CODE.to_string(), expected.clone())],
            "case {case:?}: expected exactly the {ASSUME_ROLE_FAILURE_CODE:?} failure reading \
             {expected:?}, got {reported:?}"
        );
        let length = expected.chars().count();
        assert!(
            length <= FAILURE_REASON_LENGTH_MAX,
            "case {case:?}: expected a reason of at most {FAILURE_REASON_LENGTH_MAX} characters, \
             got {length}"
        );
        let tags = store.object_tags(REGION, &bucket, TARGET_KEY).await;
        assert!(
            tags.is_empty(),
            "case {case:?}: expected {TARGET_KEY} to be left untagged, got {tags:?}"
        );
    }
}
