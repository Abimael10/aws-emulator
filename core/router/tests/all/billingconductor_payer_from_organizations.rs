// SPDX-License-Identifier: CC0-1.0

//! Billing Conductor's payer-account gate opens for the account AWS Organizations records
//! as the management account of the organization it belongs to.
//!
//! RULE under test: the payer account of a consolidated billing family IS the management
//! account of the organization — "the account whose user is calling the
//! `CreateOrganization` operation automatically becomes the management account of the new
//! organization" — so the standing Billing Conductor's payer-only control plane is
//! reserved to is a fact the AWS Organizations control plane owns, read from THAT state
//! when the decision is made. A client that speaks only HTTP therefore reaches this
//! control plane by creating an organization, while an engine whose stores record no
//! organization and no payer designation has no payer account at all and every gated
//! operation stays refused HTTP 403 `AccessDeniedException` with the request-derived
//! `Message` (the calling account id and the operation name).
//!
//! Only a cross-service suite can pin this: the service's own suites
//! (`tests/all/payer_account_gate.rs` and its siblings) can assert its half alone, and
//! `core/router/tests/all/billingconductor_payer_account.rs` drives the gate over a store
//! whose payer slot a test wrote directly.
//!
//! The rows span what the rule generalises over: which account the organization's record
//! names as its management account (none at all, another account, the presented one), the
//! feature set the organization was created with (every value the model's
//! `OrganizationFeatureSet` declares, plus the request that omits the member), the kind of
//! gated operation (a create, a paginated read, a second read over another collection, an
//! association, a delete) and the shape of the create behind the opened gate (with and
//! without tags, a description, a primary account and an idempotency token).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_billingconductor_awsbillingconductor::ComAmazonawsBillingconductorAwsbillingconductorState as BillingConductorState;
use service_com_amazonaws_organizations_awsorganizationsv20161128::ops::types::ORGANIZATION_FEATURE_SET_VALUES;
use service_com_amazonaws_organizations_awsorganizationsv20161128::state::ComAmazonawsOrganizationsAwsorganizationsv20161128State as OrganizationsState;
use smithy_server_runtime::MultiServiceRouter;

use crate::billingconductor_payer_account::link_account_to_the_organization;

/// The region every request below is signed for; neither service's resources are
/// regional, so no assertion depends on it.
const REGION: &str = "us-east-1";

/// A 12-digit account that is not the one this emulator's callers authenticate as.
const ANOTHER_ACCOUNT: &str = "111122223333";

/// A second one, a digit from the first: the standing is decided by the record, not by how
/// closely an id resembles the caller's.
const ANOTHER_ACCOUNT_NEAR: &str = "111122223334";

/// The account the billing groups below group beside the management account: a linked
/// account of the consolidated billing family, which it becomes by being a member account
/// of the organization.
const MEMBER_ACCOUNT: &str = "000000000007";

/// A second member account, so an association has a family account to add that no group
/// already holds.
const SECOND_MEMBER_ACCOUNT: &str = "000000000008";

/// The account every emulated service presents, and so the management account of the
/// organization `CreateOrganization` creates.
fn account_id() -> &'static str {
    engine::emulator_account_id()
}

/// Billing Conductor and Organizations mounted on ONE engine, as `core/router/src/lib.rs`
/// mounts them, over the state a freshly booted emulator holds: an account in no
/// organization, and a Billing Conductor store that designates no payer account.
fn build_router() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_billingconductor_awsbillingconductor::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_organizations_awsorganizationsv20161128::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (engine, router)
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is the
/// routing key the front door reads. The signature is never checked: the router here runs
/// with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0"
    )
}

