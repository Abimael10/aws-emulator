// SPDX-License-Identifier: CC0-1.0

//! What a message published to the AWS IoT data plane does to the STORAGE targets of the topic
//! rules that select it, observed where a client observes it: as the item DynamoDB's own
//! `GetItem` serves, and as the object Amazon S3's own `ListObjectsV2` / `GetObject` serve.
//!
//! RULE, one table each half:
//!
//! 1. A message the broker accepts is evaluated by the rules engine, and every ENABLED topic
//!    rule whose `FROM`-clause topic filter covers the published topic has each of its actions
//!    performed against that action's own TARGET SERVICE for the message its statement selects —
//!    for a `dynamoDBv2` action one `dynamodb:PutItem` into the table its `putItem` names, with
//!    "each attribute in the message payload ... written to a separate column"
//!    (`DynamoDBv2Action$putItem`), and for an `s3` action one `s3:PutObject` into the bucket it
//!    names, whose body is that selected message and whose key is the action's `key` RENDERED
//!    for this message (`${sql-expression}`, the substitution-template syntax) — and the action
//!    is performed only while its `roleArn` both admits the IoT service principal and carries a
//!    grant for that call on that target, with NOTHING written otherwise.
//! 2. Nothing of either is reported to the publisher, which is answered 200 whatever the rules
//!    did with the message, so every assertion below is made on the TARGET.
//!
//! Four crates have to agree for that to hold — the IoT data plane takes the publish, the IoT
//! control plane holds the rule and runs the engine, IAM holds the action's role, and DynamoDB
//! or Amazon S3 takes (or refuses) the write — so the suite lives here: a crate-local suite can
//! only ever see one side of it.
//!
//! The rows differ along what the rule generalises over rather than along one recorded case: the
//! form of the topic filter (exact, the single-level `+`, the multi-level `#`, one that does not
//! cover the topic), the rule's enablement (as created, created disabled, and disabled then
//! enabled through `EnableTopicRule`), what the statement SELECTS (the whole message, a
//! projection naming attributes, a condition that holds and one that does not), the JSON TYPE of
//! each member of the message (string, integer, fraction, boolean, array, object, null) and a
//! payload that is no document at all, the form of the `s3` action's key (a literal, one
//! template, several templates between literal text, a nested attribute path, and a template
//! this emulator does not evaluate), whether that action names a canned ACL, what the action's
//! ROLE says about the write (the grant, a wildcard grant, another action of the same service,
//! another resource of the same type, no inline policy at all, a trust policy naming another
//! service principal), and whether the target the action names exists at all.
//!
//! Every row that expects NOTHING written is judged against a target that has since taken a
//! CONTROL message whose own write is known to work, so no row can pass against a table or a
//! bucket nothing ever reached.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::rules_engine::dynamodb_action::PUT_ITEM_ACTION;
use service_com_amazonaws_iot_awsiotservice::state::rules_engine::s3_action::PUT_OBJECT_ACTION;
use service_com_amazonaws_iot_awsiotservice::state::SERVICE_PRINCIPAL;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// The table every `dynamoDBv2` action of these rows writes to, and a second table that exists
/// only so a role can be granted the write on ANOTHER table's ARN.
const TABLE: &str = "IotRuleItems";
const ANOTHER_TABLE: &str = "IotRuleElsewhere";

/// The bucket every `s3` action of these rows writes to, and a second bucket that exists only so
/// a role can be granted the write on ANOTHER bucket's objects.
const BUCKET: &str = "iot-rule-objects";
const ANOTHER_BUCKET: &str = "iot-rule-elsewhere";

/// The partition key every row's table carries, which is the column the message supplies.
const HASH_KEY: &str = "id";

/// The rule each row stores. A rule name carries letters, digits and underscores alone
/// (`RuleName`, `pattern ^[a-zA-Z0-9_]+$`).
const RULE: &str = "to_the_store";

/// The CONTROL rule every row also stores, its topic, the marker its message carries and the key
/// its `s3` action writes: a write that is known to be performable, which is what makes a row
/// expecting NOTHING a real assertion. No row's topic filter covers this topic.
const CONTROL_RULE: &str = "control_write";
const CONTROL_TOPIC: &str = "control/write";
const CONTROL_MARKER: &str = "control-1";
const CONTROL_KEY: &str = "control/written";

/// The role the control rule runs under ([`given_control_role`]).
const CONTROL_ROLE: &str = "iot_rule_control";

/// The SQL version a rule is stored with (`TopicRulePayload$awsIotSqlVersion`).
const SQL_VERSION: &str = "2016-03-23";

/// The marker every row's own message carries, which is the item's partition key and what the
/// templated object keys are computed from.
const MARKER: &str = "m-1";

/// The message the rows publish unless one names its own: a document carrying one member of
/// every JSON type, so what each becomes in the item is asserted on one write.
const MESSAGE: &str = r#"{"id":"m-1","temp":42,"ratio":0.5,"ok":true,"tags":["a","b"],
    "state":{"reported":{"room":"r1"}},"missing":null}"#;

/// The topic the rows publish to unless one names its own, and the filter that covers it
/// exactly.
const TOPIC: &str = "sensors/room-1/temp";

/// Another action of each target service, for the role that is granted something other than the
/// write the rule action makes.
const ANOTHER_DYNAMODB_ACTION: &str = "dynamodb:GetItem";
const ANOTHER_S3_ACTION: &str = "s3:GetObject";

