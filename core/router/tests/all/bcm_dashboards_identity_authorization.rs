// SPDX-License-Identifier: CC0-1.0

//! IAM authorization of the BCM Dashboards API, over its awsJson1_0 wire.
//!
//! THE RULE UNDER TEST: every request to this service is a signed call, and such a call is
//! served only to a caller whose identity-based IAM policies allow THAT operation's own
//! action (`bcm-dashboards:<OperationName>`) on the resource the request addresses — the
//! dashboard or scheduled-report ARN the request names, and IAM's any-resource wildcard for
//! a request that addresses none (the two creates, whose resource does not exist until the
//! store mints its ARN, and the two listings). Every other caller is refused HTTP 403 under
//! the error code `AccessDeniedException`, with IAM's own sentence, BEFORE the operation
//! reads or writes anything — so a refused delete leaves its dashboard stored, a refused
//! update leaves every member as it was, a refused execution triggers nothing, and a
//! refused reader learns nothing about what the account holds, not even whether the
//! resource it named exists.
//!
//! This is a CROSS-SERVICE rule and only a cross-service suite can pin it: the principals
//! below are made with IAM's and STS's OWN operations over the same engine (`CreateUser` +
//! `PutUserPolicy` + `CreateAccessKey`, `CreateRole` + `PutRolePolicy` + `AssumeRole`), so
//! the documents this service decides on are the ones those services wrote and the
//! principal a refusal blames is the identity STS reports for the credential that signed.
//!
//! The rows vary what the rule generalises over: the OPERATION asked for (all twelve whose
//! model declares `AccessDeniedException`, covering both resource types and both
//! resource-less forms), what the principal's policies say (nothing at all, another action
//! of this service, every action of another service, an explicit `Deny` beside an `Allow`
//! of exactly the action, an `Allow` of exactly the action, of the action in another letter
//! case, of this whole service and of everything), the kind of principal (an assumed-role
//! session, an IAM user's long-term access key, the account's own published credential) and
//! the resource an `Allow` is scoped to (everything, exactly this resource, every resource
//! of this type, another resource of this type, the other resource type).
//!
//! The harness below — the emulator, the signed call, the principals made with IAM's and
//! STS's own operations, and the two refusal assertions — is shared with
//! `bcm_dashboards_write_authorization`, which pins what a refused WRITE must not have left
//! behind. It lives here, once, so both suites assert against the same IAM wording.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bcmdashboards_awsbcmdashboardsservice::service::authorization::{
    ACTION_PREFIX, ANY_RESOURCE,
};

use crate::query_wire::{element, form_encode};

/// The Region every call below is signed for. This service's ARNs carry no Region (its
/// `@pattern`s leave that segment empty), so it decides nothing here beyond the credential
/// scope the request presents.
const REGION: &str = "us-east-1";

/// The access-key id standing for the account's own published credential: neither STS nor
/// IAM holds a record for it, so it resolves to the account's ambient identity, which holds
/// no IAM policies — the position AWS account root credentials are in.
pub(crate) const ACCOUNT_KEY: &str = "EMULATORTESTKEY12345";

/// The wire name and status of the refusal the authorization step answers with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// A well-formed `ServiceRoleArn` for a scheduled report's execution role (pattern
/// `^arn:aws[-a-z0-9]*:iam::[0-9]{12}:role/[a-zA-Z0-9+=,.@_/-]+$`): the report's own
/// execution role, which has nothing to do with the CALLER this suite varies.
pub(crate) const EXECUTION_ROLE_ARN: &str = "arn:aws:iam::123456789012:role/BCMDashboardsReportRole";

/// One answer, reduced to what a client branches on.
#[derive(Debug)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: Value,
}

impl Reply {
    /// The wire error code (`__type`), which this protocol may qualify with the model's
    /// namespace.
    pub(crate) fn error_code(&self) -> String {
        self.body["__type"]
            .as_str()
            .unwrap_or("<none>")
            .to_string()
    }

    /// The `message` the error document carries.
    pub(crate) fn message(&self) -> String {
        self.body["message"].as_str().unwrap_or_default().to_string()
    }
}

/// A principal a call is signed as: the access key id it presents, and the ARN a refusal
/// must blame it by — what `sts:GetCallerIdentity` reports for that key.
#[derive(Debug)]
pub(crate) struct Principal {
    pub(crate) access_key_id: String,
    pub(crate) arn: String,
}

