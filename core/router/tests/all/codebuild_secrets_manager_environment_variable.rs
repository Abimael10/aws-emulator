// SPDX-License-Identifier: CC0-1.0

//! What a build's `SECRETS_MANAGER` environment variable is worth INSIDE the build, and what
//! a build whose service role may not read it comes to.
//!
//! RULE: `EnvironmentVariable$type` `SECRETS_MANAGER` is "an environment variable stored in
//! Secrets Manager. For environment variables of this type, specify the name of the secret as
//! the `value` of the EnvironmentVariable. The secret value will be substituted for the name at
//! runtime" — so the shell of the build sees the secret's PLAINTEXT under that name, with the
//! reference's `<secret-id>:<json-key>` selecting a key of a JSON secret, and
//! `Build$exportedEnvironmentVariables` carries the value that shell assigned to each name the
//! buildspec's `env/exported-variables` listed. The substitution is a read made AS the
//! project's service role (`CreateProject$serviceRole` is "the ARN of the IAM role that enables
//! CodeBuild to interact with dependent Amazon Web Services services on behalf of the Amazon
//! Web Services account"), and `secretsmanager:GetSecretValue` is what that read requires
//! ("Required permissions: secretsmanager:GetSecretValue", `GetSecretValue`), so a build whose
//! role no document in force entitles to the secret is a build whose environment cannot be
//! provisioned: it ends `FAILED`, carries a phase context naming that withheld action, has no
//! `BUILD` phase and exports nothing. The reference itself is all CodeBuild stores: no response
//! of that service carries the plaintext.
//!
//! Three crates have to agree for that to hold — IAM holds the role and the inline document a
//! `PutRolePolicy` rewrites under it, Secrets Manager holds the secret and takes the
//! authorization decision, and CodeBuild provisions the environment and runs the build — so the
//! suite lives here: a crate-local suite can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: whether the reference selects the whole
//! secret or a JSON key of it, whether it names the secret by name or by ARN, and the FORM of
//! the value substituted (a plain string, a JSON string field, a JSON field that is not a
//! string, and text whose byte length differs from its character count).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the secrets, the role and the
/// builds belong to.
const REGION: &str = "us-west-2";

/// The IAM action the substitution's read performs, which a grant names and a refusal blames
/// ("Actions defined by AWS Secrets Manager").
const READ_ACTION: &str = "secretsmanager:GetSecretValue";

/// The name of the one inline policy each role below carries, which every rewrite replaces.
const POLICY_NAME: &str = "inline";

/// How often to look while waiting for the transition the clock owes a started build, and how
/// many looks to take. The span is CodeBuild's own, so no fixed sleep names the instant it ends,
/// and the deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(25);
const LOOKS: usize = 2_400;

/// The emulator as it is ASSEMBLED, on one engine: the read CodeBuild makes for a build's
/// environment is dispatched back into this same router, so the secret it reads is the secret
/// this suite created.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One awsJson1_1 call, answered with the HTTP status and the parsed body — the wire both
/// Secrets Manager and CodeBuild serve, each under its own target prefix.
async fn json_rpc(
    router: &MultiServiceRouter,
    service: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("authorization", authorization(service))
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Secrets Manager operation over its own wire.
async fn secretsmanager(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    json_rpc(router, "secretsmanager", "secretsmanager", operation, body).await
}

/// One CodeBuild operation over its own wire (service shape `CodeBuild_20161006`).
async fn codebuild(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    json_rpc(router, "codebuild", "CodeBuild_20161006", operation, body).await
}

/// How many characters the token below carries: inside the published 32..64 bound of
/// `ClientRequestTokenType` (`smithy_query(query="shape", shape="ClientRequestTokenType")`).
const VERSION_TOKEN_LEN: usize = 40;

/// The `ClientRequestToken` one secret is created with — "this parameter specifies a unique
/// identifier for the new version" (`CreateSecretRequest$ClientRequestToken`), which an SDK
/// would generate for a caller that writes none.
fn version_token(name: &str) -> String {
    let mut token = format!("{name}-version-");
    token.truncate(VERSION_TOKEN_LEN);
    while token.len() < VERSION_TOKEN_LEN {
        token.push('0');
    }
    token
}

/// Create one secret holding `value` and answer the ARN Secrets Manager minted for it.
async fn create_secret(router: &MultiServiceRouter, name: &str, value: &str) -> String {
    let (status, created) = secretsmanager(
        router,
        "CreateSecret",
        &json!({
            "Name": name,
            "SecretString": value,
            "ClientRequestToken": version_token(name),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateSecret({name:?}) must succeed, got {status} with {created}"
    );
    created["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateSecret({name:?}) must answer an ARN, got {created}"))
        .to_string()
}

/// The trust policy a CodeBuild service role needs: CodeBuild assumes it for the build.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "codebuild.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The statement every service role here carries whatever else it is granted: the log actions a
/// build writes its own log stream with, which grant nothing of Secrets Manager.
fn log_statement() -> Value {
    json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })
}

