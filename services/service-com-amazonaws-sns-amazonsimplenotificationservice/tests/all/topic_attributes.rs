// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the topic ATTRIBUTE slice: the map `GetTopicAttributes`
//! projects, the write paths that fill it (`SetTopicAttributes`, `AddPermission`,
//! `RemovePermission`) and the two documents SNS computes into it (the access policy
//! and the effective delivery policy).
//!
//! Every call goes over the wire through the router the emulator serves in production,
//! so nothing but persisted state carries information between calls.

use service_com_amazonaws_sns_amazonsimplenotificationservice::state::{
    ACTION_NAMES, RECEIVE_ACTION,
};

use super::topics_subscriptions_publish::{attribute_entries, attribute_map, Answer, Sns};

/// The account id an ARN this emulator minted carries: the fifth field of
/// `arn:aws:sns:<region>:<account>:<name>`.
fn account_of(arn: &str) -> String {
    arn.split(':').nth(4).unwrap_or_default().to_string()
}

/// The JSON document an attribute value carries, or a panic naming the case.
fn json_of(case: &str, name: &str, value: &str) -> serde_json::Value {
    serde_json::from_str(value)
        .unwrap_or_else(|e| panic!("case {case:?}: {name} must be a JSON document, got {value:?}: {e}"))
}

/// Set one topic attribute, asserting the call was accepted.
async fn set(sns: &Sns, case: &str, arn: &str, name: &str, value: &str) {
    let answer = sns
        .call(
            "SetTopicAttributes",
            &[
                ("TopicArn", arn),
                ("AttributeName", name),
                ("AttributeValue", value),
            ],
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: SetTopicAttributes {name} expected 200, got {} / {}",
        answer.status, answer.body
    );
}

/// A read of one topic's attributes, asserting the call was answered.
async fn read(sns: &Sns, case: &str, arn: &str) -> Answer {
    let answer = sns.call("GetTopicAttributes", &[("TopicArn", arn)]).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: GetTopicAttributes expected 200, got {} / {}",
        answer.status, answer.body
    );
    answer
}

/// One row of the projection table: the case, the topic's name, the attributes set on
/// it, and the attribute keys SNS reports for it, in order.
type ProjectionCase<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)], &'a [&'a str]);

/// GetTopicAttributes reports ONE entry per attribute, each key paired with its own
/// value, in the order SNS reports them — and the same order on every call, whatever
/// the topic holds. The rows differ in which attributes the topic carries, which is
/// what the order generalises over.
#[tokio::test]
async fn topic_attributes_are_reported_in_one_stable_order() {
    let sns = Sns::new();
    let cases: &[ProjectionCase<'_>] = &[
        (
            "a topic with nothing set",
            "order-plain",
            &[],
            &[
                "Policy",
                "Owner",
                "SubscriptionsPending",
                "TopicArn",
                "EffectiveDeliveryPolicy",
                "SubscriptionsConfirmed",
                "DisplayName",
                "SubscriptionsDeleted",
            ],
        ),
        (
            "a topic with a display name",
            "order-display",
            &[("DisplayName", "probe-display-name")],
            &[
                "Policy",
                "Owner",
                "SubscriptionsPending",
                "TopicArn",
                "EffectiveDeliveryPolicy",
                "SubscriptionsConfirmed",
                "DisplayName",
                "SubscriptionsDeleted",
            ],
        ),
        (
            "an encrypted topic",
            "order-encrypted",
            &[("KmsMasterKeyId", "alias/aws/sns")],
            &[
                "Policy",
                "Owner",
                "SubscriptionsPending",
                "KmsMasterKeyId",
                "TopicArn",
                "EffectiveDeliveryPolicy",
                "SubscriptionsConfirmed",
                "DisplayName",
                "SubscriptionsDeleted",
            ],
        ),
        (
            "an encrypted topic with a delivery policy and a display name",
            "order-everything",
            &[
                ("KmsMasterKeyId", "alias/aws/sns"),
                ("DisplayName", "probe-display-name"),
                ("DeliveryPolicy", r#"{"http":{"defaultHealthyRetryPolicy":{"numRetries":7}}}"#),
                ("TracingConfig", "Active"),
            ],
            &[
                "Policy",
                "Owner",
                "SubscriptionsPending",
                "KmsMasterKeyId",
                "TopicArn",
                "EffectiveDeliveryPolicy",
                "SubscriptionsConfirmed",
                "DisplayName",
                "SubscriptionsDeleted",
                // Every other attribute the topic holds follows, ordered by name.
                "DeliveryPolicy",
                "TracingConfig",
            ],
        ),
    ];

    for (case, name, configured, expected_keys) in cases {
        let arn = sns.topic(name).await;
        for (attribute, value) in *configured {
            set(&sns, case, &arn, attribute, value).await;
        }

        let answer = read(&sns, case, &arn).await;
        let entries = attribute_entries(&answer);
        let keys: Vec<&str> = entries.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            keys, *expected_keys,
            "case {case:?}: expected the attribute keys {expected_keys:?}, got {keys:?}"
        );

        // Two reads of one topic answer the same document, entry for entry: the order
        // is SNS's, not a hash map's.
        let again = read(&sns, case, &arn).await;
        assert_eq!(
            attribute_entries(&again),
            entries,
            "case {case:?}: two reads of one topic must report the same entries in the same order"
        );

        // Each key carries its OWN value, so every one is the kind of value its
        // attribute holds.
        let held = attribute_map(&answer);
        assert_eq!(
            held.get("TopicArn").map(String::as_str),
            Some(arn.as_str()),
            "case {case:?}: TopicArn must be the topic's ARN, got {held:?}"
        );
        assert_eq!(
            held.get("Owner").map(String::as_str),
            Some(account_of(&arn).as_str()),
            "case {case:?}: Owner must be the account that owns the topic, got {held:?}"
        );
        for counter in ["SubscriptionsConfirmed", "SubscriptionsPending", "SubscriptionsDeleted"] {
            let value = held.get(counter).cloned().unwrap_or_default();
            assert!(
                value.parse::<u64>().is_ok(),
                "case {case:?}: {counter} must be a count, got {value:?}"
            );
        }
        let policy = json_of(case, "Policy", &held["Policy"]);
        assert!(
            policy.get("Statement").is_some_and(serde_json::Value::is_array),
            "case {case:?}: Policy must be an access policy document, got {policy}"
        );
        let effective = json_of(case, "EffectiveDeliveryPolicy", &held["EffectiveDeliveryPolicy"]);
        assert!(
            effective
                .pointer("/http/defaultHealthyRetryPolicy/numRetries")
                .is_some(),
            "case {case:?}: EffectiveDeliveryPolicy must be a delivery policy document, got {effective}"
        );
        for (attribute, value) in *configured {
            assert_eq!(
                held.get(*attribute).map(String::as_str),
                Some(*value),
                "case {case:?}: {attribute} must report the value set on the topic, got {held:?}"
            );
        }
    }
}

