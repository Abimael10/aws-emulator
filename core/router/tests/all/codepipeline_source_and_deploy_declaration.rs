// SPDX-License-Identifier: CC0-1.0

//! What CodePipeline does with a pipeline STRUCTURE a client submits, driven end to end over
//! the assembled router with IAM, Amazon S3 and CodePipeline mounted on one engine.
//!
//! The rules under test, each asserted over a SET of declarations rather than one:
//!
//! * **a `PipelineDeclaration` that satisfies the structural laws CodePipeline states — one
//!   artifact store (`artifactStore` or `artifactStores`, never both), at least two stages, a
//!   first stage carrying only `Source` actions and no later stage carrying one, at least one
//!   non-source action, every consumed artifact produced by an earlier action, no repeated
//!   stage or action name, each action naming an action type the service publishes and
//!   declaring no more artifacts than that type's published maximum, and a `roleArn` naming a
//!   role this account's IAM holds whose trust policy admits `codepipeline.amazonaws.com` — is
//!   STORED and answered HTTP 200 with the stored structure, which a later `GetPipeline`
//!   reports unchanged.** The answer carries the members the SERVICE resolves rather than the
//!   caller (`version` 1, `pipelineType`, `executionMode`, each action's `runOrder` and its
//!   empty `inputArtifacts` / `outputArtifacts` / `configuration`), so an edit-then-update
//!   cycle over what was read back submits the service's own values.
//! * **a pipeline NAME is in use once**: with the structure stored, a second `CreatePipeline`
//!   under that name is refused `PipelineNameInUseException` (HTTP 400) whatever it declares —
//!   including the identical structure, which a retried create carries — and the stored
//!   pipeline is left exactly as it was.
//! * **an action is held to its action type's published artifact maximum**: a declaration at
//!   the bound is accepted and one past it is refused `InvalidActionDeclarationException`
//!   (HTTP 400), with nothing stored. The bound is read off `ListActionTypes`, where the
//!   service publishes its own catalog, rather than written here.
//! * **tags applied by the create are stored with the pipeline**: the create reports them back
//!   and `ListTagsForResource`, addressed by the ARN the pipeline's own metadata reports,
//!   reports the same set.
//! * **every timestamp this service puts on the wire carries at most millisecond
//!   resolution**: the epoch-seconds numbers `ListPipelines` reports for a summary's
//!   `created` / `updated` and `GetPipeline` for its metadata carry at most three fractional
//!   digits, whatever precision the clock behind them has.
//!
//! Two crates have to agree for the first rule to hold — CodePipeline stores the structure and
//! IAM holds the role its pre-store gate resolves (a role IAM reports absent is refused
//! `InvalidStructureException`) — so the suite lives here rather than in the CodePipeline
//! crate, where no IAM answers that question. The source repository and the invoked function
//! are NOT created: `CreatePipeline` stores a structure and the EXECUTION resolves what each
//! action names, so what the stages point at is not part of what a create decides.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::codepipeline_action_execution_provenance::assert_millisecond_timestamps;
use crate::query_wire::{element, form_encode};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// CodePipeline's `X-Amz-Target` prefix, from that service's own model (service shape
/// `CodePipeline_20150709`).
const TARGET_PREFIX: &str = "CodePipeline_20150709";

/// The Region every call of this suite is signed for. One Region keeps `CreateBucket` free of
/// a `LocationConstraint`, which is not what is under test here.
const REGION: &str = "us-east-1";

