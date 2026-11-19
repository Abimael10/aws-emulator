// SPDX-License-Identifier: CC0-1.0

//! An `AWS::Cognito::IdentityPool` a stack updates is really updated through Amazon Cognito
//! Identity's own `UpdateIdentityPool`, so what that pool ends up holding is the owning
//! service's decision and not CloudFormation's.
//!
//! The rule under test: **a resource is reported UPDATED only where the owning API really put
//! the declared state in force. The pool's developer-provider domain is settled when the pool
//! first takes one — `UpdateIdentityPool` answers the domain the pool KEEPS, whatever the call
//! carried — so a declaration naming a different domain asks for a change this deployment
//! cannot make: that resource FAILS (one `UPDATE_FAILED` event whose reason names the domain
//! the declaration asked for) and the stack settles in `UPDATE_ROLLBACK_COMPLETE`, keeping the
//! pool, its id and the domain it already had. A declaration that restates the domain in force,
//! that names none, or that gives a domain to a pool holding none is applied, and the stack
//! reaches `UPDATE_COMPLETE` with the owning service reporting exactly what the template
//! declared.**
//!
//! Two crates have to agree for that to hold — CloudFormation makes the call and Amazon
//! Cognito Identity serves it, settling the domain — so the suite lives here rather than in
//! either crate.
//!
//! The rows differ along what the rule generalises over: whether the pool was created with a
//! domain at all, whether the update restates it, changes it, changes only its LETTER CASE
//! (the stored value is exact), names none, or adds one to a pool that had none; the mutable
//! property the same update changes beside it; and the Region the stack deploys in.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{elements, form_encode, members_of, opt_element, outputs_of};

/// How long a row goes on polling for an outcome that is already due, and how often: the
/// stack's clock-owed spans are sub-second, and a loaded machine can take far longer than that
/// without the rule being any different, so the deadline is generous — minutes for a span of
/// milliseconds — and the wait is for the CONDITION rather than for a span of time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(300);
/// See [`SETTLE_DEADLINE`].
const SETTLE_INTERVAL: Duration = Duration::from_millis(50);

/// The type whose declaration this suite is about, and the logical id every template holds it
/// under.
const POOL_TYPE: &str = "AWS::Cognito::IdentityPool";
/// See [`POOL_TYPE`].
const POOL_LOGICAL: &str = "IdentityPool";

/// The property the rule is about.
const DEVELOPER_PROVIDER_NAME: &str = "DeveloperProviderName";

