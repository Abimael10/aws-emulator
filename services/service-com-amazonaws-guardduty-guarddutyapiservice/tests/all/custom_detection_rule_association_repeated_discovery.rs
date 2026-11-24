// SPDX-License-Identifier: CC0-1.0

//! Discovery REPEATED: the identity `ListCustomDetectionRuleAssociations` advertises for
//! an association is a function of the stored association alone, so reading it again — any
//! number of times, at any point of the association's lifecycle — answers the same thing.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`]
//! (the harness lives in the `detector_lifecycle` suite), so routing, restJson1
//! serialization and the handlers are exercised together.
//!
//! THE RULE under test: `ListCustomDetectionRuleAssociations` is `@readonly`
//! (`smithy_query(operation="ListCustomDetectionRuleAssociations")`), so its answer is
//! computed from the stored associations and nothing a read does may change it — each
//! entry advertises the `AssociationId` the create that made that association
//! established, "the unique identifier for the association", serialized byte for byte as
//! the trailing `/association/<id>` segment of that same entry's own `Arn`, "the Amazon
//! Resource Name (ARN) of the association" (`smithy_query(shape="AssociationSummary")`),
//! and that string is accepted verbatim as the `AssociationId` `@httpLabel` of the
//! association's own read (`smithy_query(operation="GetCustomDetectionRuleAssociation")`).
//! A listing repeated over an unchanged store therefore answers the same bytes, and a
//! mutation between two listings moves only what it was asked to move.
//!
//! What is new here, beside the sibling association suites (`..._identity`, which walks
//! one association's own lifecycle, `..._listed_handle`, which tabulates rules, Regions,
//! modes and filters, `..._identity_independence`, which pins the identity against
//! neighbouring MUTATIONS, and `..._drawn_listed_handles`, which draws populations and
//! listings): the dimension here is the REPEAT — the same discovery issued again and
//! again, with reads of the discovered associations interleaved, which is the normal
//! list-then-act traffic and the one way a report could hand out a freshly minted
//! identity without any mutation being involved. The table's rows differ along the
//! lifecycle stage the discovery runs at, the form the discovery takes (unfiltered, each
//! filter the operation carries, no query at all, and a page walk one entry at a time)
//! and the repeat index.
//!
//! No `AssociationId` and no ARN is written into this suite: each is read off the
//! emulator's own response, because the characters a run mints are a per-run value and
//! only the binding, the agreement with the entry's own ARN, the stability under repeated
//! reads and the handle's usability are requirements.

use serde_json::{json, Value};

use crate::custom_detection_rule_association_identity::{
    arn_association_id, associate, entry_with_arn,
};
use crate::custom_detection_rule_association_listed_handle::{
    assert_serialized_identity, page_walk, Listing, MODES, RULES_PATH,
};
use crate::detector_lifecycle::{call_in, new_router, published_rule_ids, Router};

/// The Region every call of this suite addresses. One Region is enough: which Region
/// reports an association is the sibling suites' subject, and the repeat is this one's.
const REGION: &str = "us-west-2";

/// `MaxResults` is `range 1..100`
/// (`smithy_query(operation="ListCustomDetectionRuleAssociations")`), so one page this
/// size reports every association this suite makes at once.
const MAX_RESULTS: i64 = 100;

/// How often each discovery is repeated. Three is the smallest count that tells a report
/// which answers the same thing for ever from one which drifts per call.
const REPEATS: usize = 3;

