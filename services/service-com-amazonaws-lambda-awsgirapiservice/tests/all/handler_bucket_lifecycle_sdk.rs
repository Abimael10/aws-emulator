// SPDX-License-Identifier: CC0-1.0

//! The BUCKET LIFECYCLE a running handler drives with the AWS SDK the runtime supplies:
//! Lambda, IAM, the token service and Amazon S3 on ONE engine, a function whose code brings a
//! bucket into being, probes it, lists the account's buckets and tears the bucket down again,
//! and every result read back where a client reads it.
//!
//! RULE: the SDK a managed runtime bundles is generated from the called service's WHOLE model,
//! so every operation that service models is a call the SDK makes — built, signed and
//! dispatched to the emulated service — and the answer is that service's own: the operation's
//! output members for a success, and the service's own error code, message and status for a
//! refusal. There is no SDK-level notion of an operation the client "binds" nothing for, so no
//! call of a modelled operation is answered by the SDK layer itself, and no refusal of one
//! carries a name no service answered with.
//!
//! The rows differ along what the rule generalises over: WHICH lifecycle call is made (the
//! create, the probe, the account-wide listing, the delete), WHAT the create's own payload
//! carries (nothing at all, the tag set the bucket comes into being with, a location constraint
//! the endpoint cannot place a bucket in), which header-bound members it names (the Object
//! Ownership setting and a canned ACL), and whether the call is SERVED or refused by Amazon S3
//! itself (a name the account already holds, a bucket that still holds an object, a name no
//! bucket exists under). The whole table runs in ONE invocation per language, because each step
//! reads what the one before it left and because an invocation is the dearest thing this target
//! does; the last assertions read everything back through S3's OWN API, so a call that answered
//! without doing the work fails here.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::given_role;
use crate::event_source_mappings::{await_function_active, call, router_on};
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than copied here.
use crate::node_invoke_run::package as node_package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for, which is therefore the Region the invocation
/// runs in and the one the handler's own client addresses.
const REGION: &str = "us-east-1";

/// The buckets the handler brings into being, one per create row it is served.
const PLAIN_BUCKET: &str = "handler-lifecycle-plain";
const TAGGED_BUCKET: &str = "handler-lifecycle-tagged";
const OWNED_BUCKET: &str = "handler-lifecycle-owned";

/// The bucket whose create the endpoint cannot serve, which therefore never comes into being.
const ELSEWHERE_BUCKET: &str = "handler-lifecycle-elsewhere";

/// The Region the refused create asks for: one that is not [`REGION`], so the endpoint the
/// handler's client is addressed at cannot place a bucket in it. Source: the enum
/// `CreateBucketConfiguration$LocationConstraint` targets
/// (`smithy_query(query="shape",shape="BucketLocationConstraint")`), of which this is one value.
const ELSEWHERE_REGION: &str = "eu-west-1";

/// The object the handler stores in the bucket it then fails to delete, which is what makes
/// that bucket one S3 refuses to remove.
const HELD_KEY: &str = "reports/held.json";

/// A name no bucket exists under, which the delete of it is refused for.
const ABSENT_BUCKET: &str = "handler-lifecycle-never-created";

/// The tag set the create carries, as the bucket's own
/// (`CreateBucketConfiguration$Tags`: "an array of tags that you can apply to the bucket that
/// you're creating").
const CREATE_TAGS: [(&str, &str); 2] = [("owner", "platform"), ("auto-delete", "true")];

/// Lambda, IAM, the token service and Amazon S3 over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's own calls are dispatched back into this
/// same router, so the bucket it creates is the bucket this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// One Amazon S3 call, made as a CLIENT makes one: its protocol is `restXml`, so the request is
/// a method and a path and the answer is a status and a document.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/s3/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// The policy of the role both functions run as: a handler driving a bucket's lifecycle needs
/// the bucket's own operations, which is what this grants.
fn bucket_lifecycle() -> Value {
    json!([{ "Effect": "Allow", "Action": "s3:*", "Resource": "*" }])
}

/// The Python handler: one step per lifecycle call, each reported under its own name, so what
/// is asserted is what the SDK gave the guest.
///
/// Only primitive values are reported, because the report travels back as the invocation's JSON
/// answer.
const HANDLER_SOURCE: &str = r#"
import os

import boto3
import botocore.exceptions