/// SetTopicAttributes persists the named attribute on the topic, and a later
/// GetTopicAttributes reports it: a set-then-get round trip over every attribute class
/// the operation documents — an id, a name, a JSON document, a byte count, a mode, a
/// role ARN and a sample rate.
#[tokio::test]
async fn set_topic_attributes_round_trips_every_documented_attribute() {
    let sns = Sns::new();
    let arn = sns.topic("set-and-get").await;
    let policy_document = serde_json::json!({
        "Version": "2008-10-17",
        "Id": "probe",
        "Statement": [{
            "Sid": "probe-statement",
            "Effect": "Allow",
            "Principal": { "AWS": "*" },
            "Action": "SNS:Publish",
            "Resource": arn,
        }]
    })
    .to_string();
    // The policy grammar accepts a single statement OBJECT as well as a list of them.
    let single_statement_policy = serde_json::json!({
        "Version": "2008-10-17",
        "Statement": {
            "Sid": "one-statement",
            "Effect": "Allow",
            "Principal": { "AWS": "*" },
            "Action": "SNS:Publish",
            "Resource": arn,
        }
    })
    .to_string();
    // The attribute catalog from the `SetTopicAttributesInput$AttributeName` doc; the
    // FIFO-only names are covered by the FIFO row of the refusal table below.
    let cases: &[(&str, &str, &str)] = &[
        ("a display name", "DisplayName", "probe-display-name"),
        ("an encryption key id", "KmsMasterKeyId", "alias/aws/sns"),
        (
            "a delivery policy document",
            "DeliveryPolicy",
            r#"{"http":{"defaultHealthyRetryPolicy":{"numRetries":5}}}"#,
        ),
        ("an access policy document", "Policy", &policy_document),
        (
            "an access policy whose Statement is one statement",
            "Policy",
            &single_statement_policy,
        ),
        ("a byte count at the documented floor", "MaximumMessageSize", "1024"),
        ("a byte count at the documented ceiling", "MaximumMessageSize", "1048576"),
        ("a tracing mode", "TracingConfig", "Active"),
        ("a signature version", "SignatureVersion", "2"),
        (
            "a delivery-status role ARN",
            "SQSSuccessFeedbackRoleArn",
            "arn:aws:iam::000000000000:role/sns-logs",
        ),
        ("a delivery-status sample rate", "SQSSuccessFeedbackSampleRate", "100"),
    ];

    for (case, name, value) in cases {
        set(&sns, case, &arn, name, value).await;
        let held = attribute_map(&read(&sns, case, &arn).await);
        assert_eq!(
            held.get(*name).map(String::as_str),
            Some(*value),
            "case {case:?}: {name} must read back as it was set, got {held:?}"
        );
    }

    // A request that carries no value clears the attribute: the topic reports what a
    // topic that was never given one reports.
    let answer = sns
        .call(
            "SetTopicAttributes",
            &[("TopicArn", &arn), ("AttributeName", "TracingConfig")],
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "clearing an attribute: expected 200, got {} / {}",
        answer.status, answer.body
    );
    let held = attribute_map(&read(&sns, "cleared", &arn).await);
    assert!(
        !held.contains_key("TracingConfig"),
        "a cleared attribute must not be reported, got {held:?}"
    );
}

/// SetTopicAttributes refuses a name SNS does not define for a topic, a name only
/// CreateTopic sets, a value outside a documented range and a document that is not one
/// — and a topic that does not exist.
#[tokio::test]
async fn set_topic_attributes_refuses_what_sns_refuses() {
    let sns = Sns::new();
    let arn = sns.topic("set-refusals").await;
    // (case, attribute name, value, expected status, expected awsQuery error code)
    let cases: &[(&str, &str, &str, u16, &str)] = &[
        ("a name SNS does not define", "NotAnAttribute", "x", 400, "InvalidParameter"),
        ("a name only CreateTopic sets", "FifoTopic", "true", 400, "InvalidParameter"),
        ("an access policy that is not JSON", "Policy", "not-a-document", 400, "InvalidParameter"),
        (
            "an access policy with no statements",
            "Policy",
            r#"{"Version":"2008-10-17"}"#,
            400,
            "InvalidParameter",
        ),
        (
            "a delivery policy that is not JSON",
            "DeliveryPolicy",
            "not-a-document",
            400,
            "InvalidParameter",
        ),
        (
            "a delivery policy that is a bare scalar, not a document",
            "DeliveryPolicy",
            "5",
            400,
            "InvalidParameter",
        ),
        (
            "a delivery policy member of the wrong type",
            "DeliveryPolicy",
            r#"{"http":{"defaultHealthyRetryPolicy":{"numRetries":"five"}}}"#,
            400,
            "InvalidParameter",
        ),
        // The documented range is 1024 to 1048576; each bound is accepted above, and
        // one past each is refused here.
        ("one byte below the documented floor", "MaximumMessageSize", "1023", 400, "InvalidParameter"),
        (
            "one byte above the documented ceiling",
            "MaximumMessageSize",
            "1048577",
            400,
            "InvalidParameter",
        ),
        ("a byte count that is not a number", "MaximumMessageSize", "many", 400, "InvalidParameter"),
        // The closed value sets and the percentage bound SNS documents; each accepted
        // value is exercised by the round-trip table above.
        (
            "a tracing mode SNS does not define",
            "TracingConfig",
            "Sampled",
            400,
            "InvalidParameter",
        ),
        (
            "a signature version SNS does not define",
            "SignatureVersion",
            "3",
            400,
            "InvalidParameter",
        ),
        (
            "a sample rate at the documented floor",
            "SQSSuccessFeedbackSampleRate",
            "0",
            200,
            "",
        ),
        (
            "a sample rate one past the documented ceiling",
            "SQSSuccessFeedbackSampleRate",
            "101",
            400,
            "InvalidParameter",
        ),
        (
            "a sample rate that is not a percentage",
            "HTTPSuccessFeedbackSampleRate",
            "half",
            400,
            "InvalidParameter",
        ),
        ("a valid attribute", "DisplayName", "fine", 200, ""),
    ];

    for (case, name, value, status, code) in cases {
        let answer = sns
            .call(
                "SetTopicAttributes",
                &[
                    ("TopicArn", &arn),
                    ("AttributeName", name),
                    ("AttributeValue", value),
                ],
            )
            .await;
        assert_eq!(
            answer.status, *status,
            "case {case:?}: expected {status}, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.code(),
            *code,
            "case {case:?}: expected error code {code:?}, got {}",
            answer.body
        );
    }

    let missing = format!("{arn}-does-not-exist");
    let answer = sns
        .call(
            "SetTopicAttributes",
            &[
                ("TopicArn", &missing),
                ("AttributeName", "DisplayName"),
                ("AttributeValue", "x"),
            ],
        )
        .await;
    assert_eq!(
        answer.status, 404,
        "a topic that does not exist: expected 404, got {} / {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.code(),
        "NotFound",
        "a topic that does not exist: expected NotFound, got {}",
        answer.body
    );
}

