// SPDX-License-Identifier: CC0-1.0

//! The listed handle of a custom detection rule association, over DRAWN populations
//! and DRAWN listings.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`]
//! (the harness lives in the `detector_lifecycle` suite), so routing, restJson1
//! serialization and the handlers are exercised together.
//!
//! THE RULE under test, one invariant asserted over generated inputs rather than over a
//! table of written-out rows: in the BYTES `ListCustomDetectionRuleAssociations` answers
//! with, each `AssociationSummary` serializes its `associationId` as a plain JSON string
//! that is exactly the trailing `/association/<id>` segment of that same entry's own
//! `arn`, and that string is accepted verbatim as the `AssociationId` label of the
//! association's own read. `AssociationId` is "the unique identifier for the association"
//! and `Arn` is "the Amazon Resource Name (ARN) of the association"
//! (`smithy_query(shape="AssociationSummary")`), and
//! `GetCustomDetectionRuleAssociation` takes `AssociationId` as a required `@httpLabel`
//! with the same `@length 1..64` / `@pattern ^[a-zA-Z0-9_-]{1,64}$`
//! (`smithy_query(operation="GetCustomDetectionRuleAssociation")`), so one summary's two
//! members name one resource and the id it advertises is that resource's handle.
//!
//! Why DRAWN rather than tabulated: the rule is an invariant over everything a listing
//! can be narrowed and cut by, and the sibling
//! `custom_detection_rule_association_listed_handle` suite — whose harness and whose
//! serialized-entry assertion this suite reuses rather than restates — already walks the
//! written-out table (every published rule, both modes, two Regions, each single filter,
//! a page walk one entry at a time). What is new here is the generated dimension: which
//! rules of the catalogue the account associates at all, in which mode, which of those
//! are then updated or deleted, which Region each listing addresses, which COMBINATION of
//! the two filters it carries (including a filter that selects nothing), and the page size
//! it is cut by, drawn from `MaxResults`'s published range as well as from the absent
//! member. Each draw is seeded, every assertion message carries its seed, and nothing is
//! written outside the in-process engine, so a failure is replayed by seeding the
//! generator with the seed the failure names.
//!
//! No `AssociationId` is written into this suite: every one is read off the emulator's
//! own response, because the digits of a minted id are a per-run value and only its form,
//! its agreement with its own ARN and its usability are requirements. An association a
//! draw leaves in place is recorded in the sibling suite's `Made`, so both suites record a
//! made association the same way.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};

use crate::custom_detection_rule_association_listed_handle::{
    assert_serialized_identity, page_walk, Listing, Made, ASSOCIATIONS_PATH, MODES, RULES_PATH,
};
use crate::detector_lifecycle::{call_in, new_router, published_rule_ids, Router};

/// The Regions a draw places its associations in, so an entry's handle is asserted to be
/// its own Region's rather than assumed.
const REGIONS: [&str; 2] = ["us-west-2", "ap-northeast-1"];

/// `MaxResults` is `range 1..100`
/// (`smithy_query(operation="ListCustomDetectionRuleAssociations")`), so these are the
/// published bounds a drawn page size is taken between.
const MAX_RESULTS_MIN: i64 = 1;
/// See [`MAX_RESULTS_MIN`].
const MAX_RESULTS_MAX: i64 = 100;

/// How many populations are drawn. Bounded so the suite stays fast: each draw stands up
/// its own engine and makes at most one association per published rule per Region.
const DRAWS: usize = 8;

/// The first seed. Each draw's own seed is this one advanced by the draw's index, and
/// every assertion message carries it.
const BASE_SEED: u64 = 0x5155_4152_4444_5554;

/// The fewest entries the draws are expected to put the rule to, so a run that drew
/// nothing to assert on fails instead of passing vacuously.
const ASSERTED_ENTRIES_MIN: usize = DRAWS;

/// The seed of each drawn population, in order.
fn seeds() -> Vec<u64> {
    (0..DRAWS as u64)
        .map(|index| BASE_SEED.wrapping_add(index.wrapping_mul(0x9E37_79B9_7F4A_7C15)))
        .collect()
}

/// What a draw does to an association once it is made: leave it alone, move its mode, or
/// delete it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Churn {
    Kept,
    Updated(&'static str),
    Deleted,
}

/// One drawn listing: the Region it addresses, the filters it carries and the page size
/// it is cut by.
#[derive(Debug, Clone)]
struct DrawnListing {
    region: &'static str,
    rule_id: Option<String>,
    mode: Option<&'static str>,
    max_results: Option<i64>,
}

impl DrawnListing {
    /// The query string this listing is addressed with, empty when the draw names no
    /// filter and no page size.
    fn query(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(rule_id) = &self.rule_id {
            parts.push(format!("ruleId={rule_id}"));
        }
        if let Some(mode) = self.mode {
            parts.push(format!("mode={mode}"));
        }
        if let Some(max_results) = self.max_results {
            parts.push(format!("maxResults={max_results}"));
        }
        match parts.is_empty() {
            true => String::new(),
            false => format!("?{}", parts.join("&")),
        }
    }