/// Every action of each target service, for the role granted by wildcard.
const EVERY_DYNAMODB_ACTION: &str = "dynamodb:*";
const EVERY_S3_ACTION: &str = "s3:*";

/// The service principal a role may trust INSTEAD of the one the rules engine acts as.
const ANOTHER_PRINCIPAL: &str = "lambda.amazonaws.com";

/// How long one row's observation of a write is given, and how long between looks. Generous
/// beside the work a row does — the write is made before the publish is answered — without
/// making a genuine regression cost minutes a row: what each wait is ON is a condition, never a
/// span.
const DEADLINE: Duration = Duration::from_secs(30);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long a table is given to reach `ACTIVE`, which is the status DynamoDB serves items on
/// ("You can perform read and write operations only on an ACTIVE table"). Generous beside that
/// service's own transition span.
const ACTIVATION_DEADLINE: Duration = Duration::from_secs(60);

/// The access key id every call below presents: one neither IAM nor STS holds a record for,
/// which is the account's own published credential — so what these rows decide is the RULE's and
/// not an identity's.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The emulator these rows run against: the IoT control plane, the IoT data plane, DynamoDB,
/// Amazon S3 and IAM on ONE engine — the shape `core/router/src/lib.rs` mounts them in.
struct Emulator {
    router: MultiServiceRouter,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the front door's concern, not this suite's: the credential
    // scope is what routes a request here.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iot_awsiotservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iotdataplane_iotmoonrakerservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    Emulator { router }
}

/// The emulator with EVERY service mounted, as `core/router/src/lib.rs` assembles it for the
/// production entrypoint — the shape a client's rule actually runs in, where another service
/// could claim the request a rule action makes.
fn assembled_emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    Emulator { router }
}

/// One HTTP answer: the status and the body, read as JSON where there is one.
struct Answer {
    status: u16,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

impl Emulator {
    async fn send(
        &self,
        method: &str,
        uri: &str,
        signing_name: &str,
        content_type: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Answer {
        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/{signing_name}/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .header("content-type", content_type);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = self
            .router
            .dispatch(
                request
                    .body(bytes::Bytes::from(body))
                    .expect("the test request is well formed"),
            )
            .await;
        Answer {
            status: response.status().as_u16(),
            body: response.into_body().to_vec(),
        }
    }

    /// `CreateTopicRule`: the restJson1 `POST /rules/{ruleName}` of the IoT control plane, whose
    /// body IS the `topicRulePayload` (the member is the operation's HTTP payload).
    async fn create_topic_rule(&self, case: &str, rule_name: &str, payload: &Value) {
        let answer = self
            .send(
                "POST",
                &format!("/rules/{rule_name}"),
                "iot",
                "application/json",
                &[],
                serde_json::to_vec(payload).expect("the payload serializes"),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: CreateTopicRule({rule_name}) answered {} {}",
            answer.status,
            answer.text()
        );
    }

    /// `EnableTopicRule` (`POST /rules/{ruleName}/enable`): the call that makes a rule stored
    /// disabled start delivering.
    async fn enable_topic_rule(&self, case: &str, rule_name: &str) {
        let answer = self
            .send(
                "POST",
                &format!("/rules/{rule_name}/enable"),
                "iot",
                "application/json",
                &[],
                Vec::new(),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: EnableTopicRule({rule_name}) answered {} {}",
            answer.status,
            answer.text()
        );
    }

    /// `Publish` of the IoT data plane: `POST /topics/{topic}` at QoS 1, the call the rules
    /// engine runs on. A `{topic}` path label carries no `/` of its own, so a topic name's
    /// separators travel percent-encoded, which is how an SDK sends one holding slashes.
    async fn publish(&self, case: &str, topic: &str, payload: &[u8]) {
        let answer = self
            .send(
                "POST",
                &format!("/topics/{}?qos=1", form_encode(topic)),
                "iotdata",
                "application/octet-stream",
                &[],
                payload.to_vec(),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: Publish({topic}) answered {} {}",
            answer.status,
            answer.text()
        );
    }

    /// One DynamoDB call (`awsJson1_0`) that must be served, answered with its document.
    async fn dynamodb(&self, case: &str, operation: &str, body: &Value) -> Value {
        let answer = self
            .send(
                "POST",
                "/",
                "dynamodb",
                "application/x-amz-json-1.0",
                &[("x-amz-target", &format!("DynamoDB_20120810.{operation}"))],
                serde_json::to_vec(body).expect("the request serializes"),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: DynamoDB {operation} answered {} {}",
            answer.status,
            answer.text()
        );
        answer.json()
    }

    /// One IAM call over the `awsQuery` protocol that must be served, answered with its XML body.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, &str)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let answer = self
            .send(
                "POST",
                "/",
                "iam",
                "application/x-www-form-urlencoded",
                &[],
                body.into_bytes(),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: IAM {action} answered {} {}",
            answer.status,
            answer.text()
        );
        answer.text()
    }

    /// `CreateBucket`: `PUT /<bucket>`, so an `s3` action has a bucket to write into.
    async fn create_bucket(&self, case: &str, bucket: &str) {
        let answer = self
            .send(
                "PUT",
                &format!("/{bucket}"),
                "s3",
                "application/xml",
                &[],
                Vec::new(),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: CreateBucket({bucket}) answered {} {}",
            answer.status,
            answer.text()
        );
    }

