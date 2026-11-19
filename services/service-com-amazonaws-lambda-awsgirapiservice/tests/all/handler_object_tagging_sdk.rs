// SPDX-License-Identifier: CC0-1.0

//! An object's own TAG SET, reached from a RUNNING handler through the AWS SDK the runtime
//! presents: Lambda, IAM, the token service and Amazon S3 on ONE engine, a handler that
//! writes, reads back and removes the tags of an object, and the tag set read where a client
//! reads it.
//!
//! RULE: the tag set of an object is a subresource a handler administers through the SDK's
//! own three calls, and each reaches Amazon S3 as the function's execution role and answers
//! that service's own answer — `PutObjectTagging` replaces the whole set
//! ("`PutObjectTagging` ... replaces any existing tag-set"), `GetObjectTagging` reports the
//! set the object carries (`GetObjectTaggingOutput$TagSet` is required, so an object carrying
//! none is reported with an empty set rather than with nothing), and
//! `DeleteObjectTagging` removes the whole set. A parameter the seam does not bind is
//! reported as a call that was NOT MADE rather than sent with the value dropped.
//!
//! The rows differ along what the rule generalises over: how many tags the set carries (one,
//! several), the shape of the values in it (a plain word, a value carrying the spaces and
//! punctuation a tag value admits, an empty value), and which of the three calls acted last
//! on the object (a replace, then a removal). The last assertions read the tag set back
//! through Amazon S3's OWN API, so a call that answered without doing the work fails here.
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

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The bucket this suite's objects live in.
const BUCKET: &str = "runtime-sdk-object-tags";

/// Lambda, IAM, the token service and Amazon S3 over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's own calls are dispatched back into
/// this same router, so the object it tags is the object this suite reads.
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

/// One request to Amazon S3, made as a CLIENT makes one.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
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
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// The handler this suite runs: it replaces each object's tag set, reads it back, removes the
/// set of one object and reads that back too, and probes a parameter the seam does not bind.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def _tag_set(client, bucket, key):
    """The object's tag set as (key, value) pairs, in the order the answer carries them."""
    read = client.get_object_tagging(Bucket=bucket, Key=key)
    return [[tag["Key"], tag["Value"]] for tag in read["TagSet"]]


def lambda_handler(event, context):
    client = boto3.client("s3")
    bucket = event["bucket"]
    steps = {"written": {}, "removed": {}, "probes": {}}

    for case in event["cases"]:
        key = case["key"]
        client.put_object_tagging(
            Bucket=bucket,
            Key=key,
            Tagging={"TagSet": [{"Key": k, "Value": v} for k, v in case["tags"]]},
        )
        steps["written"][key] = _tag_set(client, bucket, key)

    # The whole set removed: the object then carries an EMPTY set rather than no answer.
    removed = event["removed"]
    client.delete_object_tagging(Bucket=bucket, Key=removed)
    steps["removed"][removed] = _tag_set(client, bucket, removed)

    # A parameter the seam does not bind is a call that was NOT MADE, naming it.
    try:
        client.put_object_tagging(
            Bucket=bucket,
            Key=event["cases"][0]["key"],
            Tagging={"TagSet": [{"Key": "k", "Value": "v"}]},
            ChecksumAlgorithm="CRC32",
        )
        steps["probes"]["unbound_parameter"] = None
    except botocore.exceptions.SdkCallNotMade as error:
        steps["probes"]["unbound_parameter"] = str(error)

    return steps
"#;

/// The policy a role that may read and tag the bucket's objects carries.
fn object_access() -> Value {
    json!([{ "Effect": "Allow", "Action": "s3:*", "Resource": "*" }])
}

/// One row: the object, and the tag set the handler replaces its set with.
struct Row {
    case: &'static str,
    key: &'static str,
    /// Source for the characters used here: `s3:PutObjectTagging`'s tag set admits letters,
    /// numbers, spaces and `+ - = . _ : / @` in a key or a value, and a value may be empty
    /// (`smithy_query(query="shape",shape="Tag")`, whose `Value` is a `Value` string with no
    /// minimum length).
    tags: &'static [(&'static str, &'static str)],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one tag",
            key: "reports/one.json",
            tags: &[("stage", "archived")],
        },
        Row {
            case: "several tags, one value carrying the punctuation a tag admits",
            key: "reports/nested/several.json",
            tags: &[
                ("stage", "archived"),
                ("owner", "team platform"),
                ("path", "a/b=c:d+e-f.g_h@i"),
            ],
        },
        Row {
            case: "a tag whose value is empty",
            key: "reports/empty-value.json",
            tags: &[("checked", "")],
        },
    ]
}

