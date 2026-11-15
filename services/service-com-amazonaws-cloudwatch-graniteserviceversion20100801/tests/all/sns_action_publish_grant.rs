// SPDX-License-Identifier: CC0-1.0

//! The GRANT an alarm's SNS action needs before anything is published to its topic, end to end
//! over the wire with CloudWatch, SNS, SQS and AWS KMS on one engine — so what a client sees is
//! what is asserted: the `Action` item the alarm's history records, and the depth of the queue
//! subscribed to the topic.
//!
//! The RULE asserted here: executing an alarm action is a cross-service request this service
//! makes as the service principal `cloudwatch.amazonaws.com` on behalf of the alarm whose
//! transition fired it, so the documents in force on the DESTINATION decide it and the publish
//! is made only where each of them ALLOWS it — the topic's own access policy for `SNS:Publish`
//! on that topic, and, for a topic that encrypts its messages under a customer managed key,
//! that key's policy for the operations sealing one message (`kms:GenerateDataKey` and
//! `kms:Decrypt`). A document that states no such grant withholds the publish exactly as an
//! explicit deny does: the topic's subscribers see nothing and the action is recorded with
//! `actionState` `Failed`, an `error` naming the authorization failure, the resource that
//! withheld it and the reading that refused it, and no `publishedMessage`.
//!
//! Two tables, each over what the rule generalises over:
//!
//! 1. [`every_state_s_action_list_needs_the_publish_grant`] — the ACTION LIST the transition
//!    fires (`AlarmActions`, `OKActions`, `InsufficientDataActions`), against a topic policy
//!    that grants this principal the publish and one that is silent about it. The rule is about
//!    the publish rather than about which state fired it, so every list reads it the same way.
//! 2. [`an_encrypted_topics_key_policy_decides_the_alarm_action`] — the FORM the key policy
//!    takes: the one a new key carries, an allow of this principal for both sealing operations,
//!    for one of them only, an allow of every principal, an allow of someone else, an allow
//!    scoped by the firing alarm and one scoped to another alarm, one scoped by the service the
//!    key is reached through, an allow with an explicit deny beside it, and a topic on no key at
//!    all.
//!
//! [`crate::sns_action_topic_policy`] tables the topic-policy form itself; what only this suite
//! reaches is the publish grant for each state's action list and the second document an
//! ENCRYPTED topic puts in force.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    element, given_queue, grant_sns_delivery_on, rpc, set_alarm_state, sns, subscribe_queue,
    Emulator, REGION, TARGET_PREFIX,
};
use crate::sns_action_topic_policy::{
    action_items, alarm_arn, assert_action_item, delivered, poll_delivered, policy, statement,
    Outcome, CLOUDWATCH_PRINCIPAL, EXPLICIT_DENY, NO_ALLOW,
};

/// AWS KMS's `X-Amz-Target` prefix and the protocol version its model declares.
const KMS_TARGET_PREFIX: &str = "TrentService";
const KMS_CONTENT_TYPE: &str = "application/x-amz-json-1.1";

/// The topic attribute naming the customer managed key a topic seals its messages under.
const KMS_MASTER_KEY_ID: &str = "KmsMasterKeyId";

/// The two KMS operations sealing ONE message performs, in the order the service asks the key
/// policy about them: the data key the message is encrypted under is generated, and a data key
/// already in use is decrypted to seal another message under it.
const GENERATE_DATA_KEY: &str = "kms:GenerateDataKey";
const DECRYPT: &str = "kms:Decrypt";

/// One emulator with CloudWatch, SNS, SQS and AWS KMS mounted on one engine.
///
/// KMS is mounted beside the three the alarm-action suites need because an ENCRYPTED topic's
/// key, its manager and its key policy are KMS's own facts: the decision is taken on the
/// document `PutKeyPolicy` left on the key rather than on anything this suite holds. Shared with
/// the suite that drives the OTHER decision this service takes over a key
/// (`dataset_kms_key_preconditions`), so one emulator mounts KMS beside CloudWatch once.
pub(crate) fn emulator_with_kms() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's concern, not this service's.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
    );
    Emulator { router, engine }
}

