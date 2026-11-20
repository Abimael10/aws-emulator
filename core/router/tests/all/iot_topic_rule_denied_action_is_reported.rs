// SPDX-License-Identifier: CC0-1.0

//! What a topic rule DOES when the role its action names does not allow the call that action
//! makes, end to end over the real `MultiServiceRouter`: the whole emulator on one engine, a
//! message published to the IoT data plane, and both halves of the permission contract read
//! where a client reads them — on the action's TARGET, and in the report the rule's
//! `errorAction` was handed.
//!
//! RULE: a rule's action names "the ARN of the IAM role that grants access" to its target and
//! AWS IoT performs the action AS a SESSION of that role, so the call is made only while the
//! permissions in force on the role allow it on the target the action names. When they do not,
//! TWO things hold together: the action is not performed — nothing reaches the target — and the
//! failure is REPORTED, because "the action to take when an error occurs"
//! (`TopicRulePayload$errorAction`) is performed instead, with the rule-failure report as its
//! message: the rule's name, the topic, the published payload, and one failure per action that
//! was refused, naming the shape of the action, the ARN of the target it failed on, and the
//! refusal in the words the target service refuses an unauthorized call with — `User:
//! <principal> is not authorized to perform: <action> on resource: <resource> because no
//! identity-based policy allows the <action> action`, whose principal is the SESSION of the
//! action's role (`arn:<partition>:sts::<account>:assumed-role/<role name>/<session name>`) and
//! not the role's own ARN, since a role is never acted as directly. Nothing of either half is
//! reported to the publisher, which is answered 200 whatever the rules did with the message,
//! which is why every assertion below is made on a TARGET.
//!
//! Four crates have to agree for that to hold — the IoT data plane takes the publish, the IoT
//! control plane holds the rule and runs the engine, IAM holds the action's role, and DynamoDB
//! or Amazon S3 takes (or never sees) the write — so the suite lives here: a crate-local suite
//! can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over rather than along one recorded case:
//! which ACTION the rule carries and therefore which call on which kind of target is withheld
//! (`dynamoDBv2` and one `dynamodb:PutItem` on a table, `s3` and one `s3:PutObject` on an
//! object, `republish` and one `iot:Publish` on a topic); what the action's ROLE says about that
//! call (no inline policy at all, another action of the same service on the same target, the
//! call itself on another target of the same type, and the call itself — the permitted leg,
//! which must deliver); and how IAM HOLDS the role (at the account root and under a path, which
//! the session principal names the role out of).
//!
//! Every row that expects NOTHING written is judged beside a write that IS made for the same
//! message — the failure report, stored by the error action's own role — so no row can pass
//! against an emulator whose rules engine never ran at all.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::SERVICE_PRINCIPAL;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The role-session name the rules engine performs a rule's actions under, written out here
/// rather than read from the IoT crate's own constant so this suite pins it.
const ROLE_SESSION_NAME: &str = "AWSIoTRulesEngine";

/// The table every `dynamoDBv2` action of these rows names, and a second table that exists only
/// so a role can be granted the write on ANOTHER table's ARN.
const TABLE: &str = "IotDeniedItems";
const ANOTHER_TABLE: &str = "IotDeniedElsewhere";

/// The partition key the table carries, which is the column the published message supplies.
const HASH_KEY: &str = "id";

/// The bucket the `s3` action of a row writes into, and the one every row's failure REPORT is
/// stored in.
const BUCKET: &str = "iot-denied-actions";

/// The key prefix a row's failure report is stored under, kept apart from the prefix an `s3`
/// action of a row writes to so one is never read for the other.
const REPORT_PREFIX: &str = "failures";
const DATA_PREFIX: &str = "readings";

/// The topic a `republish` action of a row republishes to, and the rule over that topic whose
/// own `dynamoDBv2` action writes the republished message — so a republish that was made is
/// observable as an item, and one that was withheld as the absence of it.
const STAGED_TOPIC: &str = "iot/denied/staged";
const STAGED_RULE: &str = "iot_denied_staged";

