// SPDX-License-Identifier: CC0-1.0

//! The MESSAGE TEXT of three billing refusals, end to end over the awsJson1_0
//! wire — the status and the error code of each is asserted elsewhere; what is
//! asserted here is the sentence each one serializes.
//!
//! Three rules, each asserted over the whole class of inputs it holds for, not
//! over one request:
//!
//! 1. The per-feature billing-access `AccessDeniedException` reports, in its
//!    bracketed `[Operation = …]` segment, the token of the BACKEND operation
//!    that made the decision — the public preference operation's name with the
//!    preference backend's suffix appended — never the public API name alone.
//!    The rows span both preference operations (the read and the write) and every
//!    `BillingFeature` the model declares, so the token tracks the operation and
//!    the feature segment tracks the request.
//!
//! 2. `DisassociateSourceViews` refusing a view that is not `CUSTOM` answers its
//!    OWN single sentence, which is fixed text for the error class: it
//!    interpolates no ARN, name, account or source view. The rows span every
//!    non-`CUSTOM` `BillingViewType` the model declares and requests that differ
//!    in how many source views they name and whether the view holds them, and
//!    the view-lifecycle writers' differently-worded refusal on the same views is
//!    the control.
//!
//! 3. An operation reserved to the organization's management account refuses
//!    every other caller with a sentence that is fixed for the error class: it
//!    interpolates no account id and no value the request carried. The rows span
//!    the ways a caller can fail the gate (no organization at all, an
//!    organization another account manages) crossed with requests that differ in
//!    every member the operation takes, and the management account itself is the
//!    control. Which account manages the organization is AWS Organizations' own
//!    record, read when the decision is made, so one router answers differently
//!    as that record changes.
//!
//! 4. Every read that takes a billing month places it against the month the
//!    clock is in through ONE shared decision, so the reads that gate on that
//!    placement cannot drift apart on which months they refuse or on the
//!    sentence they refuse them with. The rows span the three reads of the
//!    Enterprise Support profile crossed with months on both sides of the
//!    clock's own — the current month, months after it and months before it —
//!    and the Business Support read, whose own doc admits the current month, is
//!    the control that the shared placement is a placement and not one
//!    operation's gate.
//!
//! The harness (`call`, `error_code`, …) is the one `billing_views.rs` owns.

use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_billing_awsbilling::ops::types::BILLING_VIEW_TYPE_VALUES;
use service_com_amazonaws_billing_awsbilling::state::{billing_period, month_index};
use service_com_amazonaws_billing_awsbilling::ComAmazonawsBillingAwsbillingState;
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

// The per-feature harness — the model-derived feature catalogue, the two request
// bodies and the access switch — is the one `billing_feature_access.rs` owns, so
// the feature list exists once for both suites.
use crate::billing_feature_access::{get_body, set_access, update_body, ALL_FEATURES};
use crate::billing_views::{
    call, emulator_with_engine, error_code, set_organization_management_account,
    standalone_emulator_with_engine, ACCOUNT_ID, PRIMARY_ARN,
};
use crate::rejection_fidelity::message;

/// The two public operations that gate a request on per-feature billing access.
const PREFERENCE_OPERATIONS: &[&str] = &["GetBillingPreferences", "UpdateBillingPreferences"];

/// The suffix the preference surface's backend operation token carries on top of
/// the public operation name — spelled out here so the expectation is written
/// independently of the service code that builds the message.
const BACKEND_SUFFIX: &str = "Paperwork";

/// The request body `operation` sends for `feature`.
fn preference_body(operation: &str, feature: &str) -> Value {
    if operation == "GetBillingPreferences" {
        get_body(feature)
    } else {
        update_body(feature)
    }
}

/// The whole message the per-feature refusal must serialize: fixed brackets, the
/// caller's account, the backend token of the operation that refused, and the
/// feature the request named.
fn denied_message(operation: &str, feature: &str) -> String {
    format!(
        "[Exception=AccessDeniedException][AccountId = {ACCOUNT_ID}]\
         [Operation = {operation}{BACKEND_SUFFIX}][Message=No access to feature {feature}]"
    )
}