/// The trust policy a pipeline's service role carries: the document that admits the service
/// principal CodePipeline assumes a pipeline's role as.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
"Principal":{"Service":"codepipeline.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The bucket every declaration of this suite names as its artifact store.
const ARTIFACT_STORE: &str = "cp-declaration-artifacts";

/// The canonical emulator account id (`emulator_server::EMULATOR_ACCOUNT_ID`), which is the
/// account every refusal naming one is built from.
const ACCOUNT_ID: &str = "000000000000";

/// The `pipelineType` a declaration that states none resolves to, and the `executionMode` a
/// declaration that states none resolves to — the values `GetPipeline` reports for a pipeline
/// created without them.
const RESOLVED_PIPELINE_TYPE: &str = "V2";
/// See [`RESOLVED_PIPELINE_TYPE`].
const RESOLVED_EXECUTION_MODE: &str = "SUPERSEDED";

/// The published maximum number of INPUT artifacts an `Invoke`/`AWS`/`Lambda`/`1` action takes,
/// as the SERVICE publishes it rather than as a figure written here.
///
/// `ActionType$inputArtifactDetails` is an `ArtifactDetails`, whose `maximumCount` is "the
/// maximum number of artifacts allowed for the action type", and `ListActionTypes` is where the
/// service publishes its own catalog — so the bound a declaration is judged against is read off
/// that listing and cannot drift from it.
fn published_input_maximum(emulator: &Emulator, id: &Value) -> usize {
    let case = "the published artifact bound";
    // The catalog is bounded, so the walk over it is too: a listing that kept handing back a
    // token would otherwise spin here instead of failing.
    const MAX_PAGES: usize = 64;
    let mut token: Option<String> = None;
    for page_number in 1..=MAX_PAGES {
        let mut request = json!({ "actionOwnerFilter": "AWS" });
        if let Some(next) = &token {
            request["nextToken"] = json!(next);
        }
        let page = emulator.cp_ok(case, "ListActionTypes", &request);
        let types = page["actionTypes"].as_array().cloned().unwrap_or_default();
        if let Some(published) = types.iter().find(|published| {
            ["category", "owner", "provider", "version"]
                .iter()
                .all(|member| published["id"][member] == id[member])
        }) {
            let maximum = published["inputArtifactDetails"]["maximumCount"]
                .as_u64()
                .unwrap_or_else(|| {
                    panic!("the published action type states an input maximum, got {published}")
                });
            return usize::try_from(maximum).expect("a published count fits a usize");
        }
        token = page["nextToken"].as_str().map(str::to_string);
        assert!(
            token.is_some(),
            "the service must publish the action type {id}, got {page}"
        );
        assert!(
            page_number < MAX_PAGES,
            "the catalog listing must end within {MAX_PAGES} pages; it had not reached the \
             action type {id} by then"
        );
    }
    unreachable!("the loop above either answers the bound or fails naming what it read")
}

/// The emulator this suite drives: the whole router over one engine, and the runtime its
/// requests are blocked on.
struct Emulator {
    router: MultiServiceRouter,
    runtime: tokio::runtime::Runtime,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let mut router = build_router(engine);
        router.set_request_authentication(false);
        Self {
            router,
            // A MULTI-thread runtime: creating a pipeline RUNS it once, and that run's work
            // is performed through in-process calls into other services of this same router.
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("a multi-thread runtime"),
        }
    }

    /// Drive one request and answer its status and body.
    fn send(
        &self,
        signing_name: &str,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> (u16, Vec<u8>) {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                     aws4_request, SignedHeaders=host, Signature=deadbeef"
                ),
            );
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.runtime.block_on(self.router.dispatch(request));
        let status = response.status().as_u16();
        (status, response.into_body().to_vec())
    }

    /// One IAM action (awsQuery, API version 2010-05-08).
    fn iam(&self, action: &str, members: &[(&str, &str)]) -> (u16, String) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (key, value) in members {
            body.push('&');
            body.push_str(&form_encode(key));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let (status, payload) = self.send(
            "iam",
            "POST",
            "/",
            &[("content-type", "application/x-www-form-urlencoded")],
            body.into_bytes(),
        );
        (status, String::from_utf8_lossy(&payload).into_owned())
    }

    /// One awsJson1_1 CodePipeline operation.
    fn cp(&self, operation: &str, body: &Value) -> (u16, Value) {
        let (status, payload) = self.send(
            "codepipeline",
            "POST",
            "/",
            &[
                ("content-type", "application/x-amz-json-1.1"),
                ("x-amz-target", &format!("{TARGET_PREFIX}.{operation}")),
            ],
            serde_json::to_vec(body).expect("the request body serializes"),
        );
        let payload = String::from_utf8_lossy(&payload).into_owned();
        let parsed = if payload.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&payload).unwrap_or_else(|_| json!({ "unparsed": payload }))
        };
        (status, parsed)
    }

    /// One CodePipeline operation that has to succeed.
    fn cp_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, answer) = self.cp(operation, body);
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must answer HTTP 200, got {status} with {answer}"
        );
        answer
    }

    /// One CodePipeline operation that has to be refused: its status, error code and message.
    fn cp_refusal(&self, case: &str, operation: &str, body: &Value) -> (u16, String, String) {
        let (status, answer) = self.cp(operation, body);
        let code = answer["__type"]
            .as_str()
            .unwrap_or_default()
            .rsplit(['#', '/'])
            .next()
            .unwrap_or_default()
            .to_string();
        assert!(
            !code.is_empty(),
            "case {case:?}: {operation} must answer a modelled error, got {status} with {answer}"
        );
        (
            status,
            code,
            answer["message"].as_str().unwrap_or_default().to_string(),
        )
    }

    /// Create the bucket a declaration names as its artifact store.
    fn bucket(&self, name: &str) {
        let (status, payload) = self.send("s3", "PUT", &format!("/{name}"), &[], Vec::new());
        assert_eq!(
            status,
            200,
            "CreateBucket {name} must answer HTTP 200, got {status} with {:?}",
            String::from_utf8_lossy(&payload)
        );
    }

    /// Create a role whose trust policy is `trust`, and answer its ARN.
    fn role(&self, name: &str, trust: &str) -> String {
        let (status, body) = self.iam(
            "CreateRole",
            &[("RoleName", name), ("AssumeRolePolicyDocument", trust)],
        );
        assert_eq!(
            status, 200,
            "CreateRole for {name} must answer HTTP 200, got {status} with {body}"
        );
        element(&body, "Arn")
    }
}

