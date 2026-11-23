// SPDX-License-Identifier: CC0-1.0

//! What a `NextToken` of the two pricing-rule association reads is a cursor INTO, end to
//! end over the wire.
//!
//! RULE under test: a paginated read that is ANSWERED FOR one billing period walks THAT
//! month's sequence, so the scope its `NextToken` carries names the month as well as the
//! parent resource the sequence belongs to — the period decides which associations were in
//! force and so which elements the read walks at all. A token handed back on a read of any
//! other month, or of another parent, names no position in the sequence being read and is
//! refused with the modelled 400 `ValidationException`, `Reason` `CANNOT_PARSE` and
//! `Fields` naming `NextToken`, rather than cutting that other sequence at a key it names
//! nothing in and silently dropping every element sorting below it. A request that names
//! the SAME month and parent, however it spelled them, resumes exactly where the page
//! stopped.
//!
//! The rows span what the rule generalises over rather than one reported call: both reads
//! answered for a period within one parent (`ListPricingRulesAssociatedToPricingPlan` and
//! `ListPricingPlansAssociatedWithPricingRule`), and per read every way a second request
//! can name another sequence (a later month, the month a request naming none is answered
//! for, a month named in the other spelling of a different month, another parent) or the
//! same one (the same month, the month left implicit on either side, either spelling of a
//! two-spelling month, the parent named by its bare resource id).
//!
//! Every assertion goes over the service's real `restJson1` wire through
//! [`smithy_server_runtime::MultiServiceRouter::dispatch`], on the shared harness of
//! `tests/all/payer_account_gate.rs` — this service's control plane answers the payer
//! account alone.

use serde_json::{json, Value};

use crate::cost_report_token_breakdown_scope::one_digit_month;
use crate::pagination_token_opacity::{period, two_spelling_period};
use crate::payer_account_gate::{error_code, Emulator};

/// How many pricing plans and pricing rules the fixture creates: three, so the sequence
/// each read under test walks holds more rows than the one-row page it is asked for and a
/// first page always hands back a `NextToken`.
const POPULATION: usize = 3;

/// The page every first call here asks for: one row, the minimum of the `MaxResults` range
/// both reads model (1..100,
/// `smithy_query(query="operation",operation="ListPricingRulesAssociatedToPricingPlan")`).
const PAGE: usize = 1;

/// A bound on the pages a walk may take before it is called non-terminating: far above the
/// rows the fixture holds, so reaching it means the paging never ended.
const MAX_PAGES: usize = 8;

/// One of the two association reads: both are answered for ONE billing period and read
/// within ONE parent resource.
struct Read {
    /// The modelled operation, which is also the case name.
    operation: &'static str,
    /// The route the model binds it to.
    path: &'static str,
    /// The output member its page travels in.
    member: &'static str,
    /// The request naming the parent whose sequence is under test, minus the
    /// `BillingPeriod` the cases vary and the `MaxResults` the paging helpers add.
    body: Value,
    /// The same read against ANOTHER parent, whose own sequence a token of the first names
    /// no position in.
    other_parent: Value,
    /// The same read naming the SAME parent by its bare resource id — the other spelling
    /// the member's modelled pattern admits.
    bare_parent: Value,
}

/// The fixture: a live emulator with both association sequences longer than one page.
struct World {
    em: Emulator,
    reads: Vec<Read>,
}

/// One row of the tables below: the sequence a token is minted over and the sequence it is
/// then handed back on, each named by the parent request and the month it is answered for.
struct Case<'a> {
    /// What this row is, named in every assertion it makes.
    what: &'a str,
    /// The parent request the first page is asked of.
    mint_in: &'a Value,
    /// The month that page is answered for — `None` for a request that names none.
    minted_for: Option<&'a str>,
    /// The parent request the token is handed back on.
    read_in: &'a Value,
    /// The month THAT read is answered for.
    read_for: Option<&'a str>,
}