/// A document granting the read on `resources` beside those log actions.
fn document_allowing(resources: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            log_statement(),
            {"Effect": "Allow", "Action": READ_ACTION, "Resource": resources},
        ],
    })
    .to_string()
}

/// Create the role a project runs as, carrying `document` as its one inline policy, and answer
/// the ARN IAM minted for it.
async fn create_role(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust_policy())],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    let arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole({name:?}) must answer an ARN, got {created}"));
    put_role_policy(router, name, document).await;
    arn
}

/// Put `document` in force as the one inline policy of `role`.
async fn put_role_policy(router: &MultiServiceRouter, role: &str, document: &str) {
    let (status, answered) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({role:?}) must succeed, got {status} with {answered}"
    );
}

/// The name the variable every project below declares is read under inside the build.
const VARIABLE_NAME: &str = "BUILD_SECRET";

/// The buildspec every project below runs: it computes a digest and a byte count over what the
/// shell holds for [`VARIABLE_NAME`] and exports both, which is the only way a value that must
/// not appear in any response can be asserted on.
const BUILDSPEC: &str = "version: 0.2\nenv:\n  exported-variables:\n    - SECRET_DIGEST\n    \
                         - SECRET_LENGTH\nphases:\n  build:\n    commands:\n      \
                         - export SECRET_DIGEST=\"$(printf '%s' \"$BUILD_SECRET\" | sha256sum \
                         | cut -c1-64)\"\n      \
                         - export SECRET_LENGTH=\"$(printf '%s' \"$BUILD_SECRET\" | wc -c)\"\n";