/// A SigV4 `Authorization` header whose credential scope names `service`. The signature is
/// never checked — the router here runs with request authentication off — and the access
/// key id is what resolves the caller's identity.
fn authorization(access_key_id: &str, service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=0"
    )
}

/// The emulator these rows drive: BCM Dashboards beside the IAM and STS services the
/// principals and their policies are made with, and the EventBridge an execution's
/// notification is delivered to, on ONE engine, as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    build_router_on(Arc::new(engine::InMemoryEngine::new()))
}

/// That same emulator mounted on an engine the CALLER holds, so a suite can also read
/// another mounted service's store back through it — the event bus an execution delivers
/// its notification to, for one.
pub(crate) fn build_router_on(engine: Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_bcmdashboards_awsbcmdashboardsservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(engine),
    );
    router
}

/// One BCM Dashboards call: `awsJson1_0` under this service's credential scope, routed by
/// `X-Amz-Target`, signed with `access_key_id`.
pub(crate) async fn dashboards(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> Reply {
    let payload = serde_json::to_vec(body).expect("the request body serializes");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("bcm-dashboards.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AWSBCMDashboardsService.{operation}"))
        .header("authorization", authorization(access_key_id, ACTION_PREFIX))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let body = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    Reply { status, body }
}

/// One call signed as the account itself, which must succeed: the suite's setup and
/// read-back legs.
pub(crate) async fn served(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let reply = dashboards(router, ACCOUNT_KEY, operation, body).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the account's own {operation} must succeed, got {reply:?}"
    );
    reply.body
}

/// One awsQuery call to IAM or STS, signed with the account's own credential, which must
/// succeed: these are the suite's setup legs.
pub(crate) async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(ACCOUNT_KEY, signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {action:?}: the setup call must succeed, got {status} with {text}"
    );
    text
}