def _refusal(error):
    """The service's own code, sentence and status, as the SDK raised them."""
    return {
        "ok": False,
        "code": error.response["Error"]["Code"],
        "message": error.response["Error"]["Message"],
        "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
    }


def _made(step):
    """One call: what it answered, or why it did not answer.

    A refusal the SERVICE sent is reported as its own code; anything the SDK itself raised is
    reported under the class the SDK raised it as, so the two are told apart by the caller.
    """
    try:
        answered = step()
    except botocore.exceptions.ClientError as error:
        return _refusal(error)
    except botocore.exceptions.BotoCoreError as error:
        return {"ok": False, "code": type(error).__name__, "message": str(error)}
    return {"ok": True, "answered": answered}


def _status(made):
    """The answer's own status, reported without the answer itself."""
    if not made["ok"]:
        return made
    return {
        "ok": True,
        "status": made["answered"]["ResponseMetadata"]["HTTPStatusCode"],
    }


def _probed(client, bucket):
    """Whether the bucket is there and usable, as the probe answers it."""
    return _status(_made(lambda: client.head_bucket(Bucket=bucket)))


def _tags(client, bucket):
    """The bucket's own tag set, read back through the service's own API."""
    made = _made(lambda: client.get_bucket_tagging(Bucket=bucket))
    if not made["ok"]:
        return {"ok": False, "code": made["code"]}
    carried = made["answered"].get("TagSet", [])
    return {"ok": True, "tags": sorted([[tag["Key"], tag["Value"]] for tag in carried])}


def lambda_handler(event, context):
    client = boto3.client("s3")
    report = {"region": os.environ.get("AWS_REGION"), "creates": []}

    # One row per create: what the create answered, and — for a create that was served — the
    # bucket it brought into being, read back through the probe and the tag set.
    for row in event["creates"]:
        parameters = {"Bucket": row["bucket"]}
        if row.get("configuration"):
            parameters["CreateBucketConfiguration"] = row["configuration"]
        if row.get("ownership"):
            parameters["ObjectOwnership"] = row["ownership"]
        if row.get("acl"):
            parameters["ACL"] = row["acl"]
        made = _made(lambda: client.create_bucket(**parameters))
        entry = {"case": row["case"]}
        if made["ok"]:
            answered = made["answered"]
            entry.update(
                {
                    "ok": True,
                    "status": answered["ResponseMetadata"]["HTTPStatusCode"],
                    "location": answered.get("Location"),
                    "head": _probed(client, row["bucket"]),
                    "tags": _tags(client, row["bucket"]),
                }
            )
        else:
            entry.update(made)
        report["creates"].append(entry)

    # The account's own buckets, which is how a handler finds what it has.
    listed = _made(lambda: client.list_buckets())
    if listed["ok"]:
        held = listed["answered"].get("Buckets", [])
        report["listed"] = {
            "ok": True,
            "names": sorted([one["Name"] for one in held]),
        }
    else:
        report["listed"] = listed

    # The teardown half: a bucket that still holds an object, the same bucket once it holds
    # none, the probe of what the delete removed, and a name no bucket exists under.
    client.put_object(Bucket=event["occupied"], Key=event["key"], Body=b"held")
    deletes = {
        "occupied": _status(_made(lambda: client.delete_bucket(Bucket=event["occupied"]))),
    }
    client.delete_object(Bucket=event["occupied"], Key=event["key"])
    deletes["emptied"] = _status(
        _made(lambda: client.delete_bucket(Bucket=event["occupied"]))
    )
    deletes["probe_after"] = _probed(client, event["occupied"])
    deletes["absent"] = _status(_made(lambda: client.delete_bucket(Bucket=event["absent"])))
    report["deletes"] = deletes
    report["listed_after"] = sorted(
        [one["Name"] for one in client.list_buckets().get("Buckets", [])]
    )
    return report
"#;

