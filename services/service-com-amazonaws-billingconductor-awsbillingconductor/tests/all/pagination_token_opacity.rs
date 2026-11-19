// SPDX-License-Identifier: CC0-1.0

//! What a `NextToken` of this service IS, end to end over the wire, and what every
//! paginated read answers a caller that hands back one it was never given.
//!
//! RULE under test: a pagination token of this service is OPAQUE and belongs to the
//! listing that returned it — the only thing a caller may do with one is hand it back,
//! unchanged, to the same listing — so a `NextToken` the service did not mint for THAT
//! listing (a word, an empty value, a store key, a value minted by another listing or for
//! another parent resource, a minted value one character of which changed) is refused with
//! the modelled 400 `ValidationException`, `Reason` `CANNOT_PARSE` and `Fields` naming
//! `NextToken`, rather than being read as a cut point and answered 200 with an arbitrarily
//! truncated page. A token a listing DID mint resumes exactly where its page stopped, and
//! carries neither an ARN nor an account id nor an index a caller could edit.
//!
//! A read ANSWERED FOR ONE BILLING PERIOD belongs to that month as well as to the listing,
//! which [`a_period_scoped_token_is_refused_by_a_read_of_another_billing_period`] and
//! [`a_period_scoped_token_resumes_every_request_naming_the_month_it_was_minted_for`] pin:
//! the month decides which resources were in effect and what every reported element says
//! about them, so each month is its own sequence, a token is refused by a read of any other
//! month, and every spelling of the month it was minted for resumes it.
//!
//! The rows span what the rule generalises over rather than one reported call: EVERY
//! paginated read this service implements (whether its cursor is a stored ARN, an account
//! id, a fixed-width position or a composite report key, and whether its sequence is the
//! account's or one parent resource's), and, per read, the class of the rejected value —
//! not hex at all, hex of the wrong length, hex of bytes that are not text, a bare store
//! key, hex of a cursor carrying no scope, a token minted by each of the other listings, a
//! token minted for another parent of the same listing, and a minted token with one
//! character changed.
//!
//! Every assertion goes over the service's real `restJson1` wire through
//! [`smithy_server_runtime::MultiServiceRouter::dispatch`], on the shared harness of
//! `tests/all/payer_account_gate.rs` — this service's control plane answers the payer
//! account alone.

use serde_json::{json, Value};

use crate::cost_report_token_breakdown_scope::one_digit_month;
use crate::payer_account_gate::{error_code, Emulator};

/// How many pricing plans, pricing rules, billing groups and custom line items the
/// fixture creates. Three is enough that a one-row page always leaves a remainder, so
/// every read that can be asked for one row hands back a token.
const POPULATION: usize = 3;

/// A bound on the pages a walk may take before it is called non-terminating: far above the
/// rows the fixture holds, so reaching it means the paging never ended.
const MAX_PAGES: usize = 16;

/// One paginated read of this service.
struct Read {
    /// The modelled operation, which is also the case name.
    operation: &'static str,
    /// The route the model binds it to.
    path: &'static str,
    /// The output member its page travels in.
    member: &'static str,
    /// The request, minus `NextToken`: the members that pick the sequence being read.
    body: Value,
    /// A SECOND request for the same read against another parent resource, for the reads
    /// whose sequence belongs to one resource. `None` for a read of the account's own
    /// collection, which has no second sequence to confuse.
    other_parent: Option<Value>,
    /// Can this read be asked for a page small enough that the fixture's sequence is cut
    /// into more than one? `ListAccountAssociations` models no `MaxResults` at all and
    /// `GetBillingGroupCostReport`'s is `@range(200..300)`, so neither mints a token over
    /// a fixture this size; their minted-token walks are
    /// `tests/all/billing_group_core_loop.rs` and `tests/all/billing_group_cost_report.rs`.
    ///
    /// In [`period_scoped_fixture`], whose rows ask for a page through [`page_request`],
    /// this says whether the read's own `MaxResults` picks the page: a read that models
    /// none is paged at the server's own page ([`SERVER_PAGE`]) instead, over a fixture
    /// that holds one row more than that.
    mints: bool,
}

/// The fixture: a live emulator with every collection these reads page over populated.
struct World {
    em: Emulator,
    reads: Vec<Read>,
    /// A stored key of this service, as a value that was a legal cursor before a token was
    /// opaque: the first billing group's full ARN.
    stored_arn: String,
    /// Another stored key class: a linked account id, the cursor `ListAccountAssociations`
    /// pages over.
    stored_account: String,
}

/// The linked account id of the `index`th billing group: twelve digits, as `AccountId`'s
/// modelled `^[0-9]{12}$` requires, and distinct per group so no account is claimed twice.
fn linked_account(index: usize) -> String {
    format!("{:012}", 100_000_000_000_u64 + index as u64)
}