/// A `Source` action reading `branch` of the CodeCommit repository `repository` and producing
/// `output`.
fn code_commit_source(name: &str, repository: &str, branch: &str, output: &str) -> Value {
    json!({
        "name": name,
        "actionTypeId": {
            "category": "Source", "owner": "AWS", "provider": "CodeCommit", "version": "1",
        },
        "configuration": {
            "RepositoryName": repository,
            "BranchName": branch,
            "PollForSourceChanges": "false",
        },
        "inputArtifacts": [],
        "outputArtifacts": [{ "name": output }],
        "runOrder": 1,
    })
}

/// A `Source` action reading one object of an Amazon S3 bucket and producing `output`.
fn s3_source(name: &str, bucket: &str, key: &str, output: &str) -> Value {
    json!({
        "name": name,
        "actionTypeId": {
            "category": "Source", "owner": "AWS", "provider": "S3", "version": "1",
        },
        "configuration": { "S3Bucket": bucket, "S3ObjectKey": key },
        "outputArtifacts": [{ "name": output }],
    })
}

/// An `Invoke` action that hands `inputs` to an AWS Lambda function.
fn lambda_invoke(name: &str, function: &str, inputs: &[&str]) -> Value {
    json!({
        "name": name,
        "actionTypeId": {
            "category": "Invoke", "owner": "AWS", "provider": "Lambda", "version": "1",
        },
        "configuration": {
            "FunctionName": function,
            "UserParameters": "{\"stage\":\"deploy\"}",
        },
        "inputArtifacts": inputs
            .iter()
            .map(|input| json!({ "name": input }))
            .collect::<Vec<_>>(),
        "outputArtifacts": [],
        "runOrder": 1,
    })
}

/// A `Deploy` action that delivers one input artifact to an Amazon S3 bucket.
fn s3_deploy(name: &str, bucket: &str, input: &str) -> Value {
    json!({
        "name": name,
        "actionTypeId": {
            "category": "Deploy", "owner": "AWS", "provider": "S3", "version": "1",
        },
        "configuration": { "BucketName": bucket, "ObjectKey": "release.zip", "Extract": "false" },
        "inputArtifacts": [{ "name": input }],
    })
}

/// A manual-approval action: the non-source action that takes and produces no artifact at all.
fn manual_approval(name: &str) -> Value {
    json!({
        "name": name,
        "actionTypeId": {
            "category": "Approval", "owner": "AWS", "provider": "Manual", "version": "1",
        },
    })
}

