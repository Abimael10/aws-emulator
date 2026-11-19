// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING Python handler does to an OpenSearch Serverless collection with the AWS SDK
//! the managed runtime supplies: it creates one, reads it back by name and by its minted id,
//! asks for one nobody created, lists the account's collections, writes a tag and reads the
//! tags back — and is refused each of those when its execution role grants none of them.
//!
//! RULE: AWS's managed Python runtime ships the AWS SDK on the handler's import path with the
//! data for the services a client can be built for, so `boto3.client(<service>)` CONSTRUCTS
//! inside a handler for every service the runtime's call seam dispatches to, and each method
//! on that client makes a real signed call as a session of the function's execution role: what
//! the handler observes is the SERVICE's own answer — the document the operation models for a
//! call IAM permits, and `AccessDeniedException` (HTTP 403) with IAM's own sentence for one it
//! does not — and never a client-side failure of the SDK itself, which tells a caller nothing
//! about the service and leaves an IAM denial indistinguishable from a broken runtime image.
//!
//! That last clause is the one a per-service suite cannot take: a service whose model the
//! runtime's SDK does not carry raises `botocore.exceptions.UnknownServiceError` before
//! anything is signed, so every outcome of every call — a served create, a served read, a
//! served listing, an empty tag-write success and a refusal — collapses into one client-side
//! exception. Each row therefore asserts that the client was BUILT and the call dispatched
//! before it asserts what came back.
//!
//! Five crates have to agree for the rule to hold — CloudFormation deploys each arm's role and
//! the function running as it, IAM holds the roles and their documents, the token service mints
//! the session the handler's calls are signed as, Lambda runs the handler and supplies it the
//! SDK, and OpenSearch Serverless answers the calls — so the suite lives here. The wire harness
//! is [`super::lambda_handler_invalidates_its_distribution`]'s, reused rather than repeated.
//!
//! What the neighbours pin and this one does not repeat:
//! [`super::aoss_and_bedrock_agent_identity_authorization`] drives the TABLE of policy document
//! shapes one listing is decided by, over two control planes. What only this suite reaches is
//! the WHOLE set of collection operations a glue function performs, each in one invocation
//! against one role, with the service's own state read back afterwards.
//!
//! The rows differ along what the rule generalises over: WHICH operation the handler calls (a
//! create, two reads, a read of something absent, a listing, a tag write, a tag read), the FORM
//! of the selector a read names its target by (a name, a server-minted id, a name nobody
//! created), whether the request addresses its target by ARN — which is what a refusal of it
//! names, rather than every resource — and whether the execution role allows the action. Each
//! arm asserts both halves: what the SDK handed the handler, and what the service HOLDS
//! afterwards, so a refused create that still minted a collection and a refused tag write that
//! still tagged one both fail here.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::lambda_handler_invalidates_its_distribution::{
    authorization, build_router, call, deployed, wait_until_active, Answer, LOOK_INTERVAL, REGION,
    WAIT,
};

/// The account every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// This service's action namespace, which is also its SigV4 signing name and its endpoint
/// prefix, and the prefix its `awsJson1_0` requests name an operation under
/// (`smithy_query(query="service_info")` on the OpenSearch Serverless model).
const AOSS: &str = "aoss";
const AOSS_TARGET_PREFIX: &str = "OpenSearchServerless";

/// Where Lambda's function routes are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime the handler runs on, and the module and function its handler names.
const RUNTIME: &str = "python3.12";
const HANDLER_NAME: &str = "index.lambda_handler";

/// How long ONE invocation is given, as the function's own `Timeout`: generous, because what
/// is under test is what the SDK answers and not how fast a host gets through seven calls.
const FUNCTION_TIMEOUT: u64 = 120;

/// The code and status AWS refuses a request IAM does not permit with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The resource IAM names in a refusal of a request that addresses no resource of its own.
const EVERY_RESOURCE: &str = "*";