/// The `BillingPeriod` `months` after the current one (negative for earlier), zero-padded
/// as the modelled `^\d{4}-(0?[1-9]|1[012])$` allows.
///
/// Read off the service's own clock rather than fixed, because a write may only take
/// effect in the current or the previous billing period.
///
/// `pub(crate)` because the sibling suite `tests/all/cost_report_token_breakdown_scope.rs`
/// names the same periods relative to the same clock rather than re-spelling this.
pub(crate) fn period(months: i64) -> String {
    let current =
        service_com_amazonaws_billingconductor_awsbillingconductor::state::current_billing_period();
    let (year, month) = current
        .split_once('-')
        .expect("the current billing period is spelled YYYY-MM");
    let total = year.parse::<i64>().expect("a four-digit year") * 12
        + month.parse::<i64>().expect("a month")
        - 1
        + months;
    format!("{:04}-{:02}", total.div_euclid(12), total.rem_euclid(12) + 1)
}

/// Send one setup call that must succeed, and return its body.
async fn setup(em: &Emulator, method: &str, path: &str, body: &Value, label: &str) -> Value {
    let (status, response, _) = em.call(method, path, Some(body)).await;
    assert_eq!(
        status, 200,
        "setup {label}: expected HTTP 200 from {path}, got {status} with {response}"
    );
    response
}

/// Send one setup call that must succeed, and return the `Arn` it answered with.
async fn setup_arn(em: &Emulator, method: &str, path: &str, body: &Value, label: &str) -> String {
    let response = setup(em, method, path, body, label).await;
    response
        .get("Arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {label}: expected an Arn member, got {response}"))
        .to_string()
}

