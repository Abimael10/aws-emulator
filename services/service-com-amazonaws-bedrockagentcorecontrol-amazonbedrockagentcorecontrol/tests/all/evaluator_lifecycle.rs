// SPDX-License-Identifier: CC0-1.0

//! The custom evaluator end to end over HTTP: what `CreateEvaluator` persists, how
//! `GetEvaluator` reports it, the statuses the clock owes it, what an accepted
//! `UpdateEvaluator` changes, and what a deletion leaves a later call to see.
//!
//! Every row drives the operations as a client does — one shared engine behind one router —
//! so a create and the read that follows it are asserted against the same store rather than
//! against each other's envelopes.
//!
//! Sources: `smithy_query(query="closure", operation="CreateEvaluator")` and
//! `smithy_query(query="operation", operation="GetEvaluator,UpdateEvaluator,DeleteEvaluator")`
//! for every member, id, ARN form and enum value asserted here; the documentation of
//! `GetEvaluatorResponse`'s `evaluatorType` ("`Custom` – A customer-created evaluator",
//! "`CustomCode` – A customer-created code-based evaluator", "`CustomDerived` – A
//! customer-created evaluator derived from an existing base evaluator"), of its `provider`
//! ("The source of the evaluator's logic: Amazon Web Services, a third-party library, or
//! you") and of its `lockedForModification` ("Whether the evaluator is locked for
//! modification due to being referenced by active online evaluation configurations"); and
//! `GetEvaluatorRequest`'s `includedData` ("`METADATA_ONLY` returns evaluator metadata and
//! model configuration without instructions or rating scale").

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for, which the ARNs they assert carry.
const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// How long a poll for a clock-owed transition keeps looking, and how long it waits between
/// looks. Generous: the condition is what the test waits for, not a duration.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const SETTLE_POLL: Duration = Duration::from_millis(50);

/// A customer managed key inside `KmsKeyArn`'s pattern
/// `^arn:aws(|-cn|-us-gov):kms:[a-zA-Z0-9-]*:[0-9]{12}:key/[a-zA-Z0-9-]{36}$`.
const KMS_KEY: &str = "arn:aws:kms:us-east-1:000000000000:key/12345678-1234-1234-1234-123456789012";

/// A Lambda function inside `LambdaArn`'s pattern.
const LAMBDA: &str = "arn:aws:lambda:us-east-1:000000000000:function:evaluatorJudge";

/// An IAM role ARN inside `RoleArn`'s pattern
/// `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`, for the online evaluation configuration
/// the lock rows create.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/AgentCoreEvaluation";

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

async fn create_evaluator(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    send(router, "POST", "/evaluators/create", Some(body)).await
}

async fn get_evaluator(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    send(router, "GET", &format!("/evaluators/{id}"), None).await
}

async fn get_evaluator_with(
    router: &MultiServiceRouter,
    id: &str,
    included_data: &str,
) -> (u16, Value) {
    send(
        router,
        "GET",
        &format!("/evaluators/{id}?includedData={included_data}"),
        None,
    )
    .await
}

async fn update_evaluator(router: &MultiServiceRouter, id: &str, body: &Value) -> (u16, Value) {
    send(router, "PUT", &format!("/evaluators/{id}"), Some(body)).await
}

