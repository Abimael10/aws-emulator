// SPDX-License-Identifier: CC0-1.0

//! The WIRE ENVELOPE every refusal of the capacity-provider surface answers with.
//!
//! RULE: a capacity-provider call this service refuses is answered at the status its error
//! shape is modelled with, with the shape's name in the `x-amzn-errortype` header, the class
//! of the fault in the body's `Type` member — `User` for every 4xx, which is what that
//! member states (`smithy_query(query="shape",shape="ResourceNotFoundException,InvalidParameterValueException",model_path="services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json")`)
//! — and the sentence of that refusal in the shape's message member. Nothing of the kind
//! travels on a call this service SERVES: a served answer carries no fault class at all.
//!
//! Two refusals are what the surface is addressed by:
//!
//! * a `CapacityProviderName` in either documented form that names no provider of this
//!   Region is `ResourceNotFoundException` (404) from every operation addressed at a
//!   provider, whether the name was never created or the provider it named has since been
//!   removed;
//! * a `PermissionsConfig.CapacityProviderOperatorRoleArn` the create cannot manage compute
//!   as is refused before anything is stored — the modelled
//!   `InvalidParameterValueException` (400) for a role this account's IAM answered about,
//!   and `AccessDeniedException` (403) for a role of another account, which no principal
//!   here may pass.
//!
//! The rows differ along what the rule generalises over: the operation that asked, which of
//! the two documented `CapacityProviderName` forms addressed it, whether the provider was
//! never created or removed after it existed, and which role the create named. The sentences
//! themselves — and which value each operation's not-found sentence states — are pinned by
//! `crate::refusal_message_rules` and `crate::capacity_provider_operator_role`; what this
//! suite adds is the whole envelope those sentences arrive in, over every one of those
//! operations at once.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::capacity_provider::{
    CROSS_ACCOUNT_PASS_ROLE_MESSAGE, OPERATOR_ROLE_INVALID_MESSAGE,
};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;
use smithy_server_runtime::MultiServiceRouter;

use crate::capacity_provider_operator_role::{given_role, router_with_iam, trust};
use crate::capacity_provider_refusal_messages::make_removal_due;
use crate::function_lifecycle::{
    json_call, make_activation_due, router_on, Answer, ACCOUNT, REGION,
};

/// Where the capacity-provider operations are bound
/// (`smithy_query(query="operation",operation="GetCapacityProvider")`:
/// `GET /2025-11-30/capacity-providers/{CapacityProviderName}`).
const CAPACITY_PROVIDERS_URI: &str = "/2025-11-30/capacity-providers";

/// The class of fault a request the CALLER can fix is reported under, as the `Type` member
/// of this service's error shapes spells it.
const USER_FAULT: &str = "User";

/// The service principal an operator role has to let assume it.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// A well-formed operator role ARN of THIS account, which an emulator with no IAM mounted
/// knows nothing against and therefore serves.
const OPERATOR_ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-capacity";

/// The ARN a capacity provider of `name` carries, as the model's `CapacityProviderArn`
/// pattern spells it.
fn provider_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:capacity-provider:{name}")
}

/// The error shape an answer names in its `x-amzn-errortype` header, with any namespace a
/// renderer qualified it with and any URL a renderer appended dropped.
fn errortype(answer: &Answer) -> String {
    let raw = answer.header("x-amzn-errortype").unwrap_or_default();
    let named = raw.split(':').next().unwrap_or_default();
    match named.rsplit_once('#') {
        Some((_, shape)) => shape.to_string(),
        None => named.to_string(),
    }
}

/// The refusal sentence an answer carries, under either casing this service's error shapes
/// spell the member with.
fn sentence(answer: &Answer) -> String {
    answer
        .string("message")
        .or_else(|| answer.string("Message"))
        .unwrap_or_else(|| String::from_utf8_lossy(&answer.body).to_string())
}

/// The class of fault an answer names, or `None` when it names none.
fn fault_class(answer: &Answer) -> Option<String> {
    answer.string("Type")
}

/// The body text of an answer, for an assertion message.
fn text(answer: &Answer) -> String {
    String::from_utf8_lossy(&answer.body).to_string()
}

/// The not-found sentence a capacity-provider operation states about `subject`: two fixed
/// clauses around the one value the refusal names.
fn not_found_sentence(subject: &str) -> String {
    format!(
        "Capacity provider not found: {subject}. Check that the ARN is correct and the \
         capacity provider exists in this Region."
    )
}

/// `CreateCapacityProvider` of `name`, managed as `role_arn`, stating the required
/// configuration alone.
async fn create_provider(router: &MultiServiceRouter, name: &str, role_arn: &str) -> Answer {
    json_call(
        router,
        "POST",
        CAPACITY_PROVIDERS_URI,
        &json!({
            "CapacityProviderName": name,
            "VpcConfig": { "SubnetIds": ["subnet-0123456789abcdef0"], "SecurityGroupIds": [] },
            "PermissionsConfig": { "CapacityProviderOperatorRoleArn": role_arn },
        }),
    )
    .await
}

