// SPDX-License-Identifier: CC0-1.0

//! Which documents decide the registry calls a build's own commands make against a repository
//! of this account.
//!
//! RULE under test: TWO documents can allow a call made for a build against a repository — the
//! identity-based policies in force on the role the build RUNS AS, and that repository's own
//! resource-based policy — so an explicit `Deny` in EITHER withholds the call, and otherwise an
//! `Allow` from either is enough. A repository whose policy denies the role the build runs as
//! is therefore a repository that build cannot push to: the command exits non-zero with the
//! refusal naming the document that withheld it, the phase it is in fails, and the registry
//! holds nothing. A statement that names another principal, or another repository, withholds
//! nothing.
//!
//! Three services have to agree for that to hold — IAM holds the role's policies, Amazon ECR
//! holds the repository and the policy it shares itself with, CodeBuild runs the build — so
//! this suite mounts them on ONE engine, with CloudWatch Logs beside them because the refusal
//! a build was given is read back out of the build's own log. What the registry holds
//! afterwards is read through Amazon ECR's own listing, never out of the build record.
//!
//! The rows differ along what the rule generalises over: the effect the repository's policy
//! states, the spelling it names the build by (the role itself or the account it belongs to),
//! the actions it covers, the repository it covers, and what the role's own identity-based
//! documents say about the same call.
//!
//! The other half of every check of this shape is what a client asking ABOUT the tag the push
//! never delivered is told, so each row reads it: an `imageIds` entry the repository answers
//! nothing to is refused `ImageNotFoundException`, and the sentence echoes the ENTRY the
//! request sent — both members of it spelled out, the one the caller omitted as the literal
//! `null` — then the repository and the registry it was looked for in.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::build_log_delivery::{
    buildspec, codebuild, delivered_messages, json_call, project_body, role_stating, settled_build,
    LOG_ACTIONS,
};
use super::project_build_lifecycle::REGION;

/// Amazon ECR's `X-Amz-Target` prefix, from that service's own OPERATIONS.md.
const ECR_TARGET: &str = "AmazonEC2ContainerRegistry_V20150921";

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

/// The action a push performs FIRST against the repository it addresses, which is the action a
/// refusal of the push therefore names.
const FIRST_PUSH_ACTION: &str = "ecr:InitiateLayerUpload";

/// The action a push performs LAST: the manifest it stores once every blob is in place.
const LAST_PUSH_ACTION: &str = "ecr:PutImage";

/// The repository the pushes below address.
const SHARED_REPOSITORY: &str = "shared-images";

/// A second repository of the same registry, which no push addresses: a statement scoped to it
/// decides nothing about a push to [`SHARED_REPOSITORY`].
const OTHER_REPOSITORY: &str = "other-images";

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
    format!("{}.dkr.ecr.{REGION}.amazonaws.com", engine::EMULATOR_ACCOUNT_ID)
}