/// Stand up the fixture: three pricing plans, three pricing rules (all of them associated
/// to the first plan, the first of them to the second plan as well), three billing groups
/// each with its own linked account, three custom line items in the first group, a
/// percentage item the group and those items are associated to, a second percentage item
/// with no associations, and an item carrying two generations.
async fn fixture() -> World {
    let em = Emulator::payer();

    let mut plans: Vec<String> = Vec::new();
    for index in 0..POPULATION {
        plans.push(
            setup_arn(
                &em,
                "POST",
                "/create-pricing-plan",
                &json!({ "Name": format!("TokenPlan{index}") }),
                &format!("pricing plan {index}"),
            )
            .await,
        );
    }

    let mut rules: Vec<String> = Vec::new();
    for index in 0..POPULATION {
        rules.push(
            setup_arn(
                &em,
                "POST",
                "/create-pricing-rule",
                &json!({
                    "Name": format!("TokenRule{index}"),
                    "Scope": "GLOBAL",
                    "Type": "DISCOUNT",
                    "ModifierPercentage": 5.0,
                }),
                &format!("pricing rule {index}"),
            )
            .await,
        );
    }
    setup(
        &em,
        "PUT",
        "/associate-pricing-rules",
        &json!({ "Arn": plans[0], "PricingRuleArns": rules }),
        "every rule on the first plan",
    )
    .await;
    setup(
        &em,
        "PUT",
        "/associate-pricing-rules",
        &json!({ "Arn": plans[1], "PricingRuleArns": [rules[0]] }),
        "the first rule on the second plan",
    )
    .await;

    let mut groups: Vec<String> = Vec::new();
    for index in 0..POPULATION {
        let account = linked_account(index);
        groups.push(
            setup_arn(
                &em,
                "POST",
                "/create-billing-group",
                &json!({
                    "Name": format!("TokenGroup{index}"),
                    "AccountGrouping": { "LinkedAccountIds": [account] },
                    "ComputationPreference": { "PricingPlanArn": plans[0] },
                    "PrimaryAccountId": account,
                }),
                &format!("billing group {index}"),
            )
            .await,
        );
    }

    let mut items: Vec<String> = Vec::new();
    for index in 0..POPULATION {
        items.push(
            setup_arn(
                &em,
                "POST",
                "/create-custom-line-item",
                &json!({
                    "Name": format!("TokenItem{index}"),
                    "Description": "an item of the pagination-token fixture",
                    "BillingGroupArn": groups[0],
                    "ChargeDetails": { "Type": "FEE", "Flat": { "ChargeValue": 1.0 } },
                }),
                &format!("custom line item {index}"),
            )
            .await,
        );
    }

    // Resources are associated to a PERCENTAGE item only, so the association listing needs
    // one of those — and a second one, with no associations, as another parent of the same
    // listing.
    let mut percentage: Vec<String> = Vec::new();
    for index in 0..2 {
        percentage.push(
            setup_arn(
                &em,
                "POST",
                "/create-custom-line-item",
                &json!({
                    "Name": format!("TokenPercentage{index}"),
                    "Description": "a percentage item of the pagination-token fixture",
                    "BillingGroupArn": groups[0],
                    "ChargeDetails": { "Type": "FEE", "Percentage": { "PercentageValue": 10.0 } },
                }),
                &format!("percentage custom line item {index}"),
            )
            .await,
        );
    }
    let associated: Vec<String> = std::iter::once(groups[0].clone())
        .chain(items.iter().cloned())
        .collect();
    let response = setup(
        &em,
        "PUT",
        "/batch-associate-resources-to-custom-line-item",
        &json!({ "TargetArn": percentage[0], "ResourceArns": associated }),
        "the group and its items on the first percentage item",
    )
    .await;
    assert_eq!(
        response
            .get("SuccessfullyAssociatedResources")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default(),
        associated.len(),
        "setup associations: expected every resource associated, got {response}"
    );

    // A custom line item with TWO generations: created in the previous billing period and
    // written again from the current one, which opens a second generation rather than
    // replacing the first.
    let versioned = setup_arn(
        &em,
        "POST",
        "/create-custom-line-item",
        &json!({
            "Name": "TokenVersioned",
            "Description": "a versioned item of the pagination-token fixture",
            "BillingGroupArn": groups[0],
            "ChargeDetails": { "Type": "FEE", "Flat": { "ChargeValue": 2.0 } },
            "BillingPeriodRange": { "InclusiveStartBillingPeriod": period(-1) },
        }),
        "the versioned custom line item",
    )
    .await;
    setup(
        &em,
        "POST",
        "/update-custom-line-item",
        &json!({
            "Arn": versioned,
            "Description": "the second generation of the versioned item",
            "BillingPeriodRange": { "InclusiveStartBillingPeriod": period(0) },
        }),
        "the second generation of the versioned item",
    )
    .await;

    let reads = vec![
        Read {
            operation: "ListBillingGroups",
            path: "/list-billing-groups",
            member: "BillingGroups",
            body: json!({}),
            other_parent: None,
            mints: true,
        },
        Read {
            operation: "ListPricingPlans",
            path: "/list-pricing-plans",
            member: "PricingPlans",
            body: json!({}),
            other_parent: None,
            mints: true,
        },
        Read {
            operation: "ListPricingRules",
            path: "/list-pricing-rules",
            member: "PricingRules",
            body: json!({}),
            other_parent: None,
            mints: true,
        },
        Read {
            operation: "ListCustomLineItems",
            path: "/list-custom-line-items",
            member: "CustomLineItems",
            body: json!({}),
            other_parent: None,
            mints: true,
        },
        Read {
            operation: "ListBillingGroupCostReports",
            path: "/list-billing-group-cost-reports",
            member: "BillingGroupCostReports",
            body: json!({}),
            other_parent: None,
            mints: true,
        },
        Read {
            operation: "ListAccountAssociations",
            path: "/list-account-associations",
            member: "LinkedAccounts",
            body: json!({}),
            other_parent: None,
            mints: false,
        },
        Read {
            operation: "ListPricingRulesAssociatedToPricingPlan",
            path: "/list-pricing-rules-associated-to-pricing-plan",
            member: "PricingRuleArns",
            body: json!({ "PricingPlanArn": plans[0] }),
            other_parent: Some(json!({ "PricingPlanArn": plans[1] })),
            mints: true,
        },
        Read {
            operation: "ListPricingPlansAssociatedWithPricingRule",
            path: "/list-pricing-plans-associated-with-pricing-rule",
            member: "PricingPlanArns",
            body: json!({ "PricingRuleArn": rules[0] }),
            other_parent: Some(json!({ "PricingRuleArn": rules[1] })),
            mints: true,
        },
        Read {
            operation: "ListResourcesAssociatedToCustomLineItem",
            path: "/list-resources-associated-to-custom-line-item",
            member: "AssociatedResources",
            body: json!({ "Arn": percentage[0] }),
            other_parent: Some(json!({ "Arn": percentage[1] })),
            mints: true,
        },
        Read {
            operation: "ListCustomLineItemVersions",
            path: "/list-custom-line-item-versions",
            member: "CustomLineItemVersions",
            body: json!({ "Arn": versioned }),
            other_parent: Some(json!({ "Arn": items[0] })),
            mints: true,
        },
        Read {
            operation: "GetBillingGroupCostReport",
            path: "/get-billing-group-cost-report",
            member: "BillingGroupCostReportResults",
            body: json!({ "Arn": groups[0] }),
            other_parent: Some(json!({ "Arn": groups[1] })),
            mints: false,
        },
    ];

    World {
        em,
        reads,
        stored_arn: groups[0].clone(),
        stored_account: linked_account(0),
    }
}

/// `read`'s request with the extra members `extra` carries written onto it.
fn request(body: &Value, extra: Value) -> Value {
    let mut request = body.clone();
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        request[member] = value;
    }
    request
}

/// The elements of a page, or an empty list when the response carries none.
fn page(body: &Value, member: &str) -> Vec<Value> {
    body.get(member)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Every element `read` reports with no page bound of its own — the sequence a paged walk
/// has to reproduce.
async fn whole_listing(em: &Emulator, read: &Read) -> Vec<Value> {
    let (status, body, _) = em.call("POST", read.path, Some(&read.body)).await;
    assert_eq!(
        status, 200,
        "case {}: expected the unpaged listing to be served, got {status} with {body}",
        read.operation
    );
    page(&body, read.member)
}

/// The `NextToken` a one-row page of `read` hands back, for a read that can be asked for
/// one row.
async fn minted_token(em: &Emulator, read: &Read) -> String {
    let body = request(&read.body, json!({ "MaxResults": 1 }));
    let (status, response, _) = em.call("POST", read.path, Some(&body)).await;
    assert_eq!(
        status, 200,
        "case {}: expected a first page, got {status} with {response}",
        read.operation
    );
    response
        .get("NextToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {}: expected a NextToken for a one-row page of a longer sequence, got \
                 {response}",
                read.operation
            )
        })
        .to_string()
}

