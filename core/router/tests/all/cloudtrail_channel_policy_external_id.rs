// SPDX-License-Identifier: CC0-1.0

//! The CloudTrail Lake channel RESOURCE POLICY as the thing that decides which
//! `externalId` may ingest onto that channel.
//!
//! `cloudtrail-data`'s `PutAuditEvents$externalId` is "conditionally required when the
//! channel's resource policy includes an external ID" — and that policy is the CONTROL
//! PLANE's document: `cloudtrail:PutResourcePolicy` attaches and replaces it,
//! `cloudtrail:DeleteResourcePolicy` detaches it. Two service crates, two stores in the
//! one shared engine, so a service-level suite mounting either one on its own cannot see
//! the step these tests are about: that the call which WRITES a channel's policy is what
//! states — or withdraws — the external-id requirement the data plane then holds a
//! request to, in that same call.
//!
//! Each test is a TABLE whose rows differ along what the rule generalises over: how many
//! external IDs the policy's condition states, how each is spelled, whether the statement
//! that carries the condition grants or denies, the case the condition key is written in,
//! and whether the policy was attached, replaced or detached. The recorded case is one
//! row.
//!
//! The wire helpers are the ones `cloudtrail_lake_custom_event_ingestion` already states
//! for this service pair, so both suites drive one client.
//!
//! Assembling the router costs stack (see `boot_serve.rs`), so every test runs on an
//! explicitly sized thread.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudtrail_lake_custom_event_ingestion::{
    control_plane, create_channel, create_event_data_store, entries, error_code, message_of,
    on_a_router, put_audit_events, ACCOUNT_ID,
};

/// The sentence the data plane refuses an `externalId` that does not satisfy the
/// channel's resource policy with, and the error code it carries.
const INSUFFICIENT_PERMISSION_CODE: &str = "ChannelInsufficientPermission";
/// See [`INSUFFICIENT_PERMISSION_CODE`].
const INSUFFICIENT_PERMISSION_MESSAGE: &str =
    "The externalId does not match the external ID in the channel's resource policy.";

/// The action a channel's resource policy grants: `PutAuditEvents` is the call a custom
/// integration ingests onto a channel with, and the one the policy's external-id
/// condition guards.
const INGEST_ACTION: &str = "cloudtrail-data:PutAuditEvents";

/// A channel resource policy of the documented shape, granting [`INGEST_ACTION`] on
/// `channel_arn` to this account, with `condition` as its statement's `Condition` element
/// (`None` for a statement that states none).
fn channel_policy(channel_arn: &str, condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Sid": "ChannelPolicy",
        "Effect": "Allow",
        "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT_ID}:root") },
        "Action": INGEST_ACTION,
        "Resource": channel_arn,
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// A `StringEquals` condition on `key` with `values` — the way an IAM policy states that
/// a grant applies only when a condition key equals one of a set of values.
fn string_equals(key: &str, values: &[&str]) -> Value {
    let values = if values.len() == 1 {
        json!(values[0])
    } else {
        json!(values)
    };
    json!({ "StringEquals": { key: values } })
}

/// The batch one ingest attempt submits: one well-formed custom event, under `id`.
fn batch(id: &str) -> Vec<(String, String)> {
    vec![(
        id.to_string(),
        r#"{"eventSource":"custom.app","eventName":"ReproAction"}"#.to_string(),
    )]
}

/// Attach `policy` to `resource_arn` and assert the control plane stored it.
async fn put_resource_policy(
    router: &MultiServiceRouter,
    region: &str,
    resource_arn: &str,
    policy: &Value,
    case: &str,
) {
    let request = json!({
        "ResourceArn": resource_arn,
        "ResourcePolicy": serde_json::to_string(policy).expect("serialize the policy"),
    });
    let (status, body) = control_plane(router, region, "PutResourcePolicy", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutResourcePolicy must attach the policy, got {status} with {body}"
    );
}

/// Create an event data store and a `Custom` channel logging to it in `region`, and
/// answer the channel's ARN. One Region holds at most one channel per source, so each row
/// homes its channel in a Region of its own.
async fn channel_in(router: &MultiServiceRouter, region: &str, name: &str, case: &str) -> String {
    let (status, store) = create_event_data_store(router, region, name, true).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateEventDataStore must succeed, got {status} with {store}"
    );
    let store_arn = store["EventDataStoreArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    create_channel(router, region, name, &store_arn, case).await
}