/// Whether the store holds a provider under `name`, which is what says a refused create
/// persisted nothing.
fn holds_provider(engine: &Arc<engine::InMemoryEngine>, name: &str) -> bool {
    engine
        .state::<ComAmazonawsLambdaAwsgirapiserviceState>()
        .lock()
        .capacity_providers
        .contains_key(name)
}

/// One operation addressed at a capacity provider: how it is called, the status it SERVES
/// at, and what its not-found sentence names — the identifier the request carried, or the
/// ARN that identifier names (recorded per operation; see
/// `crate::state::capacity_provider::capacity_provider_not_found_message`).
struct ProviderOperation {
    operation: &'static str,
    method: &'static str,
    /// The request URI, built from the reference the request addresses the provider by.
    uri: fn(&str) -> String,
    body: fn() -> Value,
    served_status: u16,
    /// Whether the not-found sentence states the ARN rather than the identifier as supplied.
    states_the_arn: bool,
}

/// Every operation this surface addresses a provider by name with: the read, the update,
/// the deletion and the version listing.
fn provider_operations() -> Vec<ProviderOperation> {
    vec![
        ProviderOperation {
            operation: "GetCapacityProvider",
            method: "GET",
            uri: |reference| format!("{CAPACITY_PROVIDERS_URI}/{reference}"),
            body: || Value::Null,
            served_status: 200,
            states_the_arn: true,
        },
        ProviderOperation {
            operation: "UpdateCapacityProvider",
            method: "PUT",
            uri: |reference| format!("{CAPACITY_PROVIDERS_URI}/{reference}"),
            body: || json!({ "CapacityProviderScalingConfig": { "MaxVCpuCount": 10 } }),
            served_status: 202,
            states_the_arn: true,
        },
        ProviderOperation {
            operation: "ListFunctionVersionsByCapacityProvider",
            method: "GET",
            uri: |reference| format!("{CAPACITY_PROVIDERS_URI}/{reference}/function-versions"),
            body: || Value::Null,
            served_status: 200,
            states_the_arn: false,
        },
        ProviderOperation {
            operation: "DeleteCapacityProvider",
            method: "DELETE",
            uri: |reference| format!("{CAPACITY_PROVIDERS_URI}/{reference}"),
            body: || Value::Null,
            served_status: 202,
            states_the_arn: false,
        },
    ]
}

/// Call `operation` against the provider `reference` names.
async fn call_operation(
    router: &MultiServiceRouter,
    operation: &ProviderOperation,
    reference: &str,
) -> Answer {
    json_call(
        router,
        operation.method,
        &(operation.uri)(reference),
        &(operation.body)(),
    )
    .await
}

/// Assert the whole 404 envelope one operation answered for `reference`.
fn assert_not_found(case: &str, operation: &ProviderOperation, reference: &str, answer: &Answer) {
    let subject = if operation.states_the_arn {
        provider_arn(trimmed_name(reference))
    } else {
        reference.to_string()
    };
    let expected = not_found_sentence(&subject);
    assert_eq!(
        (
            answer.status,
            errortype(answer),
            fault_class(answer),
            sentence(answer),
        ),
        (
            404,
            "ResourceNotFoundException".to_string(),
            Some(USER_FAULT.to_string()),
            expected.clone(),
        ),
        "case {case:?} at {}: expected 404 ResourceNotFoundException / {USER_FAULT} / \
         {expected:?}, got {} {}",
        operation.operation,
        answer.status,
        text(answer),
    );
}

/// The provider NAME a reference carries: a capacity-provider ARN's last segment, or the
/// bare name as given.
fn trimmed_name(reference: &str) -> &str {
    match reference.rsplit_once(':') {
        Some((_, name)) if reference.starts_with("arn:") => name,
        _ => reference,
    }
}