/// `token` with one character changed — a token a client corrupted in transit or in a log.
fn corrupted(token: &str) -> String {
    let last = token.chars().count().saturating_sub(1);
    token
        .chars()
        .enumerate()
        .map(|(index, character)| match (index == last, character) {
            (true, '0') => '1',
            (true, _) => '0',
            (false, _) => character,
        })
        .collect()
}

/// Assert `read`, asked to resume from `token`, is refused as a token this service did not
/// mint: the modelled 400 `ValidationException` with `Reason` `CANNOT_PARSE`, `Fields`
/// naming `NextToken`, a `Message` that states the member — and no page at all.
async fn assert_refused(em: &Emulator, read: &Read, case: &str, body: &Value) {
    let (status, response, error_type) = em.call("POST", read.path, Some(body)).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for a token this service did not mint, got {status} \
         with {response}"
    );
    assert_eq!(
        error_code(&response),
        "ValidationException",
        "case {case:?}: expected the modelled ValidationException, got {response}"
    );
    assert_eq!(
        error_type.as_deref().map(|value| value
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()),
        Some("ValidationException".to_string()),
        "case {case:?}: expected the error shape in x-amzn-errortype, got {error_type:?}"
    );
    assert_eq!(
        response.get("Reason").and_then(Value::as_str),
        Some("CANNOT_PARSE"),
        "case {case:?}: expected Reason CANNOT_PARSE, got {response}"
    );
    let fields: Vec<String> = response
        .get("Fields")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|field| field.get("Name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    assert_eq!(
        fields,
        vec!["NextToken".to_string()],
        "case {case:?}: expected Fields to name NextToken, got {response}"
    );
    let message = response
        .get("Message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        message.contains("NextToken"),
        "case {case:?}: expected the message to state the member, got {message:?}"
    );
    assert!(
        response.get(read.member).is_none(),
        "case {case:?}: the refusal must carry no {} page, got {response}",
        read.member
    );
    assert!(
        response.get("NextToken").is_none(),
        "case {case:?}: the refusal must carry no NextToken, got {response}"
    );
}