/// One ingest attempt: assert the data plane admits or refuses the submitted
/// `external_id` as the channel's resource policy says it must.
async fn expect_ingest(
    router: &MultiServiceRouter,
    region: &str,
    channel_arn: &str,
    external_id: Option<&str>,
    admitted: bool,
    case: &str,
) {
    let event_id = format!(
        "evt-{}",
        external_id
            .unwrap_or("none")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
    );
    let events = batch(&event_id);
    let (status, reply) = put_audit_events(router, region, channel_arn, external_id, &events).await;
    if admitted {
        assert_eq!(
            status, 200,
            "case {case:?} with externalId {external_id:?}: the policy admits this value, so \
             the batch must ingest, got {status} with {reply}"
        );
        assert_eq!(
            error_code(&reply),
            "",
            "case {case:?} with externalId {external_id:?}: expected no error code, got {reply}"
        );
        let successful = entries(&reply, "successful", "eventID");
        let failed = entries(&reply, "failed", "errorCode");
        assert!(
            failed.is_empty() && successful.len() == events.len(),
            "case {case:?} with externalId {external_id:?}: expected one successful entry per \
             submitted event and none failed, got successful {successful:?} and failed {failed:?}"
        );
    } else {
        assert_eq!(
            status, 400,
            "case {case:?} with externalId {external_id:?}: the policy requires an external ID \
             this request does not present, so the ingest must be refused, got {status} with \
             {reply}"
        );
        assert_eq!(
            error_code(&reply),
            INSUFFICIENT_PERMISSION_CODE,
            "case {case:?} with externalId {external_id:?}: expected the error code \
             {INSUFFICIENT_PERMISSION_CODE:?}, got {reply}"
        );
        let stated = message_of(&reply);
        assert_eq!(
            stated, INSUFFICIENT_PERMISSION_MESSAGE,
            "case {case:?} with externalId {external_id:?}: expected the sentence this refusal \
             carries ({INSUFFICIENT_PERMISSION_MESSAGE:?}), got {stated:?}"
        );
    }
}

/// One row of the policy table: the channel it attaches its policy to, the policy it
/// attaches, and the `externalId` values it then probes with the verdict each must get.
struct PolicyRow {
    case: &'static str,
    region: &'static str,
    name: &'static str,
    /// The policy document the row attaches, built from the channel ARN once the control
    /// plane has minted it.
    policy: fn(&str) -> Value,
    /// `(the externalId the request carries, whether the policy admits it)` — `None` is a
    /// request carrying none at all.
    probes: &'static [(Option<&'static str>, bool)],
}