/// Create an IAM user carrying `policy` as its inline policy (none at all when `policy` is
/// `None`), mint a long-term access key for it, and answer that key id with the user's own
/// ARN.
pub(crate) async fn user(
    router: &MultiServiceRouter,
    user_name: &str,
    policy: Option<&str>,
) -> Principal {
    let created = query(
        router,
        "iam",
        "2010-05-08",
        "CreateUser",
        &[("UserName", user_name.to_string())],
    )
    .await;
    let arn = element(&created, "Arn");
    if let Some(policy) = policy {
        query(
            router,
            "iam",
            "2010-05-08",
            "PutUserPolicy",
            &[
                ("UserName", user_name.to_string()),
                ("PolicyName", "inline".to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
    }
    let minted = query(
        router,
        "iam",
        "2010-05-08",
        "CreateAccessKey",
        &[("UserName", user_name.to_string())],
    )
    .await;
    Principal {
        access_key_id: element(&minted, "AccessKeyId"),
        arn,
    }
}

/// Create an IAM role carrying `policy` as its inline policy (none at all when `policy` is
/// `None`), assume it through STS, and answer the session's access key id with the session
/// ARN.
pub(crate) async fn assumed_role(
    router: &MultiServiceRouter,
    role_name: &str,
    policy: Option<&str>,
) -> Principal {
    let account = engine::emulator_account_id();
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{account}:root") },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    if let Some(policy) = policy {
        query(
            router,
            "iam",
            "2010-05-08",
            "PutRolePolicy",
            &[
                ("RoleName", role_name.to_string()),
                ("PolicyName", "inline".to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
    }
    let assumed = query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[
            (
                "RoleArn",
                format!("arn:aws:iam::{account}:role/{role_name}"),
            ),
            ("RoleSessionName", "probe".to_string()),
        ],
    )
    .await;
    Principal {
        access_key_id: element(&assumed, "AccessKeyId"),
        arn: element(&assumed, "Arn"),
    }
}

/// A policy document whose one statement allows `action` on `resource`.
pub(crate) fn allow(action: &Value, resource: &Value) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
    .to_string()
}

/// The fixture: one dashboard and one scheduled report over it, created by the account's
/// own credential, plus a second dashboard an allow can be scoped to instead.
struct Fixture {
    dashboard: String,
    other_dashboard: String,
    report: String,
}

/// Create the fixture on `router`.
async fn fixture(router: &MultiServiceRouter) -> Fixture {
    let dashboard = served(
        router,
        "fixture",
        "CreateDashboard",
        &json!({"name": "AuthzBoard", "widgets": []}),
    )
    .await["arn"]
        .as_str()
        .expect("CreateDashboard answers an arn")
        .to_string();
    let other_dashboard = served(
        router,
        "fixture",
        "CreateDashboard",
        &json!({"name": "OtherBoard", "widgets": []}),
    )
    .await["arn"]
        .as_str()
        .expect("CreateDashboard answers an arn")
        .to_string();
    let report = served(
        router,
        "fixture",
        "CreateScheduledReport",
        &json!({
            "scheduledReport": {
                "name": "AuthzReport",
                "dashboardArn": dashboard,
                "scheduledReportExecutionRoleArn": EXECUTION_ROLE_ARN,
                "scheduleConfig": {"scheduleExpression": "cron(0 8 1 * ? *)"},
            },
        }),
    )
    .await["arn"]
        .as_str()
        .expect("CreateScheduledReport answers an arn")
        .to_string();
    Fixture {
        dashboard,
        other_dashboard,
        report,
    }
}

/// One request a decision is taken about: the call itself, the operation whose name is the
/// action IAM judges it by, and the resource the request addresses.
struct Gated {
    operation: &'static str,
    body: Value,
    resource: String,
}

impl Gated {
    /// The IAM action this request asks for.
    fn action(&self) -> String {
        format!("{ACTION_PREFIX}:{}", self.operation)
    }

    /// The resource clause IAM's sentence carries for it: none for a request that addresses
    /// no resource of its own.
    fn clause(&self) -> String {
        if self.resource == ANY_RESOURCE {
            String::new()
        } else {
            format!(" on resource: {}", self.resource)
        }
    }
}

/// Every operation whose model declares `AccessDeniedException` (`OPERATIONS.md`, the error
/// list of each operation), with a request each would otherwise answer and the resource it
/// addresses.
///
/// The three resource-tagging operations are deliberately absent: their model declares no
/// `AccessDeniedException`, so there is no modelled refusal for them to answer with.
fn gated(fixture: &Fixture) -> Vec<Gated> {
    vec![
        Gated {
            operation: "CreateDashboard",
            body: json!({"name": "MintedBoard", "widgets": []}),
            resource: ANY_RESOURCE.to_string(),
        },
        Gated {
            operation: "CreateScheduledReport",
            body: json!({
                "scheduledReport": {
                    "name": "MintedReport",
                    "dashboardArn": fixture.dashboard,
                    "scheduledReportExecutionRoleArn": EXECUTION_ROLE_ARN,
                    "scheduleConfig": {"scheduleExpression": "cron(0 9 1 * ? *)"},
                },
            }),
            resource: ANY_RESOURCE.to_string(),
        },
        Gated {
            operation: "DeleteDashboard",
            body: json!({"arn": fixture.dashboard}),
            resource: fixture.dashboard.clone(),
        },
        Gated {
            operation: "DeleteScheduledReport",
            body: json!({"arn": fixture.report}),
            resource: fixture.report.clone(),
        },
        Gated {
            operation: "ExecuteScheduledReport",
            body: json!({"arn": fixture.report}),
            resource: fixture.report.clone(),
        },
        Gated {
            operation: "GetDashboard",
            body: json!({"arn": fixture.dashboard}),
            resource: fixture.dashboard.clone(),
        },
        Gated {
            operation: "GetResourcePolicy",
            body: json!({"resourceArn": fixture.dashboard}),
            resource: fixture.dashboard.clone(),
        },
        Gated {
            operation: "GetScheduledReport",
            body: json!({"arn": fixture.report}),
            resource: fixture.report.clone(),
        },
        Gated {
            operation: "ListDashboards",
            body: json!({}),
            resource: ANY_RESOURCE.to_string(),
        },
        Gated {
            operation: "ListScheduledReports",
            body: json!({}),
            resource: ANY_RESOURCE.to_string(),
        },
        Gated {
            operation: "UpdateDashboard",
            body: json!({"arn": fixture.dashboard, "name": "RenamedBoard"}),
            resource: fixture.dashboard.clone(),
        },
        Gated {
            operation: "UpdateScheduledReport",
            body: json!({"arn": fixture.report, "name": "RenamedReport"}),
            resource: fixture.report.clone(),
        },
    ]
}

/// Which of IAM's two refusals a row expects.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Refusal {
    /// The default deny of a request no readable statement allows.
    DefaultDeny,
    /// The explicit deny of a request a `Deny` statement covers.
    ExplicitDeny,
}

impl Refusal {
    /// The clause IAM closes the refusal with.
    pub(crate) fn reason(self, action: &str) -> String {
        match self {
            Self::DefaultDeny => {
                format!("because no identity-based policy allows the {action} action")
            }
            Self::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        }
    }
}

/// The documents a row puts in force on the principal asking for `action`, and which of
/// IAM's refusals the request then meets — `None` for a request a matching `Allow` permits.
///
/// `other` is another action of this service, so the row that grants it grants something of
/// this service that is not what the request asks for.
fn documents_in_force(action: &str, other: &str) -> Vec<(&'static str, Option<String>, Option<Refusal>)> {
    vec![
        ("no policy at all", None, Some(Refusal::DefaultDeny)),
        (
            "an allow of another action of this service, but not this one",
            Some(allow(&json!(other), &json!("*"))),
            Some(Refusal::DefaultDeny),
        ),
        (
            "an allow of every action of another service",
            Some(allow(&json!("ce:*"), &json!("*"))),
            Some(Refusal::DefaultDeny),
        ),
        (
            "an explicit deny of this whole service beside an allow of exactly this action",
            Some(
                json!({
                    "Statement": [
                        { "Effect": "Allow", "Action": action, "Resource": "*" },
                        { "Effect": "Deny", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
                    ],
                })
                .to_string(),
            ),
            Some(Refusal::ExplicitDeny),
        ),
        (
            "an allow of exactly this action",
            Some(allow(&json!(action), &json!("*"))),
            None,
        ),
        (
            // IAM compares action names case-insensitively, so the spelling of the allow is
            // not what decides the request.
            "an allow of this action spelled in lower case",
            Some(allow(&json!(action.to_lowercase()), &json!("*"))),
            None,
        ),
        (
            "an allow of every action of this service",
            Some(allow(&json!(format!("{ACTION_PREFIX}:*")), &json!("*"))),
            None,
        ),
        (
            "an allow of every action",
            Some(allow(&json!("*"), &json!("*"))),
            None,
        ),
    ]
}

/// Require `reply` to be IAM's refusal, with the sentence it states.
pub(crate) fn assert_refused(case: &str, reply: &Reply, expected_message: &str) {
    assert_eq!(
        reply.status, ACCESS_DENIED_STATUS,
        "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS}, got {reply:?}"
    );
    assert!(
        reply.error_code().ends_with(ACCESS_DENIED),
        "case {case:?}: expected the {ACCESS_DENIED} code, got {reply:?}"
    );
    assert_eq!(
        reply.message(),
        expected_message,
        "case {case:?}: expected IAM's wording, got {:?}",
        reply.message()
    );
}

/// Require `reply` not to be an authorization refusal: an allowed request reaches the
/// operation's own answer.
pub(crate) fn assert_not_refused(case: &str, reply: &Reply) {
    assert_ne!(
        reply.status, ACCESS_DENIED_STATUS,
        "case {case:?}: an allowed request must not be refused, got {reply:?}"
    );
    assert!(
        !reply.error_code().ends_with(ACCESS_DENIED),
        "case {case:?}: an allowed request must not be refused, got {reply:?}"
    );
}

/// THE RULE: every one of the twelve operations that models `AccessDeniedException` is
/// judged against its OWN `bcm-dashboards:<OperationName>` action on the resource the
/// request addresses; a refusal is HTTP 403 `AccessDeniedException` naming the principal,
/// the action, that resource and which of IAM's rules refused it, and an allowed call
/// reaches the operation's own answer instead.
#[tokio::test]
async fn every_gated_operation_is_authorized_against_its_own_action() {
    let names: Vec<&'static str> = gated(&Fixture {
        dashboard: String::new(),
        other_dashboard: String::new(),
        report: String::new(),
    })
    .iter()
    .map(|request| request.operation)
    .collect();

    for index in 0..names.len() {
        let action_name = names[index];
        // Another action of this service, which one row grants instead of the one under
        // test.
        let other = format!(
            "{ACTION_PREFIX}:{}",
            names.iter().find(|name| **name != action_name).expect("this service has more than one gated operation")
        );

        for (row, document, refused) in documents_in_force(&format!("{ACTION_PREFIX}:{action_name}"), &other) {
            // A fresh emulator, fixture and principal per row: no row can pass or fail on
            // another row's write.
            let router = build_router();
            let fixture = fixture(&router).await;
            let request = gated(&fixture).into_iter().nth(index).expect("the row's request");
            let case = format!("{} with {row}", request.operation);
            let principal = assumed_role(&router, "dashboard-caller", document.as_deref()).await;

            let reply = dashboards(
                &router,
                &principal.access_key_id,
                request.operation,
                &request.body,
            )
            .await;
            match refused {
                Some(refusal) => {
                    let action = request.action();
                    assert_refused(
                        &case,
                        &reply,
                        &format!(
                            "User: {} is not authorized to perform: {action}{} {}",
                            principal.arn,
                            request.clause(),
                            refusal.reason(&action)
                        ),
                    );
                    // The refusal happened before anything was read or written: the
                    // account's own reads still find the fixture exactly as it was.
                    let dashboard = served(
                        &router,
                        &case,
                        "GetDashboard",
                        &json!({"arn": fixture.dashboard}),
                    )
                    .await;
                    assert_eq!(
                        dashboard["name"].as_str(),
                        Some("AuthzBoard"),
                        "case {case:?}: a refused call must change nothing, got {dashboard}"
                    );
                    let report = served(
                        &router,
                        &case,
                        "GetScheduledReport",
                        &json!({"arn": fixture.report}),
                    )
                    .await;
                    assert_eq!(
                        report["scheduledReport"]["name"].as_str(),
                        Some("AuthzReport"),
                        "case {case:?}: a refused call must change nothing, got {report}"
                    );
                    assert_eq!(
                        report["scheduledReport"]["lastExecutionAt"],
                        Value::Null,
                        "case {case:?}: a refused call must trigger no execution, got {report}"
                    );
                    let listed = served(&router, &case, "ListDashboards", &json!({})).await;
                    let created: Vec<&str> = listed["dashboards"]
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item["name"].as_str())
                                .collect()
                        })
                        .unwrap_or_default();
                    assert!(
                        created.contains(&"AuthzBoard") && !created.contains(&"MintedBoard"),
                        "case {case:?}: a refused call must create and delete nothing, got {listed}"
                    );
                }
                None => assert_not_refused(&case, &reply),
            }
        }
    }
}

