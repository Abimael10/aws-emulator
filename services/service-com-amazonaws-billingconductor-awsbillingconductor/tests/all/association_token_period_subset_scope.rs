// SPDX-License-Identifier: CC0-1.0

//! What a `NextToken` of the two pricing-rule association reads is a cursor into when the
//! months being paged report DIFFERENT element sets, end to end over the service's real
//! `restJson1` wire.
//!
//! RULE: a paginated read that is ANSWERED FOR one billing period walks THAT month's
//! sequence, so a `NextToken` is a cursor into the sequence of the month it was minted
//! while paging and names no position in any other month's; handing one back on a read of
//! another month is refused with the modelled 400 `ValidationException`, `Reason`
//! `CANNOT_PARSE` and `Fields` naming `NextToken`, and a read of the same month resumes
//! exactly where its page stopped.
//!
//! `tests/all/association_token_period_scope.rs` holds the two reads to that rule over
//! months whose element sets are the same; this suite is about the case where they are
//! NOT, which is what a cursor read as a cut point into the wrong month silently costs: an
//! association in force from an earlier period onward sorts somewhere in the middle of the
//! later month's longer sequence, so a token of the earlier month that was read as a cut
//! point would drop every element of the later month sorting below it and report no
//! `NextToken` for them. Each refused call is therefore bracketed by a full walk of the
//! month being read, and the walk's elements — not just the refusal — are what the rows
//! assert.
//!
//! The rows span what the rule generalises over: both reads answered for a period within
//! one parent (`ListPricingRulesAssociatedToPricingPlan` and
//! `ListPricingPlansAssociatedWithPricingRule`), and per read the ways two months can
//! differ (a token of the shorter earlier month on the longer current one, a token of the
//! longer month on the shorter one, either against the month a request naming none is
//! answered for, and against a month a year on, whose set is the current one's).
//!
//! The wire cannot make an association in a past period — an associating call happens now —
//! so the fixture makes every association over the wire and then backdates the ones that
//! are to be in force earlier, through the harness
//! `tests/all/pricing_rule_association_periods.rs` owns.

use serde_json::{json, Value};

use crate::pagination_token_opacity::period;
use crate::pricing_rule_association_billing_periods::{PLANS_FOR_RULE, RULES_FOR_PLAN};
use crate::pricing_rule_association_periods::{arns, Emulator};

/// The page every first call here asks for: one row, the minimum of the `MaxResults` range
/// both reads model (1..100,
/// `smithy_query(query="operation",operation="ListPricingPlansAssociatedWithPricingRule")`),
/// so a sequence of more than one element always hands back a `NextToken`.
const PAGE: i64 = 1;

/// A bound on the pages one walk may take before it is called non-terminating: far above
/// the three elements the longest sequence of the fixture holds.
const MAX_PAGES: usize = 8;

/// One of the two association reads: both are answered for ONE billing period and read
/// within ONE parent resource, and both report a LONGER sequence for the current month than
/// for the earlier one in the world the fixture builds.
struct Read {
    /// The modelled operation, which is also what every assertion names.
    operation: &'static str,
    /// The route the model binds it to.
    path: &'static str,
    /// The output member its page travels in.
    member: &'static str,
    /// The request naming the parent whose sequence is under test, minus the
    /// `BillingPeriod`, `MaxResults` and `NextToken` the cases add.
    body: Value,
    /// The elements the read reports for the EARLIER month, in the ARN order it pages in.
    earlier: Vec<String>,
    /// The elements it reports for the current month and every later one — the earlier
    /// set plus the association made now.
    later: Vec<String>,
}

/// One row: the month a token is minted while paging, and the month it is then handed back
/// on, each of which may be left unnamed (a request that names none is answered for the
/// current month).
struct Case<'a> {
    /// What this row is, named in every assertion it makes.
    what: &'a str,
    /// The month the first page is asked for — `None` for a request that names none.
    minted_for: Option<&'a str>,
    /// The month the token is handed back on.
    read_for: Option<&'a str>,
}

/// The fixture: a live emulator in which both reads under test report two elements for the
/// earlier month and three for the current one.
struct World {
    em: Emulator,
    reads: Vec<Read>,
    /// The month whose sequence is the strict subset.
    earlier: String,
}

/// `base` with the members `extra` carries written onto it.
fn request(base: &Value, extra: Value) -> Value {
    let mut request = base.clone();
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        request[member] = value;
    }
    request
}

/// `base` answered FOR `billing_period` — named explicitly, or left to the current month.
fn in_period(base: &Value, billing_period: Option<&str>) -> Value {
    match billing_period {
        Some(month) => request(base, json!({ "BillingPeriod": month })),
        None => base.clone(),
    }
}

