// SPDX-License-Identifier: CC0-1.0

//! The SERVICE EXECUTION ROLE an application runs as, judged against the IAM records of the
//! account the request was made in.
//!
//! RULE: a write that puts an application on a service execution role — `CreateApplication`,
//! and an `UpdateApplication` that names `ServiceExecutionRoleUpdate` — is refused with the
//! modelled `InvalidArgumentException` (HTTP 400), persisting nothing, when IAM ANSWERED that
//! the role this service would have to assume is one it cannot: this account's IAM holds no
//! role under the name the ARN carries, or the trust policy of the role it does hold does not
//! admit this service principal. The refusal names the role the request carried, and nothing
//! else of the request.
//!
//! The complement: wherever nothing is KNOWN about the role the request names, the write is
//! SERVED — a role ARN of another account (another account's IAM is not this emulator's to
//! ask) and an emulator with no IAM mounted to ask at all, which the last test pins.
//!
//! The rows differ along what the rule generalises over: whether this account's IAM holds the
//! role, what its trust policy says (this service admitted on its own and in a list, another
//! service admitted, the assume denied), where the role lives (the default path and a path),
//! whose account the ARN names, and both operations that put an application on a role.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{call, create_body, describe, error_code, error_message, router_on, ACCOUNT};

/// The service principal a service execution role has to let assume it: this service is what
/// assumes the role an application runs as.
const SERVICE_PRINCIPAL: &str = "kinesisanalytics.amazonaws.com";

/// The runtime every application below runs on; which runtime it is decides nothing about the
/// role.
const RUNTIME: &str = "FLINK-1_20";

/// The whole `Message` a role this service cannot assume is refused with. Written out here,
/// so this suite pins the wording rather than reading the service's own constant.
fn unassumable_message(role_arn: &str) -> String {
    format!(
        "Kinesis Analytics service doesn't have sufficient privileges to assume the role: \
         {role_arn}. Please check the role provided."
    )
}

/// Percent-encode one component of an `application/x-www-form-urlencoded` body, which is how
/// IAM's `awsQuery` parameters travel. The maintained `percent-encoding` crate, a
/// dev-dependency of this crate: `NON_ALPHANUMERIC` escapes every byte outside `[A-Za-z0-9]`,
/// a superset of what a form component must escape, and an over-escaped component decodes to
/// exactly the same bytes.
fn form_encode(value: &str) -> String {
    percent_encoding::percent_encode(value.as_bytes(), percent_encoding::NON_ALPHANUMERIC)
        .to_string()
}

/// This service over the SAME engine as IAM, so a write resolves the role in the service that
/// owns it — exactly what `core/router` assembles.
fn router_with_iam() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(Arc::clone(&engine));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A trust policy with one statement: `effect` on `action` for `service`, each given as the
/// JSON it travels as (a string or a list), which is the shape variation AWS accepts.
fn trust_policy(effect: &str, action: Value, service: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": {"Service": service},
            "Action": action,
        }],
    })
}

/// Create one role through IAM's own `CreateRole`.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    path: Option<&str>,
    trust: &Value,
) {
    let mut body = format!(
        "Action=CreateRole&Version=2010-05-08&RoleName={}&AssumeRolePolicyDocument={}",
        form_encode(name),
        form_encode(&trust.to_string()),
    );
    if let Some(path) = path {
        body.push_str(&format!("&Path={}", form_encode(path)));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let xml = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: the role must be created in IAM first, got {status} {xml}"
    );
}

/// The ARN of a role of this account, under `path` (`""` for the default path).
fn role_arn(path: &str, name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{path}{name}")
}

/// Write every role the table names into IAM.
async fn given_the_roles(router: &MultiServiceRouter) {
    given_role(
        router,
        "setup",
        "kda-service-role",
        None,
        &trust_policy("Allow", json!("sts:AssumeRole"), json!(SERVICE_PRINCIPAL)),
    )
    .await;
    given_role(
        router,
        "setup",
        "kda-listed-role",
        None,
        &trust_policy(
            "Allow",
            json!(["sts:TagSession", "sts:AssumeRole"]),
            json!(["lambda.amazonaws.com", SERVICE_PRINCIPAL]),
        ),
    )
    .await;
    given_role(
        router,
        "setup",
        "kda-path-role",
        Some("/service-role/"),
        &trust_policy("Allow", json!("sts:AssumeRole"), json!(SERVICE_PRINCIPAL)),
    )
    .await;
    given_role(
        router,
        "setup",
        "lambda-only-role",
        None,
        &trust_policy(
            "Allow",
            json!("sts:AssumeRole"),
            json!("lambda.amazonaws.com"),
        ),
    )
    .await;
    given_role(
        router,
        "setup",
        "denied-role",
        None,
        &trust_policy("Deny", json!("sts:AssumeRole"), json!(SERVICE_PRINCIPAL)),
    )
    .await;
}

/// One row: the role a create names and whether this service can assume it.
struct RoleCase {
    case: &'static str,
    role_arn: String,
    accepted: bool,
}

