// SPDX-License-Identifier: CC0-1.0

//! Which BUILD a CodeBuild artifact's location names, across the one emulator.
//!
//! RULE under test: a build's output destination composes its key from the destination's
//! `path`, its namespace and its `name`, and the namespace a destination of
//! `namespaceType: BUILD_ID` asks for names the BUILD — which is the build's own half of
//! its id, the uuid after the `<projectName>:` prefix every id of this service carries, so
//! the project half is not part of where the build's output goes. That is the same half
//! that names the CloudWatch Logs stream the build's output was delivered to, which each
//! row reads back off the build beside its artifacts. A destination that asks for no
//! namespace composes no such segment at all.
//!
//! The rows span what the rule generalises over: whether the destination asks for the
//! build's namespace, whether it states a `path`, whether it names the output artifact (and
//! the `name` of "/" that asks for the bucket's own root), and both published
//! `ArtifactPackaging` values — `NONE`, a folder of objects, and `ZIP`, one object.
//!
//! This suite needs more than one service on one engine — CodeBuild reaches S3 (for the
//! objects) and IAM (for the project's service role) through the in-process callback, so a
//! single-service router has no store to upload to — which is why it lives here rather than
//! in the CodeBuild crate, exactly as `codebuild_s3_build_output.rs` does for the
//! composition's other parts.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode};

/// The region every request below is signed for.
const REGION: &str = "ap-southeast-2";

/// How long a poll waits for the transition the clock owes a build. Far above the span the
/// service itself allots a build, because what is waited for is the CONDITION (the build
/// reporting itself complete), never an elapsed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long one poll pauses before asking again.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// The bytes each row's build writes into the file its artifacts declare, so the object at
/// the destination can be compared with what the build produced.
const MARKER: &str = "built-output-bytes";

/// The emulator for this suite: CodeBuild, S3 and IAM on ONE engine, exactly as the router
/// mounts them, with request authentication off so each call carries a credential scope
/// (which names the Region and the service) without a computed signature.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// Dispatch one request addressed to `signing_name`, the way a signed SDK call is.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: Bytes,
) -> (u16, Vec<u8>) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let response = router
        .dispatch(request.body(body).expect("a well-formed request"))
        .await;
    (response.status().as_u16(), response.into_body().to_vec())
}

/// One awsJson1_1 call to CodeBuild, answered with the HTTP status and the JSON body.
async fn codebuild(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let (status, bytes) = send(
        router,
        "codebuild",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1".to_string()),
            ("x-amz-target", format!("CodeBuild_20161006.{operation}")),
        ],
        Bytes::from(serde_json::to_vec(&body).expect("the test body serializes")),
    )
    .await;
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `PUT /<bucket>` — S3's `CreateBucket`, naming the Region this suite is signed for.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let (status, body) = send(
        router,
        "s3",
        "PUT",
        &format!("/{bucket}"),
        &[("content-type", "application/xml".to_string())],
        Bytes::from(format!(
            "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
        )),
    )
    .await;
    assert_eq!(
        status,
        200,
        "setup: CreateBucket({bucket:?}) must succeed, got {status} / {}",
        String::from_utf8_lossy(&body)
    );
}

/// The keys the bucket holds, as its own `ListObjectsV2` reports them.
async fn object_keys(router: &MultiServiceRouter, bucket: &str) -> Vec<String> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        &format!("/{bucket}?list-type=2"),
        &[],
        Bytes::new(),
    )
    .await;
    let body = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 200,
        "ListObjectsV2({bucket:?}): got {status} / {body}"
    );
    elements(&body, "Key")
}

/// The bytes the object at `bucket`/`key` holds — `None` when the store holds none.
async fn object_bytes(router: &MultiServiceRouter, bucket: &str, key: &str) -> Option<Vec<u8>> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        &format!("/{bucket}/{key}"),
        &[],
        Bytes::new(),
    )
    .await;
    (status == 200).then_some(body)
}

/// Create an IAM role CodeBuild can assume and hand back its ARN: a project's `serviceRole`
/// is resolved against the account's IAM, so the role has to exist and trust the service
/// before a project can be stored at all.
async fn codebuild_role(router: &MultiServiceRouter, name: &str) -> String {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "codebuild.amazonaws.com" },
            "Action": "sts:AssumeRole"
        }]
    })
    .to_string();
    let form = format!(
        "Action=CreateRole&Version=2010-05-08&RoleName={}&AssumeRolePolicyDocument={}",
        form_encode(name),
        form_encode(&document)
    );
    let (status, body) = send(
        router,
        "iam",
        "POST",
        "/",
        &[(
            "content-type",
            "application/x-www-form-urlencoded".to_string(),
        )],
        Bytes::from(form),
    )
    .await;
    let body = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} / {body}"
    );
    element(&body, "Arn")
}