/// Stand up a pricing plan and a pricing rule whose association sets GROW from the earlier
/// month to the current one, and answer the two reads of them.
///
/// The plan `anchor` associates three rules and the rule `anchor` is associated by three
/// plans; in both cases two of those associations are backdated to the earlier month and
/// the third is left in the current one, so the earlier month reports two elements and the
/// current month three.
async fn fixture() -> World {
    let em = Emulator::new();
    let earlier = period(-1);

    // The plan whose rule associations are read, and the rules it associates.
    let mut rules: Vec<String> = Vec::new();
    for index in 0..3 {
        rules.push(em.create_rule(&format!("Subset-Rule-{index}")).await);
    }
    let plan = em.create_plan("subset-plan", &rules).await;

    // The other plans associating the first rule, so the rule-side read has a sequence of
    // its own longer than one page.
    let mut plans: Vec<String> = vec![plan.clone()];
    for index in 1..3 {
        let other = em
            .create_plan(
                &format!("subset-plan-{index}"),
                std::slice::from_ref(&rules[0]),
            )
            .await;
        plans.push(other);
    }

    // Everything but the last association of each sequence is in force from the earlier
    // month onward; the last one was made now, so it is in force in the current month and
    // in no earlier one.
    for rule in &rules[..2] {
        em.start_association_in(&plan, rule, &earlier);
    }
    for other in &plans[..2] {
        em.start_association_in(other, &rules[0], &earlier);
    }

    // Both reads page in ARN order, and the ARNs are server-minted, so the expectation is
    // the sorted set rather than the order the fixture created them in.
    let sorted = |mut values: Vec<String>| {
        values.sort();
        values
    };
    let reads = vec![
        Read {
            operation: "ListPricingRulesAssociatedToPricingPlan",
            path: RULES_FOR_PLAN,
            member: "PricingRuleArns",
            body: json!({ "PricingPlanArn": plan }),
            earlier: sorted(rules[..2].to_vec()),
            later: sorted(rules.clone()),
        },
        Read {
            operation: "ListPricingPlansAssociatedWithPricingRule",
            path: PLANS_FOR_RULE,
            member: "PricingPlanArns",
            body: json!({ "PricingRuleArn": rules[0] }),
            earlier: sorted(plans[..2].to_vec()),
            later: sorted(plans.clone()),
        },
    ];

    World { em, reads, earlier }
}

/// The elements `read` must report for `billing_period`: the earlier month's subset for
/// that month alone, and the whole set for the current month and every month after it.
fn expected<'a>(read: &'a Read, billing_period: Option<&str>, earlier: &str) -> &'a Vec<String> {
    match billing_period {
        Some(month) if month == earlier => &read.earlier,
        _ => &read.later,
    }
}

/// Every element `read` reports for `billing_period`, walked through its own [`PAGE`]-sized
/// pages — the sequence of THAT month, which a cursor minted while paging it points into.
async fn walk(em: &Emulator, read: &Read, case: &str, billing_period: Option<&str>) -> Vec<String> {
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for pages in 0..MAX_PAGES {
        let mut body = request(
            &in_period(&read.body, billing_period),
            json!({ "MaxResults": PAGE }),
        );
        if let Some(token) = &token {
            body["NextToken"] = json!(token);
        }
        let (status, response) = em.post(read.path, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected page {pages} of the month's sequence to be served, got \
             {status} with {response:?}"
        );
        walked.extend(arns(&response, read.member));
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

/// Walk `billing_period`'s sequence and assert it is reported whole — the assertion a
/// cursor read as a cut point into this month breaks by dropping the elements below it.
async fn assert_month_reports_its_whole_sequence(
    em: &Emulator,
    read: &Read,
    case: &str,
    billing_period: Option<&str>,
    earlier: &str,
) {
    let walked = walk(em, read, case, billing_period).await;
    let expected = expected(read, billing_period, earlier);
    assert_eq!(
        &walked, expected,
        "case {case:?}: expected the month {billing_period:?} to report the associations in \
         force in it, {expected:?}, got {walked:?}"
    );
}

/// The `NextToken` a [`PAGE`]-sized first page of `read` for `billing_period` hands back.
async fn minted_in(
    em: &Emulator,
    read: &Read,
    case: &str,
    billing_period: Option<&str>,
) -> String {
    let body = request(
        &in_period(&read.body, billing_period),
        json!({ "MaxResults": PAGE }),
    );
    let (status, response) = em.post(read.path, &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected a first page for {billing_period:?}, got {status} with \
         {response:?}"
    );
    assert_eq!(
        arns(&response, read.member).len(),
        usize::try_from(PAGE).expect("a page of one fits in a usize"),
        "case {case:?}: expected the page asked for to be full, got {response:?}"
    );
    response
        .get("NextToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a NextToken for a page of a longer sequence, got \
                 {response:?}"
            )
        })
        .to_string()
}

