// SPDX-License-Identifier: CC0-1.0

//! Amazon Bedrock resolves the model identifier a batch-job create names BEFORE it decides
//! the `iam:PassRole` that create asks for, driven over the real wire path: the Bedrock
//! control plane and IAM mounted on one engine, so the control plane's in-process callback
//! reaches this very IAM and the pass-role decision is the grounded one.
//!
//! RULE: a value submitted to a member that IDENTIFIES the model a job is to run on is part
//! of what the request ASKS FOR, so a create resolves it against the families the member's
//! own pattern admits before it decides whether the caller may pass the role the job would
//! run as: an identifier that resolves to no model is the input fault it is — 400
//! `ValidationException` carrying the one fixed sentence Amazon Bedrock states for that
//! class, or 404 `ResourceNotFoundException` with the named resource's own not-found
//! sentence where the spelling is an ARN of a family the create resolves — whatever the
//! pass-role decision would have been, and the pass-role denial stays reserved for a request
//! whose model identifier does resolve. A create whose own resource identity embeds the
//! resolved model is the exception the corpus records (`CreateModelCustomizationJob`, whose
//! `ModelCustomizationJobArn` spells the base model before the job id): there the pass is
//! decided first and only the identifier's FORM is read in front of it, which
//! `all/bedrock_model_customization_pass_role.rs` pins.
//!
//! The rows span what the rule generalises over:
//!
//! * the OPERATION carrying the member: `CreateModelInvocationJob` (`modelId`) and
//!   `CreateEvaluationJob` (`inferenceConfig`), whose job ARNs are minted out of the Region
//!   and a fresh id alone;
//! * the MEMBER inside the evaluation's configuration: the `bedrockModel` of a `models`
//!   configuration and the `modelArn` a `ragConfigs` configuration generates responses with;
//! * the SPELLING the value takes — a bare model id, a geography-prefixed one, and every ARN
//!   resource type the member's published pattern admits (`smithy_query(query="shape",
//!   shape="ModelId,EvaluationBedrockModelIdentifier")`), including one of another service's
//!   namespace — hence which refusal the spelling draws;
//! * WHY the value resolves to no model the job could run on: a model the catalogue never
//!   carried, and one it recognises and OFFERS NO LONGER — which is offered under nothing, so
//!   a job submitted now has no model to run on;
//! * and the ROLE the create passes, over both halves of the pass-role denial: a role this
//!   account's IAM holds and trusts Amazon Bedrock with (the pass is admitted), one it holds
//!   that trusts another service only (IAM ANSWERED, so the pass is denied), and one of
//!   another account (denied with no IAM answer needed at all).
//!
//! Every expectation comes from the model
//! (`smithy_query(query="operation",operation="CreateEvaluationJob,CreateModelInvocationJob,GetEvaluationJob,GetModelInvocationJob,ListModelInvocationJobs")`:
//! the success statuses, `ValidationException` at `@httpError(400)`,
//! `ResourceNotFoundException` at 404 and `AccessDeniedException` at 403) or from the
//! sentences this service states for each class.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder an awsQuery form body needs is the one
// `all/cloudfront_key_value_store_handshake.rs` already owns: a form component is escaped
// exactly as a URI path segment is, so the codec is shared rather than written again.
use crate::cloudfront_key_value_store_handshake::segment as form_encode;

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// The sentence Amazon Bedrock states a denied pass-role with, spelled here so this suite
/// pins the wording a client sees rather than reading it off the service.
const PASS_ROLE_REFUSAL: &str = "Cross-account pass role is not allowed.";

/// The one sentence Amazon Bedrock states for a value it cannot read as the model an
/// operation addresses, spelled here for the same reason.
const INVALID_MODEL_IDENTIFIER: &str = "The provided model identifier is invalid.";

/// The not-found sentence of a `custom-model`.
const CUSTOM_MODEL_NOT_FOUND: &str = "Custom model not found.";

