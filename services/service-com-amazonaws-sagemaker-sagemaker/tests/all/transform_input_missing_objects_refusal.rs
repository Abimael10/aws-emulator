// SPDX-License-Identifier: CC0-1.0

//! What `CreateTransformJob` TELLS a caller whose `TransformInput` names an Amazon S3
//! location holding no object, driven end to end over the wire with SageMaker and Amazon S3
//! mounted on ONE shared engine.
//!
//! THE RULE UNDER TEST: a create that resolves an Amazon S3 input data source and finds no
//! object under it refuses the call 400 `ValidationException` with the whole diagnosis — the
//! S3 URL the request named, the Region the call addressed, the execution role of the model
//! the job would run, and the bucket taken off that URL, which is the resource `s3:ListBucket`
//! is granted on. One sentence for both conditions it covers, because a prefix that holds
//! nothing and a listing the documents in force withheld are indistinguishable from the
//! service's side.
//!
//! Every variable part comes from the REQUEST and from stored state, so the table varies all
//! of them independently: the Region the call is made in, the execution role the stored model
//! carries, the bucket, and the shape of the location (a prefix nothing was written under, a
//! key nothing is stored at, a deeper prefix under a stored key, a bucket this account does
//! not hold). A location that DOES hold an object is accepted, which is what makes the rows
//! above a refusal of the location rather than of every call.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulator ARN carries.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// SageMaker and Amazon S3 on ONE shared engine, the way the production router mounts every
/// service.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite exercises handler behaviour, not SigV4: the credential scope travels only
        // so the call carries the Region it was made in.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_sagemaker_sagemaker::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// A SigV4 credential scope naming `service` in `region`. Nothing is verified — the
    /// harness turns request authentication off — so these are not credentials.
    fn authorization(region: &str, service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{service}/aws4_request, \
             SignedHeaders=host, Signature=not-verified"
        )
    }

    /// One SageMaker call made in `region`, as a client makes it.
    async fn sagemaker(&self, region: &str, target: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("SageMaker.{target}"))
            .header("authorization", Self::authorization(region, "sagemaker"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One SageMaker call that must be served.
    async fn sagemaker_ok(&self, case: &str, region: &str, target: &str, body: &Value) -> Value {
        let (status, answered) = self.sagemaker(region, target, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {target}, got {status} with {answered}"
        );
        answered
    }

    /// One path-style request to the emulator's own Amazon S3 front door.
    async fn s3(&self, region: &str, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://s3.{region}.amazonaws.com{path}"))
            .header("host", format!("s3.{region}.amazonaws.com"))
            .header("authorization", Self::authorization(region, "s3"))
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        (response.status().as_u16(), response.body().to_vec())
    }

    /// `PUT /<bucket>` — `CreateBucket`. Outside `us-east-1` Amazon S3 requires the Region to
    /// be named in the request's own `CreateBucketConfiguration`.
    async fn given_bucket(&self, region: &str, bucket: &str) {
        let configuration = if region == "us-east-1" {
            String::new()
        } else {
            format!(
                "<CreateBucketConfiguration><LocationConstraint>{region}\
                 </LocationConstraint></CreateBucketConfiguration>"
            )
        };
        let (status, body) = self
            .s3(region, "PUT", &format!("/{bucket}"), configuration.as_bytes())
            .await;
        assert_eq!(
            status,
            200,
            "setup: the bucket {bucket} must be created, got {status} {}",
            String::from_utf8_lossy(&body)
        );
    }

    /// `PUT /<bucket>/<key>` — `PutObject`.
    async fn given_object(&self, region: &str, bucket: &str, key: &str, body: &str) {
        let (status, answered) = self
            .s3(region, "PUT", &format!("/{bucket}/{key}"), body.as_bytes())
            .await;
        assert_eq!(
            status,
            200,
            "setup: the object {key} must be stored, got {status} {}",
            String::from_utf8_lossy(&answered)
        );
    }
}

/// The `__type` an error body carries, with any `namespace#` prefix stripped.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` an error body carries.
fn message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The sentence AWS words for an input data source it found no object under, built from the
/// four facts the refusal is made of.
fn no_objects_found(s3_uri: &str, region: &str, role_arn: &str, bucket: &str) -> String {
    format!(
        "No S3 objects found under S3 URL \"{s3_uri}\" given in input data source. Please \
         ensure that the bucket exists in the selected region ({region}), that objects exist \
         under that S3 prefix, and that the role \"{role_arn}\" has \"s3:ListBucket\" \
         permissions on bucket \"{bucket}\"."
    )
}

/// One `CreateTransformJob` request over the location `s3_uri`.
fn transform_request(job: &str, model: &str, s3_uri: &str, output: &str) -> Value {
    json!({
        "TransformJobName": job,
        "ModelName": model,
        "TransformInput": {
            "DataSource": {"S3DataSource": {"S3DataType": "S3Prefix", "S3Uri": s3_uri}},
            "ContentType": "text/csv",
            "SplitType": "Line",
        },
        "TransformOutput": {"S3OutputPath": output, "Accept": "text/csv"},
        "TransformResources": {"InstanceType": "ml.m5.large", "InstanceCount": 1},
    })
}

/// One row of the table: where the call was made, which model (and so which execution role)
/// it names, the location it hands the job, and whether Amazon S3 holds an object under it.
struct Row {
    case: &'static str,
    /// The Region the call is signed for, which is the one its refusal names.
    region: &'static str,
    /// The name of the role the stored model runs as, which is the one its refusal names.
    role: &'static str,
    /// The bucket the location addresses.
    bucket: &'static str,
    /// The key the location names under that bucket.
    key: &'static str,
    /// Whether Amazon S3 holds an object under the location.
    populated: bool,
}

/// RULE: `CreateTransformJob` refuses an input data source holding no object with AWS's
/// four-part diagnosis — the S3 URL, the Region the call addressed, the stored model's
/// execution role and the bucket — and stores no job; a location holding an object is
/// accepted.
#[tokio::test]
async fn a_transform_input_holding_no_object_is_refused_with_the_whole_diagnosis() {
    let emulator = Emulator::new();

    // (the bucket a row addresses, the Region it is created in, the objects it holds)
    let held: [(&str, &str, &[&str]); 3] = [
        ("transform-input-east", "us-east-1", &["data/one.csv"]),
        ("transform-input-west", "eu-west-2", &["sets/part-1.csv"]),
        ("transform-input-south", "ap-southeast-1", &["single.csv"]),
    ];
    for (bucket, region, objects) in held {
        emulator.given_bucket(region, bucket).await;
        for key in objects {
            emulator.given_object(region, bucket, key, "1,2\n3,4\n").await;
        }
    }

    let rows = [
        Row {
            case: "a prefix nothing was ever written under",
            region: "us-east-1",
            role: "transform-reader-east",
            bucket: "transform-input-east",
            key: "untouched/",
            populated: false,
        },
        Row {
            case: "a key nothing is stored at",
            region: "eu-west-2",
            role: "transform-reader-west",
            bucket: "transform-input-west",
            key: "sets/does-not-exist.csv",
            populated: false,
        },
        Row {
            case: "a deeper prefix under a stored key",
            region: "ap-southeast-1",
            role: "transform-reader-south",
            bucket: "transform-input-south",
            key: "single.csv/deeper/",
            populated: false,
        },
        Row {
            case: "a bucket this account does not hold",
            region: "us-east-1",
            role: "transform-reader-absent",
            bucket: "transform-input-nowhere",
            key: "data/",
            populated: false,
        },
        Row {
            case: "a prefix holding an object",
            region: "us-east-1",
            role: "transform-reader-east",
            bucket: "transform-input-east",
            key: "data/",
            populated: true,
        },
        Row {
            case: "a key naming one stored object",
            region: "ap-southeast-1",
            role: "transform-reader-south",
            bucket: "transform-input-south",
            key: "single.csv",
            populated: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // A role ARN of this account, taken from the row: the refusal has to read it off the
        // stored MODEL rather than off the request that refers to it.
        let role_arn = format!("arn:aws:iam::{ACCOUNT}:role/{}", row.role);
        let model = format!("transformmodel{index}");
        emulator
            .sagemaker_ok(
                case,
                row.region,
                "CreateModel",
                &json!({
                    "ModelName": model,
                    "ExecutionRoleArn": role_arn,
                    "PrimaryContainer": {"Image": "demo:1"},
                }),
            )
            .await;

        let job = format!("transformjob{index}");
        let s3_uri = format!("s3://{}/{}", row.bucket, row.key);
        let (status, answered) = emulator
            .sagemaker(
                row.region,
                "CreateTransformJob",
                &transform_request(
                    &job,
                    &model,
                    &s3_uri,
                    &format!("s3://{}/out/{index}/", row.bucket),
                ),
            )
            .await;
        let (read_status, read) = emulator
            .sagemaker(
                row.region,
                "DescribeTransformJob",
                &json!({"TransformJobName": job}),
            )
            .await;

        if row.populated {
            assert_eq!(
                status, 200,
                "case {case:?}: a location holding an object must be accepted, got {status} \
                 with {answered}"
            );
            assert_eq!(
                read_status, 200,
                "case {case:?}: the accepted create must store the job, got {read_status} with \
                 {read}"
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?}: a location holding no object must be refused, got {status} with \
             {answered}"
        );
        assert_eq!(
            message(&answered),
            no_objects_found(&s3_uri, row.region, &role_arn, row.bucket),
            "case {case:?}: the refusal must name the location, the Region, the model's role \
             and the bucket, got {:?}",
            message(&answered)
        );
        // The refusal is PRE-CREATION, so the name it was made under is still free.
        assert_eq!(
            read_status, 400,
            "case {case:?}: a refused create must store nothing, got {read_status} with {read}"
        );
    }
}