/// One row of the delivery-policy table: the case, the `DeliveryPolicy` the topic is
/// given, and the document members that must then differ from the system default,
/// named by JSON pointer.
type DeliveryPolicyCase<'a> = (&'a str, Option<&'a str>, &'a [(&'a str, serde_json::Value)]);

/// EffectiveDeliveryPolicy is the system default delivery policy with the topic's own
/// DeliveryPolicy laid over it, member by member: a member the topic sets overrides the
/// default, and every member it leaves out keeps the default.
#[tokio::test]
async fn effective_delivery_policy_lays_the_topic_policy_over_the_system_default() {
    let sns = Sns::new();
    // Each row: the case, the DeliveryPolicy set on the topic, and the members that
    // must differ from the system default.
    let cases: &[DeliveryPolicyCase<'_>] = &[
        ("no delivery policy at all", None, &[]),
        (
            "one retry member",
            Some(r#"{"http":{"defaultHealthyRetryPolicy":{"numRetries":10}}}"#),
            &[("/http/defaultHealthyRetryPolicy/numRetries", serde_json::json!(10))],
        ),
        (
            "several retry members",
            Some(
                r#"{"http":{"defaultHealthyRetryPolicy":{"minDelayTarget":5,"maxDelayTarget":300,"backoffFunction":"exponential"}}}"#,
            ),
            &[
                ("/http/defaultHealthyRetryPolicy/minDelayTarget", serde_json::json!(5)),
                ("/http/defaultHealthyRetryPolicy/maxDelayTarget", serde_json::json!(300)),
                (
                    "/http/defaultHealthyRetryPolicy/backoffFunction",
                    serde_json::json!("exponential"),
                ),
            ],
        ),
        (
            "the subscription-override switch",
            Some(r#"{"http":{"disableSubscriptionOverrides":true}}"#),
            &[("/http/disableSubscriptionOverrides", serde_json::json!(true))],
        ),
        (
            "the default request policy",
            Some(r#"{"http":{"defaultRequestPolicy":{"headerContentType":"application/json"}}}"#),
            &[(
                "/http/defaultRequestPolicy/headerContentType",
                serde_json::json!("application/json"),
            )],
        ),
        (
            "members this emulator gives no meaning to",
            Some(
                r#"{"http":{"defaultThrottlePolicy":{"maxReceivesPerSecond":5},"defaultHealthyRetryPolicy":{"numRetries":2,"extraMember":"kept"}}}"#,
            ),
            &[
                (
                    "/http/defaultThrottlePolicy/maxReceivesPerSecond",
                    serde_json::json!(5),
                ),
                ("/http/defaultHealthyRetryPolicy/numRetries", serde_json::json!(2)),
                (
                    "/http/defaultHealthyRetryPolicy/extraMember",
                    serde_json::json!("kept"),
                ),
            ],
        ),
    ];

    // SNS's system default delivery policy: a linear retry of 3 attempts 20 seconds
    // apart, no subscription overrides, a plain-text request policy.
    let system_default: &[(&str, serde_json::Value)] = &[
        ("/http/defaultHealthyRetryPolicy/minDelayTarget", serde_json::json!(20)),
        ("/http/defaultHealthyRetryPolicy/maxDelayTarget", serde_json::json!(20)),
        ("/http/defaultHealthyRetryPolicy/numRetries", serde_json::json!(3)),
        ("/http/defaultHealthyRetryPolicy/numMaxDelayRetries", serde_json::json!(0)),
        ("/http/defaultHealthyRetryPolicy/numNoDelayRetries", serde_json::json!(0)),
        ("/http/defaultHealthyRetryPolicy/numMinDelayRetries", serde_json::json!(0)),
        (
            "/http/defaultHealthyRetryPolicy/backoffFunction",
            serde_json::json!("linear"),
        ),
        ("/http/disableSubscriptionOverrides", serde_json::json!(false)),
        (
            "/http/defaultRequestPolicy/headerContentType",
            serde_json::json!("text/plain; charset=UTF-8"),
        ),
    ];

    for (index, (case, configured, overrides)) in cases.iter().enumerate() {
        let arn = sns.topic(&format!("effective-policy-{index}")).await;
        if let Some(configured) = configured {
            set(&sns, case, &arn, "DeliveryPolicy", configured).await;
        }

        let held = attribute_map(&read(&sns, case, &arn).await);
        let effective = json_of(
            case,
            "EffectiveDeliveryPolicy",
            &held["EffectiveDeliveryPolicy"],
        );
        for (pointer, expected) in *overrides {
            assert_eq!(
                effective.pointer(pointer),
                Some(expected),
                "case {case:?}: {pointer} must carry what the topic set, got {effective}"
            );
        }
        for (pointer, expected) in system_default {
            if overrides.iter().any(|(overridden, _)| overridden == pointer) {
                continue;
            }
            assert_eq!(
                effective.pointer(pointer),
                Some(expected),
                "case {case:?}: {pointer} must keep the system default, got {effective}"
            );
        }
    }
}

/// One row of the grant table: the case, the label the grant carries, the accounts and
/// actions it names, and the `Principal.AWS` and `Action` the resulting statement must
/// carry — one value as a bare string, several as an array.
type GrantCase<'a> = (
    &'a str,
    &'a str,
    &'a [&'a str],
    &'a [&'a str],
    serde_json::Value,
    serde_json::Value,
);

/// AddPermission adds a labelled Allow statement for the named accounts and actions to
/// the topic's access policy, GetTopicAttributes reports the policy with it, and
/// RemovePermission takes that statement back out. The rows differ in how many
/// accounts and actions one grant names, which is what the statement's shape
/// generalises over.
#[tokio::test]
async fn add_permission_grants_and_remove_permission_revokes_a_labelled_statement() {
    let sns = Sns::new();
    let cases: &[GrantCase<'_>] = &[
        (
            "one account, one action",
            "CrossAccountPublish",
            &["111122223333"],
            &["Publish"],
            serde_json::json!("arn:aws:iam::111122223333:root"),
            serde_json::json!("SNS:Publish"),
        ),
        (
            "two accounts, one action",
            "TwoAccounts",
            &["111122223333", "444455556666"],
            &["Publish"],
            serde_json::json!([
                "arn:aws:iam::111122223333:root",
                "arn:aws:iam::444455556666:root"
            ]),
            serde_json::json!("SNS:Publish"),
        ),
        (
            "one account, three actions",
            "ThreeActions",
            &["111122223333"],
            &["Publish", "Subscribe", "GetTopicAttributes"],
            serde_json::json!("arn:aws:iam::111122223333:root"),
            serde_json::json!(["SNS:Publish", "SNS:Subscribe", "SNS:GetTopicAttributes"]),
        ),
    ];

    for (index, (case, label, accounts, actions, principal, action)) in cases.iter().enumerate() {
        let arn = sns.topic(&format!("permissions-{index}")).await;

        // The policy of a topic nobody has granted anything on: SNS's default document.
        let before = attribute_map(&read(&sns, case, &arn).await);
        let before = json_of(case, "Policy", &before["Policy"]);
        assert_eq!(
            before.pointer("/Statement/0/Sid"),
            Some(&serde_json::json!("__default_statement_ID")),
            "case {case:?}: a fresh topic must report the default policy, got {before}"
        );

        let mut params: Vec<(&str, &str)> = vec![("TopicArn", &arn), ("Label", label)];
        let account_params: Vec<(String, &str)> = accounts
            .iter()
            .enumerate()
            .map(|(at, account)| (format!("AWSAccountId.member.{}", at + 1), *account))
            .collect();
        let action_params: Vec<(String, &str)> = actions
            .iter()
            .enumerate()
            .map(|(at, action)| (format!("ActionName.member.{}", at + 1), *action))
            .collect();
        for (name, value) in account_params.iter().chain(action_params.iter()) {
            params.push((name.as_str(), value));
        }
        let answer = sns.call("AddPermission", &params).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: AddPermission expected 200, got {} / {}",
            answer.status, answer.body
        );

        let granted = attribute_map(&read(&sns, case, &arn).await);
        let policy = json_of(case, "Policy", &granted["Policy"]);
        let statements = policy["Statement"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: Policy must carry a Statement list: {policy}"));
        assert_eq!(
            statements.len(),
            2,
            "case {case:?}: the grant must be added beside the default statement, got {policy}"
        );
        let added = statements
            .iter()
            .find(|statement| statement["Sid"] == serde_json::json!(label))
            .unwrap_or_else(|| panic!("case {case:?}: no statement labelled {label}: {policy}"));
        assert_eq!(
            added.get("Effect"),
            Some(&serde_json::json!("Allow")),
            "case {case:?}: the added statement must allow, got {added}"
        );
        assert_eq!(
            added.pointer("/Principal/AWS"),
            Some(principal),
            "case {case:?}: expected the principal {principal}, got {added}"
        );
        assert_eq!(
            added.get("Action"),
            Some(action),
            "case {case:?}: expected the action {action}, got {added}"
        );
        assert_eq!(
            added.get("Resource"),
            Some(&serde_json::json!(arn)),
            "case {case:?}: the grant must be on this topic alone, got {added}"
        );

        // A label identifies ONE statement, so a label the policy already carries one for
        // names a grant that has already been made: a repeat of the SAME grant asks for the
        // state the topic is already in and is served, while a DIFFERENT grant under that
        // label would have to replace the statement and is refused. Either way the policy is
        // left exactly as it was.
        let changed: Vec<(&str, &str)> = vec![
            ("TopicArn", &arn),
            ("Label", label),
            ("AWSAccountId.member.1", "999988887777"),
            ("ActionName.member.1", "Subscribe"),
        ];
        for (repeat, request, expected_status) in [
            ("the same grant", &params, 200),
            ("a different grant", &changed, 400),
        ] {
            let again = sns.call("AddPermission", request).await;
            assert_eq!(
                again.status, expected_status,
                "case {case:?}: a repeated label ({repeat}) expected {expected_status}, got \
                 {} / {}",
                again.status, again.body
            );
            if expected_status == 400 {
                assert_eq!(
                    again.code(),
                    "InvalidParameter",
                    "case {case:?}: a repeated label ({repeat}) expected the InvalidParameter \
                     code, got {}",
                    again.body
                );
            }
            let repeated = attribute_map(&read(&sns, case, &arn).await);
            let repeated = json_of(case, "Policy", &repeated["Policy"]);
            assert_eq!(
                repeated, policy,
                "case {case:?}: a repeated grant ({repeat}) must leave the policy as it \
                 was, got {repeated}"
            );
        }

        // And RemovePermission takes exactly that statement back out.
        let removed = sns
            .call("RemovePermission", &[("TopicArn", &arn), ("Label", label)])
            .await;
        assert_eq!(
            removed.status, 200,
            "case {case:?}: RemovePermission expected 200, got {} / {}",
            removed.status, removed.body
        );
        let after = attribute_map(&read(&sns, case, &arn).await);
        let after = json_of(case, "Policy", &after["Policy"]);
        assert_eq!(
            after.pointer("/Statement/0/Sid"),
            Some(&serde_json::json!("__default_statement_ID")),
            "case {case:?}: the default statement must survive the revoke, got {after}"
        );
        assert_eq!(
            after["Statement"].as_array().map(Vec::len),
            Some(1),
            "case {case:?}: the labelled statement must be gone, got {after}"
        );

        // Revoking a label the policy does not carry asks for a state the topic is
        // already in.
        let twice = sns
            .call("RemovePermission", &[("TopicArn", &arn), ("Label", label)])
            .await;
        assert_eq!(
            twice.status, 200,
            "case {case:?}: a repeated revoke expected 200, got {} / {}",
            twice.status, twice.body
        );
    }
}

/// Editing a caller's access policy keeps the parts of the document this emulator
/// gives no meaning to: a top-level member and a statement member it does not model
/// both survive a grant, beside the statement the grant adds.
#[tokio::test]
async fn a_grant_keeps_the_parts_of_a_caller_policy_it_does_not_model() {
    let sns = Sns::new();
    let case = "a caller policy carrying unmodelled members";
    let arn = sns.topic("policy-passthrough").await;
    let policy = serde_json::json!({
        "Version": "2012-10-17",
        "Id": "caller-policy",
        "UnmodelledDocumentMember": { "kept": true },
        "Statement": [{
            "Sid": "caller-statement",
            "Effect": "Allow",
            "NotPrincipal": { "AWS": "arn:aws:iam::111122223333:root" },
            "Action": "SNS:Publish",
            "Resource": arn,
            "UnmodelledStatementMember": "kept",
        }]
    })
    .to_string();
    set(&sns, case, &arn, "Policy", &policy).await;

    let answer = sns
        .call(
            "AddPermission",
            &[
                ("TopicArn", &arn),
                ("Label", "Grant"),
                ("AWSAccountId.member.1", "111122223333"),
                ("ActionName.member.1", "Publish"),
            ],
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: AddPermission expected 200, got {} / {}",
        answer.status, answer.body
    );

    let held = attribute_map(&read(&sns, case, &arn).await);
    let edited = json_of(case, "Policy", &held["Policy"]);
    assert_eq!(
        edited.get("UnmodelledDocumentMember"),
        Some(&serde_json::json!({ "kept": true })),
        "case {case:?}: a top-level member must survive the grant, got {edited}"
    );
    assert_eq!(
        edited.pointer("/Statement/0/UnmodelledStatementMember"),
        Some(&serde_json::json!("kept")),
        "case {case:?}: a statement member must survive the grant, got {edited}"
    );
    assert_eq!(
        edited.pointer("/Statement/0/NotPrincipal/AWS"),
        Some(&serde_json::json!("arn:aws:iam::111122223333:root")),
        "case {case:?}: a statement's NotPrincipal must survive the grant, got {edited}"
    );
    assert_eq!(
        edited["Statement"].as_array().map(Vec::len),
        Some(2),
        "case {case:?}: the grant must be added beside the caller's statement, got {edited}"
    );
}

/// AddPermission and RemovePermission refuse a topic that does not exist, and
/// AddPermission refuses a principal that is not an account id.
#[tokio::test]
async fn permission_operations_refuse_what_sns_refuses() {
    let sns = Sns::new();
    let arn = sns.topic("permission-refusals").await;
    let missing = format!("{arn}-does-not-exist");

    // (case, account id, expected status, expected error code)
    let cases: &[(&str, &str, u16, &str)] = &[
        ("a 12 digit account id", "111122223333", 200, ""),
        ("an account id that is too short", "11112222333", 400, "InvalidParameter"),
        ("an account id that is too long", "1111222233334", 400, "InvalidParameter"),
        ("an account id that is not digits", "11112222333x", 400, "InvalidParameter"),
        ("an ARN rather than an account id", "arn:aws:iam::111122223333:root", 400, "InvalidParameter"),
    ];

    for (index, (case, account, status, code)) in cases.iter().enumerate() {
        let label = format!("label-{index}");
        let answer = sns
            .call(
                "AddPermission",
                &[
                    ("TopicArn", &arn),
                    ("Label", &label),
                    ("AWSAccountId.member.1", account),
                    ("ActionName.member.1", "Publish"),
                ],
            )
            .await;
        assert_eq!(
            answer.status, *status,
            "case {case:?}: expected {status}, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.code(),
            *code,
            "case {case:?}: expected error code {code:?}, got {}",
            answer.body
        );
    }

    for action in ["AddPermission", "RemovePermission"] {
        let mut params: Vec<(&str, &str)> =
            vec![("TopicArn", &missing), ("Label", "on-a-missing-topic")];
        if action == "AddPermission" {
            params.push(("AWSAccountId.member.1", "111122223333"));
            params.push(("ActionName.member.1", "Publish"));
        }
        let answer = sns.call(action, &params).await;
        assert_eq!(
            answer.status, 404,
            "case {action:?}: a topic that does not exist expected 404, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.code(),
            "NotFound",
            "case {action:?}: expected NotFound, got {}",
            answer.body
        );
    }
}

// =============================================================================
// A grant is decided by its own parameters, not by what the policy already holds
// =============================================================================

/// `AddPermission` on `arn`, granting `accounts` the `actions` under `label`.
async fn grant(sns: &Sns, arn: &str, label: &str, accounts: &[&str], actions: &[&str]) -> Answer {
    let members: Vec<(String, &str)> = accounts
        .iter()
        .enumerate()
        .map(|(at, account)| (format!("AWSAccountId.member.{}", at + 1), *account))
        .chain(
            actions
                .iter()
                .enumerate()
                .map(|(at, action)| (format!("ActionName.member.{}", at + 1), *action)),
        )
        .collect();
    let mut params: Vec<(&str, &str)> = vec![("TopicArn", arn), ("Label", label)];
    for (name, value) in &members {
        params.push((name.as_str(), value));
    }
    sns.call("AddPermission", &params).await
}

/// The envelope a served `AddPermission` answers with: the operation's output is `Unit`,
/// so the document carries the response element and its one request id and NO result
/// element.
fn served_envelope(case: &str, answer: &Answer) {
    assert!(
        answer.body.contains("<AddPermissionResponse"),
        "case {case:?}: the document must carry the AddPermissionResponse element, got {}",
        answer.body
    );
    // The output is `Unit`, so the envelope carries no member of its own: the result
    // element the awsQuery protocol wraps a payload in is empty wherever it is written.
    assert!(
        answer.body.contains("<AddPermissionResult></AddPermissionResult>")
            || !answer.body.contains("AddPermissionResult"),
        "case {case:?}: a Unit output carries no member, got {}",
        answer.body
    );
    let ids = answer.elements("RequestId");
    assert_eq!(
        ids.len(),
        1,
        "case {case:?}: the envelope carries exactly one request id, got {ids:?} in {}",
        answer.body
    );
    assert!(
        !ids[0].is_empty(),
        "case {case:?}: the request id must name the request, got {:?}",
        ids[0]
    );
}

/// The access policy document the topic `arn` reports.
async fn policy_of(sns: &Sns, case: &str, arn: &str) -> serde_json::Value {
    let held = attribute_map(&read(sns, case, arn).await);
    let value = held
        .get("Policy")
        .unwrap_or_else(|| panic!("case {case:?}: no Policy attribute was reported, got {held:?}"));
    json_of(case, "Policy", value)
}

/// Every statement of `policy`, in document order: the policy grammar admits ONE
/// statement as a bare object and several as a list, and a caller's document is reported
/// in the form it was written in.
fn statements(case: &str, policy: &serde_json::Value) -> Vec<serde_json::Value> {
    match &policy["Statement"] {
        serde_json::Value::Array(list) => list.clone(),
        one @ serde_json::Value::Object(_) => vec![one.clone()],
        _ => panic!("case {case:?}: a policy document carries a Statement, got {policy}"),
    }
}

/// The `Sid` of every statement of `policy`, in document order.
fn statement_labels(case: &str, policy: &serde_json::Value) -> Vec<String> {
    statements(case, policy)
        .iter()
        .map(|statement| statement["Sid"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// A statement member that holds ONE value bare and several as an array — the shape SNS
/// writes `Principal` and `Action` in — read as a list either way.
fn value_list(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::String(one)) => vec![one.clone()],
        Some(serde_json::Value::Array(many)) => many
            .iter()
            .map(|entry| entry.as_str().unwrap_or_default().to_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// Assert the statement labelled `label` is the `Allow` the grant of `accounts` on
/// `actions` of the topic `arn` describes.
fn granted_statement(
    case: &str,
    policy: &serde_json::Value,
    label: &str,
    arn: &str,
    accounts: &[&str],
    actions: &[&str],
) {
    let added = statements(case, policy)
        .into_iter()
        .find(|statement| statement["Sid"] == serde_json::json!(label))
        .unwrap_or_else(|| panic!("case {case:?}: no statement labelled {label}: {policy}"));
    assert_eq!(
        added.get("Effect"),
        Some(&serde_json::json!("Allow")),
        "case {case:?}: the granted statement must allow, got {added}"
    );
    // An account id is written as its root user's ARN, the principal form a policy names
    // a delegated account by.
    let expected_principals: Vec<String> = accounts
        .iter()
        .map(|account| format!("arn:aws:iam::{account}:root"))
        .collect();
    assert_eq!(
        value_list(added.pointer("/Principal/AWS")),
        expected_principals,
        "case {case:?}: expected the principals {expected_principals:?}, got {added}"
    );
    let expected_actions: Vec<String> = actions
        .iter()
        .map(|action| format!("SNS:{action}"))
        .collect();
    assert_eq!(
        value_list(added.get("Action")),
        expected_actions,
        "case {case:?}: expected the actions {expected_actions:?}, got {added}"
    );
    assert_eq!(
        added.get("Resource"),
        Some(&serde_json::json!(arn)),
        "case {case:?}: the grant must be on this topic alone, got {added}"
    );
    // One value is written bare and several as an array, whichever member carries them.
    assert_eq!(
        added.pointer("/Principal/AWS").map(serde_json::Value::is_string),
        Some(accounts.len() == 1),
        "case {case:?}: one principal is written bare and several as an array, got {added}"
    );
    assert_eq!(
        added.get("Action").map(serde_json::Value::is_string),
        Some(actions.len() == 1),
        "case {case:?}: one action is written bare and several as an array, got {added}"
    );
}

/// One row of the policy-state table: the case, the document the topic's `Policy`
/// attribute holds when the grant arrives (`None` for a topic whose owner has set none),
/// the `Sid` of every statement that document carries, and the accounts and the actions
/// the grant names.
///
/// `<TOPIC>` and `<ACCOUNT>` inside a document stand for the topic's ARN and the account
/// that owns it, which the create mints and a literal therefore cannot carry.
type PolicyStateCase<'a> = (
    &'a str,
    Option<&'a str>,
    &'a [&'a str],
    &'a [&'a str],
    &'a [&'a str],
);

/// AddPermission adds one labelled `Allow` statement to the addressed topic's access
/// policy and answers 200 with the empty response envelope whenever its own parameters
/// comply with their constraints — WHATEVER document that policy already holds, which is
/// no parameter of the request. The rows differ along the form of that document, which is
/// what the rule generalises over: the one SNS generates for a topic with no policy, one
/// its owner set, one whose `Statement` is a single object rather than a list (both forms
/// the policy grammar admits), one carrying several statements, one stating neither
/// `Version` nor `Id`, and one carrying no statement at all — and in how many accounts
/// and actions one grant names. Every statement the document already held survives the
/// grant.
#[tokio::test]
async fn add_permission_is_served_whatever_policy_document_the_topic_holds() {
    let sns = Sns::new();
    let label = "CrossAccountAccess";
    let rows: &[PolicyStateCase<'_>] = &[
        (
            "the document SNS generates for a topic whose owner set none",
            None,
            &["__default_statement_ID"],
            &["111122223333"],
            &["Publish"],
        ),
        (
            "a document the owner set in place of the default",
            Some(
                r#"{"Version":"2008-10-17","Id":"owner-policy","Statement":[{"Sid":"owner-reads",
                "Effect":"Allow","Principal":{"AWS":"*"},"Action":"SNS:GetTopicAttributes",
                "Resource":"<TOPIC>","Condition":{"StringEquals":{"AWS:SourceOwner":"<ACCOUNT>"}}}]}"#,
            ),
            &["owner-reads"],
            &["111122223333"],
            &["Publish", "Subscribe"],
        ),
        (
            "a document whose Statement is one object rather than a list",
            Some(
                r#"{"Version":"2008-10-17","Statement":{"Sid":"single-object","Effect":"Allow",
                "Principal":{"AWS":"arn:aws:iam::444455556666:root"},"Action":"SNS:Publish",
                "Resource":"<TOPIC>"}}"#,
            ),
            &["single-object"],
            &["111122223333", "444455556666"],
            &["Publish"],
        ),
        (
            "a document carrying several statements",
            Some(
                r#"{"Version":"2008-10-17","Statement":[
                {"Sid":"first","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::111122223333:root"},
                 "Action":"SNS:Subscribe","Resource":"<TOPIC>"},
                {"Sid":"second","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::444455556666:root"},
                 "Action":["SNS:Publish","SNS:GetTopicAttributes"],"Resource":"<TOPIC>"},
                {"Sid":"third","Effect":"Allow","Principal":{"AWS":"*"},
                 "Action":"SNS:ListSubscriptionsByTopic","Resource":"<TOPIC>",
                 "Condition":{"StringEquals":{"AWS:SourceOwner":"<ACCOUNT>"}}}]}"#,
            ),
            &["first", "second", "third"],
            &["111122223333", "444455556666"],
            &["Publish", "Subscribe", "GetTopicAttributes"],
        ),
        (
            "a document stating neither Version nor Id",
            Some(
                r#"{"Statement":[{"Sid":"bare","Effect":"Allow",
                "Principal":{"AWS":"arn:aws:iam::111122223333:root"},"Action":"SNS:Publish",
                "Resource":"<TOPIC>"}]}"#,
            ),
            &["bare"],
            &["999988887777"],
            &["Publish"],
        ),
        (
            "a document carrying no statement at all",
            Some(r#"{"Version":"2008-10-17","Statement":[]}"#),
            &[],
            &["111122223333"],
            &["ListSubscriptionsByTopic"],
        ),
    ];

    for (index, (case, document, held, accounts, actions)) in rows.iter().enumerate() {
        let arn = sns.topic(&format!("grant-onto-{index:02}")).await;
        if let Some(document) = document {
            let document = document
                .replace("<TOPIC>", &arn)
                .replace("<ACCOUNT>", &account_of(&arn));
            set(&sns, case, &arn, "Policy", &document).await;
        }
        // What the policy carries BEFORE the grant, which the grant must leave in place.
        let before = policy_of(&sns, case, &arn).await;
        let expected_before: Vec<String> = held.iter().map(|sid| (*sid).to_string()).collect();
        assert_eq!(
            statement_labels(case, &before),
            expected_before,
            "case {case:?}: the policy the grant lands on must carry the statements it was \
             given, got {before}"
        );

        let answer = grant(&sns, &arn, label, accounts, actions).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: AddPermission expected 200, got {} / {}",
            answer.status, answer.body
        );
        served_envelope(case, &answer);

        let granted = policy_of(&sns, case, &arn).await;
        // Whatever form the document was written in, the one the grant leaves behind
        // carries its statements as a LIST — the form SNS answers with.
        assert!(
            granted["Statement"].is_array(),
            "case {case:?}: the edited document must carry a Statement list, got {granted}"
        );
        let mut expected_after = expected_before.clone();
        expected_after.push(label.to_string());
        assert_eq!(
            statement_labels(case, &granted),
            expected_after,
            "case {case:?}: the grant must be added beside the statements the policy held, \
             got {granted}"
        );
        granted_statement(case, &granted, label, &arn, accounts, actions);
    }
}

