// SPDX-License-Identifier: CC0-1.0

//! What a handler that RESOLVES where its buckets live can do with the AWS SDK the runtime
//! supplies: Lambda, IAM, the token service and Amazon S3 on ONE engine, a Python function
//! that reports the SDK's own version, lists the account's buckets and reads the Region each
//! of them resides in, and every answer checked against what Amazon S3 itself holds.
//!
//! RULE: the AWS SDK a managed runtime supplies is the called service's own API, so a
//! handler's call on an operation that service models and serves is BUILT, signed and
//! dispatched to it, and what comes back is that service's own answer — the operation's
//! output members for a success and the service's own code, message and status for a refusal.
//! The SDK itself answers no call of a service this emulator serves, and it reports the
//! version of the SDK it is.
//!
//! The rows differ along what the rule generalises over: WHERE the bucket resides (the Region
//! an absent location constraint places it in, and each of several constraints a create may
//! name, one per partition area), and whether the bucket EXISTS at all — the last row is
//! where the other half of the rule shows: a call on a bucket Amazon S3 holds none of comes
//! back as that service's own refusal rather than as a call the SDK declined to make.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::given_role;
use crate::event_source_mappings::{await_function_active, call, router_on};

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region the function itself runs in, and the one a create naming no location constraint
/// places its bucket in ("buckets in Region us-east-1 have a LocationConstraint of null",
/// `GetBucketLocationOutput$LocationConstraint`).
const HOME_REGION: &str = "us-east-1";

/// One bucket the handler resolves: what the row is called, the bucket's name, the location
/// constraint its create named, and the constraint `GetBucketLocation` must then report for
/// it — `None` where the answer must carry none at all.
struct Located {
    /// What the row is called in a failure.
    case: &'static str,
    /// The bucket the handler asks about.
    bucket: &'static str,
    /// The `CreateBucketConfiguration.LocationConstraint` the bucket was created with, or
    /// `None` for a create that named none.
    constraint: Option<&'static str>,
    /// The constraint the read must report, or `None` for a bucket whose answer carries none.
    located_in: Option<&'static str>,
}

/// The buckets the handler resolves, one per Region a create may place a bucket in.
///
/// Each constraint is a value of `BucketLocationConstraint`
/// (`smithy_query(query="shape",shape="BucketLocationConstraint",
/// model_path="services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json")`),
/// which is the member's own vocabulary; `us-east-1` is not among them, because a bucket there
/// is the one whose answer carries no constraint.
fn located() -> Vec<Located> {
    vec![
        Located {
            case: "a create that named no location constraint",
            bucket: "handler-located-home",
            constraint: None,
            located_in: None,
        },
        Located {
            case: "a constraint in the same partition as the home Region",
            bucket: "handler-located-west",
            constraint: Some("us-west-2"),
            located_in: Some("us-west-2"),
        },
        Located {
            case: "a constraint in Europe",
            bucket: "handler-located-europe",
            constraint: Some("eu-central-1"),
            located_in: Some("eu-central-1"),
        },
        Located {
            case: "a constraint in the Asia Pacific",
            bucket: "handler-located-pacific",
            constraint: Some("ap-southeast-2"),
            located_in: Some("ap-southeast-2"),
        },
        Located {
            case: "a constraint in South America",
            bucket: "handler-located-south",
            constraint: Some("sa-east-1"),
            located_in: Some("sa-east-1"),
        },
    ]
}

/// The bucket no create was made for: the row the refusal half of the rule rests on.
const ABSENT_BUCKET: &str = "handler-located-nowhere";

/// The code Amazon S3 answers a bucket it holds none of with, which is what the SDK raises
/// for it ("the specified bucket does not exist", S3's own error list).
const NO_SUCH_BUCKET: &str = "NoSuchBucket";

/// Lambda, IAM, the token service and Amazon S3 over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's own calls are dispatched back into this
/// same router, so the buckets it resolves are the buckets this suite created.
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

