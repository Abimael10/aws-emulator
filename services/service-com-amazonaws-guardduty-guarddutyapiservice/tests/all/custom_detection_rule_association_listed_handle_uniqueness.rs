// SPDX-License-Identifier: CC0-1.0

//! A listing addresses each custom detection rule association ONCE, under the identity
//! that association's own history established.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`]
//! (the harness lives in the `detector_lifecycle` suite), so routing, restJson1
//! serialization and the handlers are exercised together.
//!
//! THE RULE under test, one sentence: an account holds at most ONE association per
//! `(Region, RuleId)`, so in any page `ListCustomDetectionRuleAssociations` answers no
//! two entries share an `arn`, an `associationId` or a `ruleId`, the single entry
//! reporting a rule advertises the identity that rule's CURRENT association was
//! established under, and an identity the account no longer holds appears in no listing
//! and resolves nowhere.
//!
//! Why those three facts are one rule: `AssociationId` is "the unique identifier for the
//! association" and `Arn` is "the Amazon Resource Name (ARN) of the association"
//! (`smithy_query(shape="AssociationSummary")`); a second create for a rule the account
//! has already associated is the modelled `ConflictException` (httpError 409) and a
//! replayed `ClientToken` is `@idempotencyToken`
//! (`smithy_query(operation="CreateCustomDetectionRuleAssociation")`), so the number of
//! creates issued for one rule never changes how many associations it has nor which
//! identity they carry; and `AssociationId` is a required `@httpLabel` of the read, the
//! update and the delete, which answer the modelled `ResourceNotFoundException`
//! (httpError 404) for a pair the account does not hold
//! (`smithy_query(operation="GetCustomDetectionRuleAssociation")`).
//!
//! What is new here, beside the sibling suites: those assert that a listed entry's
//! `associationId` agrees with its own `arn` and with the create that minted it
//! (`custom_detection_rule_association_listed_handle`, over a written-out table, and
//! `..._drawn_listed_handles`, over drawn populations) and that one payload repeats one
//! id (`..._identity`). None of them asserts that the ARN a listing reports addresses
//! exactly ONE entry — the ambiguity a second record for one `(Region, RuleId)` would
//! create — nor that an identity a delete retired has left the listing once the rule is
//! associated AGAIN, which is the state a lifecycle ends in. The rows below therefore
//! span the HISTORIES that could leave a second record or a stale identity behind: a
//! replayed create, a refused duplicate create, a mode update, and a delete followed by a
//! fresh create.
//!
//! No `AssociationId` is written into this suite: every one is read off the emulator's
//! own response, because the digits of a minted id are a per-run value and only its
//! uniqueness, its agreement with its own ARN and its usability are requirements.

use serde_json::{json, Value};

use crate::custom_detection_rule_association_identity::arn_association_id;
use crate::custom_detection_rule_association_listed_handle::{
    assert_serialized_identity, listing, page_walk, Listing, ASSOCIATIONS_PATH, MODES, RULES_PATH,
};
use crate::detector_lifecycle::{call_in, error_code, new_router, published_rule_ids, Router};

/// The Regions the histories are played out in, so an entry is asserted to be its own
/// Region's rather than assumed — and so a rule associated in BOTH Regions is still one
/// entry per listing.
const REGIONS: [&str; 2] = ["us-west-2", "eu-central-1"];

/// `MaxResults` is `range 1..100`
/// (`smithy_query(operation="ListCustomDetectionRuleAssociations")`), so a page this size
/// reports every association a Region holds at once.
const MAX_RESULTS: i64 = 100;

/// The modelled error shape a pair the account holds no association for is refused with
/// (`smithy_query(operation="GetCustomDetectionRuleAssociation")`).
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The modelled error shape a second create for an already associated rule is refused
/// with (`smithy_query(operation="CreateCustomDetectionRuleAssociation")`).
const CONFLICT: &str = "ConflictException";

/// What happened to a rule's association before the listing is read: every history that
/// could leave a second record for one `(Region, RuleId)` or a stale identity behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum History {
    /// One create and nothing else.
    Created,
    /// A create under a `ClientToken`, then that same create replayed twice.
    CreateReplayed,
    /// A create, then a second create for the same rule — the modelled 409, which mints
    /// nothing.
    CreateRefused,
    /// A create, then an update that moves the mode.
    ModeUpdated,
    /// A create, a delete that retires that identity, then a create again.
    Remade,
}

/// Every history, in the order the rows cycle through them.
const HISTORIES: [History; 5] = [
    History::Created,
    History::CreateReplayed,
    History::CreateRefused,
    History::ModeUpdated,
    History::Remade,
];

