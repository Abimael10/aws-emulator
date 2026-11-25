// SPDX-License-Identifier: CC0-1.0

//! What a build's own `docker pull` DOES: the image it downloads out of this emulator's own
//! private registry, and the image it then builds ON that base and pushes back.
//!
//! RULE under test: a build's phase commands are run by the shell of the build environment,
//! and what a command does is what the program it names does — so `docker pull <reference>`
//! fetches the image the repository that reference names really holds (its manifest and every
//! blob the manifest references, through the registry's own data plane, with the credential
//! `docker login` recorded), a `docker build` whose Dockerfile's `FROM` names an image the
//! environment then holds composes an image built ON it, and the push of that image delivers
//! the base's own layers beside the new one. Every call a pull makes is authorized against the
//! permissions in force on the project's SERVICE ROLE together with the addressed
//! repository's own resource-based policy — an explicit `Deny` in either withholds the pull,
//! which fails the phase the command is in — and a repository the registry does not hold, a
//! tag it holds no image under and a registry nothing logged in to each refuse the pull the
//! way the registry refuses one.
//!
//! Three services have to agree for that to hold — IAM holds the role's policies, Amazon ECR
//! holds the registry, its repositories and the policies they share themselves with,
//! CodeBuild runs the build — so this suite mounts them on ONE engine, with CloudWatch Logs
//! beside them because the refusal a build was given is read back out of the build's own log.
//! What the registry holds afterwards, and what the pulled image was made of, is read through
//! Amazon ECR's own reads, never out of the build record.
//!
//! The rows differ along what the rule generalises over: which repository the pull addresses,
//! which tag, whether the buildspec logs in, what the addressed repository's own policy
//! states, what the role's own identity-based documents allow, and whether a repository OTHER
//! than the addressed one carries the deny.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::build_log_delivery::{
    buildspec, codebuild, delivered_messages, json_call, project_body, role_stating, settled_build,
    LOG_ACTIONS,
};
use super::ecr_repository_policy_authorizes_a_build::{
    described_image, image_not_found_message, UNSET_IDENTIFIER_MEMBER,
};
use super::project_build_lifecycle::REGION;

/// Amazon ECR's `X-Amz-Target` prefix, from that service's own OPERATIONS.md.
const ECR_TARGET: &str = "AmazonEC2ContainerRegistry_V20150921";

/// The actions AWS documents a role needs to PUSH an image to a repository (Amazon ECR User
/// Guide, the pushing permissions it lists for a repository), beside the registry login.
const PUSH_ACTIONS: [&str; 5] = [
    "ecr:BatchCheckLayerAvailability",
    "ecr:InitiateLayerUpload",
    "ecr:UploadLayerPart",
    "ecr:CompleteLayerUpload",
    "ecr:PutImage",
];

/// The actions AWS documents a role needs to PULL an image from a repository (Amazon ECR User
/// Guide, the pulling permissions it lists for a repository), beside the registry login.
const PULL_ACTIONS: [&str; 2] = ["ecr:BatchGetImage", "ecr:GetDownloadUrlForLayer"];

/// The registry-wide action the login performs.
const LOGIN_ACTION: &str = "ecr:GetAuthorizationToken";

/// The action a pull performs FIRST against the repository it addresses, which is therefore
/// the action a refusal of the pull names.
const FIRST_PULL_ACTION: &str = "ecr:BatchGetImage";

/// The media type a Docker image manifest carries, which is what this emulator's own pushes
/// store and what a pull asks for.
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";

/// The repository the base image is pushed to and pulled from.
const BASE_REPOSITORY: &str = "pull-base-images";

/// A second repository holding the same kind of base image, which no policy below ever
/// denies: the control the deny differential is read against.
const CONTROL_REPOSITORY: &str = "pull-control-images";

/// The repository every derived image is pushed to.
const DERIVED_REPOSITORY: &str = "pull-derived-images";