/// One request to Amazon S3, made as a CLIENT makes one against the endpoint of `region`: its
/// protocol is `restXml`, so the request is a method and a path and the answer is a document.
///
/// The Region travels where a client puts it — the host it addresses and the scope its
/// signature is in — because a create naming a location constraint must reach the endpoint of
/// the Region that constraint names.
async fn s3(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: &[u8],
) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{region}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{region}/s3/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Create the bucket of `row`, in the Region its own location constraint names.
async fn given_bucket(router: &MultiServiceRouter, row: &Located) {
    let region = row.constraint.unwrap_or(HOME_REGION);
    let body = row.constraint.map(|constraint| {
        format!(
            "<CreateBucketConfiguration><LocationConstraint>{constraint}\
             </LocationConstraint></CreateBucketConfiguration>"
        )
    });
    let (status, document) = s3(
        router,
        region,
        "PUT",
        &format!("/{}", row.bucket),
        body.as_deref().unwrap_or_default().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: expected CreateBucket to answer 200, got {status}: {document}",
        row.case
    );
}

/// The handler this suite runs: the SDK's own version, the buckets the account holds, and the
/// Region each bucket the event names resides in — each reported under its own name, so what
/// is asserted is what the runtime gave the guest.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore
import botocore.exceptions


def _located(client, bucket):
    """Where one bucket resides, as the SDK the runtime supplies answers it."""
    try:
        read = client.get_bucket_location(Bucket=bucket)
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    except botocore.exceptions.SdkCallNotMade as error:
        # A call the SDK itself declined to make, which is no answer from the service at
        # all: reported as one so the assertion names it.
        return {"ok": False, "code": "SdkCallNotMade", "message": str(error)}
    return {
        "ok": True,
        "carries_member": "LocationConstraint" in read,
        "location": read.get("LocationConstraint"),
        "status": read["ResponseMetadata"]["HTTPStatusCode"],
    }


def lambda_handler(event, context):
    client = boto3.client("s3")
    listed = client.list_buckets()
    return {
        "versions": {"boto3": boto3.__version__, "botocore": botocore.__version__},
        "listing": {
            "status": listed["ResponseMetadata"]["HTTPStatusCode"],
            "names": sorted([one["Name"] for one in listed.get("Buckets", [])]),
            "owner_id": (listed.get("Owner") or {}).get("ID"),
        },
        "located": [
            {"case": row["case"], "read": _located(client, row["bucket"])}
            for row in event["buckets"]
        ],
    }
"#;

/// The policy of the role the function runs as: listing the account's buckets and reading
/// where one resides are the two permissions this handler needs
/// (`s3:ListAllMyBuckets`, `s3:GetBucketLocation`).
fn bucket_discovery() -> Value {
    json!([{
        "Effect": "Allow",
        "Action": ["s3:ListAllMyBuckets", "s3:GetBucketLocation"],
        "Resource": "*",
    }])
}

/// Create the function `name`, running as `role_arn`, out of [`HANDLER_SOURCE`].
async fn given_function(router: &MultiServiceRouter, case: &str, name: &str, role_arn: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 60,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(crate::synchronous_invoke_run::package(HANDLER_SOURCE)),
            },
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

/// Invoke `name`, handing it the table of buckets to resolve.
async fn invoked(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    rows: &[Located],
) -> Value {
    let mut buckets: Vec<Value> = rows
        .iter()
        .map(|row| json!({ "case": row.case, "bucket": row.bucket }))
        .collect();
    buckets.push(json!({
        "case": "a bucket Amazon S3 holds none of",
        "bucket": ABSENT_BUCKET,
    }));
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &json!({ "buckets": buckets }),
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
        answered.header("X-Amz-Function-Error"),
        None,
        "case {case:?}: expected the handler to run without a function error, got {}",
        answered.text()
    );
    answered.json()
}

/// Whether `reported` is a version of the SDK's own generation: three dot-separated numbers
/// whose first is the generation `boto3` and `botocore` are published in.
fn a_version_string(reported: &str) -> bool {
    let fields: Vec<&str> = reported.split('.').collect();
    fields.len() == 3
        && fields
            .iter()
            .all(|field| !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit()))
        && fields[0] == "1"
}

