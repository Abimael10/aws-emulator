// SPDX-License-Identifier: CC0-1.0

//! What a BCM Dashboards WRITE that IAM refuses leaves behind: nothing.
//!
//! THE RULE UNDER TEST: a state-changing operation whose model declares
//! `AccessDeniedException` is answered HTTP 403 under that code when the caller's
//! identity-based IAM policies do not allow that operation's own
//! `bcm-dashboards:<OperationName>` action on the resource the request addresses, and the
//! decision is taken BEFORE the store is reached and before any rule about the request's
//! own members is applied — so everything a later caller can read is exactly what it was
//! and every effect the write would have had is absent: no resource minted or removed, no
//! member replaced, no notification delivered to the account's event bus, no idempotency
//! token spent. The same request from a principal the policies allow is served and takes
//! effect.
//!
//! The suite beside this one (`bcm_dashboards_identity_authorization`) pins the DECISION
//! across all twelve gated operations and every form of policy and principal, and this one
//! — sharing its harness — pins what a refused WRITE must not have done, which only a write
//! path can show. Its rows are the seven state-changing operations the model gates: both
//! resource types, both creates (which address no resource of their own), both updates, both
//! deletes and the execution. Each row is driven by two principals IAM refuses for different
//! reasons — an assumed-role session whose role carries no policy at all (IAM's default
//! deny) and an IAM user carrying an explicit `Deny` beside an `Allow` of the same action
//! (an explicit deny) — and then by one the policy allows, with the identical request, the
//! same client token and all. The comparison is over EVERY read a client has of this
//! account: the dashboard, the resource policy and the tags stored on it, the scheduled
//! report, both listings, and the events the mounted EventBridge has taken.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bcmdashboards_awsbcmdashboardsservice::service::authorization::{
    ACTION_PREFIX, ANY_RESOURCE,
};

use crate::bcm_dashboards_identity_authorization::{
    allow, assert_not_refused, assert_refused, assumed_role, build_router_on, dashboards, served,
    user, Refusal, ACCOUNT_KEY, EXECUTION_ROLE_ARN,
};

/// The client token the refused `CreateScheduledReport` presents, and that the allowed call
/// then presents again: a refusal that had claimed it would make that second call answer the
/// conflict a spent token is answered with instead of creating the report.
const CREATE_TOKEN: &str = "write-authz-create-0001";

/// The same, for the refused `ExecuteScheduledReport`: a refusal that had reserved the token
/// would make the allowed call replay an execution that never happened.
const EXECUTE_TOKEN: &str = "write-authz-execute-0001";

/// The dashboard and the scheduled report every row is asked to change, as the account's own
/// credential created them.
struct Fixture {
    dashboard: String,
    report: String,
}

/// A widget whose data comes from a cost-and-usage query shown as a table, so the fixture
/// dashboard carries structure a write that slipped past a refusal would disturb.
///
/// `AmortizedCost` is a `MetricName` the model's note on `CostAndUsageQuery$metrics` admits,
/// `DAILY` a `Granularity` value and `ABSOLUTE` a `DateTimeType` one (sources: the
/// `CostAndUsageQuery`, `Granularity` and `DateTimeType` shapes of this service's model).
fn widget() -> Value {
    json!({
        "title": "Spend",
        "configs": [{
            "queryParameters": {
                "costAndUsage": {
                    "metrics": ["AmortizedCost"],
                    "timeRange": {
                        "startTime": {"type": "ABSOLUTE", "value": "2025-01-01"},
                        "endTime": {"type": "ABSOLUTE", "value": "2025-07-01"}
                    },
                    "granularity": "DAILY"
                }
            },
            "displayConfig": {"table": {}}
        }]
    })
}

