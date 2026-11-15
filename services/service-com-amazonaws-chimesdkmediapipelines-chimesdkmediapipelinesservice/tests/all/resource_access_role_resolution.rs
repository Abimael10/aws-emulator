// SPDX-License-Identifier: CC0-1.0

//! How a media insights pipeline configuration's `ResourceAccessRoleArn` is RESOLVED, end to
//! end over the wire against the service that holds the account's IAM roles.
//!
//! The RULE under test, one level above the recorded case (one create, one misnamed role):
//!
//! > The role a configuration will be run under has to BE a role of this account: the
//! > member's modelled `@pattern` only says the value is ARN-shaped, so a well-formed ARN
//! > naming a role this account's IAM holds nothing under is refused `403
//! > ForbiddenException` with the message "Service does not have permission to invoke role:
//! > <the ARN the request carried>" — the service reports a role it cannot assume as a
//! > permissions failure rather than as an absent resource — and nothing is created or
//! > changed; a value naming a role IAM does hold is served, whatever path that role sits
//! > under, and a value naming another account's role is not this emulator's IAM to ask
//! > about, so it is served too.
//!
//! The rows span what the rule generalises over: whether IAM holds the role (never created,
//! created, created and then deleted), the PATH it sits under, the account the ARN names, and
//! the operation the value arrived on — the rule holds for
//! `CreateMediaInsightsPipelineConfiguration` and for
//! `UpdateMediaInsightsPipelineConfiguration`, which replaces the stored role.
//!
//! The harness is the sibling suite's ([`crate::identity_authorization`]): the same three
//! services on one engine, since the role this service resolves is IAM's.

use serde_json::{json, Value};

use crate::identity_authorization::{account, Answer, Harness, ACCOUNT_KEY, CONFIGURATIONS};

/// The wire error-shape name restJson1 resolves from `x-amzn-errortype`, and the `ErrorCode`
/// value the modelled `ForbiddenException` carries in its body `Code` member. Source:
/// `smithy_query(query="shape",shape="ErrorCode")` — `BadRequest | Forbidden | NotFound |
/// ResourceLimitExceeded | ServiceFailure | ServiceUnavailable | Throttling`.
const FORBIDDEN_EXCEPTION: &str = "ForbiddenException";
const ERROR_CODE_FORBIDDEN: &str = "Forbidden";

/// The message a configuration naming a role IAM holds nothing under is refused with.
/// Written out HERE rather than taken from the service's own formatter, so any change to
/// that wording fails this suite: fixed text followed by the ARN the request carried.
fn role_refusal(role_arn: &str) -> String {
    format!("Service does not have permission to invoke role: {role_arn}")
}

/// A create body naming `role_arn` under `name`, otherwise valid.
fn configuration(name: &str, role_arn: &str) -> Value {
    json!({
        "MediaInsightsPipelineConfigurationName": name,
        "ResourceAccessRoleArn": role_arn,
        "Elements": [{
            "Type": "AmazonTranscribeProcessor",
            "AmazonTranscribeProcessorConfiguration": { "LanguageCode": "en-US" },
        }],
    })
}

/// Assert `answer` is the role refusal for `role_arn`.
fn assert_role_refused(case: &str, answer: &Answer, role_arn: &str) {
    assert_eq!(
        answer.status, 403,
        "case {case:?}: expected the modelled 403, got {} body {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        FORBIDDEN_EXCEPTION,
        "case {case:?}: expected the modelled error shape, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.body.pointer("/Code").and_then(Value::as_str),
        Some(ERROR_CODE_FORBIDDEN),
        "case {case:?}: expected the modelled ErrorCode, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.message(),
        role_refusal(role_arn),
        "case {case:?}: expected the role-permission wording, got {:?}",
        answer.message()
    );
}

