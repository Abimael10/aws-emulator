// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING `python3.x` handler reaches with the AWS SDK its managed runtime supplies,
//! end to end over the wire: Lambda, Amazon S3, Amazon DynamoDB, IAM and the token service on
//! ONE engine, a function whose module-scope code does `import boto3`, and the object and the
//! item it wrote read back where a client reads them.
//!
//! RULE: a managed runtime supplies the AWS SDK as an importable module, so `import boto3` at
//! module scope succeeds and a client the handler builds from it makes its calls with
//! temporary credentials of the FUNCTION's execution role — "the execution role grants the
//! function permission to use Amazon Web Services services". Each such call is therefore
//! served or refused by the addressed service exactly as any other signed request, and WHAT
//! that service answered reaches the handler as its own result or as its own caught
//! `botocore.exceptions.ClientError` — the error code, message and status that service words
//! the refusal in, whether the refusal is about the addressed resource (a key the bucket holds
//! nothing under) or about the grant (an action no identity-based policy in force on the role
//! allows). None of it is an initialization failure of the runtime: a run whose module imported
//! the SDK reports no `FunctionError` at all.
//!
//! The rows differ along what the rule generalises over: WHICH service the client is built for
//! (an object store bound to methods, paths and headers of its own; an item store carrying an
//! input document), WHICH operation it calls (a write carrying bytes, a read whose payload is
//! those bytes, a read of a target that does not exist, an update answering the item it left
//! behind, a read of that item), and WHICH actions the execution role's own identity policy
//! grants (both services, one of them, the other, and the bucket's own listing beside the
//! object grant or not) — so the same call is served under one role and refused under another,
//! and the refusal comes from the service that owns the resource.
//!
//! Every group's steps run in ONE invocation, because an invocation is the dearest thing this
//! target does, and the last assertions read the whole thing back through S3's and DynamoDB's
//! OWN APIs: a call that answered without doing the work fails there, and so does a refusal
//! that wrote something anyway.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The DynamoDB wire call, the table fixture and the key attribute it keys a table on are the
// ones the stream-delivery suite publishes, reached rather than written a second time here.
use crate::dynamodb_stream_event_source_delivery::{dynamodb, given_table, KEY};
use crate::event_source_mapping_role_access::{allow, given_role, logs_only};
use crate::event_source_mappings::{await_function_active, call, router_on};
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than copied here.
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The runtime every function below runs, and the module and attribute its package carries
/// its handler as.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The bucket every object below lives in, and the table every item below lives in.
const BUCKET: &str = "handler-sdk-reach";
/// See [`BUCKET`].
const TABLE: &str = "handler-sdk-reach-ledger";

/// The bytes the handler writes and reads back.
const OBJECT_BODY: &str = "id,total\n1,42\n";

/// A key the bucket holds nothing under, which is what makes one row a read of a target that
/// does not exist rather than a refusal about the grant.
const ABSENT_KEY: &str = "reports/absent.csv";

/// The value the handler's update sets on the item's `total` attribute. Neither this nor the
/// attribute names below are read by this emulator: they are the handler's own, and what is
/// asserted is that they round-trip.
const TOTAL: &str = "7";

/// The code and status Amazon S3 refuses an unauthorized request with (that service's "Error
/// responses": `AccessDenied`, HTTP 403).
const S3_ACCESS_DENIED: &str = "AccessDenied";
/// See [`S3_ACCESS_DENIED`].
const S3_ACCESS_DENIED_STATUS: u16 = 403;

/// The code and status DynamoDB refuses an unauthorized request with (its `AccessDeniedException`,
/// HTTP 400).
const DYNAMODB_ACCESS_DENIED: &str = "AccessDeniedException";
/// See [`DYNAMODB_ACCESS_DENIED`].
const DYNAMODB_ACCESS_DENIED_STATUS: u16 = 400;

/// The refusal Amazon S3 answers a read or write of a key nothing is stored under with
/// (`NoSuchKey`, HTTP 404, "The specified key does not exist.").
const NO_SUCH_KEY: &str = "NoSuchKey";
/// See [`NO_SUCH_KEY`].
const NO_SUCH_KEY_MESSAGE: &str = "The specified key does not exist.";
/// See [`NO_SUCH_KEY`].
const NO_SUCH_KEY_STATUS: u16 = 404;