/// A handler resolving where its buckets live: the version the SDK reports, the listing of the
/// account's buckets, the Region each bucket resides in, and the service's own refusal for a
/// bucket that does not exist.
#[tokio::test]
async fn a_handler_resolves_where_its_buckets_live_with_what_the_runtime_supplies() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a handler resolving its buckets through the runtime's SDK";
    let router = emulator();
    let rows = located();
    for row in &rows {
        given_bucket(&router, row).await;
    }
    let role = given_role(
        &router,
        case,
        "handler-bucket-discovery-role",
        Some(bucket_discovery()),
    )
    .await;
    let function = "handler-bucket-locator";
    given_function(&router, case, function, &role).await;
    let report = invoked(&router, case, function, &rows).await;

    // The SDK reports the generation it is, under both names a handler reads a version from.
    for module in ["boto3", "botocore"] {
        let reported = report["versions"][module].as_str().unwrap_or_default();
        assert!(
            a_version_string(reported),
            "case {module:?}: expected the SDK to report its own version, got {reported:?}"
        );
    }

    // The listing: served by Amazon S3 itself, carrying every bucket the account holds and the
    // owner those buckets belong to.
    let listing = &report["listing"];
    assert_eq!(
        listing["status"],
        json!(200),
        "case {case:?}: expected Amazon S3's own status for the listing, got {listing}"
    );
    let mut expected: Vec<&str> = rows.iter().map(|row| row.bucket).collect();
    expected.sort_unstable();
    assert_eq!(
        listing["names"],
        json!(expected),
        "case {case:?}: expected every bucket the account holds, got {listing}"
    );
    assert!(
        listing["owner_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "case {case:?}: expected the listing to carry the owner's id, got {listing}"
    );

    // One assertion block per bucket: the Region the read reports is the one the bucket was
    // created in, and the member is absent exactly for the Region whose answer carries none.
    let answered = report["located"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a report per row, got {report}"));
    assert_eq!(
        answered.len(),
        rows.len() + 1,
        "case {case:?}: expected one report per bucket, got {report}"
    );
    for (row, reported) in rows.iter().zip(answered) {
        let read = &reported["read"];
        assert_eq!(
            read["ok"],
            json!(true),
            "case {:?}: expected the read to be served by Amazon S3, got {read}",
            row.case
        );
        assert_eq!(
            read["status"],
            json!(200),
            "case {:?}: expected Amazon S3's own status, got {read}",
            row.case
        );
        assert_eq!(
            read["location"],
            row.located_in.map_or(Value::Null, |region| json!(region)),
            "case {:?}: expected the constraint of the Region the bucket resides in, got {read}",
            row.case
        );
        // The member is reported either way, because that is what an answer carrying no
        // constraint says: the bucket resides where a null constraint means.
        assert_eq!(
            read["carries_member"],
            json!(true),
            "case {:?}: expected the answer to carry the member, got {read}",
            row.case
        );
        // What Amazon S3 itself holds for the same bucket, read through its own API.
        let (status, document) = s3(
            &router,
            HOME_REGION,
            "GET",
            &format!("/{}?location", row.bucket),
            b"",
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected GetBucketLocation to answer 200, got {status}: {document}",
            row.case
        );
        match row.located_in {
            Some(region) => assert!(
                document.contains(region),
                "case {:?}: expected the store to place the bucket in {region}, got {document}",
                row.case
            ),
            None => assert!(
                !document.contains("us-east-1"),
                "case {:?}: expected the store to carry no constraint for the home Region, got \
                 {document}",
                row.case
            ),
        }
    }

    // The bucket no create was made for: Amazon S3's own refusal, not a call the SDK declined.
    let refused = &answered[rows.len()]["read"];
    assert_eq!(
        refused["code"],
        json!(NO_SUCH_BUCKET),
        "case {ABSENT_BUCKET:?}: expected Amazon S3's own error code, got {refused}"
    );
    assert_eq!(
        refused["status"],
        json!(404),
        "case {ABSENT_BUCKET:?}: expected Amazon S3's own status, got {refused}"
    );
}