/// RULE: a request's `externalId` is held to the channel's resource-based policy — a policy
/// whose grant of the ingest is conditional on `sts:ExternalId` admits only a request
/// carrying a value that condition accepts, and a policy stating no such condition admits
/// whatever the request carries, including nothing.
///
/// Rows differ along how many values the condition states, how each is spelled (words, the
/// punctuation the member's pattern admits, digits), the comparison and set qualifier the
/// clause is written under, the case of the condition key, the action the grant covers, and
/// whether the statement carrying the condition grants or denies.
#[test]
fn a_channels_resource_policy_decides_which_external_id_may_ingest() {
    on_a_router("cloudtrail-channel-policy-external-id", |router| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime");
        runtime.block_on(async {
            let rows: Vec<PolicyRow> = vec![
                PolicyRow {
                    case: "one external id",
                    region: "us-west-2",
                    name: "guarded-west",
                    policy: |arn| {
                        channel_policy(arn, Some(string_equals("sts:ExternalId", &["migration-911"])))
                    },
                    // The equality is case-sensitive, so the same value in another case is
                    // not the value the policy states.
                    probes: &[
                        (None, false),
                        (Some("migration-911"), true),
                        (Some("MIGRATION-911"), false),
                        (Some("an-unrelated-external-id"), false),
                    ],
                },
                PolicyRow {
                    case: "two external ids in one condition",
                    region: "us-east-1",
                    name: "guarded-east",
                    policy: |arn| {
                        channel_policy(
                            arn,
                            Some(string_equals(
                                "sts:ExternalId",
                                &["first-passphrase", "second-passphrase"],
                            )),
                        )
                    },
                    // An IAM string condition is satisfied by ANY of the values listed for
                    // its key.
                    probes: &[
                        (None, false),
                        (Some("first-passphrase"), true),
                        (Some("second-passphrase"), true),
                        (Some("third-passphrase"), false),
                    ],
                },
                PolicyRow {
                    case: "an external id spelled with the punctuation the member admits",
                    region: "eu-central-1",
                    name: "guarded-central",
                    policy: |arn| {
                        channel_policy(
                            arn,
                            Some(string_equals(
                                "sts:ExternalId",
                                &["org:unit/team-7.alpha@example"],
                            )),
                        )
                    },
                    probes: &[
                        (None, false),
                        (Some("org:unit/team-7.alpha@example"), true),
                        (Some("ORG:UNIT/TEAM-7.ALPHA@EXAMPLE"), false),
                        (Some("org:unit/team-8.alpha@example"), false),
                    ],
                },
                PolicyRow {
                    case: "an external id written as an account number",
                    region: "ap-northeast-1",
                    name: "guarded-northeast",
                    policy: |arn| {
                        channel_policy(arn, Some(string_equals("sts:ExternalId", &["210987654321"])))
                    },
                    probes: &[
                        (None, false),
                        (Some("210987654321"), true),
                        (Some("210987654322"), false),
                    ],
                },
                PolicyRow {
                    // An IAM condition key is not case-sensitive, so the same key in
                    // another spelling states the same requirement.
                    case: "the condition key in another case",
                    region: "eu-west-1",
                    name: "guarded-west-eu",
                    policy: |arn| {
                        channel_policy(
                            arn,
                            Some(string_equals("STS:EXTERNALID", &["cased-key-secret"])),
                        )
                    },
                    probes: &[
                        (None, false),
                        (Some("cased-key-secret"), true),
                        (Some("another-secret"), false),
                    ],
                },
                PolicyRow {
                    // One value stated twice is one value: the reading drops repeats.
                    case: "the same external id stated twice",
                    region: "ap-south-1",
                    name: "guarded-south",
                    policy: |arn| {
                        channel_policy(
                            arn,
                            Some(string_equals(
                                "sts:ExternalId",
                                &["repeat-secret", "repeat-secret"],
                            )),
                        )
                    },
                    probes: &[
                        (None, false),
                        (Some("repeat-secret"), true),
                        (Some("another-secret"), false),
                    ],
                },
                PolicyRow {
                    // The policy language writes a condition's values as strings, and a
                    // document may equally state one as the JSON scalar that names it:
                    // the same value, so both spellings state an external id.
                    case: "a condition value stated as a JSON number beside a string",
                    region: "ca-central-1",
                    name: "guarded-central-ca",
                    policy: |arn| {
                        channel_policy(
                            arn,
                            Some(json!({
                                "StringEquals": { "sts:ExternalId": ["mixed-secret", 42] },
                            })),
                        )
                    },
                    probes: &[
                        (None, false),
                        (Some("mixed-secret"), true),
                        (Some("42"), true),
                        // Within the member's `@length 2..1224`, so the refusal under test
                        // is the policy's and not the boundary's.
                        (Some("43"), false),
                    ],
                },
                PolicyRow {
                    // IAM's `ForAnyValue:` qualifier is false for a request carrying no
                    // value for the key, so a grant qualified by it still requires one.
                    case: "the condition under a ForAnyValue qualifier",
                    region: "eu-north-1",
                    name: "guarded-north",
                    policy: |arn| {
                        channel_policy(
                            arn,
                            Some(json!({
                                "ForAnyValue:StringEquals": {
                                    "sts:ExternalId": "qualified-secret",
                                },
                            })),
                        )
                    },
                    probes: &[
                        (None, false),
                        (Some("qualified-secret"), true),
                        (Some("another-secret"), false),
                    ],
                },
                PolicyRow {
                    // `…IfExists` is true for a request carrying no value for the key
                    // ("if the condition key is not present … evaluate the condition
                    // element as true"), so such a grant admits a caller that presents
                    // nothing — while a value it DOES present is still held to the clause.
                    case: "the condition under an IfExists operator",
                    region: "ap-southeast-2",
                    name: "open-southeast",
                    policy: |arn| {
                        channel_policy(
                            arn,
                            Some(json!({
                                "StringEqualsIfExists": {
                                    "sts:ExternalId": "if-exists-secret",
                                },
                            })),
                        )
                    },
                    probes: &[
                        (None, true),
                        (Some("if-exists-secret"), true),
                        (Some("another-secret"), false),
                    ],
                },
                PolicyRow {
                    // A pattern comparison is false for a request carrying no value for the
                    // key, and admits exactly the values it matches.
                    case: "the condition under a pattern comparison",
                    region: "eu-west-2",
                    name: "guarded-west-two",
                    policy: |arn| {
                        channel_policy(
                            arn,
                            Some(json!({
                                "StringLike": { "sts:ExternalId": "migration-*" },
                            })),
                        )
                    },
                    probes: &[
                        (None, false),
                        (Some("migration-911"), true),
                        (Some("migration-"), true),
                        (Some("an-unrelated-external-id"), false),
                    ],
                },
                PolicyRow {
                    // A case-insensitive equality admits the value in any case, and nothing
                    // else.
                    case: "the condition under a case-insensitive equality",
                    region: "af-south-1",
                    name: "guarded-south-africa",
                    policy: |arn| {
                        channel_policy(
                            arn,
                            Some(json!({
                                "StringEqualsIgnoreCase": { "sts:ExternalId": "Mixed-Secret" },
                            })),
                        )
                    },
                    probes: &[
                        (None, false),
                        (Some("Mixed-Secret"), true),
                        (Some("mixed-secret"), true),
                        (Some("MIXED-SECRET"), true),
                        (Some("another-secret"), false),
                    ],
                },
                PolicyRow {
                    // `Null` asserting the key is not absent admits any value a request
                    // carries, and refuses a request carrying none.
                    case: "a Null condition asserting the external id is present",
                    region: "me-south-1",
                    name: "guarded-middle-east",
                    policy: |arn| {
                        channel_policy(
                            arn,
                            Some(json!({ "Null": { "sts:ExternalId": "false" } })),
                        )
                    },
                    probes: &[
                        (None, false),
                        (Some("any-passphrase-at-all"), true),
                        (Some("another-one"), true),
                    ],
                },
                PolicyRow {
                    // A grant of ANOTHER action is not a grant of the ingest, so what its
                    // condition states is required of nothing this data plane serves.
                    case: "the condition on a grant of another action",
                    region: "us-west-1",
                    name: "other-action",
                    policy: |arn| {
                        json!({
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Sid": "GrantOfAnotherAction",
                                "Effect": "Allow",
                                "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT_ID}:root") },
                                "Action": "cloudtrail:GetChannel",
                                "Resource": arn,
                                "Condition": string_equals(
                                    "sts:ExternalId",
                                    &["other-action-secret"],
                                ),
                            }],
                        })
                    },
                    probes: &[
                        (None, true),
                        (Some("other-action-secret"), true),
                        (Some("an-unrelated-external-id"), true),
                    ],
                },
                PolicyRow {
                    case: "a grant with no condition at all",
                    region: "sa-east-1",
                    name: "open-south",
                    policy: |arn| channel_policy(arn, None),
                    probes: &[(None, true), (Some("an-unrelated-external-id"), true)],
                },
                PolicyRow {
                    // A `Deny` withholds rather than grants, so its condition requires
                    // nothing of the caller: this policy differs from the one above in
                    // that one element.
                    case: "the condition carried by a Deny statement",
                    region: "us-east-2",
                    name: "denied-condition",
                    policy: |arn| {
                        let mut document = channel_policy(arn, None);
                        document["Statement"]
                            .as_array_mut()
                            .expect("the policy states its statements as an array")
                            .push(json!({
                                "Sid": "DenyWithoutTheExternalId",
                                "Effect": "Deny",
                                "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT_ID}:root") },
                                "Action": INGEST_ACTION,
                                "Resource": arn,
                                "Condition": string_equals("sts:ExternalId", &["denied-secret"]),
                            }));
                        document
                    },
                    probes: &[(None, true), (Some("an-unrelated-external-id"), true)],
                },
            ];

            for row in &rows {
                let PolicyRow {
                    case,
                    region,
                    name,
                    policy,
                    probes,
                } = row;
                let channel_arn = channel_in(&router, region, name, case).await;
                put_resource_policy(&router, region, &channel_arn, &policy(&channel_arn), case)
                    .await;

                for (probe, admitted) in *probes {
                    expect_ingest(&router, region, &channel_arn, *probe, *admitted, case).await;
                }
            }
        });
    });
}

