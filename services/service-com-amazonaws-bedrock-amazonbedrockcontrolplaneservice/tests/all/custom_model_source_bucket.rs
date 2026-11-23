// SPDX-License-Identifier: CC0-1.0

//! The pre-condition `CreateCustomModel`'s `modelSourceConfig` states, asserted end to end
//! with this emulator's own Amazon S3 mounted beside the control plane on one engine — the
//! way the assembled emulator runs them.
//!
//! THE RULE UNDER TEST: "The Amazon S3 URI in the model source must be for the
//! Amazon-managed Amazon S3 bucket containing your model artifacts"
//! (`smithy_query(query="doc",shape="CreateCustomModelRequest",member="modelSourceConfig")`,
//! and the operation's own documentation repeats it). A URI inside the member's published
//! form therefore does not satisfy it on form alone: WHOSE bucket it names is part of the
//! requirement, so a model source naming a bucket the ACCOUNT holds is refused with the
//! modelled `ValidationException` (`@httpError(400)`) carrying `S3 URI not supported`, one
//! naming a bucket the object store holds NOTHING by is refused with the same error
//! carrying the input it could not take (`Unsupported input passed: '<bucket>'.`, the
//! bucket the submitted URI named), and no model is minted either way — while a source
//! naming a bucket AWS itself publishes (one that resolves in S3's namespace and is none of
//! "the buckets in your account") is admitted and the model round-trips through
//! `GetCustomModel`.
//!
//! The rows differ along what the rule generalises over: who holds the bucket (the caller,
//! AWS, nobody) and how the URI addresses it (the bucket alone, a prefix, a nested prefix,
//! one object, a trailing separator). A row also pins that a refusal leaves NOTHING behind
//! — `GetCustomModel` answers 404 and `ListCustomModels(nameContains=…)` reports no model —
//! and that the OTHER way of naming artifacts (`customModelDataSource`, a model package
//! ARN) asks the object store nothing and stays admitted.
//!
//! The same harness carries the OTHER pre-condition this service puts to the object store:
//! the Amazon S3 locations a job-creating request names
//! ([`a_job_location_must_name_a_bucket_the_object_store_holds`]). An advanced prompt
//! optimization job READS its prompt templates from `inputConfig.s3Uri` and WRITES its
//! results under `outputConfig.s3Uri`, so a bucket the store does not hold describes work
//! that can never be carried out and the create is refused with the modelled
//! `ValidationException` (`@httpError(400)`) stating the location it could not reach and the
//! exception the Amazon S3 client raised for it.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice as bedrock;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{error_code, label, Reply};

/// The Region both services are addressed in. Amazon S3 places a bucket created with no
/// `LocationConstraint` in us-east-1, which is also the Region the bucket AWS publishes
/// lives in, so one Region label serves every call here.
const REGION: &str = "us-east-1";

/// A SigV4 credential scope naming Amazon S3, which is what routes a request to the object
/// store. No authenticator is registered, so the signature is never checked; the scope is
/// what claims the request.
const S3_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/s3/aws4_request, \
     SignedHeaders=host, Signature=0000";

/// The bucket the CALLER creates below: its own, however readable, and therefore not the
/// Amazon-managed one the member demands.
const CALLER_BUCKET: &str = "example-custom-model-artifacts";

/// A bucket no `CreateBucket` below ever creates and no publication holds.
const ABSENT_BUCKET: &str = "example-custom-model-nowhere";

/// The Amazon-managed bucket this emulator's Amazon S3 publishes: it resolves in S3's
/// namespace while being none of the account's own buckets
/// (`service_com_amazonaws_s3_amazons3::state::aws_published`), which is what makes it
/// AWS's rather than the caller's.
const AWS_BUCKET: &str =
    service_com_amazonaws_s3_amazons3::state::aws_published::SAGEMAKER_SAMPLE_FILES;

/// One object that bucket publishes, so a row can address a single artifact rather than a
/// prefix. Read off the published catalog rather than spelled here, so the row follows the
/// publication.
fn aws_published_key() -> &'static str {
    service_com_amazonaws_s3_amazons3::state::aws_published::PUBLISHED_BUCKETS
        .iter()
        .find(|bucket| bucket.name == AWS_BUCKET)
        .and_then(|bucket| bucket.objects.first())
        .map(|object| object.key)
        .expect("the published catalog holds an object under that bucket")
}

/// The sentence Amazon Bedrock refuses a bucket that EXISTS and is not the Amazon-managed
/// one with: RECORDED behaviour, with no variable part.
const UNSUPPORTED: &str = "S3 URI not supported";