async fn delete_evaluator(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    send(router, "DELETE", &format!("/evaluators/{id}"), None).await
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The error code `restJson1` puts in `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// An LLM-as-a-Judge configuration, the `EvaluatorConfig` member that carries instructions
/// and a rating scale.
fn llm_as_a_judge(instructions: &str) -> Value {
    json!({"llmAsAJudge": {
        "instructions": instructions,
        "ratingScale": {"numerical": [
            {"definition": "the answer is wrong", "value": 1.5, "label": "poor"},
            {"definition": "the answer is right", "value": 3.5, "label": "good"}
        ]},
        "modelConfig": {"bedrockEvaluatorModelConfig": {
            "modelId": "anthropic.claude-3-sonnet",
            "inferenceConfig": {"maxTokens": 512, "temperature": 0.5, "topP": 0.25}
        }}
    }})
}

/// A code-based configuration: a customer-managed Lambda function.
fn code_based() -> Value {
    json!({"codeBased": {"lambdaConfig": {
        "lambdaArn": LAMBDA,
        "lambdaTimeoutInSeconds": 30
    }}})
}

/// A derived configuration: a base evaluator the service provides, run on the caller's own
/// model. `Builtin.Correctness` is one of the `Builtin.…` forms `EvaluatorId` admits.
fn derived() -> Value {
    json!({"derived": {
        "baseEvaluatorId": "Builtin.Correctness",
        "modelConfig": {"responsesEvaluatorModelConfig": {
            "modelId": "openai.gpt-4",
            "maxOutputTokens": 256,
            "temperature": 0.5,
            "topP": 0.75,
            "reasoning": {"effort": "low"}
        }}
    }})
}

/// One create this suite drives, and the `EvaluatorType` the configuration it carries makes
/// the evaluator.
struct Row {
    case: &'static str,
    body: Value,
    evaluator_type: &'static str,
}

/// The rows: creates that differ in the `EvaluatorConfig` member they carry, in the
/// `EvaluatorLevel` they evaluate at (each of the enum's three values), in the optional
/// members they carry and in the length of the name.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an LLM-as-a-Judge evaluator with every optional member, at session level",
            body: json!({
                "evaluatorName": "judgeQuality",
                "description": "the evaluator the suite reads back",
                "evaluatorConfig": llm_as_a_judge("Rate the agent's answer."),
                "level": "SESSION",
                "kmsKeyArn": KMS_KEY,
                "tags": {"owner": "agentcore", "tier": "test"}
            }),
            evaluator_type: "Custom",
        },
        Row {
            case: "a code-based evaluator with the required members alone, at tool-call level",
            body: json!({
                "evaluatorName": "codeJudge",
                "evaluatorConfig": code_based(),
                "level": "TOOL_CALL"
            }),
            evaluator_type: "CustomCode",
        },
        Row {
            case: "a derived evaluator at a name of the member's maximum length, at trace level",
            body: json!({
                // `CustomEvaluatorName` is `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`.
                "evaluatorName": format!("d{}", "x".repeat(47)),
                "description": "d",
                "evaluatorConfig": derived(),
                "level": "TRACE"
            }),
            evaluator_type: "CustomDerived",
        },
    ]
}

/// What `CreateEvaluator` persists is what a read of the evaluator reports: the whole
/// configuration the create carried, the id and ARN it minted, the status it enters, the type
/// its configuration makes it and the provider a customer-created evaluator carries.
#[tokio::test]
async fn a_created_evaluator_reads_back() {
    let router = router();

    for row in &rows() {
        let case = row.case;
        let (status, created) = create_evaluator(&router, &row.body).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from CreateEvaluator, got {status} ({created})"
        );
        let name = text(&row.body, "evaluatorName");
        let id = text(&created, "evaluatorId");
        let expected_arn = format!("arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:evaluator/{id}");
        assert_eq!(
            (text(&created, "evaluatorArn"), text(&created, "status")),
            (expected_arn.clone(), "CREATING".to_string()),
            "case {case:?}: the create reports the evaluator's ARN and the status it enters, \
             got {created}"
        );
        assert!(
            created.get("createdAt").is_some(),
            "case {case:?}: the create reports createdAt, got {created}"
        );
        // `EvaluatorId`'s customer-created form is `^[a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}$`:
        // the name, then a 10-character suffix this service minted.
        let suffix = id
            .strip_prefix(&format!("{name}-"))
            .unwrap_or_else(|| panic!("case {case:?}: {id:?} is not {name:?} plus a suffix"));
        assert!(
            suffix.len() == 10 && suffix.chars().all(|character| character.is_ascii_alphanumeric()),
            "case {case:?}: the minted id's suffix is 10 alphanumerics, got {suffix:?}"
        );

        let (status, fetched) = get_evaluator(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetEvaluator, got {status} ({fetched})"
        );
        assert_eq!(
            (
                text(&fetched, "evaluatorId"),
                text(&fetched, "evaluatorArn"),
                text(&fetched, "evaluatorName"),
                text(&fetched, "level"),
                text(&fetched, "evaluatorType"),
                text(&fetched, "provider")
            ),
            (
                id.clone(),
                expected_arn,
                name.clone(),
                text(&row.body, "level"),
                row.evaluator_type.to_string(),
                "Custom".to_string()
            ),
            "case {case:?}: the read reports the evaluator the create minted, got {fetched}"
        );
        for member in ["description", "evaluatorConfig", "kmsKeyArn"] {
            assert_eq!(
                fetched.get(member),
                row.body.get(member),
                "case {case:?}: the read reports the {member} the create persisted, got {fetched}"
            );
        }
        assert_eq!(
            fetched.get("lockedForModification"),
            Some(&json!(false)),
            "case {case:?}: no online evaluation configuration references this evaluator, so it \
             is not locked, got {fetched}"
        );
        // The read reports the status the one record carries at the instant it is read: the
        // transient one until the transition the clock owes comes due, the settled one
        // afterwards, and nothing else. The transition itself is asserted by polling for it
        // in `a_created_evaluator_settles_from_creating_to_active`.
        let read_status = text(&fetched, "status");
        assert!(
            read_status == "CREATING" || read_status == "ACTIVE",
            "case {case:?}: the read reports the status the record carries, got \
             {read_status:?} ({fetched})"
        );
        for instant in ["createdAt", "updatedAt"] {
            assert!(
                fetched.get(instant).is_some(),
                "case {case:?}: the read reports {instant}, got {fetched}"
            );
        }
    }
}

/// The name "Must be unique within your account": a second create carrying a name this
/// account already holds is the modelled conflict, and leaves the first evaluator as the
/// first create wrote it.
#[tokio::test]
async fn a_name_this_account_already_holds_is_a_conflict() {
    let router = router();
    let body = json!({
        "evaluatorName": "uniqueJudge",
        "evaluatorConfig": llm_as_a_judge("Rate the first answer."),
        "level": "TRACE"
    });
    let (status, created) = create_evaluator(&router, &body).await;
    assert_eq!(
        status, 202,
        "case \"the first create\": expected HTTP 202, got {status} ({created})"
    );
    let first_id = text(&created, "evaluatorId");

    // A second create of the same name, with another configuration, is still the name that
    // collides.
    let mut again = body.clone();
    again["evaluatorConfig"] = code_based();
    let (status, refused) = create_evaluator(&router, &again).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (409, "ConflictException"),
        "case \"a repeated name\": expected HTTP 409 ConflictException, got {status} {} \
         ({refused})",
        error_code(&refused)
    );
    assert_eq!(
        text(&refused, "message"),
        "Evaluator with name 'uniqueJudge' already exists in this account",
        "case \"a repeated name\": got {refused}"
    );

    let (_, fetched) = get_evaluator(&router, &first_id).await;
    assert_eq!(
        fetched.get("evaluatorConfig"),
        body.get("evaluatorConfig"),
        "case \"a repeated name\": the refused create left the first evaluator as it was, got \
         {fetched}"
    );
}

/// A replayed `clientToken` answers with the evaluator that token first produced rather than
/// minting a second one — whatever else the repeated request carries.
#[tokio::test]
async fn a_repeated_client_token_answers_with_the_first_evaluator() {
    let router = router();
    // `ClientToken` is @length 33..256 with pattern `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`.
    let token = "a".repeat(33);
    let body = json!({
        "evaluatorName": "idempotentJudge",
        "clientToken": token,
        "evaluatorConfig": llm_as_a_judge("Rate the answer once."),
        "level": "SESSION"
    });
    let (status, first) = create_evaluator(&router, &body).await;
    assert_eq!(
        status, 202,
        "case \"the first create\": expected HTTP 202, got {status} ({first})"
    );

    let mut again = body.clone();
    again["evaluatorConfig"] = llm_as_a_judge("Rate the answer twice.");
    let (status, replayed) = create_evaluator(&router, &again).await;
    assert_eq!(
        (status, text(&replayed, "evaluatorId")),
        (202, text(&first, "evaluatorId")),
        "case \"the replayed token\": expected the evaluator the token first produced, got \
         {status} ({replayed})"
    );

    // The replay persisted nothing of its own: the stored evaluator is still the first
    // create's.
    let (_, fetched) = get_evaluator(&router, &text(&first, "evaluatorId")).await;
    assert_eq!(
        fetched.get("evaluatorConfig"),
        body.get("evaluatorConfig"),
        "case \"the replayed token\": the replay left the first evaluator as it was, got \
         {fetched}"
    );
}

/// An id naming no evaluator this account holds is the modelled 404, for ids that differ in
/// which form of `EvaluatorId` they carry — including the `Builtin.…` and `ThirdParty.…`
/// forms of the evaluators the service provides, which this account created none of.
#[tokio::test]
async fn an_id_naming_no_evaluator_is_not_found() {
    let router = router();
    let (status, created) = create_evaluator(
        &router,
        &json!({
            "evaluatorName": "heldJudge",
            "evaluatorConfig": code_based(),
            "level": "TRACE"
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case \"the create\": expected HTTP 202, got {status} ({created})"
    );
    let held = text(&created, "evaluatorId");

    // (case, the id the read names) — each inside one of `EvaluatorId`'s own forms.
    let rows: [(&str, String); 4] = [
        (
            "a name this account never created",
            "otherJudge-abcdefghij".to_string(),
        ),
        (
            "the held evaluator's name with another suffix",
            format!("heldJudge-{}", "z".repeat(10)),
        ),
        (
            "a built-in evaluator of the service",
            "Builtin.Correctness".to_string(),
        ),
        (
            "a third-party evaluator of the service",
            "ThirdParty.vendor.quality".to_string(),
        ),
    ];

    for (case, id) in rows {
        let (status, refused) = get_evaluator(&router, &id).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?} ({id}): expected HTTP 404 ResourceNotFoundException, got {status} {} \
             ({refused})",
            error_code(&refused)
        );
        assert_eq!(
            text(&refused, "message"),
            "Evaluator does not exist",
            "case {case:?}: got {refused}"
        );
    }

    let (status, fetched) = get_evaluator(&router, &held).await;
    assert_eq!(
        status, 200,
        "case \"the held evaluator\": the id the create minted is still served, got {status} \
         ({fetched})"
    );
}

/// Poll a read of `id` until its status is `wanted`, and answer the last body read. Waits for
/// the CONDITION the clock owes, never for a duration.
async fn poll_until_status(router: &MultiServiceRouter, id: &str, wanted: &str) -> Value {
    let started = Instant::now();
    let (_, mut last) = get_evaluator(router, id).await;
    while started.elapsed() < SETTLE_DEADLINE && text(&last, "status") != wanted {
        tokio::time::sleep(SETTLE_POLL).await;
        let (_, fresh) = get_evaluator(router, id).await;
        last = fresh;
    }
    last
}

/// The clock-owed transition: an evaluator read straight after the create is still
/// `CREATING`, and reports `ACTIVE` once the span the create owes has passed.
#[tokio::test]
async fn a_created_evaluator_settles_from_creating_to_active() {
    let router = router();
    let (status, created) = create_evaluator(
        &router,
        &json!({
            "evaluatorName": "settlingJudge",
            "evaluatorConfig": llm_as_a_judge("Rate the settling answer."),
            "level": "SESSION"
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case \"the create\": expected HTTP 202, got {status} ({created})"
    );
    assert_eq!(
        text(&created, "status"),
        "CREATING",
        "case \"straight after the create\": a new evaluator enters CREATING, got {created}"
    );
    let id = text(&created, "evaluatorId");

    let settled = poll_until_status(&router, &id, "ACTIVE").await;
    assert_eq!(
        text(&settled, "status"),
        "ACTIVE",
        "case \"past the settle span\": the CREATING -> ACTIVE transition is due, got {settled}"
    );
}

/// An accepted update replaces the members the request carries and leaves the ones it does
/// not: the evaluator reports `UPDATING` until the clock owes it `ACTIVE`, and a read reports
/// the patched configuration, the level it now evaluates at and the type its new
/// configuration makes it.
#[tokio::test]
async fn an_accepted_update_patches_the_members_it_carries() {
    let router = router();
    let created_body = json!({
        "evaluatorName": "patchedJudge",
        "description": "the description the create persisted",
        "evaluatorConfig": llm_as_a_judge("Rate the answer before the update."),
        "level": "TRACE",
        "kmsKeyArn": KMS_KEY
    });
    let (status, created) = create_evaluator(&router, &created_body).await;
    assert_eq!(
        status, 202,
        "case \"the create\": expected HTTP 202, got {status} ({created})"
    );
    let id = text(&created, "evaluatorId");
    poll_until_status(&router, &id, "ACTIVE").await;

    // (case, the update body, the members a read must report afterwards)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "the description alone",
            json!({"description": "the description the update carried"}),
            json!({
                "description": "the description the update carried",
                "evaluatorConfig": llm_as_a_judge("Rate the answer before the update."),
                "level": "TRACE",
                "evaluatorType": "Custom",
                "kmsKeyArn": KMS_KEY
            }),
        ),
        (
            "the level and the configuration, which changes the evaluator's type",
            json!({"level": "SESSION", "evaluatorConfig": code_based()}),
            json!({
                "description": "the description the update carried",
                "evaluatorConfig": code_based(),
                "level": "SESSION",
                "evaluatorType": "CustomCode",
                "kmsKeyArn": KMS_KEY
            }),
        ),
        (
            "every editable member at once",
            json!({
                "description": "the last description",
                "evaluatorConfig": derived(),
                "level": "TOOL_CALL",
                "kmsKeyArn": KMS_KEY
            }),
            json!({
                "description": "the last description",
                "evaluatorConfig": derived(),
                "level": "TOOL_CALL",
                "evaluatorType": "CustomDerived",
                "kmsKeyArn": KMS_KEY
            }),
        ),
    ];

    for (case, update, expected) in rows {
        let (status, accepted) = update_evaluator(&router, &id, &update).await;
        assert_eq!(
            (status, text(&accepted, "status").as_str()),
            (202, "UPDATING"),
            "case {case:?}: an accepted update is 202 and reports UPDATING, got {status} \
             ({accepted})"
        );
        assert_eq!(
            (
                text(&accepted, "evaluatorId"),
                text(&accepted, "evaluatorArn")
            ),
            (
                id.clone(),
                format!("arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:evaluator/{id}")
            ),
            "case {case:?}: the update reports the evaluator it changed, got {accepted}"
        );
        assert!(
            accepted.get("updatedAt").is_some(),
            "case {case:?}: the update reports updatedAt, got {accepted}"
        );

        let settled = poll_until_status(&router, &id, "ACTIVE").await;
        for (member, want) in expected.as_object().expect("the expectation is an object") {
            assert_eq!(
                settled.get(member),
                Some(want),
                "case {case:?}: the read reports the {member} the update left, got {settled}"
            );
        }
        // The name is not modifiable by this operation — the request carries no member for
        // it — so it is still the one the create stored.
        assert_eq!(
            text(&settled, "evaluatorName"),
            "patchedJudge",
            "case {case:?}: the update left the evaluator's name alone, got {settled}"
        );
    }
}

/// An evaluator whose own create is still settling takes a change and takes a deletion: the
/// only state a write collides with is a deletion already accepted, which
/// `a_deleted_evaluator_is_refused_a_change_and_then_gone` asserts.
#[tokio::test]
async fn an_evaluator_still_creating_takes_a_change_and_a_deletion() {
    let router = router();
    let body = json!({
        "evaluatorName": "busyJudge",
        "evaluatorConfig": llm_as_a_judge("Rate the busy answer."),
        "level": "SESSION"
    });
    let (status, created) = create_evaluator(&router, &body).await;
    assert_eq!(
        (status, text(&created, "status").as_str()),
        (202, "CREATING"),
        "case \"the create\": got {status} ({created})"
    );
    let id = text(&created, "evaluatorId");

    // The change is asked for while the create's own transition is still owed: it is accepted,
    // and what it carried is what the evaluator reports once the clock settles it.
    let (status, accepted) = update_evaluator(&router, &id, &json!({"level": "TRACE"})).await;
    assert_eq!(
        (status, text(&accepted, "status").as_str()),
        (202, "UPDATING"),
        "case \"an update straight after the create\": an accepted update is 202 and reports \
         UPDATING, got {status} ({accepted})"
    );
    let settled = poll_until_status(&router, &id, "ACTIVE").await;
    assert_eq!(
        (
            text(&settled, "status").as_str(),
            text(&settled, "level").as_str()
        ),
        ("ACTIVE", "TRACE"),
        "case \"an update straight after the create\": the read reports the level the update \
         carried, got {settled}"
    );

    // A DELETION asked of an evaluator still creating is accepted too: the resource is on its
    // way out whatever it was doing.
    let (status, creating) = create_evaluator(
        &router,
        &json!({
            "evaluatorName": "removedWhileCreating",
            "evaluatorConfig": code_based(),
            "level": "TRACE"
        }),
    )
    .await;
    assert_eq!(
        (status, text(&creating, "status").as_str()),
        (202, "CREATING"),
        "case \"the create of the evaluator deleted while creating\": got {status} ({creating})"
    );
    let creating_id = text(&creating, "evaluatorId");
    let (status, accepted) = delete_evaluator(&router, &creating_id).await;
    assert_eq!(
        (status, text(&accepted, "status").as_str()),
        (202, "DELETING"),
        "case \"a delete straight after the create\": an accepted deletion is 202 and reports \
         DELETING, got {status} ({accepted})"
    );
}

/// An accepted deletion is reported `DELETING` and the evaluator stays readable until the
/// removal the clock owes comes due: a change asked of it in that window is the modelled
/// conflict, and once the removal is due every call that names it is the modelled 404.
#[tokio::test]
async fn a_deleted_evaluator_is_refused_a_change_and_then_gone() {
    let router = router();
    let (status, created) = create_evaluator(
        &router,
        &json!({
            "evaluatorName": "deletedJudge",
            "evaluatorConfig": code_based(),
            "level": "TOOL_CALL"
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case \"the create\": expected HTTP 202, got {status} ({created})"
    );
    let id = text(&created, "evaluatorId");
    poll_until_status(&router, &id, "ACTIVE").await;

    let (status, accepted) = delete_evaluator(&router, &id).await;
    assert_eq!(
        (status, text(&accepted, "status").as_str()),
        (202, "DELETING"),
        "case \"the delete\": an accepted deletion is 202 and reports DELETING, got {status} \
         ({accepted})"
    );
    assert_eq!(
        (
            text(&accepted, "evaluatorId"),
            text(&accepted, "evaluatorArn")
        ),
        (
            id.clone(),
            format!("arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:evaluator/{id}")
        ),
        "case \"the delete\": the deletion reports the evaluator it is removing, got {accepted}"
    );

    // In the window the removal is owed in, the evaluator is still readable and reports that
    // it is going away, and an update of it is refused as the conflict it is.
    let (status, fetched) = get_evaluator(&router, &id).await;
    assert_eq!(
        (status, text(&fetched, "status").as_str()),
        (200, "DELETING"),
        "case \"a read in the deletion window\": got {status} ({fetched})"
    );
    let (status, refused) = update_evaluator(&router, &id, &json!({"level": "TRACE"})).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (409, "ConflictException"),
        "case \"an update in the deletion window\": expected HTTP 409 ConflictException, got \
         {status} {} ({refused})",
        error_code(&refused)
    );
    assert_eq!(
        text(&refused, "message"),
        format!("Evaluator {id} is in DELETING status and cannot be modified."),
        "case \"an update in the deletion window\": got {refused}"
    );

    // Past the removal the clock owes, the evaluator is gone: the read, the update and a
    // repeated delete all report the modelled 404.
    let started = Instant::now();
    let mut read = get_evaluator(&router, &id).await;
    while started.elapsed() < SETTLE_DEADLINE && read.0 != 404 {
        tokio::time::sleep(SETTLE_POLL).await;
        read = get_evaluator(&router, &id).await;
    }
    assert_eq!(
        (read.0, error_code(&read.1).as_str()),
        (404, "ResourceNotFoundException"),
        "case \"past the removal\": expected HTTP 404 ResourceNotFoundException, got {} ({})",
        read.0,
        read.1
    );
    for (case, answered) in [
        (
            "an update past the removal",
            update_evaluator(&router, &id, &json!({"level": "TRACE"})).await,
        ),
        (
            "a repeated delete past the removal",
            delete_evaluator(&router, &id).await,
        ),
    ] {
        assert_eq!(
            (answered.0, error_code(&answered.1).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected HTTP 404 ResourceNotFoundException, got {} ({})",
            answered.0,
            answered.1
        );
        assert_eq!(
            text(&answered.1, "message"),
            "Evaluator does not exist",
            "case {case:?}: got {}",
            answered.1
        );
    }

    // The name the removed evaluator held is free again.
    let (status, again) = create_evaluator(
        &router,
        &json!({
            "evaluatorName": "deletedJudge",
            "evaluatorConfig": code_based(),
            "level": "TOOL_CALL"
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case \"a create of the removed evaluator's name\": expected HTTP 202, got {status} \
         ({again})"
    );
}

/// An evaluator an online evaluation configuration is evaluating with is locked for
/// modification: the read reports the lock and a write of it is refused the modelled
/// conflict, while an evaluator no running evaluation references is not locked and takes the
/// change.
#[tokio::test]
async fn an_evaluator_a_running_evaluation_uses_is_locked_for_modification() {
    let router = router();

    // (case, the evaluator's name, the configuration's name, whether its evaluation runs,
    //  whether the evaluator is then locked)
    let rows: [(&str, &str, &str, bool, bool); 2] = [
        (
            "an evaluation that began on creation",
            "lockedJudge",
            "runningConfig",
            true,
            true,
        ),
        (
            "an evaluation that is not running",
            "unlockedJudge",
            "stoppedConfig",
            false,
            false,
        ),
    ];

    for (case, evaluator_name, config_name, running, locked) in rows {
        let (status, created) = create_evaluator(
            &router,
            &json!({
                "evaluatorName": evaluator_name,
                "evaluatorConfig": llm_as_a_judge("Rate the referenced answer."),
                "level": "SESSION"
            }),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from CreateEvaluator, got {status} ({created})"
        );
        let id = text(&created, "evaluatorId");
        poll_until_status(&router, &id, "ACTIVE").await;

        let (status, config) = send(
            &router,
            "POST",
            "/online-evaluation-configs/create",
            Some(&json!({
                "onlineEvaluationConfigName": config_name,
                "rule": {"samplingConfig": {"samplingPercentage": 10.0}},
                "dataSourceConfig": {"cloudWatchLogs": {
                    "serviceNames": ["agentservice"],
                    "logGroupNames": ["/aws/agentcore/locking"]
                }},
                "evaluators": [{"evaluatorId": id}],
                "evaluationExecutionRoleArn": EXECUTION_ROLE,
                "enableOnCreate": running
            })),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from CreateOnlineEvaluationConfig, got {status} \
             ({config})"
        );

        let (_, fetched) = get_evaluator(&router, &id).await;
        assert_eq!(
            fetched.get("lockedForModification"),
            Some(&json!(locked)),
            "case {case:?}: expected lockedForModification {locked}, got {fetched}"
        );

        let (status, answered) = update_evaluator(&router, &id, &json!({"level": "TRACE"})).await;
        if locked {
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (409, "ConflictException"),
                "case {case:?}: expected HTTP 409 ConflictException, got {status} {} \
                 ({answered})",
                error_code(&answered)
            );
            assert_eq!(
                text(&answered, "message"),
                format!(
                    "Evaluator {id} is referenced by an active online evaluation configuration \
                     and cannot be modified."
                ),
                "case {case:?}: got {answered}"
            );
            // The deletion of a locked evaluator is NOT a modification of it: the lock
            // refuses a change asked of the evaluator, and a read still reports it as the
            // refused update left it.
            let (status, still) = get_evaluator(&router, &id).await;
            assert_eq!(
                (status, text(&still, "status").as_str(), text(&still, "level").as_str()),
                (200, "ACTIVE", "SESSION"),
                "case {case:?}: the refused update left the evaluator active and as it was, got \
                 {status} ({still})"
            );
        } else {
            assert_eq!(
                (status, text(&answered, "status").as_str()),
                (202, "UPDATING"),
                "case {case:?}: an evaluator no running evaluation uses takes the change, got \
                 {status} ({answered})"
            );
        }
    }
}

/// `includedData` is what a read reports: `ALL_DATA` — the default — returns the full
/// evaluator including its instructions and rating scale, `METADATA_ONLY` returns its
/// metadata and model configuration without them, and the stored evaluator is unchanged by
/// either read.
#[tokio::test]
async fn metadata_only_withholds_the_instructions_and_the_rating_scale() {
    let router = router();
    let config = llm_as_a_judge("Rate the withheld answer.");
    let (status, created) = create_evaluator(
        &router,
        &json!({
            "evaluatorName": "withholdingJudge",
            "evaluatorConfig": config,
            "level": "TRACE"
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case \"the create\": expected HTTP 202, got {status} ({created})"
    );
    let id = text(&created, "evaluatorId");
    let model_config = &config["llmAsAJudge"]["modelConfig"];

    // (case, the includedData the read asks for, the instructions it reports, the rating
    //  scale it reports)
    let rows: [(&str, Option<&str>, &str, Value); 3] = [
        (
            "no includedData, which is ALL_DATA",
            None,
            "Rate the withheld answer.",
            config["llmAsAJudge"]["ratingScale"].clone(),
        ),
        (
            "ALL_DATA",
            Some("ALL_DATA"),
            "Rate the withheld answer.",
            config["llmAsAJudge"]["ratingScale"].clone(),
        ),
        (
            "METADATA_ONLY",
            Some("METADATA_ONLY"),
            "",
            json!({"numerical": []}),
        ),
    ];

    for (case, included_data, instructions, rating_scale) in rows {
        let (status, fetched) = match included_data {
            Some(value) => get_evaluator_with(&router, &id, value).await,
            None => get_evaluator(&router, &id).await,
        };
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} ({fetched})"
        );
        let read = &fetched["evaluatorConfig"]["llmAsAJudge"];
        assert_eq!(
            read.get("instructions"),
            Some(&json!(instructions)),
            "case {case:?}: got {fetched}"
        );
        assert_eq!(
            read.get("ratingScale"),
            Some(&rating_scale),
            "case {case:?}: got {fetched}"
        );
        // The model configuration is metadata: every read reports it in full.
        assert_eq!(
            read.get("modelConfig"),
            Some(model_config),
            "case {case:?}: the read reports the model configuration in full, got {fetched}"
        );
    }

    // The withholding read changed nothing: the full data is still there to be read.
    let (_, fetched) = get_evaluator(&router, &id).await;
    assert_eq!(
        fetched.get("evaluatorConfig"),
        Some(&config),
        "case \"after a METADATA_ONLY read\": the stored evaluator is unchanged, got {fetched}"
    );
}