/// One rule's association after its history: where it lives, the identity it ended up
/// under, and the identities the history retired.
struct Row {
    region: &'static str,
    rule_id: String,
    /// The history this rule's association went through.
    history: History,
    /// The mode the live association is in once the history has played out.
    mode: &'static str,
    /// The identity the account holds the association under now.
    id: String,
    /// That association's ARN.
    arn: String,
    /// Identities this rule's associations were held under and are not any more.
    retired: Vec<String>,
    /// The label a failure of this row names.
    case: String,
}

/// The other `AssociationMode` member, so a mode update asks for a mode the association
/// is not in.
fn other_mode(mode: &str) -> &'static str {
    MODES
        .into_iter()
        .find(|candidate| *candidate != mode)
        .expect("AssociationMode carries more than one member")
}

/// Issue one `CreateCustomDetectionRuleAssociation`, with the `ClientToken` member
/// present only when `token` names one.
async fn create(
    router: &Router,
    region: &str,
    rule_id: &str,
    mode: &str,
    token: Option<&str>,
) -> (u16, Value) {
    let mut body = json!({"ruleId": rule_id, "mode": mode});
    if let Some(token) = token {
        body["clientToken"] = Value::String(token.to_string());
    }
    call_in(router, region, "POST", ASSOCIATIONS_PATH, Some(body)).await
}

/// The `(associationId, arn)` of a create's `ruleAssociation`, failing the test when the
/// create did not succeed.
fn created_identity(status: u16, body: &Value, case: &str) -> (String, String) {
    assert_eq!(
        status, 200,
        "case {case:?}: create-association expected 200, got {status} {body:?}"
    );
    let association = &body["ruleAssociation"];
    let id = association["associationId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an associationId, got {body:?}"))
        .to_string();
    let arn = association["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an arn, got {body:?}"))
        .to_string();
    (id, arn)
}

/// The read of one `(rule_id, id)` pair, with the status it answered.
async fn read_association(router: &Router, region: &str, rule_id: &str, id: &str) -> (u16, Value) {
    call_in(
        router,
        region,
        "GET",
        &format!("{RULES_PATH}/{rule_id}/association/{id}"),
        None,
    )
    .await
}

/// Play `history` out for `rule_id` in `region` and report what the account is left
/// holding.
async fn play(
    router: &Router,
    region: &'static str,
    rule_id: &str,
    history: History,
    mode: &'static str,
) -> Row {
    let case = format!("{rule_id}/{mode} in {region}: {history:?}");
    let (status, body) = create(router, region, rule_id, mode, None).await;
    let (first_id, first_arn) = created_identity(status, &body, &case);
    let mut row = Row {
        region,
        rule_id: rule_id.to_string(),
        history,
        mode,
        id: first_id.clone(),
        arn: first_arn.clone(),
        retired: Vec::new(),
        case: case.clone(),
    };

    match history {
        History::Created => {}
        History::CreateReplayed => {
            // The first create carried no token, so the replay needs an association that
            // did: delete this one and make it again under a token, then replay that
            // create twice. `ClientToken` is `len 0..64`
            // (`smithy_query(operation="CreateCustomDetectionRuleAssociation")`), so the
            // token below is a value of the member.
            let (status, body) = call_in(
                router,
                region,
                "DELETE",
                &format!("{RULES_PATH}/{rule_id}/association/{first_id}"),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: delete-association expected 200, got {status} {body:?}"
            );
            row.retired.push(first_id);

            let token = "listed-handle-uniqueness";
            let (status, body) = create(router, region, rule_id, mode, Some(token)).await;
            let (id, arn) = created_identity(status, &body, &case);
            for replay in 1..=2 {
                let case = format!("{case}: replay {replay}");
                let (status, body) = create(router, region, rule_id, mode, Some(token)).await;
                let (replayed_id, replayed_arn) = created_identity(status, &body, &case);
                assert_eq!(
                    replayed_id, id,
                    "case {case:?}: a replayed create must answer with the association the first \
                     call made, got {body:?}"
                );
                assert_eq!(
                    replayed_arn, arn,
                    "case {case:?}: a replayed create must answer with the first call's arn, got \
                     {body:?}"
                );
            }
            row.id = id;
            row.arn = arn;
        }
        History::CreateRefused => {
            // A second create for an associated rule is the modelled conflict whatever
            // mode it names, and mints nothing.
            let (status, body) = create(router, region, rule_id, other_mode(mode), None).await;
            assert_eq!(
                status, 409,
                "case {case:?}: a second create expected 409, got {status} {body:?}"
            );
            assert_eq!(
                error_code(&body),
                CONFLICT,
                "case {case:?}: expected the {CONFLICT} class, got {body:?}"
            );
        }
        History::ModeUpdated => {
            let to = other_mode(mode);
            let (status, body) = call_in(
                router,
                region,
                "PUT",
                &format!("{RULES_PATH}/{rule_id}/association/{first_id}"),
                Some(json!({"mode": to})),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: update-association expected 200, got {status} {body:?}"
            );
            row.mode = to;
        }
        History::Remade => {
            let (status, body) = call_in(
                router,
                region,
                "DELETE",
                &format!("{RULES_PATH}/{rule_id}/association/{first_id}"),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: delete-association expected 200, got {status} {body:?}"
            );
            row.retired.push(first_id.clone());

            let (status, body) = create(router, region, rule_id, mode, None).await;
            let (id, arn) = created_identity(status, &body, &case);
            assert_ne!(
                id, first_id,
                "case {case:?}: a fresh association must not reuse the retired identity, got \
                 {body:?}"
            );
            row.id = id;
            row.arn = arn;
        }
    }
    row
}

