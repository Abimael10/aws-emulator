// SPDX-License-Identifier: CC0-1.0

//! What a read of this service STATES on its 200, driven end-to-end over the same
//! `MultiServiceRouter` the emulator boots.
//!
//! # The rule
//!
//! **A read's 200 always states the member that IS its report; that the thing reported does
//! not exist is stated IN the member — the empty form its shape admits, or JSON `null` when
//! its shape admits none — and never by leaving the member out.** A client that reads this
//! member has no other place to look: the key's absence would be indistinguishable from a
//! response the emulator truncated, and an SDK caller could not tell "nothing is
//! configured" from "this member was not served".
//!
//! The two reads whose whole report is ONE optional member
//! (`smithy_query(query="operation", operation="GetModelInvocationLoggingConfiguration,
//! GetResourcePolicy")`) are the two this suite drives, and they differ in exactly what the
//! rule generalises over — the FORM "nothing" takes in the member's own shape:
//!
//! * `GetModelInvocationLoggingConfiguration` reports `loggingConfig`, a `LoggingConfig`
//!   STRUCTURE with no empty form, and models no not-found error at all (its errors are
//!   `AccessDeniedException`, `InternalServerException`, `ThrottlingException`), so while no
//!   configuration is in force the member is stated as JSON `null`;
//! * `GetResourcePolicy` reports `resourcePolicy`, a `ResourcePolicyDocument` STRING whose
//!   empty form is the empty policy document, so a resource holding no policy has that
//!   document stated.
//!
//! Every other read of this service either reports a `@required` member, or answers an
//! absent thing with the not-found error it models, so its 200 carries the member by
//! construction.
//!
//! The rows differ along what the rule generalises over for each: which destinations and
//! flags a configuration names (including one naming none), and how the reported thing came
//! to be absent — it was never written, or it was written and then removed. Each row reads
//! TWICE, so what is stated is the stored state a later caller sees rather than what one
//! call happened to answer.
//!
//! The calls themselves come from the suites that own each rule —
//! `tests/all/model_invocation_logging.rs` for the configuration and
//! `tests/all/resource_policies.rs` for the policy, which is also where the DOCUMENT a
//! policy-less resource reports and the resource types it reports it across are pinned
//! (`tests/all/resource_policy_empty_document.rs`). What this suite adds, for both members
//! at once, is the half neither can see through a `get(..).unwrap_or(Null)` read: that the
//! KEY is there.

use serde_json::{json, Value};

use crate::model_invocation_logging::{delete, get, put};
use crate::resource_policies::{
    admitted_documents, delete_policy, get_policy, guardrail_arn, put_policy, NO_POLICY,
};
use crate::support::{emulator, Emulator, Reply};

/// What a row does to the thing a read reports, before the read that follows it.
#[derive(Debug)]
enum Act {
    /// Nothing at all: read the thing as it stands.
    Nothing,
    /// Write it — a `loggingConfig`, or a policy document.
    Write(Value),
    /// Remove it.
    Remove,
}

/// The value the read states for `member`, asserting the read was a 200 whose body STATES
/// the member — a key that is present, whatever value it carries.
fn stated(case: &str, reply: &Reply, member: &str) -> Value {
    assert_eq!(
        (reply.status, reply.error.as_deref()),
        (200, None),
        "case {case:?}: the read must answer 200, got {} {:?}",
        reply.status,
        reply.body
    );
    let body = reply.body.as_object().unwrap_or_else(|| {
        panic!(
            "case {case:?}: the 200 must carry a JSON object, got {:?}",
            reply.body
        )
    });
    body.get(member).cloned().unwrap_or_else(|| {
        panic!(
            "case {case:?}: the 200 must state {member:?} whether or not there is anything \
             to report, got {:?}",
            reply.body
        )
    })
}

/// The value two consecutive reads state for `member`, asserting both state it and that the
/// second states what the first did — a read reports stored state and disturbs none.
fn stated_twice(case: &str, member: &str, first: &Reply, again: &Reply) -> Value {
    let first = stated(case, first, member);
    let again = stated(case, again, member);
    assert_eq!(
        again, first,
        "case {case:?}: a second read must state the same value, got {again:?} after {first:?}"
    );
    first
}

/// What `GetModelInvocationLoggingConfiguration` states for `loggingConfig`, read twice.
async fn stated_logging_config(case: &str, emu: &Emulator) -> Value {
    let first = get(emu).await;
    let again = get(emu).await;
    stated_twice(case, "loggingConfig", &first, &again)
}

/// What `GetResourcePolicy` states for `resourcePolicy` on `arn`, read twice.
async fn stated_resource_policy(case: &str, emu: &Emulator, arn: &str) -> Value {
    let first = get_policy(emu, arn).await;
    let again = get_policy(emu, arn).await;
    stated_twice(case, "resourcePolicy", &first, &again)
}

/// A configuration naming only the S3 destination.
fn s3_only() -> Value {
    json!({ "s3Config": { "bucketName": "bedrock-invocation-logs", "keyPrefix": "prod/" } })
}

/// A configuration naming only the CloudWatch Logs destination. `roleArn` is a role of the
/// calling account, which is the only kind this account may pass.
fn cloudwatch_only(account: &str) -> Value {
    json!({
        "cloudWatchConfig": {
            "logGroupName": "logs/bedrock",
            "roleArn": format!("arn:aws:iam::{account}:role/BedrockInvocationLogging"),
        },
        "textDataDeliveryEnabled": true,
    })
}