/// Create a project named `name` whose source carries `buildspec` inline and whose output
/// artifacts are `artifacts`.
async fn create_project(
    router: &MultiServiceRouter,
    name: &str,
    role_arn: &str,
    buildspec: &str,
    artifacts: Value,
) {
    let body = json!({
        "name": name,
        "source": {"type": "NO_SOURCE", "buildspec": buildspec},
        "artifacts": artifacts,
        "environment": {
            "type": "LINUX_CONTAINER",
            "image": "aws/codebuild/standard:7.0",
            "computeType": "BUILD_GENERAL1_SMALL",
        },
        "serviceRole": role_arn,
    });
    let (status, answer) = codebuild(router, "CreateProject", body).await;
    assert_eq!(
        status, 200,
        "setup: CreateProject({name:?}) must succeed, got {status} / {answer}"
    );
}

/// Start a build of `project` and POLL `BatchGetBuilds` until the build reports itself
/// complete, answering the settled record.
///
/// What is waited for is the build's own `buildComplete`, so the loop looks again until that
/// holds or [`SETTLE_DEADLINE`] — far above the span the service allots a build — passes.
async fn settled_build(router: &MultiServiceRouter, project: &str) -> Value {
    let (status, started) = codebuild(router, "StartBuild", json!({"projectName": project})).await;
    assert_eq!(
        status, 200,
        "StartBuild({project:?}): expected 200, got {status} / {started}"
    );
    let id = started["build"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("StartBuild({project:?}) must mint a build, got {started}"))
        .to_string();
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let (status, read) =
            codebuild(router, "BatchGetBuilds", json!({"ids": [id.clone()]})).await;
        assert_eq!(
            status, 200,
            "BatchGetBuilds({id:?}): expected 200, got {status} / {read}"
        );
        last = read["builds"][0].clone();
        if last["buildComplete"] == json!(true) {
            return last;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
    panic!("build {id} never reported itself complete; last read {last}")
}

/// A buildspec that writes the marker into one file at the build's root and declares that
/// file as the build's artifact.
fn buildspec() -> String {
    format!(
        "version: 0.2\nphases:\n  build:\n    commands:\n      \
         - \"printf '%s' '{MARKER}' > result.txt\"\nartifacts:\n  files:\n    - result.txt\n"
    )
}

/// One row of the namespace table.
struct Row {
    case: &'static str,
    /// The `artifacts` the project is created with, beside its bucket and type.
    artifacts: Value,
    /// The key the build's one object must land at. `{uuid}` stands for the build's own half
    /// of its id and `{project}` for the project's name, which is the artifact name
    /// CodeBuild derives for a destination that names none.
    key: &'static str,
    /// The key `Build$artifacts.location` must address — the folder the objects went in, or
    /// the single object a zipped destination produced.
    location: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the build's namespace under a path, with a named artifact",
            artifacts: json!({"path": "MyArtifacts", "name": "out", "namespaceType": "BUILD_ID"}),
            key: "MyArtifacts/{uuid}/out/result.txt",
            location: "MyArtifacts/{uuid}/out",
        },
        Row {
            case: "the build's namespace with no path at all",
            artifacts: json!({"name": "out", "namespaceType": "BUILD_ID"}),
            key: "{uuid}/out/result.txt",
            location: "{uuid}/out",
        },
        Row {
            case: "the build's namespace with no artifact name, which derives the project's",
            artifacts: json!({"namespaceType": "BUILD_ID"}),
            key: "{uuid}/{project}/result.txt",
            location: "{uuid}/{project}",
        },
        Row {
            case: "the build's namespace with the name that asks for the bucket's root",
            artifacts: json!({"name": "/", "namespaceType": "BUILD_ID"}),
            key: "{uuid}/result.txt",
            location: "{uuid}",
        },
        Row {
            case: "the build's namespace, zipped into one object",
            artifacts: json!({
                "path": "MyArtifacts",
                "name": "MyArtifact.zip",
                "namespaceType": "BUILD_ID",
                "packaging": "ZIP",
            }),
            key: "MyArtifacts/{uuid}/MyArtifact.zip",
            location: "MyArtifacts/{uuid}/MyArtifact.zip",
        },
        Row {
            case: "a destination that asks for no namespace",
            artifacts: json!({"path": "MyArtifacts", "name": "out", "namespaceType": "NONE"}),
            key: "MyArtifacts/out/result.txt",
            location: "MyArtifacts/out",
        },
        Row {
            case: "a destination that states no namespace type, which includes none",
            artifacts: json!({"path": "MyArtifacts", "name": "out"}),
            key: "MyArtifacts/out/result.txt",
            location: "MyArtifacts/out",
        },
    ]
}