/// The `encryption` security policy the collections are created under: a create whose name no
/// such policy covers and whose request names no key is missing the encryption setting a
/// collection must have, so the policy is written first by the account's own credential.
const ENCRYPTION_POLICY: &str = "handler-aoss-encryption";

/// The collection the reads and the tag writes address, created in setup and waited into its
/// settled status there.
const BASE_COLLECTION: &str = "handler-aoss-base";

/// A well-formed collection name nobody creates, which the read of an absent collection asks
/// for (`CollectionName`: @length 3..64, @pattern `^[a-z][a-z0-9-]+$`).
const MISSING_COLLECTION: &str = "handler-aoss-absent";

/// The statuses a collection is reported in: the non-terminal one a create answers with, and
/// the one it settles into on its own (`CollectionStatus`).
const CREATING: &str = "CREATING";
const ACTIVE: &str = "ACTIVE";

/// The `errorCode` a batch read reports for a selector that names no collection
/// (`CollectionErrorDetail$errorCode`, whose documentation gives `NOT_FOUND` as its example).
const NOT_FOUND: &str = "NOT_FOUND";

/// The `CollectionType` every collection here is created as.
const COLLECTION_TYPE: &str = "SEARCH";

/// The bounds a minted collection id is within (`CollectionId`: @length 3..40, @pattern
/// `^[a-z0-9]{3,40}$`), which is what a create's answer is read against rather than any one
/// run's id.
const ID_LENGTH: ::std::ops::RangeInclusive<usize> = 3..=40;

/// The value the tag a handler WRITES carries, and the value the tag a handler passes to its
/// create carries — distinct, so a read tells the two writes apart.
const WRITTEN_VALUE: &str = "written-in-handler";
const CREATED_VALUE: &str = "created-in-handler";

/// The stack that declares both arms' roles and functions.
const STACK: &str = "xsvc-handler-aoss-stack";

/// The actions a granted role allows: `<namespace>:<OperationName>` for each operation the
/// handler calls.
const GRANTED_ACTIONS: &[&str] = &[
    "aoss:CreateCollection",
    "aoss:BatchGetCollection",
    "aoss:ListCollections",
    "aoss:TagResource",
    "aoss:ListTagsForResource",
];

/// The handler both arms run: it builds the client for this service with the SDK the runtime
/// supplies, then makes each call the event names and reports, per call, either the fields of
/// the service's own answer or the refusal the SDK raised for it.
///
/// The client is built inside the handler and its failure reported under a key of its own: a
/// client that cannot be built at all is not a refusal of anything, and this suite asserts it
/// apart from what any one call answered.
const HANDLER: &str = r#"
import boto3
from botocore.exceptions import ClientError


def _reported(call, answered):
    if call == "create_collection":
        detail = answered.get("createCollectionDetail") or {}
        return {
            "name": detail.get("name"),
            "id": detail.get("id"),
            "status": detail.get("status"),
            "type": detail.get("type"),
            "arn": detail.get("arn"),
        }
    if call == "batch_get_collection":
        return {
            "details": [
                {
                    "name": detail.get("name"),
                    "id": detail.get("id"),
                    "status": detail.get("status"),
                    "collectionEndpoint": detail.get("collectionEndpoint"),
                }
                for detail in answered.get("collectionDetails") or []
            ],
            "errors": [
                {
                    "name": failed.get("name"),
                    "id": failed.get("id"),
                    "errorCode": failed.get("errorCode"),
                }
                for failed in answered.get("collectionErrorDetails") or []
            ],
        }
    if call == "list_collections":
        return {
            "names": sorted(
                (summary.get("name") or "")
                for summary in answered.get("collectionSummaries") or []
            )
        }
    if call == "list_tags_for_resource":
        return {
            "tags": sorted(
                "%s=%s" % (tag.get("key") or "", tag.get("value") or "")
                for tag in answered.get("tags") or []
            )
        }
    return {"members": sorted(answered.keys())}