/// One create row: the payload and headers the handler's create carries, and what Amazon S3
/// must answer it with.
struct Create {
    /// What the row is called in a failure.
    case: &'static str,
    /// The bucket the create addresses.
    bucket: &'static str,
    /// The `CreateBucketConfiguration` payload it carries, or `Value::Null` for a create that
    /// carries none at all.
    configuration: Value,
    /// The Object Ownership setting it names, where the row names one.
    ownership: Option<&'static str>,
    /// The canned ACL it names, where the row names one.
    acl: Option<&'static str>,
    /// The code Amazon S3 refuses this row with, or `None` for a row it serves.
    refused_with: Option<&'static str>,
    /// What the refusal's own sentence must carry, where the row states it.
    refusal_names: Option<&'static str>,
    /// The tag set the bucket carries once the row has run, for a row that is served.
    expected_tags: Vec<(&'static str, &'static str)>,
}

/// The creates the handler makes, in order: the last row addresses the name the first row's
/// bucket already holds, so it is also where a re-create of an existing bucket is observed.
fn creates() -> Vec<Create> {
    vec![
        Create {
            case: "a create carrying no configuration at all",
            bucket: PLAIN_BUCKET,
            configuration: Value::Null,
            ownership: None,
            acl: None,
            refused_with: None,
            refusal_names: None,
            expected_tags: Vec::new(),
        },
        Create {
            case: "a create whose configuration carries the bucket's own tag set",
            bucket: TAGGED_BUCKET,
            configuration: json!({
                "Tags": CREATE_TAGS
                    .iter()
                    .map(|(key, value)| json!({ "Key": key, "Value": value }))
                    .collect::<Vec<Value>>(),
            }),
            ownership: None,
            acl: None,
            refused_with: None,
            refusal_names: None,
            expected_tags: CREATE_TAGS.to_vec(),
        },
        Create {
            case: "a create naming the Object Ownership setting and a canned ACL",
            bucket: OWNED_BUCKET,
            configuration: Value::Null,
            // ACLs are disabled under the default `BucketOwnerEnforced` setting
            // (`smithy_query(query="shape",shape="ObjectOwnership")`), so a create naming an
            // ACL names the setting that leaves them enabled.
            ownership: Some("ObjectWriter"),
            acl: Some("private"),
            refused_with: None,
            refusal_names: None,
            expected_tags: Vec::new(),
        },
        Create {
            case: "a create whose location constraint the endpoint cannot place a bucket in",
            bucket: ELSEWHERE_BUCKET,
            configuration: json!({ "LocationConstraint": ELSEWHERE_REGION }),
            ownership: None,
            acl: None,
            // S3's own refusal for a constraint naming a Region the endpoint the request
            // reached cannot serve, which is the answer the call came back with rather than
            // anything the SDK decided.
            refused_with: Some("IllegalLocationConstraintException"),
            refusal_names: Some(ELSEWHERE_REGION),
            expected_tags: Vec::new(),
        },
        Create {
            case: "a create of the name the first row's bucket already holds",
            bucket: PLAIN_BUCKET,
            configuration: Value::Null,
            ownership: None,
            acl: None,
            // "For legacy compatibility, if you re-create an existing bucket that you already
            // own in the North Virginia Region, Amazon S3 returns 200 OK"
            // (`BucketAlreadyOwnedByYou`'s own documentation), and [`REGION`] is that Region.
            refused_with: None,
            refusal_names: None,
            expected_tags: Vec::new(),
        },
    ]
}

/// The class a refusal would carry if the SDK layer answered the call itself instead of
/// sending it, which no row may report.
const SDK_REFUSAL: &str = "SdkCallNotMade";

/// Create the function `name` on `runtime` out of `zip`, running as `role_arn`, and wait it out
/// to `Active`.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    runtime: &str,
    handler: &str,
    zip: Vec<u8>,
    role_arn: &str,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": runtime,
            "Handler": handler,
            "Timeout": 60,
            "Code": { "ZipFile": base64::engine::general_purpose::STANDARD.encode(zip) },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(router, case, name).await;
}

/// Invoke `name` with `event`, asserting the invocation itself was served and that the handler
/// did not fault — a `FunctionError` means a call the handler made raised, which is exactly
/// what this suite is about.
async fn invoked(router: &MultiServiceRouter, case: &str, name: &str, event: &Value) -> Value {
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        event,
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.header("x-amz-function-error"),
        None,
        "case {case:?}: expected the handler not to fault, got {}",
        answered.text()
    );
    answered.json()
}