/// The bare resource id of a full ARN — the OTHER form both the `PricingPlanArn` and the
/// `PricingRuleArn` pattern admit, so a request may name the same parent either way.
fn bare_id(arn: &str) -> String {
    arn.rsplit('/').next().unwrap_or(arn).to_string()
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

/// Stand up [`POPULATION`] pricing plans and [`POPULATION`] pricing rules, associate every
/// rule to the first plan and the first rule to every plan, so each read under test walks
/// [`POPULATION`] elements within its own parent and one element within the other parent.
///
/// Every association is made NOW, so it is in force in the current billing period and in
/// every later one: the sequence each read walks is non-empty for the current month and for
/// the months after it, which is what lets a token be minted for one month and handed back
/// on another.
async fn fixture() -> World {
    let em = Emulator::payer();

    let mut plans: Vec<String> = Vec::new();
    for index in 0..POPULATION {
        plans.push(
            setup_arn(
                &em,
                "POST",
                "/create-pricing-plan",
                &json!({ "Name": format!("ScopePlan{index}") }),
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
                    "Name": format!("ScopeRule{index}"),
                    "Scope": "GLOBAL",
                    "Type": "DISCOUNT",
                    "ModifierPercentage": 5.0,
                }),
                &format!("pricing rule {index}"),
            )
            .await,
        );
    }

    // Every rule on the first plan, so the first plan's rule sequence is [`POPULATION`]
    // long; the first rule on every plan, so the first rule's plan sequence is too.
    setup(
        &em,
        "PUT",
        "/associate-pricing-rules",
        &json!({ "Arn": plans[0], "PricingRuleArns": rules }),
        "every rule on the first plan",
    )
    .await;
    for plan in plans.iter().skip(1) {
        setup(
            &em,
            "PUT",
            "/associate-pricing-rules",
            &json!({ "Arn": plan, "PricingRuleArns": [rules[0]] }),
            "the first rule on a later plan",
        )
        .await;
    }

    let reads = vec![
        Read {
            operation: "ListPricingRulesAssociatedToPricingPlan",
            path: "/list-pricing-rules-associated-to-pricing-plan",
            member: "PricingRuleArns",
            body: json!({ "PricingPlanArn": plans[0] }),
            other_parent: json!({ "PricingPlanArn": plans[1] }),
            bare_parent: json!({ "PricingPlanArn": bare_id(&plans[0]) }),
        },
        Read {
            operation: "ListPricingPlansAssociatedWithPricingRule",
            path: "/list-pricing-plans-associated-with-pricing-rule",
            member: "PricingPlanArns",
            body: json!({ "PricingRuleArn": rules[0] }),
            other_parent: json!({ "PricingRuleArn": rules[1] }),
            bare_parent: json!({ "PricingRuleArn": bare_id(&rules[0]) }),
        },
    ];

    World { em, reads }
}

/// `base` with the members `extra` carries written onto it.
fn request(base: &Value, extra: Value) -> Value {
    let mut request = base.clone();
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        request[member] = value;
    }
    request
}

/// `base` answered FOR `billing_period` — the month named explicitly, or left to the current
/// one, which is the month a request naming none is answered for.
fn in_period(base: &Value, billing_period: Option<&str>) -> Value {
    match billing_period {
        Some(month) => request(base, json!({ "BillingPeriod": month })),
        None => base.clone(),
    }
}