/// The not-found sentence of the other resource types these rows name: the resource type as
/// the noun phrase that opens the sentence.
const IMPORTED_MODEL_NOT_FOUND: &str = "Imported model not found";
const INFERENCE_PROFILE_NOT_FOUND: &str = "Inference profile not found";
const APPLICATION_INFERENCE_PROFILE_NOT_FOUND: &str = "Application inference profile not found";

/// The service principal a job role's trust policy has to grant the assume to: this
/// service's own `awsService$cloudTrailEventSource` (`smithy_query(query="service_info")`),
/// spelled as a JSON string so it can sit in a policy document.
const SERVICE_PRINCIPAL: &str = "\"bedrock.amazonaws.com\"";

/// `ModelInvocationJobStatus` values a submitted batch job may be read back in before it has
/// run (`smithy_query(query="shape",shape="ModelInvocationJobStatus")`).
const INVOCATION_NON_TERMINAL: &[&str] = &["Submitted", "Validating", "Scheduled", "InProgress"];

/// `EvaluationJobStatus` — the status an accepted evaluation job is read back in.
const EVALUATION_IN_PROGRESS: &str = "InProgress";

/// The Bedrock control plane and IAM over ONE shared engine, as `router/src/lib.rs` mounts
/// them, so the control plane's in-process callback reaches this very IAM.
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
        service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// One awsQuery call to IAM: `POST /` with the parameters in the form body.
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
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One restJson1 call to the Bedrock control plane, answered with the status, the wire error
/// code and the JSON body.
async fn bedrock(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, String, Value) {
    let payload = body
        .map(|value| Bytes::from(serde_json::to_vec(&value).expect("the test body serializes")))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("bedrock.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock/\
                 aws4_request, SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header_error = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(bare_code);
    let bytes = response.into_body();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    let error = header_error.unwrap_or_else(|| {
        parsed
            .get("__type")
            .and_then(Value::as_str)
            .map(bare_code)
            .unwrap_or_default()
    });
    (status, error, parsed)
}

/// The bare error code out of an `__type` / `x-amzn-errortype` value, which may carry a
/// namespace or a URL beside it.
fn bare_code(raw: &str) -> String {
    let without_url = raw.split(':').next().unwrap_or(raw);
    without_url
        .rsplit('#')
        .next()
        .unwrap_or(without_url)
        .to_string()
}

/// The text of `member`, or the empty string when the body carries none.
fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A URI label: a value carried in a path segment, percent-encoded the way an SDK encodes
/// one.
fn label(raw: &str) -> String {
    raw.replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
}

/// The ARN of a role of this account, under the default path.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// A trust policy granting `service` the `sts:AssumeRole` a pass of this role would make.
fn trust_policy(service: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":{service}}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// The role every admitted row passes: one this account's IAM holds, whose trust policy
/// grants Amazon Bedrock the assume.
const TRUSTED_ROLE: &str = "bedrock-batch-role";

/// A role this account's IAM holds whose trust policy grants the assume to ANOTHER service,
/// so IAM's answer about it withholds the pass.
const UNTRUSTED_ROLE: &str = "lambda-only-role";

/// Create the roles the rows name.
async fn create_roles(router: &MultiServiceRouter) {
    for (name, service) in [
        (TRUSTED_ROLE, SERVICE_PRINCIPAL),
        (UNTRUSTED_ROLE, "\"lambda.amazonaws.com\""),
    ] {
        let policy = trust_policy(service);
        let (status, body) = iam(
            router,
            "CreateRole",
            &[
                ("RoleName", name),
                ("AssumeRolePolicyDocument", policy.as_str()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: CreateRole {name} must succeed, got {status} {body}"
        );
    }
}

/// One role a create passes, and whether the `iam:PassRole` it makes is admitted.
struct RolePass {
    case: &'static str,
    arn: String,
    admitted: bool,
}

fn role_passes() -> Vec<RolePass> {
    vec![
        RolePass {
            case: "a role this account holds that trusts Amazon Bedrock",
            arn: role_arn(TRUSTED_ROLE),
            admitted: true,
        },
        RolePass {
            case: "a role this account holds that trusts another service only",
            arn: role_arn(UNTRUSTED_ROLE),
            admitted: false,
        },
        RolePass {
            // `iam:PassRole` is granted by the account that HOLDS the role, so another
            // account's role is one no principal of this account can pass — a denial that
            // needs no answer from IAM at all.
            case: "a role of another account",
            arn: format!("arn:aws:iam::123456789012:role/{TRUSTED_ROLE}"),
            admitted: false,
        },
    ]
}

/// One unresolvable model identifier, the create that carries it, and the refusal the rule
/// demands for the spelling it is.
struct Case {
    case: &'static str,
    /// The create: its path, and the body it carries the identifier in, built for a
    /// `jobName` stem so two rows never collide.
    create: Create,
    identifier: String,
    status: u16,
    code: &'static str,
    message: String,
}

/// Which create a row drives, and how a job it mints is read back.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Create {
    /// `POST /model-invocation-job`, carrying the identifier in `modelId`.
    BatchInference,
    /// `POST /evaluation-jobs`, carrying it in `inferenceConfig.models[].bedrockModel`.
    EvaluationModels,
    /// The same create, carrying it in the `modelArn` a `ragConfigs` configuration
    /// generates responses with.
    EvaluationRag,
}

impl Create {
    fn path(self) -> &'static str {
        match self {
            Self::BatchInference => "/model-invocation-job",
            Self::EvaluationModels | Self::EvaluationRag => "/evaluation-jobs",
        }
    }

    /// The success status the model declares for the create.
    fn success(self) -> u16 {
        match self {
            Self::BatchInference => 200,
            Self::EvaluationModels | Self::EvaluationRag => 202,
        }
    }

    /// A request body carrying the members the model marks required, naming `identifier` as
    /// the model and `role_arn` as the role the job would run as.
    fn body(self, name: &str, identifier: &str, role_arn: &str) -> Value {
        match self {
            Self::BatchInference => json!({
                "jobName": name,
                "roleArn": role_arn,
                "modelId": identifier,
                "inputDataConfig": {
                    "s3InputDataConfig": {"s3Uri": "s3://batch-in/prompts.jsonl"},
                },
                "outputDataConfig": {"s3OutputDataConfig": {"s3Uri": "s3://batch-out/"}},
            }),
            Self::EvaluationModels => evaluation_body(
                name,
                role_arn,
                json!({"models": [{"bedrockModel": {"modelIdentifier": identifier}}]}),
            ),
            Self::EvaluationRag => evaluation_body(
                name,
                role_arn,
                json!({
                    "ragConfigs": [{
                        "knowledgeBaseConfig": {
                            "retrieveAndGenerateConfig": {
                                "type": "KNOWLEDGE_BASE",
                                "knowledgeBaseConfiguration": {
                                    "knowledgeBaseId": "KB12345678",
                                    "modelArn": identifier,
                                },
                            },
                        },
                    }],
                }),
            ),
        }
    }
}

/// A `CreateEvaluationJob` body carrying the four `@required` members, with `inference_config`
/// as the row spells it.
fn evaluation_body(name: &str, role_arn: &str, inference_config: Value) -> Value {
    json!({
        "jobName": name,
        "roleArn": role_arn,
        "outputDataConfig": {"s3Uri": "s3://evaluation-out/"},
        "evaluationConfig": {"automated": {"datasetMetricConfigs": [{
            "taskType": "Summarization",
            "dataset": {"name": "Builtin.Gigaword"},
            "metricNames": ["Builtin.Accuracy"],
        }]}},
        "inferenceConfig": inference_config,
    })
}

/// A `foundation-model` ARN of this Region naming a model the catalogue carries no row for.
/// The resource is `foundation-model/<modelId>` behind an EMPTY account field, as
/// `FoundationModelArn` spells it.
fn missing_foundation_model_arn() -> String {
    format!("arn:aws:bedrock:{REGION}::foundation-model/acme.not-a-model-v1")
}

/// The sentence this service states for a `foundation-model` ARN that names no model: it
/// reports the resource it could not resolve, which IS the ARN the request addressed.
fn foundation_model_not_found(identifier: &str) -> String {
    format!("Could not resolve the model from the provided model identifier: {identifier}")
}

/// The sentence a Provisioned Throughput this account does not hold is reported absent with:
/// the identifier the request provided is its one variable part.
fn provisioned_model_not_found(identifier: &str) -> String {
    format!("Provisioned model with the provided name {identifier} was not found.")
}

/// The sentence a prompt router nothing holds is reported absent with.
fn prompt_router_not_found(arn: &str) -> String {
    format!("Cannot find prompt router {arn}.")
}

/// Two base model ids this service RECOGNISES and offers NO LONGER, written out from the
/// published retired set (`state::RETIRED_FOUNDATION_MODELS`, which records the ids AWS
/// publishes as past their end of life) and held to it by
/// [`the_catalogue_offers_none_of_the_retired_models_these_rows_name`] — neither of them the
/// id the recorded case carried, because the rule is about the CLASS and not about one row.
const RETIRED_MODELS: &[&str] = &["anthropic.claude-v2", "meta.llama2-70b-chat-v1"];

/// The `foundation-model` ARN of `model_id` in this Region, behind the EMPTY account field
/// `FoundationModelArn` spells.
fn foundation_model_arn(model_id: &str) -> String {
    format!("arn:aws:bedrock:{REGION}::foundation-model/{model_id}")
}

fn cases() -> Vec<Case> {
    let missing_foundation_model = missing_foundation_model_arn();
    let missing_provisioned_model =
        format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:provisioned-model/aaaaaaaaaaaa");
    let missing_prompt_router =
        format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:default-prompt-router/acme.router");
    vec![
        // ---- a batch inference submission: the families `ModelId` admits ---------------
        Case {
            case: "CreateModelInvocationJob by a bare id no catalogue row carries",
            create: Create::BatchInference,
            identifier: "acme.not-a-model-v1".to_string(),
            status: 400,
            code: "ValidationException",
            message: INVALID_MODEL_IDENTIFIER.to_string(),
        },
        Case {
            case: "CreateModelInvocationJob by a foundation-model ARN no row carries",
            create: Create::BatchInference,
            identifier: missing_foundation_model.clone(),
            status: 404,
            code: "ResourceNotFoundException",
            message: foundation_model_not_found(&missing_foundation_model),
        },
        Case {
            case: "CreateModelInvocationJob by a custom-model ARN nothing minted",
            create: Create::BatchInference,
            identifier: format!(
                "arn:aws:bedrock:{REGION}:{ACCOUNT}:custom-model/acme.not-a-model-v1/aaaaaaaaaaaa"
            ),
            status: 404,
            code: "ResourceNotFoundException",
            message: CUSTOM_MODEL_NOT_FOUND.to_string(),
        },
        Case {
            case: "CreateModelInvocationJob by an application-inference-profile ARN nothing \
                   minted",
            create: Create::BatchInference,
            identifier: format!(
                "arn:aws:bedrock:{REGION}:{ACCOUNT}:application-inference-profile/aaaaaaaaaaaa"
            ),
            status: 404,
            code: "ResourceNotFoundException",
            message: APPLICATION_INFERENCE_PROFILE_NOT_FOUND.to_string(),
        },
        Case {
            // A model the service recognises and offers NO LONGER is offered under nothing,
            // so a submission made now has no model to run on: the recorded class, read
            // here off the published retired set rather than off the recorded row.
            case: "CreateModelInvocationJob by the bare id of a model offered no longer",
            create: Create::BatchInference,
            identifier: RETIRED_MODELS[0].to_string(),
            status: 400,
            code: "ValidationException",
            message: INVALID_MODEL_IDENTIFIER.to_string(),
        },
        Case {
            case: "CreateModelInvocationJob by the ARN of a model offered no longer",
            create: Create::BatchInference,
            identifier: foundation_model_arn(RETIRED_MODELS[1]),
            status: 404,
            code: "ResourceNotFoundException",
            message: foundation_model_not_found(&foundation_model_arn(RETIRED_MODELS[1])),
        },
        // ---- an evaluation: the families `EvaluationBedrockModelIdentifier` admits -----
        Case {
            case: "CreateEvaluationJob by the bare id of a model offered no longer",
            create: Create::EvaluationModels,
            identifier: RETIRED_MODELS[0].to_string(),
            status: 400,
            code: "ValidationException",
            message: INVALID_MODEL_IDENTIFIER.to_string(),
        },
        Case {
            case: "CreateEvaluationJob by the ARN of a model offered no longer",
            create: Create::EvaluationModels,
            identifier: foundation_model_arn(RETIRED_MODELS[1]),
            status: 404,
            code: "ResourceNotFoundException",
            message: foundation_model_not_found(&foundation_model_arn(RETIRED_MODELS[1])),
        },
        Case {
            case: "CreateEvaluationJob by a bare id no catalogue row carries",
            create: Create::EvaluationModels,
            identifier: "acme.not-a-model-v1".to_string(),
            status: 400,
            code: "ValidationException",
            message: INVALID_MODEL_IDENTIFIER.to_string(),
        },
        Case {
            // The geography-prefixed spelling names a model THROUGH a cross-region profile,
            // and this emulator publishes none for a model it has no row for.
            case: "CreateEvaluationJob by a geography-prefixed id no profile routes",
            create: Create::EvaluationModels,
            identifier: "us.acme.not-a-model-v1".to_string(),
            status: 400,
            code: "ValidationException",
            message: INVALID_MODEL_IDENTIFIER.to_string(),
        },
        Case {
            case: "CreateEvaluationJob by a foundation-model ARN no row carries",
            create: Create::EvaluationModels,
            identifier: missing_foundation_model.clone(),
            status: 404,
            code: "ResourceNotFoundException",
            message: foundation_model_not_found(&missing_foundation_model),
        },
        Case {
            case: "CreateEvaluationJob by a provisioned-model ARN nothing bought",
            create: Create::EvaluationModels,
            identifier: missing_provisioned_model.clone(),
            status: 404,
            code: "ResourceNotFoundException",
            message: provisioned_model_not_found(&missing_provisioned_model),
        },
        Case {
            case: "CreateEvaluationJob by an imported-model ARN nothing imported",
            create: Create::EvaluationModels,
            identifier: format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:imported-model/aaaaaaaaaaaa"),
            status: 404,
            code: "ResourceNotFoundException",
            message: IMPORTED_MODEL_NOT_FOUND.to_string(),
        },
        Case {
            case: "CreateEvaluationJob by an inference-profile ARN nothing publishes",
            create: Create::EvaluationModels,
            identifier: format!(
                "arn:aws:bedrock:{REGION}:{ACCOUNT}:inference-profile/us.acme.not-a-model-v1"
            ),
            status: 404,
            code: "ResourceNotFoundException",
            message: INFERENCE_PROFILE_NOT_FOUND.to_string(),
        },
        Case {
            case: "CreateEvaluationJob by a prompt-router ARN nothing holds",
            create: Create::EvaluationModels,
            identifier: missing_prompt_router.clone(),
            status: 404,
            code: "ResourceNotFoundException",
            message: prompt_router_not_found(&missing_prompt_router),
        },
        Case {
            // An Amazon SageMaker endpoint names a model only once it has been REGISTERED
            // with Amazon Bedrock Marketplace. The ARN belongs to another service's
            // namespace, so it names no resource of this one to report missing: it is read
            // as the spelling it is.
            case: "CreateEvaluationJob by a SageMaker endpoint ARN nothing registered",
            create: Create::EvaluationModels,
            identifier: format!("arn:aws:sagemaker:{REGION}:{ACCOUNT}:endpoint/acme-endpoint"),
            status: 400,
            code: "ValidationException",
            message: INVALID_MODEL_IDENTIFIER.to_string(),
        },
        Case {
            // The OTHER model-identifying member of the same configuration: the model a
            // knowledge-base evaluation generates its responses with.
            case: "CreateEvaluationJob by a ragConfigs generation model nothing carries",
            create: Create::EvaluationRag,
            identifier: "acme.not-a-model-v1".to_string(),
            status: 400,
            code: "ValidationException",
            message: INVALID_MODEL_IDENTIFIER.to_string(),
        },
    ]
}

/// A model the catalogue this emulator publishes carries, read through
/// `ListFoundationModels` exactly as a caller would, so the resolving rows follow the
/// catalogue rather than a pinned spelling.
async fn published_model(router: &MultiServiceRouter) -> String {
    let (status, error, listed) = bedrock(router, "GET", "/foundation-models", None).await;
    assert_eq!(
        status, 200,
        "setup: ListFoundationModels must answer 200, got {status} {error} ({listed})"
    );
    listed["modelSummaries"]
        .as_array()
        .and_then(|models| models.first())
        .map(|model| text(model, "modelId"))
        .expect("the catalogue must publish at least one model")
}

/// A `jobName` for row `index`: `^[a-z0-9](-*[a-z0-9]){0,62}$` is the narrower of the two
/// creates' name patterns, so one stem of lowercase letters, digits and dashes suits both.
fn job_name(stem: &str, index: usize) -> String {
    format!("{stem}-{index}")
}

/// The rows above name models the service offers no longer; this holds that claim to the
/// catalogue the service itself publishes, so a row cannot quietly become one about an
/// OFFERED model.
#[tokio::test]
async fn the_catalogue_offers_none_of_the_retired_models_these_rows_name() {
    let router = emulator();
    let (status, error, listed) = bedrock(&router, "GET", "/foundation-models", None).await;
    assert_eq!(
        status, 200,
        "ListFoundationModels must answer 200, got {status} {error} ({listed})"
    );
    let offered: Vec<String> = listed["modelSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|model| text(model, "modelId"))
        .collect();
    assert!(
        !offered.is_empty(),
        "the catalogue must publish models, got {listed}"
    );
    for retired in RETIRED_MODELS {
        assert!(
            !offered.iter().any(|model_id| model_id == retired),
            "case {retired:?}: this row names a model the catalogue must offer no row for, \
             and the listing carries one"
        );
    }
}

#[tokio::test]
async fn an_unresolvable_model_identifier_is_refused_before_the_pass_role_decision() {
    let router = emulator();
    create_roles(&router).await;

    for (index, case) in cases().iter().enumerate() {
        for pass in role_passes() {
            let name = job_name("unresolvable", index);
            let (status, error, body) = bedrock(
                &router,
                "POST",
                case.create.path(),
                Some(case.create.body(&name, &case.identifier, &pass.arn)),
            )
            .await;
            assert_eq!(
                (status, error.as_str(), text(&body, "message").as_str()),
                (case.status, case.code, case.message.as_str()),
                "case {:?} with {} ({}): the identifier is resolved before the pass-role \
                 decision, so the refusal is the identifier's own whatever that decision \
                 would be, got {status} {error} ({body})",
                case.case,
                pass.case,
                case.identifier
            );
            assert!(
                body.get("jobArn").is_none(),
                "case {:?} with {}: a refused create mints nothing, got {body}",
                case.case,
                pass.case
            );
            assert_ne!(
                text(&body, "message"),
                PASS_ROLE_REFUSAL,
                "case {:?} with {}: a request naming no model is never answered with a \
                 report about the role, got {body}",
                case.case,
                pass.case
            );
        }
    }
}

#[tokio::test]
async fn a_resolving_model_identifier_is_still_judged_by_the_pass_role_decision() {
    let router = emulator();
    create_roles(&router).await;
    let model = published_model(&router).await;

    for create in [Create::BatchInference, Create::EvaluationModels] {
        for (index, pass) in role_passes().iter().enumerate() {
            let name = job_name("resolving", index);
            let (status, error, body) = bedrock(
                &router,
                "POST",
                create.path(),
                Some(create.body(&name, &model, &pass.arn)),
            )
            .await;
            if !pass.admitted {
                assert_eq!(
                    (status, error.as_str(), text(&body, "message").as_str()),
                    (403, "AccessDeniedException", PASS_ROLE_REFUSAL),
                    "case {:?} ({model}): the identifier resolves, so the pass-role decision \
                     is what refuses the create, got {status} {error} ({body})",
                    pass.case
                );
                continue;
            }
            assert_eq!(
                status,
                create.success(),
                "case {:?} ({model}): an admitted pass over a model the catalogue carries \
                 must mint the job, got {status} {error} ({body})",
                pass.case
            );
            // Read the job back through the operation a caller polls it with: the create
            // wrote the record every later request sees.
            let job_arn = text(&body, "jobArn");
            let read_path = match create {
                Create::BatchInference => format!("/model-invocation-job/{}", label(&job_arn)),
                Create::EvaluationModels | Create::EvaluationRag => {
                    format!("/evaluation-jobs/{}", label(&job_arn))
                }
            };
            let (status, error, read) = bedrock(&router, "GET", &read_path, None).await;
            let reported = text(&read, "status");
            let expected_status = match create {
                Create::BatchInference => INVOCATION_NON_TERMINAL.contains(&reported.as_str()),
                Create::EvaluationModels | Create::EvaluationRag => {
                    reported == EVALUATION_IN_PROGRESS
                }
            };
            assert_eq!(
                (
                    status,
                    text(&read, "jobArn").as_str(),
                    text(&read, "roleArn").as_str(),
                    expected_status,
                ),
                (200, job_arn.as_str(), pass.arn.as_str(), true),
                "case {:?}: the job must read back as created under the role it was passed, \
                 got {status} {error} ({read})",
                pass.case
            );
        }
    }
}

#[tokio::test]
async fn a_refusal_over_the_model_identifier_leaves_nothing_behind() {
    let router = emulator();
    create_roles(&router).await;
    let model = published_model(&router).await;
    let trusted = role_arn(TRUSTED_ROLE);

    for (index, case) in cases().iter().enumerate() {
        let name = job_name("leaves-nothing", index);
        let (status, _, _) = bedrock(
            &router,
            "POST",
            case.create.path(),
            Some(case.create.body(&name, &case.identifier, &trusted)),
        )
        .await;
        assert_eq!(
            status, case.status,
            "setup: case {:?} must be refused {} first",
            case.case, case.status
        );

        // Nothing was written: the batch inference listing reports no job under that name,
        // and the evaluation name is still FREE — a name a refused create had reserved would
        // make this second create a conflict instead.
        if case.create == Create::BatchInference {
            let (status, error, listed) = bedrock(
                &router,
                "GET",
                &format!("/model-invocation-jobs?nameContains={name}"),
                None,
            )
            .await;
            let rows = listed["invocationJobSummaries"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default();
            assert_eq!(
                (status, rows),
                (200, 0),
                "case {:?}: a refused submission leaves no job behind, got {status} {error} \
                 ({listed})",
                case.case
            );
        }
        let (status, error, body) = bedrock(
            &router,
            "POST",
            case.create.path(),
            Some(case.create.body(&name, &model, &trusted)),
        )
        .await;
        assert_eq!(
            status,
            case.create.success(),
            "case {:?}: the refused create reserved nothing, so the same name is free for a \
             create naming a model that resolves, got {status} {error} ({body})",
            case.case
        );
    }
}