/// One association this suite made: the rule it binds, the mode it is currently in, and
/// the identity its own create established.
struct Subject {
    rule_id: String,
    mode: &'static str,
    id: String,
    arn: String,
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

/// One discovery: the query string it addresses, the most pages it can need, and the
/// label a failure names it by.
struct Discovery {
    query: String,
    pages_max: usize,
    label: String,
}

/// Every discovery form that has to report `subject`: the Region's whole set, each filter
/// the operation takes, the request that names no query at all, and a page walk one entry
/// at a time.
fn discoveries(subject: &Subject, made: usize) -> Vec<Discovery> {
    vec![
        Discovery {
            query: format!("?maxResults={MAX_RESULTS}"),
            pages_max: 2,
            label: "unfiltered listing".to_string(),
        },
        Discovery {
            query: format!("?maxResults={MAX_RESULTS}&ruleId={}", subject.rule_id),
            pages_max: 2,
            label: "listing filtered by rule".to_string(),
        },
        Discovery {
            query: format!("?maxResults={MAX_RESULTS}&mode={}", subject.mode),
            pages_max: 2,
            label: "listing filtered by mode".to_string(),
        },
        Discovery {
            query: String::new(),
            pages_max: 2,
            label: "listing naming no filter and no page size".to_string(),
        },
        Discovery {
            query: "?maxResults=1".to_string(),
            pages_max: made + 1,
            label: "page walk one entry at a time".to_string(),
        },
    ]
}

/// Every entry the discovery `query` reports, together with the bytes of the pages it
/// arrived in joined in page order.
///
/// Joined rather than parsed-and-reassembled because the serialized form is what is
/// asserted: an entry appears in exactly one page, so the serialized members of each are
/// in these bytes, and two runs of the same discovery over an unchanged store have to
/// produce the same string.
async fn discover(
    router: &Router,
    query: &str,
    pages_max: usize,
    case: &str,
) -> (Vec<Value>, String) {
    let pages: Vec<Listing> = page_walk(router, REGION, query, pages_max, case).await;
    let entries: Vec<Value> = pages
        .iter()
        .flat_map(|page| page.entries.iter().cloned())
        .collect();
    let raw = pages
        .iter()
        .map(|page| page.raw.clone())
        .collect::<Vec<String>>()
        .join("\n");
    (entries, raw)
}

/// Assert the entry `subject`'s discovery reports carries the identity its create
/// established, and that the id the entry advertises — never the one the create
/// reported — is accepted as the label of the association's own read.
async fn assert_entry_is_the_handle(
    router: &Router,
    entries: &[Value],
    raw: &str,
    subject: &Subject,
    case: &str,
) {
    let entry = entry_with_arn(entries, &subject.arn, case);
    assert_serialized_identity(raw, &entry, &subject.id, &subject.arn, case);
    assert_eq!(
        text(&entry, "ruleId", case),
        subject.rule_id,
        "case {case:?}: expected the entry to name the rule it was made for, got {entry:?}"
    );
    assert_eq!(
        text(&entry, "mode", case),
        subject.mode,
        "case {case:?}: expected the entry to report the mode the association is in, got {entry:?}"
    );

    let listed = text(&entry, "associationId", case);
    let (status, body) = call_in(
        router,
        REGION,
        "GET",
        &format!("{RULES_PATH}/{}/association/{listed}", subject.rule_id),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the listed handle {listed:?} must resolve, got {status} {body:?}"
    );
    let read = body["ruleAssociation"].clone();
    assert_eq!(
        text(&read, "associationId", case),
        subject.id,
        "case {case:?}: the read must answer under the identity the create established, got {read:?}"
    );
    assert_eq!(
        text(&read, "arn", case),
        subject.arn,
        "case {case:?}: the listed handle must address its own association, got {read:?}"
    );
    assert_eq!(
        arn_association_id(&text(&read, "arn", case)).map(str::to_string),
        Some(listed.clone()),
        "case {case:?}: the read's arn must end in the handle it was reached through, got {read:?}"
    );
}

/// What has happened to the subjects by the time a discovery runs. Each stage is applied
/// once, in this order, to the same store.
#[derive(Debug, Clone, Copy)]
enum Stage {
    /// Nothing but the creates.
    Created,
    /// Every subject read through the handle its last discovery advertised — a pure read
    /// between two listings.
    ReadThroughListedHandle,
    /// Every subject's mode moved to the other `AssociationMode`: the one mutation, which
    /// may move the mode and nothing else about the entry.
    ModeMoved,
}

/// Every stage, in the order one run walks them.
const STAGES: [Stage; 3] = [
    Stage::Created,
    Stage::ReadThroughListedHandle,
    Stage::ModeMoved,
];

/// The other `AssociationMode` — the one a move takes an association in `mode` to.
fn other_mode(mode: &str) -> &'static str {
    MODES
        .into_iter()
        .find(|candidate| *candidate != mode)
        .unwrap_or(MODES[0])
}