    /// Every object key `bucket` holds, as `ListObjectsV2` reports them.
    async fn keys(&self, case: &str, bucket: &str) -> Vec<String> {
        let answer = self
            .send(
                "GET",
                &format!("/{bucket}?list-type=2"),
                "s3",
                "application/xml",
                &[],
                Vec::new(),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: ListObjectsV2({bucket}) answered {} {}",
            answer.status,
            answer.text()
        );
        let mut keys = elements(&answer.text(), "Key");
        keys.sort();
        keys
    }

    /// The body of the object `key` in `bucket`, or `None` when the store holds no such object.
    async fn object(&self, bucket: &str, key: &str) -> Option<String> {
        let answer = self
            .send(
                "GET",
                &format!("/{bucket}/{key}"),
                "s3",
                "application/octet-stream",
                &[],
                Vec::new(),
            )
            .await;
        (answer.status == 200).then(|| answer.text())
    }

    /// The item `marker` keys in `table`, read with `ConsistentRead` so what is asserted is what
    /// the store holds rather than a stale read.
    async fn item(&self, case: &str, table: &str, marker: &str) -> Option<Value> {
        let read = self
            .dynamodb(
                case,
                "GetItem",
                &json!({
                    "TableName": table,
                    "Key": { HASH_KEY: { "S": marker } },
                    "ConsistentRead": true,
                }),
            )
            .await;
        read.get("Item").cloned()
    }