/// Create the fixture on `router` with the account's own credential: a dashboard carrying a
/// description, a widget and a tag, and a scheduled report over it carrying a description and
/// a schedule.
async fn fixture(router: &MultiServiceRouter) -> Fixture {
    let dashboard = served(
        router,
        "fixture",
        "CreateDashboard",
        &json!({
            "name": "WriteAuthzBoard",
            "description": "The dashboard every row is asked to change",
            "widgets": [widget()],
            "resourceTags": [{"key": "owner", "value": "finops"}],
        }),
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
                "name": "WriteAuthzReport",
                "description": "The report every row is asked to change",
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
    Fixture { dashboard, report }
}

/// One state-changing call a decision is taken about: the request a client makes, and the
/// resource it addresses ([`ANY_RESOURCE`] for one that addresses none).
struct WriteCall {
    operation: &'static str,
    body: Value,
    resource: String,
}

impl WriteCall {
    /// The resource clause IAM's refusal carries for it: none for a request that addresses no
    /// resource of its own.
    fn clause(&self) -> String {
        if self.resource == ANY_RESOURCE {
            String::new()
        } else {
            format!(" on resource: {}", self.resource)
        }
    }
}

/// Every state-changing operation whose model declares `AccessDeniedException`
/// (`smithy_query(query="digest")`: the three resource-tagging operations declare none, and
/// the other five gated operations only report), with the request a client makes of it.
fn write_calls(fixture: &Fixture) -> Vec<WriteCall> {
    vec![
        WriteCall {
            operation: "CreateDashboard",
            body: json!({
                "name": "MintedByADeniedCaller",
                "description": "A dashboard no denied caller may mint",
                "widgets": [widget()],
            }),
            resource: ANY_RESOURCE.to_string(),
        },
        WriteCall {
            operation: "UpdateDashboard",
            // Every optional member supplied, so a write that slipped through would replace
            // the name, the description and the widget list at once.
            body: json!({
                "arn": fixture.dashboard,
                "name": "RenamedByADeniedCaller",
                "description": "A description no denied caller may store",
                "widgets": [],
            }),
            resource: fixture.dashboard.clone(),
        },
        WriteCall {
            operation: "DeleteDashboard",
            body: json!({"arn": fixture.dashboard}),
            resource: fixture.dashboard.clone(),
        },
        WriteCall {
            operation: "CreateScheduledReport",
            body: json!({
                "clientToken": CREATE_TOKEN,
                "scheduledReport": {
                    "name": "ReportMintedByADeniedCaller",
                    "dashboardArn": fixture.dashboard,
                    "scheduledReportExecutionRoleArn": EXECUTION_ROLE_ARN,
                    "scheduleConfig": {"scheduleExpression": "cron(0 9 1 * ? *)"},
                },
            }),
            resource: ANY_RESOURCE.to_string(),
        },
        WriteCall {
            operation: "UpdateScheduledReport",
            body: json!({
                "arn": fixture.report,
                "name": "ReportRenamedByADeniedCaller",
                "scheduleConfig": {"scheduleExpression": "cron(0 10 1 * ? *)", "state": "DISABLED"},
            }),
            resource: fixture.report.clone(),
        },
        WriteCall {
            operation: "DeleteScheduledReport",
            body: json!({"arn": fixture.report}),
            resource: fixture.report.clone(),
        },
        WriteCall {
            operation: "ExecuteScheduledReport",
            body: json!({"arn": fixture.report, "clientToken": EXECUTE_TOKEN}),
            resource: fixture.report.clone(),
        },
    ]
}

/// One read of this account, and the answer it gave.
#[derive(Debug, PartialEq)]
struct Probe {
    label: &'static str,
    status: u16,
    body: Value,
}

/// What the mounted EventBridge has been handed — the bus, the detail type and the resources
/// of each event — read back through the shared engine the handlers publish through. An
/// execution of a scheduled report IS such a delivery, so a refused execution has to leave
/// this list exactly as it was.
fn delivered(engine: &Arc<engine::InMemoryEngine>) -> Vec<Value> {
    let store: engine::ServiceState<
        service_com_amazonaws_eventbridge_awsevents::state::ComAmazonawsEventbridgeAwseventsState,
    > = engine.state();
    let events: Vec<Value> = store
        .lock()
        .delivered
        .iter()
        .map(|event| {
            json!({
                "bus": event.bus,
                "detailType": event.detail_type,
                "resources": event.resources,
            })
        })
        .collect();
    events
}

/// Everything a client can read back about this account, answered to the account's own
/// credential, plus what the event bus holds.
///
/// A read the state makes an error — the dashboard a delete removed — is kept as the answer
/// it gave rather than asserted on, so one snapshot describes the account before and after
/// any row. `maxResults` is the ceiling the model's `MaxResults` fixes (100), so each listing
/// answers in ONE page for a store this small and no record hides behind a cursor.
async fn snapshot(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    fixture: &Fixture,
) -> Vec<Probe> {
    let reads: [(&'static str, &str, Value); 6] = [
        (
            "the dashboard",
            "GetDashboard",
            json!({"arn": fixture.dashboard}),
        ),
        (
            "the dashboard's resource policy",
            "GetResourcePolicy",
            json!({"resourceArn": fixture.dashboard}),
        ),
        (
            "the dashboard's tags",
            "ListTagsForResource",
            json!({"resourceArn": fixture.dashboard}),
        ),
        (
            "the scheduled report",
            "GetScheduledReport",
            json!({"arn": fixture.report}),
        ),
        (
            "the dashboard listing",
            "ListDashboards",
            json!({"maxResults": 100}),
        ),
        (
            "the scheduled-report listing",
            "ListScheduledReports",
            json!({"maxResults": 100}),
        ),
    ];
    let mut probes = Vec::with_capacity(reads.len() + 1);
    for (label, operation, body) in reads {
        let reply = dashboards(router, ACCOUNT_KEY, operation, &body).await;
        probes.push(Probe {
            label,
            status: reply.status,
            body: reply.body,
        });
    }
    probes.push(Probe {
        label: "the account's default event bus",
        status: 200,
        body: json!(delivered(engine)),
    });
    probes
}

/// Require every read to answer exactly what it answered before: the refused call wrote
/// nothing a later caller can see.
fn assert_unchanged(case: &str, before: &[Probe], after: &[Probe]) {
    for (before, after) in before.iter().zip(after) {
        assert_eq!(
            (after.status, &after.body),
            (before.status, &before.body),
            "case {case:?}: a refused write must leave {} exactly as it was, got {} {} where \
             {} {} was read before",
            after.label,
            after.status,
            after.body,
            before.status,
            before.body
        );
    }
}

/// Require at least one read to answer differently: the allowed call took effect, which is
/// what makes the refusals a decision about the caller rather than a write path that never
/// works at all.
fn assert_changed(case: &str, before: &[Probe], after: &[Probe]) {
    let unchanged = before
        .iter()
        .zip(after)
        .all(|(before, after)| before.status == after.status && before.body == after.body);
    assert!(
        !unchanged,
        "case {case:?}: the allowed call must take effect, but every read answered exactly as \
         before: {after:?}"
    );
}

/// The document that refuses a caller EXPLICITLY: an `Allow` of everything this service does
/// beside a `Deny` of it, where IAM's explicit deny is what decides.
fn deny_beside_allow() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {"Effect": "Allow", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*"},
            {"Effect": "Deny", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*"},
        ],
    })
    .to_string()
}