def lambda_handler(event, context):
    try:
        client = boto3.client(event["service"])
    except Exception as raised:
        return {"client": {"raised": type(raised).__name__, "detail": str(raised)}}
    probed = {}
    for step in event["steps"]:
        try:
            answered = getattr(client, step["call"])(**step["params"])
        except ClientError as refused:
            probed[step["case"]] = {
                "ok": False,
                "code": refused.response["Error"]["Code"],
                "message": refused.response["Error"]["Message"],
                "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
            }
            continue
        except Exception as raised:
            probed[step["case"]] = {
                "ok": False,
                "raised": type(raised).__name__,
                "detail": str(raised),
            }
            continue
        answered.pop("ResponseMetadata", None)
        probed[step["case"]] = {"ok": True, "reported": _reported(step["call"], answered)}
    return {"client": {"raised": None}, "probed": probed}
"#;

/// The collection the reads address, as setup left it.
#[derive(Debug)]
struct Collection {
    id: String,
    arn: String,
}

/// WHICH call a row makes, which decides both the request it sends and what its answer has to
/// be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    /// `CreateCollection`: the collection it mints does not exist yet, so the request carries
    /// its NAME and addresses no resource of its own.
    Create,
    /// `BatchGetCollection` selecting the settled collection by name.
    ReadByName,
    /// `BatchGetCollection` selecting the same collection by the id the service minted.
    ReadById,
    /// `BatchGetCollection` selecting a name nobody created.
    ReadAbsent,
    /// `ListCollections`, which addresses no resource of its own.
    List,
    /// `TagResource`, which names its target by ARN.
    Tag,
    /// `ListTagsForResource`, which names its target by ARN.
    ReadTags,
}

impl Probe {
    /// The SDK method the handler calls, which is the operation it makes.
    fn call(self) -> &'static str {
        match self {
            Self::Create => "create_collection",
            Self::ReadByName | Self::ReadById | Self::ReadAbsent => "batch_get_collection",
            Self::List => "list_collections",
            Self::Tag => "tag_resource",
            Self::ReadTags => "list_tags_for_resource",
        }
    }

    /// The IAM action that operation performs.
    fn action(self) -> &'static str {
        match self {
            Self::Create => "aoss:CreateCollection",
            Self::ReadByName | Self::ReadById | Self::ReadAbsent => "aoss:BatchGetCollection",
            Self::List => "aoss:ListCollections",
            Self::Tag => "aoss:TagResource",
            Self::ReadTags => "aoss:ListTagsForResource",
        }
    }

    /// The resource a refusal of it names: the ARN the request itself carries, and every
    /// resource for a request that carries none.
    fn resource(self, base: &Collection) -> String {
        match self {
            Self::Tag | Self::ReadTags => base.arn.clone(),
            _ => EVERY_RESOURCE.to_string(),
        }
    }

    /// The input document the handler sends: the members this operation's model declares,
    /// filled from the collection setup settled and the names this arm writes under.
    fn params(self, base: &Collection, made: &str, tag_key: &str, created_key: &str) -> Value {
        match self {
            Self::Create => json!({
                "name": made,
                "type": COLLECTION_TYPE,
                "encryptionConfig": {"aWSOwnedKey": true},
                "tags": [{"key": created_key, "value": CREATED_VALUE}],
            }),
            Self::ReadByName => json!({"names": [BASE_COLLECTION]}),
            Self::ReadById => json!({"ids": [base.id]}),
            Self::ReadAbsent => json!({"names": [MISSING_COLLECTION]}),
            Self::List => json!({}),
            Self::Tag => json!({
                "resourceArn": base.arn,
                "tags": [{"key": tag_key, "value": WRITTEN_VALUE}],
            }),
            Self::ReadTags => json!({"resourceArn": base.arn}),
        }
    }
}