    /// The item `marker` keys in `table` once it is there, or `None` once [`DEADLINE`] passes
    /// without it: the write is made while the publish is being served, so this is what a client
    /// polling the downstream service does.
    async fn polled_item(&self, case: &str, table: &str, marker: &str) -> Option<Value> {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(item) = self.item(case, table, marker).await {
                return Some(item);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }

    /// The object `key` in `bucket` once it is there, or `None` once [`DEADLINE`] passes without
    /// it.
    async fn polled_object(&self, bucket: &str, key: &str) -> Option<String> {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(body) = self.object(bucket, key).await {
                return Some(body);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }
}

/// A table of `name` keyed by [`HASH_KEY`], as `CreateTable` takes it. The table is `CREATING`
/// when this answers; [`active_table_arn`] is the wait for the status DynamoDB serves items on,
/// kept apart from the create so two tables of one row spend ONE span between them rather than
/// two.
async fn given_table(emulator: &Emulator, case: &str, name: &str) {
    emulator
        .dynamodb(
            case,
            "CreateTable",
            &json!({
                "TableName": name,
                "KeySchema": [{ "AttributeName": HASH_KEY, "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": HASH_KEY, "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST",
            }),
        )
        .await;
}

/// The ARN of the table `name` once DynamoDB reports it `ACTIVE` — the status it serves items on
/// ("You can perform read and write operations only on an ACTIVE table") — as `DescribeTable`
/// reports it, which is the value a role's grant names the table by.
async fn active_table_arn(emulator: &Emulator, case: &str, name: &str) -> String {
    let deadline = Instant::now() + ACTIVATION_DEADLINE;
    loop {
        let described = emulator
            .dynamodb(case, "DescribeTable", &json!({ "TableName": name }))
            .await;
        if described["Table"]["TableStatus"] == json!("ACTIVE") {
            return described["Table"]["TableArn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: DescribeTable({name}) reported no ARN"))
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the table {name} never became ACTIVE: {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// A trust policy admitting one service principal, written as a client sends it.
fn trust_policy(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// An identity policy allowing one action on one resource.
fn identity_policy(action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
    .to_string()
}

/// What the ROLE a rule action names says about the write it asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoleKind {
    /// Exactly the call the action makes, on exactly the resource it names.
    Granted,
    /// Every action of the target service on every resource.
    Wildcard,
    /// Another action of the same service on the same resource.
    AnotherAction,
    /// The call the action makes, on another resource of the same type.
    AnotherTarget,
    /// A role carrying no identity-based policy at all.
    NoPolicy,
    /// The grant, under a role that trusts another service principal — so AWS IoT cannot act as
    /// it in the first place.
    AnotherPrincipal,
}

impl RoleKind {
    /// The role name each kind is created under, so two kinds in one account are two roles.
    fn role_name(self) -> &'static str {
        match self {
            Self::Granted => "iot_rule_granted",
            Self::Wildcard => "iot_rule_wildcard",
            Self::AnotherAction => "iot_rule_another_action",
            Self::AnotherTarget => "iot_rule_another_target",
            Self::NoPolicy => "iot_rule_no_policy",
            Self::AnotherPrincipal => "iot_rule_another_principal",
        }
    }

    /// Whether a role of this kind lets the rules engine make the action's call: the grant and
    /// the wildcard do, and nothing else does.
    fn admits(self) -> bool {
        matches!(self, Self::Granted | Self::Wildcard)
    }
}

/// The calls and resources one target service's role is judged over.
#[derive(Debug, Clone, Copy)]
struct Target<'a> {
    /// The call the rule action makes.
    action: &'a str,
    /// Every action of that service.
    every_action: &'a str,
    /// Another action of that service.
    another_action: &'a str,
    /// The resource the action writes to.
    resource: &'a str,
    /// Another resource of the same type.
    another_resource: &'a str,
}

/// The role the CONTROL rule of every row runs under: one the rules engine may assume, granted
/// exactly the call that rule's own action makes on the target it names.
///
/// Its own role rather than the row's, because a row varies what the ROW's role grants — down to
/// one object key — and a control write judged by that grant would stop being a control.
async fn given_control_role(
    emulator: &Emulator,
    case: &str,
    action: &str,
    resource: &str,
) -> String {
    let trust = trust_policy(SERVICE_PRINCIPAL);
    let created = emulator
        .iam(
            case,
            "CreateRole",
            &[
                ("RoleName", CONTROL_ROLE),
                ("AssumeRolePolicyDocument", &trust),
            ],
        )
        .await;
    let document = identity_policy(action, resource);
    emulator
        .iam(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", CONTROL_ROLE),
                ("PolicyName", "rule-action"),
                ("PolicyDocument", &document),
            ],
        )
        .await;
    element(&created, "Arn")
}

/// A role of `kind` for `target`, answered with the ARN IAM persisted — the value the action's
/// `roleArn` carries.
async fn given_role(emulator: &Emulator, case: &str, kind: RoleKind, target: Target<'_>) -> String {
    let principal = if kind == RoleKind::AnotherPrincipal {
        ANOTHER_PRINCIPAL
    } else {
        SERVICE_PRINCIPAL
    };
    let trust = trust_policy(principal);
    let created = emulator
        .iam(
            case,
            "CreateRole",
            &[
                ("RoleName", kind.role_name()),
                ("AssumeRolePolicyDocument", &trust),
            ],
        )
        .await;
    let arn = element(&created, "Arn");
    let allowed = match kind {
        RoleKind::Granted | RoleKind::AnotherPrincipal => Some((target.action, target.resource)),
        RoleKind::Wildcard => Some((target.every_action, "*")),
        RoleKind::AnotherAction => Some((target.another_action, target.resource)),
        RoleKind::AnotherTarget => Some((target.action, target.another_resource)),
        RoleKind::NoPolicy => None,
    };
    if let Some((action, resource)) = allowed {
        let document = identity_policy(action, resource);
        emulator
            .iam(
                case,
                "PutRolePolicy",
                &[
                    ("RoleName", kind.role_name()),
                    ("PolicyName", "rule-action"),
                    ("PolicyDocument", &document),
                ],
            )
            .await;
    }
    arn
}

/// How a row's rule is stored, and what is then done to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Enablement {
    /// Stored as an enabled rule.
    Enabled,
    /// Stored with `ruleDisabled` set, which delivers nothing.
    Disabled,
    /// Stored disabled and then enabled through `EnableTopicRule`, which delivers again.
    EnabledAfterwards,
}

/// The `topicRulePayload` one rule is stored with.
fn payload(sql: &str, actions: Value, enablement: Enablement) -> Value {
    json!({
        "sql": sql,
        "awsIotSqlVersion": SQL_VERSION,
        "ruleDisabled": enablement != Enablement::Enabled,
        "actions": actions,
    })
}

// ============================================================================
// `Action$dynamoDBv2`
// ============================================================================

/// One row of the DynamoDB table.
#[derive(Debug, Clone)]
struct ItemRow {
    case: &'static str,
    /// The statement the rule is stored with.
    sql: &'static str,
    /// The topic the message is published to.
    topic: &'static str,
    /// The message published, as its bytes.
    message: &'static str,
    /// What the rule's own enablement is.
    enablement: Enablement,
    /// What the action's role says about the write.
    role: RoleKind,
    /// The table the action's `putItem` names, which is [`TABLE`] unless the row is about naming
    /// another.
    table: &'static str,
    /// The marker the item is keyed by, which is the member the message carries under
    /// [`HASH_KEY`].
    marker: &'static str,
    /// The item the table must hold afterwards, or `None` for a row that must write nothing.
    expected: Option<Value>,
}

impl ItemRow {
    /// A row writing [`MESSAGE`] to [`TOPIC`] under the granted role, with the whole message as
    /// the item — what every row carries unless it varies one of these.
    fn new(case: &'static str, sql: &'static str, topic: &'static str) -> Self {
        Self {
            case,
            sql,
            topic,
            message: MESSAGE,
            enablement: Enablement::Enabled,
            role: RoleKind::Granted,
            table: TABLE,
            marker: MARKER,
            expected: Some(whole_message_item()),
        }
    }

    fn message(mut self, message: &'static str, expected: Option<Value>) -> Self {
        self.message = message;
        self.expected = expected;
        self
    }

    fn selecting(mut self, expected: Option<Value>) -> Self {
        self.expected = expected;
        self
    }

    fn enablement(mut self, enablement: Enablement, expected: Option<Value>) -> Self {
        self.enablement = enablement;
        self.expected = expected;
        self
    }

    fn under(mut self, role: RoleKind) -> Self {
        self.role = role;
        self.expected = role.admits().then(whole_message_item);
        self
    }

    fn writing_to(mut self, table: &'static str) -> Self {
        self.table = table;
        // Every action of the service, so what refuses the write is DYNAMODB answering about the
        // table the action named rather than the role.
        self.role = RoleKind::Wildcard;
        self.expected = None;
        self
    }
}

/// The item [`MESSAGE`] is written as: one column per top-level member, of the DynamoDB type the
/// member's JSON value has.
fn whole_message_item() -> Value {
    json!({
        "id": { "S": MARKER },
        "temp": { "N": "42" },
        "ratio": { "N": "0.5" },
        "ok": { "BOOL": true },
        "tags": { "L": [{ "S": "a" }, { "S": "b" }] },
        "state": { "M": { "reported": { "M": { "room": { "S": "r1" } } } } },
        "missing": { "NULL": true },
    })
}

/// The rows of the DynamoDB table.
fn item_rows() -> Vec<ItemRow> {
    vec![
        // What the filter and the statement decide.
        ItemRow::new("an exact topic filter", "SELECT * FROM 'sensors/room-1/temp'", TOPIC),
        ItemRow::new(
            "a single-level wildcard filter",
            "SELECT * FROM 'sensors/+/temp'",
            TOPIC,
        ),
        ItemRow::new(
            "a multi-level wildcard filter",
            "SELECT * FROM 'sensors/#'",
            TOPIC,
        ),
        ItemRow::new(
            "a filter that does not cover the topic",
            "SELECT * FROM 'sensors/room-2/temp'",
            TOPIC,
        )
        .selecting(None),
        ItemRow::new(
            "a projection, whose columns are the ones the statement selects",
            "SELECT id, temp * 2 AS doubled, topic(2) AS level FROM 'sensors/room-1/temp'",
            TOPIC,
        )
        .selecting(Some(json!({
            "id": { "S": MARKER },
            "doubled": { "N": "84" },
            "level": { "S": "room-1" },
        }))),
        ItemRow::new(
            "a condition that holds for the message",
            "SELECT * FROM 'sensors/room-1/temp' WHERE temp > 10",
            TOPIC,
        ),
        ItemRow::new(
            "a condition that does not hold for the message",
            "SELECT * FROM 'sensors/room-1/temp' WHERE temp > 100",
            TOPIC,
        )
        .selecting(None),
        ItemRow::new(
            "a message carrying only the table's own key",
            "SELECT * FROM 'sensors/room-1/temp'",
            TOPIC,
        )
        .message(r#"{"id":"m-1"}"#, Some(json!({ "id": { "S": MARKER } }))),
        ItemRow::new(
            "a payload that is no JSON document, which carries no column for the key",
            "SELECT * FROM 'sensors/room-1/temp'",
            TOPIC,
        )
        .message("not a document", None),
        // What the rule's enablement decides.
        ItemRow::new(
            "a rule stored disabled",
            "SELECT * FROM 'sensors/room-1/temp'",
            TOPIC,
        )
        .enablement(Enablement::Disabled, None),
        ItemRow::new(
            "a rule stored disabled and then enabled",
            "SELECT * FROM 'sensors/room-1/temp'",
            TOPIC,
        )
        .enablement(Enablement::EnabledAfterwards, Some(whole_message_item())),
        // What the action's role decides.
        ItemRow::new(
            "an action whose role grants every action of the service",
            "SELECT * FROM 'sensors/room-1/temp'",
            TOPIC,
        )
        .under(RoleKind::Wildcard),
        ItemRow::new(
            "an action whose role grants another action on the table",
            "SELECT * FROM 'sensors/room-1/temp'",
            TOPIC,
        )
        .under(RoleKind::AnotherAction),
        ItemRow::new(
            "an action whose role grants the write on another table",
            "SELECT * FROM 'sensors/room-1/temp'",
            TOPIC,
        )
        .under(RoleKind::AnotherTarget),
        ItemRow::new(
            "an action whose role carries no inline policy",
            "SELECT * FROM 'sensors/room-1/temp'",
            TOPIC,
        )
        .under(RoleKind::NoPolicy),
        ItemRow::new(
            "an action whose role trusts another service principal",
            "SELECT * FROM 'sensors/room-1/temp'",
            TOPIC,
        )
        .under(RoleKind::AnotherPrincipal),
        // What the table the action names decides.
        ItemRow::new(
            "an action naming a table the account does not hold",
            "SELECT * FROM 'sensors/room-1/temp'",
            TOPIC,
        )
        .writing_to("IotRuleAbsent"),
    ]
}

/// RULE: a message published to a topic an enabled rule's filter covers is written into the table
/// that rule's `dynamoDBv2` action names — one column per attribute of the message its statement
/// selects — under the action's own role, and nothing is written for a rule that does not select
/// the message, a rule that is disabled, a role that does not admit the write, or a table
/// DynamoDB refuses the item for.
#[tokio::test]
async fn a_published_message_is_written_as_the_item_of_every_rule_that_selects_it() {
    for row in item_rows() {
        let emulator = emulator();
        let case = row.case;
        given_table(&emulator, case, TABLE).await;
        // A second table that EXISTS, so a role granted the write on it is refused on the
        // resource element rather than because the table it names is not there. Both creates are
        // made before either status is waited on, so the row spends one activation span.
        given_table(&emulator, case, ANOTHER_TABLE).await;
        let table_arn = active_table_arn(&emulator, case, TABLE).await;
        let another_arn = active_table_arn(&emulator, case, ANOTHER_TABLE).await;
        let target = Target {
            action: PUT_ITEM_ACTION,
            every_action: EVERY_DYNAMODB_ACTION,
            another_action: ANOTHER_DYNAMODB_ACTION,
            resource: &table_arn,
            another_resource: &another_arn,
        };
        // The role the row's action names, and the CONTROL rule's own role.
        let role_arn = given_role(&emulator, case, row.role, target).await;
        let control_role = given_control_role(&emulator, case, PUT_ITEM_ACTION, &table_arn).await;

        emulator
            .create_topic_rule(
                case,
                RULE,
                &payload(
                    row.sql,
                    json!([{ "dynamoDBv2": {
                        "roleArn": role_arn,
                        "putItem": { "tableName": row.table },
                    }}]),
                    row.enablement,
                ),
            )
            .await;
        if row.enablement == Enablement::EnabledAfterwards {
            emulator.enable_topic_rule(case, RULE).await;
        }
        emulator
            .create_topic_rule(
                case,
                CONTROL_RULE,
                &payload(
                    &format!("SELECT * FROM '{CONTROL_TOPIC}'"),
                    json!([{ "dynamoDBv2": {
                        "roleArn": control_role,
                        "putItem": { "tableName": TABLE },
                    }}]),
                    Enablement::Enabled,
                ),
            )
            .await;

        emulator
            .publish(case, row.topic, row.message.as_bytes())
            .await;
        // The CONTROL message is published AFTER the row's, so an item that was going to appear
        // has appeared by the time the control item has.
        emulator
            .publish(
                case,
                CONTROL_TOPIC,
                format!(r#"{{"id":"{CONTROL_MARKER}"}}"#).as_bytes(),
            )
            .await;
        let control = emulator.polled_item(case, TABLE, CONTROL_MARKER).await;
        assert_eq!(
            control,
            Some(json!({ "id": { "S": CONTROL_MARKER } })),
            "case {case:?}: the control rule must write its own item, or this row cannot tell \
             an enforced refusal from a broken rules engine; got {control:?}"
        );

        let written = match &row.expected {
            Some(_) => emulator.polled_item(case, TABLE, row.marker).await,
            None => emulator.item(case, TABLE, row.marker).await,
        };
        assert_eq!(
            written, row.expected,
            "case {case:?}: expected the table to hold {:?}, got {written:?}",
            row.expected
        );
    }
}

// ============================================================================
// `Action$s3`
// ============================================================================

/// One row of the Amazon S3 table.
#[derive(Debug, Clone)]
struct ObjectRow {
    case: &'static str,
    /// The statement the rule is stored with.
    sql: &'static str,
    /// The `key` the action names, which may carry substitution templates.
    key: &'static str,
    /// The `cannedAcl` the action names, or `None` for an action naming none.
    canned_acl: Option<&'static str>,
    /// What the action's role says about the write.
    role: RoleKind,
    /// The bucket the action names.
    bucket: &'static str,
    /// The key the object must be stored under and the body it must hold, or `None` for a row
    /// that must store nothing.
    expected: Option<(&'static str, &'static str)>,
}

/// The literal key the role rows grant the write on, so a grant can name the object exactly.
const LITERAL_KEY: &str = "readings/latest.json";

impl ObjectRow {
    /// A row storing [`MESSAGE`] under [`LITERAL_KEY`] with the granted role.
    fn new(case: &'static str, key: &'static str, expected_key: &'static str) -> Self {
        Self {
            case,
            sql: "SELECT * FROM 'sensors/room-1/temp'",
            key,
            canned_acl: None,
            role: RoleKind::Granted,
            bucket: BUCKET,
            expected: Some((expected_key, MESSAGE)),
        }
    }

    fn selecting(mut self, sql: &'static str, body: &'static str) -> Self {
        self.sql = sql;
        self.expected = self.expected.map(|(key, _)| (key, body));
        self
    }

    fn with_acl(mut self, canned_acl: &'static str) -> Self {
        self.canned_acl = Some(canned_acl);
        self
    }

    fn storing_nothing(mut self) -> Self {
        self.expected = None;
        self
    }

    fn under(mut self, role: RoleKind) -> Self {
        self.role = role;
        if !role.admits() {
            self.expected = None;
        }
        self
    }

    fn writing_to(mut self, bucket: &'static str) -> Self {
        self.bucket = bucket;
        // Every action of the service, so what refuses the write is AMAZON S3 answering about the
        // bucket the action named rather than the role.
        self.role = RoleKind::Wildcard;
        self.expected = None;
        self
    }
}

/// The rows of the Amazon S3 table.
fn object_rows() -> Vec<ObjectRow> {
    vec![
        // What the key the action names decides.
        ObjectRow::new("a literal key", LITERAL_KEY, LITERAL_KEY),
        ObjectRow::new(
            "a key computed from one attribute of the message",
            "readings/${id}.json",
            "readings/m-1.json",
        )
        .under(RoleKind::Wildcard),
        ObjectRow::new(
            "a key computed from the topic and the message, between literal text",
            "archive/${topic(2)}/${id}-reading.json",
            "archive/room-1/m-1-reading.json",
        )
        .under(RoleKind::Wildcard),
        ObjectRow::new(
            "a key computed from a nested attribute path",
            "rooms/${state.reported.room}/${id}",
            "rooms/r1/m-1",
        )
        .under(RoleKind::Wildcard),
        ObjectRow::new(
            "a key computed by arithmetic over the message",
            "temps/${temp * 2}",
            "temps/84",
        )
        .under(RoleKind::Wildcard),
        // Two functions of the AWS IoT SQL library a key is written with, each over a FIXED
        // instant and the message's own members, so the key the action computes is the one this
        // row names rather than one the clock decides.
        ObjectRow::new(
            "a key computed by parse_time over an instant the template names",
            "archive/${parse_time('yyyy/MM/dd', 1767323045678)}/${id}.json",
            "archive/2026/01/02/m-1.json",
        )
        .under(RoleKind::Wildcard),
        ObjectRow::new(
            "a key computed by concat and upper over the topic and the message",
            "${concat(topic(2), '-', upper(id))}.json",
            "room-1-M-1.json",
        )
        .under(RoleKind::Wildcard),
        ObjectRow::new(
            "a key whose template this emulator does not evaluate",
            "${clientid()}.json",
            LITERAL_KEY,
        )
        .under(RoleKind::Wildcard)
        .storing_nothing(),
        ObjectRow::new(
            "a key naming an attribute the message does not carry",
            "readings/${absent}.json",
            LITERAL_KEY,
        )
        .under(RoleKind::Wildcard)
        .storing_nothing(),
        // What the statement selects is the object's BODY.
        ObjectRow::new(
            "a projection, which is the body the object holds",
            LITERAL_KEY,
            LITERAL_KEY,
        )
        .selecting(
            "SELECT id, topic(2) AS level FROM 'sensors/room-1/temp'",
            r#"{"id":"m-1","level":"room-1"}"#,
        ),
        ObjectRow::new(
            "a condition that does not hold for the message",
            LITERAL_KEY,
            LITERAL_KEY,
        )
        .selecting("SELECT * FROM 'sensors/room-1/temp' WHERE temp > 100", MESSAGE)
        .storing_nothing(),
        // What the action's own members and role decide.
        //
        // `cannedAcl` is "the Amazon S3 canned ACL that controls access to the object
        // identified by the object key", so the object is stored with the ACL the action names:
        // one row per value of `CannedAccessControlList`
        // (smithy_query(query="shape",shape="CannedAccessControlList")) that a bucket left at
        // its own default ownership setting takes, which is the owner's own access.
        ObjectRow::new("an action naming the private canned ACL", LITERAL_KEY, LITERAL_KEY)
            .with_acl("private"),
        ObjectRow::new(
            "an action naming the bucket-owner-full-control canned ACL",
            LITERAL_KEY,
            LITERAL_KEY,
        )
        .with_acl("bucket-owner-full-control"),
        ObjectRow::new(
            "an action naming the bucket-owner-read canned ACL",
            LITERAL_KEY,
            LITERAL_KEY,
        )
        .with_acl("bucket-owner-read")
        // A bucket left at its own default ownership setting takes no ACL beyond the owner's
        // own access ("PUT requests that specify bucket owner full control ACLs … are allowed;
        // requests that contain other ACLs … fail and return a 400 error"), so the object store
        // REFUSES this write — which is the action failing, and nothing is stored.
        .storing_nothing(),
        ObjectRow::new(
            "an action whose role grants another action on the object",
            LITERAL_KEY,
            LITERAL_KEY,
        )
        .under(RoleKind::AnotherAction),
        ObjectRow::new(
            "an action whose role grants the write in another bucket",
            LITERAL_KEY,
            LITERAL_KEY,
        )
        .under(RoleKind::AnotherTarget),
        ObjectRow::new(
            "an action whose role carries no inline policy",
            LITERAL_KEY,
            LITERAL_KEY,
        )
        .under(RoleKind::NoPolicy),
        ObjectRow::new(
            "an action whose role trusts another service principal",
            LITERAL_KEY,
            LITERAL_KEY,
        )
        .under(RoleKind::AnotherPrincipal),
        ObjectRow::new(
            "an action naming a bucket the account does not hold",
            LITERAL_KEY,
            LITERAL_KEY,
        )
        .writing_to("iot-rule-absent"),
    ]
}

/// RULE: a message published to a topic an enabled rule's filter covers is stored as the object
/// that rule's `s3` action names — the message its statement selected as the body, under the key
/// the action renders from that same message — under the action's own role, and nothing is stored
/// for a statement that does not select the message, a key this emulator cannot render, a role
/// that does not admit the write, or a bucket Amazon S3 refuses the object for.
#[tokio::test]
async fn a_published_message_is_stored_as_the_object_of_every_rule_that_selects_it() {
    for row in object_rows() {
        let emulator = emulator();
        let case = row.case;
        emulator.create_bucket(case, BUCKET).await;
        // A second bucket that EXISTS, so a role granted the write in it is refused on the
        // resource element rather than because the bucket it names is not there.
        emulator.create_bucket(case, ANOTHER_BUCKET).await;
        let literal_object = format!("arn:aws:s3:::{BUCKET}/{LITERAL_KEY}");
        let another_object = format!("arn:aws:s3:::{ANOTHER_BUCKET}/*");
        let target = Target {
            action: PUT_OBJECT_ACTION,
            every_action: EVERY_S3_ACTION,
            another_action: ANOTHER_S3_ACTION,
            resource: &literal_object,
            another_resource: &another_object,
        };
        // The role the row's action names, and the CONTROL rule's own role, granted the write on
        // the control object alone.
        let control_object = format!("arn:aws:s3:::{BUCKET}/{CONTROL_KEY}");
        let role_arn = given_role(&emulator, case, row.role, target).await;
        let control_role =
            given_control_role(&emulator, case, PUT_OBJECT_ACTION, &control_object).await;

        let mut action = json!({
            "roleArn": role_arn,
            "bucketName": row.bucket,
            "key": row.key,
        });
        if let Some(acl) = row.canned_acl {
            action["cannedAcl"] = json!(acl);
        }
        emulator
            .create_topic_rule(
                case,
                RULE,
                &payload(row.sql, json!([{ "s3": action }]), Enablement::Enabled),
            )
            .await;
        emulator
            .create_topic_rule(
                case,
                CONTROL_RULE,
                &payload(
                    &format!("SELECT * FROM '{CONTROL_TOPIC}'"),
                    json!([{ "s3": {
                        "roleArn": control_role,
                        "bucketName": BUCKET,
                        "key": CONTROL_KEY,
                    }}]),
                    Enablement::Enabled,
                ),
            )
            .await;

        emulator.publish(case, TOPIC, MESSAGE.as_bytes()).await;
        // The CONTROL message is published AFTER the row's, so an object that was going to be
        // stored has been stored by the time the control object has.
        emulator
            .publish(
                case,
                CONTROL_TOPIC,
                format!(r#"{{"id":"{CONTROL_MARKER}"}}"#).as_bytes(),
            )
            .await;
        let control = emulator.polled_object(BUCKET, CONTROL_KEY).await;
        // The control object holds the message published to the control topic, so it is proof the
        // engine ran for THIS emulator rather than proof that some object exists.
        assert_eq!(
            control.as_deref(),
            Some(format!(r#"{{"id":"{CONTROL_MARKER}"}}"#).as_str()),
            "case {case:?}: the control rule must store its own object, or this row cannot tell \
             an enforced refusal from a broken rules engine; got {control:?}"
        );

        let mut expected_keys = vec![CONTROL_KEY.to_string()];
        if let Some((key, _)) = row.expected {
            expected_keys.push(key.to_string());
        }
        expected_keys.sort();
        let keys = emulator.keys(case, BUCKET).await;
        assert_eq!(
            keys, expected_keys,
            "case {case:?}: expected the bucket to hold {expected_keys:?}, got {keys:?}"
        );
        if let Some((key, body)) = row.expected {
            let stored = emulator.polled_object(BUCKET, key).await;
            assert!(
                stored
                    .as_deref()
                    .is_some_and(|held| same_document(held, body)),
                "case {case:?}: expected the object {key} to hold {body}, got {stored:?}"
            );
        }
    }
}

/// RULE, in the emulator as it is ASSEMBLED: the two storage actions reach their targets with
/// every service of this emulator mounted, so a rule action's own in-process call is served by
/// the service that owns it rather than claimed by another.
///
/// One case each, because what this adds to the tables above is the ASSEMBLY: the key is
/// computed from the message, the item's columns are the message's attributes, and both are read
/// back through the owning service's own reader.
#[tokio::test]
async fn the_assembled_emulator_runs_both_storage_actions_of_a_selecting_rule() {
    let case = "every service mounted, as the entrypoint assembles them";
    let emulator = assembled_emulator();
    emulator.create_bucket(case, BUCKET).await;
    given_table(&emulator, case, TABLE).await;
    // Waited on for the status DynamoDB serves items on; the ARN it reports is no part of this
    // case, whose one grant covers every call either action makes.
    active_table_arn(&emulator, case, TABLE).await;
    let role = given_control_role(
        &emulator,
        case,
        // Both calls under one grant: what this case is about is the ASSEMBLY, not the
        // permission boundary the tables above span.
        "*",
        "*",
    )
    .await;

    emulator
        .create_topic_rule(
            case,
            RULE,
            &payload(
                "SELECT * FROM 'sensors/room-1/temp'",
                json!([
                    { "s3": {
                        "roleArn": role,
                        "bucketName": BUCKET,
                        "key": "readings/${id}.json",
                    }},
                    { "dynamoDBv2": {
                        "roleArn": role,
                        "putItem": { "tableName": TABLE },
                    }},
                ]),
                Enablement::Enabled,
            ),
        )
        .await;
    emulator.publish(case, TOPIC, MESSAGE.as_bytes()).await;

    let stored = emulator.polled_object(BUCKET, "readings/m-1.json").await;
    assert!(
        stored
            .as_deref()
            .is_some_and(|held| same_document(held, MESSAGE)),
        "case {case:?}: expected the object readings/m-1.json to hold the published message, \
         got {stored:?}"
    );
    let written = emulator.polled_item(case, TABLE, MARKER).await;
    assert_eq!(
        written,
        Some(whole_message_item()),
        "case {case:?}: expected the table to hold the message's own columns, got {written:?}"
    );
}

/// Whether the object's body is the message `expected`: the same text, or the same JSON DOCUMENT
/// where both are documents — a projected document's members are a set, and the order a JSON
/// object writes them in carries no meaning.
fn same_document(held: &str, expected: &str) -> bool {
    if held == expected {
        return true;
    }
    match (
        serde_json::from_str::<Value>(held),
        serde_json::from_str::<Value>(expected),
    ) {
        (Ok(held), Ok(expected)) => held == expected,
        _ => false,
    }
}