/// The `ResourceStatus` and `StackStatus` values the rule above is stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";
const UPDATE_FAILED: &str = "UPDATE_FAILED";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cognitoidentity_awscognitoidentityservice::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Amazon Cognito Identity operation over the awsJson1_1 wire its model declares,
/// signed for `region`.
async fn cognito_identity(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cognito-identity.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityService.{operation}"),
        )
        .header("authorization", authorization("cognito-identity", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// One `StackEvent` as the wire reports it: a `StackEvent` holds no list of its own, so each
/// `<member>` of the answer is one event and the shared member reader is what separates them.
#[derive(Debug, Clone)]
struct WireEvent {
    logical_id: String,
    resource_type: String,
    status: String,
    status_reason: Option<String>,
}

/// Every event one `DescribeStackEvents` answer carries.
fn wire_events(xml: &str) -> Vec<WireEvent> {
    members_of(xml)
        .into_iter()
        .map(|entry| {
            let one = |name: &str| elements(entry, name).into_iter().next();
            WireEvent {
                logical_id: one("LogicalResourceId").unwrap_or_default(),
                resource_type: one("ResourceType").unwrap_or_default(),
                status: one("ResourceStatus").unwrap_or_default(),
                status_reason: one("ResourceStatusReason"),
            }
        })
        .collect()
}

/// The events a stack has recorded.
async fn events_of(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack: &str,
) -> Vec<WireEvent> {
    let (status, body) =
        cloudformation(router, region, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body {body}"
    );
    wire_events(&body)
}

/// The `DescribeStacks` answer for `stack` once its operation has left `*_IN_PROGRESS`.
///
/// The wait is for the terminal status, polled until it holds or the generous
/// [`SETTLE_DEADLINE`] passes — never for a span of wall clock, which would make the row's
/// outcome depend on how fast the host runs.
async fn settled_stack(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body \
             {described}"
        );
        let reached = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reached.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {stack} was still {reached:?} after {SETTLE_DEADLINE:?}, so nothing \
             settled it: {described}"
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// The template one deployment states: the pool alone, carrying `domain` only when the row
/// names one (so a row with `None` exercises the property's ABSENCE) and the login provider
/// `login_provider_app_id` under `graph.facebook.com`, which is the MUTABLE property each
/// update changes beside the domain.
fn template(name: &str, domain: Option<&str>, login_provider_app_id: &str) -> String {
    let mut properties = json!({
        "IdentityPoolName": name,
        "AllowUnauthenticatedIdentities": false,
        "SupportedLoginProviders": {"graph.facebook.com": login_provider_app_id},
    });
    if let Some(domain) = domain {
        properties[DEVELOPER_PROVIDER_NAME] = json!(domain);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "one identity pool, deployed and then updated",
        "Resources": {POOL_LOGICAL: {"Type": POOL_TYPE, "Properties": properties}},
        "Outputs": {"PoolRef": {"Value": {"Ref": POOL_LOGICAL}}},
    })
    .to_string()
}

/// Deploy `template` as a new stack and answer the pool id the stack published for it, having
/// asserted the create reached `CREATE_COMPLETE`.
async fn created_pool(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack: &str,
    body: &str,
) -> String {
    let (status, created) = cloudformation(
        router,
        region,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", body)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
    );
    let described = settled_stack(router, region, case, stack).await;
    let reached = opt_element(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the create to reach {CREATE_COMPLETE}, got {reached:?} with \
         {described}"
    );
    outputs_of(&described)
        .get("PoolRef")
        .unwrap_or_else(|| panic!("case {case:?}: no PoolRef output in {described}"))
        .clone()
}

/// Update `stack` with `body` and answer the status it settled in.
async fn updated_stack(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack: &str,
    body: &str,
) -> String {
    let (status, updated) = cloudformation(
        router,
        region,
        "UpdateStack",
        &[("StackName", stack), ("TemplateBody", body)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {updated}"
    );
    let described = settled_stack(router, region, case, stack).await;
    opt_element(&described, "StackStatus").unwrap_or_default()
}

/// What Amazon Cognito Identity itself reports for `pool_id`: the domain it holds (absent when
/// it holds none) and the app id of its one login provider.
async fn described_pool(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    pool_id: &str,
) -> (Option<String>, Option<String>) {
    let (status, described) = cognito_identity(
        router,
        region,
        "DescribeIdentityPool",
        &json!({"IdentityPoolId": pool_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeIdentityPool, got {status} with body \
         {described}"
    );
    let domain = described
        .get(DEVELOPER_PROVIDER_NAME)
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let app_id = described
        .get("SupportedLoginProviders")
        .and_then(|providers| providers.get("graph.facebook.com"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    (domain, app_id)
}

/// One row of the table: what the create declared, what the update declares, and whether the
/// rule says the owning API can put that update in force.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the pool lives in.
    region: &'static str,
    /// The developer-provider domain the CREATE declares, `None` for a pool that takes none.
    created_domain: Option<&'static str>,
    /// The domain the UPDATE declares, `None` for an update that states none.
    updated_domain: Option<&'static str>,
    /// Whether the update asks for a change the owning API will not make, which the rule says
    /// fails the resource and rolls the stack back.
    refused: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "an update restating the domain in force",
        region: "us-east-1",
        created_domain: Some("login.example.app"),
        updated_domain: Some("login.example.app"),
        refused: false,
    },
    Row {
        case: "an update naming a different domain",
        region: "us-west-2",
        created_domain: Some("login.example.app"),
        updated_domain: Some("login.example.changed"),
        refused: true,
    },
    Row {
        // The stored value is exact, so a domain differing only in letter case is a different
        // domain and the pool keeps the one it has.
        case: "an update changing only the domain's letter case",
        region: "eu-west-1",
        created_domain: Some("login.example.app"),
        updated_domain: Some("Login.Example.App"),
        refused: true,
    },
    Row {
        case: "an update naming no domain at all",
        region: "us-east-1",
        created_domain: Some("login.example.app"),
        updated_domain: None,
        refused: false,
    },
    Row {
        case: "an update giving a domain to a pool that had none",
        region: "us-west-2",
        created_domain: None,
        updated_domain: Some("login.added.app"),
        refused: false,
    },
    Row {
        case: "a pool that declares no domain either time",
        region: "eu-west-1",
        created_domain: None,
        updated_domain: None,
        refused: false,
    },
    Row {
        case: "a domain spelled with the characters its pattern admits",
        region: "us-east-1",
        created_domain: Some("login_example-app.v2"),
        updated_domain: Some("login_example-app.v3"),
        refused: true,
    },
];

/// Every row: an update the owning API cannot put in force fails that resource and rolls the
/// stack back, leaving the pool and the domain it already had; one it can is applied, and the
/// owning service reports exactly what the template declared.
#[tokio::test]
async fn a_developer_provider_change_the_pool_will_not_take_rolls_the_update_back() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack = format!("developer-provider-stack-{index}");
        let name = format!("developer-provider-pool-{index}");
        // The app id of the pool's login provider is changed by the same update, so every row
        // asks for a change the API CAN make beside the domain it declares: that is what makes
        // a refused row observably "nothing applied" and a served row observably applied.
        let created_app_id = format!("1000000000{index}");
        let updated_app_id = format!("2000000000{index}");

        let pool_id = created_pool(
            &router,
            region,
            case,
            &stack,
            &template(&name, row.created_domain, &created_app_id),
        )
        .await;
        let (domain, app_id) = described_pool(&router, region, case, &pool_id).await;
        assert_eq!(
            (domain.as_deref(), app_id.as_deref()),
            (row.created_domain, Some(created_app_id.as_str())),
            "case {case:?}: expected the create to put the declared configuration in force"
        );

        let settled = updated_stack(
            &router,
            region,
            case,
            &stack,
            &template(&name, row.updated_domain, &updated_app_id),
        )
        .await;
        let events = events_of(&router, region, case, &stack).await;
        let (domain, app_id) = described_pool(&router, region, case, &pool_id).await;

        if row.refused {
            assert_eq!(
                settled, UPDATE_ROLLBACK_COMPLETE,
                "case {case:?}: expected the update to roll back to {UPDATE_ROLLBACK_COMPLETE}, \
                 got {settled:?} with events {events:?}"
            );
            let failed: Vec<(&str, &str)> = events
                .iter()
                .filter(|event| event.status == UPDATE_FAILED)
                .map(|event| (event.logical_id.as_str(), event.resource_type.as_str()))
                .collect();
            assert_eq!(
                failed,
                vec![(POOL_LOGICAL, POOL_TYPE)],
                "case {case:?}: expected exactly the pool to have failed, got {failed:?}"
            );
            let reason = events
                .iter()
                .find(|event| event.status == UPDATE_FAILED)
                .and_then(|event| event.status_reason.clone())
                .unwrap_or_default();
            let asked = row.updated_domain.unwrap_or_default();
            assert!(
                reason.contains(DEVELOPER_PROVIDER_NAME) && reason.contains(asked),
                "case {case:?}: expected the failure reason to name {DEVELOPER_PROVIDER_NAME} \
                 and the declared {asked:?}, got {reason:?}"
            );
            // The pool keeps the domain it had, and the rollback put its other configuration
            // back as it was.
            assert_eq!(
                (domain.as_deref(), app_id.as_deref()),
                (row.created_domain, Some(created_app_id.as_str())),
                "case {case:?}: expected the rolled-back pool to hold what it held before the \
                 update"
            );
            continue;
        }

        assert_eq!(
            settled, UPDATE_COMPLETE,
            "case {case:?}: expected the update to reach {UPDATE_COMPLETE}, got {settled:?} with \
             events {events:?}"
        );
        let failed: Vec<&WireEvent> = events
            .iter()
            .filter(|event| event.status == UPDATE_FAILED)
            .collect();
        assert!(
            failed.is_empty(),
            "case {case:?}: expected no failed resource, got {failed:?}"
        );
        // The domain in force afterwards: the one the update declared where the pool had none
        // to keep, and otherwise the one it already held — an update that states none asks for
        // no change to it.
        let expected_domain = row.created_domain.or(row.updated_domain);
        assert_eq!(
            (domain.as_deref(), app_id.as_deref()),
            (expected_domain, Some(updated_app_id.as_str())),
            "case {case:?}: expected the applied update to leave the declared configuration in \
             force"
        );
    }
}

/// RULE, the other side of it: the refusal damages neither the stack nor the pool — the pool
/// keeps the id the stack published for it, and the very update that was rolled back is served
/// once it restates the domain in force, applying the change it carried beside it.
#[tokio::test]
async fn a_rolled_back_update_leaves_the_pool_updatable() {
    let router = build_router();
    let case = "a rolled-back domain change, then the same change with the domain restated";
    let region = "us-east-1";
    let stack = "developer-provider-recovery";
    let name = "developer-provider-recovery-pool";
    let domain = "login.recovery.app";

    let pool_id = created_pool(
        &router,
        region,
        case,
        stack,
        &template(name, Some(domain), "1111111111"),
    )
    .await;

    let settled = updated_stack(
        &router,
        region,
        case,
        stack,
        &template(name, Some("login.recovery.changed"), "2222222222"),
    )
    .await;
    assert_eq!(
        settled, UPDATE_ROLLBACK_COMPLETE,
        "case {case:?}: expected the refused update to roll back, got {settled:?}"
    );

    // The stack still publishes the SAME pool: a refused update is not a replacement.
    let described = settled_stack(&router, region, case, stack).await;
    let published = outputs_of(&described)
        .get("PoolRef")
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        published, pool_id,
        "case {case:?}: expected the stack to keep publishing {pool_id:?}, got {published:?}"
    );

    let settled = updated_stack(
        &router,
        region,
        case,
        stack,
        &template(name, Some(domain), "3333333333"),
    )
    .await;
    assert_eq!(
        settled, UPDATE_COMPLETE,
        "case {case:?}: expected the update restating the domain to be served, got {settled:?}"
    );
    let (reported_domain, app_id) = described_pool(&router, region, case, &pool_id).await;
    assert_eq!(
        (reported_domain.as_deref(), app_id.as_deref()),
        (Some(domain), Some("3333333333")),
        "case {case:?}: expected the served update to apply the change it carried"
    );
}
