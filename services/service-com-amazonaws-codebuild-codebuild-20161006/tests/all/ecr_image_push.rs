// SPDX-License-Identifier: CC0-1.0

//! What a build's own commands DO, across the three services the standard container-image
//! build spans: the build logs in to Amazon ECR, builds an image and pushes it, and the
//! registry really holds what was pushed.
//!
//! RULE under test: a build's phase commands are run by the shell of the build environment,
//! and what a command does is what the program it names does — the managed image carries the
//! AWS CLI and the Docker CLI, so `aws ecr get-login-password` performs that registry's
//! `GetAuthorizationToken`, `docker login` presents the password it issued, `docker build`
//! composes an image from the Dockerfile and the context in the build's own tree, and
//! `docker push` uploads that image to the repository its reference names. The build's
//! terminal `buildStatus` is what those commands answered: every command exiting zero is
//! `SUCCEEDED`, and a command whose call was refused exits non-zero, which fails the phase
//! it is in. Every call is authorized against the permissions in force on the project's
//! SERVICE ROLE, so a role whose policies do not allow the action makes no call and pushes
//! nothing.
//!
//! Three services have to agree for that to hold — IAM holds the role's policies, Amazon ECR
//! holds the registry and the repository, CodeBuild runs the build — so this suite mounts
//! all three on ONE engine, the way the assembled emulator does. What the registry holds
//! afterwards is read back through ECR's own reads, never out of CodeBuild: an image is
//! pushed only if the service that owns repositories reports it.
//!
//! The rows differ along what the rule generalises over: which ECR actions the build's role
//! is granted, whether the buildspec logs in at all, which repository the push addresses, and
//! whether the context holds a Dockerfile to build from.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::build_log_delivery::LOG_ACTIONS;
use super::project_build_lifecycle::{call, REGION};

/// Amazon ECR's `X-Amz-Target` prefix, from that service's own OPERATIONS.md.
const ECR_TARGET: &str = "AmazonEC2ContainerRegistry_V20150921";

/// The trust policy a build project's service role carries.
const CODEBUILD_TRUST: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
"Principal":{"Service":"codebuild.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The actions AWS documents a role needs to PUSH an image to a repository (Amazon ECR User
/// Guide, the pushing permissions it lists for a repository), beside the login the push
/// presents.
const PUSH_ACTIONS: [&str; 6] = [
    "ecr:GetAuthorizationToken",
    "ecr:BatchCheckLayerAvailability",
    "ecr:InitiateLayerUpload",
    "ecr:UploadLayerPart",
    "ecr:CompleteLayerUpload",
    "ecr:PutImage",
];

/// The media type a Docker image manifest carries, which is what the registry reports for a
/// pushed image's `imageManifestMediaType`.
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";

/// How long a wait for a build to settle may take before the suite calls it stalled.
/// Generous on purpose: the wait ends on the CONDITION, and the deadline is only there so a
/// build that never settles fails the test instead of hanging it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long the wait below leaves before it looks again.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// One emulator carrying IAM, Amazon ECR and CodeBuild over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService::new(engine),
    );
    router
}

/// One value, percent-encoded for the form body an awsQuery call carries.
fn form_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// The text of the first `<name>` element of an XML answer.
fn element(xml: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    xml.split_once(open.as_str())
        .and_then(|(_, rest)| rest.split_once(close.as_str()))
        .map(|(value, _)| value.to_string())
        .unwrap_or_default()
}

/// One awsQuery call to IAM, answered with the HTTP status and the XML body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One awsJson1_1 call to Amazon ECR, answered with the HTTP status and the JSON body.
async fn ecr(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecr.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ECR_TARGET}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/ecr/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=abc"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the test body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let decoded = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, decoded)
}

