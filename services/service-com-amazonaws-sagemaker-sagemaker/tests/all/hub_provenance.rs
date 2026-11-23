// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the PROVENANCE a JumpStart hub records: the identity that
//! created it and the identity of the last call that modified it, as `DescribeHub` reports
//! them.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`, and the `awsQuery` form IAM and STS take) through the same
//! `MultiServiceRouter` the production router uses, on the harness that already mounts
//! SageMaker beside the two services holding the account's credentials
//! ([`crate::image_digest_from_registry`]), because who a request speaks for is those
//! services' answer rather than this one's.
//!
//! The RULE, asserted over a TABLE of inputs rather than one recorded call: the identity a
//! hub records as its creator and as its last modifier is the identity whose credentials
//! SIGNED the call that created or last modified it — as `sts:GetCallerIdentity` reports
//! that identity — reported on every read of the hub and never changed by a read. The rows
//! differ along the KIND of credential the create is signed with, which is what the rule
//! generalises over: the account's own published credential, a credential id no record of
//! the account names, an IAM user's own long-term access key, and an assumed-role session.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::image_digest_from_registry::{Emulator, ACCOUNT_KEY_ID};
// The one reading of an `awsQuery` answer's `<name>` element this tree has.
use crate::lineage_read_authorization::element;

/// The account every ARN below is minted under, as the emulator mints it.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The two provenance members a hub reports, each a `UserContext`.
const PROVENANCE: [&str; 2] = ["CreatedBy", "LastModifiedBy"];

/// A deadline far longer than the span this state gives a hub's `Creating` status, so a
/// loaded runner cannot fail the wait: nothing here asserts on how long the transition
/// took, only that the hub reaches the status the clock owes it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll below waits before looking again. It bounds how often the condition is
/// re-read, not how long the test waits for it.
const POLL_EVERY: Duration = Duration::from_millis(20);

/// A `CreateHub` body carrying only the two required members.
fn minimal_hub(name: &str) -> Value {
    json!({ "HubName": name, "HubDescription": "a hub" })
}