/// Lambda, Amazon S3, Amazon DynamoDB, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's own SDK call is dispatched back into
/// this same router, so the bucket and the table it reaches are the ones this suite reads, and
/// the role its credentials are minted of is the record IAM holds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// One Amazon S3 request made as a CLIENT makes it — path style, signed for `s3` — answering
/// its status and its body.
async fn s3(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host.clone())
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/s3/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    (
        parts.status.as_u16(),
        String::from_utf8_lossy(&body).to_string(),
    )
}

/// One DynamoDB call that must be served, over the shared wire helper.
async fn dynamodb_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answered) = dynamodb(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DynamoDB {operation} must succeed, got {status} with {answered}"
    );
    answered
}

/// The bucket every object of this suite lives in, created the way a client creates one.
async fn given_bucket(router: &MultiServiceRouter, case: &str) {
    let (status, answered) = s3(router, "PUT", &format!("/{BUCKET}"), b"").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateBucket to answer 200, got {status} ({answered})"
    );
}

/// The table every item of this suite lives in — the shared fixture creates it and waits it
/// out to `ACTIVE` — answered as the ARN DynamoDB persisted, which is the resource a grant
/// for it names.
async fn item_table(router: &MultiServiceRouter, case: &str) -> String {
    given_table(router, case, TABLE, None).await;
    let described = dynamodb_ok(router, case, "DescribeTable", &json!({"TableName": TABLE})).await;
    described["Table"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no TableArn in {described}"))
        .to_string()
}

/// The handler every group runs: for each step the event names it builds a client for that
/// step's service with the SDK the runtime supplies, calls the operation the step names with
/// the input the step carries, and reports what came back — so the invocation's payload IS
/// what the SDK gave the guest, the service's own answer or the error it raised.
///
/// `import boto3` is at module scope, so a runtime shipping no SDK fails the initialization
/// phase and no step runs at all.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def lambda_handler(event, context):
    results = []
    for step in event["steps"]:
        client = boto3.client(step["service"])
        arguments = dict(step["arguments"])
        # The bytes an object write carries: the event holds them as text, and the SDK's own
        # member is a blob.
        if "Body" in arguments:
            arguments["Body"] = arguments["Body"].encode("utf-8")
        try:
            answered = getattr(client, step["operation"])(**arguments)
        except botocore.exceptions.ClientError as error:
            results.append({
                "ok": False,
                "code": error.response["Error"]["Code"],
                "message": error.response["Error"]["Message"],
                "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
            })
            continue
        read = {"ok": True, "status": answered["ResponseMetadata"]["HTTPStatusCode"]}
        if "Body" in answered:
            read["body"] = answered["Body"].read().decode("utf-8")
        if "Item" in answered:
            read["item"] = answered["Item"]
        if "Attributes" in answered:
            read["attributes"] = answered["Attributes"]
        results.append(read)
    return {"results": results}
"#;

/// One step of a group's invocation: the call the handler makes, and the document the SDK must
/// have given the guest for it.
struct Step {
    case: &'static str,
    /// The token the handler builds the client with, which names the service the call goes to.
    service: &'static str,
    /// The client method, which names the operation.
    operation: &'static str,
    /// The keyword arguments it is called with: the operation's own input members.
    arguments: Value,
    /// What the handler must have read back.
    expected: Value,
}

/// One group: an execution role carrying one identity policy, a function running as it, and
/// the steps one invocation of that function makes.
struct Group {
    case: &'static str,
    /// What the role and the function of this group are named after.
    slug: &'static str,
    /// The statements of the role's one inline identity policy.
    statements: Value,
    steps: Vec<Step>,
}

impl Group {
    /// The role this group's function runs as.
    fn role(&self) -> String {
        format!("sdk-reach-{}", self.slug)
    }

    /// The function this group invokes.
    fn function(&self) -> String {
        format!("sdk-reach-{}-caller", self.slug)
    }

    /// The assumed-role session a call made by an invocation of this group's function speaks
    /// for: the session is named for the FUNCTION, and it is the principal a refusal names.
    fn session(&self) -> String {
        format!(
            "arn:aws:sts::{}:assumed-role/{}/{}",
            engine::emulator_account_id(),
            self.role(),
            self.function()
        )
    }
}

/// The key the group `slug` writes and reads, so one group's object is its own.
fn object_key(slug: &str) -> String {
    format!("reports/{slug}.csv")
}

/// The item the group `slug` updates and reads, so one group's item is its own.
fn item_id(slug: &str) -> String {
    format!("ledger-{slug}")
}

/// The item that stands in the table once a group's update has run: the key it addressed and
/// the attribute that update set, each as the wire carries an attribute value. [`KEY`] is the
/// attribute the shared table fixture keys a table on.
fn stored_item(slug: &str) -> Value {
    json!({ KEY: { "S": item_id(slug) }, "total": { "N": TOTAL } })
}

/// The input of the object write: the bytes go in as text and the handler encodes them.
fn put_object_input(slug: &str) -> Value {
    json!({ "Bucket": BUCKET, "Key": object_key(slug), "Body": OBJECT_BODY })
}

/// The input of an object read of `key`.
fn get_object_input(key: &str) -> Value {
    json!({ "Bucket": BUCKET, "Key": key })
}

/// The input of the item update: it sets the one attribute through an update expression and
/// asks for the item it leaves behind.
fn update_item_input(slug: &str) -> Value {
    json!({
        "TableName": TABLE,
        "Key": { KEY: { "S": item_id(slug) } },
        "UpdateExpression": "SET #total = :total",
        "ExpressionAttributeNames": { "#total": "total" },
        "ExpressionAttributeValues": { ":total": { "N": TOTAL } },
        "ReturnValues": "ALL_NEW",
    })
}

/// The input of the item read.
fn get_item_input(slug: &str) -> Value {
    json!({ "TableName": TABLE, "Key": { KEY: { "S": item_id(slug) } } })
}

/// The refusal Amazon S3 words when the policies in force allow the calling session nothing:
/// its one authorization sentence, with the resource it addressed quoted as that service
/// quotes it.
fn s3_refusal(session: &str, action: &str, resource: &str) -> Value {
    json!({
        "ok": false,
        "code": S3_ACCESS_DENIED,
        "status": S3_ACCESS_DENIED_STATUS,
        "message": format!(
            "User: {session} is not authorized to perform: {action} on resource: \
             \"{resource}\" because no identity-based policy allows the {action} action"
        ),
    })
}

/// The same refusal as DynamoDB words it: the same sentence over the table it addressed, under
/// that service's own error code and status.
fn dynamodb_refusal(session: &str, action: &str, table_arn: &str) -> Value {
    json!({
        "ok": false,
        "code": DYNAMODB_ACCESS_DENIED,
        "status": DYNAMODB_ACCESS_DENIED_STATUS,
        "message": format!(
            "User: {session} is not authorized to perform: {action} on resource: {table_arn} \
             because no identity-based policy allows the {action} action"
        ),
    })
}

/// What a served object write and a served object read must come to.
fn written() -> Value {
    json!({ "ok": true, "status": 200 })
}

/// See [`written`].
fn read_back() -> Value {
    json!({ "ok": true, "status": 200, "body": OBJECT_BODY })
}

/// The groups: which services the execution role's identity policy grants the function, and
/// what each of its SDK calls must then come to.
///
/// Every action named in a grant and in a refusal is the IAM action of the operation the step
/// calls, and every resource is the one that call addressed.
fn groups(table_arn: &str) -> Vec<Group> {
    let bucket_arn = format!("arn:aws:s3:::{BUCKET}");
    let objects = format!("{bucket_arn}/*");
    let object_grant = allow("s3", &["PutObject", "GetObject"], &objects);
    // Which of the two refusals a read of a key nothing is stored under earns is the grant
    // over the BUCKET: "if you have the s3:ListBucket permission on the bucket, Amazon S3
    // returns an HTTP status code 404 (Not Found) error. If you don't have the s3:ListBucket
    // permission, Amazon S3 returns an HTTP status code 403 (Access Denied) error"
    // (`GetObject`). One group carries it and one does not, so both halves are pinned.
    let listing_grant = allow("s3", &["ListBucket"], &bucket_arn);
    let item_grant = allow("dynamodb", &["GetItem", "UpdateItem"], table_arn);

    let both = Group {
        case: "a role granting the object store and the item store alike",
        slug: "both",
        statements: json!([
            object_grant.clone(),
            listing_grant,
            item_grant.clone(),
            logs_only(),
        ]),
        steps: Vec::new(),
    };
    let objects_only = Group {
        case: "a role granting the object store alone",
        slug: "objects",
        statements: json!([object_grant, logs_only()]),
        steps: Vec::new(),
    };
    let items_only = Group {
        case: "a role granting the item store alone",
        slug: "items",
        statements: json!([item_grant, logs_only()]),
        steps: Vec::new(),
    };

    vec![
        Group {
            steps: vec![
                Step {
                    case: "an object write carrying the handler's own bytes",
                    service: "s3",
                    operation: "put_object",
                    arguments: put_object_input(both.slug),
                    expected: written(),
                },
                Step {
                    case: "an object read whose payload is those bytes",
                    service: "s3",
                    operation: "get_object",
                    arguments: get_object_input(&object_key(both.slug)),
                    expected: read_back(),
                },
                Step {
                    case: "an object read of a key the bucket holds nothing under, by a session\
                           the bucket's own listing is granted to",
                    service: "s3",
                    operation: "get_object",
                    arguments: get_object_input(ABSENT_KEY),
                    expected: json!({
                        "ok": false,
                        "code": NO_SUCH_KEY,
                        "message": NO_SUCH_KEY_MESSAGE,
                        "status": NO_SUCH_KEY_STATUS,
                    }),
                },
                Step {
                    case: "an item update answering the item it left behind",
                    service: "dynamodb",
                    operation: "update_item",
                    arguments: update_item_input(both.slug),
                    expected: json!({
                        "ok": true,
                        "status": 200,
                        "attributes": stored_item(both.slug),
                    }),
                },
                Step {
                    case: "an item read of what that update stored",
                    service: "dynamodb",
                    operation: "get_item",
                    arguments: get_item_input(both.slug),
                    expected: json!({ "ok": true, "status": 200, "item": stored_item(both.slug) }),
                },
            ],
            ..both
        },
        Group {
            steps: vec![
                Step {
                    case: "the granted object write",
                    service: "s3",
                    operation: "put_object",
                    arguments: put_object_input(objects_only.slug),
                    expected: written(),
                },
                Step {
                    case: "the granted object read",
                    service: "s3",
                    operation: "get_object",
                    arguments: get_object_input(&object_key(objects_only.slug)),
                    expected: read_back(),
                },
                Step {
                    case: "the same read of a key the bucket holds nothing under, by a session\
                           the bucket's own listing is NOT granted to",
                    service: "s3",
                    operation: "get_object",
                    arguments: get_object_input(ABSENT_KEY),
                    expected: s3_refusal(
                        &objects_only.session(),
                        "s3:ListBucket",
                        &format!("arn:aws:s3:::{BUCKET}"),
                    ),
                },
                Step {
                    case: "an item update no identity policy in force allows",
                    service: "dynamodb",
                    operation: "update_item",
                    arguments: update_item_input(objects_only.slug),
                    expected: dynamodb_refusal(
                        &objects_only.session(),
                        "dynamodb:UpdateItem",
                        table_arn,
                    ),
                },
                Step {
                    case: "an item read no identity policy in force allows",
                    service: "dynamodb",
                    operation: "get_item",
                    arguments: get_item_input(objects_only.slug),
                    expected: dynamodb_refusal(
                        &objects_only.session(),
                        "dynamodb:GetItem",
                        table_arn,
                    ),
                },
            ],
            ..objects_only
        },
        Group {
            steps: vec![
                Step {
                    case: "the granted item update",
                    service: "dynamodb",
                    operation: "update_item",
                    arguments: update_item_input(items_only.slug),
                    expected: json!({
                        "ok": true,
                        "status": 200,
                        "attributes": stored_item(items_only.slug),
                    }),
                },
                Step {
                    case: "the granted item read",
                    service: "dynamodb",
                    operation: "get_item",
                    arguments: get_item_input(items_only.slug),
                    expected: json!({
                        "ok": true,
                        "status": 200,
                        "item": stored_item(items_only.slug),
                    }),
                },
                Step {
                    case: "an object write no identity policy in force allows",
                    service: "s3",
                    operation: "put_object",
                    arguments: put_object_input(items_only.slug),
                    expected: s3_refusal(
                        &items_only.session(),
                        "s3:PutObject",
                        &format!("arn:aws:s3:::{BUCKET}/{}", object_key(items_only.slug)),
                    ),
                },
                Step {
                    // An object the bucket DOES hold — the one the first group's handler
                    // wrote — so what refuses the read is the grant over the read itself and
                    // not the bucket-listing rule above.
                    case: "an object read no identity policy in force allows",
                    service: "s3",
                    operation: "get_object",
                    arguments: get_object_input(&object_key("both")),
                    expected: s3_refusal(
                        &items_only.session(),
                        "s3:GetObject",
                        &format!("arn:aws:s3:::{BUCKET}/{}", object_key("both")),
                    ),
                },
            ],
            ..items_only
        },
    ]
}

/// A `python3.x` handler reaches both services with the SDK its runtime supplies, under its own
/// execution role — and what each service answered, served or refused, is what the handler
/// read back.
#[tokio::test]
async fn a_python_handler_reaches_both_services_with_the_sdk_its_runtime_supplies() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a python handler reaching two services with the runtime's own SDK";
    let router = emulator();

    given_bucket(&router, case).await;
    let table_arn = item_table(&router, case).await;
    let groups = groups(&table_arn);

    // Every role and function first, so the provisioning spans the clock owes run down
    // together rather than one after another.
    for group in &groups {
        let role_arn = given_role(
            &router,
            group.case,
            &group.role(),
            Some(group.statements.clone()),
        )
        .await;
        let created = call(
            &router,
            "POST",
            FUNCTIONS_URI,
            &json!({
                "FunctionName": group.function(),
                "Role": role_arn,
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Timeout": 30,
                "Code": {
                    "ZipFile": base64::engine::general_purpose::STANDARD
                        .encode(package(&[("lambda_function.py", HANDLER_SOURCE)])),
                },
            }),
        )
        .await;
        assert_eq!(
            created.status,
            201,
            "case {:?}: expected CreateFunction to answer 201, got {} {}",
            group.case,
            created.status,
            created.text()
        );
    }
    for group in &groups {
        await_function_active(&router, group.case, &group.function()).await;
    }

    for group in &groups {
        let steps: Vec<Value> = group
            .steps
            .iter()
            .map(|step| {
                json!({
                    "service": step.service,
                    "operation": step.operation,
                    "arguments": step.arguments,
                })
            })
            .collect();
        let answered = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{}/invocations", group.function()),
            &json!({ "steps": steps }),
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {:?}: expected HTTP 200 from Invoke, got {} {}",
            group.case,
            answered.status,
            answered.text()
        );
        let payload = answered.json();
        // The handler RAN: a runtime that could not import the SDK fails the initialization
        // phase, which Invoke reports as a function error instead of a result.
        assert_eq!(
            answered.header("x-amz-function-error"),
            None,
            "case {:?}: expected the handler's own result, got a function error with {payload}",
            group.case
        );
        assert_eq!(
            payload["results"].as_array().map(Vec::len),
            Some(group.steps.len()),
            "case {:?}: expected one result per step, got {payload}",
            group.case
        );
        for (index, step) in group.steps.iter().enumerate() {
            let actual = payload["results"][index].clone();
            assert_eq!(
                actual, step.expected,
                "case {:?} / {:?}: expected {}, got {actual} (the whole run: {payload})",
                group.case, step.case, step.expected
            );
        }
    }

    // What the two services THEMSELVES hold, read where a client reads it: a step that
    // reported a write did the work, and a step that was refused did none.
    for slug in ["both", "objects"] {
        let (status, held) = s3(&router, "GET", &format!("/{BUCKET}/{}", object_key(slug)), b"").await;
        assert_eq!(
            (status, held.as_str()),
            (200, OBJECT_BODY),
            "case {case:?}: expected S3 to serve the object the {slug:?} handler wrote"
        );
    }
    let (status, refused_write) = s3(
        &router,
        "GET",
        &format!("/{BUCKET}/{}", object_key("items")),
        b"",
    )
    .await;
    assert_eq!(
        status, NO_SUCH_KEY_STATUS,
        "case {case:?}: expected the refused object write to have stored nothing, got {status} \
         ({refused_write})"
    );
    for slug in ["both", "items"] {
        let held = dynamodb_ok(&router, case, "GetItem", &get_item_input(slug)).await;
        assert_eq!(
            held["Item"],
            stored_item(slug),
            "case {case:?}: expected DynamoDB to hold the item the {slug:?} handler wrote, got \
             {held}"
        );
    }
    let absent = dynamodb_ok(&router, case, "GetItem", &get_item_input("objects")).await;
    assert_eq!(
        absent.get("Item"),
        None,
        "case {case:?}: expected the refused item update to have stored nothing, got {absent}"
    );
}