/// A repository name the registry holds nothing under.
const ABSENT_REPOSITORY: &str = "pull-absent-images";

/// The tag the base image is held under in both base repositories.
const BASE_TAG: &str = "base";

/// One emulator carrying IAM, Amazon ECR, CloudWatch Logs and CodeBuild over ONE shared
/// engine.
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
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService::new(engine),
    );
    router
}

/// One Amazon ECR operation that has to succeed.
async fn ecr_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = json_call(router, "ecr", ECR_TARGET, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ecr {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// The registry a private repository of this account lives in, as Amazon ECR publishes its
/// host.
fn registry_host() -> String {
    format!(
        "{}.dkr.ecr.{REGION}.amazonaws.com",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// The ARN of the repository `name` of this account's registry — the resource an authorization
/// decision about a call against it is taken on.
fn repository_arn(name: &str) -> String {
    format!(
        "arn:aws:ecr:{REGION}:{}:repository/{name}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// The environment a container build runs in: the daemon its `docker` commands are served by
/// runs only in privileged mode.
fn privileged_environment() -> Value {
    json!({"environment": {
        "type": "LINUX_CONTAINER",
        "image": "aws/codebuild/standard:7.0",
        "computeType": "BUILD_GENERAL1_SMALL",
        "privilegedMode": true,
    }})
}

/// The login command AWS documents, with the password piped into `docker login
/// --password-stdin`.
fn login_command() -> String {
    format!(
        "aws ecr get-login-password --region $AWS_DEFAULT_REGION | docker login --username AWS \
         --password-stdin {}",
        registry_host()
    )
}

/// The commands that push the BASE image: a Dockerfile built on nothing, built and pushed.
fn base_push_commands(reference: &str) -> Vec<String> {
    vec![
        "printf \"FROM scratch\\n\" > Dockerfile".to_string(),
        login_command(),
        format!("docker build -t {reference} ."),
        format!("docker push {reference}"),
    ]
}

/// The commands that PULL `base` and push the very image it delivered back under `verified`:
/// the sequence a build that verifies it can pull a private image runs, which addresses one
/// repository and sends no new content.
fn pull_and_verify_commands(base: &str, verified: &str) -> Vec<String> {
    vec![
        login_command(),
        format!("docker pull {base}"),
        format!("docker tag {base} {verified}"),
        format!("docker push {verified}"),
    ]
}

/// The commands that PULL `base` and push the image built on it as `derived`: the sequence a
/// build layering an image on a private base image runs.
fn pull_and_build_commands(base: &str, derived: &str) -> Vec<String> {
    vec![
        format!("printf \"FROM {base}\\nENV LAYERED=yes\\n\" > Dockerfile"),
        login_command(),
        format!("docker pull {base}"),
        format!("docker build -t {derived} ."),
        format!("docker push {derived}"),
    ]
}

/// The tags the repository `name` holds images under, as Amazon ECR's own listing reports
/// them.
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

/// The manifest Amazon ECR holds for the tag `tag` of the repository `repository`, as its own
/// `BatchGetImage` serves it.
async fn held_manifest(
    router: &MultiServiceRouter,
    case: &str,
    repository: &str,
    tag: &str,
) -> Value {
    let read = ecr_ok(
        router,
        case,
        "BatchGetImage",
        &json!({
            "repositoryName": repository,
            "imageIds": [{"imageTag": tag}],
            "acceptedMediaTypes": [MANIFEST_MEDIA_TYPE],
        }),
    )
    .await;
    let manifest = read["images"][0]["imageManifest"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: {repository}:{tag} must be served a manifest, got {read}")
        })
        .to_string();
    serde_json::from_str(&manifest)
        .unwrap_or_else(|_| panic!("case {case:?}: {repository}:{tag} must hold a manifest"))
}

/// The layer digests a manifest names, in its own order.
fn layer_digests(manifest: &Value) -> Vec<String> {
    manifest["layers"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|layer| layer["digest"].as_str().map(str::to_string))
        .collect()
}

/// Run a build of `project` to its terminal state and answer `(the build, its log lines)`.
async fn ran(router: &MultiServiceRouter, case: &str, project: &str) -> (Value, Vec<String>) {
    let build = settled_build(router, case, project).await;
    let group = build["logs"]["groupName"].as_str().unwrap_or_default();
    let stream = build["logs"]["streamName"].as_str().unwrap_or_default();
    let log = delivered_messages(router, group, stream)
        .await
        .unwrap_or_default();
    (build, log)
}

/// Create a project running `commands` as `role` and run it, answering what it came to.
async fn built(
    router: &MultiServiceRouter,
    case: &str,
    project: &str,
    role: &str,
    commands: &[String],
) -> (Value, Vec<String>) {
    let written: Vec<&str> = commands.iter().map(String::as_str).collect();
    codebuild(
        router,
        case,
        "CreateProject",
        &project_body(project, role, &buildspec(&written), privileged_environment()),
    )
    .await;
    ran(router, case, project).await
}

/// One row of the table: which base reference the pull addresses, what the documents in force
/// state about it, and what the build then comes to.
struct Row {
    /// The case, named in every assertion.
    case: &'static str,
    /// The tag the derived push addresses, which is also what says whether the build got that
    /// far.
    tag: &'static str,
    /// The repository the PULL addresses.
    base_repository: &'static str,
    /// The tag the pull addresses in it.
    base_tag: &'static str,
    /// Whether the buildspec logs in to the registry at all.
    logs_in: bool,
    /// Whether the role the build runs as is allowed the pull actions by its own documents.
    role_allows_the_pull: bool,
    /// The repository whose own policy DENIES the build's role the pull actions for this row —
    /// `None` for a row in which neither repository carries a policy.
    denied_repository: Option<&'static str>,
    /// The `buildStatus` the build must end in.
    status: &'static str,
    /// Whether the derived repository then holds the row's tag.
    derived_held: bool,
    /// The sentence the build's log must carry for a pull that was withheld by a document —
    /// `None` for every other row.
    refusal: Option<String>,
}

/// RULE: a build's `docker pull` delivers the image the registry holds, a build FROM it is
/// built ON that image's own layers, and the documents in force on the role and on the
/// addressed repository together decide whether the pull happens at all.
#[tokio::test]
async fn a_build_pulls_a_private_base_image_and_builds_on_it() {
    let router = emulator();
    let registry = registry_host();
    let suite = "the suite's own records";
    for repository in [BASE_REPOSITORY, CONTROL_REPOSITORY, DERIVED_REPOSITORY] {
        ecr_ok(
            &router,
            suite,
            "CreateRepository",
            &json!({"repositoryName": repository}),
        )
        .await;
    }

    // The role whose own documents allow the whole round trip, and the role allowed to log in
    // and push but nothing of a pull.
    let pulling_role = role_stating(
        &router,
        "codebuild-image-pull",
        json!([
            {"Effect": "Allow", "Action": LOGIN_ACTION, "Resource": "*"},
            {"Effect": "Allow", "Action": PULL_ACTIONS, "Resource": "*"},
            {"Effect": "Allow", "Action": PUSH_ACTIONS, "Resource": "*"},
            {"Effect": "Allow", "Action": LOG_ACTIONS, "Resource": "*"},
        ]),
    )
    .await;
    let push_only_role = role_stating(
        &router,
        "codebuild-image-push-only",
        json!([
            {"Effect": "Allow", "Action": LOGIN_ACTION, "Resource": "*"},
            {"Effect": "Allow", "Action": PUSH_ACTIONS, "Resource": "*"},
            {"Effect": "Allow", "Action": LOG_ACTIONS, "Resource": "*"},
        ]),
    )
    .await;

    // The base image every pull below addresses, delivered by a build of its own: what the
    // pulls read is what a push really put in the registry.
    let mut base_layers: Vec<String> = Vec::new();
    for (index, repository) in [BASE_REPOSITORY, CONTROL_REPOSITORY].iter().enumerate() {
        let case = "the base image a pull addresses";
        let reference = format!("{registry}/{repository}:{BASE_TAG}");
        let (build, log) = built(
            &router,
            case,
            &format!("image-pull-base-{index}"),
            &pulling_role,
            &base_push_commands(&reference),
        )
        .await;
        assert_eq!(
            build["buildStatus"],
            json!("SUCCEEDED"),
            "case {case:?}: the base push into {repository} must succeed, got {build} with \
             {log:?}"
        );
        let held = held_manifest(&router, case, repository, BASE_TAG).await;
        let layers = layer_digests(&held);
        assert_eq!(
            layers.len(),
            1,
            "case {case:?}: the base image carries its context as one layer, got {held}"
        );
        base_layers.push(layers[0].clone());
    }

    let denied_by_the_repository = |repository: &str| {
        format!(
            "is not authorized to perform: {FIRST_PULL_ACTION} on resource: {} with an explicit \
             deny in a resource-based policy",
            repository_arn(repository)
        )
    };
    let rows: Vec<Row> = vec![
        Row {
            case: "a role granted the pull, pulling the base out of the registry and pushing \
                   the image built on it",
            tag: "derived",
            base_repository: BASE_REPOSITORY,
            base_tag: BASE_TAG,
            logs_in: true,
            role_allows_the_pull: true,
            denied_repository: None,
            status: "SUCCEEDED",
            derived_held: true,
            refusal: None,
        },
        Row {
            case: "the addressed repository's own policy denying the build's role the pull \
                   actions",
            tag: "deny-blocked",
            base_repository: BASE_REPOSITORY,
            base_tag: BASE_TAG,
            logs_in: true,
            role_allows_the_pull: true,
            denied_repository: Some(BASE_REPOSITORY),
            status: "FAILED",
            derived_held: false,
            refusal: Some(denied_by_the_repository(BASE_REPOSITORY)),
        },
        Row {
            case: "the control repository, pulled while the other one carries that deny",
            tag: "control-derived",
            base_repository: CONTROL_REPOSITORY,
            base_tag: BASE_TAG,
            logs_in: true,
            role_allows_the_pull: true,
            denied_repository: Some(BASE_REPOSITORY),
            status: "SUCCEEDED",
            derived_held: true,
            refusal: None,
        },
        Row {
            case: "the identical build once that policy is deleted",
            tag: "restored",
            base_repository: BASE_REPOSITORY,
            base_tag: BASE_TAG,
            logs_in: true,
            role_allows_the_pull: true,
            denied_repository: None,
            status: "SUCCEEDED",
            derived_held: true,
            refusal: None,
        },
        Row {
            case: "a role whose own documents allow nothing of a pull",
            tag: "role-denied",
            base_repository: BASE_REPOSITORY,
            base_tag: BASE_TAG,
            logs_in: true,
            role_allows_the_pull: false,
            denied_repository: None,
            status: "FAILED",
            derived_held: false,
            refusal: Some(format!(
                "is not authorized to perform: {FIRST_PULL_ACTION} on resource: {} because no \
                 identity-based policy allows the {FIRST_PULL_ACTION} action",
                repository_arn(BASE_REPOSITORY)
            )),
        },
        Row {
            case: "a buildspec that never logs in to the registry",
            tag: "no-login",
            base_repository: BASE_REPOSITORY,
            base_tag: BASE_TAG,
            logs_in: false,
            role_allows_the_pull: true,
            denied_repository: None,
            status: "FAILED",
            derived_held: false,
            refusal: None,
        },
        Row {
            case: "a pull of a tag the repository holds no image under",
            tag: "absent-tag",
            base_repository: BASE_REPOSITORY,
            base_tag: "never-pushed",
            logs_in: true,
            role_allows_the_pull: true,
            denied_repository: None,
            status: "FAILED",
            derived_held: false,
            refusal: None,
        },
        Row {
            case: "a pull of a repository the registry does not hold",
            tag: "absent-repository",
            base_repository: ABSENT_REPOSITORY,
            base_tag: BASE_TAG,
            logs_in: true,
            role_allows_the_pull: true,
            denied_repository: None,
            status: "FAILED",
            derived_held: false,
            refusal: None,
        },
    ];

    let mut delivered: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // The documents the two base repositories carry FOR THIS ROW, stored and removed again
        // through Amazon ECR's own operations, so each row is decided by what it names.
        for repository in [BASE_REPOSITORY, CONTROL_REPOSITORY] {
            if row.denied_repository == Some(repository) {
                let policy = json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Deny",
                        "Principal": {"AWS": pulling_role.clone()},
                        "Action": PULL_ACTIONS,
                    }],
                })
                .to_string();
                ecr_ok(
                    &router,
                    case,
                    "SetRepositoryPolicy",
                    &json!({"repositoryName": repository, "policyText": policy}),
                )
                .await;
            } else {
                let (status, answer) = json_call(
                    &router,
                    "ecr",
                    ECR_TARGET,
                    "DeleteRepositoryPolicy",
                    &json!({"repositoryName": repository}),
                )
                .await;
                assert!(
                    status == 200 || status == 400,
                    "case {case:?}: {repository} either carries a policy to delete or carries \
                     none, got {status} with {answer}"
                );
            }
        }

        let role = if row.role_allows_the_pull {
            &pulling_role
        } else {
            &push_only_role
        };
        let base = format!("{registry}/{}:{}", row.base_repository, row.base_tag);
        let derived = format!("{registry}/{DERIVED_REPOSITORY}:{}", row.tag);
        let mut commands = pull_and_build_commands(&base, &derived);
        if !row.logs_in {
            commands.retain(|command| command != &login_command());
        }
        let (build, log) = built(
            &router,
            case,
            &format!("image-pull-proj-{index}"),
            role,
            &commands,
        )
        .await;
        assert_eq!(
            build["buildStatus"],
            json!(row.status),
            "case {case:?}: expected the build to end {}, got {build} with {log:?}",
            row.status
        );

        // WHY it ended there, as the client reads it: the refusal the command reported, in the
        // build's own log.
        if let Some(expected) = row.refusal.as_deref() {
            let reported = log.join("\n");
            assert!(
                reported.contains(expected),
                "case {case:?}: expected {expected:?} in the build's log, got {log:?}"
            );
        }

        // What the REGISTRY holds is what says whether the round trip happened, read through
        // Amazon ECR's own listing rather than out of the build record.
        if row.derived_held {
            delivered.push(row.tag.to_string());
            delivered.sort();
        }
        let held = image_tags(&router, case, DERIVED_REPOSITORY).await;
        assert_eq!(
            held, delivered,
            "case {case:?}: expected {DERIVED_REPOSITORY} to hold {delivered:?}, got {held:?}"
        );
        if !row.derived_held {
            continue;
        }

        // The image that was delivered is really built ON the base the pull fetched: the
        // base's own layer is beneath the one this build composed, and the registry holds
        // every blob the manifest names — which is what the push had to upload for the
        // manifest to be stored at all.
        let base_layer = if row.base_repository == CONTROL_REPOSITORY {
            base_layers[1].clone()
        } else {
            base_layers[0].clone()
        };
        let manifest = held_manifest(&router, case, DERIVED_REPOSITORY, row.tag).await;
        let layers = layer_digests(&manifest);
        assert_eq!(
            layers.len(),
            2,
            "case {case:?}: an image built on the base carries the base's layer and its own, \
             got {manifest}"
        );
        assert_eq!(
            layers.first().map(String::as_str),
            Some(base_layer.as_str()),
            "case {case:?}: expected the base's own layer {base_layer} beneath, got {layers:?}"
        );
        assert_ne!(
            layers.last().map(String::as_str),
            Some(base_layer.as_str()),
            "case {case:?}: the layer this build composed is not the base's, got {layers:?}"
        );

        // And a client asking ABOUT the tag is told it is there, with the tag it asked for.
        let described = ecr_ok(
            &router,
            case,
            "DescribeImages",
            &json!({
                "repositoryName": DERIVED_REPOSITORY,
                "imageIds": [{"imageTag": row.tag}],
            }),
        )
        .await;
        let details = described["imageDetails"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            details.len(),
            1,
            "case {case:?}: expected exactly one imageDetails entry, got {described}"
        );
        let tags: Vec<String> = details[0]["imageTags"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|tag| tag.as_str().map(str::to_string))
            .collect();
        assert!(
            tags.iter().any(|held| held == row.tag),
            "case {case:?}: expected the entry to carry the tag {:?}, got {tags:?}",
            row.tag
        );
    }
}

