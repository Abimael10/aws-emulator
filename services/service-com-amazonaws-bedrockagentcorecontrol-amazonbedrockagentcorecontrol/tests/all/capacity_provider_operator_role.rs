// SPDX-License-Identifier: CC0-1.0

//! The OPERATOR ROLE a capacity provider is managed as, judged against the IAM records of
//! the account the create was made in, and the sentence a provider that is not there is
//! reported missing in.
//!
//! RULE 1: `CreateCapacityProvider` resolves
//! `permissionsConfiguration.capacityProviderOperatorRoleArn` against this account's IAM
//! BEFORE it mints anything, and is refused the modelled `ValidationException` (HTTP 400,
//! `reason` `FieldValidationFailed`), persisting nothing, when IAM ANSWERED that the role is
//! one this service cannot assume — this account's IAM holds no role under the name the ARN
//! carries, or the trust policy of the role it does hold does not let this service assume it.
//! The provider is MANAGED as that role: it is what the service assumes "to manage the
//! capacity provider, including launching, tagging, and terminating instances and their
//! network interfaces" (`smithy_query(query="doc",shape="PermissionsConfiguration",
//! member="capacityProviderOperatorRoleArn")`). Wherever nothing is KNOWN about a role — an
//! ARN that names no role of THIS account, an emulator with no IAM mounted to ask at all —
//! the create is SERVED, because a refusal is grounded in what IAM answered.
//!
//! RULE 2: a capacity provider this account does not hold is reported missing in this
//! backend's own sentence, "No capacity provider found with id " around the
//! `capacityProviderId` the request's path label carried, on the modelled
//! `ResourceNotFoundException` (HTTP 404).
//!
//! The rows differ along what the rules generalise over: whether this account's IAM holds the
//! role at all, what its trust policy says (this service admitted, admitted through a
//! wildcard action, another principal admitted, the grant denied, no document at all), the
//! path IAM holds the role under, whose account the ARN names, and whether the ARN carries an
//! account field at all — which the member's own pattern makes optional
//! (`^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`,
//! `smithy_query(query="shape",shape="RoleArn")`).
//!
//! Sources: `smithy_query(query="operation",operation="CreateCapacityProvider,
//! GetCapacityProvider,ListCapacityProviders")` for every member, status and error asserted
//! here, and `smithy_query(query="shape",shape="ValidationException,ValidationExceptionField,
//! ComputeConfiguration")` for the refusal's shape and for the compute union the attributed
//! member path runs through.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// The service principal an AgentCore operator role's trust policy has to admit — the
/// `cloudTrailEventSource` of this service's own model
/// (`smithy_query(query="service_info")`).
const SERVICE_PRINCIPAL: &str = "bedrock-agentcore.amazonaws.com";

/// The whole sentence AWS answers a create whose operator role it could not assume with. The
/// text is FIXED for this error class: it names no role, account or other request value.
const OPERATOR_ROLE_REFUSAL: &str =
    "Role validation failed for the operator role. Please verify that the role exists and \
     its trust policy allows assumption by this service";

/// `ValidationExceptionReason` for a member of the request that failed validation.
const FIELD_VALIDATION_FAILED: &str = "FieldValidationFailed";

/// The member the refusal attributes the failure to: the subnets the operator role has to
/// launch the instances and their network interfaces in, under the compute variant the
/// request carried, as the JSON pointer into the request body this `/capacity-providers`
/// backend addresses a member with.
const ATTRIBUTED_FIELD: &str = "/computeConfiguration/ec2Configuration/vpcConfiguration/subnets";

/// This service alone, on its own engine: nothing can be asked about any role, which is the
/// complement the second test pins.
fn router_without_iam() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// This service over the SAME engine as IAM, so a create resolves the operator role in the
/// service that owns it — exactly what `core/router` assembles.
fn router_with_iam() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (mut router, engine) = router_without_iam();
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    (router, engine)
}

fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One call to this service: the status, the wire error code and the parsed body.
async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, String, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header(
            "host",
            format!("bedrock-agentcore-control.{REGION}.amazonaws.com"),
        );
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, header_code, parsed)
}