/// RULE: the decision is taken on the policies of the PRINCIPAL the signing credential
/// names — an assumed-role session is judged on its role's, an IAM user's long-term key on
/// the user's own — and the refusal blames that principal by the ARN
/// `sts:GetCallerIdentity` reports for the key. The account's own published credential
/// holds no IAM policies at all, so it is served.
#[tokio::test]
async fn each_principal_form_is_judged_on_its_own_policies() {
    let router = build_router();
    let fixture = fixture(&router).await;
    // Neither principal carries a document that says anything about this service, so both
    // meet IAM's default deny — under their own ARNs.
    let session = assumed_role(&router, "denied-role", None).await;
    let long_term = user(&router, "denied-user", None).await;

    for request in gated(&fixture) {
        let action = request.action();
        for (form, principal) in [("an assumed-role session", &session), ("an IAM user's long-term key", &long_term)] {
            let case = format!("{} as {form}", request.operation);
            let reply = dashboards(
                &router,
                &principal.access_key_id,
                request.operation,
                &request.body,
            )
            .await;
            assert_refused(
                &case,
                &reply,
                &format!(
                    "User: {} is not authorized to perform: {action}{} {}",
                    principal.arn,
                    request.clause(),
                    Refusal::DefaultDeny.reason(&action)
                ),
            );
        }

        let case = format!("{} as the account's own credential", request.operation);
        let reply = dashboards(&router, ACCOUNT_KEY, request.operation, &request.body).await;
        assert_not_refused(&case, &reply);
    }
}