/// One row of the re-grant table: the case, the accounts and actions of the first grant,
/// and those of the grant made under the SAME label after that one was revoked.
type RegrantCase<'a> = (
    &'a str,
    &'a [&'a str],
    &'a [&'a str],
    &'a [&'a str],
    &'a [&'a str],
);

/// A `Label` is refused as a duplicate only while the policy carries a DIFFERENT
/// statement under it: once that statement is revoked the label names no grant, so a
/// grant under it is served again and writes the statement the new request describes —
/// and a grant under a label the policy never carried is served beside it. The rows
/// differ in how the re-grant differs from the grant it follows.
#[tokio::test]
async fn a_revoked_label_can_be_granted_again() {
    let sns = Sns::new();
    let label = "Delegated";
    // A grant that differs from every row's, so it conflicts with whichever statement the
    // label carries at the time.
    let other_accounts: &[&str] = &["000011112222"];
    let other_actions: &[&str] = &["ListSubscriptionsByTopic"];
    let rows: &[RegrantCase<'_>] = &[
        (
            "the same grant again",
            &["111122223333"],
            &["Publish"],
            &["111122223333"],
            &["Publish"],
        ),
        (
            "more accounts than before",
            &["111122223333"],
            &["Publish"],
            &["111122223333", "444455556666"],
            &["Publish"],
        ),
        (
            "other actions than before",
            &["111122223333"],
            &["Publish", "Subscribe"],
            &["444455556666"],
            &["GetTopicAttributes"],
        ),
    ];

    for (index, (case, first_accounts, first_actions, next_accounts, next_actions)) in
        rows.iter().enumerate()
    {
        let arn = sns.topic(&format!("regrant-{index:02}")).await;
        let answer = grant(&sns, &arn, label, first_accounts, first_actions).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: the first grant expected 200, got {} / {}",
            answer.status, answer.body
        );
        served_envelope(case, &answer);
        let granted = policy_of(&sns, case, &arn).await;
        granted_statement(case, &granted, label, &arn, first_accounts, first_actions);

        // While that statement is there, a DIFFERENT grant under its label is refused and
        // the policy is left exactly as it was.
        let conflicting = grant(&sns, &arn, label, other_accounts, other_actions).await;
        assert_eq!(
            conflicting.status, 400,
            "case {case:?}: a different grant under a live label expected 400, got {} / {}",
            conflicting.status, conflicting.body
        );
        assert_eq!(
            conflicting.code(),
            "InvalidParameter",
            "case {case:?}: expected the InvalidParameter code, got {}",
            conflicting.body
        );
        assert_eq!(
            policy_of(&sns, case, &arn).await,
            granted,
            "case {case:?}: the refused grant must leave the policy as it was"
        );

        let revoked = sns
            .call("RemovePermission", &[("TopicArn", &arn), ("Label", label)])
            .await;
        assert_eq!(
            revoked.status, 200,
            "case {case:?}: RemovePermission expected 200, got {} / {}",
            revoked.status, revoked.body
        );

        // The label now names no statement, so the grant is served and the policy carries
        // what THIS request described.
        let answer = grant(&sns, &arn, label, next_accounts, next_actions).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: the grant after the revoke expected 200, got {} / {}",
            answer.status, answer.body
        );
        served_envelope(case, &answer);
        let regranted = policy_of(&sns, case, &arn).await;
        granted_statement(case, &regranted, label, &arn, next_accounts, next_actions);

        // And a label the policy never carried is a grant of its own beside it.
        let second = "AlsoDelegated";
        let answer = grant(&sns, &arn, second, other_accounts, other_actions).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: a grant under a second label expected 200, got {} / {}",
            answer.status, answer.body
        );
        let both = policy_of(&sns, case, &arn).await;
        assert_eq!(
            statement_labels(case, &both),
            vec![
                "__default_statement_ID".to_string(),
                label.to_string(),
                second.to_string(),
            ],
            "case {case:?}: both grants must stand beside the default statement, got {both}"
        );
        granted_statement(case, &both, label, &arn, next_accounts, next_actions);
        granted_statement(case, &both, second, &arn, other_accounts, other_actions);
    }
}