async fn create_provider(
    router: &MultiServiceRouter,
    name: &str,
    operator_role_arn: &str,
) -> (u16, String, Value) {
    let body = json!({
        "name": name,
        "permissionsConfiguration": {"capacityProviderOperatorRoleArn": operator_role_arn},
        "computeConfiguration": compute_configuration(),
    });
    send(router, "PUT", "/capacity-providers", Some(&body)).await
}

async fn get_provider(router: &MultiServiceRouter, id: &str) -> (u16, String, Value) {
    send(router, "GET", &format!("/capacity-providers/{id}"), None).await
}

/// Every provider name the store holds, which is what says whether a refused create persisted
/// anything.
async fn listed_names(router: &MultiServiceRouter) -> Vec<String> {
    let (_, _, listed) = send(router, "POST", "/capacity-providers", None).await;
    listed
        .get("capacityProviders")
        .and_then(Value::as_array)
        .map(|summaries| {
            summaries
                .iter()
                .map(|summary| text(summary, "name"))
                .collect()
        })
        .unwrap_or_default()
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
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The compute configuration every row creates its provider with: the one modelled
/// `ComputeConfiguration` variant, carrying the required launch template source and VPC
/// configuration, every value inside its own modelled constraint.
fn compute_configuration() -> Value {
    json!({
        "ec2Configuration": {
            "launchTemplateSource": {
                "launchParameters": {
                    "operatingSystem": "LINUX_X86_64",
                    "instanceRequirements": {"allowedInstanceTypes": ["m5.large"]}
                }
            },
            "vpcConfiguration": {
                "subnets": ["subnet-0123456789abcdef0"],
                "securityGroups": ["sg-0123456789abcdef0"]
            }
        }
    })
}

/// A trust policy with one statement: `effect` on `action` for the service `principal`.
fn trust(effect: &str, principal: &str, action: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": {"Service": principal},
            "Action": action,
        }],
    })
}

/// Record one role in IAM's own store, under `path`, carrying `trust` (`None` for a role with
/// no trust document at all), and answer the ARN IAM holds it under.
fn given_role(
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
    path: &str,
    document: Option<&Value>,
) -> String {
    let arn = format!(
        "arn:aws:iam::{}:role{path}{name}",
        ::engine::emulator_account_id()
    );
    let store = iam::service_state(&**engine).expect("the IAM store of this engine");
    store.lock().roles.insert(
        iam::name_key(name),
        Role {
            path: path.to_string(),
            role_name: name.to_string(),
            role_id: format!("AROAPROBE{name}"),
            arn: arn.clone(),
            create_date: ::aws_smithy_types::DateTime::from_secs(0),
            assume_role_policy_document: document.map(::std::string::ToString::to_string),
            description: None,
            max_session_duration: None,
            permissions_boundary: None,
            tags: None,
            role_last_used: None,
            source_role_template: None,
        },
    );
    arn
}

/// What one row says about the operator role its create names.
enum OperatorRole {
    /// This account's IAM holds the role, under this path, with this trust document.
    Held {
        path: &'static str,
        document: Option<Value>,
    },
    /// The ARN names a role of this account that IAM holds nothing under.
    AbsentFromIam,
    /// The ARN carries NO account field — which `RoleArn`'s pattern admits — and names a role
    /// IAM holds nothing under.
    AccountlessAbsent,
    /// The ARN carries no account field and names a role IAM HOLDS, with a trust policy that
    /// admits this service.
    AccountlessHeld,
    /// A well-formed role ARN of another account, whose IAM this emulator cannot ask.
    AnotherAccount,
}

/// The ARN one row's create names, recording in IAM whatever the row says IAM holds.
fn operator_role_arn(
    engine: &Arc<engine::InMemoryEngine>,
    index: usize,
    role: &OperatorRole,
) -> String {
    let name = format!("capacityOperator{index}");
    match role {
        OperatorRole::Held { path, document } => given_role(engine, &name, path, document.as_ref()),
        OperatorRole::AbsentFromIam => format!(
            "arn:aws:iam::{}:role/{name}",
            ::engine::emulator_account_id()
        ),
        OperatorRole::AccountlessAbsent => format!("arn:aws:iam:::role/{name}"),
        OperatorRole::AccountlessHeld => {
            given_role(
                engine,
                &name,
                "/",
                Some(&trust("Allow", SERVICE_PRINCIPAL, "sts:AssumeRole")),
            );
            format!("arn:aws:iam:::role/{name}")
        }
        OperatorRole::AnotherAccount => format!("arn:aws:iam::123456789012:role/{name}"),
    }
}