/// THE RULE: every paginated read refuses a `NextToken` it did not mint, whatever class
/// the value belongs to — and the refusal changes nothing a later caller reads.
#[tokio::test]
async fn every_paginated_read_refuses_a_next_token_it_did_not_mint() {
    let world = fixture().await;

    // The sequence each read serves before any of this, so the refusals can be shown to
    // have changed nothing.
    let mut before: Vec<Vec<Value>> = Vec::new();
    for read in &world.reads {
        before.push(whole_listing(&world.em, read).await);
    }

    // The tokens each read that can be asked for one row really did mint, so a value
    // minted by ANOTHER listing is a row of the table below rather than an invention.
    let mut minted: Vec<(&'static str, String)> = Vec::new();
    for read in world.reads.iter().filter(|read| read.mints) {
        minted.push((read.operation, minted_token(&world.em, read).await));
    }

    // (case, the rejected value) — one row per class of value that is not a token this
    // service minted. The hex rows matter because the tokens this service mints ARE hex:
    // a value of the right alphabet is still refused unless it was minted whole.
    let unminted: Vec<(String, String)> = vec![
        ("a word nothing minted".to_string(), "not-a-token".to_string()),
        ("the empty string".to_string(), String::new()),
        (
            "the stored ARN a cursor used to be".to_string(),
            world.stored_arn.clone(),
        ),
        (
            "the stored account id a cursor used to be".to_string(),
            world.stored_account.clone(),
        ),
        (
            "the fixed-width index a version cursor is".to_string(),
            "0000000001".to_string(),
        ),
        (
            "hex of a cursor carrying no scope".to_string(),
            ::hex::encode(&world.stored_arn),
        ),
        (
            "hex of bytes that are not text".to_string(),
            "ffffffffffffffffff".to_string(),
        ),
        (
            "hex too short to carry anything".to_string(),
            "ab".to_string(),
        ),
        (
            "an odd number of hex digits".to_string(),
            "abc".to_string(),
        ),
        (
            "a value longer than any token this service mints".to_string(),
            "ab".repeat(2048),
        ),
    ];

    for (index, read) in world.reads.iter().enumerate() {
        for (class, token) in &unminted {
            let case = format!("{} given {class}", read.operation);
            let body = request(&read.body, json!({ "NextToken": token }));
            assert_refused(&world.em, read, &case, &body).await;
        }

        // A token another listing of this service minted is not a cursor here either: the
        // token belongs to the listing that returned it.
        for (donor, token) in &minted {
            if *donor == read.operation {
                continue;
            }
            let case = format!("{} given a token {donor} minted", read.operation);
            let body = request(&read.body, json!({ "NextToken": token }));
            assert_refused(&world.em, read, &case, &body).await;
        }

        // For a read whose sequence belongs to one parent resource, its own token is no
        // cursor into another parent's sequence.
        if let (Some(other), Some((_, token))) = (
            read.other_parent.as_ref(),
            minted.iter().find(|(donor, _)| *donor == read.operation),
        ) {
            let case = format!("{} given its own token for another parent", read.operation);
            let body = request(other, json!({ "NextToken": token }));
            assert_refused(&world.em, read, &case, &body).await;
        }

        // A token this read really did mint, with ONE character changed, is a token it did
        // not mint.
        if let Some((_, token)) = minted.iter().find(|(donor, _)| *donor == read.operation) {
            let case = format!("{} given its own token with one character changed", read.operation);
            let body = request(&read.body, json!({ "NextToken": corrupted(token) }));
            assert_refused(&world.em, read, &case, &body).await;
        }

        // The refused reads changed nothing: the same sequence is served afterwards.
        let after = whole_listing(&world.em, read).await;
        assert_eq!(
            after, before[index],
            "case {}: a refused token must change nothing a later caller reads, got {after:?}",
            read.operation
        );
    }
}

/// THE RULE: a token a listing minted is opaque — it carries no ARN, no account id and no
/// index a caller could read or edit — and handing it back resumes exactly where the page
/// stopped, so a walk reports the whole sequence once, in the unpaged order.
#[tokio::test]
async fn a_minted_next_token_is_opaque_and_resumes_the_listing_it_belongs_to() {
    let world = fixture().await;

    for read in world.reads.iter().filter(|read| read.mints) {
        let token = minted_token(&world.em, read).await;
        assert!(
            token.chars().all(|character| character.is_ascii_hexdigit()),
            "case {}: expected an opaque token, got {token:?}",
            read.operation
        );
        assert!(
            !token.contains("arn:") && token != world.stored_arn && token != world.stored_account,
            "case {}: a token must not be a store key, got {token:?}",
            read.operation
        );

        let whole = whole_listing(&world.em, read).await;
        assert!(
            whole.len() > 1,
            "case {}: this fixture must hold more than one row for the walk to page, got \
             {whole:?}",
            read.operation
        );

        let mut walked: Vec<Value> = Vec::new();
        let mut next: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut extra = json!({ "MaxResults": 1 });
            if let Some(token) = &next {
                extra["NextToken"] = json!(token);
            }
            let body = request(&read.body, extra);
            let (status, response, _) = world.em.call("POST", read.path, Some(&body)).await;
            assert_eq!(
                status, 200,
                "case {}: expected page {pages} to be served, got {status} with {response}",
                read.operation
            );
            let rows = page(&response, read.member);
            assert!(
                rows.len() <= 1,
                "case {}: expected at most the one row asked for, got {rows:?}",
                read.operation
            );
            walked.extend(rows);
            next = response
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            pages += 1;
            assert!(
                pages <= MAX_PAGES,
                "case {}: expected the walk to end, got {pages} pages",
                read.operation
            );
            if next.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, whole,
            "case {}: expected the paged walk to report every element once in the unpaged \
             order, got {walked:?}",
            read.operation
        );
    }
}

// --- The month a period-scoped token belongs to --------------------------------------
//
// A read of this service that is ANSWERED FOR one billing period walks a sequence that
// month decides: which resources were in effect then, and what every reported element says
// about them (the generation in force then, the counts and memberships of that month, the
// amounts that month was charged). So each month is its own sequence, and the month travels
// in the scope of the token a truncated page mints — the rows below are every such read, and
// per read every way a second request can name another month or the same one.

/// The page the server itself cuts a listing at when the request asks for none — the page
/// `ListAccountAssociations`, which models no `MaxResults`, is always cut at. Read from the
/// service rather than re-spelled, so this suite cannot drift from the bound it pages over.
const SERVER_PAGE: usize =
    service_com_amazonaws_billingconductor_awsbillingconductor::state::DEFAULT_PAGE_SIZE as usize;

/// How many linked accounts the fixture below puts in one billing group: one past
/// [`SERVER_PAGE`], so the only page `ListAccountAssociations` can be asked for still leaves
/// a remainder and hands back a `NextToken`.
const ACCOUNTS_PAST_ONE_PAGE: usize = SERVER_PAGE + 1;

/// How many of each other resource the fixture below creates: three, so a one-row page
/// always leaves a remainder.
const PERIOD_POPULATION: usize = 3;

/// How many linked accounts ONE request may name: the maximum of the modelled
/// `AccountIdList` length (0..30), which both `AccountGrouping.LinkedAccountIds` and
/// `AssociateAccounts.AccountIds` target
/// (`smithy_query(query="shape",shape="AccountIdList")`).
const MAX_ACCOUNTS_PER_CALL: usize = 30;

/// The linked account id of the `index`th account of the period-scoped fixture: twelve
/// digits, as `AccountId`'s modelled `^[0-9]{12}$` requires, and distinct per account so no
/// account is claimed twice.
fn period_account(index: usize) -> String {
    format!("{:012}", 200_000_000_000_u64 + index as u64)
}