/// RULE: a request is judged against the resource IT addresses, so an `Allow` scoped to one
/// dashboard covers that dashboard and no other, and one scoped to the scheduled-report
/// resource type covers no dashboard at all.
#[tokio::test]
async fn a_resource_scoped_grant_covers_only_the_resource_it_names() {
    let account = engine::emulator_account_id();
    // (case, the Resource the allow is scoped to, whether the addressed dashboard is
    // covered)
    let rows: Vec<(&str, Value, bool)> = vec![
        ("an allow of every resource", json!("*"), true),
        ("an allow of every dashboard of this account", json!(format!("arn:aws:bcm-dashboards::{account}:dashboard/*")), true),
        ("an allow of this account's scheduled reports only", json!(format!("arn:aws:bcm-dashboards::{account}:scheduled-report/*")), false),
    ];

    for (case, resource, covered) in rows {
        let router = build_router();
        let fixture = fixture(&router).await;
        let principal = assumed_role(
            &router,
            "scoped-role",
            Some(&allow(
                &json!(format!("{ACTION_PREFIX}:GetDashboard")),
                &resource,
            )),
        )
        .await;

        let reply = dashboards(
            &router,
            &principal.access_key_id,
            "GetDashboard",
            &json!({"arn": fixture.dashboard}),
        )
        .await;
        if covered {
            assert_not_refused(case, &reply);
            assert_eq!(
                reply.body["arn"].as_str(),
                Some(fixture.dashboard.as_str()),
                "case {case:?}: expected the addressed dashboard, got {reply:?}"
            );
        } else {
            let action = format!("{ACTION_PREFIX}:GetDashboard");
            assert_refused(
                case,
                &reply,
                &format!(
                    "User: {} is not authorized to perform: {action} on resource: {} {}",
                    principal.arn,
                    fixture.dashboard,
                    Refusal::DefaultDeny.reason(&action)
                ),
            );
        }
    }

    // The sharpest scoping case, on one emulator: a grant naming EXACTLY one dashboard
    // serves a read of that dashboard and refuses the same read of another one.
    let router = build_router();
    let fixture = fixture(&router).await;
    let principal = assumed_role(
        &router,
        "one-board-role",
        Some(&allow(
            &json!(format!("{ACTION_PREFIX}:GetDashboard")),
            &json!(fixture.dashboard),
        )),
    )
    .await;
    let reply = dashboards(
        &router,
        &principal.access_key_id,
        "GetDashboard",
        &json!({"arn": fixture.dashboard}),
    )
    .await;
    assert_not_refused("an allow of exactly this dashboard", &reply);
    let reply = dashboards(
        &router,
        &principal.access_key_id,
        "GetDashboard",
        &json!({"arn": fixture.other_dashboard}),
    )
    .await;
    let action = format!("{ACTION_PREFIX}:GetDashboard");
    assert_refused(
        "an allow of another dashboard only",
        &reply,
        &format!(
            "User: {} is not authorized to perform: {action} on resource: {} {}",
            principal.arn,
            fixture.other_dashboard,
            Refusal::DefaultDeny.reason(&action)
        ),
    );
}