/// The ARN of the IAM identity a `UserContext` member reports, or `None` when the member or
/// any part of it is absent.
fn identity_arn(described: &Value, member: &str) -> Option<String> {
    described
        .pointer(&format!("/{member}/IamIdentity/Arn"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The unique id of the IAM identity a `UserContext` member reports.
fn principal_id(described: &Value, member: &str) -> Option<String> {
    described
        .pointer(&format!("/{member}/IamIdentity/PrincipalId"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Read `hub` again until `DescribeHub` reports the `InService` status the clock owes it, or
/// a generous deadline passes: the wait is on the CONDITION, never on a fixed time.
async fn poll_until_in_service(emulator: &Emulator, case: &str, hub: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = emulator
            .ok(case, "DescribeHub", &json!({ "HubName": hub }))
            .await;
        let status = described
            .get("HubStatus")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if status == "InService" {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected hub {hub} to reach InService before the deadline, \
             last reported {status:?}"
        );
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// Every credential kind a create below is signed with, and the identity ARN each speaks
/// for. The two records the account holds (an IAM user's key, an assumed-role session) are
/// created through IAM's and STS's own operations, so the identity a hub records is one the
/// account really holds.
async fn signing_credentials(emulator: &Emulator) -> Vec<(&'static str, String, String)> {
    let ambient_arn = service_common::caller_principal::ambient_user_arn();

    let user = "hub-author";
    emulator
        .query(
            "an IAM user's own access key",
            "iam",
            "2010-05-08",
            "CreateUser",
            &[("UserName", user.to_string())],
        )
        .await;
    let created_key = emulator
        .query(
            "an IAM user's own access key",
            "iam",
            "2010-05-08",
            "CreateAccessKey",
            &[("UserName", user.to_string())],
        )
        .await;
    let user_key_id = element(&created_key, "AccessKeyId")
        .unwrap_or_else(|| panic!("expected an <AccessKeyId> in {created_key}"))
        .to_string();

    let role = "hub-session-role";
    let session = "hub-session";
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    emulator
        .query(
            "an assumed-role session",
            "iam",
            "2010-05-08",
            "CreateRole",
            &[
                ("RoleName", role.to_string()),
                ("AssumeRolePolicyDocument", trust),
            ],
        )
        .await;
    let assumed = emulator
        .query(
            "an assumed-role session",
            "sts",
            "2011-06-15",
            "AssumeRole",
            &[
                ("RoleArn", format!("arn:aws:iam::{ACCOUNT}:role/{role}")),
                ("RoleSessionName", session.to_string()),
            ],
        )
        .await;
    let session_key_id = element(&assumed, "AccessKeyId")
        .unwrap_or_else(|| panic!("expected an <AccessKeyId> in {assumed}"))
        .to_string();

    vec![
        (
            "the account's own published credential",
            ACCOUNT_KEY_ID.to_string(),
            ambient_arn.clone(),
        ),
        (
            "another credential id no record names",
            "AKIAIOSFODNN7EXAMPLE".to_string(),
            ambient_arn,
        ),
        (
            "an IAM user's own access key",
            user_key_id,
            format!("arn:aws:iam::{ACCOUNT}:user/{user}"),
        ),
        (
            "an assumed-role session",
            session_key_id,
            format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{session}"),
        ),
    ]
}

/// RULE: a hub reports `CreatedBy` and `LastModifiedBy` as the identity whose credentials
/// signed the call that created it — the account's own published identity for a credential
/// no record names, the IAM user that owns a long-term access key, the session for an
/// assumed-role credential — and an access key id, being a credential id rather than a user
/// name, is no part of that identity's ARN.
#[tokio::test]
async fn a_hub_records_the_identity_that_signed_the_create() {
    let emulator = Emulator::new();
    let rows = signing_credentials(&emulator).await;

    for (index, (case, access_key_id, expected)) in rows.iter().enumerate() {
        let name = format!("hub{index}");
        let created = emulator
            .ok_as(access_key_id, case, "CreateHub", &minimal_hub(&name))
            .await;
        let arn = created
            .get("HubArn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected a HubArn, got {created}"))
            .to_string();

        // Both provenance members are reported, both name the signing identity, and both
        // carry the unique id AWS gives it.
        let described = emulator
            .ok_as(
                access_key_id,
                case,
                "DescribeHub",
                &json!({ "HubName": name }),
            )
            .await;
        for member in PROVENANCE {
            let reported = identity_arn(&described, member);
            assert_eq!(
                reported.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected {member}.IamIdentity.Arn to name the signing \
                 identity {expected:?}, got {described}"
            );
            let id = principal_id(&described, member).unwrap_or_default();
            assert!(
                !id.is_empty(),
                "case {case:?}: expected {member}.IamIdentity.PrincipalId to carry the id \
                 of the signing principal, got {described}"
            );
            assert!(
                !reported.unwrap_or_default().contains(access_key_id.as_str()),
                "case {case:?}: an access key id is no part of an identity's ARN, \
                 got {described}"
            );
        }
        // Nothing has modified the hub since the create, so the modifying identity is the
        // creating one.
        assert_eq!(
            described.get("LastModifiedBy"),
            described.get("CreatedBy"),
            "case {case:?}: an unmodified hub must report its creator as its last \
             modifier, got {described}"
        );

        // `HubName` is modelled `HubNameOrArn`, so the hub's ARN reaches the same record —
        // and the same provenance.
        let by_arn = emulator
            .ok_as(access_key_id, case, "DescribeHub", &json!({ "HubName": arn }))
            .await;
        for member in PROVENANCE {
            assert_eq!(
                identity_arn(&by_arn, member),
                identity_arn(&described, member),
                "case {case:?}: the ARN form must reach the same hub's {member}, \
                 got {by_arn}"
            );
        }

        // The identity belongs to the CREDENTIAL, not to the resource: a second hub the
        // same caller creates reports the same creator, and a hub another caller creates
        // reports that other caller.
        let sibling = format!("hub{index}-second");
        emulator
            .ok_as(access_key_id, case, "CreateHub", &minimal_hub(&sibling))
            .await;
        let described_sibling = emulator
            .ok_as(
                access_key_id,
                case,
                "DescribeHub",
                &json!({ "HubName": sibling }),
            )
            .await;
        assert_eq!(
            identity_arn(&described_sibling, "CreatedBy"),
            Some(expected.clone()),
            "case {case:?}: two hubs created by one caller must report one creator, \
             got {described_sibling}"
        );
    }

    // Across the rows, a hub created by a DIFFERENT principal reports a different creator:
    // the value follows the signing credential rather than being fixed for the service.
    let mut creators = Vec::new();
    for (index, (case, access_key_id, _)) in rows.iter().enumerate() {
        let described = emulator
            .ok_as(
                access_key_id,
                case,
                "DescribeHub",
                &json!({ "HubName": format!("hub{index}") }),
            )
            .await;
        creators.push(identity_arn(&described, "CreatedBy").unwrap_or_default());
    }
    let user_creator = creators[2].clone();
    let session_creator = creators[3].clone();
    assert_ne!(
        user_creator, creators[0],
        "case \"distinct principals\": expected the IAM user's hub to report a creator \
         other than the account's own identity, got {creators:?}"
    );
    assert_ne!(
        session_creator, user_creator,
        "case \"distinct principals\": expected the session's hub to report a creator \
         other than the IAM user, got {creators:?}"
    );
}

/// RULE: a hub's provenance is a fact about the calls made to it, so reading it changes
/// nothing a later caller sees — not even across the `Creating` -> `InService` transition
/// the clock owes, which is nobody's call and records no identity.
#[tokio::test]
async fn reading_a_hub_never_changes_the_provenance_it_reports() {
    let case = "a hub read before and after its transition";
    let emulator = Emulator::new();
    let ambient_arn = service_common::caller_principal::ambient_user_arn();
    emulator
        .ok(case, "CreateHub", &minimal_hub("stable-hub"))
        .await;

    let fresh = emulator
        .ok(case, "DescribeHub", &json!({ "HubName": "stable-hub" }))
        .await;
    for member in PROVENANCE {
        assert_eq!(
            identity_arn(&fresh, member),
            Some(ambient_arn.clone()),
            "case {case:?}: expected {member} on the first read, got {fresh}"
        );
    }

    // The status the clock owes is materialised by a later read, which moves
    // `LastModifiedTime` — and must leave the recorded identities alone.
    let settled = poll_until_in_service(&emulator, case, "stable-hub").await;
    for member in PROVENANCE {
        assert_eq!(
            identity_arn(&settled, member),
            identity_arn(&fresh, member),
            "case {case:?}: the transition must not rewrite {member}, got {settled}"
        );
    }
    let again = emulator
        .ok(case, "DescribeHub", &json!({ "HubName": "stable-hub" }))
        .await;
    assert_eq!(
        again, settled,
        "case {case:?}: a settled hub must report the same body on every read"
    );
}