/// THE RULE: each state-changing operation the model gates refuses a caller IAM does not
/// allow with HTTP 403 `AccessDeniedException`, having written nothing — no record minted,
/// replaced or removed, no notification delivered, no client token spent — and serves the
/// identical request from a caller IAM allows, which does take effect.
#[tokio::test]
async fn a_refused_write_changes_nothing_and_an_allowed_one_takes_effect() {
    // How many rows there are, counted off a placeholder fixture: each row's own request is
    // built against the fixture of the emulator it runs on.
    let rows = write_calls(&Fixture {
        dashboard: String::new(),
        report: String::new(),
    })
    .len();

    for index in 0..rows {
        // A fresh emulator per row: the allowed leg below really writes, so no row may run
        // on another row's store.
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let router = build_router_on(Arc::clone(&engine));
        let fixture = fixture(&router).await;
        let call = write_calls(&fixture)
            .into_iter()
            .nth(index)
            .expect("the row's call");
        let action = format!("{ACTION_PREFIX}:{}", call.operation);

        // (why IAM refuses this principal, the principal, the clause the refusal closes with)
        let denied = [
            (
                "an assumed-role session whose role carries no policy at all",
                assumed_role(&router, "write-denied-role", None).await,
                Refusal::DefaultDeny,
            ),
            (
                "an IAM user carrying an explicit deny beside an allow of this service",
                user(&router, "write-denied-user", Some(&deny_beside_allow())).await,
                Refusal::ExplicitDeny,
            ),
        ];

        for (form, principal, refusal) in &denied {
            let case = format!("{} as {form}", call.operation);
            let before = snapshot(&router, &engine, &fixture).await;
            let reply = dashboards(
                &router,
                &principal.access_key_id,
                call.operation,
                &call.body,
            )
            .await;
            assert_refused(
                &case,
                &reply,
                &format!(
                    "User: {} is not authorized to perform: {action}{} {}",
                    principal.arn,
                    call.clause(),
                    refusal.reason(&action)
                ),
            );
            let after = snapshot(&router, &engine, &fixture).await;
            assert_unchanged(&case, &before, &after);
        }

        // The identical request — the same client token and all — from a principal allowed
        // exactly this action.
        let allowed = assumed_role(
            &router,
            "write-allowed-role",
            Some(&allow(&json!(action.clone()), &json!("*"))),
        )
        .await;
        let case = format!("{} as a principal allowed exactly that action", call.operation);
        let before = snapshot(&router, &engine, &fixture).await;
        let reply = dashboards(&router, &allowed.access_key_id, call.operation, &call.body).await;
        assert_not_refused(&case, &reply);
        assert_eq!(
            reply.status, 200,
            "case {case:?}: the allowed write must be served, got {reply:?}"
        );
        let after = snapshot(&router, &engine, &fixture).await;
        assert_changed(&case, &before, &after);
    }
}