/// The nearest billing period, from the current one on, whose month has TWO spellings — a
/// month `1..=9`, which the modelled `BillingPeriod` pattern
/// `^\d{4}-(0?[1-9]|1[012])$` admits both with and without its leading zero.
///
/// Read off the service's own clock, and never earlier than the current period, because
/// every resource of this store stands in the period it was created in and in every later
/// one: so the sequence this month holds is the same one the current month holds, whatever
/// month the suite runs in.
fn two_spelling_period() -> String {
    (0..12)
        .map(period)
        .find(|billing_period| {
            billing_period
                .split_once('-')
                .and_then(|(_, month)| month.parse::<u32>().ok())
                .is_some_and(|month| month <= 9)
        })
        .expect("one of any twelve consecutive months is numbered 1..=9")
}

/// `body` answered FOR `billing_period` — the month named explicitly, or left to the current
/// one, which is the month a request naming none is answered for.
fn in_period(body: &Value, billing_period: Option<&str>) -> Value {
    match billing_period {
        Some(month) => request(body, json!({ "BillingPeriod": month })),
        None => body.clone(),
    }
}

/// The page `read` is asked for while paging: the one row a read whose `MaxResults` can
/// name it is asked for, and the server's own page for the read that models no `MaxResults`
/// at all.
fn page_size(read: &Read) -> usize {
    if read.mints {
        1
    } else {
        SERVER_PAGE
    }
}

/// `read`'s request for one page of the month `billing_period`, resuming from `token` when
/// there is one.
fn page_request(read: &Read, billing_period: Option<&str>, token: Option<&str>) -> Value {
    let mut body = in_period(&read.body, billing_period);
    if read.mints {
        body["MaxResults"] = json!(page_size(read));
    }
    if let Some(token) = token {
        body["NextToken"] = json!(token);
    }
    body
}

/// Every element `read` reports for `billing_period`, walked through its own pages — the
/// sequence of THAT month, which a cursor minted while paging it points into.
async fn walk_in_period(
    em: &Emulator,
    read: &Read,
    case: &str,
    billing_period: Option<&str>,
) -> Vec<Value> {
    let mut walked: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    for pages in 0..MAX_PAGES {
        let body = page_request(read, billing_period, token.as_deref());
        let (status, response, _) = em.call("POST", read.path, Some(&body)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected page {pages} of the month's sequence to be served, got \
             {status} with {response}"
        );
        walked.extend(page(&response, read.member));
        token = response
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            return walked;
        }
    }
    panic!("case {case:?}: expected the walk to end, got {MAX_PAGES} pages");
}

/// The `NextToken` the first page of `read` for the month `billing_period` hands back.
async fn minted_in_period(
    em: &Emulator,
    read: &Read,
    case: &str,
    billing_period: Option<&str>,
) -> String {
    let body = page_request(read, billing_period, None);
    let (status, response, _) = em.call("POST", read.path, Some(&body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected a first page, got {status} with {response}"
    );
    assert_eq!(
        page(&response, read.member).len(),
        page_size(read),
        "case {case:?}: expected the page asked for to be full, got {response}"
    );
    response
        .get("NextToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a NextToken for a page of a longer sequence, got \
                 {response}"
            )
        })
        .to_string()
}