/// One Billing Conductor call, over its `restJson1` wire: the operation IS the path.
async fn billingconductor(
    router: &MultiServiceRouter,
    path: &str,
    body: &Value,
) -> (u16, Value, Option<String>) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://billingconductor.{REGION}.amazonaws.com{path}"
        ))
        .header("host", format!("billingconductor.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("billingconductor"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed, error_type)
}

/// One Organizations call, over that service's `awsJson1_1` wire (routed by
/// `X-Amz-Target`).
async fn organizations(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("organizations.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSOrganizationsV20161128.{operation}"),
        )
        .header("authorization", authorization("organizations"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The `restJson1` error code a body carries, without its namespace.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `Message` the gate owes for `operation`, read from the SERVICE's own builder rather
/// than re-spelled here, so this suite cannot drift from the text the service puts on the
/// wire.
fn denial_message(operation: &str) -> String {
    service_com_amazonaws_billingconductor_awsbillingconductor::state::not_payer_account(operation)
        .message
}

/// Create an organization through Organizations' own `CreateOrganization`, the one request
/// that makes the calling account a management account, and answer the management account
/// the organization's record names.
async fn create_organization(router: &MultiServiceRouter, feature_set: Option<&str>) -> String {
    let request = match feature_set {
        None => json!({}),
        Some(value) => json!({"FeatureSet": value}),
    };
    let (status, created) = organizations(router, "CreateOrganization", &request).await;
    assert_eq!(
        status, 200,
        "setup CreateOrganization {feature_set:?}: expected 200, got {status} with {created}"
    );
    created["Organization"]["MasterAccountId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateOrganization reports a MasterAccountId, got {created}"))
        .to_string()
}

/// Hand the organization to `management_account_id` in the Organizations store.
///
/// No request of either API transfers an organization's management account, so this is
/// written into the store the Organizations handlers themselves answer from — the same
/// arrangement that service's other cross-service suites make — and only the one member
/// the record names is changed.
fn hand_organization_to(engine: &Arc<engine::InMemoryEngine>, management_account_id: &str) {
    let state = engine.state::<OrganizationsState>();
    let mut store = state.lock();
    store
        .organization
        .as_mut()
        .expect("the account must already be in an organization")
        .master_account_id = management_account_id.to_string();
}

/// One gated call per KIND of operation: `(case, operation, path, body)`. None of the
/// resources these bodies name exists, so a denial that leaked a lookup would show up as
/// a 404 or a `ValidationException` instead of the 403.
fn gated_calls() -> Vec<(&'static str, &'static str, &'static str, Value)> {
    let absent_group = format!(
        "arn:aws:billingconductor::{}:billinggroup/ABCDEFGHIJ",
        account_id()
    );
    vec![
        (
            "a create",
            "CreatePricingPlan",
            "/create-pricing-plan",
            json!({"Name": "PayerOnlyPlan"}),
        ),
        (
            "a paginated read",
            "ListBillingGroups",
            "/list-billing-groups",
            json!({"MaxResults": 10}),
        ),
        (
            "a read over another collection",
            "ListAccountAssociations",
            "/list-account-associations",
            json!({}),
        ),
        (
            "an association",
            "AssociateAccounts",
            "/associate-accounts",
            json!({"Arn": absent_group.clone(), "AccountIds": [MEMBER_ACCOUNT]}),
        ),
        (
            "a delete",
            "DeleteBillingGroup",
            "/delete-billing-group",
            json!({"Arn": absent_group}),
        ),
    ]
}

/// Assert every gated call is refused the payer-account denial, in the one shape the model
/// declares for it: the status, the wire error code in both places `restJson1` carries it,
/// and the `Message` built from the request.
async fn assert_every_gated_call_is_denied(router: &MultiServiceRouter, world: &str) {
    for (kind, operation, path, body) in gated_calls() {
        let case = format!("{kind} ({operation}) with {world}");
        let (status, answered, error_type) = billingconductor(router, path, &body).await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected HTTP 403, got {status} with {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "AccessDeniedException",
            "case {case:?}: expected the modelled AccessDeniedException, got {answered}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("AccessDeniedException"),
            "case {case:?}: expected x-amzn-errortype: AccessDeniedException, got {error_type:?}"
        );
        assert_eq!(
            answered["Message"].as_str(),
            Some(denial_message(operation).as_str()),
            "case {case:?}: expected the request-derived Message {:?}, got {answered}",
            denial_message(operation)
        );
    }
}

/// The `CreateBillingGroup` requests driven behind an opened gate, differing in the
/// members the operation takes: the accounts grouped, whether a primary account is
/// designated, a description and tags. Source:
/// `smithy_query(query="operation", operation="CreateBillingGroup")`.
///
/// `plan_arn` is a pricing plan created over the wire first, and every account named is a
/// linked account of the family, which is what the operation requires of them.
fn create_group_requests(plan_arn: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "the management account alone",
            json!({
                "Name": "PayerGroupOne",
                "AccountGrouping": {"LinkedAccountIds": [account_id()]},
                "ComputationPreference": {"PricingPlanArn": plan_arn},
            }),
        ),
        (
            "a member account, designated the primary account",
            json!({
                "Name": "PayerGroupTwo",
                "AccountGrouping": {"LinkedAccountIds": [MEMBER_ACCOUNT]},
                "ComputationPreference": {"PricingPlanArn": plan_arn},
                "PrimaryAccountId": MEMBER_ACCOUNT,
                "Description": "the team that pays",
                "Tags": {"owner": "finance"},
            }),
        ),
        (
            "a request naming no account at all",
            json!({
                "Name": "PayerGroupThree",
                "AccountGrouping": {"LinkedAccountIds": []},
                "ComputationPreference": {"PricingPlanArn": plan_arn},
            }),
        ),
    ]
}