/// The sentence it refuses a bucket that does not exist with: RECORDED behaviour, carrying
/// the BUCKET the submitted URI named.
fn unsupported_input(bucket: &str) -> String {
    format!("Unsupported input passed: '{bucket}'.")
}

/// A `RoleArn`, which a create naming its artifacts in Amazon S3 must carry.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/BedrockCustomModelImport";

/// `CustomizationType` — the type a model created from artifacts carries.
const IMPORTED: &str = "IMPORTED";

/// One emulator: this service and Amazon S3 on a single shared engine behind the production
/// router — what `build_router` assembles, narrowed to the services under test.
struct Emulator {
    router: MultiServiceRouter,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(bedrock::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    Emulator { router }
}

impl Emulator {
    /// One control-plane call, signed for [`REGION`] the way an SDK signs it.
    async fn bedrock(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(&value).expect("a serializable request body")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://bedrock.{REGION}.amazonaws.com{path}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock/\
                     aws4_request, SignedHeaders=host, Signature=00"
                ),
            )
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let header_error = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(|value| error_code(value).to_string());
        let body: Value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        let error = header_error.or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(|value| error_code(value).to_string())
        });
        Reply {
            status,
            content_type,
            body,
            error,
        }
    }

    /// One path-style request to the emulator's Amazon S3 front door: the bucket lives in
    /// the URI, so this is a raw request rather than a modelled call. The credential scope
    /// names `s3`, which is what the router claims on.
    async fn s3(&self, method: &str, path: &str) -> u16 {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://s3.{REGION}.amazonaws.com{path}"))
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", S3_AUTHORIZATION)
            .body(bytes::Bytes::new())
            .expect("the request builds");
        self.router.dispatch(request).await.status().as_u16()
    }
}

/// `PUT /<bucket>` — `CreateBucket` — so the caller holds a bucket of its own.
async fn given_caller_bucket(emu: &Emulator) {
    let status = emu.s3("PUT", &format!("/{CALLER_BUCKET}")).await;
    assert_eq!(
        status, 200,
        "the caller's bucket must be created first, got {status}"
    );
}

/// `CreateCustomModel` naming its artifacts at `uri`.
async fn create_from(emu: &Emulator, name: &str, uri: &str) -> Reply {
    emu.bedrock(
        "POST",
        "/custom-models/create-custom-model",
        Some(json!({
            "modelName": name,
            "modelSourceConfig": { "s3DataSource": { "s3Uri": uri } },
            "roleArn": ROLE_ARN,
        })),
    )
    .await
}

/// `GetCustomModel` on the model `identifier` names.
async fn read(emu: &Emulator, identifier: &str) -> Reply {
    emu.bedrock("GET", &format!("/custom-models/{}", label(identifier)), None)
        .await
}