/// Stand up a fixture every period-scoped read of this service pages over: three pricing
/// plans with three rules on the first, three billing groups — the first holding
/// [`ACCOUNTS_PAST_ONE_PAGE`] linked accounts — three custom line items in it, and a
/// percentage item the group and two of those items are associated to.
///
/// Every resource is created NOW, so it stands in the current billing period and in every
/// later one: the sequence each read walks is non-empty for the current month and for the
/// months after it, which is what lets a token be minted for one month and handed back on
/// another.
async fn period_scoped_fixture() -> World {
    let em = Emulator::payer();

    let mut plans: Vec<String> = Vec::new();
    for index in 0..PERIOD_POPULATION {
        plans.push(
            setup_arn(
                &em,
                "POST",
                "/create-pricing-plan",
                &json!({ "Name": format!("PeriodPlan{index}") }),
                &format!("pricing plan {index}"),
            )
            .await,
        );
    }

    let mut rules: Vec<String> = Vec::new();
    for index in 0..PERIOD_POPULATION {
        rules.push(
            setup_arn(
                &em,
                "POST",
                "/create-pricing-rule",
                &json!({
                    "Name": format!("PeriodRule{index}"),
                    "Scope": "GLOBAL",
                    "Type": "DISCOUNT",
                    "ModifierPercentage": 5.0,
                }),
                &format!("pricing rule {index}"),
            )
            .await,
        );
    }
    setup(
        &em,
        "PUT",
        "/associate-pricing-rules",
        &json!({ "Arn": plans[0], "PricingRuleArns": rules }),
        "every rule on the first plan",
    )
    .await;

    // The accounts go in the FIRST group, so the account listing holds more rows than the
    // one page it can be asked for while every group still exists in the same months. A
    // request may name at most [`MAX_ACCOUNTS_PER_CALL`] accounts, so they are added a
    // chunk at a time: the create carries the first chunk and `AssociateAccounts` the rest.
    let accounts: Vec<String> = (0..ACCOUNTS_PAST_ONE_PAGE).map(period_account).collect();
    let mut chunks = accounts.chunks(MAX_ACCOUNTS_PER_CALL);
    let first_chunk: Vec<String> = chunks.next().unwrap_or_default().to_vec();
    let mut groups: Vec<String> = Vec::new();
    for index in 0..PERIOD_POPULATION {
        groups.push(
            setup_arn(
                &em,
                "POST",
                "/create-billing-group",
                &json!({
                    "Name": format!("PeriodGroup{index}"),
                    "AccountGrouping": {
                        "LinkedAccountIds":
                            if index == 0 { first_chunk.clone() } else { Vec::new() },
                    },
                    "ComputationPreference": { "PricingPlanArn": plans[0] },
                }),
                &format!("billing group {index}"),
            )
            .await,
        );
    }
    for (index, chunk) in chunks.enumerate() {
        setup(
            &em,
            "POST",
            "/associate-accounts",
            &json!({ "Arn": groups[0], "AccountIds": chunk }),
            &format!("account chunk {index} on the first group"),
        )
        .await;
    }

    let mut items: Vec<String> = Vec::new();
    for index in 0..PERIOD_POPULATION {
        items.push(
            setup_arn(
                &em,
                "POST",
                "/create-custom-line-item",
                &json!({
                    "Name": format!("PeriodItem{index}"),
                    "Description": "an item of the period-scope fixture",
                    "BillingGroupArn": groups[0],
                    "ChargeDetails": { "Type": "FEE", "Flat": { "ChargeValue": 1.0 } },
                }),
                &format!("custom line item {index}"),
            )
            .await,
        );
    }

    // Resources are associated to a PERCENTAGE item only, so the association listing needs
    // one of those; the group and two items are three associations, one past a one-row page.
    let percentage = setup_arn(
        &em,
        "POST",
        "/create-custom-line-item",
        &json!({
            "Name": "PeriodPercentage",
            "Description": "a percentage item of the period-scope fixture",
            "BillingGroupArn": groups[0],
            "ChargeDetails": { "Type": "FEE", "Percentage": { "PercentageValue": 10.0 } },
        }),
        "the percentage custom line item",
    )
    .await;
    setup(
        &em,
        "PUT",
        "/batch-associate-resources-to-custom-line-item",
        &json!({
            "TargetArn": percentage,
            "ResourceArns": [groups[0].clone(), items[0].clone(), items[1].clone()],
        }),
        "the group and two of its items on the percentage item",
    )
    .await;

    // Every paginated read of this service that is answered FOR one billing period. The
    // `body` of each holds the members that pick the sequence MINUS the `BillingPeriod` the
    // cases vary; `mints` says whether the read's own `MaxResults` can ask for a page
    // smaller than the server's.
    //
    // The reads NOT here are the ones whose sequence no billing period shapes:
    // `ListCustomLineItemVersions` (no `BillingPeriod` member at all) and
    // `ListPricingRulesAssociatedToPricingPlan` /
    // `ListPricingPlansAssociatedWithPricingRule`, whose elements are the associated ARNs
    // themselves — the period selects which of them are reported without changing what one
    // element is or what its cursor key spells.
    let reads = vec![
        Read {
            operation: "ListBillingGroups",
            path: "/list-billing-groups",
            member: "BillingGroups",
            body: json!({}),
            other_parent: None,
            mints: true,
        },
        Read {
            operation: "ListPricingPlans",
            path: "/list-pricing-plans",
            member: "PricingPlans",
            body: json!({}),
            other_parent: None,
            mints: true,
        },
        Read {
            operation: "ListPricingRules",
            path: "/list-pricing-rules",
            member: "PricingRules",
            body: json!({}),
            other_parent: None,
            mints: true,
        },
        Read {
            operation: "ListCustomLineItems",
            path: "/list-custom-line-items",
            member: "CustomLineItems",
            body: json!({}),
            other_parent: None,
            mints: true,
        },
        Read {
            operation: "ListBillingGroupCostReports",
            path: "/list-billing-group-cost-reports",
            member: "BillingGroupCostReports",
            body: json!({}),
            other_parent: None,
            mints: true,
        },
        Read {
            operation: "ListResourcesAssociatedToCustomLineItem",
            path: "/list-resources-associated-to-custom-line-item",
            member: "AssociatedResources",
            body: json!({ "Arn": percentage }),
            other_parent: None,
            mints: true,
        },
        Read {
            operation: "ListAccountAssociations",
            path: "/list-account-associations",
            member: "LinkedAccounts",
            body: json!({}),
            other_parent: None,
            mints: false,
        },
    ];

    World {
        em,
        reads,
        stored_arn: groups[0].clone(),
        stored_account: period_account(0),
    }
}