/// Create a project whose environment declares the one `SECRETS_MANAGER` variable `reference`
/// names, running as `service_role`.
async fn create_project(
    router: &MultiServiceRouter,
    name: &str,
    service_role: &str,
    reference: &str,
) {
    let (status, created) = codebuild(
        router,
        "CreateProject",
        &json!({
            "name": name,
            "source": {"type": "NO_SOURCE", "buildspec": BUILDSPEC},
            "artifacts": {"type": "NO_ARTIFACTS"},
            "cache": {"type": "NO_CACHE"},
            "environment": {
                "type": "LINUX_CONTAINER",
                "image": "aws/codebuild/standard:7.0",
                "computeType": "BUILD_GENERAL1_SMALL",
                "environmentVariables": [{
                    "name": VARIABLE_NAME,
                    "value": reference,
                    "type": "SECRETS_MANAGER",
                }],
            },
            "serviceRole": service_role,
            "timeoutInMinutes": 5,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateProject({name:?}) must succeed, got {status} with {created}"
    );
}

/// Start a build of `project`, look again until it reports itself complete, and answer the
/// record `BatchGetBuilds` reports.
async fn settled_build(router: &MultiServiceRouter, case: &str, project: &str) -> Value {
    let (status, started) = codebuild(router, "StartBuild", &json!({"projectName": project})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartBuild({project:?}) must succeed, got {status} with {started}"
    );
    let id = started["build"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartBuild must answer a build id, got {started}"))
        .to_string();
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (status, read) = codebuild(router, "BatchGetBuilds", &json!({"ids": [id.clone()]})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: BatchGetBuilds({id:?}) must succeed, got {status} with {read}"
        );
        last = read["builds"][0].clone();
        if last["buildComplete"] == json!(true) {
            return last;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the build must complete, last read {last}")
}

/// The variables a build reports as exported, by name.
fn exported(build: &Value) -> std::collections::BTreeMap<String, String> {
    build["exportedEnvironmentVariables"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|variable| {
            (
                variable["name"].as_str().unwrap_or_default().to_string(),
                variable["value"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The `(phaseType, phaseStatus)` pairs a build reports, in the order it reached them.
fn phase_pairs(build: &Value) -> Vec<(String, String)> {
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

/// Every phase-context message a build reports, in order.
fn context_messages(build: &Value) -> Vec<String> {
    build["phases"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .flat_map(|phase| phase["contexts"].as_array().cloned().unwrap_or_default())
        .map(|context| context["message"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The digest the build's own commands compute over `plaintext`: the SHA-256 of those bytes,
/// lowercase hexadecimal, which is what `sha256sum | cut -c1-64` writes.
fn digest_of(plaintext: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(plaintext.as_bytes());
    hex::encode(hasher.finalize())
}

/// How one row's variable references its secret.
enum Reference {
    /// The whole secret, named by its name.
    WholeByName,
    /// One JSON key of it, named by the secret's name.
    KeyByName(&'static str),
    /// One JSON key of it, named by the secret's ARN.
    KeyByArn(&'static str),
}

/// One row: the value the secret holds, how the variable references it, and the plaintext the
/// build's shell must see.
struct Row {
    case: &'static str,
    stored: &'static str,
    reference: Reference,
    expected: &'static str,
}

/// RULE: the build's shell sees the secret's plaintext under the variable's name, and the
/// exported variables carry what that shell computed over it; CodeBuild still stores only the
/// typed reference, and no response of it carries the plaintext.
#[tokio::test]
async fn a_secrets_manager_variable_is_substituted_inside_the_build() {
    let router = build_router();
    let rows = [
        Row {
            case: "the whole secret, a plain string",
            stored: "a-plain-build-secret",
            reference: Reference::WholeByName,
            expected: "a-plain-build-secret",
        },
        Row {
            case: "a JSON key of the secret, named by the secret's name",
            stored: r#"{"token":"s3cr3t-token-value","other":"not-this-one"}"#,
            reference: Reference::KeyByName("token"),
            expected: "s3cr3t-token-value",
        },
        Row {
            case: "a JSON key of the secret, named by the secret's ARN",
            stored: r#"{"token":"arn-addressed-token","other":"not-this-one"}"#,
            reference: Reference::KeyByArn("token"),
            expected: "arn-addressed-token",
        },
        // The number this row stores is WIDE for a reason the assertions below turn on: the
        // last of them reads the whole answer as text and requires the plaintext to be absent
        // from it, and an answer carries machine-generated numbers of its own (a build's
        // `startTime` and `endTime` are epoch seconds). A plaintext narrow enough to appear
        // INSIDE one of those would make this case depend on what the clock reads rather than
        // on what the build substituted, so the value is wider than any number the answer can
        // carry while still being what the row is about: a JSON field that is not a string.
        Row {
            case: "a JSON field that is not a string",
            stored: r#"{"revision":867530913572468}"#,
            reference: Reference::KeyByName("revision"),
            expected: "867530913572468",
        },
        Row {
            case: "a value whose byte length differs from its character count",
            stored: "clé-secrète-£",
            reference: Reference::WholeByName,
            expected: "clé-secrète-£",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let secret_name = format!("codebuild-substituted-secret-{index}");
        let secret_arn = create_secret(&router, &secret_name, row.stored).await;
        let role = format!("codebuild-substituted-role-{index}");
        let role_arn = create_role(&router, &role, &document_allowing(&[&secret_arn])).await;
        let reference = match row.reference {
            Reference::WholeByName => secret_name.clone(),
            Reference::KeyByName(key) => format!("{secret_name}:{key}"),
            Reference::KeyByArn(key) => format!("{secret_arn}:{key}"),
        };
        let project = format!("codebuild-substituted-project-{index}");
        create_project(&router, &project, &role_arn, &reference).await;

        let build = settled_build(&router, case, &project).await;
        assert_eq!(
            build["buildStatus"],
            json!("SUCCEEDED"),
            "case {case:?}: the build must succeed, got {build}"
        );
        let reported = exported(&build);
        assert_eq!(
            reported.get("SECRET_DIGEST").map(String::as_str),
            Some(digest_of(row.expected).as_str()),
            "case {case:?}: expected the digest of the substituted plaintext, got {reported:?}"
        );
        assert_eq!(
            reported.get("SECRET_LENGTH").map(String::as_str),
            Some(row.expected.len().to_string().as_str()),
            "case {case:?}: expected the byte length of the substituted plaintext, got \
             {reported:?}"
        );

        // The reference is all CodeBuild stores, and no answer of it carries the plaintext.
        let (status, projects) =
            codebuild(&router, "BatchGetProjects", &json!({"names": [project.clone()]})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: BatchGetProjects must succeed, got {status} with {projects}"
        );
        let stored_variable = projects["projects"][0]["environment"]["environmentVariables"][0]
            .clone();
        assert_eq!(
            stored_variable,
            json!({"name": VARIABLE_NAME, "value": reference, "type": "SECRETS_MANAGER"}),
            "case {case:?}: the project keeps the typed reference, got {stored_variable}"
        );
        for (what, answered) in [("the build", &build), ("the project", &projects)] {
            assert!(
                !answered.to_string().contains(row.expected),
                "case {case:?}: {what} must carry no plaintext of the secret, got {answered}"
            );
        }
    }
}

/// RULE: the substitution is a read made as the project's service role, so a build whose role no
/// document in force entitles to the secret runs nothing — it ends `FAILED`, names the withheld
/// action, reports no `BUILD` phase and exports nothing — and the same project built again once
/// the grant is in force succeeds with the value substituted.
#[tokio::test]
async fn a_build_whose_role_may_not_read_the_secret_runs_nothing() {
    let router = build_router();
    let plaintext = "the-barriered-token";
    let secret_arn = create_secret(
        &router,
        "codebuild-barrier-secret",
        &json!({"token": plaintext}).to_string(),
    )
    .await;
    // A second secret of the account, which the role below is granted and the project does not
    // reference: a grant on another secret is no grant on this one.
    let decoy_arn = create_secret(&router, "codebuild-barrier-decoy", "the-decoys-own-value").await;

    let role = "codebuild-barrier-role";
    let role_arn = create_role(&router, role, &document_allowing(&[&decoy_arn])).await;
    let project = "codebuild-barrier-project";
    create_project(
        &router,
        project,
        &role_arn,
        "codebuild-barrier-secret:token",
    )
    .await;

    let case = "the role may read only the decoy";
    let refused = settled_build(&router, case, project).await;
    assert_eq!(
        refused["buildStatus"],
        json!("FAILED"),
        "case {case:?}: a build whose secret cannot be read must fail, got {refused}"
    );
    let messages = context_messages(&refused);
    assert!(
        messages.iter().any(|message| message.contains(READ_ACTION)),
        "case {case:?}: a phase context must name the withheld action, got {messages:?}"
    );
    let pairs = phase_pairs(&refused);
    assert!(
        pairs
            .iter()
            .any(|(phase, status)| phase == "PROVISIONING" && status == "FAILED"),
        "case {case:?}: the phase that provisions the environment carries the failure, got \
         {pairs:?}"
    );
    assert!(
        pairs.iter().all(|(phase, _)| phase != "BUILD"),
        "case {case:?}: no command phase ran, so none may be reported, got {pairs:?}"
    );
    assert!(
        !exported(&refused).contains_key("SECRET_DIGEST"),
        "case {case:?}: a build that ran nothing exports nothing, got {refused}"
    );
    assert!(
        !refused.to_string().contains(plaintext),
        "case {case:?}: the answer must carry no plaintext of the secret, got {refused}"
    );

    // The only thing that changes is the document IAM holds for the role.
    put_role_policy(&router, role, &document_allowing(&[&secret_arn, &decoy_arn])).await;
    let case = "the grant on the referenced secret is put in force";
    let served = settled_build(&router, case, project).await;
    assert_eq!(
        served["buildStatus"],
        json!("SUCCEEDED"),
        "case {case:?}: the identical project must now build, got {served}"
    );
    let reported = exported(&served);
    assert_eq!(
        reported.get("SECRET_DIGEST").map(String::as_str),
        Some(digest_of(plaintext).as_str()),
        "case {case:?}: expected the digest of the substituted plaintext, got {reported:?}"
    );
}

/// RULE: there is nothing to substitute for a reference this account holds no secret for, or for
/// one whose secret holds no such key, so that build's environment cannot be provisioned either.
#[tokio::test]
async fn a_reference_with_nothing_to_substitute_fails_the_build() {
    let router = build_router();
    // The secret the second row references by name: it is there, and the key that row names is
    // not in the document it holds.
    create_secret(
        &router,
        "codebuild-unresolved-secret",
        &json!({"token": "a-value-under-another-key"}).to_string(),
    )
    .await;
    let role_arn = create_role(
        &router,
        "codebuild-unresolved-role",
        // Every secret of the account, so what refuses these builds is never the grant.
        &document_allowing(&["*"]),
    )
    .await;
    // (case, the reference the project declares)
    let rows = [
        (
            "a secret this account holds none of",
            "codebuild-no-such-secret",
        ),
        (
            "a JSON key the secret does not state",
            "codebuild-unresolved-secret:absent",
        ),
    ];
    for (index, (case, reference)) in rows.iter().enumerate() {
        let project = format!("codebuild-unresolved-project-{index}");
        create_project(&router, &project, &role_arn, reference).await;
        let build = settled_build(&router, case, &project).await;
        assert_eq!(
            build["buildStatus"],
            json!("FAILED"),
            "case {case:?}: a reference with nothing to substitute fails the build, got {build}"
        );
        let pairs = phase_pairs(&build);
        assert!(
            pairs
                .iter()
                .any(|(phase, status)| phase == "PROVISIONING" && status == "FAILED"),
            "case {case:?}: the phase that provisions the environment carries the failure, got \
             {pairs:?}"
        );
        assert!(
            pairs.iter().all(|(phase, _)| phase != "BUILD"),
            "case {case:?}: no command phase ran, so none may be reported, got {pairs:?}"
        );
    }
}