/// The elements of a page, or an empty list when the response carries none.
fn page(body: &Value, member: &str) -> Vec<Value> {
    body.get(member)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Every element `read` reports for `billing_period` within `base`'s parent, walked through
/// its own [`PAGE`]-sized pages — the sequence of THAT month, which a cursor minted while
/// paging it points into.
async fn walk_in_period(
    em: &Emulator,
    read: &Read,
    case: &str,
    base: &Value,
    billing_period: Option<&str>,
) -> Vec<Value> {
    let mut walked: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    for pages in 0..MAX_PAGES {
        let mut body = request(&in_period(base, billing_period), json!({ "MaxResults": PAGE }));
        if let Some(token) = token {
            body["NextToken"] = json!(token);
        }
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

/// The `NextToken` a [`PAGE`]-sized first page of `read` for the month `billing_period`
/// within `base`'s parent hands back.
async fn minted_in_period(
    em: &Emulator,
    read: &Read,
    case: &str,
    base: &Value,
    billing_period: Option<&str>,
) -> String {
    let body = request(&in_period(base, billing_period), json!({ "MaxResults": PAGE }));
    let (status, response, _) = em.call("POST", read.path, Some(&body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected a first page, got {status} with {response}"
    );
    assert_eq!(
        page(&response, read.member).len(),
        PAGE,
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

/// Assert `body` is refused as a `NextToken` this read did not mint for the sequence being
/// read: the modelled 400 `ValidationException`, `Reason` `CANNOT_PARSE`, `Fields` naming
/// `NextToken` — and no page at all.
async fn assert_refused(em: &Emulator, read: &Read, case: &str, body: &Value) {
    let (status, response, error_type) = em.call("POST", read.path, Some(body)).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for a token minted for another sequence, got \
         {status} with {response}"
    );
    assert_eq!(
        error_code(&response),
        "ValidationException",
        "case {case:?}: expected the modelled ValidationException, got {response}"
    );
    assert_eq!(
        error_type
            .as_deref()
            .map(|value| value.rsplit('#').next().unwrap_or_default().to_string()),
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
    assert!(
        response.get(read.member).is_none(),
        "case {case:?}: the refusal must carry no page of elements, got {response}"
    );
    assert!(
        response.get("NextToken").is_none(),
        "case {case:?}: the refusal must carry no NextToken, got {response}"
    );
}

/// THE RULE, refusing side: a token minted while paging one month, or one parent, names no
/// position in another month's or another parent's sequence, so handing it back on a read of
/// a different sequence is refused — never read as a cut point that silently drops every
/// element sorting below it.
#[tokio::test]
async fn an_association_token_is_refused_by_a_read_of_another_billing_period() {
    let world = fixture().await;
    let current = period(0);
    let next = period(1);
    let padded_month = two_spelling_period();
    let bare_month = one_digit_month(&padded_month);
    // A month that is NOT the two-spelling one above, which is picked from the next twelve:
    // the same month a year on, so the two cannot name one period whatever month the suite
    // runs in.
    let another_month = period(12);

    for read in &world.reads {
        // One row per way a second request can name a sequence the cursor points into
        // nothing of: a later month, the month a request naming none is answered for, the
        // other spelling of a different month, and another parent resource.
        let crossings = vec![
            Case {
                what: "the current month's token on the next month",
                mint_in: &read.body,
                minted_for: Some(current.as_str()),
                read_in: &read.body,
                read_for: Some(next.as_str()),
            },
            Case {
                what: "the next month's token on the current month",
                mint_in: &read.body,
                minted_for: Some(next.as_str()),
                read_in: &read.body,
                read_for: Some(current.as_str()),
            },
            Case {
                what: "the next month's token on the implicit current month",
                mint_in: &read.body,
                minted_for: Some(next.as_str()),
                read_in: &read.body,
                read_for: None,
            },
            Case {
                what: "the implicit current month's token on the next month",
                mint_in: &read.body,
                minted_for: None,
                read_in: &read.body,
                read_for: Some(next.as_str()),
            },
            Case {
                what: "a token minted for a month spelled without its leading zero, on \
                       another month",
                mint_in: &read.body,
                minted_for: Some(bare_month.as_str()),
                read_in: &read.body,
                read_for: Some(another_month.as_str()),
            },
            Case {
                what: "the same month's token on another parent resource",
                mint_in: &read.body,
                minted_for: Some(current.as_str()),
                read_in: &read.other_parent,
                read_for: Some(current.as_str()),
            },
        ];

        for crossing in crossings {
            let case = format!("{} given {}", read.operation, crossing.what);
            // The sequence the second read serves on its own, so the refusal can be shown
            // to have changed nothing a later caller reads.
            let before =
                walk_in_period(&world.em, read, &case, crossing.read_in, crossing.read_for).await;
            let token = minted_in_period(
                &world.em,
                read,
                &case,
                crossing.mint_in,
                crossing.minted_for,
            )
            .await;
            assert_refused(
                &world.em,
                read,
                &case,
                &request(
                    &in_period(crossing.read_in, crossing.read_for),
                    json!({ "NextToken": token }),
                ),
            )
            .await;
            let after =
                walk_in_period(&world.em, read, &case, crossing.read_in, crossing.read_for).await;
            assert_eq!(
                after, before,
                "case {case:?}: a refused token must change nothing a later caller reads, \
                 got {after:?}"
            );
        }
    }
}

/// THE RULE, resuming side: every request that names the month and the parent a token was
/// minted for resumes exactly where its page stopped, whichever spelling of that month and
/// of that parent it names — and a request naming no month resumes a token minted for the
/// current one, because that is the month it is itself answered for.
#[tokio::test]
async fn an_association_token_resumes_every_request_naming_the_month_it_was_minted_for() {
    let world = fixture().await;
    let current = period(0);
    let padded_month = two_spelling_period();
    let bare_month = one_digit_month(&padded_month);

    for read in &world.reads {
        // Every row names the SAME month and the SAME parent twice, however it spells
        // them, so every row resumes.
        let same_sequence = vec![
            Case {
                what: "the same month, named the same way",
                mint_in: &read.body,
                minted_for: Some(current.as_str()),
                read_in: &read.body,
                read_for: Some(current.as_str()),
            },
            Case {
                what: "the current month, named explicitly after an implicit first page",
                mint_in: &read.body,
                minted_for: None,
                read_in: &read.body,
                read_for: Some(current.as_str()),
            },
            Case {
                what: "the current month, left implicit after an explicit first page",
                mint_in: &read.body,
                minted_for: Some(current.as_str()),
                read_in: &read.body,
                read_for: None,
            },
            Case {
                what: "the same month, resumed without the leading zero of its month",
                mint_in: &read.body,
                minted_for: Some(padded_month.as_str()),
                read_in: &read.body,
                read_for: Some(bare_month.as_str()),
            },
            Case {
                what: "the same month, resumed with the leading zero of its month",
                mint_in: &read.body,
                minted_for: Some(bare_month.as_str()),
                read_in: &read.body,
                read_for: Some(padded_month.as_str()),
            },
            Case {
                what: "the same month, with the parent named by its bare resource id",
                mint_in: &read.body,
                minted_for: Some(current.as_str()),
                read_in: &read.bare_parent,
                read_for: Some(current.as_str()),
            },
        ];

        for spelling in same_sequence {
            let case = format!("{} resumed on {}", read.operation, spelling.what);
            let whole =
                walk_in_period(&world.em, read, &case, spelling.read_in, spelling.read_for).await;
            assert_eq!(
                whole.len(),
                POPULATION,
                "case {case:?}: expected the fixture's {POPULATION} associations in the \
                 month's sequence, got {whole:?}"
            );
            let token = minted_in_period(
                &world.em,
                read,
                &case,
                spelling.mint_in,
                spelling.minted_for,
            )
            .await;
            let body = request(
                &in_period(spelling.read_in, spelling.read_for),
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
                whole[PAGE..].to_vec(),
                "case {case:?}: expected the elements the first page left out, got {response}"
            );
        }
    }
}