/// `ListCustomModels(nameContains=<name>)`, answering the `modelSummaries` it reported.
async fn list_named(emu: &Emulator, name: &str) -> Vec<Value> {
    let reply = emu
        .bedrock("GET", &format!("/custom-models?nameContains={name}"), None)
        .await;
    assert_eq!(
        reply.status, 200,
        "case {name}: the listing must be served, got {} {:?}",
        reply.status, reply.body
    );
    reply
        .get("modelSummaries")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// RULE: a model source naming a bucket that is not the Amazon-managed one is refused with
/// the modelled 400 `ValidationException` — worded for what the object store said about
/// that bucket — and mints nothing; one naming the bucket AWS publishes is admitted and
/// round-trips.
#[tokio::test]
async fn a_model_source_must_name_an_amazon_managed_bucket() {
    let emu = emulator();
    given_caller_bucket(&emu).await;
    let published_key = aws_published_key();
    let published_prefix = published_key
        .rsplit_once('/')
        .map(|(prefix, _)| prefix)
        .expect("the published key sits under a prefix");

    // (case, the model name, the s3Uri, the refusal it draws — `None` when admitted)
    let rows: &[(&str, &str, String, Option<String>)] = &[
        (
            "the caller's own bucket, a prefix under it",
            "caller-prefix",
            format!("s3://{CALLER_BUCKET}/model/"),
            Some(UNSUPPORTED.to_string()),
        ),
        (
            "the caller's own bucket, the bucket itself",
            "caller-bucket",
            format!("s3://{CALLER_BUCKET}"),
            Some(UNSUPPORTED.to_string()),
        ),
        (
            "the caller's own bucket, a nested key",
            "caller-nested",
            format!("s3://{CALLER_BUCKET}/exports/model/weights.safetensors"),
            Some(UNSUPPORTED.to_string()),
        ),
        (
            "a bucket nothing holds",
            "absent-bucket",
            format!("s3://{ABSENT_BUCKET}/model/"),
            Some(unsupported_input(ABSENT_BUCKET)),
        ),
        (
            "another bucket nothing holds, so the sentence follows the request",
            "absent-bucket-two",
            format!("s3://{ABSENT_BUCKET}-again/artifacts/"),
            Some(unsupported_input(&format!("{ABSENT_BUCKET}-again"))),
        ),
        (
            "the Amazon-managed bucket, the bucket itself",
            "aws-bucket",
            format!("s3://{AWS_BUCKET}"),
            None,
        ),
        (
            "the Amazon-managed bucket, a nested prefix",
            "aws-prefix",
            format!("s3://{AWS_BUCKET}/{published_prefix}/"),
            None,
        ),
        (
            "the Amazon-managed bucket, one object",
            "aws-object",
            format!("s3://{AWS_BUCKET}/{published_key}"),
            None,
        ),
    ];

    for (case, name, uri, refusal) in rows {
        let reply = create_from(&emu, name, uri).await;
        let Some(refusal) = refusal else {
            assert_eq!(
                reply.status, 202,
                "case {case:?}: the Amazon-managed bucket satisfies the pre-condition, so \
                 the create must be accepted, got {} {:?}",
                reply.status, reply.body
            );
            // The round trip: what the create minted is what a read reports.
            let arn = reply.text("modelArn");
            let read_back = read(&emu, &arn).await;
            assert_eq!(
                read_back.status, 200,
                "case {case:?}: the minted model must read back, got {} {:?}",
                read_back.status, read_back.body
            );
            assert_eq!(
                read_back.text("modelName"),
                *name,
                "case {case:?}: expected the name the create carried, got {:?}",
                read_back.body
            );
            assert_eq!(
                read_back.get("customizationType"),
                json!(IMPORTED),
                "case {case:?}: expected a customizationType of {IMPORTED}, got {:?}",
                read_back.body
            );
            let listed = list_named(&emu, name).await;
            assert_eq!(
                listed.len(),
                1,
                "case {case:?}: expected the admitted model to be listed once, got {listed:?}"
            );
            continue;
        };

        assert_eq!(
            reply.status, 400,
            "case {case:?}: a bucket that is not the Amazon-managed one must be refused, \
             got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ValidationException"),
            "case {case:?}: expected the modelled ValidationException, got {:?}",
            reply.error
        );
        assert_eq!(
            reply.get("message"),
            json!(refusal),
            "case {case:?}: expected {refusal:?}, got {:?}",
            reply.body
        );
        // And the refusal left nothing behind: the listing reports no such model.
        let listed = list_named(&emu, name).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: a refused create must leave nothing to list, got {listed:?}"
        );
    }
}

/// The name of a bucket an advanced prompt optimization job's locations live in, which the
/// caller creates for itself.
const JOB_BUCKET: &str = "example-apo-locations";

/// A bucket no `CreateBucket` here ever creates, so the object store answers that it holds
/// no such bucket.
const JOB_ABSENT_BUCKET: &str = "example-apo-nowhere";

/// `PUT /<bucket>` — `CreateBucket` — for one named bucket.
async fn given_bucket(emu: &Emulator, name: &str) {
    let status = emu.s3("PUT", &format!("/{name}")).await;
    assert_eq!(
        status, 200,
        "the bucket {name} must be created first, got {status}"
    );
}

/// `CreateAdvancedPromptOptimizationJob` reading from `input` and writing under `output`,
/// carrying `client_token` when one is given (`@idempotencyToken`).
async fn create_optimization_job_with_token(
    emu: &Emulator,
    name: &str,
    input: &str,
    output: &str,
    client_token: Option<&str>,
) -> Reply {
    let mut body = json!({
        "jobName": name,
        "inputConfig": { "s3Uri": input },
        "outputConfig": { "s3Uri": output },
        "modelConfigurations": [{ "modelId": "amazon.nova-lite-v1:0" }],
    });
    if let Some(token) = client_token {
        body["clientToken"] = json!(token);
    }
    emu.bedrock("POST", "/advanced-prompt-optimization-jobs", Some(body))
        .await
}

/// `CreateAdvancedPromptOptimizationJob` reading from `input` and writing under `output`.
async fn create_optimization_job(
    emu: &Emulator,
    name: &str,
    input: &str,
    output: &str,
) -> Reply {
    create_optimization_job_with_token(emu, name, input, output, None).await
}