/// The tag a pull-and-verify build delivers back into the repository it pulled from.
const VERIFIED_TAG: &str = "pull-verified";

/// One leg of the differential below: what the addressed repository's policy states, and what
/// the identical build then comes to.
struct Leg {
    /// The case, named in every assertion.
    case: &'static str,
    /// The repository the build pulls from and pushes back into.
    repository: &'static str,
    /// Whether that repository carries a policy DENYING the build's role the pull.
    denied: bool,
    /// The `buildStatus` the build must end in.
    status: &'static str,
    /// Whether the repository then holds [`VERIFIED_TAG`].
    verified_held: bool,
}

/// RULE: the image a pull delivered is the registry's own, so pushing it back under another
/// reference of the same repository sends no new content and the reference then holds it —
/// which makes the differential a repository policy draws observable: the identical build is
/// refused while the repository denies the role the pull actions, succeeds once that policy is
/// gone, succeeds again when it is run again, and a repository that never carried the policy
/// succeeds throughout.
#[tokio::test]
async fn the_repository_policy_decides_each_run_of_one_pull_and_verify_build() {
    let router = emulator();
    let registry = registry_host();
    let suite = "the suite's own records";
    for repository in [BASE_REPOSITORY, CONTROL_REPOSITORY] {
        ecr_ok(
            &router,
            suite,
            "CreateRepository",
            &json!({"repositoryName": repository}),
        )
        .await;
    }
    let role = role_stating(
        &router,
        "codebuild-pull-verify",
        json!([
            {"Effect": "Allow", "Action": LOGIN_ACTION, "Resource": "*"},
            {"Effect": "Allow", "Action": PULL_ACTIONS, "Resource": "*"},
            {"Effect": "Allow", "Action": PUSH_ACTIONS, "Resource": "*"},
            {"Effect": "Allow", "Action": LOG_ACTIONS, "Resource": "*"},
        ]),
    )
    .await;

    // The base image both repositories hold, delivered by a build of its own.
    for (index, repository) in [BASE_REPOSITORY, CONTROL_REPOSITORY].iter().enumerate() {
        let case = "the base image the verify builds pull";
        let (build, log) = built(
            &router,
            case,
            &format!("pull-verify-base-{index}"),
            &role,
            &base_push_commands(&format!("{registry}/{repository}:{BASE_TAG}")),
        )
        .await;
        assert_eq!(
            build["buildStatus"],
            json!("SUCCEEDED"),
            "case {case:?}: the base push into {repository} must succeed, got {build} with \
             {log:?}"
        );
    }

    let legs: Vec<Leg> = vec![
        Leg {
            case: "the repository denying the build's role the pull actions",
            repository: BASE_REPOSITORY,
            denied: true,
            status: "FAILED",
            verified_held: false,
        },
        Leg {
            case: "the control repository, pulled while the other one carries that deny",
            repository: CONTROL_REPOSITORY,
            denied: false,
            status: "SUCCEEDED",
            verified_held: true,
        },
        Leg {
            case: "the identical build once that policy is deleted",
            repository: BASE_REPOSITORY,
            denied: false,
            status: "SUCCEEDED",
            verified_held: true,
        },
        Leg {
            case: "the identical build run once more, with nothing left to send",
            repository: BASE_REPOSITORY,
            denied: false,
            status: "SUCCEEDED",
            verified_held: true,
        },
        Leg {
            case: "the control repository again, throughout",
            repository: CONTROL_REPOSITORY,
            denied: false,
            status: "SUCCEEDED",
            verified_held: true,
        },
    ];

    for (index, leg) in legs.iter().enumerate() {
        let case = leg.case;
        if leg.denied {
            let policy = json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": {"AWS": role.clone()},
                    "Action": PULL_ACTIONS,
                }],
            })
            .to_string();
            ecr_ok(
                &router,
                case,
                "SetRepositoryPolicy",
                &json!({"repositoryName": leg.repository, "policyText": policy}),
            )
            .await;
        } else {
            let (status, answer) = json_call(
                &router,
                "ecr",
                ECR_TARGET,
                "DeleteRepositoryPolicy",
                &json!({"repositoryName": leg.repository}),
            )
            .await;
            assert!(
                status == 200 || status == 400,
                "case {case:?}: {} either carries a policy to delete or carries none, got \
                 {status} with {answer}",
                leg.repository
            );
        }

        let base = format!("{registry}/{}:{BASE_TAG}", leg.repository);
        let verified = format!("{registry}/{}:{VERIFIED_TAG}", leg.repository);
        let (build, log) = built(
            &router,
            case,
            &format!("pull-verify-proj-{index}"),
            &role,
            &pull_and_verify_commands(&base, &verified),
        )
        .await;
        assert_eq!(
            build["buildStatus"],
            json!(leg.status),
            "case {case:?}: expected the build to end {}, got {build} with {log:?}",
            leg.status
        );

        // What a client asking ABOUT the verified tag is told: the entry is there once the
        // pull was allowed, and the refusal echoes the entry the request sent while it was
        // not.
        let (status, class, message) =
            described_image(&router, leg.repository, &json!({"imageTag": VERIFIED_TAG})).await;
        if leg.verified_held {
            assert_eq!(
                status, 200,
                "case {case:?}: the tag the build delivered is described, got {status} with \
                 {class:?} {message:?}"
            );
            // The image pushed back is the very one the pull delivered: the repository holds
            // it under both references.
            let pulled = held_manifest(&router, case, leg.repository, BASE_TAG).await;
            let delivered = held_manifest(&router, case, leg.repository, VERIFIED_TAG).await;
            assert_eq!(
                layer_digests(&delivered),
                layer_digests(&pulled),
                "case {case:?}: expected the pulled image's own layers, got {delivered}"
            );
        } else {
            assert_eq!(
                (status, class.as_str()),
                (400, "ImageNotFoundException"),
                "case {case:?}: a tag no push delivered is refused ImageNotFoundException, got \
                 {status} with {class:?} {message:?}"
            );
            let expected =
                image_not_found_message(UNSET_IDENTIFIER_MEMBER, VERIFIED_TAG, leg.repository);
            assert_eq!(
                message, expected,
                "case {case:?}: expected the refusal to echo the requested entry as \
                 {expected:?}, got {message:?}"
            );
        }

        // The repository holds ONE image under each reference however often the build ran: a
        // push with nothing left to send adds nothing.
        let tags = image_tags(&router, case, leg.repository).await;
        let expected: Vec<String> = if leg.verified_held {
            vec![BASE_TAG.to_string(), VERIFIED_TAG.to_string()]
        } else {
            vec![BASE_TAG.to_string()]
        };
        assert_eq!(
            tags, expected,
            "case {case:?}: expected {} to hold {expected:?}, got {tags:?}",
            leg.repository
        );
    }
}