/// One topic's policy takes one statement per granted label: each grant is served and the
/// document ends up carrying every label beside the statement SNS generated, so an
/// accumulating policy is not what decides a grant.
#[tokio::test]
async fn one_topic_takes_a_statement_per_granted_label() {
    let sns = Sns::new();
    let case = "a label per account of a fan-out";
    let arn = sns.topic("many-grants").await;
    let accounts: Vec<String> = (0..25).map(|at| format!("1111222233{at:02}")).collect();

    for account in &accounts {
        let label = format!("Grant-{account}");
        let answer = grant(&sns, &arn, &label, &[account.as_str()], &["Publish"]).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: AddPermission {label} expected 200, got {} / {}",
            answer.status, answer.body
        );
        served_envelope(case, &answer);
    }

    let policy = policy_of(&sns, case, &arn).await;
    let held = statement_labels(case, &policy);
    assert_eq!(
        held.len(),
        accounts.len() + 1,
        "case {case:?}: expected the default statement and one per label, got {held:?}"
    );
    for account in &accounts {
        let label = format!("Grant-{account}");
        assert!(
            held.contains(&label),
            "case {case:?}: {label} must be a statement of the policy, got {held:?}"
        );
        granted_statement(case, &policy, &label, &arn, &[account.as_str()], &["Publish"]);
    }
}