/// RULE 1: an operator role IAM ANSWERED about and this service cannot assume is refused the
/// modelled 400 with nothing persisted; a role nothing is known against is served, and the
/// provider is managed with the role the request named.
#[tokio::test]
async fn create_capacity_provider_judges_the_operator_role_iam_answered_about() {
    // (case, the role the create names, whether the create is served)
    let rows: Vec<(&str, OperatorRole, bool)> = vec![
        (
            "a held role whose trust policy admits this service",
            OperatorRole::Held {
                path: "/",
                document: Some(trust("Allow", SERVICE_PRINCIPAL, "sts:AssumeRole")),
            },
            true,
        ),
        (
            "a held role admitting this service through a wildcard action",
            OperatorRole::Held {
                path: "/",
                document: Some(trust("Allow", SERVICE_PRINCIPAL, "sts:*")),
            },
            true,
        ),
        (
            "a held role under a path, admitting this service",
            OperatorRole::Held {
                path: "/service-role/",
                document: Some(trust("Allow", SERVICE_PRINCIPAL, "sts:AssumeRole")),
            },
            true,
        ),
        (
            "a held role whose trust policy admits another service",
            OperatorRole::Held {
                path: "/",
                document: Some(trust("Allow", "ecs-tasks.amazonaws.com", "sts:AssumeRole")),
            },
            false,
        ),
        (
            "a held role whose trust policy DENIES this service",
            OperatorRole::Held {
                path: "/",
                document: Some(trust("Deny", SERVICE_PRINCIPAL, "sts:AssumeRole")),
            },
            false,
        ),
        (
            "a held role carrying no trust document at all",
            OperatorRole::Held {
                path: "/",
                document: None,
            },
            false,
        ),
        (
            "a role this account's IAM holds nothing under",
            OperatorRole::AbsentFromIam,
            false,
        ),
        (
            // An ARN with no account field names no OTHER account, so the role it names is
            // one this emulator's IAM can be asked about.
            "an accountless ARN naming a role IAM holds nothing under",
            OperatorRole::AccountlessAbsent,
            false,
        ),
        (
            "an accountless ARN naming a held role that admits this service",
            OperatorRole::AccountlessHeld,
            true,
        ),
        (
            // Another account's IAM is not this emulator's to ask, so nothing was answered
            // about the role and the create is served.
            "a well-formed role ARN of another account",
            OperatorRole::AnotherAccount,
            true,
        ),
    ];

    for (index, (case, role, served)) in rows.into_iter().enumerate() {
        let (router, engine) = router_with_iam();
        let role_arn = operator_role_arn(&engine, index, &role);
        let name = format!("operatorProbe{index}");
        let (status, header_code, answered) = create_provider(&router, &name, &role_arn).await;

        if served {
            assert_eq!(
                status, 202,
                "case {case:?}: expected HTTP 202 from the create, got {status} ({answered})"
            );
            let id = text(&answered, "capacityProviderId");
            let (read_status, _, read) = get_provider(&router, &id).await;
            assert_eq!(
                read_status, 200,
                "case {case:?}: the created provider reads back, got {read_status} ({read})"
            );
            assert_eq!(
                read["permissionsConfiguration"]["capacityProviderOperatorRoleArn"].as_str(),
                Some(role_arn.as_str()),
                "case {case:?}: the provider is managed with the role the request named, got \
                 {read}"
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&answered).as_str(), header_code.as_str()),
            (400, "ValidationException", "ValidationException"),
            "case {case:?}: expected HTTP 400 ValidationException on the body and the \
             x-amzn-errortype header, got {status} ({answered})"
        );
        assert_eq!(
            (text(&answered, "message"), text(&answered, "reason")),
            (
                OPERATOR_ROLE_REFUSAL.to_string(),
                FIELD_VALIDATION_FAILED.to_string()
            ),
            "case {case:?}: got {answered}"
        );
        assert_eq!(
            answered["fieldList"],
            json!([{"name": ATTRIBUTED_FIELD, "message": OPERATOR_ROLE_REFUSAL}]),
            "case {case:?}: the refusal attributes the failure to one field, got {answered}"
        );
        assert!(
            !answered.to_string().contains(&role_arn),
            "case {case:?}: the refusal names no role of the request, got {answered}"
        );
        assert_eq!(
            listed_names(&router).await,
            Vec::<String>::new(),
            "case {case:?}: a refused create persists no provider"
        );
    }
}

