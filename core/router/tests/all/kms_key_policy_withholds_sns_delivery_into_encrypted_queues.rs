// SPDX-License-Identifier: CC0-1.0

//! A notification this emulator's SNS writes into an SQS queue that is encrypted at rest with
//! a customer managed key reaches that queue only where the KEY's own policy admits the
//! service principal SNS delivers as — asserted DIFFERENTIALLY, over twin queues subscribed
//! to ONE topic before anything is published, through the assembled `MultiServiceRouter` with
//! KMS, SNS and Amazon SQS mounted on one engine.
//!
//! The RULE asserted here: a queue carrying `KmsMasterKeyId` has every message written to it
//! sealed under that key, and the queue's own service generates that data key for the WRITER —
//! which on a subscription delivery is the service principal `sns.amazonaws.com` and not the
//! queue's owner. The key's policy is therefore a second document in force over the delivery,
//! beside the queue's own access policy: a key whose policy admits that principal neither the
//! data-key nor the decrypt operation takes no notification at all, while a twin queue whose
//! key grants them takes every one in the same instant. The withhold is not a refusal of the
//! publishing call — it answers with a message id either way — so it is observable ONLY as the
//! absence of the delivery, and a client that adds the grant sees the notification published
//! after it arrive while the ones withheld before it never do.
//!
//! What this suite adds over the single-queue reading in the SNS crate's own
//! `sqs_delivery_queue_encryption.rs` is the DIFFERENTIAL: the decision belongs to each
//! destination, so within one fan-out a withheld queue does not stop its twin and a granted
//! twin does not carry the withheld one through. Its rows differ along what the rule
//! generalises over: how the withholding key's policy fails to admit the principal (the
//! administrator's statement alone, a grant to another service's principal, a grant scoped by
//! condition to a use arriving through another service, a grant of the data-key operation
//! without the decrypt), which spelling the queues name their keys by (the key id, the key
//! ARN, an alias), how many messages the publisher sends, and whether it sends them one call
//! at a time or as one batch.
//!
//! Three crates have to agree for the rule to hold (KMS mints the keys and decides what each
//! policy admits, SNS performs the delivery as a service principal, Amazon SQS stores the
//! message under the queue's key), so the suite lives here rather than in any one of them.
//! Every arrival is waited for by POLLING the queue, so no outcome depends on how fast the
//! host runs; the empty drain of the withheld twin is read only once its granted twin holds
//! everything published — the differential the absence of a delivery is observable against —
//! and is then re-read over a window ([`nothing_more_arrives`]), so it does not rest on the
//! two subscriptions' legs resolving in any particular order; the granted twin is read the
//! same way afterwards, so a duplicated delivery to it is not missed either.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{element, elements, form_encode};

/// The Region every call here is signed for, and so the Region the topic, the queues and the
/// keys belong to.
const REGION: &str = "us-east-1";