/// The `associationId` of a listed entry, asserted to be a JSON string.
fn listed_id(entry: &Value, case: &str) -> String {
    entry["associationId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected associationId to be a JSON string, got {entry:?}")
        })
        .to_string()
}

/// The `arn` of a listed entry.
fn listed_arn(entry: &Value, case: &str) -> String {
    entry["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an arn, got {entry:?}"))
        .to_string()
}

/// Assert the rule on one page: every entry is addressable ONCE — by its own ARN, by its
/// own id and by its own rule — and advertises the identity its row's history left the
/// association under.
fn assert_entries_addressable(page: &Listing, live: &[&Row], case: &str) {
    let mut seen: Vec<(String, String, String)> = Vec::new();
    for entry in &page.entries {
        let id = listed_id(entry, case);
        let arn = listed_arn(entry, case);
        let rule_id = entry["ruleId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a ruleId, got {entry:?}"))
            .to_string();
        let named = arn_association_id(&arn).unwrap_or_else(|| {
            panic!("case {case:?}: expected an arn naming an association, got {entry:?}")
        });
        assert_eq!(
            named, id,
            "case {case:?}: expected the entry's arn to end in its own associationId {id:?}, got \
             {entry:?}"
        );

        // The ARN, the id and the rule each address ONE entry of the page: a second
        // record for one (Region, RuleId) would make any of the three ambiguous.
        assert!(
            !seen.iter().any(|(seen_arn, _, _)| *seen_arn == arn),
            "case {case:?}: expected the arn {arn:?} to address one entry, already saw {seen:?}"
        );
        assert!(
            !seen.iter().any(|(_, seen_id, _)| *seen_id == id),
            "case {case:?}: expected the associationId {id:?} to address one entry, already saw \
             {seen:?}"
        );
        assert!(
            !seen.iter().any(|(_, _, seen_rule)| *seen_rule == rule_id),
            "case {case:?}: expected one association per rule, {rule_id:?} already seen in {seen:?}"
        );

        let row = live
            .iter()
            .find(|row| row.arn == arn)
            .unwrap_or_else(|| panic!("case {case:?}: expected an entry of this Region's own set, got {entry:?}"));
        assert_eq!(
            row.rule_id, rule_id,
            "case {case:?}: expected the entry to report its own rule, got {entry:?}"
        );
        assert_eq!(
            id, row.id,
            "case {case:?} ({}): expected the entry to advertise the identity the account holds \
             the association under {:?}, got {entry:?}",
            row.case, row.id
        );
        assert_serialized_identity(&page.raw, entry, &row.id, &row.arn, case);
        seen.push((arn, id, rule_id));
    }
}

/// The listings one Region's set is read through: the whole set, each rule, each mode,
/// and both filters together — paired with the label a failure names.
fn queries(live: &[&Row]) -> Vec<(String, String)> {
    let mut queries = vec![(
        format!("?maxResults={MAX_RESULTS}"),
        "unfiltered listing".to_string(),
    )];
    for row in live {
        queries.push((
            format!("?maxResults={MAX_RESULTS}&ruleId={}", row.rule_id),
            format!("listing filtered by rule {}", row.rule_id),
        ));
        queries.push((
            format!(
                "?maxResults={MAX_RESULTS}&ruleId={}&mode={}",
                row.rule_id, row.mode
            ),
            format!("listing filtered by rule {} and mode {}", row.rule_id, row.mode),
        ));
    }
    for mode in MODES {
        queries.push((
            format!("?maxResults={MAX_RESULTS}&mode={mode}"),
            format!("listing filtered by mode {mode}"),
        ));
    }
    queries
}