/// The `<TagSet>` of a `GetObjectTagging` answer, as (key, value) pairs.
fn tag_set_of(document: &str) -> Vec<(String, String)> {
    let texts = |name: &str| -> Vec<String> {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        document
            .split(&open)
            .skip(1)
            .filter_map(|tail| tail.split(&close).next())
            .map(str::to_string)
            .collect()
    };
    texts("Key").into_iter().zip(texts("Value")).collect()
}

/// The pairs a row wrote, as the handler and the store report them.
fn written_pairs(row: &Row) -> Vec<(String, String)> {
    row.tags
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

#[tokio::test]
async fn a_handler_replaces_reads_and_removes_an_objects_tag_set() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a handler administering an object's tag set through the runtime's SDK";
    let router = emulator();
    let (created, body) = s3(&router, "PUT", &format!("/{BUCKET}"), b"").await;
    assert_eq!(
        created, 200,
        "case {case:?}: expected CreateBucket to answer 200, got {created}: {body}"
    );
    let rows = rows();
    for row in &rows {
        let (stored, body) = s3(
            &router,
            "PUT",
            &format!("/{BUCKET}/{}", row.key),
            b"the object's bytes",
        )
        .await;
        assert_eq!(
            stored, 200,
            "case {:?}: expected PutObject to answer 200, got {stored}: {body}",
            row.case
        );
    }
    // An object whose tag set the handler then REMOVES: the row it starts from is the first
    // one, so the removal acts on a set that was written by a call of this same seam.
    let removed = rows[0].key;

    let role = given_role(&router, case, "runtime-object-tags-role", Some(object_access())).await;
    let function = "runtime-object-tagger";
    let created = call(
        &router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": function,
            "Role": role,
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
    await_function_active(&router, case, function).await;

    let answered = call(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        &json!({
            "bucket": BUCKET,
            "removed": removed,
            "cases": rows
                .iter()
                .map(|row| json!({ "key": row.key, "tags": row.tags }))
                .collect::<Vec<Value>>(),
        }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        answered.text()
    );
    let steps = answered.json();

    // What the handler read back through `GetObjectTagging`, one row per tag set written.
    for row in &rows {
        let read: Vec<(String, String)> = steps["written"][row.key]
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the handler to report the tag set it read back, got \
                     {steps}",
                    row.case
                )
            })
            .iter()
            .filter_map(|pair| {
                Some((
                    pair.get(0)?.as_str()?.to_string(),
                    pair.get(1)?.as_str()?.to_string(),
                ))
            })
            .collect();
        let mut sorted = read.clone();
        sorted.sort();
        let mut expected = written_pairs(row);
        expected.sort();
        assert_eq!(
            sorted, expected,
            "case {:?}: expected the tag set the handler wrote to be the one it read back, got \
             {read:?}",
            row.case
        );
    }

    // The removal leaves an EMPTY set, which is what the required output member reports.
    assert_eq!(
        steps["removed"][removed],
        json!([]),
        "case {case:?}: expected the removed tag set to read back empty, got {}",
        steps["removed"]
    );

    // A parameter the seam does not bind stops the call rather than travelling dropped.
    let probe = steps["probes"]["unbound_parameter"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        probe.contains("ChecksumAlgorithm"),
        "case {case:?}: expected the unbound parameter to be named as a call that was not \
         made, got {probe:?}"
    );

    // What the STORE itself holds: the tag set of every object the handler did not remove,
    // read through Amazon S3's own API, and the empty set of the one it did.
    for row in &rows {
        let (status, document) = s3(
            &router,
            "GET",
            &format!("/{BUCKET}/{}?tagging", row.key),
            b"",
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected GetObjectTagging to answer 200, got {status}: {document}",
            row.case
        );
        let mut held = tag_set_of(&document);
        held.sort();
        let mut expected = if row.key == removed {
            Vec::new()
        } else {
            written_pairs(row)
        };
        expected.sort();
        assert_eq!(
            held, expected,
            "case {:?}: expected the store to hold {expected:?}, got {held:?} in {document}",
            row.case
        );
    }
}