/// `GetAdvancedPromptOptimizationJob` on the job `identifier` names.
async fn read_optimization_job(emu: &Emulator, identifier: &str) -> Reply {
    emu.bedrock(
        "GET",
        &format!("/advanced-prompt-optimization-jobs/{}", label(identifier)),
        None,
    )
    .await
}

/// The FIXED text of the sentence Amazon Bedrock words for an Amazon S3 location it could
/// not reach, around the location the request named: everything after it is the exception
/// the Amazon S3 client raised, whose request ids and attempt count belong to that one call.
fn unreachable_location_head(location: &str) -> String {
    format!(
        "Cannot access S3 location: {location}. Error: The specified bucket does not exist \
         (Service: S3, Status Code: 404, Request ID: "
    )
}

/// RULE: a job pointed at a bucket the object store does not hold can never read its input or
/// save its results, so the create is refused with the modelled 400 `ValidationException`
/// stating the location it could not reach — and mints nothing; a job whose locations the
/// store DOES hold is accepted and round-trips through a read.
///
/// The rows differ along what the rule generalises over: WHICH configuration names the absent
/// bucket (the input, the output, both), the FORM of the location (a prefix, a nested prefix,
/// an object key, the bucket alone) and the bucket NAME, so the sentence follows the request
/// rather than one recorded run.
#[tokio::test]
async fn a_job_location_must_name_a_bucket_the_object_store_holds() {
    let emu = emulator();
    given_bucket(&emu, JOB_BUCKET).await;
    let held_input = format!("s3://{JOB_BUCKET}/apo-input/templates.jsonl");
    let held_output = format!("s3://{JOB_BUCKET}/apo-output/");
    let other_absent = format!("{JOB_ABSENT_BUCKET}-two");

    // (case, jobName, inputConfig.s3Uri, outputConfig.s3Uri, the location the refusal names
    //  — `None` when the create is admitted)
    let rows: &[(&str, &str, String, String, Option<String>)] = &[
        (
            "both locations in a bucket the caller holds",
            "apo-held",
            held_input.clone(),
            held_output.clone(),
            None,
        ),
        (
            "the bucket itself as the input location",
            "apo-held-bucket",
            format!("s3://{JOB_BUCKET}"),
            held_output.clone(),
            None,
        ),
        (
            "the input in a bucket nothing holds, named by a prefix",
            "apo-absent-input",
            format!("s3://{JOB_ABSENT_BUCKET}/apo-input/"),
            held_output.clone(),
            Some(format!("{JOB_ABSENT_BUCKET}/apo-input/")),
        ),
        (
            "the input in a bucket nothing holds, named by a nested prefix",
            "apo-absent-input-nested",
            format!("s3://{JOB_ABSENT_BUCKET}/runs/2026/apo-input/"),
            held_output.clone(),
            Some(format!("{JOB_ABSENT_BUCKET}/runs/2026/apo-input/")),
        ),
        (
            "the input in a bucket nothing holds, named by the bucket alone",
            "apo-absent-input-bucket",
            format!("s3://{JOB_ABSENT_BUCKET}"),
            held_output.clone(),
            Some(format!("{JOB_ABSENT_BUCKET}/")),
        ),
        (
            "the OUTPUT in a bucket nothing holds, so the refusal follows that one",
            "apo-absent-output",
            held_input.clone(),
            format!("s3://{other_absent}/apo-output/"),
            Some(format!("{other_absent}/apo-output/")),
        ),
        (
            "both locations in buckets nothing holds, so the input is refused first",
            "apo-absent-both",
            format!("s3://{JOB_ABSENT_BUCKET}/apo-input/"),
            format!("s3://{other_absent}/apo-output/"),
            Some(format!("{JOB_ABSENT_BUCKET}/apo-input/")),
        ),
    ];

    for (case, name, input, output, refused_location) in rows {
        let reply = create_optimization_job(&emu, name, input, output).await;
        let Some(location) = refused_location else {
            assert_eq!(
                reply.status, 200,
                "case {case:?}: a location the object store holds satisfies the \
                 pre-condition, so the create must be accepted, got {} {:?}",
                reply.status, reply.body
            );
            // The round trip: what the create minted is what a read reports.
            let arn = reply.text("jobArn");
            let read_back = read_optimization_job(&emu, &arn).await;
            assert_eq!(
                read_back.status, 200,
                "case {case:?}: the minted job must read back, got {} {:?}",
                read_back.status, read_back.body
            );
            assert_eq!(
                read_back.text("jobName"),
                *name,
                "case {case:?}: expected the name the create carried, got {:?}",
                read_back.body
            );
            continue;
        };

        assert_eq!(
            reply.status, 400,
            "case {case:?}: a location in a bucket nothing holds must be refused, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ValidationException"),
            "case {case:?}: expected the modelled ValidationException, got {:?}",
            reply.error
        );
        let message = reply.text("message");
        let head = unreachable_location_head(location);
        assert!(
            message.starts_with(&head),
            "case {case:?}: expected the message to open {head:?}, got {message:?}"
        );
        // The ids and the attempt count belong to the call the object store answered, so the
        // shape is the requirement and the values are that one call's.
        let tail = ") (SDK Attempt Count: 1)";
        assert!(
            message.ends_with(tail),
            "case {case:?}: expected the message to end {tail:?}, got {message:?}"
        );
        let ids = message
            .strip_prefix(&head)
            .and_then(|rest| rest.strip_suffix(tail))
            .unwrap_or_default();
        let (request_id, extended) = ids
            .split_once(", Extended Request ID: ")
            .unwrap_or_else(|| panic!("case {case:?}: expected both request ids, got {ids:?}"));
        assert!(
            !request_id.is_empty() && !extended.is_empty(),
            "case {case:?}: both request ids must name the call that answered, got {ids:?}"
        );
        // And the refusal handed back nothing to address: no job was minted.
        assert_eq!(
            reply.get("jobArn"),
            Value::Null,
            "case {case:?}: a refused create mints no job, got {:?}",
            reply.body
        );
    }
}