/// The ARNs of the billing groups the account holds, read back through the paginated list.
async fn listed_group_arns(router: &MultiServiceRouter) -> Vec<String> {
    let (status, listed, _) = billingconductor(router, "/list-billing-groups", &json!({})).await;
    assert_eq!(
        status, 200,
        "the control listing answers the payer account, got {status} with {listed}"
    );
    listed["BillingGroups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|group| group["Arn"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// RULE: creating an organization over the wire makes the calling account its management
/// account, which IS the payer account of its consolidated billing family — so every
/// payer-gated operation, refused 403 before, is served afterwards and what it stores
/// reads back through the paginated list. Driven for every feature set a client can ask
/// the organization to be created with.
#[tokio::test]
async fn creating_an_organization_over_the_wire_opens_the_payer_gated_control_plane() {
    // Every value the model declares (source: `ORGANIZATION_FEATURE_SET_VALUES`, the
    // model's `OrganizationFeatureSet` enum), plus the request that omits the member and
    // takes the operation's documented default. The calling account becomes the
    // management account in each, so the gate must open in each.
    let mut feature_sets: Vec<Option<&str>> = vec![None];
    feature_sets.extend(ORGANIZATION_FEATURE_SET_VALUES.iter().copied().map(Some));

    for feature_set in feature_sets {
        let world = format!("featureSet {feature_set:?}");
        let (engine, router) = build_router();

        // Before any organization exists the account is standalone and nothing designated
        // a payer, so the whole control plane is refused.
        assert_every_gated_call_is_denied(&router, &format!("no organization, {world}")).await;

        // The one request that establishes the fact, sent by a client that speaks only
        // HTTP.
        let management = create_organization(&router, feature_set).await;
        assert_eq!(
            management,
            account_id(),
            "case {world:?}: the calling account manages the new organization"
        );

        // Behind the established fact the control plane is served. A pricing plan first:
        // a billing group's `ComputationPreference` names one.
        let (status, plan, _) = billingconductor(
            &router,
            "/create-pricing-plan",
            &json!({"Name": "PayerPlan"}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {world:?}: expected CreatePricingPlan to be served, got {status} with {plan}"
        );
        let plan_arn = plan["Arn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {world:?}: the created plan carries an Arn, got {plan}"))
            .to_string();

        // The refused calls above stored nothing: the store holds no billing group yet.
        assert!(
            listed_group_arns(&router).await.is_empty(),
            "case {world:?}: a refused create must store no billing group"
        );

        // The member accounts of the organization, so a group can be created and grown
        // over accounts other than the management one: an account joins a consolidated
        // billing family in AWS Organizations and no Billing Conductor request puts one
        // there.
        link_account_to_the_organization(&engine, MEMBER_ACCOUNT);
        link_account_to_the_organization(&engine, SECOND_MEMBER_ACCOUNT);

        let mut minted = Vec::new();
        for (shape, body) in create_group_requests(&plan_arn) {
            let case = format!("{shape}, {world}");
            let (status, created, _) =
                billingconductor(&router, "/create-billing-group", &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected CreateBillingGroup to be served, got {status} \
                 with {created}"
            );
            let arn = created["Arn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: the created group carries an Arn"))
                .to_string();
            assert!(
                arn.starts_with(&format!(
                    "arn:aws:billingconductor::{}:billinggroup/",
                    account_id()
                )),
                "case {case:?}: expected a billing-group ARN of the payer account, got {arn:?}"
            );
            minted.push(arn);
        }

        // Every served create reads back through the list, and nothing else does.
        let listed = listed_group_arns(&router).await;
        assert_eq!(
            listed.len(),
            minted.len(),
            "case {world:?}: expected the {} created groups to be listed, got {listed:?}",
            minted.len()
        );
        for arn in &minted {
            assert!(
                listed.contains(arn),
                "case {world:?}: expected the created group {arn} among {listed:?}"
            );
        }

        // The association, over a member account of the family the groups above left
        // ungrouped, and the read that reports the family.
        let group = minted
            .first()
            .expect("the create table mints at least one group")
            .clone();
        let (status, associated, _) = billingconductor(
            &router,
            "/associate-accounts",
            &json!({"Arn": group, "AccountIds": [SECOND_MEMBER_ACCOUNT]}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {world:?}: expected AssociateAccounts to be served, got {status} \
             with {associated}"
        );
        // And a request the OPEN gate let through is decided by the operation's own rules:
        // an account the organization does not hold is not in the consolidated billing
        // family, which is the modelled 400 rather than the gate's 403.
        let (status, refused, _) = billingconductor(
            &router,
            "/associate-accounts",
            &json!({"Arn": group, "AccountIds": [ANOTHER_ACCOUNT_NEAR]}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {world:?}: an account outside the family cannot be grouped, got {status} \
             with {refused}"
        );
        let (status, associations, _) =
            billingconductor(&router, "/list-account-associations", &json!({})).await;
        assert_eq!(
            status, 200,
            "case {world:?}: expected ListAccountAssociations to be served, got {status} \
             with {associations}"
        );
        let reported: Vec<(&str, &str)> = associations["LinkedAccounts"]
            .as_array()
            .map(|accounts| {
                accounts
                    .iter()
                    .map(|account| {
                        (
                            account["AccountId"].as_str().unwrap_or_default(),
                            account["BillingGroupArn"].as_str().unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        for account in [account_id(), MEMBER_ACCOUNT, SECOND_MEMBER_ACCOUNT] {
            assert!(
                reported.iter().any(|(id, _)| *id == account),
                "case {world:?}: expected the family account {account} to be reported, got \
                 {associations}"
            );
        }
        assert!(
            reported
                .iter()
                .any(|(id, billing_group)| *id == SECOND_MEMBER_ACCOUNT
                    && *billing_group == group),
            "case {world:?}: expected the associated account to report its billing group, \
             got {associations}"
        );

        // And the delete the gate refused before is served too, which the list confirms.
        let (status, deleted, _) =
            billingconductor(&router, "/delete-billing-group", &json!({"Arn": group})).await;
        assert_eq!(
            status, 200,
            "case {world:?}: expected DeleteBillingGroup to be served, got {status} \
             with {deleted}"
        );
        let listed = listed_group_arns(&router).await;
        assert!(
            !listed.contains(&group),
            "case {world:?}: the deleted group must be gone, got {listed:?}"
        );
    }
}

/// RULE: the standing is the organization's RECORD of its management account, so an
/// organization another account manages leaves this caller a non-payer and the control
/// plane refused — the gate is not "an organization exists".
#[tokio::test]
async fn an_organization_another_account_manages_leaves_the_control_plane_refused() {
    for management in [ANOTHER_ACCOUNT, ANOTHER_ACCOUNT_NEAR] {
        let (engine, router) = build_router();
        let created = create_organization(&router, None).await;
        assert_eq!(
            created,
            account_id(),
            "setup: the calling account manages the organization it created"
        );
        hand_organization_to(&engine, management);

        assert_every_gated_call_is_denied(
            &router,
            &format!("an organization managed by {management}"),
        )
        .await;

        // Handing it back makes the same caller the payer again, with no Billing Conductor
        // request in between: the decision is read from the organization's record at the
        // time of the call.
        hand_organization_to(&engine, account_id());
        let (status, served, _) = billingconductor(
            &router,
            "/create-pricing-plan",
            &json!({"Name": "PayerPlanAgain"}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {management:?}: expected the management account to be served, got {status} \
             with {served}"
        );
    }
}

/// RULE: the organization's record is not the only source of the standing — a store that
/// designates a payer account answers by that designation, which is the seam for an engine
/// mounting no Organizations surface, and a store with neither refuses.
#[tokio::test]
async fn a_designated_payer_account_still_authorizes_without_an_organization() {
    // (case, the account the Billing Conductor store designates, whether the control plane
    // is served)
    let rows: [(&str, Option<&str>, bool); 3] = [
        ("no designation at all", None, false),
        ("another account designated", Some(ANOTHER_ACCOUNT), false),
        ("the calling account designated", Some(account_id()), true),
    ];

    for (case, designated, served) in rows {
        let (engine, router) = build_router();
        engine
            .state::<BillingConductorState>()
            .lock()
            .payer_account_id = designated.map(str::to_string);

        let (status, answered, _) = billingconductor(
            &router,
            "/create-pricing-plan",
            &json!({"Name": "DesignatedPayerPlan"}),
        )
        .await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the designated payer account to be served, got \
                 {status} with {answered}"
            );
        } else {
            assert_eq!(
                status, 403,
                "case {case:?}: expected HTTP 403, got {status} with {answered}"
            );
            assert_eq!(
                answered["Message"].as_str(),
                Some(denial_message("CreatePricingPlan").as_str()),
                "case {case:?}: expected the request-derived Message, got {answered}"
            );
        }
    }
}