/// One row: the call the handler makes, in the order the handler makes them — the tag read
/// comes after the tag write, so one invocation reports what the service answered the write
/// AND what it then held.
struct Row {
    case: &'static str,
    probe: Probe,
}

const ROWS: &[Row] = &[
    Row {
        case: "a create of a collection the account does not hold",
        probe: Probe::Create,
    },
    Row {
        case: "a read of the settled collection by name",
        probe: Probe::ReadByName,
    },
    Row {
        case: "a read of the same collection by the id the service minted",
        probe: Probe::ReadById,
    },
    Row {
        case: "a read of a collection nobody created",
        probe: Probe::ReadAbsent,
    },
    // The listing is driven here as well as in
    // [`super::aoss_and_bedrock_agent_identity_authorization`]'s function rows because what it
    // has to report is the collection the CREATE a call earlier made: only a listing inside
    // this sequence can show that one invocation's write is what the next call enumerates.
    Row {
        case: "the account's collections, listed",
        probe: Probe::List,
    },
    Row {
        case: "a tag written onto a collection named by its ARN",
        probe: Probe::Tag,
    },
    Row {
        case: "the tags of that collection, read back",
        probe: Probe::ReadTags,
    },
];

/// One arm: a function whose execution role either grants the actions the handler performs or
/// grants none of them.
struct Arm {
    case: &'static str,
    /// The logical id the stack declares the function under; its role is `<logical>Role` and
    /// its name is reported as the `<logical>Name` output.
    logical: &'static str,
    role: &'static str,
    function: &'static str,
    allowed: bool,
}

const ARMS: &[Arm] = &[
    Arm {
        case: "an execution role granted no action of the service",
        logical: "Ungranted",
        role: "handler-aoss-ungranted-role",
        function: "handler-aoss-ungranted",
        allowed: false,
    },
    Arm {
        case: "an execution role granted the actions the handler performs",
        logical: "Granted",
        role: "handler-aoss-granted-role",
        function: "handler-aoss-granted",
        allowed: true,
    },
];