/// RULE: the decision comes FIRST — ahead of every rule about the request's own members — so
/// a denied caller is told it is not authorized rather than told what is wrong with the
/// values it submitted, and the same request from an allowed caller meets that rule instead.
#[tokio::test]
async fn the_decision_precedes_every_rule_about_the_requests_own_members() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = build_router_on(Arc::clone(&engine));
    let fixture = fixture(&router).await;
    // A create whose name breaks `DashboardName`'s own `@pattern` — it must begin with a
    // letter — so this is a request the service refuses on its merits once it is allowed to
    // look at them.
    let invalid = json!({"name": "9 Not A Name", "widgets": []});
    let action = format!("{ACTION_PREFIX}:CreateDashboard");

    let denied = assumed_role(&router, "write-denied-role", None).await;
    let case = "a denied caller submitting an invalid create";
    let before = snapshot(&router, &engine, &fixture).await;
    let reply = dashboards(&router, &denied.access_key_id, "CreateDashboard", &invalid).await;
    assert_refused(
        case,
        &reply,
        &format!(
            "User: {} is not authorized to perform: {action} {}",
            denied.arn,
            Refusal::DefaultDeny.reason(&action)
        ),
    );
    let after = snapshot(&router, &engine, &fixture).await;
    assert_unchanged(case, &before, &after);

    let allowed = assumed_role(
        &router,
        "write-allowed-role",
        Some(&allow(&json!(action.clone()), &json!("*"))),
    )
    .await;
    let case = "an allowed caller submitting an invalid create";
    let reply = dashboards(&router, &allowed.access_key_id, "CreateDashboard", &invalid).await;
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected the modelled ValidationException, got {reply:?}"
    );
    assert!(
        reply.error_code().ends_with("ValidationException"),
        "case {case:?}: expected the modelled ValidationException, got {reply:?}"
    );
    let after = snapshot(&router, &engine, &fixture).await;
    assert_unchanged(case, &before, &after);
}