/// The ARN of the repository `name` of this account's registry — the resource a statement about
/// a call against it names ("Resource types defined by Amazon Elastic Container Registry",
/// `repository`).
fn repository_arn(name: &str) -> String {
    format!(
        "arn:aws:ecr:{REGION}:{}:repository/{name}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// The root ARN of this account, which is how a resource-based policy names the whole account.
fn account_root() -> String {
    format!("arn:aws:iam::{}:root", engine::EMULATOR_ACCOUNT_ID)
}

/// The tags the repository `name` holds images under, as Amazon ECR's own listing reports them.
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

/// How a member of a rendered `ImageIdentifier` reads when the request supplied none: Amazon
/// ECR spells both members of the entry out and the absent one as the literal `null`.
const UNSET_IDENTIFIER_MEMBER: &str = "null";

/// Amazon ECR's sentence for an `imageIds` entry the repository `repository` answers nothing
/// to: the entry the REQUEST sent — both members spelled out, the one it omitted as
/// [`UNSET_IDENTIFIER_MEMBER`] — then the repository and the registry it was looked for in.
fn image_not_found_message(digest: &str, tag: &str, repository: &str) -> String {
    format!(
        "The image with imageId {{imageDigest:'{digest}', imageTag:'{tag}'}} does not exist \
         within the repository with name '{repository}' in the registry with id '{}'",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// What Amazon ECR answers `DescribeImages` for the single `imageIds` entry `id` of the
/// repository `repository`: the HTTP status, the error type and the message.
async fn described_image(
    router: &MultiServiceRouter,
    repository: &str,
    id: &Value,
) -> (u16, String, String) {
    let (status, answer) = json_call(
        router,
        "ecr",
        ECR_TARGET,
        "DescribeImages",
        &json!({"repositoryName": repository, "imageIds": [id]}),
    )
    .await;
    let class = answer["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string();
    let message = answer["message"].as_str().unwrap_or_default().to_string();
    (status, class, message)
}

/// A content digest no push stored, in the `<algorithm>:<hex>` form a content digest has: a
/// digest-addressed entry the registry answers nothing to.
const UNSTORED_DIGEST: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";

/// The commands a container-image build runs: it writes a Dockerfile, logs in to the registry
/// with the password Amazon ECR issues, builds the image and pushes it.
///
/// This is the command sequence AWS documents for pushing an image from a build, with the login
/// piped into `docker login --password-stdin` exactly as the Amazon ECR User Guide writes it.
fn push_commands(reference: &str) -> Vec<String> {
    vec![
        "printf \"FROM scratch\\n\" > Dockerfile".to_string(),
        format!(
            "aws ecr get-login-password --region $AWS_DEFAULT_REGION | docker login \
             --username AWS --password-stdin {}",
            registry_host()
        ),
        format!("docker build -t {reference} ."),
        format!("docker push {reference}"),
    ]
}

/// The environment a container build runs in: the daemon its `docker` commands are served by
/// runs only in privileged mode.
fn privileged_environment() -> Value {
    json!({"environment": {
        "type": "LINUX_CONTAINER",
        "image": "aws/codebuild/amazonlinux2-x86_64-standard:5.0",
        "computeType": "BUILD_GENERAL1_SMALL",
        "privilegedMode": true,
    }})
}

/// One row of the table: what the role's identity-based documents state, what the repository's
/// own policy states, and what the push then comes to.
struct Row {
    /// The case, named in every assertion.
    case: &'static str,
    /// The tag this row's push addresses, which is also what says whether it happened.
    tag: &'static str,
    /// Which of the two roles below the build runs as.
    login_only_role: bool,
    /// The statements the policy of the repository the push ADDRESSES carries — `None` for a
    /// repository that carries no policy at all.
    shared: Option<Value>,
    /// The statements the policy of the OTHER repository carries, which the push addresses
    /// nothing of.
    shared_elsewhere: Option<Value>,
    /// The `buildStatus` the build must end in.
    status: &'static str,
    /// Whether the registry then holds the tag.
    held: bool,
    /// The sentence the build's log must carry for a push that was withheld — `None` for a push
    /// that was made.
    refusal: Option<String>,
}

/// RULE: a build's push is allowed by the role's identity-based policies and by the
/// repository's own resource-based policy together — an explicit deny in either withholds it,
/// an allow from either is enough — and what the registry holds afterwards says which happened.
#[tokio::test]
async fn a_repository_policy_decides_the_registry_calls_a_build_makes() {
    let router = emulator();
    let suite = "the suite's own records";
    for repository in [SHARED_REPOSITORY, OTHER_REPOSITORY] {
        ecr_ok(
            &router,
            suite,
            "CreateRepository",
            &json!({"repositoryName": repository}),
        )
        .await;
    }

    // The role whose own documents allow the whole push, and the role whose own documents allow
    // only the registry login — for the latter, the repository's policy is the only document
    // that can allow the push at all.
    let pushing_role = role_stating(
        &router,
        "codebuild-repository-policy",
        json!([
            {"Effect": "Allow", "Action": PUSH_ACTIONS, "Resource": "*"},
            {"Effect": "Allow", "Action": LOG_ACTIONS, "Resource": "*"},
        ]),
    )
    .await;
    let login_only_role = role_stating(
        &router,
        "codebuild-repository-policy-login",
        json!([
            {"Effect": "Allow", "Action": "ecr:GetAuthorizationToken", "Resource": "*"},
            {"Effect": "Allow", "Action": LOG_ACTIONS, "Resource": "*"},
        ]),
    )
    .await;
    // A principal of this account that is not either of them: a statement about it decides
    // nothing about a build running as one of them. It is a role IAM really holds, because a
    // repository policy may only grant to an identity that exists.
    let other_role = role_stating(
        &router,
        "codebuild-somebody-else",
        json!([{"Effect": "Allow", "Action": PUSH_ACTIONS, "Resource": "*"}]),
    )
    .await;

    let denied_by_the_repository = |action: &str| {
        format!(
            "is not authorized to perform: {action} on resource: {} with an explicit deny in a \
             resource-based policy",
            repository_arn(SHARED_REPOSITORY)
        )
    };
    // A statement of a REPOSITORY policy names no `Resource`: the repository the document is
    // attached to is the resource every statement of it applies to, which is why Amazon ECR's
    // own repository-policy examples name none.
    let rows: Vec<Row> = vec![
        Row {
            case: "a repository that shares itself with nobody, pushed to by a role whose own \
                   documents allow the push",
            tag: "no-policy",
            login_only_role: false,
            shared: None,
            shared_elsewhere: None,
            status: "SUCCEEDED",
            held: true,
            refusal: None,
        },
        Row {
            case: "a repository whose policy denies the build's role every action of this \
                   registry",
            tag: "role-denied",
            login_only_role: false,
            shared: Some(json!([{
                "Effect": "Deny",
                "Principal": {"AWS": pushing_role.clone()},
                "Action": "ecr:*",
            }])),
            shared_elsewhere: None,
            status: "FAILED",
            held: false,
            refusal: Some(denied_by_the_repository(FIRST_PUSH_ACTION)),
        },
        Row {
            case: "a repository whose policy denies the whole ACCOUNT the one action the push \
                   ends with",
            tag: "account-denied",
            login_only_role: false,
            shared: Some(json!([{
                "Effect": "Deny",
                "Principal": {"AWS": account_root()},
                "Action": LAST_PUSH_ACTION,
            }])),
            shared_elsewhere: None,
            status: "FAILED",
            held: false,
            refusal: Some(denied_by_the_repository(LAST_PUSH_ACTION)),
        },
        Row {
            case: "a repository whose policy denies another principal of the same account",
            tag: "somebody-else-denied",
            login_only_role: false,
            shared: Some(json!([{
                "Effect": "Deny",
                "Principal": {"AWS": other_role.clone()},
                "Action": "ecr:*",
            }])),
            shared_elsewhere: None,
            status: "SUCCEEDED",
            held: true,
            refusal: None,
        },
        Row {
            case: "ANOTHER repository of the same registry denying the build's role, with the \
                   addressed one sharing itself with nobody",
            tag: "elsewhere-denied",
            login_only_role: false,
            shared: None,
            shared_elsewhere: Some(json!([{
                "Effect": "Deny",
                "Principal": {"AWS": pushing_role.clone()},
                "Action": "ecr:*",
            }])),
            status: "SUCCEEDED",
            held: true,
            refusal: None,
        },
        Row {
            case: "a repository whose policy shares it with a role whose own documents allow \
                   nothing of the push",
            tag: "shared-with-the-role",
            login_only_role: true,
            shared: Some(json!([{
                "Effect": "Allow",
                "Principal": {"AWS": login_only_role.clone()},
                "Action": PUSH_ACTIONS,
            }])),
            shared_elsewhere: None,
            status: "SUCCEEDED",
            held: true,
            refusal: None,
        },
        Row {
            case: "ANOTHER repository sharing itself with that role, with the addressed one \
                   sharing itself with nobody",
            tag: "shared-elsewhere",
            login_only_role: true,
            shared: None,
            shared_elsewhere: Some(json!([{
                "Effect": "Allow",
                "Principal": {"AWS": login_only_role.clone()},
                "Action": PUSH_ACTIONS,
            }])),
            status: "FAILED",
            held: false,
            refusal: Some(format!(
                "is not authorized to perform: {FIRST_PUSH_ACTION} on resource: {} because no \
                 identity-based policy allows the {FIRST_PUSH_ACTION} action",
                repository_arn(SHARED_REPOSITORY)
            )),
        },
    ];

    let mut pushed: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // The documents the two repositories share themselves with for THIS row, stored through
        // Amazon ECR's own operation — and removed again where the row states none, so each row
        // is decided by the documents it names.
        for (repository, statements) in [
            (SHARED_REPOSITORY, row.shared.as_ref()),
            (OTHER_REPOSITORY, row.shared_elsewhere.as_ref()),
        ] {
            match statements {
                Some(statements) => {
                    let policy =
                        json!({"Version": "2012-10-17", "Statement": statements}).to_string();
                    ecr_ok(
                        &router,
                        case,
                        "SetRepositoryPolicy",
                        &json!({"repositoryName": repository, "policyText": policy}),
                    )
                    .await;
                }
                None => {
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
                        "case {case:?}: {repository} either carries a policy to delete or \
                         carries none, got {status} with {answer}"
                    );
                }
            }
        }

        let role = if row.login_only_role {
            &login_only_role
        } else {
            &pushing_role
        };
        let project = format!("repository-policy-proj-{index}");
        let reference = format!("{}/{SHARED_REPOSITORY}:{}", registry_host(), row.tag);
        let commands = push_commands(&reference);
        let written: Vec<&str> = commands.iter().map(String::as_str).collect();
        codebuild(
            &router,
            case,
            "CreateProject",
            &project_body(
                &project,
                role,
                &buildspec(&written),
                privileged_environment(),
            ),
        )
        .await;

        let build = settled_build(&router, case, &project).await;
        assert_eq!(
            build["buildStatus"],
            json!(row.status),
            "case {case:?}: expected the build to end {}, got {build}",
            row.status
        );

        // WHY the build ended there, as the client reads it: the refusal the command reported,
        // in the build's own log.
        if let Some(expected) = row.refusal.as_deref() {
            let group = build["logs"]["groupName"].as_str().unwrap_or_default();
            let stream = build["logs"]["streamName"].as_str().unwrap_or_default();
            let messages = delivered_messages(&router, group, stream)
                .await
                .unwrap_or_else(|| {
                    panic!("case {case:?}: CloudWatch Logs must hold {group}/{stream}")
                });
            let reported = messages.join("\n");
            assert!(
                reported.contains(expected),
                "case {case:?}: expected {expected:?} in the build's log, got {messages:?}"
            );
        }

        // What the REGISTRY holds is what says whether the push happened: read through Amazon
        // ECR's own listing, never out of the build record.
        if row.held {
            pushed.push(row.tag.to_string());
            pushed.sort();
        }
        let held = image_tags(&router, case, SHARED_REPOSITORY).await;
        assert_eq!(
            held, pushed,
            "case {case:?}: expected the repository to hold {pushed:?}, got {held:?}"
        );
        let untouched = image_tags(&router, case, OTHER_REPOSITORY).await;
        assert!(
            untouched.is_empty(),
            "case {case:?}: a repository no push addressed holds nothing, got {untouched:?}"
        );

        // What a client asking ABOUT that tag is then told, which is the other half of every
        // check of this shape: a tag the push never delivered is an `imageIds` entry the
        // repository answers nothing to, and the refusal echoes the entry the request sent.
        let (status, class, message) =
            described_image(&router, SHARED_REPOSITORY, &json!({"imageTag": row.tag})).await;
        if row.held {
            assert_eq!(
                status, 200,
                "case {case:?}: the tag the push delivered is described, got {status} \
                 with {class:?} {message:?}"
            );
        } else {
            assert_eq!(
                (status, class.as_str()),
                (400, "ImageNotFoundException"),
                "case {case:?}: a tag no push delivered is refused ImageNotFoundException, got \
                 {status} with {class:?} {message:?}"
            );
            let expected = image_not_found_message(
                UNSET_IDENTIFIER_MEMBER,
                row.tag,
                SHARED_REPOSITORY,
            );
            assert_eq!(
                message, expected,
                "case {case:?}: expected the refusal to echo the requested entry as \
                 {expected:?}, got {message:?}"
            );
        }
    }

    // The same refusal for the OTHER way an entry addresses an image: a digest-addressed entry
    // the registry answers nothing to echoes the digest, with the tag it omitted as the literal
    // null — so the sentence is built from the entry the request sent, whichever member it
    // carries.
    let case = "a digest-addressed entry nothing stored";
    let (status, class, message) =
        described_image(&router, SHARED_REPOSITORY, &json!({"imageDigest": UNSTORED_DIGEST})).await;
    assert_eq!(
        (status, class.as_str()),
        (400, "ImageNotFoundException"),
        "case {case:?}: expected ImageNotFoundException, got {status} with {class:?} {message:?}"
    );
    let expected = image_not_found_message(
        UNSTORED_DIGEST,
        UNSET_IDENTIFIER_MEMBER,
        SHARED_REPOSITORY,
    );
    assert_eq!(
        message, expected,
        "case {case:?}: expected the refusal to echo the requested entry as {expected:?}, got \
         {message:?}"
    );
}