/// Put the stored view's `billingViewType` at `view_type`: no modelled operation
/// mints a view of any type but `CUSTOM`, so this is how a managed view other
/// than the account's `PRIMARY` one comes to exist in a test.
fn set_view_type(engine: &Arc<engine::InMemoryEngine>, arn: &str, view_type: &str) {
    let state = engine.state::<ComAmazonawsBillingAwsbillingState>();
    let mut state = state.lock();
    state
        .billing_views
        .get_mut(arn)
        .expect("the harness created this view")
        .billing_view_type = view_type.to_string();
}

/// The source views the STORE holds for `arn`.
fn stored_source_views(engine: &Arc<engine::InMemoryEngine>, arn: &str) -> Vec<String> {
    engine
        .state::<ComAmazonawsBillingAwsbillingState>()
        .lock()
        .billing_views
        .get(arn)
        .map(|view| view.source_views.clone())
        .unwrap_or_default()
}

/// Create a `CUSTOM` billing view holding `held` and return its minted ARN.
///
/// A create resolves every ARN its `sourceViews` names against the store, so the
/// views it is handed are planted first: this suite is about the WORDING of a
/// later refusal, not about whether a source exists.
async fn create_view(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
    held: &[String],
) -> String {
    crate::billing_views::plant_source_views(engine, held);
    let (status, created) = call(
        router,
        "CreateBillingView",
        &json!({ "name": name, "sourceViews": held }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the harness create must answer 200, got {status} {created:?}"
    );
    created["arn"].as_str().unwrap_or_default().to_string()
}

/// A pattern-valid `BillingViewArn` for a source view with resource id `id`.
fn source_arn(id: &str) -> String {
    format!("arn:aws:billing::{ACCOUNT_ID}:billingview/{id}")
}

#[tokio::test]
async fn the_per_feature_refusal_names_the_backend_operation_token() {
    // Both preference operations, every feature: the message is built from the
    // account, the backend token of the operation that refused, and the feature
    // the request named — and from nothing else.
    for operation in PREFERENCE_OPERATIONS {
        for feature in ALL_FEATURES {
            let (router, engine) = standalone_emulator_with_engine();
            set_access(&engine, feature, false);
            let case = format!("{operation} on {feature} without access");

            let (status, body) = call(&router, operation, &preference_body(operation, feature)).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (403, "AccessDeniedException"),
                "case {case:?}: expected 403 AccessDeniedException, got {status} {body:?}"
            );
            let stated = message(&body);
            assert_eq!(
                stated,
                denied_message(operation, feature),
                "case {case:?}: expected the bracketed refusal naming the backend operation \
                 token, got {stated:?}"
            );
            // The public API name alone is never the token: it is the backend
            // operation that decided, so the bare name in that segment is the
            // divergence this rule forbids.
            assert!(
                !stated.contains(&format!("[Operation = {operation}]")),
                "case {case:?}: the operation segment must carry the backend token, \
                 got {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_accessible_feature_is_not_refused_at_all() {
    // The control for the rule above: the segment only ever appears on a refusal,
    // so a feature the account can reach answers instead of carrying any token.
    let (router, engine) = standalone_emulator_with_engine();
    for feature in ALL_FEATURES {
        set_access(&engine, feature, true);
        let (status, body) = call(
            &router,
            "GetBillingPreferences",
            &preference_body("GetBillingPreferences", feature),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {feature:?}: an accessible feature answers, got {status} {body:?}"
        );
    }
}

/// AWS's sentence for disassociating source views from a view that is not
/// `CUSTOM`: fixed text for the error class.
const NOT_CUSTOM_SENTENCE: &str = "Only custom billing views can have source views disassociated";

#[tokio::test]
async fn disassociating_from_a_non_custom_view_answers_its_own_sentence() {
    let (router, engine) = emulator_with_engine();

    // Every `BillingViewType` the model declares except `CUSTOM` — source:
    // `ops::types::BILLING_VIEW_TYPE_VALUES` — so the refusal is about the KIND
    // of view and not about the one managed type an account starts with.
    let managed_types: Vec<&&str> = BILLING_VIEW_TYPE_VALUES
        .iter()
        .filter(|view_type| **view_type != "CUSTOM")
        .collect();
    assert!(
        managed_types.len() > 1,
        "case \"model types\": expected several managed types, got {managed_types:?}"
    );

    let held: Vec<String> = (0..10).map(|i| source_arn(&format!("rmt-{i}"))).collect();

    for (index, view_type) in managed_types.iter().enumerate() {
        let arn = create_view(&router, &engine, view_type, &format!("rmt-view-{index}"), &held).await;
        set_view_type(&engine, &arn, view_type);

        // The requests differ in how many source views they name (one, several,
        // the modeled maximum of ten), where in the stored order they sit, and
        // whether the view holds them at all: none of that reaches the message.
        let requests: Vec<(&str, Vec<String>)> = vec![
            ("the first stored source view", vec![held[0].clone()]),
            ("the last stored source view", vec![held[9].clone()]),
            (
                "three of the stored source views",
                vec![held[1].clone(), held[5].clone(), held[8].clone()],
            ),
            ("all ten stored source views", held.clone()),
            (
                "a source view the view does not hold",
                vec![source_arn("never-associated")],
            ),
        ];

        for (what, source_views) in requests {
            let case = format!("{view_type} view, {what}");
            let (status, body) = call(
                &router,
                "DisassociateSourceViews",
                &json!({ "arn": arn, "sourceViews": source_views }),
            )
            .await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "ValidationException"),
                "case {case:?}: expected 400 ValidationException, got {status} {body:?}"
            );
            assert_eq!(
                message(&body),
                NOT_CUSTOM_SENTENCE,
                "case {case:?}: expected this operation's own fixed sentence, got {:?}",
                message(&body)
            );
            assert_eq!(
                stored_source_views(&engine, &arn),
                held,
                "case {case:?}: a refused disassociation leaves the stored set alone"
            );
        }
    }

    // The recorded shape of the refusal: the `PRIMARY` view every account owns,
    // which no test set up.
    let (status, body) = call(
        &router,
        "DisassociateSourceViews",
        &json!({ "arn": PRIMARY_ARN, "sourceViews": [source_arn("never-associated")] }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ValidationException"),
        "case \"the account's PRIMARY view\": expected 400 ValidationException, \
         got {status} {body:?}"
    );
    assert_eq!(
        message(&body),
        NOT_CUSTOM_SENTENCE,
        "case \"the account's PRIMARY view\": expected this operation's own fixed sentence, \
         got {:?}",
        message(&body)
    );
}

/// AWS's sentence pair for an organization-management-account operation called
/// by any other caller: fixed text for the error class, interpolating nothing —
/// no account id, no name, no ARN, no request value.
const NOT_MANAGEMENT_ACCOUNT_SENTENCE: &str =
    "User account is not an AWS Organization's management account. \
     This operation is only permitted to management accounts.";

/// The `CUSTOM` views the account holds, by name.
async fn custom_view_names(router: &MultiServiceRouter) -> Vec<String> {
    let (status, listed) = call(
        router,
        "ListBillingViews",
        &json!({ "billingViewTypes": ["CUSTOM"] }),
    )
    .await;
    assert_eq!(status, 200, "the control list answers, got {listed:?}");
    listed["billingViews"]
        .as_array()
        .expect("billingViews is a list")
        .iter()
        .map(|view| view["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn a_caller_that_is_not_the_management_account_reads_the_fixed_sentence() {
    // The ways the caller can fail the gate: it belongs to no organization at
    // all, or to one a DIFFERENT account manages — two of those, so a message
    // that named the managing account instead of the caller would be caught too.
    let worlds: Vec<(&str, Option<&str>)> = vec![
        ("no organization", None),
        ("an organization managed by another account", Some("111111111111")),
        ("an organization managed by a third account", Some("999999999999")),
    ];

    // Requests differing in every member `CreateBillingView` declares — source:
    // `smithy_query(query="operation", operation="CreateBillingView")`: the name
    // at both modeled length bounds (1 and 128) and in several of the shapes its
    // pattern admits, with and without a description, naming one source view and
    // the modeled maximum of ten, with tags, with a client token, and with a
    // data filter expression. None of it reaches the message.
    let shortest_name = "a".to_string();
    let longest_name = "n".repeat(128);
    let ten_sources: Vec<String> = (0..10).map(|i| source_arn(&format!("mgmt-{i}"))).collect();
    let requests: Vec<(&str, Value)> = vec![
        (
            "the shortest name the model admits",
            json!({ "name": shortest_name, "sourceViews": [PRIMARY_ARN] }),
        ),
        (
            "the longest name the model admits",
            json!({ "name": longest_name, "sourceViews": [PRIMARY_ARN] }),
        ),
        (
            "a name carrying spaces and the punctuation the pattern admits",
            json!({ "name": "a view _+=.-@ 1", "sourceViews": [PRIMARY_ARN] }),
        ),
        (
            "a described view",
            json!({
                "name": "described",
                "description": "a described view",
                "sourceViews": [PRIMARY_ARN],
            }),
        ),
        (
            "the modeled maximum of ten source views",
            json!({ "name": "ten-sources", "sourceViews": ten_sources }),
        ),
        (
            "a tagged view",
            json!({
                "name": "tagged",
                "sourceViews": [PRIMARY_ARN],
                "resourceTags": [{ "key": "team", "value": "finance" }],
            }),
        ),
        (
            "an idempotent request",
            json!({
                "name": "with-token",
                "sourceViews": [PRIMARY_ARN],
                "clientToken": "token-1",
            }),
        ),
        (
            "a filtered view",
            json!({
                "name": "filtered",
                "sourceViews": [PRIMARY_ARN],
                "dataFilterExpression": {
                    "dimensions": { "key": "LINKED_ACCOUNT", "values": [ACCOUNT_ID] },
                },
            }),
        ),
    ];

    for (world, management_account) in &worlds {
        let (router, engine) = standalone_emulator_with_engine();
        set_organization_management_account(&engine, *management_account);

        for (what, body) in &requests {
            let case = format!("{what}, caller in {world}");
            let (status, got) = call(&router, "CreateBillingView", body).await;
            assert_eq!(
                (status, error_code(&got).as_str()),
                (400, "ValidationException"),
                "case {case:?}: expected 400 ValidationException, got {status} {got:?}"
            );
            let stated = message(&got);
            assert_eq!(
                stated, NOT_MANAGEMENT_ACCOUNT_SENTENCE,
                "case {case:?}: expected the fixed management-account sentence pair, \
                 got {stated:?}"
            );
            // Fixed means fixed: no account id — the caller's or the managing
            // one's — and no name the request carried reaches the text, so a
            // digit anywhere in it is an interpolation.
            assert!(
                !stated.chars().any(|character| character.is_ascii_digit()),
                "case {case:?}: the sentence pair names no account id, got {stated:?}"
            );
            // A name short enough to be a substring of ordinary English says
            // nothing, so the check is on the distinctive ones.
            let name = body["name"].as_str().unwrap_or_default();
            assert!(
                name.len() < 4 || !stated.contains(name),
                "case {case:?}: the sentence pair names nothing the request carried, \
                 got {stated:?}"
            );
        }

        let names = custom_view_names(&router).await;
        assert!(
            names.is_empty(),
            "case {world:?}: a refused create stores no view, listed {names:?}"
        );
    }
}

#[tokio::test]
async fn the_gate_follows_the_organization_store_call_by_call() {
    // The standing the gate judges by is AWS Organizations' own record, read
    // when the decision is made — so one router answers differently as that
    // record changes, and nothing this service stores can stand in for it. The
    // rows span the ways the record can stand: no organization, one the caller
    // manages, and one each of two OTHER accounts manage.
    let (router, engine) = standalone_emulator_with_engine();
    let worlds: Vec<(&str, Option<&str>, bool)> = vec![
        ("no organization", None, false),
        ("an organization the caller manages", Some(ACCOUNT_ID), true),
        (
            "an organization another account manages",
            Some("111111111111"),
            false,
        ),
        ("the caller manages it again", Some(ACCOUNT_ID), true),
        (
            "an organization a third account manages",
            Some("999999999999"),
            false,
        ),
    ];

    let mut created_arns: Vec<String> = Vec::new();
    for (index, (world, management_account, served)) in worlds.iter().enumerate() {
        set_organization_management_account(&engine, *management_account);
        let body = json!({
            "name": format!("call-by-call-{index}"),
            "sourceViews": [PRIMARY_ARN],
        });
        let (status, got) = call(&router, "CreateBillingView", &body).await;
        if *served {
            assert_eq!(
                status, 200,
                "case {world:?}: expected the create to be served, got {status} {got:?}"
            );
            created_arns.push(got["arn"].as_str().unwrap_or_default().to_string());
        } else {
            assert_eq!(
                (status, error_code(&got).as_str()),
                (400, "ValidationException"),
                "case {world:?}: expected 400 ValidationException, got {status} {got:?}"
            );
            assert_eq!(
                message(&got),
                NOT_MANAGEMENT_ACCOUNT_SENTENCE,
                "case {world:?}: expected the fixed management-account sentence pair, got {:?}",
                message(&got)
            );
        }

        // Whatever the record says now, the views already created stay stored
        // and readable: the fact gates the WRITE, it does not withdraw a view.
        for arn in &created_arns {
            let (status, read) = call(&router, "GetBillingView", &json!({ "arn": arn })).await;
            assert_eq!(
                status, 200,
                "case {world:?}: the view created earlier still reads back, got {status} {read:?}"
            );
        }
        let names = custom_view_names(&router).await;
        assert_eq!(
            names.len(),
            created_arns.len(),
            "case {world:?}: exactly the served creates are listed, listed {names:?}"
        );
    }
}

#[tokio::test]
async fn the_management_account_itself_is_not_refused() {
    // The control for the rule above: the sentence belongs to the refusal, so
    // the one caller the gate admits — the organization's management account —
    // creates the same views instead of reading it.
    let (router, engine) = emulator_with_engine();
    let ten_sources: Vec<String> = (0..10).map(|i| source_arn(&format!("mgmt-ok-{i}"))).collect();
    crate::billing_views::plant_source_views(&engine, &ten_sources);

    for (case, body) in [
        (
            "one source view",
            json!({ "name": "admitted-one", "sourceViews": [PRIMARY_ARN] }),
        ),
        (
            "ten source views",
            json!({ "name": "admitted-ten", "sourceViews": ten_sources }),
        ),
    ] {
        let (status, created) = call(&router, "CreateBillingView", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the management account creates the view, got {status} {created:?}"
        );
        let arn = created["arn"].as_str().unwrap_or_default().to_string();
        let (status, got) = call(&router, "GetBillingView", &json!({ "arn": arn })).await;
        assert_eq!(
            (status, got["billingView"]["name"].as_str()),
            (200, body["name"].as_str()),
            "case {case:?}: the created view reads back, got {status} {got:?}"
        );
    }
}

#[tokio::test]
async fn the_view_lifecycle_writers_keep_their_own_wording() {
    // The sentence is per OPERATION: the writers that refuse the same view for
    // the same reason word it their own way, so the disassociation sentence must
    // not be theirs and theirs must not be the disassociation's.
    let (router, _engine) = emulator_with_engine();

    let (status, body) = call(
        &router,
        "DisassociateSourceViews",
        &json!({ "arn": PRIMARY_ARN, "sourceViews": [source_arn("a")] }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"disassociate PRIMARY\": got {status} {body:?}"
    );
    let disassociate = message(&body);

    for (case, operation, request) in [
        (
            "update PRIMARY",
            "UpdateBillingView",
            json!({ "arn": PRIMARY_ARN, "name": "renamed" }),
        ),
        (
            "delete PRIMARY",
            "DeleteBillingView",
            json!({ "arn": PRIMARY_ARN }),
        ),
    ] {
        let (status, body) = call(&router, operation, &request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} {body:?}"
        );
        assert_ne!(
            message(&body),
            disassociate,
            "case {case:?}: this writer words its own refusal, got {:?}",
            message(&body)
        );
    }
}

/// The request member every read of this service takes a billing month in.
const BILLING_MONTH_MEMBER: &str = "billingMonth";

/// The three reads of the Enterprise Support profile, each of which takes a
/// modeled `EnterpriseSupportBillingMonth`: "The billing month in YYYY-MM
/// format. This must be a month in the past."
///
/// Source: the `billingMonth` member of each operation's input, as
/// `smithy_query(query="operation", operation="GetEnterpriseSupportCharge\
/// Summary,GetEnterpriseSupportContractDetails,ListEnterpriseSupportLinked\
/// AccountCharges")` reports it.
const ENTERPRISE_SUPPORT_READS: &[&str] = &[
    "GetEnterpriseSupportChargeSummary",
    "GetEnterpriseSupportContractDetails",
    "ListEnterpriseSupportLinkedAccountCharges",
];

/// The read whose `BusinessSupportBillingMonth` admits one month more: "You can
/// request the current month (charges will be estimated) or a past month
/// (charges will be finalized)."
const BUSINESS_SUPPORT_READ: &str = "ListBusinessSupportAccountCharges";

/// The constraint a month that is not in the past breaks, and the one a month
/// that has not begun breaks: each continues the sentence the member name
/// starts.
const NOT_PAST_CONSTRAINT: &str = "must be a month in the past";
const NOT_CURRENT_OR_EARLIER_CONSTRAINT: &str =
    "must be the current billing month or an earlier one";

/// The whole message a refused billing month serializes for `constraint`.
fn month_refusal_message(constraint: &str) -> String {
    format!(
        "InvalidArgument: {BILLING_MONTH_MEMBER} \
         (DetailedCode={BILLING_MONTH_MEMBER} {constraint})"
    )
}

/// The one `ValidationExceptionField` a refused billing month reports: the
/// member it names and that member's own sentence.
fn refused_field(body: &Value) -> (Option<&str>, Option<&str>) {
    (
        body["fieldList"][0]["name"].as_str(),
        body["fieldList"][0]["message"].as_str(),
    )
}

/// The billing month `offset` whole calendar months from the one the clock is
/// in, in the modeled `YYYY-MM` form — built from the service's own reading of
/// the clock (`state::billing_period`) and its own month arithmetic
/// (`state::month_index`), so a row names a month relative to today's rather
/// than a month one recording happened in.
fn month_from_now(offset: i32) -> String {
    let current = billing_period(SystemTime::now()).expect("the clock names a billing month");
    let index = month_index(current) + offset;
    format!("{:04}-{:02}", index.div_euclid(12), index.rem_euclid(12) + 1)
}

/// Ask `operation` for one billing month.
async fn for_month(router: &MultiServiceRouter, operation: &str, month: &str) -> (u16, Value) {
    call(router, operation, &json!({ "billingMonth": month })).await
}

#[tokio::test]
async fn every_enterprise_support_read_refuses_the_same_months_in_the_same_words() {
    let (router, _engine) = standalone_emulator_with_engine();
    let field_sentence = format!("Member {NOT_PAST_CONSTRAINT}");

    // The months the modeled "a month in the past" excludes: the one the clock
    // is in, the one after it, one later in the same year and one past the turn
    // of the year — so the gate is shown to be about where a month sits, not
    // about one month's digits.
    for offset in [0_i32, 1, 2, 13, 121] {
        let month = month_from_now(offset);
        let mut answers: Vec<(u16, String, String)> = Vec::new();
        for operation in ENTERPRISE_SUPPORT_READS {
            let case = format!("{operation} on {month}, {offset} months from now");
            let (status, body) = for_month(&router, operation, &month).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "ValidationException"),
                "case {case:?}: expected 400 ValidationException, got {status} {body:?}"
            );
            assert_eq!(
                message(&body),
                month_refusal_message(NOT_PAST_CONSTRAINT),
                "case {case:?}: expected the shared past-month sentence, got {:?}",
                message(&body)
            );
            assert_eq!(
                refused_field(&body),
                (Some(BILLING_MONTH_MEMBER), Some(field_sentence.as_str())),
                "case {case:?}: expected the refusal to name the member and its constraint, \
                 got {body:?}"
            );
            answers.push((status, error_code(&body), message(&body)));
        }
        // One rule decided once: the three reads answer the same month
        // identically, so none of them can drift from the others.
        assert!(
            answers.windows(2).all(|pair| pair[0] == pair[1]),
            "case {month:?}: expected one answer from every Enterprise Support read, \
             got {answers:?}"
        );
    }

    // ...and the months on the other side of the clock's own are answered by
    // all three, so the gate refuses a CLASS of months rather than every month.
    for offset in [-1_i32, -2, -13, -121] {
        let month = month_from_now(offset);
        for operation in ENTERPRISE_SUPPORT_READS {
            let case = format!("{operation} on {month}, {offset} months from now");
            let (status, body) = for_month(&router, operation, &month).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected a closed month to be answered, got {status} {body:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_business_support_read_gates_the_same_placement_by_its_own_doc() {
    let (router, _engine) = standalone_emulator_with_engine();

    // The placement is shared; what each operation's doc admits of it is not.
    // This read admits the current month, and whether the month's charges are
    // estimated is that same placement — so the current month is answered here
    // and refused by the three reads above.
    let current = month_from_now(0);
    let (status, body) = for_month(&router, BUSINESS_SUPPORT_READ, &current).await;
    assert_eq!(
        status, 200,
        "case \"the current month\": expected HTTP 200 for {current}, got {status} {body:?}"
    );
    // Guard against the calendar turning over mid-test: the month asked for is
    // only the current one while it still is.
    if month_from_now(0) == current {
        assert_eq!(
            body["isEstimated"].as_bool(),
            Some(true),
            "case \"the current month\": expected the open month's charges estimated, \
             got {body:?}"
        );
        for operation in ENTERPRISE_SUPPORT_READS {
            let case = format!("{operation} on the current month");
            let (status, refused) = for_month(&router, operation, &current).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (400, "ValidationException"),
                "case {case:?}: expected the open month refused, got {status} {refused:?}"
            );
        }
    }

    // A closed month is finalized by the same placement.
    for offset in [-1_i32, -12] {
        let month = month_from_now(offset);
        let case = format!("{offset} months from now");
        let (status, body) = for_month(&router, BUSINESS_SUPPORT_READ, &month).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for {month}, got {status} {body:?}"
        );
        assert_eq!(
            body["isEstimated"].as_bool(),
            Some(false),
            "case {case:?}: expected a closed month's charges finalized, got {body:?}"
        );
    }

    // A month that has not begun is neither, and it is refused in THIS
    // operation's words rather than the Enterprise reads'.
    let field_sentence = format!("Member {NOT_CURRENT_OR_EARLIER_CONSTRAINT}");
    for offset in [1_i32, 2, 13, 121] {
        let month = month_from_now(offset);
        let case = format!("{offset} months from now");
        let (status, body) = for_month(&router, BUSINESS_SUPPORT_READ, &month).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException for {month}, got {status} {body:?}"
        );
        assert_eq!(
            message(&body),
            month_refusal_message(NOT_CURRENT_OR_EARLIER_CONSTRAINT),
            "case {case:?}: expected this read's own sentence, got {:?}",
            message(&body)
        );
        assert_eq!(
            refused_field(&body),
            (Some(BILLING_MONTH_MEMBER), Some(field_sentence.as_str())),
            "case {case:?}: expected the refusal to name the member and its constraint, \
             got {body:?}"
        );
    }
}