/// One OpenSearch Serverless call over its `awsJson1_0` wire, made as the account's own
/// credential: the status and the parsed body.
async fn aoss(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let host = format!("{AOSS}.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("authorization", authorization(AOSS))
        .header("x-amz-target", format!("{AOSS_TARGET_PREFIX}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        ::serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    (status, parsed)
}

/// One Lambda `restJson1` call over the shared request helper.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: &str) -> Answer {
    call(
        router,
        "lambda",
        &format!("lambda.{REGION}.amazonaws.com"),
        method,
        uri,
        "application/json",
        body,
    )
    .await
}

/// The sentence IAM refuses a request its documents do not allow with, built here from the
/// request rather than from the service's own helper so the wording has something to be wrong
/// about: the principal that signed, the action it asked for, the resource it addressed and
/// which of IAM's rules withheld the grant.
fn refusal_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// The ARN of a collection of this account in this Region: the `Collection` resource's own ARN
/// template (`smithy_query(query="shape", shape="Collection")` -> `collection/{id}`) under this
/// service's action namespace.
fn collection_arn(id: &str) -> String {
    format!("arn:aws:{AOSS}:{REGION}:{ACCOUNT}:collection/{id}")
}

/// The document a role carries: the log actions every execution role needs — which name no
/// action of this service, so an arm carrying nothing else is the position a policy that was
/// never extended is in — plus, for a granted arm, the actions the handler performs.
fn role_document(allowed: bool) -> Value {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": EVERY_RESOURCE,
    })];
    if allowed {
        statements.push(json!({
            "Effect": "Allow",
            "Action": GRANTED_ACTIONS,
            "Resource": EVERY_RESOURCE,
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements})
}

/// The stack declaring one role and one function per arm: the inline source is archived into
/// the deployment package by CloudFormation, which is how a function whose handler is written
/// here comes to exist at all.
fn handler_stack() -> String {
    let mut resources = ::serde_json::Map::new();
    let mut outputs = ::serde_json::Map::new();
    for arm in ARMS {
        let role = format!("{}Role", arm.logical);
        resources.insert(
            role.clone(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": arm.role,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "handler-grants",
                        "PolicyDocument": role_document(arm.allowed),
                    }],
                },
            }),
        );
        resources.insert(
            arm.logical.to_string(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": arm.function,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER_NAME,
                    "Role": {"Fn::GetAtt": [role, "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": HANDLER},
                },
            }),
        );
        outputs.insert(
            format!("{}Name", arm.logical),
            json!({"Value": {"Ref": arm.logical}}),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// Write the encryption policy the collections are created under and the collection the reads
/// address, then wait for the status its create owes it.
async fn prepare_base_collection(router: &MultiServiceRouter) -> Collection {
    let (status, written) = aoss(
        router,
        "CreateSecurityPolicy",
        &json!({
            "type": "encryption",
            "name": ENCRYPTION_POLICY,
            "policy": json!({
                "Rules": [{"ResourceType": "collection", "Resource": ["collection/*"]}],
                "AWSOwnedKey": true,
            })
            .to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateSecurityPolicy must succeed, got {status} with {written}"
    );
    let (status, created) = aoss(
        router,
        "CreateCollection",
        &json!({"name": BASE_COLLECTION, "type": COLLECTION_TYPE}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateCollection({BASE_COLLECTION:?}) must succeed, got {status} with {created}"
    );
    let detail = created["createCollectionDetail"].clone();
    let id = detail["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: the create must report an id, got {created}"))
        .to_string();
    let arn = detail["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: the create must report an ARN, got {created}"))
        .to_string();
    // The settled status is owed from the instant the create answered, which no fixed sleep
    // names: this looks until the service reports it, with a deadline generous enough that a
    // loaded host does not decide the outcome.
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut reported = Value::Null;
    for _ in 0..looks {
        let (status, read) = aoss(
            router,
            "BatchGetCollection",
            &json!({"names": [BASE_COLLECTION]}),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: BatchGetCollection({BASE_COLLECTION:?}) must succeed, got {status} with {read}"
        );
        reported = read["collectionDetails"][0]["status"].clone();
        if reported.as_str() == Some(ACTIVE) {
            return Collection { id, arn };
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("setup: {BASE_COLLECTION:?} must reach {ACTIVE}, last reported {reported}");
}

/// Whether the service HOLDS a collection under `name`, read back through its own batch read
/// as the account's own credential.
async fn holds_collection(router: &MultiServiceRouter, case: &str, name: &str) -> bool {
    let (status, read) = aoss(router, "BatchGetCollection", &json!({"names": [name]})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: BatchGetCollection({name:?}) must succeed, got {status} with {read}"
    );
    read["collectionDetails"]
        .as_array()
        .is_some_and(|details| !details.is_empty())
}

/// The id the service minted for the collection it holds under `name`, read back through its
/// own batch read as the account's own credential.
async fn held_collection_id(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, read) = aoss(router, "BatchGetCollection", &json!({"names": [name]})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: BatchGetCollection({name:?}) must succeed, got {status} with {read}"
    );
    read["collectionDetails"][0]["id"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the held collection {name:?} to report an id, got {read}")
        })
        .to_string()
}

/// The `<key>=<value>` pairs the service holds for `arn`, read back through its own tag read as
/// the account's own credential.
async fn held_tags(router: &MultiServiceRouter, case: &str, arn: &str) -> Vec<String> {
    let (status, read) = aoss(router, "ListTagsForResource", &json!({"resourceArn": arn})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListTagsForResource({arn:?}) must succeed, got {status} with {read}"
    );
    read["tags"]
        .as_array()
        .map(|tags| {
            tags.iter()
                .map(|tag| {
                    format!(
                        "{}={}",
                        tag["key"].as_str().unwrap_or_default(),
                        tag["value"].as_str().unwrap_or_default()
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What one served call has to have answered, read off the fields the handler reported.
fn assert_served(case: &str, probe: Probe, base: &Collection, made: &str, tag: &str, got: &Value) {
    let reported = &got["reported"];
    match probe {
        Probe::Create => {
            let id = reported["id"].as_str().unwrap_or_default();
            assert!(
                (ID_LENGTH.contains(&id.chars().count()))
                    && id
                        .chars()
                        .all(|char| char.is_ascii_lowercase() || char.is_ascii_digit()),
                "case {case:?}: expected a minted id within CollectionId's own bounds \
                 ({ID_LENGTH:?}, lower-case alphanumeric), got {reported}"
            );
            assert_eq!(
                (
                    reported["name"].as_str(),
                    reported["status"].as_str(),
                    reported["type"].as_str(),
                    reported["arn"].as_str(),
                ),
                (
                    Some(made),
                    Some(CREATING),
                    Some(COLLECTION_TYPE),
                    Some(collection_arn(id).as_str()),
                ),
                "case {case:?}: expected the created collection's own detail, got {reported}"
            );
        }
        Probe::ReadByName | Probe::ReadById => {
            let details = reported["details"].as_array().cloned().unwrap_or_default();
            assert_eq!(
                details.len(),
                1,
                "case {case:?}: expected exactly the collection asked for, got {reported}"
            );
            let detail = &details[0];
            assert_eq!(
                (
                    detail["name"].as_str(),
                    detail["id"].as_str(),
                    detail["status"].as_str(),
                ),
                (
                    Some(BASE_COLLECTION),
                    Some(base.id.as_str()),
                    Some(ACTIVE),
                ),
                "case {case:?}: expected the settled collection's own detail, got {reported}"
            );
            let endpoint = detail["collectionEndpoint"].as_str().unwrap_or_default();
            assert!(
                endpoint.starts_with("https://"),
                "case {case:?}: expected an endpoint a client can dial, got {endpoint:?}"
            );
            assert_eq!(
                reported["errors"],
                json!([]),
                "case {case:?}: expected no error detail beside the collection, got {reported}"
            );
        }
        Probe::ReadAbsent => {
            assert_eq!(
                reported["details"],
                json!([]),
                "case {case:?}: expected no detail for a collection nobody created, got \
                 {reported}"
            );
            assert_eq!(
                reported["errors"],
                json!([{"name": MISSING_COLLECTION, "id": null, "errorCode": NOT_FOUND}]),
                "case {case:?}: expected the modelled error detail for the name asked for, got \
                 {reported}"
            );
        }
        Probe::List => {
            let names: Vec<&str> = reported["names"]
                .as_array()
                .map(|listed| listed.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            for expected in [BASE_COLLECTION, made] {
                assert!(
                    names.contains(&expected),
                    "case {case:?}: expected the listing to name {expected:?}, got {names:?}"
                );
            }
        }
        Probe::Tag => {
            assert_eq!(
                reported["members"],
                json!([]),
                "case {case:?}: expected the modelled empty output, got {reported}"
            );
        }
        Probe::ReadTags => {
            let written = format!("{tag}={WRITTEN_VALUE}");
            let held: Vec<&str> = reported["tags"]
                .as_array()
                .map(|tags| tags.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            assert!(
                held.contains(&written.as_str()),
                "case {case:?}: expected the tag written a call earlier, got {held:?}"
            );
        }
    }
}

/// RULE over the whole set of collection operations a glue function performs, on a role that
/// allows them and on one that allows none of them: the client is built, every call is
/// dispatched, and what the handler reads back is this service's own answer.
#[tokio::test]
async fn a_handler_manages_its_collections_through_the_runtimes_sdk() {
    let router = build_router();
    let base = prepare_base_collection(&router).await;
    let outputs = deployed(&router, STACK, &handler_stack()).await;

    for arm in ARMS {
        let case = arm.case;
        let function = outputs
            .get(&format!("{}Name", arm.logical))
            .cloned()
            .unwrap_or_else(|| arm.function.to_string());
        wait_until_active(&router, &function).await;

        // The names this arm writes under, so neither arm can read the other's effect back.
        let made = format!("{}-made", arm.function);
        let tag = format!("written-by-{function}");
        let created_tag = format!("created-by-{function}");

        let steps: Vec<Value> = ROWS
            .iter()
            .map(|row| {
                json!({
                    "case": row.case,
                    "call": row.probe.call(),
                    "params": row.probe.params(&base, &made, &tag, &created_tag),
                })
            })
            .collect();
        let answered = lambda(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/invocations"),
            &json!({"service": "opensearchserverless", "steps": steps}).to_string(),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: the invocation must be served, got {} with {}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.function_error, None,
            "case {case:?}: the handler must run to completion, got {:?} with {}",
            answered.function_error, answered.body
        );
        let payload: Value = ::serde_json::from_str(&answered.body).unwrap_or_else(|_| {
            panic!(
                "case {case:?}: the invocation must answer the handler's own document, got {}",
                answered.body
            )
        });
        // The SDK BUILT a client for this service: a runtime carrying no data for it raises
        // before anything is signed, which is the one outcome that tells a handler nothing
        // about the service at all.
        assert_eq!(
            payload["client"]["raised"],
            Value::Null,
            "case {case:?}: expected the client to be built, got {}",
            payload["client"]
        );

        let principal = format!("arn:aws:sts::{ACCOUNT}:assumed-role/{}/{function}", arm.role);
        for row in ROWS {
            let case = format!("{case} / {}", row.case);
            let got = payload["probed"][row.case].clone();
            assert!(
                got.is_object(),
                "case {case:?}: expected the handler to report the call, got {got}"
            );
            // Whatever the outcome, the call REACHED the service: no client-side failure of
            // the SDK stands in for the answer.
            assert_eq!(
                got["raised"],
                Value::Null,
                "case {case:?}: expected the call to be dispatched, got {got}"
            );
            if arm.allowed {
                assert_eq!(
                    got["ok"].as_bool(),
                    Some(true),
                    "case {case:?}: expected the service's own answer, got {got}"
                );
                assert_served(&case, row.probe, &base, &made, &tag, &got);
            } else {
                assert_eq!(
                    got,
                    json!({
                        "ok": false,
                        "code": ACCESS_DENIED,
                        "message": refusal_message(
                            &principal,
                            row.probe.action(),
                            &row.probe.resource(&base),
                        ),
                        "status": ACCESS_DENIED_STATUS,
                    }),
                    "case {case:?}: expected the service's own refusal, got {got}"
                );
            }
        }

        // And what the SERVICE HOLDS afterwards: a granted arm's create and tag write are its
        // state from that moment on, and a refused arm's are nowhere.
        let held = holds_collection(&router, case, &made).await;
        assert_eq!(
            held, arm.allowed,
            "case {case:?}: expected the collection {made:?} to be held={}, got held={held}",
            arm.allowed
        );
        let tags = held_tags(&router, case, &base.arn).await;
        assert_eq!(
            tags.contains(&format!("{tag}={WRITTEN_VALUE}")),
            arm.allowed,
            "case {case:?}: expected the tag {tag:?} to be held={}, got {tags:?}",
            arm.allowed
        );
        if arm.allowed {
            let arn = collection_arn(&held_collection_id(&router, case, &made).await);
            let created = held_tags(&router, case, &arn).await;
            assert!(
                created.contains(&format!("{created_tag}={CREATED_VALUE}")),
                "case {case:?}: expected the tags the create carried to be held, got {created:?}"
            );
        }
    }
}