/// How long an arrival is given before the suite gives up: far longer than any clock-owed
/// transition on this path, so a loaded host cannot decide the outcome.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a queue that must have taken NOTHING is watched for: an absence has no arrival to
/// poll for, so it is re-read over a window instead of once. Many times [`POLL_INTERVAL`] and
/// well inside [`DEADLINE`], so the window stays a window on a loaded host.
const EMPTY_WINDOW: Duration = Duration::from_secs(1);

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One SNS operation over its awsQuery wire, with whatever status it answered.
async fn sns(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("sns"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One SNS operation that must succeed, answering its XML body.
async fn sns_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = sns(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SNS {action}, got {status} with {body}"
    );
    body
}

/// One awsJson call, over the target prefix the service's protocol names.
async fn json_call(
    router: &MultiServiceRouter,
    service: &str,
    target_prefix: &str,
    version: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", format!("application/x-amz-json-{version}"))
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(service))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Amazon SQS operation that must succeed.
async fn sqs_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: Value,
) -> Value {
    let (status, parsed) = json_call(router, "sqs", "AmazonSQS", "1.0", operation, &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One KMS operation that must succeed.
async fn kms_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: Value,
) -> Value {
    let (status, parsed) =
        json_call(router, "kms", "TrentService", "1.1", operation, &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from KMS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// Which spelling of a key's identity a queue's `KmsMasterKeyId` names it by: the forms KMS
/// resolves a `KeyId` member in — "a key ID, key ARN, alias name, or alias ARN"
/// (`smithy_query(query="doc",shape="GenerateDataKeyRequest",member="KeyId")`).
#[derive(Debug, Clone, Copy)]
enum NamedBy {
    KeyId,
    KeyArn,
    Alias,
}

/// How the publisher sends the row's messages: one call per message, or one batch carrying all
/// of them — both fan out over the same subscriptions.
#[derive(Debug, Clone, Copy)]
enum Publication {
    OneCallEach,
    OneBatch,
}

/// One customer managed key this suite minted, in each of the forms a queue can name it by.
struct Key {
    id: String,
    arn: String,
    alias: String,
}

impl Key {
    /// The spelling `named_by` asks for.
    fn named(&self, named_by: NamedBy) -> &str {
        match named_by {
            NamedBy::KeyId => &self.id,
            NamedBy::KeyArn => &self.arn,
            NamedBy::Alias => &self.alias,
        }
    }

    /// The account of the key's own ARN: the identity a key policy's administrative statement
    /// names.
    fn account(&self) -> &str {
        self.arn
            .split(':')
            .nth(4)
            .unwrap_or_else(|| panic!("no account in {}", self.arn))
    }
}

/// The statement every key policy here carries: the one `CreateKey` writes by default, which
/// "gives the Amazon Web Services account that owns the KMS key permission to use it in all
/// cryptographic operations" — and grants no service principal anything.
fn administers(account: &str) -> Value {
    json!({
        "Sid": "AccountAdministers",
        "Effect": "Allow",
        "Principal": {"AWS": format!("arn:aws:iam::{account}:root")},
        "Action": "kms:*",
        "Resource": "*",
    })
}

/// A key policy document: the administrative statement, then whatever the row adds.
fn key_policy(account: &str, statements: Vec<Value>) -> Value {
    let mut all = vec![administers(account)];
    all.extend(statements);
    json!({"Version": "2012-10-17", "Statement": all})
}

/// The statement AWS documents for a queue subscribed to an SNS topic: the service principal
/// `sns.amazonaws.com`, allowed the two operations sealing a message under the key performs.
fn grant_to_sns_principal() -> Value {
    json!({
        "Sid": "SnsEncryptsIntoTheQueue",
        "Effect": "Allow",
        "Principal": {"Service": "sns.amazonaws.com"},
        "Action": ["kms:GenerateDataKey*", "kms:Decrypt"],
        "Resource": "*",
    })
}

/// The policy of the key the GRANTED twin stands on, in every row: the documented grant.
fn granting_policy(account: &str) -> Value {
    key_policy(account, vec![grant_to_sns_principal()])
}

/// A key with the alias `alias` whose policy is what `policy` builds from its account: the
/// three spellings of its identity.
async fn given_key(
    router: &MultiServiceRouter,
    case: &str,
    alias: &str,
    policy: fn(&str) -> Value,
) -> Key {
    let created = kms_ok(router, case, "CreateKey", json!({})).await;
    let id = created["KeyMetadata"]["KeyId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no KeyMetadata.KeyId in {created}"))
        .to_string();
    let arn = created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no KeyMetadata.Arn in {created}"))
        .to_string();
    kms_ok(
        router,
        case,
        "CreateAlias",
        json!({"AliasName": alias, "TargetKeyId": id}),
    )
    .await;
    let key = Key {
        id,
        arn,
        alias: alias.to_string(),
    };
    kms_ok(
        router,
        case,
        "PutKeyPolicy",
        json!({
            "KeyId": key.arn,
            "PolicyName": "default",
            "Policy": policy(key.account()).to_string(),
        }),
    )
    .await;
    key
}

/// The queue policy AWS documents for a queue subscribed to an SNS topic: the service
/// principal `sns.amazonaws.com`, allowed `sqs:SendMessage` on that queue.
fn queue_policy_granting_sns(queue_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": {"Service": "sns.amazonaws.com"},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
        }],
    })
    .to_string()
}