    /// The associations of `live` this listing selects: both filters narrow by equality on
    /// the member the summary reports, and only the Region that made an association
    /// reports it.
    fn selects<'a>(&self, live: &'a [Made]) -> Vec<&'a Made> {
        live.iter()
            .filter(|row| row.region == self.region)
            .filter(|row| {
                self.rule_id
                    .as_deref()
                    .is_none_or(|rule_id| row.rule_id == rule_id)
            })
            .filter(|row| self.mode.is_none_or(|mode| row.mode == mode))
            .collect()
    }
}

/// Make the association for `rule_id` in `mode` through `region`'s endpoint, answering
/// the `AssociationDetail` the create reported.
async fn associate(router: &Router, region: &str, rule_id: &str, mode: &str, case: &str) -> Value {
    let (status, body) = call_in(
        router,
        region,
        "POST",
        ASSOCIATIONS_PATH,
        Some(json!({"ruleId": rule_id, "mode": mode})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: create-association {rule_id}/{mode} expected 200, got {status} {body:?}"
    );
    body["ruleAssociation"].clone()
}

/// The text member `name` of `payload`, failing the test when it is not a JSON string.
fn text(payload: &Value, name: &str, case: &str) -> String {
    payload[name]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {name} to be a JSON string, got {payload:?}")
        })
        .to_string()
}

/// Draw what one Region holds for one published rule: whether an association is made at
/// all, the mode it is made in, and what happens to it afterwards — answering the
/// association the draw left in place, or `None` for a pair it associated never or deleted
/// again.
///
/// A rule carries at most one association per Region, so each pair is drawn at most once.
async fn drawn_association(
    router: &Router,
    region: &'static str,
    rule_id: &str,
    rng: &mut StdRng,
    case: &str,
) -> Option<Made> {
    // A quarter of the pairs are left unassociated, so a drawn listing can select a strict
    // subset of the catalogue and a drawn `ruleId` filter can name a rule that has no
    // association at all.
    if rng.random_range(0..4) == 0 {
        return None;
    }
    let created_mode = MODES[rng.random_range(0..MODES.len())];
    // Each outcome is drawn BEFORE anything is sent, so the program a seed describes is
    // the same however the responses come back.
    let churn = match rng.random_range(0..6) {
        0 | 1 => Churn::Updated(MODES[rng.random_range(0..MODES.len())]),
        2 => Churn::Deleted,
        _ => Churn::Kept,
    };

    let created = associate(router, region, rule_id, created_mode, case).await;
    let id = text(&created, "associationId", case);
    let arn = text(&created, "arn", case);
    let association = format!("{RULES_PATH}/{rule_id}/association/{id}");

    match churn {
        Churn::Kept => {}
        // Updated: the mode moves, so the listing reports the association under a mode the
        // create did not set — and its identity has to survive that.
        Churn::Updated(mode) => {
            let (status, body) =
                call_in(router, region, "PUT", &association, Some(json!({"mode": mode}))).await;
            assert_eq!(
                status, 200,
                "case {case:?}: update-association {rule_id} expected 200, got {status} {body:?}"
            );
        }
        // Deleted: the association leaves the account, so a drawn listing is asserted over
        // a set the creates alone do not describe.
        Churn::Deleted => {
            let (status, body) = call_in(router, region, "DELETE", &association, None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: delete-association {rule_id} expected 200, got {status} {body:?}"
            );
            return None;
        }
    }

    Some(Made {
        region,
        rule_id: rule_id.to_string(),
        mode: match churn {
            Churn::Updated(mode) => mode,
            _ => created_mode,
        },
        id,
        arn,
    })
}

/// Draw what the account holds: one [`drawn_association`] per Region per published rule.
async fn drawn_population(
    router: &Router,
    rules: &[String],
    rng: &mut StdRng,
    case: &str,
) -> Vec<Made> {
    let mut live: Vec<Made> = Vec::new();
    for region in REGIONS {
        for rule_id in rules {
            if let Some(row) = drawn_association(router, region, rule_id, rng, case).await {
                live.push(row);
            }
        }
    }
    live
}

/// Draw the listing each Region is read with: a filter combination over the two members
/// the operation filters by, and a page size taken from `MaxResults`'s published bounds,
/// from inside the range, or left absent.
fn drawn_listings(rules: &[String], rng: &mut StdRng) -> Vec<DrawnListing> {
    REGIONS
        .iter()
        .map(|region| DrawnListing {
            region,
            rule_id: match rng.random_range(0..3) {
                0 => None,
                _ => Some(rules[rng.random_range(0..rules.len())].clone()),
            },
            mode: match rng.random_range(0..3) {
                0 => None,
                _ => Some(MODES[rng.random_range(0..MODES.len())]),
            },
            max_results: match rng.random_range(0..4) {
                0 => None,
                1 => Some(MAX_RESULTS_MIN),
                2 => Some(MAX_RESULTS_MAX),
                _ => Some(rng.random_range(MAX_RESULTS_MIN + 1..MAX_RESULTS_MAX)),
            },
        })
        .collect()
}