fn role_cases() -> Vec<RoleCase> {
    vec![
        RoleCase {
            case: "a role IAM holds that trusts this service",
            role_arn: role_arn("", "kda-service-role"),
            accepted: true,
        },
        RoleCase {
            case: "a trust policy whose Action and Service are lists",
            role_arn: role_arn("", "kda-listed-role"),
            accepted: true,
        },
        RoleCase {
            case: "a role under a path",
            role_arn: role_arn("service-role/", "kda-path-role"),
            accepted: true,
        },
        RoleCase {
            // Nothing is known about another account's IAM, so such a value is served.
            case: "a well-formed role ARN of another account",
            role_arn: "arn:aws:iam::123456789012:role/kda-service-role".to_string(),
            accepted: true,
        },
        RoleCase {
            case: "a role of this account IAM holds nothing under",
            role_arn: role_arn("", "never-created-role"),
            accepted: false,
        },
        RoleCase {
            case: "a role that trusts another service only",
            role_arn: role_arn("", "lambda-only-role"),
            accepted: false,
        },
        RoleCase {
            case: "a role whose statement denies the assume",
            role_arn: role_arn("", "denied-role"),
            accepted: false,
        },
    ]
}

/// The `ServiceExecutionRole` a described application reports.
fn described_role(detail: &Value) -> Option<&str> {
    detail["ServiceExecutionRole"].as_str()
}

/// A create is served only for a role this service can be shown to assume, and a refused one
/// stores no application at all.
#[tokio::test]
async fn an_application_is_created_only_for_a_role_this_service_may_assume() {
    let router = router_with_iam();
    given_the_roles(&router).await;

    for (index, case) in role_cases().into_iter().enumerate() {
        let name = format!("role-gate-{index}");
        let mut body = create_body(&name, RUNTIME);
        body["ServiceExecutionRole"] = json!(case.role_arn);
        let (status, answered) = call(&router, "CreateApplication", &body).await;

        if case.accepted {
            assert_eq!(
                status, 200,
                "case {:?}: expected the create to be accepted, got {status} {answered}",
                case.case
            );
            // Round trip: the stored application runs as the role the create named.
            let detail = describe(&router, &name).await;
            assert_eq!(
                described_role(&detail),
                Some(case.role_arn.as_str()),
                "case {:?}: expected the stored role to be reported, got {detail}",
                case.case
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "InvalidArgumentException"),
            "case {:?}: expected 400 InvalidArgumentException, got {status} {answered}",
            case.case
        );
        assert_eq!(
            error_message(&answered),
            unassumable_message(&case.role_arn),
            "case {:?}: expected the unassumable-role refusal, got {answered}",
            case.case
        );
        let (status, read) = call(
            &router,
            "DescribeApplication",
            &json!({"ApplicationName": name}),
        )
        .await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (400, "ResourceNotFoundException"),
            "case {:?}: a refused create must store no application, got {status} {read}",
            case.case
        );
    }
}

/// An update applies the same gate to the role it would move the application onto, and a
/// refused update leaves the application on the role it already runs as.
#[tokio::test]
async fn an_update_moves_an_application_only_onto_a_role_this_service_may_assume() {
    let router = router_with_iam();
    given_the_roles(&router).await;

    let name = "role-gate-update";
    let first = role_arn("", "kda-service-role");
    let mut body = create_body(name, RUNTIME);
    body["ServiceExecutionRole"] = json!(first);
    let (status, created) = call(&router, "CreateApplication", &body).await;
    assert_eq!(status, 200, "CreateApplication got {status} {created}");

    // (case, the role the update names, whether the update is accepted)
    let rows: [(&str, String, bool); 3] = [
        (
            "an update onto a role IAM holds nothing under",
            role_arn("", "never-created-role"),
            false,
        ),
        (
            "an update onto a role that trusts another service only",
            role_arn("", "lambda-only-role"),
            false,
        ),
        (
            "an update onto a role IAM holds that trusts this service",
            role_arn("", "kda-listed-role"),
            true,
        ),
    ];

    for (case, role, accepted) in rows {
        let (status, answered) = call(
            &router,
            "UpdateApplication",
            &json!({
                "ApplicationName": name,
                "CurrentApplicationVersionId": 1,
                "ServiceExecutionRoleUpdate": role,
            }),
        )
        .await;

        if accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the update to be accepted, got {status} {answered}"
            );
            let detail = describe(&router, name).await;
            assert_eq!(
                described_role(&detail),
                Some(role.as_str()),
                "case {case:?}: expected the application to run as the named role, got {detail}"
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "InvalidArgumentException"),
            "case {case:?}: expected 400 InvalidArgumentException, got {status} {answered}"
        );
        assert_eq!(
            error_message(&answered),
            unassumable_message(&role),
            "case {case:?}: expected the unassumable-role refusal, got {answered}"
        );
        let detail = describe(&router, name).await;
        assert_eq!(
            (described_role(&detail), detail["ApplicationVersionId"].as_i64()),
            (Some(first.as_str()), Some(1)),
            "case {case:?}: a refused update must change neither the role nor the version, \
             got {detail}"
        );
    }
}

/// The complement: with no IAM mounted nothing is known about any role, so a create naming a
/// role no IAM holds is SERVED rather than refused.
#[tokio::test]
async fn with_no_iam_to_ask_every_well_formed_role_is_served() {
    let router = router_on(Arc::new(engine::InMemoryEngine::new()));
    for (index, role) in [
        role_arn("", "never-created-role"),
        role_arn("service-role/", "never-created-role"),
        "arn:aws:iam::123456789012:role/kda-service-role".to_string(),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("no-iam-{index}");
        let mut body = create_body(&name, RUNTIME);
        body["ServiceExecutionRole"] = json!(role);
        let (status, answered) = call(&router, "CreateApplication", &body).await;
        assert_eq!(
            status, 200,
            "case {role:?}: expected the create to be served with no IAM to ask, got {status} \
             {answered}"
        );
        let detail = describe(&router, &name).await;
        assert_eq!(
            described_role(&detail),
            Some(role.as_str()),
            "case {role:?}: expected the stored role to be reported, got {detail}"
        );
    }
}