/// RULE, from the other side: a refused create writes NOTHING — not even the
/// `@idempotencyToken` it carried — so the same token replayed on a request whose locations
/// the object store does hold mints a job rather than answering with one the refusal left
/// behind.
#[tokio::test]
async fn a_refused_job_create_records_nothing_for_its_client_token() {
    let emu = emulator();
    given_bucket(&emu, JOB_BUCKET).await;
    let token = "apo-refused-then-accepted";

    let refused = create_optimization_job_with_token(
        &emu,
        "apo-token-refused",
        &format!("s3://{JOB_ABSENT_BUCKET}/apo-input/"),
        &format!("s3://{JOB_BUCKET}/apo-output/"),
        Some(token),
    )
    .await;
    assert_eq!(
        (refused.status, refused.error.as_deref()),
        (400, Some("ValidationException")),
        "the create naming an absent bucket must be refused, got {} {:?}",
        refused.status,
        refused.body
    );

    let accepted = create_optimization_job_with_token(
        &emu,
        "apo-token-accepted",
        &format!("s3://{JOB_BUCKET}/apo-input/templates.jsonl"),
        &format!("s3://{JOB_BUCKET}/apo-output/"),
        Some(token),
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "the create naming held locations must be accepted, got {} {:?}",
        accepted.status, accepted.body
    );
    let read_back = read_optimization_job(&emu, &accepted.text("jobArn")).await;
    assert_eq!(
        read_back.text("jobName"),
        "apo-token-accepted",
        "the replayed token must mint the accepted job rather than answer with the refused \
         one, got {:?}",
        read_back.body
    );
}

/// RULE: the pre-condition is the S3 model source's. Naming the artifacts the OTHER way —
/// `customModelDataSource`, a SageMaker AI model package ARN — asks the object store
/// nothing and is admitted, so the refusal above is about the bucket rather than about
/// creating a custom model at all.
#[tokio::test]
async fn a_model_package_source_asks_no_bucket_question() {
    let emu = emulator();
    given_caller_bucket(&emu).await;

    // (case, the model name, the model package ARN)
    let rows: &[(&str, &str, &str)] = &[
        (
            "a model package of this account",
            "package-source",
            "arn:aws:sagemaker:us-east-1:000000000000:model-package/nova-export-1",
        ),
        (
            "a model package under a versioned group",
            "package-versioned",
            "arn:aws:sagemaker:us-east-1:000000000000:model-package/nova-export-group/2",
        ),
    ];
    for (case, name, package_arn) in rows {
        let reply = emu
            .bedrock(
                "POST",
                "/custom-models/create-custom-model",
                Some(json!({
                    "modelName": name,
                    "customModelDataSource": {
                        "modelPackageArnDataSource": { "modelPackageArn": package_arn }
                    },
                })),
            )
            .await;
        assert_eq!(
            reply.status, 202,
            "case {case:?}: a model package source carries no bucket to judge, so the \
             create must be accepted, got {} {:?}",
            reply.status, reply.body
        );
        let read_back = read(&emu, &reply.text("modelArn")).await;
        assert_eq!(
            read_back.status, 200,
            "case {case:?}: the minted model must read back, got {} {:?}",
            read_back.status, read_back.body
        );
    }
}