/// A configuration naming both destinations and every delivery flag the model declares.
fn both_destinations(account: &str) -> Value {
    json!({
        "cloudWatchConfig": {
            "logGroupName": "logs/bedrock/all",
            "roleArn": format!("arn:aws:iam::{account}:role/BedrockLogging"),
            "largeDataDeliveryS3Config": { "bucketName": "bedrock-large-data" },
        },
        "s3Config": { "bucketName": "bedrock-invocation-logs" },
        "textDataDeliveryEnabled": true,
        "imageDataDeliveryEnabled": true,
        "embeddingDataDeliveryEnabled": false,
        "videoDataDeliveryEnabled": true,
        "audioDataDeliveryEnabled": false,
    })
}

/// RULE: `GetModelInvocationLoggingConfiguration` states `loggingConfig` on every 200 —
/// the configuration in force, or JSON `null` while none is.
///
/// The rows run in sequence against ONE emulator, so each read also witnesses what the act
/// before it left behind.
#[tokio::test]
async fn the_logging_configuration_read_always_states_its_member() {
    let emu = emulator();
    let account = crate::support::ACCOUNT;

    // (case, what to do first, the value the read after it must state)
    let rows: Vec<(&str, Act, Value)> = vec![
        ("nothing was ever put", Act::Nothing, Value::Null),
        ("an s3 destination", Act::Write(s3_only()), s3_only()),
        (
            "a cloudwatch destination",
            Act::Write(cloudwatch_only(account)),
            cloudwatch_only(account),
        ),
        (
            "both destinations and every flag",
            Act::Write(both_destinations(account)),
            both_destinations(account),
        ),
        (
            "a configuration naming no destination at all",
            Act::Write(json!({})),
            json!({}),
        ),
        ("deleted", Act::Remove, Value::Null),
        ("deleted again", Act::Remove, Value::Null),
        ("put again after the delete", Act::Write(s3_only()), s3_only()),
        ("deleted once more", Act::Remove, Value::Null),
        ("read again, still nothing", Act::Nothing, Value::Null),
    ];

    for (case, act, expected) in rows {
        match &act {
            Act::Nothing => {}
            Act::Write(logging_config) => {
                let reply = put(&emu, logging_config).await;
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: the put must answer 200, got {} {:?}",
                    reply.status, reply.body
                );
            }
            Act::Remove => {
                let reply = delete(&emu).await;
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: the delete must answer 200, got {} {:?}",
                    reply.status, reply.body
                );
            }
        }
        let value = stated_logging_config(case, &emu).await;
        assert_eq!(
            value, expected,
            "case {case:?}: expected the read to state {expected:?}, got {value:?}"
        );
    }
}

/// RULE, on the same member from the other side: a put REFUSED before it reaches the store
/// changes nothing a later read states. The row drives the one refusal this operation's own
/// input can earn — a put carrying no `loggingConfig`, which is `@required` — so what is
/// stated after it is what the act before it left, including the stated `null`.
#[tokio::test]
async fn a_refused_put_leaves_the_stated_configuration_as_it_was() {
    let emu = emulator();
    for (case, before) in [
        ("nothing configured", Value::Null),
        ("a configuration in force", s3_only()),
    ] {
        if before != Value::Null {
            let baseline = put(&emu, &before).await;
            assert_eq!(
                baseline.status, 200,
                "case {case:?}: the baseline put must answer 200, got {} {:?}",
                baseline.status, baseline.body
            );
        }
        let refused = emu
            .call("PUT", crate::model_invocation_logging::LOGGING_URI, Some(json!({})))
            .await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (400, Some("ValidationException")),
            "case {case:?}: a put carrying no loggingConfig is refused 400 \
             ValidationException, got {} {:?}",
            refused.status,
            refused.body
        );
        let value = stated_logging_config(case, &emu).await;
        assert_eq!(
            value, before,
            "case {case:?}: a refused put changes nothing a later read states, expected \
             {before:?}, got {value:?}"
        );
    }
}

/// RULE on the other member of the class: `GetResourcePolicy` states `resourcePolicy` on
/// every 200 — the attached document, or the empty policy document for a resource holding
/// none, whether it never held one or the one it held was deleted.
#[tokio::test]
async fn the_resource_policy_read_always_states_its_member() {
    let emu = emulator();
    let arn = guardrail_arn(&emu, "states-its-member-guardrail").await;
    let (form, submitted, reported) = admitted_documents()
        .into_iter()
        .next()
        .expect("the shared document table holds at least one admitted document");

    // (case, what to do first, the document the read after it must state)
    let rows: Vec<(&str, Act, Value)> = vec![
        (
            "no policy was ever attached",
            Act::Nothing,
            json!(NO_POLICY),
        ),
        (
            "a policy is attached",
            Act::Write(json!(submitted)),
            json!(reported),
        ),
        (
            "the attached policy was deleted",
            Act::Remove,
            json!(NO_POLICY),
        ),
    ];

    for (case, act, expected) in rows {
        match &act {
            Act::Nothing => {}
            Act::Write(document) => {
                let put = put_policy(
                    &emu,
                    &arn,
                    document.as_str().expect("the document table holds JSON strings"),
                )
                .await;
                assert_eq!(
                    put.status, 201,
                    "case {case:?} ({form}): PutResourcePolicy must answer 201, got {} {:?}",
                    put.status, put.body
                );
            }
            Act::Remove => {
                let removed = delete_policy(&emu, &arn).await;
                assert_eq!(
                    removed.status, 200,
                    "case {case:?}: DeleteResourcePolicy must answer 200, got {} {:?}",
                    removed.status, removed.body
                );
            }
        }
        let value = stated_resource_policy(case, &emu, &arn).await;
        assert_eq!(
            value, expected,
            "case {case:?}: expected the read to state {expected:?}, got {value:?}"
        );
    }
}