/// THE REPORTED CASE, and what it costs: with an `Allow` the caller reads a dashboard, its
/// resource-based policy and the account's listings; once an explicit `Deny` of this
/// service is attached on top the identical calls are refused 403 and expose nothing, and
/// removing the `Deny` restores them for the same identity.
#[tokio::test]
async fn a_denied_principal_reads_nothing_and_a_restored_grant_serves_again() {
    let router = build_router();
    let fixture = fixture(&router).await;
    let principal = assumed_role(
        &router,
        "reader-role",
        Some(&allow(&json!(format!("{ACTION_PREFIX}:*")), &json!("*"))),
    )
    .await;

    // The control: the grant alone serves the three reads the issue names.
    for (case, operation, body) in [
        ("GetDashboard", "GetDashboard", json!({"arn": fixture.dashboard})),
        (
            "GetResourcePolicy",
            "GetResourcePolicy",
            json!({"resourceArn": fixture.dashboard}),
        ),
        ("ListDashboards", "ListDashboards", json!({})),
    ] {
        let reply = dashboards(&router, &principal.access_key_id, operation, &body).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: the granted read must be served, got {reply:?}"
        );
    }

    // The deny on top, as `iam:PutRolePolicy` replaces the role's inline document.
    query(
        &router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", "reader-role".to_string()),
            ("PolicyName", "inline".to_string()),
            (
                "PolicyDocument",
                json!({
                    "Statement": [
                        { "Effect": "Allow", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
                        { "Effect": "Deny", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
                    ],
                })
                .to_string(),
            ),
        ],
    )
    .await;

    for (case, operation, body, resource) in [
        (
            "GetDashboard under an explicit deny",
            "GetDashboard",
            json!({"arn": fixture.dashboard}),
            fixture.dashboard.clone(),
        ),
        (
            "GetResourcePolicy under an explicit deny",
            "GetResourcePolicy",
            json!({"resourceArn": fixture.dashboard}),
            fixture.dashboard.clone(),
        ),
        (
            "GetScheduledReport under an explicit deny",
            "GetScheduledReport",
            json!({"arn": fixture.report}),
            fixture.report.clone(),
        ),
        (
            "ListDashboards under an explicit deny",
            "ListDashboards",
            json!({}),
            ANY_RESOURCE.to_string(),
        ),
        (
            "ListScheduledReports under an explicit deny",
            "ListScheduledReports",
            json!({}),
            ANY_RESOURCE.to_string(),
        ),
    ] {
        let reply = dashboards(&router, &principal.access_key_id, operation, &body).await;
        let action = format!("{ACTION_PREFIX}:{operation}");
        let clause = if resource == ANY_RESOURCE {
            String::new()
        } else {
            format!(" on resource: {resource}")
        };
        assert_refused(
            case,
            &reply,
            &format!(
                "User: {} is not authorized to perform: {action}{clause} {}",
                principal.arn,
                Refusal::ExplicitDeny.reason(&action)
            ),
        );
        // Nothing of the record is in the refusal: no ARN of a resource the caller did not
        // name, no dashboard name, no policy document.
        let rendered = reply.body.to_string();
        assert!(
            !rendered.contains("AuthzBoard") && !rendered.contains("policyDocument"),
            "case {case:?}: a refusal must expose nothing of the record, got {rendered}"
        );
    }

    // The deny removed: the same identity is served again, and the dashboard it reads back
    // is the one the account still holds.
    query(
        &router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", "reader-role".to_string()),
            ("PolicyName", "inline".to_string()),
            (
                "PolicyDocument",
                allow(&json!(format!("{ACTION_PREFIX}:*")), &json!("*")),
            ),
        ],
    )
    .await;
    let reply = dashboards(
        &router,
        &principal.access_key_id,
        "GetDashboard",
        &json!({"arn": fixture.dashboard}),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "removing the deny restores the grant, got {reply:?}"
    );
    assert_eq!(
        reply.body["name"].as_str(),
        Some("AuthzBoard"),
        "the restored read answers the stored dashboard, got {reply:?}"
    );
}