/// One Amazon ECR operation that has to succeed.
async fn ecr_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = ecr(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ecr {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// Create a role trusted by CodeBuild whose inline policy allows `actions`, beside the log
/// delivery every build's role allows ([`LOG_ACTIONS`]), and answer its ARN.
async fn role_allowing(router: &MultiServiceRouter, name: &str, actions: &[&str]) -> String {
    let (status, created) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", CODEBUILD_TRUST),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateRole {name:?}: expected the role to be persisted, got {status}: {created}"
    );
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [
            {"Effect": "Allow", "Action": actions, "Resource": "*"},
            {"Effect": "Allow", "Action": LOG_ACTIONS, "Resource": "*"},
        ],
    })
    .to_string();
    let (status, attached) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "build-permissions"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "PutRolePolicy {name:?}: expected the policy to be persisted, got {status}: {attached}"
    );
    element(&created, "Arn")
}

/// The registry a private repository of this account lives in, as Amazon ECR publishes its
/// host.
fn registry_host() -> String {
    format!(
        "{}.dkr.ecr.{REGION}.amazonaws.com",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// The buildspec of a container-image build: it writes a Dockerfile, logs in to the registry
/// (unless `logs_in` is false), builds the image and pushes it.
///
/// This is the command sequence AWS documents for pushing an image from a build, with the
/// login piped into `docker login --password-stdin` exactly as the Amazon ECR User Guide
/// writes it.
fn buildspec(reference: &str, logs_in: bool, dockerfile: bool) -> String {
    let registry = registry_host();
    let mut pre_build: Vec<String> = Vec::new();
    if dockerfile {
        pre_build.push("printf \"FROM scratch\\n\" > Dockerfile".to_string());
    }
    if logs_in {
        pre_build.push(format!(
            "aws ecr get-login-password --region $AWS_DEFAULT_REGION | docker login \
             --username AWS --password-stdin {registry}"
        ));
    }
    let listed = |commands: &[String]| {
        commands
            .iter()
            .map(|command| format!("      - '{command}'\n"))
            .collect::<String>()
    };
    format!(
        "version: 0.2\nphases:\n  pre_build:\n    commands:\n{}  build:\n    commands:\n{}  \
         post_build:\n    commands:\n{}",
        listed(&pre_build),
        listed(&[format!("docker build -t {reference} .")]),
        listed(&[format!("docker push {reference}")]),
    )
}

/// Start a build of `project` and look again until the transition the clock owes it has
/// fallen due, answering the settled record `BatchGetBuilds` reports.
async fn settled_build(router: &MultiServiceRouter, case: &str, project: &str) -> Value {
    let (status, started) = call(router, "StartBuild", json!({"projectName": project})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartBuild must answer 200, got {status} with {started}"
    );
    let id = started["build"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the start must report a build id"))
        .to_string();
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, read) = call(router, "BatchGetBuilds", json!({"ids": [id.clone()]})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: BatchGetBuilds must answer 200, got {status} with {read}"
        );
        let build = read["builds"][0].clone();
        if build["buildComplete"] == json!(true) {
            return build;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the build {id} must settle, last read {build}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The tags the repository `name` holds images under, in order, as Amazon ECR's own listing
/// reports them.
async fn image_tags(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<String> {
    let listed = ecr_ok(router, case, "ListImages", &json!({"repositoryName": name})).await;
    let mut tags: Vec<String> = listed["imageIds"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|id| id["imageTag"].as_str().map(str::to_string))
        .collect();
    tags.sort();
    tags
}

/// The `(phaseType, phaseStatus)` pairs a build reports.
fn phases_of(build: &Value) -> Vec<(String, String)> {
    build["phases"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|phase| {
            (
                phase["phaseType"].as_str().unwrap_or_default().to_string(),
                phase["phaseStatus"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// RULE: a build's status is what its own commands answered, and the image a push uploaded
/// is one the registry really holds.
#[tokio::test]
async fn a_builds_image_push_is_what_the_registry_then_holds() {
    let router = emulator();
    let registry = registry_host();
    // Two repositories: the one the pushes address, and one no push addresses — so "nothing
    // was pushed" is read against a registry that does hold repositories.
    for repository in ["pushed-images", "untouched-images"] {
        ecr_ok(
            &router,
            "the suite's repositories",
            "CreateRepository",
            &json!({"repositoryName": repository}),
        )
        .await;
    }

    /// One row: the case, the ECR actions the build's role is granted, whether the buildspec
    /// logs in, whether its context holds a Dockerfile, the repository the push addresses,
    /// the tag it pushes, the build's own status, and the phase expected to carry a failure.
    type Row = (
        &'static str,
        Vec<&'static str>,
        bool,
        bool,
        &'static str,
        &'static str,
        &'static str,
        Option<&'static str>,
    );

    let rows: Vec<Row> = vec![
        (
            "a role granted the push, logging in and pushing",
            PUSH_ACTIONS.to_vec(),
            true,
            true,
            "pushed-images",
            "first",
            "SUCCEEDED",
            None,
        ),
        (
            "the same image pushed under another tag",
            PUSH_ACTIONS.to_vec(),
            true,
            true,
            "pushed-images",
            "second",
            "SUCCEEDED",
            None,
        ),
        (
            "a role granted the login but not the push",
            vec!["ecr:GetAuthorizationToken"],
            true,
            true,
            "pushed-images",
            "denied-push",
            "FAILED",
            Some("POST_BUILD"),
        ),
        (
            "a role granted nothing of this registry",
            vec!["s3:GetObject"],
            true,
            true,
            "pushed-images",
            "denied-login",
            "FAILED",
            Some("PRE_BUILD"),
        ),
        (
            "a buildspec that never logs in",
            PUSH_ACTIONS.to_vec(),
            false,
            true,
            "pushed-images",
            "no-login",
            "FAILED",
            Some("POST_BUILD"),
        ),
        (
            "a push addressing a repository the registry does not hold",
            PUSH_ACTIONS.to_vec(),
            true,
            true,
            "absent-images",
            "absent-repo",
            "FAILED",
            Some("POST_BUILD"),
        ),
        (
            "a context with no Dockerfile to build from",
            PUSH_ACTIONS.to_vec(),
            true,
            false,
            "pushed-images",
            "no-dockerfile",
            "FAILED",
            Some("BUILD"),
        ),
    ];

    let mut pushed: Vec<String> = Vec::new();
    for (index, (case, actions, logs_in, dockerfile, repository, tag, status, failing)) in
        rows.iter().enumerate()
    {
        let role = role_allowing(&router, &format!("codebuild-ecr-{index}"), actions).await;
        let project = format!("ecr-push-proj-{index}");
        let reference = format!("{registry}/{repository}:{tag}");
        let (created_status, created) = call(
            &router,
            "CreateProject",
            json!({
                "name": project,
                "serviceRole": role,
                "source": {
                    "type": "NO_SOURCE",
                    "buildspec": buildspec(&reference, *logs_in, *dockerfile),
                },
                "artifacts": {"type": "NO_ARTIFACTS"},
                "environment": {
                    "type": "LINUX_CONTAINER",
                    "image": "aws/codebuild/amazonlinux2-x86_64-standard:5.0",
                    "computeType": "BUILD_GENERAL1_SMALL",
                    "privilegedMode": true,
                },
            }),
        )
        .await;
        assert_eq!(
            created_status, 200,
            "case {case:?}: CreateProject must answer 200, got {created_status} with {created}"
        );

        let build = settled_build(&router, case, &project).await;
        assert_eq!(
            build["buildStatus"], json!(status),
            "case {case:?}: expected buildStatus {status}, got {build}"
        );
        let phases = phases_of(&build);
        match failing {
            Some(phase) => assert!(
                phases
                    .iter()
                    .any(|(reported, status)| reported == phase && status == "FAILED"),
                "case {case:?}: expected {phase} to carry the failure, got {phases:?}"
            ),
            None => assert!(
                phases.iter().all(|(_, status)| status != "FAILED"),
                "case {case:?}: no phase may fail, got {phases:?}"
            ),
        }

        // What the REGISTRY holds is what says whether the push happened: read through ECR's
        // own listing, never out of the build record.
        if *status == "SUCCEEDED" {
            pushed.push((*tag).to_string());
            pushed.sort();
        }
        let held = image_tags(&router, case, "pushed-images").await;
        assert_eq!(
            held, pushed,
            "case {case:?}: expected the repository to hold {pushed:?}, got {held:?}"
        );
        let untouched = image_tags(&router, case, "untouched-images").await;
        assert!(
            untouched.is_empty(),
            "case {case:?}: a repository no push addressed holds nothing, got {untouched:?}"
        );
    }
}

/// RULE: a build's commands are run by the shell of the build environment, so a command that
/// materialises its Dockerfile from a Base64-encoded environment variable
/// (`base64 -d`) writes the document the build then builds from — and a `--build-arg`
/// changes the IMAGE exactly where that Dockerfile uses the variable it names: a different
/// value for a variable the document resolves into an instruction composes a different image
/// (and therefore a different digest), while a value nothing in the document uses composes
/// the very same one.
///
/// The rows differ along what the rule generalises over: whether the Dockerfile declares an
/// `ARG` for the variable the build passes, which value it passes, and whether the same
/// (document, value) pair is built twice — crossed with the tag each push addresses, so the
/// digests are read back out of Amazon ECR per tag.
#[tokio::test]
async fn a_build_argument_changes_the_image_where_the_dockerfile_uses_it() {
    let router = emulator();
    let registry = registry_host();
    let repository = "argument-images";
    ecr_ok(
        &router,
        "the suite's repository",
        "CreateRepository",
        &json!({"repositoryName": repository}),
    )
    .await;
    let role = role_allowing(&router, "codebuild-ecr-arguments", &PUSH_ACTIONS).await;

    /// A Dockerfile that RESOLVES the build argument into an instruction of the image.
    const USES_THE_ARGUMENT: &str = "FROM scratch\nARG BODY\nENV BODY=$BODY\nEXPOSE 80\n";
    /// One that declares no `ARG` for it, so a value passed for it is not in force.
    const DECLARES_NO_ARGUMENT: &str = "FROM scratch\nENV BODY=fixed\nEXPOSE 80\n";

    // (case, the Dockerfile, the value passed for BODY, the tag the push addresses)
    let rows: Vec<(&str, &str, &str, &str)> = vec![
        (
            "a Dockerfile that resolves the argument",
            USES_THE_ARGUMENT,
            "the first body",
            "uses-first",
        ),
        (
            "the same document and the same value again",
            USES_THE_ARGUMENT,
            "the first body",
            "uses-first-again",
        ),
        (
            "the same document with another value",
            USES_THE_ARGUMENT,
            "the second body",
            "uses-second",
        ),
        (
            "a document that declares no ARG for it",
            DECLARES_NO_ARGUMENT,
            "the third body",
            "no-argument-third",
        ),
        (
            "the same document with another value nothing uses",
            DECLARES_NO_ARGUMENT,
            "the fourth body",
            "no-argument-fourth",
        ),
    ];

    let mut digests: Vec<(&str, String)> = Vec::new();
    for (index, (case, dockerfile, body, tag)) in rows.iter().enumerate() {
        let reference = format!("{registry}/{repository}:{tag}");
        // The command sequence the check's own buildspec uses: the Dockerfile travels as a
        // Base64-encoded environment variable and is decoded into the working tree, then the
        // build passes the body as a build argument.
        let spec = format!(
            "version: 0.2\nphases:\n  pre_build:\n    commands:\n      - 'echo \
             \"$DOCKERFILE_B64\" | base64 -d > Dockerfile'\n      - 'aws ecr \
             get-login-password --region $AWS_REGION | docker login --username AWS \
             --password-stdin {registry}'\n  build:\n    commands:\n      - 'docker build \
             --build-arg \"BODY=$BODY\" -t {reference} .'\n  post_build:\n    commands:\n     \
             - 'docker push {reference}'\n"
        );
        let project = format!("ecr-argument-proj-{index}");
        let (status, created) = call(
            &router,
            "CreateProject",
            json!({
                "name": project,
                "serviceRole": role,
                "source": {"type": "NO_SOURCE", "buildspec": spec},
                "artifacts": {"type": "NO_ARTIFACTS"},
                "environment": {
                    "type": "LINUX_CONTAINER",
                    "image": "aws/codebuild/amazonlinux2-x86_64-standard:5.0",
                    "computeType": "BUILD_GENERAL1_SMALL",
                    "privilegedMode": true,
                    "environmentVariables": [
                        {
                            "name": "DOCKERFILE_B64",
                            "value": aws_smithy_types::base64::encode(dockerfile),
                        },
                        {"name": "BODY", "value": body},
                    ],
                },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateProject must answer 200, got {status} with {created}"
        );

        let build = settled_build(&router, case, &project).await;
        assert_eq!(
            build["buildStatus"],
            json!("SUCCEEDED"),
            "case {case:?}: every command must exit zero, got {build}"
        );

        let described = ecr_ok(
            &router,
            case,
            "DescribeImages",
            &json!({
                "repositoryName": repository,
                "imageIds": [{"imageTag": tag}],
            }),
        )
        .await;
        let digest = described["imageDetails"][0]["imageDigest"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            digest.starts_with("sha256:") && digest.len() > "sha256:".len(),
            "case {case:?}: the push must leave an image addressed by its content digest, \
             got {described}"
        );
        digests.push((case, digest));
    }

    /// The digest the row whose case is `case` pushed.
    fn digest_of<'held>(digests: &'held [(&str, String)], case: &str) -> &'held str {
        digests
            .iter()
            .find(|(held, _)| *held == case)
            .map(|(_, digest)| digest.as_str())
            .unwrap_or_else(|| panic!("case {case:?}: expected a digest for it"))
    }

    // (case, the two rows compared, whether they are the same image)
    let comparisons: Vec<(&str, &str, &str, bool)> = vec![
        (
            "the same document and value, built twice",
            "a Dockerfile that resolves the argument",
            "the same document and the same value again",
            true,
        ),
        (
            "a value the document resolves into an instruction",
            "a Dockerfile that resolves the argument",
            "the same document with another value",
            false,
        ),
        (
            "a value the document uses nowhere",
            "a document that declares no ARG for it",
            "the same document with another value nothing uses",
            true,
        ),
    ];
    for (case, left, right, same) in comparisons {
        let one = digest_of(&digests, left);
        let other = digest_of(&digests, right);
        assert_eq!(
            one == other,
            same,
            "case {case:?}: expected {} images, got {one} and {other}",
            if same { "the same" } else { "two different" }
        );
    }
}

/// RULE: what a push uploaded is a REAL image: the registry addresses it by the content
/// digest of its manifest, serves that manifest, and holds every blob the manifest names —
/// and the same context pushed under two tags is one image, because an image is addressed by
/// what it was built from.
#[tokio::test]
async fn the_pushed_image_is_one_the_registry_serves() {
    let router = emulator();
    let case = "an image pushed under two tags";
    let registry = registry_host();
    ecr_ok(
        &router,
        case,
        "CreateRepository",
        &json!({"repositoryName": "served-images"}),
    )
    .await;
    let role = role_allowing(&router, "codebuild-ecr-served", &PUSH_ACTIONS).await;
    // One buildspec, two pushes of the image it built: the second tag names the same
    // content.
    let spec = format!(
        "version: 0.2\nphases:\n  pre_build:\n    commands:\n      - 'printf \"FROM scratch\\n\" \
         > Dockerfile'\n      - 'aws ecr get-login-password --region $AWS_REGION | docker \
         login --username AWS --password-stdin {registry}'\n  build:\n    commands:\n      \
         - 'docker build -t {registry}/served-images:one .'\n      - 'docker tag \
         {registry}/served-images:one {registry}/served-images:two'\n  post_build:\n    \
         commands:\n      - 'docker push {registry}/served-images:one'\n      - 'docker push \
         {registry}/served-images:two'\n"
    );
    let (status, created) = call(
        &router,
        "CreateProject",
        json!({
            "name": "ecr-served-proj",
            "serviceRole": role,
            "source": {"type": "NO_SOURCE", "buildspec": spec},
            "artifacts": {"type": "NO_ARTIFACTS"},
            "environment": {
                "type": "LINUX_CONTAINER",
                "image": "aws/codebuild/amazonlinux2-x86_64-standard:5.0",
                "computeType": "BUILD_GENERAL1_SMALL",
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateProject must answer 200, got {status} with {created}"
    );
    let build = settled_build(&router, case, "ecr-served-proj").await;
    assert_eq!(
        build["buildStatus"], json!("SUCCEEDED"),
        "case {case:?}: the build must succeed, got {build}"
    );

    let described = ecr_ok(
        &router,
        case,
        "DescribeImages",
        &json!({"repositoryName": "served-images", "imageIds": [{"imageTag": "one"}]}),
    )
    .await;
    let detail = described["imageDetails"][0].clone();
    let digest = detail["imageDigest"].as_str().unwrap_or_default().to_string();
    assert!(
        digest.starts_with("sha256:") && digest.len() > "sha256:".len(),
        "case {case:?}: the pushed image must be addressed by its content digest, got {detail}"
    );
    assert_eq!(
        detail["imageManifestMediaType"], json!(MANIFEST_MEDIA_TYPE),
        "case {case:?}: the pushed image must carry a Docker image manifest's media type, \
         got {detail}"
    );

    // The manifest the registry SERVES names the blobs the push uploaded, and the registry
    // reports each of them as a layer it holds.
    let fetched = ecr_ok(
        &router,
        case,
        "BatchGetImage",
        &json!({"repositoryName": "served-images", "imageIds": [{"imageTag": "one"}]}),
    )
    .await;
    let manifest: Value = serde_json::from_str(
        fetched["images"][0]["imageManifest"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the registry must serve the manifest")),
    )
    .expect("the manifest the registry serves is a JSON document");
    let mut blobs: Vec<String> = manifest["config"]["digest"]
        .as_str()
        .map(str::to_string)
        .into_iter()
        .chain(
            manifest["layers"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|layer| layer["digest"].as_str().map(str::to_string)),
        )
        .collect();
    blobs.sort();
    assert_eq!(
        blobs.len(),
        2,
        "case {case:?}: the manifest must name the configuration blob and the layer, \
         got {manifest}"
    );
    let availability = ecr_ok(
        &router,
        case,
        "BatchCheckLayerAvailability",
        &json!({"repositoryName": "served-images", "layerDigests": blobs.clone()}),
    )
    .await;
    let mut available: Vec<String> = availability["layers"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|layer| layer["layerAvailability"] == json!("AVAILABLE"))
        .filter_map(|layer| layer["layerDigest"].as_str().map(str::to_string))
        .collect();
    available.sort();
    assert_eq!(
        available, blobs,
        "case {case:?}: every blob the manifest names must be a layer the repository holds, \
         got {availability}"
    );

    // The second tag names the SAME image: one content, one digest.
    let second = ecr_ok(
        &router,
        case,
        "DescribeImages",
        &json!({"repositoryName": "served-images", "imageIds": [{"imageTag": "two"}]}),
    )
    .await;
    assert_eq!(
        second["imageDetails"][0]["imageDigest"], json!(digest),
        "case {case:?}: the same image pushed under two tags has one digest, got {second}"
    );
}