/// Assert the listed id is the association's HANDLE: taken out of the listing — rule id
/// and all, since `AssociationSummary.RuleId` is itself required and makes the entry
/// self-describing (`smithy_query(shape="AssociationSummary")`) — it is accepted verbatim
/// as the `AssociationId` label of the read, which answers for the association the listing
/// reported it for.
async fn assert_handle_resolves(router: &Router, entry: &Value, row: &Made, case: &str) {
    let listed = text(entry, "associationId", case);
    let listed_rule = text(entry, "ruleId", case);
    assert_eq!(
        listed_rule, row.rule_id,
        "case {case:?}: expected the entry to name the rule it was made for, got {entry:?}"
    );
    let (status, body) = call_in(
        router,
        row.region,
        "GET",
        &format!("{RULES_PATH}/{listed_rule}/association/{listed}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the listed handle {listed:?} must resolve, got {status} {body:?}"
    );
    let read = body["ruleAssociation"].clone();
    assert_eq!(
        text(&read, "arn", case),
        row.arn,
        "case {case:?}: the listed handle must address its own association, got {read:?}"
    );
    assert_eq!(
        text(&read, "associationId", case),
        listed,
        "case {case:?}: the read must echo the handle it was reached through, got {read:?}"
    );
    assert_eq!(
        text(&read, "ruleId", case),
        listed_rule,
        "case {case:?}: the read must report the rule the entry named, got {read:?}"
    );
    assert_eq!(
        text(&read, "mode", case),
        row.mode,
        "case {case:?}: the read must report the mode the association is in, got {read:?}"
    );
}

/// The ARNs a walk reported, sorted: the listing is asserted to report the selected
/// associations ONCE EACH, which is a fact about membership rather than about the order
/// the entries arrive in (the `custom_detection_rule_association_listing` suite is where
/// the order itself is pinned).
fn sorted_arns(entries: &[Value], case: &str) -> Vec<String> {
    let mut arns: Vec<String> = entries
        .iter()
        .map(|entry| text(entry, "arn", case))
        .collect();
    arns.sort();
    arns
}

#[tokio::test]
async fn every_drawn_listing_serves_each_entry_the_handle_its_own_arn_names() {
    let mut asserted = 0usize;
    for (draw, seed) in seeds().into_iter().enumerate() {
        // One engine and one generator per draw, seeded from that draw's own seed, which
        // every assertion message carries.
        let mut rng = StdRng::seed_from_u64(seed);
        let router = new_router();
        let rules = published_rule_ids(&router).await;
        let case = format!("seed {seed} / draw {draw}");

        let live = drawn_population(&router, &rules, &mut rng, &case).await;

        for listing in drawn_listings(&rules, &mut rng) {
            let selected = listing.selects(&live);
            let case = format!(
                "{case} / {} association(s) in {} / ruleId {:?} / mode {:?} / maxResults {:?}",
                selected.len(),
                listing.region,
                listing.rule_id,
                listing.mode,
                listing.max_results
            );

            let pages: Vec<Listing> = page_walk(
                &router,
                listing.region,
                &listing.query(),
                selected.len() + 2,
                &case,
            )
            .await;
            if let Some(max_results) = listing.max_results {
                for (index, page) in pages.iter().enumerate() {
                    assert!(
                        page.entries.len() as i64 <= max_results,
                        "case {case:?}: page {index} must be no longer than the page size asked \
                         for, got {} entries",
                        page.entries.len()
                    );
                }
            }

            // The walk reports exactly the associations the listing selects, once each: a
            // page token changes WHICH entries a page carries, never the identity one
            // advertises.
            let reported: Vec<Value> = pages
                .iter()
                .flat_map(|page| page.entries.iter().cloned())
                .collect();
            let mut selected_arns: Vec<String> =
                selected.iter().map(|row| row.arn.clone()).collect();
            selected_arns.sort();
            let reported_arns = sorted_arns(&reported, &case);
            assert_eq!(
                reported_arns, selected_arns,
                "case {case:?}: expected the walk to report the selected associations once each, \
                 got {reported_arns:?}"
            );

            for page in &pages {
                for entry in &page.entries {
                    let arn = text(entry, "arn", &case);
                    let row = *selected.iter().find(|row| row.arn == arn).unwrap_or_else(|| {
                        panic!("case {case:?}: expected an entry of the selected set, got {entry:?}")
                    });
                    // The serialized-entry rule itself is the sibling suite's assertion, so
                    // the two suites cannot drift on what a listed handle must look like.
                    assert_serialized_identity(&page.raw, entry, &row.id, &row.arn, &case);
                    assert_handle_resolves(&router, entry, row, &case).await;
                    asserted += 1;
                }
            }
        }
    }

    assert!(
        asserted >= ASSERTED_ENTRIES_MIN,
        "expected the draws to put the rule to at least {ASSERTED_ENTRIES_MIN} listed entries, \
         got {asserted}"
    );
}
