// SPDX-License-Identifier: CC0-1.0

//! One organization, two services: the management account Firewall Manager
//! names when it refuses a management-account-only call is the account AWS
//! Organizations reports for the organization.
//!
//! Firewall Manager refuses a call that only the organization's management
//! account can perform — the operations that change or enumerate the
//! organization's Firewall Manager administration:
//! `ListAdminAccountsForOrganization` ("This operation can be called only from
//! the organization's management account") and `PutAdminAccount` ("Only the
//! organization's management account can create an Firewall Manager
//! administrator account") — with a message that NAMES that management
//! account, so a client learns which account to re-issue the call from. The
//! account it names must be the one the organization itself reports, which is to
//! say `Organization$MasterAccountId` from `organizations:DescribeOrganization`:
//! only a cross-service suite can pin that, because each crate's own tests can
//! assert no more than its own half.
//!
//! The rows vary what the rule generalises over: the refusing operation (both of
//! them), the form of its payload, and the Region the request is signed for —
//! none of which changes the account the refusal names.
//!
//! `GetAdminScope`, the read side of what `PutAdminAccount` writes, is NOT in
//! that class, and is asserted here as the contrast: it answers from its own
//! lookup of the account it names, so it reports the modelled
//! `ResourceNotFoundException` while the organization holds no administration
//! rather than naming the management account.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header whose credential scope names `service` and
/// `region`. The signature is never checked: the router here runs with request
/// authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0"
    )
}

/// The emulator with Firewall Manager and Organizations mounted on ONE engine,
/// as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_fms_awsfms_20180101::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_organizations_awsorganizationsv20161128::EmulatedService::new(engine),
    );
    router
}

/// One awsJson1_1 call, by target prefix and signing name, returning
/// `(http status, parsed body)`.
async fn call(
    router: &MultiServiceRouter,
    service: &str,
    target_prefix: &str,
    operation: &str,
    region: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(service, region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The account AWS Organizations reports as the management account of the
/// organization this account belongs to.
///
/// The organization is created first, through Organizations' own
/// `CreateOrganization`: membership is established by a REQUEST — "the account whose
/// user is calling the CreateOrganization operation automatically becomes the
/// management account of the new organization" — and not by mounting that control
/// plane, so until a client makes that request the caller belongs to none.
async fn management_account(router: &MultiServiceRouter) -> String {
    let (status, body) = call(
        router,
        "organizations",
        "AWSOrganizationsV20161128",
        "CreateOrganization",
        "us-east-1",
        &json!({}),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateOrganization must establish the organization, got {status} with {body}"
    );
    let (status, body) = call(
        router,
        "organizations",
        "AWSOrganizationsV20161128",
        "DescribeOrganization",
        "us-east-1",
        &json!({}),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeOrganization must report the organization, got {status} with {body}"
    );
    body["Organization"]["MasterAccountId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The modelled error code an awsJson1_1 error envelope carries in `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Every Firewall Manager call that only the organization's management account
/// can perform, with a payload whose form differs row by row.
fn management_account_only_calls() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "the enumeration with no members at all",
            "ListAdminAccountsForOrganization",
            json!({}),
        ),
        (
            "the enumeration with a page bound",
            "ListAdminAccountsForOrganization",
            json!({ "MaxResults": 5 }),
        ),
        (
            "an administrator write",
            "PutAdminAccount",
            json!({
                "AdminAccount": "222222222222",
                "AdminScope": { "AccountScope": { "AllAccountsEnabled": true } },
            }),
        ),
    ]
}

/// Account ids `GetAdminScope` is read for below, spanning the forms the model's
/// `AWSAccountId` admits (`^[0-9]+$`, length 1..1024): a 12-digit one and the
/// shortest well-formed one.
const UNADMINISTERED_ACCOUNTS: &[&str] = &["123456789012", "7"];

/// With the organization not onboarded to Firewall Manager, each
/// management-account-only call is refused with the modelled
/// `InvalidOperationException`, and the account the refusal names as the
/// organization's management account is exactly the one Organizations reports —
/// whichever operation refuses, whatever its payload, and whichever Region the
/// request is signed for.
#[tokio::test]
async fn the_firewall_manager_refusal_names_the_management_account_organizations_reports() {
    let router = build_router();
    let management = management_account(&router).await;
    assert!(
        !management.is_empty(),
        "Organizations must report a management account for the organization"
    );

    for region in ["us-west-2", "eu-central-1"] {
        for (case, operation, request) in management_account_only_calls() {
            let (status, body) = call(
                &router,
                "fms",
                "AWSFMS_20180101",
                operation,
                region,
                &request,
            )
            .await;
            let code = error_code(&body);
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidOperationException"),
                "case {case:?} in {region}: expected the modelled InvalidOperationException, got \
                 {status} / {code:?} with {body}"
            );
            let message = body["Message"].as_str().unwrap_or_default().to_string();
            assert!(
                message.ends_with(&format!(
                    "The Organization management account is {management}"
                )),
                "case {case:?} in {region}: the refusal must name the management account \
                 Organizations reports ({management}), got {message:?}"
            );
        }
    }
}