/// One stage over `actions`.
fn stage(name: &str, actions: Vec<Value>) -> Value {
    json!({ "name": name, "actions": actions })
}

/// A declaration named `name`, with `ARTIFACT_STORE` as its artifact store and `role_arn` as
/// its service role. `resolved` carries the members a row states for the service to keep.
fn declaration(name: &str, role_arn: &str, stages: Vec<Value>, resolved: Value) -> Value {
    let mut declared = json!({
        "name": name,
        "roleArn": role_arn,
        "artifactStore": { "type": "S3", "location": ARTIFACT_STORE },
        "stages": stages,
    });
    for (member, value) in resolved.as_object().cloned().unwrap_or_default() {
        declared[member] = value;
    }
    declared
}

/// The artifact names one action's `member` list carries, as a response reports them.
fn artifact_names(action: &Value, member: &str) -> Vec<String> {
    action[member]
        .as_array()
        .map(|artifacts| {
            artifacts
                .iter()
                .map(|artifact| artifact["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Assert that `reported` is the structure `declared` describes, with every member the SERVICE
/// resolves present on it.
fn assert_is_the_declared_structure(case: &str, declared: &Value, reported: &Value) {
    for member in ["name", "roleArn", "artifactStore"] {
        assert_eq!(
            reported[member], declared[member],
            "case {case:?}: the reported {member} must be the declared one, got {reported}"
        );
    }
    assert_eq!(
        reported["version"],
        json!(1),
        "case {case:?}: a created pipeline is at version 1, got {reported}"
    );
    for (member, resolved) in [
        ("pipelineType", RESOLVED_PIPELINE_TYPE),
        ("executionMode", RESOLVED_EXECUTION_MODE),
    ] {
        let expected = declared[member]
            .as_str()
            .map_or_else(|| resolved.to_string(), str::to_string);
        assert_eq!(
            reported[member].as_str(),
            Some(expected.as_str()),
            "case {case:?}: {member} must be the declared value or the resolved default, got \
             {reported}"
        );
    }

    let declared_stages = declared["stages"].as_array().cloned().unwrap_or_default();
    let reported_stages = reported["stages"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        reported_stages.len(),
        declared_stages.len(),
        "case {case:?}: every declared stage must be reported, got {reported}"
    );
    for (declared_stage, reported_stage) in declared_stages.iter().zip(&reported_stages) {
        assert_eq!(
            reported_stage["name"], declared_stage["name"],
            "case {case:?}: the stages must be reported in the declared order, got {reported}"
        );
        let declared_actions = declared_stage["actions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let reported_actions = reported_stage["actions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            reported_actions.len(),
            declared_actions.len(),
            "case {case:?}: every declared action must be reported, got {reported_stage}"
        );
        for (declared_action, reported_action) in declared_actions.iter().zip(&reported_actions) {
            assert_eq!(
                reported_action["name"], declared_action["name"],
                "case {case:?}: an action is reported under its declared name, got \
                 {reported_action}"
            );
            for member in ["category", "owner", "provider", "version"] {
                assert_eq!(
                    reported_action["actionTypeId"][member],
                    declared_action["actionTypeId"][member],
                    "case {case:?}: actionTypeId.{member} must round-trip, got {reported_action}"
                );
            }
            // The members the SERVICE resolves for an action that declares none: an empty
            // list, an empty map and the default run order, so an edit-then-update cycle
            // over what was read back carries them.
            let declared_configuration = declared_action
                .get("configuration")
                .cloned()
                .unwrap_or_else(|| json!({}));
            assert_eq!(
                reported_action["configuration"], declared_configuration,
                "case {case:?}: the action configuration must round-trip, got {reported_action}"
            );
            for member in ["inputArtifacts", "outputArtifacts"] {
                assert_eq!(
                    artifact_names(reported_action, member),
                    artifact_names(declared_action, member),
                    "case {case:?}: {member} must be reported as declared (empty where the \
                     action declares none), got {reported_action}"
                );
            }
            let expected_run_order = declared_action
                .get("runOrder")
                .cloned()
                .unwrap_or_else(|| json!(1));
            assert_eq!(
                reported_action["runOrder"], expected_run_order,
                "case {case:?}: runOrder must be the declared one or the default, got \
                 {reported_action}"
            );
        }
    }
}

/// The declarations the first rule is asserted over: a table whose rows differ in the
/// providers in play, the number of stages, the number of actions and artifacts, which
/// service-resolved members the caller states, and whether the create applies tags.
///
/// `input_maximum` is the input-artifact bound the service publishes for the invoke action
/// ([`published_input_maximum`]), which the last row declares exactly as many artifacts as.
fn accepted_declarations(role_arn: &str, input_maximum: usize) -> Vec<(&'static str, Value)> {
    let mut rows = vec![
        (
            "a codecommit source feeding a lambda invoke, every resolved member stated",
            declaration(
                "cp-decl-codecommit-lambda",
                role_arn,
                vec![
                    stage(
                        "Source",
                        vec![code_commit_source("Source", "cp-decl-repo", "main", "src")],
                    ),
                    stage(
                        "Deploy",
                        vec![lambda_invoke("Deploy", "cp-decl-function", &["src"])],
                    ),
                ],
                json!({ "pipelineType": "V2", "executionMode": "SUPERSEDED" }),
            ),
        ),
        (
            "an s3 source feeding an s3 deploy, no resolved member stated",
            declaration(
                "cp-decl-s3-s3",
                role_arn,
                vec![
                    stage(
                        "Source",
                        vec![s3_source("Src", ARTIFACT_STORE, "source.zip", "SourceArtifact")],
                    ),
                    stage(
                        "Deploy",
                        vec![s3_deploy("Dep", ARTIFACT_STORE, "SourceArtifact")],
                    ),
                ],
                json!({}),
            ),
        ),
        (
            "three stages, with an approval action that takes no artifact between them",
            declaration(
                "cp-decl-three-stages",
                role_arn,
                vec![
                    stage(
                        "Source",
                        vec![code_commit_source("Source", "cp-decl-repo", "release", "src")],
                    ),
                    stage("Approve", vec![manual_approval("Gate")]),
                    stage(
                        "Deploy",
                        vec![lambda_invoke("Deploy", "cp-decl-function", &["src"])],
                    ),
                ],
                json!({ "executionMode": "QUEUED" }),
            ),
        ),
    ];

    // The bound itself: an action at its action type's published input maximum, fed by one
    // source action per artifact.
    let artifacts: Vec<String> = (0..input_maximum)
        .map(|index| format!("src{index}"))
        .collect();
    let sources: Vec<Value> = artifacts
        .iter()
        .enumerate()
        .map(|(index, artifact)| {
            code_commit_source(&format!("Src{index}"), "cp-decl-repo", "main", artifact)
        })
        .collect();
    let inputs: Vec<&str> = artifacts.iter().map(String::as_str).collect();
    rows.push((
        "an invoke action at its action type's published input-artifact maximum",
        declaration(
            "cp-decl-at-the-bound",
            role_arn,
            vec![
                stage("Source", sources),
                stage("Deploy", vec![lambda_invoke("Deploy", "cp-decl-function", &inputs)]),
            ],
            json!({}),
        ),
    ));
    rows
}

/// RULE: a declaration satisfying CodePipeline's structural laws is STORED and answered 200
/// with the stored structure, which `GetPipeline` then reports unchanged.
fn an_accepted_declaration_round_trips(emulator: &Emulator, role_arn: &str, input_maximum: usize) {
    for (case, declared) in accepted_declarations(role_arn, input_maximum) {
        let name = declared["name"].as_str().expect("a named declaration");
        let created = emulator.cp_ok(case, "CreatePipeline", &json!({ "pipeline": declared }));
        let reported = &created["pipeline"];
        assert!(
            reported.is_object(),
            "case {case:?}: the create must answer with the stored structure, got {created}"
        );
        assert_is_the_declared_structure(case, &declared, reported);
        assert_eq!(
            created["tags"],
            Value::Null,
            "case {case:?}: a create that applied no tag reports none, got {created}"
        );

        let fetched = emulator.cp_ok(case, "GetPipeline", &json!({ "name": name }));
        assert_eq!(
            &fetched["pipeline"], reported,
            "case {case:?}: GetPipeline must report the structure the create stored"
        );
        assert_is_the_declared_structure(case, &declared, &fetched["pipeline"]);
    }
}

/// RULE (the same create, with tags applied): `CreatePipeline$tags` are stored with the
/// pipeline, so the create reports them back and `ListTagsForResource` on the pipeline's own
/// ARN reports the same set.
fn tags_applied_by_the_create_are_stored_with_the_pipeline(emulator: &Emulator, role_arn: &str) {
    // The rows differ in how many tags the create applies, and in the alphabet of the keys
    // and values (`TagKey` is 1..128, `TagValue` 0..256, so an empty VALUE is a tag too).
    for (index, (case, tags)) in [
        ("one tag", json!([{ "key": "owner", "value": "release-team" }])),
        (
            "two tags, one with an empty value",
            json!([
                { "key": "cost.centre", "value": "cc/42" },
                { "key": "retain", "value": "" },
            ]),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("cp-decl-tagged-{index}");
        let declared = declaration(
            &name,
            role_arn,
            vec![
                stage(
                    "Source",
                    vec![code_commit_source("Source", "cp-decl-repo", "main", "src")],
                ),
                stage(
                    "Deploy",
                    vec![lambda_invoke("Deploy", "cp-decl-function", &["src"])],
                ),
            ],
            json!({}),
        );
        let created = emulator.cp_ok(
            case,
            "CreatePipeline",
            &json!({ "pipeline": declared, "tags": tags }),
        );
        let expected: Vec<Value> = tags.as_array().cloned().unwrap_or_default();
        assert_eq!(
            created["tags"].as_array().cloned().unwrap_or_default(),
            expected,
            "case {case:?}: the create reports the tags it applied, got {created}"
        );

        // The pipeline's OWN ARN, as the service reports it, is what the tag listing is
        // addressed by — never an ARN written here.
        let fetched = emulator.cp_ok(case, "GetPipeline", &json!({ "name": name }));
        let arn = fetched["metadata"]["pipelineArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the metadata reports the ARN, got {fetched}"))
            .to_string();
        let listed = emulator.cp_ok(case, "ListTagsForResource", &json!({ "resourceArn": arn }));
        assert_eq!(
            listed["tags"].as_array().cloned().unwrap_or_default(),
            expected,
            "case {case:?}: the stored tags are the ones the create applied, got {listed}"
        );
    }
}

/// RULE: a pipeline name is in use once — with the structure stored, a second create under
/// that name is refused, whatever it declares, and the stored pipeline is untouched.
fn a_stored_name_is_in_use(emulator: &Emulator, role_arn: &str) {
    let stored = declaration(
        "cp-decl-name-in-use",
        role_arn,
        vec![
            stage(
                "Source",
                vec![code_commit_source("Source", "cp-decl-repo", "main", "src")],
            ),
            stage(
                "Deploy",
                vec![lambda_invoke("Deploy", "cp-decl-function", &["src"])],
            ),
        ],
        json!({}),
    );
    let created = emulator.cp_ok(
        "the first create of the name",
        "CreatePipeline",
        &json!({ "pipeline": stored }),
    );

    // The rows differ in how far the second declaration is from the stored one: the SAME
    // structure (what a retried create carries) and a different one.
    let mut different = stored.clone();
    different["stages"] = json!([
        stage(
            "Source",
            vec![s3_source("Src", ARTIFACT_STORE, "source.zip", "other")]
        ),
        stage("Approve", vec![manual_approval("Gate")]),
    ]);
    for (case, second) in [
        ("the identical structure", stored.clone()),
        ("a different structure", different),
    ] {
        let (status, code, message) =
            emulator.cp_refusal(case, "CreatePipeline", &json!({ "pipeline": second }));
        assert_eq!(
            (status, code.as_str()),
            (400, "PipelineNameInUseException"),
            "case {case:?}: a second create under a stored name is refused, got {status} {code} \
             {message:?}"
        );
        assert_eq!(
            message,
            format!(
                "A pipeline with the name 'cp-decl-name-in-use' already exists in account \
                 '{ACCOUNT_ID}'"
            ),
            "case {case:?}: the refusal states the requested name and the caller's account"
        );
        let fetched =
            emulator.cp_ok(case, "GetPipeline", &json!({ "name": "cp-decl-name-in-use" }));
        assert_eq!(
            fetched["pipeline"], created["pipeline"],
            "case {case:?}: the refused create must leave the stored structure as it was"
        );
        assert_eq!(
            fetched["pipeline"]["version"],
            json!(1),
            "case {case:?}: the refused create must not advance the version, got {fetched}"
        );
    }
}

/// RULE: an action declares no more artifacts than its action type publishes a maximum for —
/// the bound is accepted (a row of [`accepted_declarations`]) and one past it is refused, with
/// nothing stored.
fn an_action_past_its_published_bound_is_refused(
    emulator: &Emulator,
    role_arn: &str,
    input_maximum: usize,
) {
    let case = "an invoke action one input artifact past its published maximum";
    let artifacts: Vec<String> = (0..=input_maximum)
        .map(|index| format!("over{index}"))
        .collect();
    let sources: Vec<Value> = artifacts
        .iter()
        .enumerate()
        .map(|(index, artifact)| {
            code_commit_source(&format!("Src{index}"), "cp-decl-repo", "main", artifact)
        })
        .collect();
    let inputs: Vec<&str> = artifacts.iter().map(String::as_str).collect();
    let name = "cp-decl-past-the-bound";
    let declared = declaration(
        name,
        role_arn,
        vec![
            stage("Source", sources),
            stage("Deploy", vec![lambda_invoke("Deploy", "cp-decl-function", &inputs)]),
        ],
        json!({}),
    );

    let (status, code, message) =
        emulator.cp_refusal(case, "CreatePipeline", &json!({ "pipeline": declared }));
    assert_eq!(
        (status, code.as_str()),
        (400, "InvalidActionDeclarationException"),
        "case {case:?}: declaring more artifacts than the action type takes is refused, got \
         {status} {code} {message:?}"
    );
    assert_eq!(
        message,
        format!(
            "Action Deploy declares {} input artifacts which is more than the maximum count \
             ({input_maximum})",
            inputs.len()
        ),
        "case {case:?}: the refusal states the action, the count of which artifacts it declared \
         and the published maximum that count is over"
    );
    let (status, answer) = emulator.cp("GetPipeline", &json!({ "name": name }));
    assert_eq!(
        status, 400,
        "case {case:?}: a refused create stores nothing, got {status} with {answer}"
    );
}

/// RULE: every timestamp this service puts on the wire carries at most millisecond resolution.
///
/// Read through the suite that owns this claim for the execution plane
/// (`codepipeline_action_execution_provenance`): ONE walk over a response body asserts every
/// member the model types as a `Timestamp`, wherever in the body it sits, and answers how many
/// it found — so a response that carries none cannot pass it vacuously.
fn reported_timestamps_carry_at_most_millisecond_resolution(emulator: &Emulator, role_arn: &str) {
    // The rows differ in how many pipelines the listing summarises, so the claim is about the
    // projection and not about one summary.
    let mut created = 0usize;
    for (case, names) in [
        ("one summarised pipeline", vec!["cp-decl-clock-a"]),
        (
            "two summarised pipelines",
            vec!["cp-decl-clock-a", "cp-decl-clock-b"],
        ),
    ] {
        // Each row adds the pipelines it needs beyond the ones the row before it created, so
        // every create here is a first create of its name.
        for name in names.iter().skip(created) {
            emulator.cp_ok(
                case,
                "CreatePipeline",
                &json!({
                    "pipeline": declaration(
                        name,
                        role_arn,
                        vec![
                            stage(
                                "Source",
                                vec![code_commit_source("Source", "cp-decl-repo", "main", "src")],
                            ),
                            stage(
                                "Deploy",
                                vec![lambda_invoke("Deploy", "cp-decl-function", &["src"])],
                            ),
                        ],
                        json!({}),
                    )
                }),
            );
        }
        created = names.len();

        let listed = emulator.cp_ok(case, "ListPipelines", &json!({}));
        let summaries = listed["pipelines"].as_array().cloned().unwrap_or_default();
        let found = assert_millisecond_timestamps(case, "ListPipelines", &listed);
        assert_eq!(
            found,
            2 * summaries.len(),
            "case {case:?}: every summary carries a created and an updated instant, got {listed}"
        );
        for name in &names {
            let summary = summaries
                .iter()
                .find(|summary| summary["name"].as_str() == Some(*name))
                .unwrap_or_else(|| panic!("case {case:?}: {name} must be listed, got {listed}"));
            for member in ["created", "updated"] {
                assert!(
                    summary[member].is_number(),
                    "case {case:?}: {name}'s {member} must be an epoch-seconds number, got \
                     {summary}"
                );
            }
        }

        // The SAME encoder answers the read of one pipeline's metadata.
        for name in &names {
            let fetched = emulator.cp_ok(case, "GetPipeline", &json!({ "name": name }));
            let found = assert_millisecond_timestamps(case, "GetPipeline", &fetched);
            assert_eq!(
                found, 2,
                "case {case:?}: {name}'s metadata carries a created and an updated instant, got \
                 {fetched}"
            );
            for member in ["created", "updated"] {
                assert!(
                    fetched["metadata"][member].is_number(),
                    "case {case:?}: {name}'s metadata.{member} must be an epoch-seconds number, \
                     got {fetched}"
                );
            }
        }
    }
}

/// RULE (the pre-store gate the accepted rows pass): a declaration naming a role this
/// account's IAM holds nothing under, or one whose trust policy does not admit CodePipeline,
/// is refused `InvalidStructureException` and stores nothing.
fn a_role_codepipeline_cannot_assume_is_refused(emulator: &Emulator) {
    let unrelated = emulator.role(
        "cp-decl-unrelated-role",
        r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
"Principal":{"Service":"glue.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#,
    );
    for (case, role_arn) in [
        (
            "a role this account's IAM holds nothing under",
            format!("arn:aws:iam::{ACCOUNT_ID}:role/cp-decl-no-such-role"),
        ),
        (
            "a role whose trust policy admits another service",
            unrelated,
        ),
    ] {
        let name = "cp-decl-role-gate";
        let declared = declaration(
            name,
            &role_arn,
            vec![
                stage(
                    "Source",
                    vec![code_commit_source("Source", "cp-decl-repo", "main", "src")],
                ),
                stage(
                    "Deploy",
                    vec![lambda_invoke("Deploy", "cp-decl-function", &["src"])],
                ),
            ],
            json!({}),
        );
        let (status, code, message) =
            emulator.cp_refusal(case, "CreatePipeline", &json!({ "pipeline": declared }));
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidStructureException"),
            "case {case:?}: a role CodePipeline cannot assume is refused, got {status} {code} \
             {message:?}"
        );
        assert_eq!(
            message,
            format!("CodePipeline is not authorized to perform AssumeRole on role {role_arn}"),
            "case {case:?}: the refusal names the role the declaration carried"
        );
        let (status, answer) = emulator.cp("GetPipeline", &json!({ "name": name }));
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create stores nothing, got {status} with {answer}"
        );
    }
}

#[test]
fn codepipeline_stores_the_declaration_it_accepts_and_reports_it_back() {
    std::thread::Builder::new()
        .name("codepipeline-source-and-deploy-declaration".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let emulator = Emulator::new();
            emulator.bucket(ARTIFACT_STORE);
            let role = emulator.role("cp-decl-service-role", TRUST_POLICY);
            // The bound the service publishes for the invoke action, read off its own
            // catalog, so the rows at the bound and past it are judged by the figure the
            // service states rather than by one written here.
            let input_maximum = published_input_maximum(
                &emulator,
                &json!({
                    "category": "Invoke", "owner": "AWS", "provider": "Lambda", "version": "1",
                }),
            );
            an_accepted_declaration_round_trips(&emulator, &role, input_maximum);
            tags_applied_by_the_create_are_stored_with_the_pipeline(&emulator, &role);
            a_stored_name_is_in_use(&emulator, &role);
            an_action_past_its_published_bound_is_refused(&emulator, &role, input_maximum);
            a_role_codepipeline_cannot_assume_is_refused(&emulator);
            reported_timestamps_carry_at_most_millisecond_resolution(&emulator, &role);
        })
        .expect("the suite's thread starts")
        .join()
        .expect("the suite's thread runs to completion");
}