/// What one step of the rewriting table does to the channel's policy before it probes
/// the ingest again.
enum PolicyChange {
    /// Leave the policy as the step before it left it — the opening step, where the
    /// channel carries none at all.
    Unchanged,
    /// Attach (or replace) a policy whose grant carries this `Condition` element; `None`
    /// is a grant stating no condition.
    Attach(Option<Value>),
}

/// One step of that table: what it does to the policy, and the external ids the channel
/// then admits — EMPTY where it admits a request carrying none as well.
struct Step {
    case: &'static str,
    change: PolicyChange,
    admits: &'static [&'static str],
}

/// RULE: the requirement follows the policy for the channel's whole life — attaching a
/// policy states it, replacing the policy restates it, detaching the policy withdraws it,
/// and a channel carrying no such policy is unaffected by another channel's.
#[test]
fn rewriting_or_detaching_the_policy_moves_the_external_id_requirement() {
    on_a_router("cloudtrail-channel-policy-rewrite", |router| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime");
        runtime.block_on(async {
            const REGION: &str = "us-west-2";
            const OTHER_REGION: &str = "us-east-1";
            let case = "the channel whose policy is rewritten";
            let guarded = channel_in(&router, REGION, "rewritten-channel", case).await;
            // A second channel, never given a policy: it must keep ingesting with no
            // external id throughout, so the requirement is the guarded channel's own.
            let untouched =
                channel_in(&router, OTHER_REGION, "untouched-channel", "the untouched channel")
                    .await;

            let steps: Vec<Step> = vec![
                Step {
                    case: "before any policy is attached",
                    change: PolicyChange::Unchanged,
                    admits: &[],
                },
                Step {
                    case: "a policy requiring one external id",
                    change: PolicyChange::Attach(Some(string_equals(
                        "sts:ExternalId",
                        &["first-secret"],
                    ))),
                    admits: &["first-secret"],
                },
                Step {
                    case: "the policy replaced with another external id",
                    change: PolicyChange::Attach(Some(string_equals(
                        "sts:ExternalId",
                        &["second-secret"],
                    ))),
                    admits: &["second-secret"],
                },
                Step {
                    case: "the policy replaced with one stating no condition",
                    change: PolicyChange::Attach(None),
                    admits: &[],
                },
                Step {
                    case: "a policy requiring an external id again",
                    change: PolicyChange::Attach(Some(string_equals(
                        "sts:ExternalId",
                        &["third-secret"],
                    ))),
                    admits: &["third-secret"],
                },
            ];

            for Step {
                case: step,
                change,
                admits,
            } in &steps
            {
                if let PolicyChange::Attach(condition) = change {
                    let document = channel_policy(&guarded, condition.clone());
                    put_resource_policy(&router, REGION, &guarded, &document, step).await;

                    // The document the control plane reports back is the one just
                    // attached, so the requirement and the stored policy are the same
                    // fact read two ways.
                    let (status, body) = control_plane(
                        &router,
                        REGION,
                        "GetResourcePolicy",
                        &json!({ "ResourceArn": guarded }),
                    )
                    .await;
                    assert_eq!(
                        status, 200,
                        "case {step:?}: GetResourcePolicy must report the attached policy, got \
                         {status} with {body}"
                    );
                    let reported: Value = serde_json::from_str(
                        body["ResourcePolicy"].as_str().unwrap_or_default(),
                    )
                    .unwrap_or(Value::Null);
                    assert_eq!(
                        reported, document,
                        "case {step:?}: the policy read back must be the policy attached, got \
                         {reported}"
                    );
                }

                // Every secret any step of this table states, so each step asserts what its
                // own policy admits and what it no longer does.
                for probe in [
                    None,
                    Some("first-secret"),
                    Some("second-secret"),
                    Some("third-secret"),
                ] {
                    let admitted = admits.is_empty()
                        || probe.is_some_and(|value| admits.contains(&value));
                    expect_ingest(&router, REGION, &guarded, probe, admitted, step).await;
                    // The other channel carries no policy of its own, whatever this one
                    // requires.
                    expect_ingest(
                        &router,
                        OTHER_REGION,
                        &untouched,
                        probe,
                        true,
                        "the untouched channel",
                    )
                    .await;
                }
            }

            // Detaching the policy withdraws the requirement it stated.
            let (status, body) = control_plane(
                &router,
                REGION,
                "DeleteResourcePolicy",
                &json!({ "ResourceArn": guarded }),
            )
            .await;
            assert_eq!(
                status, 200,
                "DeleteResourcePolicy must detach the policy, got {status} with {body}"
            );
            for probe in [None, Some("third-secret"), Some("an-unrelated-external-id")] {
                expect_ingest(
                    &router,
                    REGION,
                    &guarded,
                    probe,
                    true,
                    "after the policy is detached",
                )
                .await;
            }
        });
    });
}