/// A queue called `name` encrypted with the key `master_key_id` names, carrying the queue
/// policy an SNS subscriber needs: its url, and the ARN a subscription names it by.
async fn given_encrypted_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    master_key_id: &str,
) -> (String, String) {
    let created = sqs_ok(
        router,
        case,
        "CreateQueue",
        json!({"QueueName": name, "Attributes": {"KmsMasterKeyId": master_key_id}}),
    )
    .await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let reported = sqs_ok(
        router,
        case,
        "GetQueueAttributes",
        json!({"QueueUrl": url, "AttributeNames": ["QueueArn", "KmsMasterKeyId"]}),
    )
    .await;
    let arn = reported["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {reported}"))
        .to_string();
    assert_eq!(
        reported["Attributes"]["KmsMasterKeyId"].as_str(),
        Some(master_key_id),
        "case {case:?}: the queue must stand on the key it was created with, got {reported}"
    );
    sqs_ok(
        router,
        case,
        "SetQueueAttributes",
        json!({"QueueUrl": url, "Attributes": {"Policy": queue_policy_granting_sns(&arn)}}),
    )
    .await;
    (url, arn)
}

/// Subscribe the queue `queue_arn` names to `topic`, asking for raw delivery so what the queue
/// holds is the published body itself.
async fn subscribe_raw(router: &MultiServiceRouter, case: &str, topic: &str, queue_arn: &str) {
    let answer = sns_ok(
        router,
        case,
        "Subscribe",
        &[
            ("TopicArn", topic),
            ("Protocol", "sqs"),
            ("Endpoint", queue_arn),
            ("Attributes.entry.1.key", "RawMessageDelivery"),
            ("Attributes.entry.1.value", "true"),
        ],
    )
    .await;
    let subscription = element(&answer, "SubscriptionArn");
    assert!(
        subscription.contains(':'),
        "case {case:?}: an SQS subscription is confirmed at once, got {subscription:?}"
    );
}

/// Publish `bodies` to `topic`, the way `how` asks for, and assert the call answered with one
/// message id per body: a withheld delivery is still an accepted publish.
async fn publish(
    router: &MultiServiceRouter,
    case: &str,
    topic: &str,
    bodies: &[&str],
    how: Publication,
) {
    match how {
        Publication::OneCallEach => {
            for body in bodies {
                let answer =
                    sns_ok(router, case, "Publish", &[("TopicArn", topic), ("Message", body)])
                        .await;
                let message_id = element(&answer, "MessageId");
                assert!(
                    !message_id.is_empty(),
                    "case {case:?}: the publish of {body:?} must answer with a MessageId, got \
                     {answer}"
                );
            }
        }
        Publication::OneBatch => {
            let mut entries: Vec<(String, String)> = Vec::new();
            for (index, body) in bodies.iter().enumerate() {
                let prefix = format!("PublishBatchRequestEntries.member.{}", index + 1);
                entries.push((format!("{prefix}.Id"), format!("e{index}")));
                entries.push((format!("{prefix}.Message"), (*body).to_string()));
            }
            let mut members: Vec<(&str, &str)> = vec![("TopicArn", topic)];
            members.extend(
                entries
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            );
            let answer = sns_ok(router, case, "PublishBatch", &members).await;
            let ids = elements(&answer, "MessageId");
            assert_eq!(
                ids.len(),
                bodies.len(),
                "case {case:?}: the batch must answer with one MessageId per entry, got {answer}"
            );
        }
    }
}