#[tokio::test]
async fn every_operation_refuses_a_provider_that_does_not_exist_with_the_same_envelope() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    // A provider that DOES exist, so every refusal below is about the provider the request
    // named and not about the operation being broken.
    let created = create_provider(&router, "cp-envelope-live", OPERATOR_ROLE).await;
    assert_eq!(
        created.status,
        202,
        "CreateCapacityProvider must answer 202, got {} {}",
        created.status,
        text(&created),
    );
    make_activation_due(&engine);

    // Never created, addressed by each documented `CapacityProviderName` form.
    for operation in provider_operations() {
        for (form, reference) in [
            ("a bare name", "cp-envelope-absent".to_string()),
            ("the provider ARN", provider_arn("cp-envelope-ghost")),
        ] {
            let answer = call_operation(&router, &operation, &reference).await;
            assert_not_found(
                &format!("a provider never created, addressed by {form}"),
                &operation,
                &reference,
                &answer,
            );
        }
    }

    // The complement: the provider that exists is SERVED by every one of them, and a served
    // answer names no class of fault. The deletion goes last, since it is what starts the
    // removal the rows after this wait on.
    for operation in provider_operations() {
        let answer = call_operation(&router, &operation, "cp-envelope-live").await;
        assert_eq!(
            (answer.status, fault_class(&answer)),
            (operation.served_status, None),
            "case a provider that exists at {}: expected HTTP {} and no fault class, got {} {}",
            operation.operation,
            operation.served_status,
            answer.status,
            text(&answer),
        );
        assert!(
            answer.header("x-amzn-errortype").is_none(),
            "case a provider that exists at {}: a served answer names no error shape, got {:?}",
            operation.operation,
            answer.header("x-amzn-errortype"),
        );
    }

    // And a provider that DID exist until the clock made its removal due is refused exactly
    // as one that never existed: the lookup is of the store, not of the name's shape.
    make_removal_due(&engine);
    for operation in provider_operations() {
        let answer = call_operation(&router, &operation, "cp-envelope-live").await;
        assert_not_found(
            "a provider whose removal the clock made due",
            &operation,
            "cp-envelope-live",
            &answer,
        );
    }
}

/// One create row: the role the request names, and the envelope the create answers.
struct CreateCase {
    case: &'static str,
    role_arn: String,
    status: u16,
    code: &'static str,
    sentence: &'static str,
}

#[tokio::test]
async fn a_create_naming_an_unusable_operator_role_is_refused_with_the_user_class() {
    let (router, engine) = router_with_iam();
    let held = given_role(
        &router,
        "a role this service may manage compute as",
        "capacity-envelope-operator",
        &trust("Allow", LAMBDA_PRINCIPAL),
    )
    .await;

    // The control first: the same create, naming a role this account's IAM holds and whose
    // trust policy admits this service, is served and names no class of fault.
    let served = create_provider(&router, "cp-envelope-held", &held).await;
    assert_eq!(
        (served.status, fault_class(&served)),
        (202, None),
        "case a role this service may assume: expected HTTP 202 and no fault class, got {} {}",
        served.status,
        text(&served),
    );
    assert!(
        holds_provider(&engine, "cp-envelope-held"),
        "case a role this service may assume: a served create must persist the provider",
    );

    // A second create of a name this account already holds is refused too, and its envelope
    // follows the same rule — a case this surface's own sentence rules cover elsewhere.
    let duplicate = create_provider(&router, "cp-envelope-held", &held).await;
    assert_eq!(
        (
            duplicate.status,
            errortype(&duplicate),
            fault_class(&duplicate)
        ),
        (
            409,
            "ResourceConflictException".to_string(),
            Some(USER_FAULT.to_string())
        ),
        "case a name this account already holds: expected 409 ResourceConflictException / \
         {USER_FAULT}, got {} {}",
        duplicate.status,
        text(&duplicate),
    );

    let rows = [
        CreateCase {
            case: "a role this account's IAM holds none of",
            role_arn: "arn:aws:iam::000000000000:role/capacity-envelope-never-created".to_string(),
            status: 400,
            code: "InvalidParameterValueException",
            sentence: OPERATOR_ROLE_INVALID_MESSAGE,
        },
        CreateCase {
            case: "a well-formed role ARN of another account",
            role_arn: "arn:aws:iam::123456789012:role/capacity-envelope-operator".to_string(),
            status: 403,
            code: "AccessDeniedException",
            sentence: CROSS_ACCOUNT_PASS_ROLE_MESSAGE,
        },
        CreateCase {
            case: "another account's role under a path in another partition",
            role_arn: "arn:aws-us-gov:iam::123456789012:role/service-role/capacity-envelope"
                .to_string(),
            status: 403,
            code: "AccessDeniedException",
            sentence: CROSS_ACCOUNT_PASS_ROLE_MESSAGE,
        },
    ];

    for (index, row) in rows.iter().enumerate() {

        let name = format!("cp-envelope-refused-{index}");
        let answer = create_provider(&router, &name, &row.role_arn).await;
        assert_eq!(
            (
                answer.status,
                errortype(&answer),
                fault_class(&answer),
                sentence(&answer),
            ),
            (
                row.status,
                row.code.to_string(),
                Some(USER_FAULT.to_string()),
                row.sentence.to_string(),
            ),
            "case {:?}: expected HTTP {} {} / {USER_FAULT} / {:?}, got {} {}",
            row.case,
            row.status,
            row.code,
            row.sentence,
            answer.status,
            text(&answer),
        );
        assert!(
            !holds_provider(&engine, &name),
            "case {:?}: a refused create must persist nothing",
            row.case,
        );
    }
}