/// The contrast, on the same organization: the scope read is answered from its
/// own lookup of the account it names, so an organization that holds no Firewall
/// Manager administration answers the modelled `ResourceNotFoundException` for
/// it — never the management-account refusal, which would name an account
/// instead of reporting what the identifier resolves to.
#[tokio::test]
async fn the_scope_read_answers_its_own_not_found_for_an_unadministered_organization() {
    let router = build_router();
    let management = management_account(&router).await;

    for region in ["us-west-2", "eu-central-1"] {
        let mut accounts: Vec<&str> = UNADMINISTERED_ACCOUNTS.to_vec();
        // The organization's own management account too: it administers the
        // organization, but it is no Firewall Manager administrator until one is
        // associated, which is what this read reports on.
        accounts.push(management.as_str());
        for account in accounts {
            let (status, body) = call(
                &router,
                "fms",
                "AWSFMS_20180101",
                "GetAdminScope",
                region,
                &json!({ "AdminAccount": account }),
            )
            .await;
            let code = error_code(&body);
            assert_eq!(
                (status, code.as_str()),
                (400, "ResourceNotFoundException"),
                "case {account} in {region}: an account that is no administrator is this read's \
                 own not-found, got {status} / {code:?} with {body}"
            );
            let message = body["Message"].as_str().unwrap_or_default().to_string();
            assert!(
                !message.contains(&management),
                "case {account} in {region}: the not-found text is the service-wide constant, so \
                 it names no account, got {message:?}"
            );
        }
    }
}

/// Once the organization IS onboarded to Firewall Manager, the same calls are
/// answered from Firewall Manager's own state instead — so the refusal above is
/// about the organization's administration, not about the caller's credentials.
#[tokio::test]
async fn an_onboarded_organization_answers_the_same_calls_from_firewall_manager_state() {
    let router = build_router();
    let management = management_account(&router).await;

    let (status, body) = call(
        &router,
        "fms",
        "AWSFMS_20180101",
        "AssociateAdminAccount",
        "us-west-2",
        &json!({ "AdminAccount": management }),
    )
    .await;
    assert_eq!(
        status, 200,
        "AssociateAdminAccount must onboard the organization, got {status} with {body}"
    );

    let (status, body) = call(
        &router,
        "fms",
        "AWSFMS_20180101",
        "ListAdminAccountsForOrganization",
        "us-west-2",
        &json!({}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the enumeration must be served once the organization is onboarded, got {status} with \
         {body}"
    );
    assert_eq!(
        body["AdminAccounts"][0]["AdminAccount"].as_str(),
        Some(management.as_str()),
        "the administrator reported must be the account that was associated, got {body}"
    );
}