/// A grant may name any action of this service's POLICY scope and nothing else: every
/// operation the model binds, and the one action this service defines as a policy action
/// alone, which names no operation a caller could ever invoke (`state::ACTION_NAMES`,
/// read off `smithy_query(query="list_operations")` plus that action). Each is written
/// into the policy as `SNS:<action>`. A name outside the scope — another service's
/// action, one cased differently, or a name no action of this service carries — is
/// refused `400 InvalidParameter` with SNS's own sentence and writes nothing.
#[tokio::test]
async fn a_grant_names_an_action_of_this_services_policy_scope() {
    let sns = Sns::new();
    let arn = sns.topic("action-scope").await;
    let account = "111122223333";

    for action in ACTION_NAMES {
        let answer = grant(&sns, &arn, action, &[account], &[action]).await;
        assert_eq!(
            answer.status, 200,
            "action {action:?}: expected 200, got {} / {}",
            answer.status, answer.body
        );
        served_envelope(action, &answer);
    }
    let policy = policy_of(&sns, "the whole policy scope", &arn).await;
    for action in ACTION_NAMES {
        granted_statement(action, &policy, action, &arn, &[account], &[action]);
    }
    // The action that names no operation of this service: a scope read off the operations
    // alone would refuse the grant AWS applies.
    assert!(
        policy.to_string().contains(&format!("SNS:{RECEIVE_ACTION}")),
        "the policy-only action must be written into the policy like any other, got {policy}"
    );

    // (case, the name the grant asks for)
    let outside: &[(&str, &str)] = &[
        ("another service's action", "sqs:SendMessage"),
        ("an action of this service, lower-cased", "publish"),
        ("a name no action of this service carries", "ReceiveNotifications"),
        ("a near miss of the policy-only action", "Receives"),
        ("no name at all", ""),
    ];
    for (case, action) in outside {
        let answer = grant(&sns, &arn, "outside-the-scope", &[account], &[action]).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected 400, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.code(),
            "InvalidParameter",
            "case {case:?}: expected the InvalidParameter code, got {}",
            answer.body
        );
        assert_eq!(
            answer.element("Message").as_deref(),
            Some("Invalid parameter: Policy statement action out of service scope!"),
            "case {case:?}: unexpected sentence, got {}",
            answer.body
        );
    }
    assert_eq!(
        policy_of(&sns, "after the refusals", &arn).await,
        policy,
        "a refused grant must leave the policy exactly as it was"
    );
}