/// RULE: the gate runs BEFORE the operation resolves what the request names, so a refused
/// caller is told it is not authorized rather than told whether the resource exists — and
/// an authorized caller naming a resource the account does not hold meets the operation's
/// own not-found instead.
#[tokio::test]
async fn a_refusal_says_nothing_about_whether_the_resource_exists() {
    let account = engine::emulator_account_id();
    let absent = format!("arn:aws:bcm-dashboards::{account}:dashboard/0000000000000000000000000000ffff");
    let router = build_router();
    let fixture = fixture(&router).await;

    let denied = assumed_role(&router, "unprivileged-role", None).await;
    let action = format!("{ACTION_PREFIX}:GetDashboard");
    for (case, arn) in [
        ("a dashboard the account holds", fixture.dashboard.clone()),
        ("a dashboard the account does not hold", absent.clone()),
    ] {
        let reply = dashboards(
            &router,
            &denied.access_key_id,
            "GetDashboard",
            &json!({"arn": arn}),
        )
        .await;
        assert_refused(
            case,
            &reply,
            &format!(
                "User: {} is not authorized to perform: {action} on resource: {arn} {}",
                denied.arn,
                Refusal::DefaultDeny.reason(&action)
            ),
        );
    }

    // The same absent ARN, asked for by a caller IAM allows: now the answer is the
    // operation's own not-found rather than an authorization refusal.
    let allowed = assumed_role(
        &router,
        "privileged-role",
        Some(&allow(&json!(action.clone()), &json!("*"))),
    )
    .await;
    let reply = dashboards(
        &router,
        &allowed.access_key_id,
        "GetDashboard",
        &json!({"arn": absent}),
    )
    .await;
    assert_not_refused("an allowed read of an absent dashboard", &reply);
    assert!(
        reply.error_code().ends_with("ResourceNotFoundException"),
        "an allowed read of an absent dashboard meets the not-found, got {reply:?}"
    );
}