/// THE RULE, refusing side: a read answered for one billing period walks THAT month's
/// sequence, so a `NextToken` minted while paging one month names no position in another
/// month's sequence and is refused — never read as a cut point that silently drops every
/// element sorting below it.
#[tokio::test]
async fn a_period_scoped_token_is_refused_by_a_read_of_another_billing_period() {
    let world = period_scoped_fixture().await;
    let current = period(0);
    let next = period(1);
    let previous = period(-1);
    let two_spellings = two_spelling_period();

    for read in &world.reads {
        // The month's sequence before any of this, so the refusals can be shown to have
        // changed nothing a later caller reads.
        let before = walk_in_period(&world.em, read, read.operation, Some(&current)).await;
        assert!(
            before.len() > page_size(read),
            "case {}: this fixture must hold more rows than one page for a token to be \
             minted, got {} rows",
            read.operation,
            before.len()
        );

        // (case, the month the token is minted for, the month it is handed back on) — one
        // row per way a second request can name a month the cursor points into nothing of:
        // a later month, an earlier month, the month a request naming none is answered for,
        // and a month named in the other spelling of a DIFFERENT month.
        let crossings: Vec<(String, Option<&str>, Option<&str>)> = vec![
            (
                "the current month's token on the next month".to_string(),
                Some(current.as_str()),
                Some(next.as_str()),
            ),
            (
                "the current month's token on the previous month".to_string(),
                Some(current.as_str()),
                Some(previous.as_str()),
            ),
            (
                "the next month's token on the current month".to_string(),
                Some(next.as_str()),
                Some(current.as_str()),
            ),
            (
                "the next month's token on the implicit current month".to_string(),
                Some(next.as_str()),
                None,
            ),
            (
                "the implicit current month's token on the next month".to_string(),
                None,
                Some(next.as_str()),
            ),
        ];

        for (crossing, minted_for, read_for) in crossings {
            let case = format!("{} given {crossing}", read.operation);
            let token = minted_in_period(&world.em, read, &case, minted_for).await;
            let body = in_period(&read.body, read_for);
            assert_refused(
                &world.em,
                read,
                &case,
                &request(&body, json!({ "NextToken": token })),
            )
            .await;
        }

        // The two spellings of ONE month name the same sequence, so a token minted for a
        // month spelled either way is still refused by a read of ANOTHER month, however
        // that month is spelled.
        let case = format!(
            "{} given a token minted for a month spelled without its leading zero, on \
             another month",
            read.operation
        );
        let token = minted_in_period(
            &world.em,
            read,
            &case,
            Some(&one_digit_month(&two_spellings)),
        )
        .await;
        let body = in_period(&read.body, Some(&period(-2)));
        assert_refused(
            &world.em,
            read,
            &case,
            &request(&body, json!({ "NextToken": token })),
        )
        .await;

        let after = walk_in_period(&world.em, read, read.operation, Some(&current)).await;
        assert_eq!(
            after, before,
            "case {}: a refused token must change nothing a later caller reads, got \
             {after:?}",
            read.operation
        );
    }
}

/// THE RULE, resuming side: every request that names the month a token was minted for
/// resumes exactly where its page stopped, whichever of the two spellings of that month it
/// names — and a request naming no month resumes a token minted for the current one, because
/// that is the month it is itself answered for.
#[tokio::test]
async fn a_period_scoped_token_resumes_every_request_naming_the_month_it_was_minted_for() {
    let world = period_scoped_fixture().await;
    let current = period(0);
    let two_spellings = two_spelling_period();
    let padded_month = two_spellings.clone();
    let bare_month = one_digit_month(&two_spellings);

    for read in &world.reads {
        // (case, the month the token is minted for, the month the next page is asked for) —
        // every row names the SAME month twice, so every row resumes.
        let same_month: Vec<(&str, Option<&str>, Option<&str>)> = vec![
            (
                "the same month, named the same way",
                Some(current.as_str()),
                Some(current.as_str()),
            ),
            (
                "the current month, named explicitly after an implicit first page",
                None,
                Some(current.as_str()),
            ),
            (
                "the current month, left implicit after an explicit first page",
                Some(current.as_str()),
                None,
            ),
            (
                "the same month, resumed without the leading zero of its month",
                Some(padded_month.as_str()),
                Some(bare_month.as_str()),
            ),
            (
                "the same month, resumed with the leading zero of its month",
                Some(bare_month.as_str()),
                Some(padded_month.as_str()),
            ),
        ];

        for (spelling, minted_for, read_for) in same_month {
            let case = format!("{} resumed on {spelling}", read.operation);
            let whole = walk_in_period(&world.em, read, &case, minted_for).await;
            let cut = page_size(read);
            assert!(
                whole.len() > cut,
                "case {case:?}: this fixture must hold more rows than one page for a token \
                 to be minted, got {} rows",
                whole.len()
            );
            let token = minted_in_period(&world.em, read, &case, minted_for).await;
            let body = request(
                &in_period(&read.body, read_for),
                json!({ "NextToken": token }),
            );
            let (status, response, _) = world.em.call("POST", read.path, Some(&body)).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the month's own token to resume its listing, got \
                 {status} with {response}"
            );
            assert_eq!(
                page(&response, read.member),
                whole[cut..].to_vec(),
                "case {case:?}: expected the elements the first page left out, got {response}"
            );
        }
    }
}