/// Assert `body` is refused as a `NextToken` this read did not mint for the month being
/// read: the modelled 400 `ValidationException`, `Reason` `CANNOT_PARSE`, `Fields` naming
/// `NextToken` — and no page and no token at all.
async fn assert_refused(em: &Emulator, read: &Read, case: &str, body: &Value) {
    let (status, response) = em.post(read.path, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for a token minted while paging another month, got \
         {status} with {response:?}"
    );
    let code = response
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        code, "ValidationException",
        "case {case:?}: expected the modelled ValidationException, got {response:?}"
    );
    assert_eq!(
        response.get("Reason").and_then(Value::as_str),
        Some("CANNOT_PARSE"),
        "case {case:?}: expected Reason CANNOT_PARSE, got {response:?}"
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
        "case {case:?}: expected Fields to name NextToken, got {response:?}"
    );
    assert!(
        response.get(read.member).is_none(),
        "case {case:?}: the refusal must carry no page of elements, got {response:?}"
    );
    assert!(
        response.get("NextToken").is_none(),
        "case {case:?}: the refusal must carry no NextToken, got {response:?}"
    );
}

/// THE RULE, refusing side: a token minted while paging one month names no position in
/// another month's sequence, so handing it back on a read of a month whose element set
/// differs is refused — never read as a cut point that reports that month short of the
/// associations in force in it.
#[tokio::test]
async fn an_association_token_of_one_month_never_cuts_another_months_longer_sequence() {
    let world = fixture().await;
    let earlier = world.earlier.clone();
    let current = period(0);
    // A month a year on: its associations are the current month's, so a cursor minted there
    // names a key the current month's sequence holds — and is still a cursor into another
    // month's sequence.
    let a_year_on = period(12);

    for read in &world.reads {
        let crossings = vec![
            Case {
                what: "the earlier month's token on the current month, which holds one more \
                       association",
                minted_for: Some(earlier.as_str()),
                read_for: Some(current.as_str()),
            },
            Case {
                what: "the current month's token on the earlier month, whose set is a strict \
                       subset",
                minted_for: Some(current.as_str()),
                read_for: Some(earlier.as_str()),
            },
            Case {
                what: "the earlier month's token on the implicit current month",
                minted_for: Some(earlier.as_str()),
                read_for: None,
            },
            Case {
                what: "the implicit current month's token on the earlier month",
                minted_for: None,
                read_for: Some(earlier.as_str()),
            },
            Case {
                what: "a token of a month a year on, on the current month",
                minted_for: Some(a_year_on.as_str()),
                read_for: Some(current.as_str()),
            },
            Case {
                what: "the earlier month's token on a month a year on",
                minted_for: Some(earlier.as_str()),
                read_for: Some(a_year_on.as_str()),
            },
        ];

        for crossing in crossings {
            let case = format!("{} given {}", read.operation, crossing.what);
            assert_month_reports_its_whole_sequence(
                &world.em,
                read,
                &case,
                crossing.read_for,
                &earlier,
            )
            .await;
            let token = minted_in(&world.em, read, &case, crossing.minted_for).await;
            assert_refused(
                &world.em,
                read,
                &case,
                &request(
                    &in_period(&read.body, crossing.read_for),
                    json!({ "NextToken": token }),
                ),
            )
            .await;
            // A refused token changes nothing a later caller reads: the month still reports
            // every association in force in it.
            assert_month_reports_its_whole_sequence(
                &world.em,
                read,
                &case,
                crossing.read_for,
                &earlier,
            )
            .await;
        }
    }
}

/// THE RULE, resuming side: a token handed back on a read of the month it was minted for
/// resumes that month's own sequence exactly where the page stopped, whichever of the two
/// months of this world it is and however that month was named.
#[tokio::test]
async fn an_association_token_resumes_the_month_it_was_minted_while_paging() {
    let world = fixture().await;
    let earlier = world.earlier.clone();
    let current = period(0);

    for read in &world.reads {
        let same_month = vec![
            Case {
                what: "the earlier month, whose sequence is the shorter one",
                minted_for: Some(earlier.as_str()),
                read_for: Some(earlier.as_str()),
            },
            Case {
                what: "the current month, whose sequence holds one association more",
                minted_for: Some(current.as_str()),
                read_for: Some(current.as_str()),
            },
            Case {
                what: "the current month, left implicit after an explicit first page",
                minted_for: Some(current.as_str()),
                read_for: None,
            },
            Case {
                what: "the current month, named explicitly after an implicit first page",
                minted_for: None,
                read_for: Some(current.as_str()),
            },
        ];

        for spelling in same_month {
            let case = format!("{} resumed on {}", read.operation, spelling.what);
            let whole = expected(read, spelling.read_for, &earlier);
            let token = minted_in(&world.em, read, &case, spelling.minted_for).await;
            let body = request(
                &in_period(&read.body, spelling.read_for),
                json!({ "NextToken": token }),
            );
            let (status, response) = world.em.post(read.path, &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the month's own token to resume its listing, got \
                 {status} with {response:?}"
            );
            let page = usize::try_from(PAGE).expect("a page of one fits in a usize");
            assert_eq!(
                arns(&response, read.member),
                whole[page..].to_vec(),
                "case {case:?}: expected the elements the first page left out, {:?}, got \
                 {response:?}",
                &whole[page..]
            );
        }
    }
}
