// SPDX-License-Identifier: CC0-1.0

//! How this service REFUSES a create whose name is already taken, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is the
//! status, the `x-amzn-errortype` header a client resolves the error shape from, the
//! `message` an SDK surfaces, and WHICH members the 409 body carries.
//!
//! The rule under test: AWS IoT has no single already-exists sentence. Each resource
//! FAMILY states the collision in its own fixed text whose only variable part is the
//! identifier the REQUEST carried, and each family's 409 carries only the members that
//! family sends:
//!
//! * the NAMED-RESOURCE families refused by name — a policy, a mitigation action — word
//!   it `<Family> cannot be created - name already exists (name=<name>)`, and a policy
//!   DOES report the record that exists, in `resourceId`/`resourceArn`;
//! * the ROLE ALIAS family spells its noun as two words and scopes the collision to the
//!   account — `Role alias <name> already exists for this account` — and reports the
//!   record that exists too;
//! * the AUTHORIZER family words it `Authorizer <name> already exist for this account`
//!   and reports the record too;
//! * the JOBS and COMMANDS planes word it `<Family> <id> already exists.`, with the
//!   terminating period, and send no identity members;
//! * the THING family names the conflicting dimension and the account —
//!   `Thing <name> already exists in account with different attributes`;
//! * the THING GROUP and THING TYPE families name theirs as `properties` in the
//!   `current account`, with the family noun in two capitalised words, and a dynamic
//!   group's create shares the thing-group sentence because both kinds share one
//!   namespace;
//! * the TOPIC RULE family single-quotes the name: `Topic Rule '<name>' already exists
//!   for account`;
//! * the STREAM family states what the REQUEST asked for, as every refusal of one stream
//!   does: `Requested stream <streamId> stream already exists.`, with the terminating
//!   period, and DOES report the record that exists;
//! * the SOFTWARE PACKAGE family reports a duplicate REQUEST rather than an existing
//!   resource, as a conditional-check failure.
//!
//! The table spans what the rule generalises over — the family, the operation, the
//! method, and the form and length of the identifier within that family's modelled
//! pattern — rather than one recorded request. The last test pins the other side of the
//! same decision: a repeat create that collides with NOTHING (the same configuration
//! again, or the same idempotency token) is still a 200, so the wording above is reached
//! only by a real collision.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id this emulator serves (`engine::emulator_account_id`), used to build
/// the job target ARNs a `CreateJob` needs.
const ACCOUNT_ID: &str = "000000000000";

/// The region every request in this suite is signed for. None of these refusals is
/// regional — the collisions are account-wide — so no sentence depends on it.
const REGION: &str = "ap-southeast-2";

/// The wire error shape of a resource that already exists.
const ALREADY_EXISTS: &str = "ResourceAlreadyExistsException";

/// The wire error shape the jobs-adjacent planes (commands, packages) refuse a
/// collision with.
const CONFLICT: &str = "ConflictException";

/// A policy document `CreatePolicy` accepts: one `Allow` statement.
fn policy_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": "iot:Connect", "Resource": "*" }],
    })
    .to_string()
}

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name:
/// the scope is how the router picks the claiming service. The signature itself is never
/// checked here — the harness turns request authentication off, exactly as an
/// unauthenticated emulator runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// A string member of the body, or `None` when the body carries none.
    fn text(&self, name: &str) -> Option<&str> {
        self.body.get(name).and_then(Value::as_str)
    }

    /// The `message` the body carries, or `""` when it carries none.
    fn message(&self) -> &str {
        self.text("message").unwrap_or_default()
    }
}

/// One request: the method, the path (query string included) and the body, if any.
#[derive(Debug, Clone)]
struct Call {
    method: &'static str,
    path: String,
    body: Option<Value>,
}

impl Call {
    fn new(method: &'static str, path: String, body: Option<Value>) -> Self {
        Self { method, path, body }
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it,
/// reached the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, call: &Call) -> Reply {
        let payload = call.body.as_ref().map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(call.method)
            .uri(format!(
                "https://iot.{REGION}.amazonaws.com{path}",
                path = call.path
            ))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_type,
            body,
        }
    }
}