/// Every lifecycle call a handler makes on a bucket, through the SDK a `python3.x` runtime
/// supplies: the create in each of its forms, the probe, the account-wide listing and the
/// delete — each answered by Amazon S3 itself, and each read back through that service's own
/// API.
#[tokio::test]
async fn a_handler_drives_a_buckets_lifecycle_with_what_the_runtime_supplies() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a handler driving a bucket's lifecycle through the runtime's SDK";
    let router = emulator();
    let role = given_role(
        &router,
        case,
        "handler-bucket-lifecycle-role",
        Some(bucket_lifecycle()),
    )
    .await;
    let function = "handler-bucket-lifecycle";
    given_function(
        &router,
        case,
        function,
        "python3.12",
        "lambda_function.lambda_handler",
        crate::synchronous_invoke_run::package(HANDLER_SOURCE),
        &role,
    )
    .await;

    let rows = creates();
    let creates: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "case": row.case,
                "bucket": row.bucket,
                "configuration": row.configuration,
                "ownership": row.ownership,
                "acl": row.acl,
            })
        })
        .collect();
    let report = invoked(
        &router,
        case,
        function,
        &json!({
            "creates": creates,
            // The bucket the teardown half runs over: the one the first row created, which
            // the handler fills and empties again.
            "occupied": PLAIN_BUCKET,
            "key": HELD_KEY,
            "absent": ABSENT_BUCKET,
        }),
    )
    .await;

    // The Region the invocation ran in, which is the Region the rows above are written for:
    // the endpoint the handler's client is addressed at is this Region's.
    assert_eq!(
        report["region"],
        json!(REGION),
        "case {case:?}: expected the invocation to run in {REGION}, got {report}"
    );

    // One assertion block per create row: what S3 answered, and what the bucket it brought
    // into being carries.
    let answered = report["creates"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a report per create, got {report}"));
    assert_eq!(
        answered.len(),
        rows.len(),
        "case {case:?}: expected one report per create, got {report}"
    );
    for (row, reported) in rows.iter().zip(answered) {
        let code = reported["code"].as_str().unwrap_or_default().to_string();
        assert_ne!(
            code, SDK_REFUSAL,
            "case {:?}: expected the call to reach Amazon S3, got {reported}",
            row.case
        );
        match row.refused_with {
            None => {
                assert_eq!(
                    reported["ok"],
                    json!(true),
                    "case {:?}: expected the create to be served, got {reported}",
                    row.case
                );
                assert_eq!(
                    reported["status"],
                    json!(200),
                    "case {:?}: expected the status S3 answers a create with, got {reported}",
                    row.case
                );
                // "Location — a forward slash followed by the name of the bucket"
                // (`CreateBucketOutput$Location`).
                assert_eq!(
                    reported["location"],
                    json!(format!("/{}", row.bucket)),
                    "case {:?}: expected the create to answer the new bucket's location, got \
                     {reported}",
                    row.case
                );
                assert_eq!(
                    reported["head"],
                    json!({ "ok": true, "status": 200 }),
                    "case {:?}: expected the bucket the create made to answer its probe, got \
                     {reported}",
                    row.case
                );
                let mut expected: Vec<Value> = row
                    .expected_tags
                    .iter()
                    .map(|(key, value)| json!([key, value]))
                    .collect();
                expected.sort_by_key(ToString::to_string);
                match row.expected_tags.is_empty() {
                    // A bucket that came into being with no tag set carries none, which S3
                    // answers `NoSuchTagSet` for.
                    true => assert_eq!(
                        reported["tags"],
                        json!({ "ok": false, "code": "NoSuchTagSet" }),
                        "case {:?}: expected the bucket to carry no tag set, got {reported}",
                        row.case
                    ),
                    false => assert_eq!(
                        reported["tags"],
                        json!({ "ok": true, "tags": expected }),
                        "case {:?}: expected the tag set the create carried, got {reported}",
                        row.case
                    ),
                }
            }
            Some(expected) => {
                assert_eq!(
                    code, expected,
                    "case {:?}: expected Amazon S3's own error code, got {reported}",
                    row.case
                );
                if let Some(named) = row.refusal_names {
                    let message = reported["message"].as_str().unwrap_or_default();
                    assert!(
                        message.contains(named),
                        "case {:?}: expected the refusal to carry {named:?}, got {message:?}",
                        row.case
                    );
                }
            }
        }
    }

    // The account-wide listing: every bucket a served create brought into being, and nothing
    // the refused create would have.
    let listed = report["listed"]["names"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected the listing's names, got {report}"))
        .clone();
    for name in [PLAIN_BUCKET, TAGGED_BUCKET, OWNED_BUCKET] {
        assert!(
            listed.contains(&json!(name)),
            "case {case:?}: expected the listing to hold {name:?}, got {listed:?}"
        );
    }
    assert!(
        !listed.contains(&json!(ELSEWHERE_BUCKET)),
        "case {case:?}: expected the refused create to have made no bucket, got {listed:?}"
    );

    // The teardown half: the delete of a bucket that still holds an object is S3's own
    // refusal, the delete of the same bucket once it holds none is served with the status
    // that operation answers, and the probe of what it removed is S3's own 404.
    let deletes = &report["deletes"];
    assert_eq!(
        deletes["occupied"]["code"],
        json!("BucketNotEmpty"),
        "case {case:?}: expected S3's own refusal for a bucket that still holds an object, got \
         {deletes}"
    );
    assert_eq!(
        deletes["emptied"],
        json!({ "ok": true, "status": 204 }),
        "case {case:?}: expected the delete of the emptied bucket to be served, got {deletes}"
    );
    assert_eq!(
        deletes["probe_after"]["ok"],
        json!(false),
        "case {case:?}: expected the probe of the deleted bucket to be refused, got {deletes}"
    );
    // An answer carrying no document names no error shape, and the SDK a `python3.x` runtime
    // supplies reports the STATUS as the code for a refusal it can read nothing else out of.
    assert_eq!(
        deletes["probe_after"]["code"],
        json!("404"),
        "case {case:?}: expected the code this SDK reports for a refusal carrying no document, \
         got {deletes}"
    );
    assert_eq!(
        deletes["probe_after"]["status"],
        json!(404),
        "case {case:?}: expected the status S3 answers a probe of a bucket that is not there, \
         got {deletes}"
    );
    assert_eq!(
        deletes["absent"]["code"],
        json!("NoSuchBucket"),
        "case {case:?}: expected S3's own refusal for a name no bucket exists under, got \
         {deletes}"
    );
    let left = report["listed_after"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected the listing's names, got {report}"))
        .clone();
    assert!(
        !left.contains(&json!(PLAIN_BUCKET)),
        "case {case:?}: expected the deleted bucket to be gone from the listing, got {left:?}"
    );

    // What Amazon S3 ITSELF holds, read where a client reads it: the buckets the handler kept
    // are there, and the one it deleted is not.
    for name in [TAGGED_BUCKET, OWNED_BUCKET] {
        let (status, body) = s3(&router, "HEAD", &format!("/{name}")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected S3 to hold the bucket the handler created ({name}), got \
             {status}: {body}"
        );
    }
    for name in [PLAIN_BUCKET, ELSEWHERE_BUCKET] {
        let (status, body) = s3(&router, "HEAD", &format!("/{name}")).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected S3 to hold no bucket under {name}, got {status}: {body}"
        );
    }
    let (status, document) = s3(&router, "GET", &format!("/{TAGGED_BUCKET}?tagging")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetBucketTagging to answer 200, got {status}: {document}"
    );
    for (key, value) in CREATE_TAGS {
        assert!(
            document.contains(key) && document.contains(value),
            "case {case:?}: expected the store to hold the tag {key}={value} the create \
             carried, got {document}"
        );
    }
}

/// The bucket the node handler brings into being and tears down again.
const NODE_BUCKET: &str = "handler-lifecycle-node";

/// The handler the node row runs: the same lifecycle through the v3 SDK a `nodejs*` runtime
/// supplies, with each command imported by NAME — which is what a v3 consumer writes, and which
/// only a client generated from the service's whole model exports.
const NODE_HANDLER_SOURCE: &str = r#"
const {
    S3Client,
    CreateBucketCommand,
    HeadBucketCommand,
    ListBucketsCommand,
    DeleteBucketCommand
} = require('@aws-sdk/client-s3');

const refusal = (error) => ({
    ok: false,
    name: error.name,
    message: error.message,
    status: error.$metadata === undefined ? null : error.$metadata.httpStatusCode ?? null
});

const made = async (step) => {
    try {
        return { ok: true, answered: await step() };
    } catch (error) {
        return refusal(error);
    }
};

exports.handler = async (event) => {
    const s3 = new S3Client({ region: process.env.AWS_REGION });
    const created = await made(() => s3.send(new CreateBucketCommand({ Bucket: event.bucket })));
    const probed = await made(() => s3.send(new HeadBucketCommand({ Bucket: event.bucket })));
    const listed = await made(() => s3.send(new ListBucketsCommand({})));
    const removed = await made(() => s3.send(new DeleteBucketCommand({ Bucket: event.bucket })));
    const after = await made(() => s3.send(new HeadBucketCommand({ Bucket: event.bucket })));
    const absent = await made(() => s3.send(new DeleteBucketCommand({ Bucket: event.absent })));
    return {
        created: created.ok
            ? { ok: true, location: created.answered.Location ?? null }
            : created,
        probed: probed.ok ? { ok: true } : probed,
        names: listed.ok
            ? (listed.answered.Buckets ?? []).map((one) => one.Name).sort()
            : null,
        listing: listed.ok ? { ok: true } : listed,
        removed: removed.ok ? { ok: true } : removed,
        after: after.ok ? { ok: true } : { ok: false, name: after.name, status: after.status },
        absent: absent.ok ? { ok: true } : { ok: false, name: absent.name }
    };
};
"#;

/// The rule holds for the OTHER language this runtime ships an SDK for: the same lifecycle
/// commands, exported by the client a `nodejs*` runtime bundles, each dispatched to Amazon S3
/// and answered by it — and a refusal carries the SERVICE's own name, never a credential
/// classification or the SDK's own.
#[tokio::test]
async fn a_node_handler_drives_the_same_lifecycle_through_the_bundled_client() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a node handler driving a bucket's lifecycle through the runtime's SDK";
    let router = emulator();
    let role = given_role(
        &router,
        case,
        "handler-node-bucket-lifecycle-role",
        Some(bucket_lifecycle()),
    )
    .await;
    let function = "handler-node-bucket-lifecycle";
    given_function(
        &router,
        case,
        function,
        "nodejs22.x",
        "index.handler",
        node_package(&[("index.js", NODE_HANDLER_SOURCE)]),
        &role,
    )
    .await;

    let report = invoked(
        &router,
        case,
        function,
        &json!({ "bucket": NODE_BUCKET, "absent": ABSENT_BUCKET }),
    )
    .await;

    assert_eq!(
        report["created"],
        json!({ "ok": true, "location": format!("/{NODE_BUCKET}") }),
        "case {case:?}: expected the create to be served with the new bucket's location, got \
         {report}"
    );
    assert_eq!(
        report["probed"],
        json!({ "ok": true }),
        "case {case:?}: expected the bucket the create made to answer its probe, got {report}"
    );
    assert_eq!(
        report["listing"],
        json!({ "ok": true }),
        "case {case:?}: expected the listing to be served, got {report}"
    );
    let listed = report["names"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected the listing's names, got {report}"))
        .clone();
    assert!(
        listed.contains(&json!(NODE_BUCKET)),
        "case {case:?}: expected the listing to hold {NODE_BUCKET:?}, got {listed:?}"
    );
    assert_eq!(
        report["removed"],
        json!({ "ok": true }),
        "case {case:?}: expected the delete to be served, got {report}"
    );
    // The probe of what the delete removed: an answer carrying no document names no error
    // shape, so a v3 client names the error it throws after the status — which is what a
    // handler branches on — and never after a status-shaped pseudo-code.
    assert_eq!(
        report["after"],
        json!({ "ok": false, "name": "NotFound", "status": 404 }),
        "case {case:?}: expected the probe of the deleted bucket to be refused as the class the \
         status stands for, got {report}"
    );
    // The delete of a name no bucket exists under is the SERVICE's own refusal: its own error
    // shape, and so the name a v3 client raises that shape under.
    assert_eq!(
        report["absent"],
        json!({ "ok": false, "name": "NoSuchBucket" }),
        "case {case:?}: expected Amazon S3's own refusal for a name no bucket exists under, got \
         {report}"
    );

    // And Amazon S3 itself holds no bucket under the name the handler removed, read where a
    // client reads it.
    let (status, body) = s3(&router, "HEAD", &format!("/{NODE_BUCKET}")).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected the bucket the node handler deleted to be gone, got {status}: \
         {body}"
    );
}