/// The complement of RULE 1: with no IAM mounted there is nothing to ask about any role, so
/// every create is served — including the ones an IAM that holds nothing refuses.
#[tokio::test]
async fn an_emulator_with_no_iam_mounted_refuses_no_operator_role() {
    // (case, the role ARN the create names) — each one an IAM-mounted emulator refuses.
    let roles: [(&str, String); 3] = [
        (
            "a role of this account no IAM can be asked about",
            format!(
                "arn:aws:iam::{}:role/capacityOperatorNeverCreated",
                ::engine::emulator_account_id()
            ),
        ),
        (
            "an accountless role ARN with no IAM mounted",
            "arn:aws:iam:::role/capacityOperatorNeverCreated".to_string(),
        ),
        (
            "another account's role with no IAM mounted",
            "arn:aws:iam::123456789012:role/capacityOperator".to_string(),
        ),
    ];

    for (index, (case, role_arn)) in roles.into_iter().enumerate() {
        let (router, _engine) = router_without_iam();
        let name = format!("noIamProbe{index}");
        let (status, _, answered) = create_provider(&router, &name, &role_arn).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 with no IAM mounted, got {status} ({answered})"
        );
        assert_eq!(
            listed_names(&router).await,
            vec![name.clone()],
            "case {case:?}: the served create persists the provider"
        );
    }
}

/// RULE 2: a `capacityProviderId` naming no provider of this account is the modelled 404 in
/// this backend's own sentence, around the id the request's path label carried; the id a
/// create minted is still served.
#[tokio::test]
async fn a_capacity_provider_that_is_not_there_is_reported_missing_by_its_id() {
    let (router, _engine) = router_without_iam();
    let (status, _, created) = create_provider(
        &router,
        "heldProvider",
        "arn:aws:iam:::role/capacityOperatorHeld",
    )
    .await;
    assert_eq!(
        status, 202,
        "case \"the create\": expected HTTP 202, got {status} ({created})"
    );
    let held = text(&created, "capacityProviderId");

    // (case, the id the read names) — each inside `CapacityProviderId`'s own format
    // `^[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$`, @length 12..59.
    let rows: [(&str, String); 4] = [
        (
            "a stem this account never created",
            "otherProvider-abcdefghij".to_string(),
        ),
        (
            "the held provider's stem with another suffix",
            format!("heldProvider-{}", "z".repeat(10)),
        ),
        (
            "an id at the member's minimum length",
            "a-bcdefghijk".to_string(),
        ),
        (
            "an id at the member's maximum length, with underscores and digits",
            format!("p{}_9-{}", "x".repeat(44), "A1b2C3d4E5"),
        ),
    ];

    for (case, id) in rows {
        let (status, header_code, refused) = get_provider(&router, &id).await;
        assert_eq!(
            (status, error_code(&refused).as_str(), header_code.as_str()),
            (404, "ResourceNotFoundException", "ResourceNotFoundException"),
            "case {case:?} ({id}): expected HTTP 404 ResourceNotFoundException, got {status} \
             ({refused})"
        );
        assert_eq!(
            text(&refused, "message"),
            format!("No capacity provider found with id {id}"),
            "case {case:?}: got {refused}"
        );
    }

    let (status, _, fetched) = get_provider(&router, &held).await;
    assert_eq!(
        status, 200,
        "case \"the held provider\": the id the create minted is still served, got {status} \
         ({fetched})"
    );
}