/// RULE: a create naming a role IAM holds nothing under is the modelled 403 naming that ARN,
/// and nothing is created; a create naming a role IAM holds is served.
#[tokio::test]
async fn a_role_iam_does_not_hold_is_refused_and_nothing_is_created() {
    let harness = Harness::new();
    let held = harness.create_role("media-insights-access", None, None).await;
    let under_path = harness
        .create_role("media-insights-scoped", Some("/service-role/"), None)
        .await;
    harness.create_role("media-insights-doomed", None, None).await;
    let doomed = format!("arn:aws:iam::{}:role/media-insights-doomed", account());
    harness.delete_role("media-insights-doomed").await;

    // Each row: the case, the ARN, and whether the create is served.
    let rows: Vec<(&str, String, bool)> = vec![
        ("a role IAM holds", held.clone(), true),
        ("a role IAM holds under a path", under_path.clone(), true),
        (
            "a role nobody ever created",
            format!("arn:aws:iam::{}:role/sim2real-no-such-role", account()),
            false,
        ),
        (
            "a role under a path nobody ever created",
            format!("arn:aws:iam::{}:role/service-role/absent", account()),
            false,
        ),
        ("a role IAM held and no longer holds", doomed, false),
        (
            "a role of ANOTHER account, which this emulator's IAM cannot be asked about",
            "arn:aws:iam::210987654321:role/cross-account-access".to_string(),
            true,
        ),
    ];

    for (index, (case, role_arn, served)) in rows.into_iter().enumerate() {
        let name = format!("role-row-{index}");
        let answer = harness
            .chime(
                ACCOUNT_KEY,
                "POST",
                CONFIGURATIONS,
                Some(configuration(&name, &role_arn)),
            )
            .await;
        if served {
            assert_eq!(
                answer.status, 201,
                "case {case:?}: expected the create to be served, got {} body {:?}",
                answer.status, answer.body
            );
            let stored = harness
                .chime(ACCOUNT_KEY, "GET", &format!("{CONFIGURATIONS}/{name}"), None)
                .await;
            assert_eq!(
                stored
                    .body
                    .pointer("/MediaInsightsPipelineConfiguration/ResourceAccessRoleArn")
                    .and_then(Value::as_str),
                Some(role_arn.as_str()),
                "case {case:?}: the stored configuration must carry the role the create named"
            );
        } else {
            assert_role_refused(case, &answer, &role_arn);
            let read_back = harness
                .chime(ACCOUNT_KEY, "GET", &format!("{CONFIGURATIONS}/{name}"), None)
                .await;
            assert_eq!(
                read_back.status, 404,
                "case {case:?}: a refused create must store nothing, got {} body {:?}",
                read_back.status, read_back.body
            );
            let listed = harness
                .chime(ACCOUNT_KEY, "GET", CONFIGURATIONS, None)
                .await;
            let present = listed
                .body
                .pointer("/MediaInsightsPipelineConfigurations")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries.iter().any(|entry| {
                        entry["MediaInsightsPipelineConfigurationName"].as_str()
                            == Some(name.as_str())
                    })
                })
                .unwrap_or(false);
            assert!(
                !present,
                "case {case:?}: a refused create must leave the listing without that name"
            );
        }
    }
}

/// RULE (the same rule on the UPDATE, which replaces the stored role): an update naming a
/// role IAM holds nothing under is the same refusal, and the stored configuration is left
/// exactly as it was.
#[tokio::test]
async fn an_update_to_a_role_iam_does_not_hold_changes_nothing() {
    let harness = Harness::new();
    let held = harness.create_role("media-insights-access", None, None).await;
    let other = harness.create_role("media-insights-other", None, None).await;
    let name = "update-role-rule";

    let created = harness
        .chime(
            ACCOUNT_KEY,
            "POST",
            CONFIGURATIONS,
            Some(configuration(name, &held)),
        )
        .await;
    assert_eq!(
        created.status, 201,
        "setup: the create must be served, got {} body {:?}",
        created.status, created.body
    );
    let before = harness
        .chime(ACCOUNT_KEY, "GET", &format!("{CONFIGURATIONS}/{name}"), None)
        .await;

    let absent = format!("arn:aws:iam::{}:role/no-such-role-for-update", account());
    let refused = harness
        .chime(
            ACCOUNT_KEY,
            "PUT",
            &format!("{CONFIGURATIONS}/{name}"),
            Some(json!({
                "ResourceAccessRoleArn": absent,
                "Elements": [{ "Type": "KinesisDataStreamSink" }],
            })),
        )
        .await;
    assert_role_refused("an update naming a role IAM does not hold", &refused, &absent);
    let after = harness
        .chime(ACCOUNT_KEY, "GET", &format!("{CONFIGURATIONS}/{name}"), None)
        .await;
    assert_eq!(
        after.body.pointer("/MediaInsightsPipelineConfiguration"),
        before.body.pointer("/MediaInsightsPipelineConfiguration"),
        "case \"a refused update\": the stored configuration must be untouched"
    );

    // An update to ANOTHER role IAM does hold is served, so the refusal is about the role
    // alone.
    let served = harness
        .chime(
            ACCOUNT_KEY,
            "PUT",
            &format!("{CONFIGURATIONS}/{name}"),
            Some(json!({
                "ResourceAccessRoleArn": other,
                "Elements": [{ "Type": "KinesisDataStreamSink" }],
            })),
        )
        .await;
    assert_eq!(
        served.status, 200,
        "case \"an update naming a role IAM holds\": expected 200, got {} body {:?}",
        served.status, served.body
    );
    assert_eq!(
        served
            .body
            .pointer("/MediaInsightsPipelineConfiguration/ResourceAccessRoleArn")
            .and_then(Value::as_str),
        Some(other.as_str()),
        "case \"an update naming a role IAM holds\": the update must apply the new role"
    );
}