/// POLL the queue at `url` until it has handed over `expected` messages, answering their
/// bodies in the order they were received and taking each one off the queue.
///
/// The wait is for the CONDITION rather than for a time.
async fn received(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    expected: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    let mut held: Vec<String> = Vec::new();
    loop {
        for message in waiting(router, case, url).await {
            held.push(message["Body"].as_str().unwrap_or_default().to_string());
            let receipt = message["ReceiptHandle"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no ReceiptHandle in {message}"));
            sqs_ok(
                router,
                case,
                "DeleteMessage",
                json!({"QueueUrl": url, "ReceiptHandle": receipt}),
            )
            .await;
        }
        if held.len() >= expected {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {expected} message(s) on the queue, got {held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One look at the queue at `url`: the messages it handed over, which may be none.
async fn waiting(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let answer = sqs_ok(
        router,
        case,
        "ReceiveMessage",
        json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    answer["Messages"].as_array().cloned().unwrap_or_default()
}

/// The bodies that ever turn up on the queue at `url` while it is watched over
/// [`EMPTY_WINDOW`]: what a queue that must hold nothing more is asserted empty by.
///
/// Read AFTER the queue's twin holds everything published, which is the barrier the absence of
/// a delivery is observable against — and then looked at again over the window, so an empty
/// answer does not rest on the two subscriptions' legs resolving in any particular order, and
/// a body that leaks through on a later leg is still seen.
async fn nothing_more_arrives(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let until = Instant::now() + EMPTY_WINDOW;
    let mut seen: Vec<String> = Vec::new();
    loop {
        seen.extend(
            waiting(router, case, url)
                .await
                .iter()
                .map(|message| message["Body"].as_str().unwrap_or_default().to_string()),
        );
        if Instant::now() >= until {
            return seen;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The control that the withheld queue and its key WORK: a client of the account that owns
/// them sends into the queue directly and receives what it sent, so the only thing left to
/// explain an absent notification is the authorization of this service's principal.
async fn owner_can_send_into(router: &MultiServiceRouter, case: &str, url: &str) {
    let sent = "the owner's own send";
    sqs_ok(
        router,
        case,
        "SendMessage",
        json!({"QueueUrl": url, "MessageBody": sent}),
    )
    .await;
    let held = received(router, case, url, 1).await;
    assert_eq!(
        held,
        vec![sent.to_string()],
        "case {case:?}: the queue and its key must serve the account's own send, got {held:?}"
    );
}

/// `held`, sorted, as a set of bodies compares whatever order a fan-out handed them over in.
fn sorted(bodies: &[String]) -> Vec<String> {
    let mut sorted = bodies.to_vec();
    sorted.sort();
    sorted
}

/// What one row stands up: the policy of the key the withheld twin stands on, how both queues
/// name their keys, and what is published to the topic they are both subscribed to.
struct Row {
    case: &'static str,
    /// The policy of the WITHHOLDING key, built from the account that owns it.
    withholding_policy: fn(&str) -> Value,
    named_by: NamedBy,
    bodies: &'static [&'static str],
    published: Publication,
}

#[tokio::test]
async fn twin_queues_on_one_topic_take_a_notification_only_where_their_own_key_admits_sns() {
    let router = build_router();

    let rows: &[Row] = &[
        Row {
            case: "the administrator's statement alone, keys named by ARN, three publishes",
            withholding_policy: |account| key_policy(account, Vec::new()),
            named_by: NamedBy::KeyArn,
            bodies: &["marker-one", "marker-two", "marker-three"],
            published: Publication::OneCallEach,
        },
        Row {
            case: "the documented grant made to another service's principal, keys named by id",
            withholding_policy: |account| {
                key_policy(
                    account,
                    vec![json!({
                        "Sid": "AnotherServiceEncrypts",
                        "Effect": "Allow",
                        "Principal": {"Service": "events.amazonaws.com"},
                        "Action": ["kms:GenerateDataKey*", "kms:Decrypt"],
                        "Resource": "*",
                    })],
                )
            },
            named_by: NamedBy::KeyId,
            bodies: &["only-marker"],
            published: Publication::OneCallEach,
        },
        Row {
            case: "a grant scoped to a use arriving through another service, keys named by \
                   alias, one batch",
            withholding_policy: |account| {
                key_policy(
                    account,
                    vec![json!({
                        "Sid": "SnsEncryptsElsewhere",
                        "Effect": "Allow",
                        "Principal": {"Service": "sns.amazonaws.com"},
                        "Action": ["kms:GenerateDataKey*", "kms:Decrypt"],
                        "Resource": "*",
                        "Condition": {
                            "StringEquals": {
                                "kms:ViaService": format!("dynamodb.{REGION}.amazonaws.com"),
                            },
                        },
                    })],
                )
            },
            named_by: NamedBy::Alias,
            bodies: &["batched-one", "batched-two"],
            published: Publication::OneBatch,
        },
        Row {
            case: "the data-key grant without the decrypt the documented statement carries, \
                   keys named by ARN, one batch",
            withholding_policy: |account| {
                key_policy(
                    account,
                    vec![json!({
                        "Sid": "SnsGeneratesButCannotDecrypt",
                        "Effect": "Allow",
                        "Principal": {"Service": "sns.amazonaws.com"},
                        "Action": "kms:GenerateDataKey*",
                        "Resource": "*",
                    })],
                )
            },
            named_by: NamedBy::KeyArn,
            bodies: &["half-granted-one", "half-granted-two", "half-granted-three"],
            published: Publication::OneBatch,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let topic = element(
            &sns_ok(
                &router,
                case,
                "CreateTopic",
                &[("Name", &format!("twin-fan-out-{index}"))],
            )
            .await,
            "TopicArn",
        );
        let granted_key = given_key(
            &router,
            case,
            &format!("alias/granted-{index}"),
            granting_policy,
        )
        .await;
        let withheld_key = given_key(
            &router,
            case,
            &format!("alias/withheld-{index}"),
            row.withholding_policy,
        )
        .await;

        let (granted_url, granted_arn) = given_encrypted_queue(
            &router,
            case,
            &format!("granted-twin-{index}"),
            granted_key.named(row.named_by),
        )
        .await;
        let (withheld_url, withheld_arn) = given_encrypted_queue(
            &router,
            case,
            &format!("withheld-twin-{index}"),
            withheld_key.named(row.named_by),
        )
        .await;

        // BOTH subscriptions stand before anything is published, so neither queue can be
        // missing a notification for having subscribed late.
        subscribe_raw(&router, case, &topic, &granted_arn).await;
        subscribe_raw(&router, case, &topic, &withheld_arn).await;

        owner_can_send_into(&router, case, &withheld_url).await;

        publish(&router, case, &topic, row.bodies, row.published).await;

        let expected: Vec<String> = row.bodies.iter().map(|body| (*body).to_string()).collect();
        let delivered = received(&router, case, &granted_url, expected.len()).await;
        assert_eq!(
            sorted(&delivered),
            sorted(&expected),
            "case {case:?}: the twin whose key grants this service must take every published \
             body, got {delivered:?}"
        );

        // Read only once its twin holds everything: the absence of a delivery is observable
        // against that arrival, and nothing else is owed to this queue.
        let withheld = nothing_more_arrives(&router, case, &withheld_url).await;
        assert!(
            withheld.is_empty(),
            "case {case:?}: the twin whose key admits this service nothing must take no \
             notification at all, got {withheld:?}"
        );

        // And the granted twin took the published bodies ONCE: nothing further is waiting on
        // it, so neither a duplicated delivery nor a withheld body arriving late is missed.
        let extra = nothing_more_arrives(&router, case, &granted_url).await;
        assert!(
            extra.is_empty(),
            "case {case:?}: the granted twin must take each published body once, got {extra:?} \
             after {delivered:?}"
        );
    }
}

#[tokio::test]
async fn a_withheld_queue_takes_the_notification_published_after_its_key_grants_sns() {
    let router = build_router();
    let case = "a key policy that grants this service nothing until a statement names it";
    let topic = element(
        &sns_ok(&router, case, "CreateTopic", &[("Name", "withheld-then-granted")]).await,
        "TopicArn",
    );
    let granted_key = given_key(&router, case, "alias/granted-then", granting_policy).await;
    let withheld_key = given_key(&router, case, "alias/withheld-then", |account| {
        key_policy(account, Vec::new())
    })
    .await;
    let (granted_url, granted_arn) = given_encrypted_queue(
        &router,
        case,
        "granted-before-the-grant",
        &granted_key.arn,
    )
    .await;
    let (withheld_url, withheld_arn) =
        given_encrypted_queue(&router, case, "withheld-before-the-grant", &withheld_key.arn).await;
    subscribe_raw(&router, case, &topic, &granted_arn).await;
    subscribe_raw(&router, case, &topic, &withheld_arn).await;

    owner_can_send_into(&router, case, &withheld_url).await;

    let before: &[&str] = &["before-one", "before-two", "before-three"];
    publish(&router, case, &topic, before, Publication::OneCallEach).await;
    let delivered = received(&router, case, &granted_url, before.len()).await;
    let expected: Vec<String> = before.iter().map(|body| (*body).to_string()).collect();
    assert_eq!(
        sorted(&delivered),
        sorted(&expected),
        "case {case:?}: the granted twin must take every body published before the grant, got \
         {delivered:?}"
    );
    let withheld = nothing_more_arrives(&router, case, &withheld_url).await;
    assert!(
        withheld.is_empty(),
        "case {case:?}: the queue whose key names no service principal must stay empty, got \
         {withheld:?}"
    );

    // The grant AWS documents for the subscriber, added to the same key.
    kms_ok(
        &router,
        case,
        "PutKeyPolicy",
        json!({
            "KeyId": withheld_key.arn,
            "PolicyName": "default",
            "Policy": key_policy(withheld_key.account(), vec![grant_to_sns_principal()])
                .to_string(),
        }),
    )
    .await;
    // What the key now says is what KMS reports it says: the administrative statement the key
    // was created with, and the grant just added.
    let read = kms_ok(
        &router,
        case,
        "GetKeyPolicy",
        json!({"KeyId": withheld_key.arn, "PolicyName": "default"}),
    )
    .await;
    let document: Value = serde_json::from_str(
        read["Policy"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no Policy in {read}")),
    )
    .unwrap_or_else(|error| panic!("case {case:?}: the policy is not JSON ({error}): {read}"));
    let sids: Vec<String> = document["Statement"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|statement| statement["Sid"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        sids,
        vec![
            "AccountAdministers".to_string(),
            "SnsEncryptsIntoTheQueue".to_string(),
        ],
        "case {case:?}: the key must report the policy just written, got {document}"
    );

    publish(&router, case, &topic, &["after-the-grant"], Publication::OneCallEach).await;
    let arrived = received(&router, case, &withheld_url, 1).await;
    assert_eq!(
        arrived,
        vec!["after-the-grant".to_string()],
        "case {case:?}: once the key policy grants this service, the body published after it \
         must arrive — and the ones withheld before it must not, got {arrived:?}"
    );
    let also = received(&router, case, &granted_url, 1).await;
    assert_eq!(
        also,
        vec!["after-the-grant".to_string()],
        "case {case:?}: the granted twin must take that body too, got {also:?}"
    );

    // Neither queue holds anything FURTHER: the bodies withheld before the grant are not
    // delivered late once the policy admits this service, and the granted twin took the body
    // published after it once.
    for (which, url) in [("withheld", &withheld_url), ("granted", &granted_url)] {
        let settled = nothing_more_arrives(&router, case, url).await;
        assert!(
            settled.is_empty(),
            "case {case:?}: the {which} queue must hold nothing beyond the body published after \
             the grant, got {settled:?}"
        );
    }
}