/// The role the error action and the staged rule both run under: one the rules engine may
/// assume, granted every call on every resource. Its own role rather than a row's, because a row
/// varies what the ROW's role grants and a report judged by that grant would report nothing.
const CONTROL_ROLE: &str = "iot_denied_control";

/// The SQL version a rule is stored with (`TopicRulePayload$awsIotSqlVersion`).
const SQL_VERSION: &str = "2016-03-23";

/// The path the one row about a role held under one creates its role under. IAM reports an
/// entity's path with the leading and trailing `/` it was created with.
const ROLE_PATH: &str = "/service-role/";

/// The service principal every role below trusts: the one the rules engine acts as, taken from
/// the IoT crate itself, so every role these rows create is one it may assume.
///
/// Each row is therefore about what the role's own PERMISSIONS say, which is the half of the
/// decision that carries a sentence; the trust half is pinned by
/// `iot_topic_rule_storage_actions.rs`.
const TRUSTED_PRINCIPAL: &str = SERVICE_PRINCIPAL;

/// The IAM action each rule action performs on its target, as each service publishes it:
/// DynamoDB's `PutItem` "requires permission for the `dynamodb:PutItem` action", Amazon S3's
/// `PutObject` the `s3:PutObject` action, and republishing IS a publish to the message broker,
/// which is `iot:Publish`.
const PUT_ITEM_ACTION: &str = "dynamodb:PutItem";
const PUT_OBJECT_ACTION: &str = "s3:PutObject";
const IOT_PUBLISH_ACTION: &str = "iot:Publish";

/// Another action of DynamoDB on the same table, for the role granted something other than the
/// write.
const ANOTHER_DYNAMODB_ACTION: &str = "dynamodb:GetItem";

/// The shape of each action as the model names it, which is what a failure reports as its
/// `failedAction`.
const DYNAMODB_FAILED_ACTION: &str = "DynamoDBv2Action";
const S3_FAILED_ACTION: &str = "S3Action";
const REPUBLISH_FAILED_ACTION: &str = "RepublishAction";