#[tokio::test]
async fn a_listing_addresses_each_association_once_under_the_identity_it_is_held_under() {
    let router = new_router();
    let rules = published_rule_ids(&router).await;

    // Each row: a published rule in a Region, the history its association went through
    // and the mode it was created in — both cycling with the two indices, so every
    // history and both modes are played out across the catalogue and the Regions.
    let mut rows: Vec<Row> = Vec::new();
    for (region_index, region) in REGIONS.into_iter().enumerate() {
        for (rule_index, rule_id) in rules.iter().enumerate() {
            let step = rule_index + region_index * rules.len();
            let history = HISTORIES[step % HISTORIES.len()];
            let mode = MODES[step % MODES.len()];
            rows.push(play(&router, region, rule_id, history, mode).await);
        }
    }
    for history in HISTORIES {
        assert!(
            rows.iter().any(|row| row.history == history),
            "expected the rows to span every history, {history:?} was never played out over {} rows",
            rows.len()
        );
    }

    for region in REGIONS {
        let live: Vec<&Row> = rows.iter().filter(|row| row.region == region).collect();

        for (query, label) in queries(&live) {
            let case = format!("{region}: {label}");
            let page = listing(&router, region, &query, &case).await;
            assert_entries_addressable(&page, &live, &case);
        }

        // The whole set, read once: every live association is there exactly once, and the
        // handle it advertises resolves to itself.
        let case = format!("{region}: the Region's whole set");
        let page = listing(&router, region, &format!("?maxResults={MAX_RESULTS}"), &case).await;
        assert_eq!(
            page.entries.len(),
            live.len(),
            "case {case:?}: expected one entry per association the Region holds, got {:?}",
            page.entries
        );
        for row in &live {
            let matching: Vec<&Value> = page
                .entries
                .iter()
                .filter(|entry| listed_arn(entry, &case) == row.arn)
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "case {case:?} ({}): expected the arn {} to address one entry, got {matching:?}",
                row.case,
                row.arn
            );
            let listed = listed_id(matching[0], &case);
            let (status, body) = read_association(&router, region, &row.rule_id, &listed).await;
            assert_eq!(
                status, 200,
                "case {case:?} ({}): the listed handle must resolve, got {status} {body:?}",
                row.case
            );
            assert_eq!(
                body["ruleAssociation"]["arn"].as_str(),
                Some(row.arn.as_str()),
                "case {case:?} ({}): the listed handle must address its own association, got \
                 {body:?}",
                row.case
            );
            assert_eq!(
                body["ruleAssociation"]["mode"].as_str(),
                Some(row.mode),
                "case {case:?} ({}): expected the mode the history left the association in, got \
                 {body:?}",
                row.case
            );
        }

        // A page walk one entry at a time reports the same set, once each: a page token
        // changes WHICH entry a page carries, never how many entries an ARN addresses.
        let case = format!("{region}: page walk one entry at a time");
        let pages = page_walk(&router, region, "?maxResults=1", live.len() + 1, &case).await;
        let mut walked: Vec<(String, String)> = Vec::new();
        for page in &pages {
            assert!(
                page.entries.len() <= 1,
                "case {case:?}: expected no page longer than the page size asked for, got {:?}",
                page.entries
            );
            assert_entries_addressable(page, &live, &case);
            for entry in &page.entries {
                walked.push((listed_arn(entry, &case), listed_id(entry, &case)));
            }
        }
        assert_eq!(
            walked.len(),
            live.len(),
            "case {case:?}: expected the walk to report each association once, got {walked:?}"
        );
        for row in &live {
            assert_eq!(
                walked
                    .iter()
                    .filter(|(arn, id)| *arn == row.arn && *id == row.id)
                    .count(),
                1,
                "case {case:?} ({}): expected the walk to report {} under {} once, got {walked:?}",
                row.case,
                row.arn,
                row.id
            );
        }

        // An identity the account no longer holds is in no listing and resolves nowhere,
        // however the Region is listed.
        for row in &live {
            for retired in &row.retired {
                let case = format!("{region} ({}): retired identity", row.case);
                assert!(
                    !page
                        .entries
                        .iter()
                        .any(|entry| listed_id(entry, &case) == *retired),
                    "case {case:?}: a retired identity must have left the listing, got {:?}",
                    page.entries
                );
                let (status, body) =
                    read_association(&router, region, &row.rule_id, retired).await;
                assert_eq!(
                    status, 404,
                    "case {case:?}: a retired identity must not resolve, got {status} {body:?}"
                );
                assert_eq!(
                    error_code(&body),
                    NOT_FOUND,
                    "case {case:?}: expected the {NOT_FOUND} class, got {body:?}"
                );
            }
        }
    }
}