/// The build's own half of its id: the uuid after the `<projectName>:` prefix every id this
/// service mints carries.
///
/// Read here rather than through the crate's own reader on purpose: the expectation a row
/// asserts has to be derived independently of the composition under test, or the assertion
/// would only re-state whatever that composition does.
fn uuid_half(build_id: &str) -> String {
    build_id
        .rsplit_once(':')
        .map_or(build_id, |(_, uuid)| uuid)
        .to_string()
}

/// RULE: the namespace a destination asks the build for is the build's own half of its id,
/// so the object lands — and `artifacts.location` addresses — that half and not the whole
/// id; a destination that asks for none composes no such segment.
#[tokio::test]
async fn a_builds_artifact_namespace_is_the_builds_own_half_of_its_id() {
    let router = emulator();
    let role = codebuild_role(&router, "codebuild-namespace-role").await;

    for (index, row) in rows().into_iter().enumerate() {
        // One bucket per row, so the keys asserted below are this row's whole output.
        let bucket = format!("codebuild-namespace-{index}");
        create_bucket(&router, &bucket).await;
        let project = format!("codebuild-namespace-proj-{index}");
        let mut artifacts = row.artifacts.clone();
        if let Value::Object(members) = &mut artifacts {
            members.insert("type".to_string(), json!("S3"));
            members.insert("location".to_string(), json!(bucket.clone()));
        }
        create_project(&router, &project, &role, &buildspec(), artifacts).await;

        let build = settled_build(&router, &project).await;
        assert_eq!(
            build["buildStatus"],
            json!("SUCCEEDED"),
            "case {:?}: the build must succeed to have uploaded anything, got {build}",
            row.case
        );
        let id = build["id"].as_str().unwrap_or_default().to_string();
        let uuid = uuid_half(&id);
        assert!(
            !uuid.is_empty() && uuid != id,
            "case {:?}: the build id must carry a project half and a uuid half, got {id:?}",
            row.case
        );
        let resolve =
            |pattern: &str| pattern.replace("{uuid}", &uuid).replace("{project}", &project);

        // The objects at the destination: exactly the one the buildspec declared, at the key
        // the destination composes.
        let keys = object_keys(&router, &bucket).await;
        let expected = vec![resolve(row.key)];
        assert_eq!(
            keys, expected,
            "case {:?}: expected the keys {expected:?}, got {keys:?}",
            row.case
        );
        for key in &keys {
            assert!(
                !key.contains(&id),
                "case {:?}: the whole build id {id:?} must not be part of the key {key:?}",
                row.case
            );
        }

        // What the build REPORTS for its artifacts is the S3 ARN of what it put there.
        let location = build["artifacts"]["location"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let want = format!("arn:aws:s3:::{bucket}/{}", resolve(row.location));
        assert_eq!(
            location, want,
            "case {:?}: expected artifacts.location {want:?}, got {location:?}",
            row.case
        );

        // The same half of the id names the stream the build's output was delivered to, which
        // is what makes it the build's own name rather than a composition of this one
        // destination.
        let stream = build["logs"]["streamName"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            stream, uuid,
            "case {:?}: expected the log stream {uuid:?}, got {stream:?}",
            row.case
        );
    }
}

/// RULE, on the bytes: the object the namespace composed holds what the build produced, and
/// a zipped destination holds one object rather than the folder an unzipped one leaves.
#[tokio::test]
async fn the_namespaced_object_holds_what_the_build_produced() {
    let router = emulator();
    let role = codebuild_role(&router, "codebuild-namespace-bytes-role").await;
    let bucket = "codebuild-namespace-bytes";
    create_bucket(&router, bucket).await;
    let project = "codebuild-namespace-bytes-proj";
    create_project(
        &router,
        project,
        &role,
        &buildspec(),
        json!({
            "type": "S3",
            "location": bucket,
            "path": "MyArtifacts",
            "name": "out",
            "namespaceType": "BUILD_ID",
        }),
    )
    .await;

    let build = settled_build(&router, project).await;
    let uuid = uuid_half(build["id"].as_str().unwrap_or_default());
    let key = format!("MyArtifacts/{uuid}/out/result.txt");
    let bytes = object_bytes(&router, bucket, &key).await;
    let read = bytes.map(|bytes| String::from_utf8_lossy(&bytes).to_string());
    assert_eq!(
        read.as_deref(),
        Some(MARKER),
        "case \"the namespaced object\": {key} must hold the bytes the build produced, got \
         {read:?}"
    );
}