/// How long one row's observation of a write is given, and how long between looks. Generous
/// beside the work a row does — every write is made before the publish is answered — without
/// making a genuine regression cost minutes: what each wait is ON is a condition, never a span.
const DEADLINE: Duration = Duration::from_secs(30);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long a table is given to reach `ACTIVE`, the status DynamoDB serves items on ("You can
/// perform read and write operations only on an ACTIVE table"). Generous beside that service's
/// own transition span.
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

    /// The body of the object `key` in [`BUCKET`], or `None` when the store holds no such
    /// object.
    async fn object(&self, key: &str) -> Option<String> {
        let answer = self
            .send(
                "GET",
                &format!("/{BUCKET}/{key}"),
                "s3",
                "application/octet-stream",
                &[],
                Vec::new(),
            )
            .await;
        (answer.status == 200).then(|| answer.text())
    }

    /// The object `key` once it is there, or `None` once [`DEADLINE`] passes without it: the
    /// write is made while the publish is being served, so this is what a client polling the
    /// downstream service does.
    async fn polled_object(&self, key: &str) -> Option<String> {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(body) = self.object(key).await {
                return Some(body);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }

    /// The item `marker` keys in [`TABLE`], read with `ConsistentRead` so what is asserted is
    /// what the store holds rather than a stale read.
    async fn item(&self, case: &str, marker: &str) -> Option<Value> {
        let read = self
            .dynamodb(
                case,
                "GetItem",
                &json!({
                    "TableName": TABLE,
                    "Key": { HASH_KEY: { "S": marker } },
                    "ConsistentRead": true,
                }),
            )
            .await;
        read.get("Item").cloned()
    }

    /// The item `marker` keys once it is there, or `None` once [`DEADLINE`] passes without it.
    async fn polled_item(&self, case: &str, marker: &str) -> Option<Value> {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(item) = self.item(case, marker).await {
                return Some(item);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }
}

/// A table of `name` keyed by [`HASH_KEY`], as `CreateTable` takes it, answered with the ARN
/// DynamoDB persisted — the value a role's grant names the table by.
async fn given_table(emulator: &Emulator, case: &str, name: &str) -> String {
    let created = emulator
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
    created["TableDescription"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateTable({name}) reported no ARN"))
        .to_string()
}

/// Wait until DynamoDB reports `name` `ACTIVE`, the status it serves items on.
async fn when_active(emulator: &Emulator, case: &str, name: &str) {
    let deadline = Instant::now() + ACTIVATION_DEADLINE;
    loop {
        let described = emulator
            .dynamodb(case, "DescribeTable", &json!({ "TableName": name }))
            .await;
        if described["Table"]["TableStatus"] == json!("ACTIVE") {
            return;
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

/// A role of `name` the rules engine may assume, held under `path` where a row names one and at
/// the account root otherwise, granted `allowed` where a row grants anything at all — answered
/// with the ARN IAM persisted, which is the value the action's `roleArn` carries.
async fn given_role(
    emulator: &Emulator,
    case: &str,
    name: &str,
    path: Option<&str>,
    allowed: Option<(&str, &str)>,
) -> String {
    let trust = trust_policy(TRUSTED_PRINCIPAL);
    let mut params = vec![("RoleName", name), ("AssumeRolePolicyDocument", &trust)];
    if let Some(path) = path {
        params.push(("Path", path));
    }
    let created = emulator.iam(case, "CreateRole", &params).await;
    if let Some((action, resource)) = allowed {
        let document = identity_policy(action, resource);
        emulator
            .iam(
                case,
                "PutRolePolicy",
                &[
                    ("RoleName", name),
                    ("PolicyName", "rule-action"),
                    ("PolicyDocument", &document),
                ],
            )
            .await;
    }
    element(&created, "Arn")
}

/// The `topicRulePayload` one rule is stored with: an enabled rule carrying `actions` and, where
/// a row has one, the `errorAction` its failures are reported to.
fn payload(sql: &str, actions: Value, error_action: Option<Value>) -> Value {
    let mut payload = json!({
        "sql": sql,
        "awsIotSqlVersion": SQL_VERSION,
        "ruleDisabled": false,
        "actions": actions,
    });
    if let Some(error_action) = error_action {
        payload["errorAction"] = error_action;
    }
    payload
}

/// Which action the row's rule carries, and so which call on which kind of target its role is
/// judged over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionKind {
    /// `Action$dynamoDBv2`: one `dynamodb:PutItem` into the table its `putItem` names.
    DynamoDbV2,
    /// `Action$s3`: one `s3:PutObject` under the key it names.
    S3,
    /// `Action$republish`: one `iot:Publish` of the message onto the topic it names, which the
    /// rule over that topic then writes as an item.
    Republish,
}

/// What the ROLE a row's action names grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// Exactly the call the action makes, on exactly the target it names — the permitted leg.
    TheCall,
    /// Another action of the same service on the same target.
    AnotherAction,
    /// The call the action makes, on another target of the same type.
    AnotherTarget,
    /// No identity-based policy at all.
    Nothing,
}

impl Grant {
    /// Whether a role granting this lets the rules engine make the action's call.
    fn admits(self) -> bool {
        self == Self::TheCall
    }
}

/// One row: a rule carrying one action under one role, and the error action every row carries.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// The rule's name, which is also the name of the role its action runs under, the marker its
    /// message carries and the key its report is stored under — so two rows share nothing.
    rule: &'static str,
    /// The topic the row publishes to, which its rule's statement selects from.
    topic: &'static str,
    kind: ActionKind,
    grant: Grant,
    /// The path IAM holds the row's role under, for the row about a role held under one.
    role_path: Option<&'static str>,
}

impl Row {
    fn new(case: &'static str, rule: &'static str, topic: &'static str) -> Self {
        Self {
            case,
            rule,
            topic,
            kind: ActionKind::DynamoDbV2,
            grant: Grant::Nothing,
            role_path: None,
        }
    }

    fn granting(mut self, grant: Grant) -> Self {
        self.grant = grant;
        self
    }

    fn acting(mut self, kind: ActionKind) -> Self {
        self.kind = kind;
        self
    }

    fn under_path(mut self) -> Self {
        self.role_path = Some(ROLE_PATH);
        self
    }

    /// The key an `s3` action of this row writes to, which is also the resource its role is
    /// judged on.
    fn data_key(&self) -> String {
        format!("{DATA_PREFIX}/{}.json", self.rule)
    }

    /// The key this row's failure report is stored under by the error action.
    fn report_key(&self) -> String {
        format!("{REPORT_PREFIX}/{}.json", self.rule)
    }

    /// The message published: one document carrying the row's own marker under the table's
    /// partition key, plus one more attribute, so what is written is a real item.
    fn message(&self) -> String {
        json!({ HASH_KEY: self.rule, "temp": 42 }).to_string()
    }

    /// The item the table holds once the write is made: one column per attribute of the message.
    fn expected_item(&self) -> Value {
        json!({ HASH_KEY: { "S": self.rule }, "temp": { "N": "42" } })
    }
}

/// Every row, each with its own rule, role, topic and marker.
fn rows() -> Vec<Row> {
    vec![
        // `dynamoDBv2`: the issue's own class — the write a role may not make, and the write it
        // may.
        Row::new(
            "a dynamoDBv2 action whose role carries no identity-based policy",
            "iot_denied_ddb_no_policy",
            "sensors/denied/no-policy",
        ),
        Row::new(
            "a dynamoDBv2 action whose role grants another action on the table",
            "iot_denied_ddb_another_action",
            "sensors/denied/another-action",
        )
        .granting(Grant::AnotherAction),
        Row::new(
            "a dynamoDBv2 action whose role grants the write on another table",
            "iot_denied_ddb_another_table",
            "sensors/denied/another-table",
        )
        .granting(Grant::AnotherTarget),
        Row::new(
            "a dynamoDBv2 action whose role IAM holds under a path",
            "iot_denied_ddb_path",
            "sensors/denied/path",
        )
        .under_path(),
        Row::new(
            "a dynamoDBv2 action whose role grants the write",
            "iot_denied_ddb_granted",
            "sensors/denied/granted",
        )
        .granting(Grant::TheCall),
        // `s3`: the same rule over another service's call and another kind of target.
        Row::new(
            "an s3 action whose role carries no identity-based policy",
            "iot_denied_s3_no_policy",
            "sensors/denied/s3",
        )
        .acting(ActionKind::S3),
        // `republish`: the call is a publish to the broker, observed through the rule over the
        // topic republished to.
        Row::new(
            "a republish action whose role carries no identity-based policy",
            "iot_denied_republish_no_policy",
            "sensors/denied/republish",
        )
        .acting(ActionKind::Republish),
        Row::new(
            "a republish action whose role grants the publish",
            "iot_denied_republish_granted",
            "sensors/denied/republish-granted",
        )
        .acting(ActionKind::Republish)
        .granting(Grant::TheCall),
    ]
}

/// RULE: a topic rule's action is performed as a SESSION of the role it names and only while the
/// permissions in force on that role allow the call it makes on the target it names; otherwise
/// nothing reaches the target and the rule's `errorAction` is handed the rule-failure report,
/// whose failure names the action's shape, the target's ARN and the access-denied sentence about
/// that session.
#[tokio::test]
async fn an_action_its_role_does_not_allow_writes_nothing_and_is_reported() {
    for row in rows() {
        let case = row.case;
        let emulator = emulator();
        emulator.create_bucket(case, BUCKET).await;
        let table_arn = given_table(&emulator, case, TABLE).await;
        // A second table that EXISTS, so a role granted the write on it is refused on the
        // resource element rather than because the table it names is not there. Both creates are
        // made before either status is waited on, so the row spends one activation span.
        let another_table_arn = given_table(&emulator, case, ANOTHER_TABLE).await;
        when_active(&emulator, case, TABLE).await;
        when_active(&emulator, case, ANOTHER_TABLE).await;

        // The call the row's action makes and the target it makes it on, which is what its role
        // is judged over and what the refusal names.
        let data_key = row.data_key();
        let (iam_action, resource, another_action, another_resource) = match row.kind {
            ActionKind::DynamoDbV2 => (
                PUT_ITEM_ACTION,
                table_arn.clone(),
                ANOTHER_DYNAMODB_ACTION,
                another_table_arn.clone(),
            ),
            ActionKind::S3 => (
                PUT_OBJECT_ACTION,
                // Amazon S3's ARNs carry neither a Region nor an account id, because a bucket
                // name is global.
                format!("arn:aws:s3:::{BUCKET}/{data_key}"),
                "s3:GetObject",
                format!("arn:aws:s3:::{BUCKET}/{DATA_PREFIX}/elsewhere.json"),
            ),
            ActionKind::Republish => (
                IOT_PUBLISH_ACTION,
                format!("arn:aws:iot:{REGION}:{ACCOUNT}:topic/{STAGED_TOPIC}"),
                "iot:Receive",
                format!("arn:aws:iot:{REGION}:{ACCOUNT}:topic/{}", row.topic),
            ),
        };
        let allowed = match row.grant {
            Grant::TheCall => Some((iam_action, resource.as_str())),
            Grant::AnotherAction => Some((another_action, resource.as_str())),
            Grant::AnotherTarget => Some((iam_action, another_resource.as_str())),
            Grant::Nothing => None,
        };
        let role_arn = given_role(&emulator, case, row.rule, row.role_path, allowed).await;
        // The role the ERROR ACTION and the staged rule run under: every call on every resource,
        // so what either of them does says nothing about the row's own grant.
        let control_role = given_role(&emulator, case, CONTROL_ROLE, None, Some(("*", "*"))).await;

        let action = match row.kind {
            ActionKind::DynamoDbV2 => json!({ "dynamoDBv2": {
                "roleArn": role_arn,
                "putItem": { "tableName": TABLE },
            }}),
            ActionKind::S3 => json!({ "s3": {
                "roleArn": role_arn,
                "bucketName": BUCKET,
                "key": data_key,
            }}),
            ActionKind::Republish => json!({ "republish": {
                "roleArn": role_arn,
                "topic": STAGED_TOPIC,
            }}),
        };
        let report_key = row.report_key();
        emulator
            .create_topic_rule(
                case,
                row.rule,
                &payload(
                    &format!("SELECT * FROM '{}'", row.topic),
                    json!([action]),
                    Some(json!({ "s3": {
                        "roleArn": control_role,
                        "bucketName": BUCKET,
                        "key": report_key,
                    }})),
                ),
            )
            .await;
        // The rule over the topic a `republish` action republishes to: it writes the republished
        // message as an item, so a republish that was made is observable and one that was
        // withheld is the absence of that item.
        emulator
            .create_topic_rule(
                case,
                STAGED_RULE,
                &payload(
                    &format!("SELECT * FROM '{STAGED_TOPIC}'"),
                    json!([{ "dynamoDBv2": {
                        "roleArn": control_role,
                        "putItem": { "tableName": TABLE },
                    }}]),
                    None,
                ),
            )
            .await;

        let message = row.message();
        emulator.publish(case, row.topic, message.as_bytes()).await;

        if row.grant.admits() {
            // THE PERMITTED LEG: the action was performed, so its target holds what the message
            // was written as — and no failure was reported for it.
            match row.kind {
                ActionKind::DynamoDbV2 | ActionKind::Republish => {
                    let written = emulator.polled_item(case, row.rule).await;
                    assert_eq!(
                        written,
                        Some(row.expected_item()),
                        "case {case:?}: expected the table to hold the message's own columns, \
                         got {written:?}"
                    );
                }
                ActionKind::S3 => {
                    let stored = emulator.polled_object(&data_key).await;
                    assert_eq!(
                        stored.as_deref(),
                        Some(message.as_str()),
                        "case {case:?}: expected the object {data_key} to hold the published \
                         message, got {stored:?}"
                    );
                }
            }
            let reported = emulator.object(&report_key).await;
            assert_eq!(
                reported, None,
                "case {case:?}: an action that was performed is no failure, so the errorAction \
                 must report nothing; got {reported:?}"
            );
            continue;
        }

        // THE REFUSED LEG: the failure report is what the error action stored, and it is also
        // this row's proof that the rules engine evaluated the publish at all.
        let reported = emulator.polled_object(&report_key).await.unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the rule's errorAction to store the rule-failure \
                 report at {report_key}, got nothing"
            )
        });
        let report: Value = serde_json::from_str(&reported).unwrap_or(Value::Null);
        assert_eq!(
            report["ruleName"],
            json!(row.rule),
            "case {case:?}: expected the report to name the rule, got {reported}"
        );
        assert_eq!(
            report["topic"],
            json!(row.topic),
            "case {case:?}: expected the report to name the topic, got {reported}"
        );
        let encoded = report["base64OriginalPayload"].as_str().unwrap_or_default();
        let decoded = aws_smithy_types::base64::decode(encoded).unwrap_or_default();
        assert_eq!(
            String::from_utf8_lossy(&decoded),
            message,
            "case {case:?}: expected the report to carry the published payload, got {reported}"
        );
        let failures = report["failures"].as_array().cloned().unwrap_or_default();
        let failure = failures.first().cloned().unwrap_or(Value::Null);
        assert_eq!(
            failures.len(),
            1,
            "case {case:?}: expected exactly the one refused action to be reported, got \
             {reported}"
        );
        let failed_action = match row.kind {
            ActionKind::DynamoDbV2 => DYNAMODB_FAILED_ACTION,
            ActionKind::S3 => S3_FAILED_ACTION,
            ActionKind::Republish => REPUBLISH_FAILED_ACTION,
        };
        assert_eq!(
            failure["failedAction"],
            json!(failed_action),
            "case {case:?}: expected the failure to name the action's shape, got {reported}"
        );
        assert_eq!(
            failure["failedResource"],
            json!(resource),
            "case {case:?}: expected the failure to name the target it failed on, got {reported}"
        );
        // The refusal AWS words for a call the permissions in force on the principal do not
        // allow, whose principal is the SESSION of the action's role: the role's own partition
        // and account, and the name IAM holds the role under — which, for a role held under a
        // path, is its last segment alone.
        let expected = format!(
            "User: arn:aws:sts::{ACCOUNT}:assumed-role/{}/{ROLE_SESSION_NAME} is not authorized \
             to perform: {iam_action} on resource: {resource} because no identity-based policy \
             allows the {iam_action} action",
            row.rule
        );
        assert_eq!(
            failure["errorMessage"],
            json!(expected),
            "case {case:?}: expected the failure to read {expected:?}, got {reported}"
        );

        // ... and NOTHING reached the action's target.
        match row.kind {
            ActionKind::DynamoDbV2 | ActionKind::Republish => {
                let written = emulator.item(case, row.rule).await;
                assert_eq!(
                    written, None,
                    "case {case:?}: a refused action writes nothing, so the table must hold no \
                     item for this message; got {written:?}"
                );
            }
            ActionKind::S3 => {
                let stored = emulator.object(&data_key).await;
                assert_eq!(
                    stored, None,
                    "case {case:?}: a refused action writes nothing, so the bucket must hold no \
                     object at {data_key}; got {stored:?}"
                );
            }
        }
    }
}