#[tokio::test]
async fn repeated_discovery_keeps_reporting_the_identity_the_create_established() {
    let router = new_router();
    let rules = published_rule_ids(&router).await;
    assert!(
        rules.len() >= 2,
        "expected the published catalogue to carry at least two rules, got {rules:?}"
    );

    // Two rules associated in different modes, so each single filter selects a strict
    // subset and a filtered discovery is asserted on more than one record.
    let mut subjects: Vec<Subject> = Vec::new();
    for (index, rule_id) in rules.iter().take(2).enumerate() {
        let mode = MODES[index % MODES.len()];
        let case = format!("{rule_id}/{mode}: the create");
        let created = associate(&router, REGION, rule_id, mode).await;
        subjects.push(Subject {
            rule_id: rule_id.clone(),
            mode,
            id: text(&created, "associationId", &case),
            arn: text(&created, "arn", &case),
        });
    }
    assert_ne!(
        subjects[0].id, subjects[1].id,
        "expected two associations to hold two identities, got {:?}",
        subjects.iter().map(|row| row.id.clone()).collect::<Vec<_>>()
    );

    let made = subjects.len();
    for stage in STAGES {
        match stage {
            Stage::Created => {}
            Stage::ReadThroughListedHandle => {
                for subject in &subjects {
                    let case = format!("{stage:?}: {}", subject.rule_id);
                    let (entries, raw) = discover(
                        &router,
                        &format!("?maxResults={MAX_RESULTS}&ruleId={}", subject.rule_id),
                        2,
                        &case,
                    )
                    .await;
                    assert_entry_is_the_handle(&router, &entries, &raw, subject, &case).await;
                }
            }
            Stage::ModeMoved => {
                for subject in &mut subjects {
                    let moved = other_mode(subject.mode);
                    let case = format!("{stage:?}: {} to {moved}", subject.rule_id);
                    let (status, body) = call_in(
                        &router,
                        REGION,
                        "PUT",
                        &format!("{RULES_PATH}/{}/association/{}", subject.rule_id, subject.id),
                        Some(json!({"mode": moved})),
                    )
                    .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: the update must succeed, got {status} {body:?}"
                    );
                    subject.mode = moved;
                }
            }
        }

        for subject in &subjects {
            for discovery in discoveries(subject, made) {
                // The same discovery issued REPEATS times over a store nothing changed
                // in between: every repeat has to carry the same identity, and the pages
                // have to arrive as the same bytes.
                let mut answers: Vec<String> = Vec::new();
                for repeat in 0..REPEATS {
                    let case = format!(
                        "{stage:?}: {}/{}: {}, repeat {repeat}",
                        subject.rule_id, subject.mode, discovery.label
                    );
                    let (entries, raw) =
                        discover(&router, &discovery.query, discovery.pages_max, &case).await;
                    assert_entry_is_the_handle(&router, &entries, &raw, subject, &case).await;
                    answers.push(raw);
                }
                let case = format!(
                    "{stage:?}: {}/{}: {}",
                    subject.rule_id, subject.mode, discovery.label
                );
                for (repeat, raw) in answers.iter().enumerate().skip(1) {
                    assert_eq!(
                        raw, &answers[0],
                        "case {case:?}: repeat {repeat} of a report over an unchanged store must \
                         answer the bytes the first one did, got {raw:?}"
                    );
                }
            }
        }
    }

    // The whole walk above acted only through ids read off the service's own answers, so
    // the identities the creates established are still the account's handles at the end
    // of it: each one deletes its own association, and only its own.
    for subject in &subjects {
        let case = format!("the delete of {}", subject.rule_id);
        let (entries, raw) = discover(&router, &format!("?maxResults={MAX_RESULTS}"), 2, &case).await;
        let entry = entry_with_arn(&entries, &subject.arn, &case);
        assert_serialized_identity(&raw, &entry, &subject.id, &subject.arn, &case);
        let discovered = text(&entry, "associationId", &case);
        let (status, body) = call_in(
            &router,
            REGION,
            "DELETE",
            &format!(
                "{RULES_PATH}/{}/association/{discovered}",
                subject.rule_id
            ),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the discovered handle must be accepted by the delete, got {status} {body:?}"
        );
        let (entries, _) = discover(&router, &format!("?maxResults={MAX_RESULTS}"), 2, &case).await;
        assert!(
            !entries
                .iter()
                .any(|entry| entry["arn"].as_str() == Some(subject.arn.as_str())),
            "case {case:?}: a deleted association must leave the listing, got {entries:?}"
        );
    }
}