/// One `awsJson1_1` call against AWS KMS, which must answer 200.
pub(crate) async fn kms(emulator: &Emulator, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = kms_call(emulator, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from KMS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// The same call with its HTTP status and body as answered — the seam for the test that drives
/// KMS into a REFUSAL and reads what it said.
async fn kms_call(emulator: &Emulator, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("kms.{REGION}.amazonaws.com"))
        .header("content-type", KMS_CONTENT_TYPE)
        .header("x-amz-target", format!("{KMS_TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/kms/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A customer managed key, as `CreateKey` mints it: its ARN.
async fn given_key(emulator: &Emulator, case: &str) -> String {
    let created = kms(
        emulator,
        case,
        "CreateKey",
        &json!({"Description": "the key an encrypted topic seals its notifications under"}),
    )
    .await;
    created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateKey reported no Arn: {created}"))
        .to_string()
}

/// Replace the key's own policy, the way its owner does.
pub(crate) async fn set_key_policy(emulator: &Emulator, case: &str, key_arn: &str, document: &str) {
    kms(
        emulator,
        case,
        "PutKeyPolicy",
        &json!({"KeyId": key_arn, "PolicyName": "default", "Policy": document}),
    )
    .await;
}

/// A KMS GRANT cannot name this service principal at all, which is why the key POLICY is the
/// only authority the decision above reads: "to specify the grantee principal, use the Amazon
/// Resource Name (ARN) of an Amazon Web Services principal"
/// (`CreateGrantRequest$GranteePrincipal`), and a service principal is not one — so there is no
/// grant for the alarm action's publish to draw permission from, however the key's owner writes
/// it.
#[tokio::test]
async fn a_grant_cannot_name_the_service_principal_an_alarm_action_publishes_as() {
    let emulator = emulator_with_kms();
    let case = "a grant to the service principal an alarm action publishes as";
    let key_arn = given_key(&emulator, case).await;
    let (status, answered) = kms_call(
        &emulator,
        "CreateGrant",
        &json!({
            "KeyId": key_arn,
            "GranteePrincipal": CLOUDWATCH_PRINCIPAL,
            "Operations": ["GenerateDataKey", "Decrypt"],
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: KMS must refuse the grant, got {status} with {answered}"
    );
    let code = answered["__type"].as_str().unwrap_or_default();
    assert!(
        code.ends_with("InvalidArnException"),
        "case {case:?}: expected the refusal KMS answers a grantee that is no principal ARN, \
         got {code:?} with {answered}"
    );
}

/// The statement every key policy keeps: "Enable IAM User Permissions", the one a new key
/// carries, which names this account's root and is what keeps the key manageable — AWS's own
/// key-policy examples add a service statement BESIDE it rather than in place of it.
pub(crate) fn account_root_statement() -> Value {
    json!({
        "Sid": "EnableIAMUserPermissions",
        "Effect": "Allow",
        "Principal": {"AWS": format!("arn:aws:iam::{}:root", engine::EMULATOR_ACCOUNT_ID)},
        "Action": "kms:*",
        "Resource": "*",
    })
}

/// One key-policy statement: a key policy's `Resource` element is `*`, which means the key the
/// policy is attached to.
pub(crate) fn key_statement(effect: &str, principal: Value, actions: Value, extra: Value) -> Value {
    let mut written = json!({
        "Sid": format!("{effect}SealingStatement"),
        "Effect": effect,
        "Principal": principal,
        "Action": actions,
        "Resource": "*",
    });
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        written[member] = value;
    }
    written
}

/// A whole key policy carrying `statements` beside the statement a new key already has.
pub(crate) fn key_policy(statements: Vec<Value>) -> String {
    let mut all = vec![account_root_statement()];
    all.extend(statements);
    json!({"Version": "2012-10-17", "Id": "key-policy-1", "Statement": all}).to_string()
}

/// A topic whose access policy grants this service the publish, with a queue subscribed to it:
/// the topic's ARN and the queue's url.
///
/// The grant is the one AWS documents for an alarm action, so the TOPIC's policy is never what
/// withholds a publish in the key table below.
async fn given_permitted_topic(emulator: &Emulator, name: &str) -> (String, String) {
    let topic = element(
        &sns(emulator, "CreateTopic", &[("Name", name)]).await,
        "TopicArn",
    );
    let (url, queue_arn) = given_queue(emulator, &format!("{name}-subscriber")).await;
    grant_sns_delivery_on(emulator, &url, &queue_arn).await;
    subscribe_queue(emulator, &topic, &queue_arn).await;
    set_topic_attribute(
        emulator,
        &topic,
        "Policy",
        &policy(vec![statement(
            "Allow",
            json!({"Service": CLOUDWATCH_PRINCIPAL}),
            json!("sns:Publish"),
            json!({}),
        )]),
    )
    .await;
    (topic, url)
}

/// Write one attribute of the topic, the way its owner does.
async fn set_topic_attribute(emulator: &Emulator, topic_arn: &str, name: &str, value: &str) {
    sns(
        emulator,
        "SetTopicAttributes",
        &[
            ("TopicArn", topic_arn),
            ("AttributeName", name),
            ("AttributeValue", value),
        ],
    )
    .await;
}

/// An alarm over a customer metric whose `member` action list is the topic `topic_arn`.
///
/// ONE list per alarm, so the transition a row drives fires exactly that list and the action
/// item the row reads is the one it fired.
async fn given_alarm_with_actions(
    emulator: &Emulator,
    name: &str,
    member: &str,
    topic_arn: &str,
) {
    rpc(
        emulator,
        "monitoring",
        TARGET_PREFIX,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": "Custom/Orders",
            "MetricName": "Backlog",
            "Dimensions": [{"Name": "QueueName", "Value": name}],
            "Statistic": "Average",
            "Period": 60,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanOrEqualToThreshold",
            member: [topic_arn],
        }),
    )
    .await;
}

#[tokio::test]
async fn every_state_s_action_list_needs_the_publish_grant() {
    // (the action list, the states forced to fire it — the last one is the transition that
    // fires, the ones before it move the alarm to where that transition starts from)
    let lists: [(&str, &[&str]); 3] = [
        ("AlarmActions", &["ALARM"]),
        ("OKActions", &["OK"]),
        // A new alarm is already INSUFFICIENT_DATA, so it is taken to OK first — which fires
        // nothing, because this row configures no OK action — and then back.
        ("InsufficientDataActions", &["OK", "INSUFFICIENT_DATA"]),
    ];
    // (whether the topic's policy grants this principal the publish, what the action records)
    let grants: [(bool, Outcome); 2] = [
        (true, Outcome::Published),
        (false, Outcome::Withheld(NO_ALLOW)),
    ];

    for (member, states) in lists {
        for (granted, outcome) in grants {
            let case = format!("{member} with the grant: {granted}");
            let emulator = emulator_with_kms();
            let (topic, queue) = given_permitted_topic(&emulator, "orders").await;
            if !granted {
                // A least-privilege policy that grants the publish to ANOTHER principal: the
                // one shape of a policy that states nothing about this publish at all.
                set_topic_attribute(
                    &emulator,
                    &topic,
                    "Policy",
                    &policy(vec![statement(
                        "Allow",
                        json!({"Service": "events.amazonaws.com"}),
                        json!("sns:Publish"),
                        json!({}),
                    )]),
                )
                .await;
            }
            let alarm = "orders-backlog";
            given_alarm_with_actions(&emulator, alarm, member, &topic).await;
            for state in states {
                set_alarm_state(&emulator, alarm, state).await;
            }

            let items = action_items(&emulator, alarm).await;
            assert_eq!(
                items.len(),
                1,
                "case {case:?}: the one action the transition fired must record one Action \
                 item, got {items:?}"
            );
            let (resource, item) = &items[0];
            assert_eq!(
                resource, &topic,
                "case {case:?}: the item must name the topic the action aimed at, got {item}"
            );
            assert_action_item(&case, item, &topic, outcome);

            let wanted = outcome.deliveries();
            let waiting = poll_delivered(&emulator, &queue, wanted).await;
            assert_eq!(
                waiting, wanted,
                "case {case:?}: expected {wanted} notification(s) on the subscribed queue, got \
                 {waiting}"
            );
        }
    }
}

/// What the key the topic seals its messages under said about this publish.
#[derive(Debug, Clone, Copy)]
enum Sealing {
    /// The message was sealed and published.
    Sealed,
    /// No statement of the key policy allows this principal the named operation.
    NoAllow(&'static str),
    /// An explicit deny of the named operation refuses it.
    Denied(&'static str),
}

/// The key policy a row writes, built from the ARN of the alarm whose transition publishes — so
/// a row may scope its statement to that alarm. `None` for the row whose topic carries no key.
type KeyPolicyFor = Box<dyn Fn(&str) -> Option<String>>;

/// A row whose topic encrypts under a key carrying the statements `build` writes.
fn with_key(build: impl Fn(&str) -> Vec<Value> + 'static) -> KeyPolicyFor {
    Box::new(move |alarm| Some(key_policy(build(alarm))))
}

/// A row whose topic encrypts nothing: it carries no key at all.
fn without_key() -> KeyPolicyFor {
    Box::new(|_alarm| None)
}

#[tokio::test]
async fn an_encrypted_topics_key_policy_decides_the_alarm_action() {
    // (case, the key policy the row writes, what the key said about the publish)
    let rows: Vec<(&str, KeyPolicyFor, Sealing)> = vec![
        (
            "a topic on no key at all",
            without_key(),
            Sealing::Sealed,
        ),
        (
            "the policy a new key carries, naming only the account root",
            with_key(|_alarm| Vec::new()),
            Sealing::NoAllow(GENERATE_DATA_KEY),
        ),
        (
            "an allow of this service principal for both sealing operations",
            with_key(move |_alarm| {
                vec![key_statement(
                    "Allow",
                    json!({"Service": CLOUDWATCH_PRINCIPAL}),
                    json!([DECRYPT, "kms:GenerateDataKey*"]),
                    json!({}),
                )]
            }),
            Sealing::Sealed,
        ),
        (
            "an allow of every principal for every operation of the service",
            with_key(|_alarm| {
                vec![key_statement(
                    "Allow",
                    json!("*"),
                    json!("kms:*"),
                    json!({}),
                )]
            }),
            Sealing::Sealed,
        ),
        (
            "an allow scoped to the alarm this publish comes from",
            with_key(move |alarm| {
                vec![key_statement(
                    "Allow",
                    json!({"Service": CLOUDWATCH_PRINCIPAL}),
                    json!([DECRYPT, "kms:GenerateDataKey*"]),
                    json!({"Condition": {"ArnEquals": {"aws:SourceArn": alarm}}}),
                )]
            }),
            Sealing::Sealed,
        ),
        (
            "an allow scoped to another alarm",
            with_key(move |alarm| {
                vec![key_statement(
                    "Allow",
                    json!({"Service": CLOUDWATCH_PRINCIPAL}),
                    json!([DECRYPT, "kms:GenerateDataKey*"]),
                    json!({
                        "Condition": {
                            "ArnEquals": {"aws:SourceArn": format!("{alarm}-elsewhere")}
                        }
                    }),
                )]
            }),
            Sealing::NoAllow(GENERATE_DATA_KEY),
        ),
        (
            "an allow scoped by the service the key is reached through",
            with_key(move |_alarm| {
                vec![key_statement(
                    "Allow",
                    json!({"Service": CLOUDWATCH_PRINCIPAL}),
                    json!([DECRYPT, "kms:GenerateDataKey*"]),
                    json!({
                        "Condition": {
                            "StringEquals": {
                                "kms:ViaService": format!("sns.{REGION}.amazonaws.com")
                            }
                        }
                    }),
                )]
            }),
            Sealing::Sealed,
        ),
        (
            "an allow of the decrypt operation only",
            with_key(|_alarm| {
                vec![key_statement(
                    "Allow",
                    json!({"Service": CLOUDWATCH_PRINCIPAL}),
                    json!(DECRYPT),
                    json!({}),
                )]
            }),
            Sealing::NoAllow(GENERATE_DATA_KEY),
        ),
        (
            "an allow of the data-key operation only",
            with_key(|_alarm| {
                vec![key_statement(
                    "Allow",
                    json!({"Service": CLOUDWATCH_PRINCIPAL}),
                    json!("kms:GenerateDataKey*"),
                    json!({}),
                )]
            }),
            Sealing::NoAllow(DECRYPT),
        ),
        (
            "an allow of another service principal only",
            with_key(|_alarm| {
                vec![key_statement(
                    "Allow",
                    json!({"Service": "events.amazonaws.com"}),
                    json!("kms:*"),
                    json!({}),
                )]
            }),
            Sealing::NoAllow(GENERATE_DATA_KEY),
        ),
        (
            "an allow of this principal with an explicit deny of the decrypt beside it",
            with_key(move |_alarm| {
                vec![
                    key_statement(
                        "Allow",
                        json!({"Service": CLOUDWATCH_PRINCIPAL}),
                        json!([DECRYPT, "kms:GenerateDataKey*"]),
                        json!({}),
                    ),
                    key_statement(
                        "Deny",
                        json!({"Service": CLOUDWATCH_PRINCIPAL}),
                        json!(DECRYPT),
                        json!({}),
                    ),
                ]
            }),
            Sealing::Denied(DECRYPT),
        ),
    ];

    for (case, document, sealing) in rows {
        let emulator = emulator_with_kms();
        let (topic, queue) = given_permitted_topic(&emulator, "orders").await;
        let alarm = "orders-backlog-high";
        given_alarm_with_actions(&emulator, alarm, "AlarmActions", &topic).await;
        let arn = alarm_arn(&emulator, alarm).await;
        let key_arn = match document(&arn) {
            Some(written) => {
                let key_arn = given_key(&emulator, case).await;
                set_key_policy(&emulator, case, &key_arn, &written).await;
                set_topic_attribute(&emulator, &topic, KMS_MASTER_KEY_ID, &key_arn).await;
                key_arn
            }
            None => String::new(),
        };
        set_alarm_state(&emulator, alarm, "ALARM").await;

        let items = action_items(&emulator, alarm).await;
        assert_eq!(
            items.len(),
            1,
            "case {case:?}: the one action executed must record one Action item, got {items:?}"
        );
        let (_, item) = &items[0];
        let wanted = assert_sealing(case, item, &key_arn, sealing);
        let waiting = poll_delivered(&emulator, &queue, wanted).await;
        assert_eq!(
            waiting, wanted,
            "case {case:?}: expected {wanted} notification(s) on the subscribed queue, got \
             {waiting}"
        );
        if wanted == 0 {
            // Nothing arrives later either: the publish was never made.
            let late = delivered(&emulator, &queue).await;
            assert_eq!(
                late, 0,
                "case {case:?}: a withheld publish delivers nothing at all, got {late}"
            );
        }
    }
}

/// Assert one `Action` item against what the topic's key said about the publish, and answer how
/// many notifications the subscribed queue is owed.
fn assert_sealing(case: &str, item: &Value, key_arn: &str, sealing: Sealing) -> usize {
    let (operation, reason) = match sealing {
        Sealing::Sealed => {
            assert_eq!(
                item["actionState"].as_str(),
                Some("Succeeded"),
                "case {case:?}: a publish the key permitted is Succeeded, got {item}"
            );
            assert_eq!(
                item["error"].as_str(),
                None,
                "case {case:?}: a published action records no error, got {item}"
            );
            assert!(
                item["publishedMessage"]
                    .as_str()
                    .is_some_and(|published| published.contains("\"NewStateValue\"")),
                "case {case:?}: a published action must record the message it handed over, got \
                 {item}"
            );
            return 1;
        }
        Sealing::NoAllow(operation) => (
            operation,
            format!("because no resource-based policy allows the {operation} action"),
        ),
        Sealing::Denied(operation) => (operation, EXPLICIT_DENY.to_string()),
    };
    assert_eq!(
        item["actionState"].as_str(),
        Some("Failed"),
        "case {case:?}: a publish the key withheld is Failed, got {item}"
    );
    assert_eq!(
        item["publishedMessage"].as_str(),
        None,
        "case {case:?}: a withheld publish hands nothing over, so it records no published \
         message, got {item}"
    );
    let error = item["error"].as_str().unwrap_or_default();
    let refused = format!(
        "User: {CLOUDWATCH_PRINCIPAL} is not authorized to perform: {operation} on resource: \
         {key_arn}"
    );
    assert!(
        error.contains(&refused) && error.contains(&reason),
        "case {case:?}: the error must name {refused:?} and {reason:?}, got {error:?}"
    );
    0
}