/// One collision: what creates the resource, the create that must be refused, and the
/// refusal that must answer it.
#[derive(Debug)]
struct Case {
    /// What this row is, for the assertion messages.
    case: String,
    /// The calls that put the resource in place; every one must answer 200.
    setup: Vec<Call>,
    /// The create that collides with it.
    retry: Call,
    error_type: &'static str,
    message: String,
    /// `Some((resourceId, arn_member))` for the families whose refusal reports the record
    /// that already exists: the `resourceId` it must carry, and the member of the
    /// successful create's output whose ARN its `resourceArn` must repeat. `None` for the
    /// families that send the message alone.
    identity: Option<(String, &'static str)>,
}

/// Names of the form `^[a-zA-Z0-9:_-]+$` within 1..128 — the registry families'
/// `ThingName` / `ThingGroupName` / `ThingTypeName` constraint — differing in length,
/// case and separator.
fn registry_names() -> Vec<String> {
    vec![
        "a".to_string(),
        "fleet-device_1".to_string(),
        "Edge:Node_7".to_string(),
        "n".repeat(128),
    ]
}

/// Identifiers of the form `^[a-zA-Z0-9_-]+$` within 1..64 — the jobs and commands
/// planes' `JobId` / `CommandId` constraint.
fn plane_ids() -> Vec<String> {
    vec![
        "x".to_string(),
        "nightly-update".to_string(),
        "Reboot_42".to_string(),
        "i".repeat(64),
    ]
}

/// Names of the form `^[\w=,@-]+$` within 1..128 — the `AuthorizerName` / `RoleAlias`
/// constraint, which admits punctuation the registry names do not.
fn punctuated_names() -> Vec<String> {
    vec![
        "A".to_string(),
        "lambda-authorizer".to_string(),
        "Auth=1,2@edge".to_string(),
        "z".repeat(128),
    ]
}

/// Names of the form `^[\w+=,.@-]+$` within 1..128 — the `PolicyName` constraint.
fn policy_names() -> Vec<String> {
    vec![
        "p".to_string(),
        "connect-only".to_string(),
        "Policy+1,v.2@edge".to_string(),
        "p".repeat(128),
    ]
}

/// Names of the form `^[a-zA-Z0-9-_.]+$` within 1..128 — the `PackageName` constraint.
fn package_names() -> Vec<String> {
    vec![
        "p".to_string(),
        "fleet.sensor-pkg_1".to_string(),
        "Firmware.2".to_string(),
        "k".repeat(128),
    ]
}

/// Names of the form `^[a-zA-Z0-9_]+$` within 1..128 — the `RuleName` constraint, which
/// admits neither a dash nor a colon.
fn rule_names() -> Vec<String> {
    vec![
        "r".to_string(),
        "route_to_lambda".to_string(),
        "Rule9".to_string(),
        "u".repeat(128),
    ]
}

/// Ids of the form `^[a-zA-Z0-9_-]+$` within 1..128 — the `StreamId` constraint, which
/// admits neither a dot nor a colon.
fn stream_ids() -> Vec<String> {
    vec![
        "s".to_string(),
        "firmware-v2_rc".to_string(),
        "Firmware9".to_string(),
        "m".repeat(128),
    ]
}

/// An idempotency token of the form `^\S{36,64}$`, which `CreatePackage` binds to the
/// `clientToken` query parameter.
fn client_token(seed: char) -> String {
    std::iter::repeat_n(seed, 40).collect()
}

/// Every row of the table: each family, created and then created again under the same
/// identifier, over every one of this service's creates that collides on it.
fn cases() -> Vec<Case> {
    let mut rows = Vec::new();

    for name in registry_names() {
        // A thing: the conflicting dimension is its ATTRIBUTES, since a repeat create of
        // the same attributes succeeds.
        rows.push(Case {
            case: format!("CreateThing on the registered thing {name:?}"),
            setup: vec![Call::new(
                "POST",
                format!("/things/{name}"),
                Some(json!({ "attributePayload": { "attributes": { "model": "a1" } } })),
            )],
            retry: Call::new(
                "POST",
                format!("/things/{name}"),
                Some(json!({ "attributePayload": { "attributes": { "model": "b2" } } })),
            ),
            error_type: ALREADY_EXISTS,
            message: format!("Thing {name} already exists in account with different attributes"),
            identity: None,
        });

        // A thing group: the conflicting dimension is its PROPERTIES.
        rows.push(Case {
            case: format!("CreateThingGroup on the registered group {name:?}"),
            setup: vec![Call::new(
                "POST",
                format!("/thing-groups/{name}"),
                Some(json!({ "thingGroupProperties": { "thingGroupDescription": "first" } })),
            )],
            retry: Call::new(
                "POST",
                format!("/thing-groups/{name}"),
                Some(json!({ "thingGroupProperties": { "thingGroupDescription": "second" } })),
            ),
            error_type: ALREADY_EXISTS,
            message: format!(
                "Thing Group {name} already exists in current account with different properties"
            ),
            identity: None,
        });

        // The two kinds of group share one namespace, so a DYNAMIC create under a name a
        // static group holds is the thing-group family's collision.
        rows.push(Case {
            case: format!("CreateDynamicThingGroup on the registered group {name:?}"),
            setup: vec![Call::new(
                "POST",
                format!("/thing-groups/{name}"),
                Some(json!({ "thingGroupProperties": { "thingGroupDescription": "first" } })),
            )],
            retry: Call::new(
                "POST",
                format!("/dynamic-thing-groups/{name}"),
                Some(json!({ "queryString": "thingName:*" })),
            ),
            error_type: ALREADY_EXISTS,
            message: format!(
                "Thing Group {name} already exists in current account with different properties"
            ),
            identity: None,
        });

        // A thing type: the thing-group family's sentence with this family's noun.
        rows.push(Case {
            case: format!("CreateThingType on the registered type {name:?}"),
            setup: vec![Call::new(
                "POST",
                format!("/thing-types/{name}"),
                Some(json!({ "thingTypeProperties": { "thingTypeDescription": "first" } })),
            )],
            retry: Call::new(
                "POST",
                format!("/thing-types/{name}"),
                Some(json!({ "thingTypeProperties": { "thingTypeDescription": "second" } })),
            ),
            error_type: ALREADY_EXISTS,
            message: format!(
                "Thing Type {name} already exists in current account with different properties"
            ),
            identity: None,
        });
    }

    for id in plane_ids() {
        // A job: the id "must be unique for your account", and the jobs plane's sentence
        // ends in a period.
        let target = format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/targeted");
        rows.push(Case {
            case: format!("CreateJob on the existing job {id:?}"),
            setup: vec![
                Call::new("POST", "/things/targeted".to_string(), Some(json!({}))),
                Call::new(
                    "PUT",
                    format!("/jobs/{id}"),
                    Some(json!({ "targets": [target.clone()], "document": "{}" })),
                ),
            ],
            retry: Call::new(
                "PUT",
                format!("/jobs/{id}"),
                Some(json!({
                    "targets": [target],
                    "document": "{}",
                    "description": "a second attempt",
                })),
            ),
            error_type: ALREADY_EXISTS,
            message: format!("Job {id} already exists."),
            identity: None,
        });

        // A command: the commands plane shares the jobs plane's sentence.
        rows.push(Case {
            case: format!("CreateCommand on the existing command {id:?}"),
            setup: vec![Call::new("PUT", format!("/commands/{id}"), Some(json!({})))],
            retry: Call::new(
                "PUT",
                format!("/commands/{id}"),
                Some(json!({ "description": "a second attempt" })),
            ),
            error_type: CONFLICT,
            message: format!("Command {id} already exists."),
            identity: None,
        });
    }

    for name in package_names() {
        // A package: this family reports the duplicate REQUEST, not the stored package.
        rows.push(Case {
            case: format!("CreatePackage on the catalogued package {name:?}"),
            setup: vec![Call::new(
                "PUT",
                format!("/packages/{name}?clientToken={}", client_token('a')),
                Some(json!({ "description": "first" })),
            )],
            retry: Call::new(
                "PUT",
                format!("/packages/{name}?clientToken={}", client_token('b')),
                Some(json!({ "description": "second" })),
            ),
            error_type: CONFLICT,
            message: format!(
                "Conditional check failure: Received duplicate request for package {name}"
            ),
            identity: None,
        });
    }

    for name in rule_names() {
        // A topic rule: the name is single-quoted and the collision is the account's.
        rows.push(Case {
            case: format!("CreateTopicRule on the existing rule {name:?}"),
            setup: vec![Call::new(
                "POST",
                format!("/rules/{name}"),
                Some(json!({ "sql": "SELECT * FROM 'a'", "actions": [] })),
            )],
            retry: Call::new(
                "POST",
                format!("/rules/{name}"),
                Some(json!({ "sql": "SELECT * FROM 'b'", "actions": [] })),
            ),
            error_type: ALREADY_EXISTS,
            message: format!("Topic Rule '{name}' already exists for account"),
            identity: None,
        });
    }

    for name in policy_names() {
        // A policy: refused BY NAME, and this family DOES report the record that exists.
        rows.push(Case {
            case: format!("CreatePolicy on the registered policy {name:?}"),
            setup: vec![Call::new(
                "POST",
                format!("/policies/{name}"),
                Some(json!({ "policyDocument": policy_document() })),
            )],
            retry: Call::new(
                "POST",
                format!("/policies/{name}"),
                Some(json!({ "policyDocument": policy_document() })),
            ),
            error_type: ALREADY_EXISTS,
            message: format!("Policy cannot be created - name already exists (name={name})"),
            identity: Some((name.clone(), "policyArn")),
        });
    }

    for name in punctuated_names() {
        let function_arn = format!("arn:aws:lambda:{REGION}:{ACCOUNT_ID}:function:authorize");
        // An authorizer: its own sentence, and the record that exists is reported.
        rows.push(Case {
            case: format!("CreateAuthorizer on the existing authorizer {name:?}"),
            setup: vec![Call::new(
                "POST",
                format!("/authorizer/{name}"),
                Some(json!({ "authorizerFunctionArn": function_arn.clone() })),
            )],
            retry: Call::new(
                "POST",
                format!("/authorizer/{name}"),
                Some(json!({ "authorizerFunctionArn": function_arn })),
            ),
            error_type: ALREADY_EXISTS,
            message: format!("Authorizer {name} already exist for this account"),
            identity: Some((name.clone(), "authorizerArn")),
        });

        // A role alias: its own two-word noun, with the collision scoped to the account.
        let role_arn = format!("arn:aws:iam::{ACCOUNT_ID}:role/device-role");
        rows.push(Case {
            case: format!("CreateRoleAlias on the existing alias {name:?}"),
            setup: vec![Call::new(
                "POST",
                format!("/role-aliases/{name}"),
                Some(json!({ "roleArn": role_arn.clone() })),
            )],
            retry: Call::new(
                "POST",
                format!("/role-aliases/{name}"),
                Some(json!({ "roleArn": role_arn })),
            ),
            error_type: ALREADY_EXISTS,
            message: format!("Role alias {name} already exists for this account"),
            identity: Some((name.clone(), "roleAliasArn")),
        });
    }

    for id in stream_ids() {
        // A stream: this family states what the request asked for, and reports the record
        // that already holds the id.
        let role_arn = format!("arn:aws:iam::{ACCOUNT_ID}:role/IoTStreamRole");
        let file = json!({
            "fileId": 0,
            "s3Location": { "bucket": "firmware-images", "key": "firmware.bin" },
        });
        rows.push(Case {
            case: format!("CreateStream on the existing stream {id:?}"),
            setup: vec![Call::new(
                "POST",
                format!("/streams/{id}"),
                Some(json!({ "files": [file.clone()], "roleArn": role_arn.clone() })),
            )],
            retry: Call::new(
                "POST",
                format!("/streams/{id}"),
                Some(json!({
                    "files": [file],
                    "roleArn": role_arn,
                    "description": "a second attempt",
                })),
            ),
            error_type: ALREADY_EXISTS,
            message: format!("Requested stream {id} stream already exists."),
            identity: Some((id.clone(), "streamArn")),
        });
    }

    rows
}

/// Every family's collision: the status, the error shape, the sentence, and which
/// members the 409 body carries.
#[tokio::test]
async fn each_resource_family_words_a_taken_name_its_own_way() {
    for case in cases() {
        // A fresh emulator per row: a refusal must depend only on what this row created.
        let emulator = Emulator::new();
        let mut created = Value::Null;
        for call in &case.setup {
            let reply = emulator.call(call).await;
            assert_eq!(
                reply.status, 200,
                "case {:?}: the setup call {} {} must answer 200, got {} {:?}",
                case.case, call.method, call.path, reply.status, reply.body
            );
            created = reply.body;
        }

        let refused = emulator.call(&case.retry).await;
        assert_eq!(
            refused.status, 409,
            "case {:?}: expected status 409, got {} {:?}",
            case.case, refused.status, refused.body
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some(case.error_type),
            "case {:?}: expected x-amzn-errortype {:?}, got {:?}",
            case.case,
            case.error_type,
            refused.error_type
        );
        assert_eq!(
            refused.message(),
            case.message,
            "case {:?}: expected message {:?}, got {:?}",
            case.case,
            case.message,
            refused.message()
        );

        match &case.identity {
            Some((resource_id, arn_member)) => {
                assert_eq!(
                    refused.text("resourceId"),
                    Some(resource_id.as_str()),
                    "case {:?}: this family reports the record that exists as resourceId, \
                     got {:?}",
                    case.case,
                    refused.body
                );
                let minted = created.get(arn_member).and_then(Value::as_str);
                assert_eq!(
                    refused.text("resourceArn"),
                    minted,
                    "case {:?}: resourceArn must be the ARN the create minted ({minted:?}), \
                     got {:?}",
                    case.case,
                    refused.body
                );
            }
            None => {
                assert_eq!(
                    refused.body.get("resourceId"),
                    None,
                    "case {:?}: this family sends no resourceId, got {:?}",
                    case.case,
                    refused.body
                );
                assert_eq!(
                    refused.body.get("resourceArn"),
                    None,
                    "case {:?}: this family sends no resourceArn, got {:?}",
                    case.case,
                    refused.body
                );
            }
        }
    }
}

/// The other side of the same decision: a second create that collides with NOTHING — the
/// same configuration again, or the same idempotency token — is still answered 200 from
/// the stored record, so the sentences above are reached only by a real collision.
#[tokio::test]
async fn a_repeat_create_of_one_configuration_is_still_served() {
    // (case, the create, the output member whose value both answers must repeat)
    let token = client_token('c');
    let rows: Vec<(&str, Call, &str)> = vec![
        (
            "CreateThing with the same attributes",
            Call::new(
                "POST",
                "/things/idempotent".to_string(),
                Some(json!({ "attributePayload": { "attributes": { "model": "a1" } } })),
            ),
            "thingId",
        ),
        (
            "CreateThingGroup with the same properties",
            Call::new(
                "POST",
                "/thing-groups/idempotent".to_string(),
                Some(json!({ "thingGroupProperties": { "thingGroupDescription": "one" } })),
            ),
            "thingGroupId",
        ),
        (
            "CreateThingType with the same properties",
            Call::new(
                "POST",
                "/thing-types/idempotent".to_string(),
                Some(json!({ "thingTypeProperties": { "thingTypeDescription": "one" } })),
            ),
            "thingTypeId",
        ),
        (
            "CreatePackage with the same clientToken",
            Call::new(
                "PUT",
                format!("/packages/idempotent?clientToken={token}"),
                Some(json!({ "description": "one" })),
            ),
            "packageArn",
        ),
    ];

    for (case, call, member) in rows {
        let emulator = Emulator::new();
        let first = emulator.call(&call).await;
        assert_eq!(
            first.status, 200,
            "case {case:?}: the first create must answer 200, got {} {:?}",
            first.status, first.body
        );
        let second = emulator.call(&call).await;
        assert_eq!(
            second.status, 200,
            "case {case:?}: the repeat create must answer 200, got {} {:?}",
            second.status, second.body
        );
        assert_eq!(
            second.text(member),
            first.text(member),
            "case {case:?}: the repeat create must mint no second {member}, got {:?} then {:?}",
            first.body,
            second.body
        );
    }
}
