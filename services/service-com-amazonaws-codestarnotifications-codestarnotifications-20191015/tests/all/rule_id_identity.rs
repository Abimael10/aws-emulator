// SPDX-License-Identifier: CC0-1.0

//! The identity a notification rule is reported under, over the wire.
//!
//! ONE rule, asserted over a TABLE of inputs rather than one recorded request: the
//! identifier this service mints for a rule is a canonical, hyphenated version-4 UUID
//! (inside `NotificationRuleId`'s modelled `@length` 1..40), it is unique per rule, it is
//! byte-for-byte the resource segment of that rule's `Arn`, and every path that names the
//! rule reports that same pair — the create's `Arn`, and `NotificationRuleSummary.Id`
//! beside its `Arn` in every listing, unfiltered, filtered on any of
//! `ListNotificationRulesFilterName`'s values, and on every page of any page size. A
//! later read or update never changes it, and the id a deleted rule held is never handed
//! to the rule that replaces it. Narrowing a listing — by `Filters`, whose values are
//! ORed within one name and ANDed across names, or by page size — decides only WHICH
//! rules are named, never the identity reported for one that is.
//!
//! The last suite drives that same relation as the INVARIANT it is, over populations and
//! narrowings drawn by a seeded generator rather than written out as rows.
//!
//! The harness (`Emulator`, `string`, ...) is the one `notification_rule_lifecycle`
//! already builds: one shared engine behind the same `MultiServiceRouter` dispatch the
//! emulator serves production requests with.

use std::collections::{BTreeSet, HashSet};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};

use crate::notification_rule_lifecycle::{string, Emulator};

/// `NotificationRuleId`'s modelled bounds
/// (`smithy_query(query="shape",shape="NotificationRuleId")`: `@length min 1 max 40`).
const ID_LENGTH_BOUNDS: std::ops::RangeInclusive<usize> = 1..=40;

/// The filter names a listing may be narrowed by — `ListNotificationRulesFilterName`'s
/// own values
/// (`smithy_query(query="shape",shape="ListNotificationRulesFilterName")`:
/// `EVENT_TYPE_ID`, `CREATED_BY`, `RESOURCE`, `TARGET_ADDRESS`).
const FILTER_NAMES: [&str; 4] = ["EVENT_TYPE_ID", "CREATED_BY", "RESOURCE", "TARGET_ADDRESS"];

/// One row of the seeded account: a rule, and what a listing can select it by.
struct SeededRule {
    label: String,
    arn: String,
    resource: String,
    event_type_id: &'static str,
    target_address: String,
}

impl SeededRule {
    /// The id this rule's ARN carries — the value every listing must report as its `Id`.
    fn id(&self) -> &str {
        id_of_arn(&self.label, &self.arn)
    }
}

/// The resource segment of a notification-rule ARN, after `notificationrule/`.
///
/// `pub(crate)` so the sibling suites assert the id/ARN pair through this ONE reading of
/// an ARN rather than each splitting the value their own way.
pub(crate) fn id_of_arn<'a>(case: &str, arn: &'a str) -> &'a str {
    arn.split_once(":notificationrule/")
        .map(|(_, id)| id)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a notificationrule/ resource segment in {arn:?}")
        })
}

/// A minted rule id is a version-4 UUID in its canonical hyphenated lowercase form, and
/// fits the member's modelled length.
///
/// `pub(crate)` for the same reason as [`id_of_arn`]: one statement of the id form, read
/// by every suite that asserts it.
pub(crate) fn assert_canonical_rule_id(case: &str, id: &str) {
    assert!(
        ID_LENGTH_BOUNDS.contains(&id.len()),
        "case {case:?}: NotificationRuleId is length 1..40, got {} for {id:?}",
        id.len()
    );
    let parsed = uuid::Uuid::parse_str(id)
        .unwrap_or_else(|error| panic!("case {case:?}: {id:?} is not a UUID: {error}"));
    assert_eq!(
        parsed.as_hyphenated().to_string(),
        id,
        "case {case:?}: the id must be the canonical hyphenated lowercase rendering, got {id:?}"
    );
    assert_eq!(
        parsed.get_version_num(),
        4,
        "case {case:?}: a rule id is a version-4 UUID, got {id:?}"
    );
    assert_eq!(
        parsed.get_variant(),
        uuid::Variant::RFC4122,
        "case {case:?}: a rule id carries the RFC-4122 variant, got {id:?}"
    );
}

/// The rows the suites below seed: they differ along what the rule generalises over —
/// the resource, the event type, the detail type, the target count and type, the tag
/// count and the status — so an id is asserted over a SET of rules rather than one.
struct RuleCase {
    label: &'static str,
    resource: &'static str,
    detail_type: &'static str,
    status: Option<&'static str>,
    /// Event type ids out of the catalog `ListEventTypes` publishes
    /// (`crate::state::EVENT_TYPE_CATALOG`), one service per row.
    event_type_ids: &'static [&'static str],
    targets: &'static [(&'static str, &'static str)],
    tags: &'static [(&'static str, &'static str)],
}

const RULE_CASES: &[RuleCase] = &[
    RuleCase {
        label: "codecommit repository, one SNS target",
        resource: "arn:aws:codecommit:us-east-1:123456789012:IdRepo",
        detail_type: "BASIC",
        status: None,
        event_type_ids: &["codecommit-repository-pull-request-created"],
        targets: &[("SNS", "arn:aws:sns:us-east-1:123456789012:id-topic-a")],
        tags: &[],
    },
    RuleCase {
        label: "codebuild project, no target, FULL detail",
        resource: "arn:aws:codebuild:us-east-1:123456789012:project/IdProject",
        detail_type: "FULL",
        status: Some("ENABLED"),
        event_type_ids: &[
            "codebuild-project-build-state-failed",
            "codebuild-project-build-state-succeeded",
        ],
        targets: &[],
        tags: &[("Team", "builders")],
    },
    RuleCase {
        label: "codepipeline pipeline, two targets, DISABLED",
        resource: "arn:aws:codepipeline:us-east-1:123456789012:IdPipeline",
        detail_type: "BASIC",
        status: Some("DISABLED"),
        event_type_ids: &["codepipeline-pipeline-manual-approval-needed"],
        targets: &[
            ("SNS", "arn:aws:sns:us-east-1:123456789012:id-topic-b"),
            (
                "AWSChatbotSlack",
                "arn:aws:chatbot:us-east-1:123456789012:chat-configuration/slack-channel/id-slack",
            ),
        ],
        tags: &[("env", "test"), ("owner", "codestar")],
    },
    RuleCase {
        label: "codedeploy application, shared resource with row 1",
        resource: "arn:aws:codecommit:us-east-1:123456789012:IdRepo",
        detail_type: "FULL",
        status: None,
        event_type_ids: &["codedeploy-application-deployment-succeeded"],
        targets: &[("SNS", "arn:aws:sns:us-east-1:123456789012:id-topic-a")],
        tags: &[],
    },
];

impl RuleCase {
    fn request(&self, name: &str) -> Value {
        let mut body = json!({
            "Name": name,
            "Resource": self.resource,
            "DetailType": self.detail_type,
            "EventTypeIds": self.event_type_ids,
            "Targets": self.targets
                .iter()
                .map(|(target_type, address)| json!({
                    "TargetType": target_type,
                    "TargetAddress": address,
                }))
                .collect::<Vec<Value>>(),
        });
        if let Some(status) = self.status {
            body["Status"] = json!(status);
        }
        if !self.tags.is_empty() {
            body["Tags"] = Value::Object(
                self.tags
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), json!(value)))
                    .collect(),
            );
        }
        body
    }
}

/// Create every row of [`RULE_CASES`], prefixing the rule names with `label` so two
/// seedings of one engine cannot collide on the per-region name index.
async fn seed(emulator: &Emulator, label: &str) -> Vec<SeededRule> {
    let mut seeded = Vec::with_capacity(RULE_CASES.len());
    for (index, case) in RULE_CASES.iter().enumerate() {
        let name = format!("{label}-{index}");
        let created = emulator
            .ok(case.label, "/createNotificationRule", &case.request(&name))
            .await;
        let arn = string(&created, "Arn");
        assert!(
            !arn.is_empty(),
            "case {:?}: create must answer with an ARN, got {created}",
            case.label
        );
        seeded.push(SeededRule {
            label: case.label.to_string(),
            arn,
            resource: case.resource.to_string(),
            event_type_id: case.event_type_ids[0],
            target_address: case
                .targets
                .first()
                .map(|(_, address)| (*address).to_string())
                .unwrap_or_default(),
        });
    }
    seeded
}

/// Every `(Id, Arn)` pair of a listing, following `NextToken` to the end.
async fn list_pairs(
    emulator: &Emulator,
    case: &str,
    body: &Value,
    max_results: Option<u32>,
) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut request = body.clone();
        if let Some(max) = max_results {
            request["MaxResults"] = json!(max);
        }
        if let Some(value) = &token {
            request["NextToken"] = json!(value);
        }
        let listed = emulator.ok(case, "/listNotificationRules", &request).await;
        for summary in listed
            .get("NotificationRules")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            pairs.push((string(&summary, "Id"), string(&summary, "Arn")));
        }
        pages += 1;
        assert!(
            pages <= 64,
            "case {case:?}: pagination did not terminate after {pages} pages"
        );
        match listed.get("NextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    pairs
}

/// The id of every rule is a canonical version-4 UUID and no two rules share one,
/// whatever the rule was created with.
#[tokio::test]
async fn a_minted_rule_id_is_a_canonical_version_4_uuid_unique_per_rule() {
    let emulator = Emulator::new();
    let seeded = seed(&emulator, "id-form").await;

    let mut seen: HashSet<String> = HashSet::new();
    for rule in &seeded {
        let id = rule.id();
        assert_canonical_rule_id(&rule.label, id);
        assert!(
            seen.insert(id.to_string()),
            "case {:?}: {id:?} was already minted for another rule",
            rule.label
        );
    }
    assert_eq!(
        seen.len(),
        RULE_CASES.len(),
        "every seeded rule must carry its own id, got {seen:?}"
    );
}

/// Every listing reports, for each rule it names, the rule's own id beside the rule's own
/// ARN — the id being that ARN's resource segment — unfiltered, on any page size, and
/// under every filter name the enum lists.
#[tokio::test]
async fn every_listing_reports_the_rules_own_id_beside_its_arn() {
    let emulator = Emulator::new();
    let seeded = seed(&emulator, "id-listing").await;

    // `CreatedBy` is the caller's own principal, read back from the store rather than
    // written into the case, so the CREATED_BY row below filters on what the service
    // recorded.
    let described = emulator
        .ok(
            "created-by",
            "/describeNotificationRule",
            &json!({ "Arn": seeded[0].arn }),
        )
        .await;
    let created_by = string(&described, "CreatedBy");
    assert!(
        !created_by.is_empty(),
        "the store must record the caller as CreatedBy, got {described}"
    );

    // Each row is (case, filters, the rules that must be named), spanning the unfiltered
    // listing and one filter per `ListNotificationRulesFilterName` value.
    let all: Vec<&SeededRule> = seeded.iter().collect();
    let cases: Vec<(String, Value, Vec<&SeededRule>)> = vec![
        ("no filter".to_string(), json!([]), all.clone()),
        (
            format!("{} on the shared resource", FILTER_NAMES[2]),
            json!([{ "Name": FILTER_NAMES[2], "Value": seeded[0].resource }]),
            seeded
                .iter()
                .filter(|rule| rule.resource == seeded[0].resource)
                .collect(),
        ),
        (
            format!("{} on one row's event type", FILTER_NAMES[0]),
            json!([{ "Name": FILTER_NAMES[0], "Value": seeded[2].event_type_id }]),
            seeded
                .iter()
                .filter(|rule| rule.event_type_id == seeded[2].event_type_id)
                .collect(),
        ),
        (
            format!("{} on the shared target address", FILTER_NAMES[3]),
            json!([{ "Name": FILTER_NAMES[3], "Value": seeded[0].target_address }]),
            seeded
                .iter()
                .filter(|rule| rule.target_address == seeded[0].target_address)
                .collect(),
        ),
        (
            format!("{} on the caller's principal", FILTER_NAMES[1]),
            json!([{ "Name": FILTER_NAMES[1], "Value": created_by }]),
            all.clone(),
        ),
    ];

    // Row 1 is the modelled minimum page size, row 2 splits the collection, row 100 is
    // the modelled maximum and `None` is a request with no `MaxResults` at all.
    for max_results in [Some(1), Some(2), Some(100), None] {
        for (filter_case, filters, expected) in &cases {
            let case = format!("{filter_case} at MaxResults={max_results:?}");
            let body = if filters.as_array().is_some_and(Vec::is_empty) {
                json!({})
            } else {
                json!({ "Filters": filters })
            };
            let pairs = list_pairs(&emulator, &case, &body, max_results).await;

            let named: Vec<String> = pairs.iter().map(|(_, arn)| arn.clone()).collect();
            let wanted: Vec<String> = expected.iter().map(|rule| rule.arn.clone()).collect();
            assert_eq!(
                named, wanted,
                "case {case:?}: the listing must name exactly those rules, got {pairs:?}"
            );
            for (id, arn) in &pairs {
                assert_eq!(
                    id.as_str(),
                    id_of_arn(&case, arn),
                    "case {case:?}: the summary's Id must be its own ARN's resource segment, got {id:?} for {arn:?}"
                );
                assert_canonical_rule_id(&case, id);
            }
        }
    }
}

/// Reading or changing a rule never changes the identity it is reported under: the same
/// listing read twice, and a listing after an update, a new subscription and a tag, all
/// report the id the rule was created with.
#[tokio::test]
async fn a_rules_id_survives_reads_updates_and_subscriptions() {
    let emulator = Emulator::new();
    let seeded = seed(&emulator, "id-stable").await;
    let minted: Vec<(String, String)> = seeded
        .iter()
        .map(|rule| (rule.id().to_string(), rule.arn.clone()))
        .collect();

    // A read must not change what a later caller sees.
    let first = list_pairs(&emulator, "first read", &json!({}), None).await;
    let second = list_pairs(&emulator, "second read", &json!({}), None).await;
    assert_eq!(
        first, minted,
        "the first listing must report the minted ids, got {first:?}"
    );
    assert_eq!(
        second, first,
        "reading a listing twice must report the same ids, got {second:?}"
    );

    // Each row mutates the rule a different way; none of them may re-identify it.
    for (index, rule) in seeded.iter().enumerate() {
        let case = format!("mutating {:?}", rule.label);
        emulator
            .ok(
                &case,
                "/updateNotificationRule",
                &json!({
                    "Arn": rule.arn,
                    "Name": format!("id-stable-renamed-{index}"),
                    "Status": "DISABLED",
                    "DetailType": "FULL",
                    "EventTypeIds": ["codecommit-repository-branches-and-tags-created"],
                }),
            )
            .await;
        emulator
            .ok(
                &case,
                "/subscribe",
                &json!({
                    "Arn": rule.arn,
                    "Target": {
                        "TargetType": "SNS",
                        "TargetAddress": format!("arn:aws:sns:us-east-1:123456789012:id-stable-{index}"),
                    },
                }),
            )
            .await;
        emulator
            .ok(
                &case,
                "/tagResource",
                &json!({ "Arn": rule.arn, "Tags": { "phase": "after-update" } }),
            )
            .await;
    }

    let after = list_pairs(&emulator, "after the mutations", &json!({}), None).await;
    assert_eq!(
        after, minted,
        "case \"after the mutations\": a rule keeps the id and ARN it was created with, got {after:?}"
    );
}

/// A rule that replaces a deleted one is a different rule: it is never handed the id the
/// deleted rule held, even when it takes its name and every other parameter.
#[tokio::test]
async fn a_recreated_rule_is_minted_a_fresh_id() {
    let emulator = Emulator::new();

    let mut retired: BTreeSet<String> = BTreeSet::new();
    for case in RULE_CASES {
        // Same name and same parameters on every round: only the identity may differ.
        let name = "id-recycled";
        for round in 0..3 {
            let label = format!("{} round {round}", case.label);
            let created = emulator
                .ok(&label, "/createNotificationRule", &case.request(name))
                .await;
            let arn = string(&created, "Arn");
            let id = id_of_arn(&label, &arn).to_string();
            assert_canonical_rule_id(&label, &id);
            assert!(
                retired.insert(id.clone()),
                "case {label:?}: {id:?} was already issued to an earlier rule"
            );

            emulator
                .ok(&label, "/deleteNotificationRule", &json!({ "Arn": arn }))
                .await;
            let listed = list_pairs(&emulator, &label, &json!({}), None).await;
            assert!(
                listed.iter().all(|(listed_id, _)| listed_id != &id),
                "case {label:?}: a deleted rule's id must not be listed, got {listed:?}"
            );
        }
    }
}

// --- narrowing a listing -------------------------------------------------------------
//
// Narrowing — by `Filters` (the values given for one name ORed, the names ANDed) or by
// page size — decides WHICH rules a listing names and nothing about the identity it
// reports for a rule it names: each entry still carries that rule's own minted id,
// byte-for-byte the resource segment of that rule's own `Arn`.

/// One rule of the narrowing table: the three request-chosen attributes a listing can
/// select a rule by. `CREATED_BY`, the fourth name the enum lists, is the caller's own
/// principal, so it is the same for every row and is read back from the store below.
struct NarrowedRule {
    label: &'static str,
    resource: &'static str,
    /// Event type ids out of the catalog `ListEventTypes` publishes
    /// (`crate::state::EVENT_TYPE_CATALOG`).
    event_type_ids: &'static [&'static str],
    target_addresses: &'static [&'static str],
}

const NARROW_REPOSITORY: &str = "arn:aws:codecommit:us-east-1:123456789012:NarrowRepo";
const NARROW_PROJECT: &str = "arn:aws:codebuild:us-east-1:123456789012:project/NarrowProject";
const NARROW_PIPELINE: &str = "arn:aws:codepipeline:us-east-1:123456789012:NarrowPipeline";

const NARROW_TOPIC_X: &str = "arn:aws:sns:us-east-1:123456789012:narrow-topic-x";
const NARROW_TOPIC_Y: &str = "arn:aws:sns:us-east-1:123456789012:narrow-topic-y";
const NARROW_SLACK: &str =
    "arn:aws:chatbot:us-east-1:123456789012:chat-configuration/slack-channel/narrow";

// Four ids of the published catalog, one per service it covers.
const NARROW_PULL_REQUEST: &str = "codecommit-repository-pull-request-created";
const NARROW_BUILD_FAILED: &str = "codebuild-project-build-state-failed";
const NARROW_APPROVAL: &str = "codepipeline-pipeline-manual-approval-needed";
const NARROW_DEPLOYMENT: &str = "codedeploy-application-deployment-succeeded";

/// The rules the narrowing suite seeds: the rows overlap on each attribute a filter
/// selects by — two rules share a resource, two share an event type, three share a
/// target address, one has no target at all — so a narrowing selects a different,
/// non-trivial subset of them for every combination below.
const NARROWED_RULES: &[NarrowedRule] = &[
    NarrowedRule {
        label: "repository, one event type, one topic",
        resource: NARROW_REPOSITORY,
        event_type_ids: &[NARROW_PULL_REQUEST],
        target_addresses: &[NARROW_TOPIC_X],
    },
    NarrowedRule {
        label: "repository again, two event types, no target",
        resource: NARROW_REPOSITORY,
        event_type_ids: &[NARROW_BUILD_FAILED, NARROW_PULL_REQUEST],
        target_addresses: &[],
    },
    NarrowedRule {
        label: "build project, two topics",
        resource: NARROW_PROJECT,
        event_type_ids: &[NARROW_BUILD_FAILED],
        target_addresses: &[NARROW_TOPIC_X, NARROW_TOPIC_Y],
    },
    NarrowedRule {
        label: "build project again, approval event",
        resource: NARROW_PROJECT,
        event_type_ids: &[NARROW_APPROVAL],
        target_addresses: &[NARROW_TOPIC_Y],
    },
    NarrowedRule {
        label: "pipeline, two event types, a chat target",
        resource: NARROW_PIPELINE,
        event_type_ids: &[NARROW_APPROVAL, NARROW_DEPLOYMENT],
        target_addresses: &[NARROW_SLACK],
    },
    NarrowedRule {
        label: "pipeline again, a topic and a chat target",
        resource: NARROW_PIPELINE,
        event_type_ids: &[NARROW_DEPLOYMENT],
        target_addresses: &[NARROW_TOPIC_X, NARROW_SLACK],
    },
];

/// The `TargetType` an address is subscribed under: a chat configuration is a Chatbot
/// target, everything else here is a topic.
fn narrow_target_type(address: &str) -> &'static str {
    if address.contains(":chatbot:") {
        "AWSChatbotSlack"
    } else {
        "SNS"
    }
}

/// Create every row of [`NARROWED_RULES`], in row order, prefixing the rule names with
/// `label` so the per-region name index cannot collide with another suite's seeding.
async fn seed_narrowed(emulator: &Emulator, label: &str) -> Vec<String> {
    let mut arns = Vec::with_capacity(NARROWED_RULES.len());
    for (index, rule) in NARROWED_RULES.iter().enumerate() {
        let request = json!({
            "Name": format!("{label}-{index}"),
            "Resource": rule.resource,
            "DetailType": "BASIC",
            "EventTypeIds": rule.event_type_ids,
            "Targets": rule.target_addresses
                .iter()
                .map(|address| json!({
                    "TargetType": narrow_target_type(address),
                    "TargetAddress": address,
                }))
                .collect::<Vec<Value>>(),
        });
        let created = emulator
            .ok(rule.label, "/createNotificationRule", &request)
            .await;
        let arn = string(&created, "Arn");
        assert!(
            !arn.is_empty(),
            "case {:?}: create must answer with an ARN, got {created}",
            rule.label
        );
        arns.push(arn);
    }
    arns
}

/// The values one filter name is driven with: variant 0 is a single value, variant 1 is
/// two values for the same name — the OR `Filters` documents.
fn narrowing_values(name: &str, variant: usize, created_by: &str) -> Vec<String> {
    let values: Vec<&str> = match (name, variant) {
        ("RESOURCE", 0) => vec![NARROW_REPOSITORY],
        ("RESOURCE", _) => vec![NARROW_REPOSITORY, NARROW_PIPELINE],
        ("EVENT_TYPE_ID", 0) => vec![NARROW_PULL_REQUEST],
        ("EVENT_TYPE_ID", _) => vec![NARROW_BUILD_FAILED, NARROW_APPROVAL],
        ("TARGET_ADDRESS", 0) => vec![NARROW_TOPIC_X],
        ("TARGET_ADDRESS", _) => vec![NARROW_TOPIC_X, NARROW_TOPIC_Y],
        ("CREATED_BY", 0) => vec![created_by],
        ("CREATED_BY", _) => vec![created_by, UNMATCHED_PRINCIPAL],
        (other, _) => panic!("case {other:?}: not a name ListNotificationRulesFilterName lists"),
    };
    values.into_iter().map(str::to_string).collect()
}

const UNMATCHED_PRINCIPAL: &str = "arn:aws:iam::123456789012:user/narrow-unused-principal";

/// One value per filter name that no seeded rule carries, each inside the same value
/// space as the ones that do: a narrowing that selects nothing must name nothing.
const UNMATCHED_VALUES: [(&str, &str); 4] = [
    ("EVENT_TYPE_ID", "codebuild-project-build-phase-failure"),
    ("CREATED_BY", UNMATCHED_PRINCIPAL),
    (
        "RESOURCE",
        "arn:aws:codecommit:us-east-1:123456789012:NarrowUnusedRepo",
    ),
    (
        "TARGET_ADDRESS",
        "arn:aws:sns:us-east-1:123456789012:narrow-unused",
    ),
];

/// Whether `rule` satisfies one filter name's ORed values, as `Filters` documents the
/// four names the enum lists.
fn narrowed_rule_matches(
    rule: &NarrowedRule,
    created_by: &str,
    name: &str,
    values: &[String],
) -> bool {
    match name {
        "EVENT_TYPE_ID" => rule
            .event_type_ids
            .iter()
            .any(|id| values.iter().any(|value| value == id)),
        "CREATED_BY" => values.iter().any(|value| value == created_by),
        "RESOURCE" => values.iter().any(|value| value == rule.resource),
        "TARGET_ADDRESS" => rule
            .target_addresses
            .iter()
            .any(|address| values.iter().any(|value| value == address)),
        other => panic!("case {other:?}: not a name ListNotificationRulesFilterName lists"),
    }
}

/// One filter name and the values a narrowing ORs for it.
type NarrowingFilter = (&'static str, Vec<String>);

/// One narrowing: the case it is reported as, and the filters it ANDs together.
type NarrowingCase = (String, Vec<NarrowingFilter>);

/// Every narrowing the suite drives: each of the 2^4 combinations of the filter names
/// (the empty one being the unfiltered listing), with one name's values ORed and the
/// names ANDed, plus one row per name whose value nothing carries.
fn narrowing_cases(created_by: &str) -> Vec<NarrowingCase> {
    let mut cases: Vec<NarrowingCase> = Vec::new();
    for mask in 0..(1usize << FILTER_NAMES.len()) {
        let mut grouped: Vec<NarrowingFilter> = Vec::new();
        for (index, name) in FILTER_NAMES.iter().enumerate() {
            if mask & (1 << index) == 0 {
                continue;
            }
            // One value and two values for one name are both driven across the table.
            let variant = (mask + index) % 2;
            grouped.push((*name, narrowing_values(name, variant, created_by)));
        }
        let label = if grouped.is_empty() {
            "no filter".to_string()
        } else {
            grouped
                .iter()
                .map(|(name, values)| format!("{name}={values:?}"))
                .collect::<Vec<String>>()
                .join(" AND ")
        };
        cases.push((label, grouped));
    }
    for (name, unmatched) in UNMATCHED_VALUES {
        cases.push((
            format!("{name}={unmatched:?} matches nothing"),
            vec![(name, vec![unmatched.to_string()])],
        ));
    }
    cases
}

/// RULE: narrowing a listing decides only WHICH rules it names — `Filters` select them
/// (values ORed within a name, names ANDed) and `MaxResults` splits them over pages —
/// and never what identity is reported for a rule it names: every entry carries that
/// rule's own minted id, byte-for-byte the resource segment of that entry's own `Arn`,
/// and no narrowing reports an identity the account was never issued.
#[tokio::test]
async fn narrowing_a_listing_never_changes_the_identity_it_reports() {
    let emulator = Emulator::new();
    let arns = seed_narrowed(&emulator, "narrowed").await;

    // `CreatedBy` is the caller's own principal, recorded by the service rather than
    // sent, so the CREATED_BY rows filter on what the store holds.
    let described = emulator
        .ok(
            "created-by",
            "/describeNotificationRule",
            &json!({ "Arn": arns[0] }),
        )
        .await;
    let created_by = string(&described, "CreatedBy");
    assert!(
        !created_by.is_empty(),
        "the store must record the caller as CreatedBy, got {described}"
    );

    // The identity each rule was minted, in creation order: what every narrowing below
    // must report for the rules it names.
    let minted: Vec<(String, String)> = arns
        .iter()
        .enumerate()
        .map(|(index, arn)| {
            let case = NARROWED_RULES[index].label;
            (id_of_arn(case, arn).to_string(), arn.clone())
        })
        .collect();
    let issued: HashSet<&String> = minted.iter().map(|(id, _)| id).collect();

    for (label, grouped) in narrowing_cases(&created_by) {
        let filters: Vec<Value> = grouped
            .iter()
            .flat_map(|(name, values)| {
                values
                    .iter()
                    .map(move |value| json!({ "Name": name, "Value": value }))
            })
            .collect();
        let body = if filters.is_empty() {
            json!({})
        } else {
            json!({ "Filters": filters })
        };
        let expected: Vec<(String, String)> = NARROWED_RULES
            .iter()
            .zip(minted.iter())
            .filter(|(rule, _)| {
                grouped
                    .iter()
                    .all(|(name, values)| narrowed_rule_matches(rule, &created_by, name, values))
            })
            .map(|(_, pair)| pair.clone())
            .collect();

        // Row 1 is the modelled minimum page size, row 2 splits the selection over
        // pages, and `None` is a request with no `MaxResults` at all.
        for max_results in [Some(1), Some(2), None] {
            let case = format!("{label} at MaxResults={max_results:?}");
            let pairs = list_pairs(&emulator, &case, &body, max_results).await;
            assert_eq!(
                pairs, expected,
                "case {case:?}: a narrowing selects which rules are named and reports each \
                 one's own identity, got {pairs:?}"
            );
            for (id, arn) in &pairs {
                assert_eq!(
                    id.as_str(),
                    id_of_arn(&case, arn),
                    "case {case:?}: the summary's Id must be its own ARN's resource segment, got {id:?} for {arn:?}"
                );
                assert_canonical_rule_id(&case, id);
                assert!(
                    issued.contains(id),
                    "case {case:?}: {id:?} is an identity this account was never issued"
                );
            }
        }
    }
}

// --- generated populations ------------------------------------------------------------
//
// The identity relation above is an INVARIANT, so it holds for any account content and
// any way of narrowing a listing — not only for the rows written out by hand. One seeded
// PRNG draws what a store holds (how many rules, and each one's resource, event types,
// targets, detail type and status) and how it is listed (which filter names, how many
// values per name, what page size). Every assertion names the seed it was driven with, so
// a failing case reports the draw that produced it and is replayed by seeding the
// generator with that value.

/// The resources a generated rule fires for: one ARN per service this notification
/// service supports (`crate::state::SUPPORTED_RESOURCE_SERVICES` — codebuild, codecommit,
/// codedeploy and codepipeline).
const GENERATED_RESOURCES: [&str; 4] = [
    NARROW_REPOSITORY,
    NARROW_PROJECT,
    NARROW_PIPELINE,
    "arn:aws:codedeploy:us-east-1:123456789012:application:GeneratedApp",
];

/// The event types a generated rule is wired to: ids out of the catalog `ListEventTypes`
/// publishes (`crate::state::EVENT_TYPE_CATALOG`), one per service above.
const GENERATED_EVENT_TYPE_IDS: [&str; 4] = [
    NARROW_PULL_REQUEST,
    NARROW_BUILD_FAILED,
    NARROW_APPROVAL,
    NARROW_DEPLOYMENT,
];

/// The target addresses a generated rule may be subscribed to: two topics and one chat
/// configuration, so both `TargetType` kinds are drawn.
const GENERATED_TARGET_ADDRESSES: [&str; 3] = [NARROW_TOPIC_X, NARROW_TOPIC_Y, NARROW_SLACK];

/// How many populations the invariant is driven over, how large one may get and how many
/// narrowings each is listed under: BOUNDED, so the suite stays a few hundred in-process
/// calls.
const GENERATED_POPULATIONS: usize = 8;
const GENERATED_RULES_PER_POPULATION: std::ops::RangeInclusive<usize> = 1..=5;
const GENERATED_NARROWINGS: usize = 4;

/// The page sizes a generated listing is read at: the modelled minimum, a size that
/// splits a population over pages, the modelled maximum, and no `MaxResults` at all.
const GENERATED_PAGE_SIZES: [Option<u32>; 4] = [Some(1), Some(2), Some(100), None];

/// One generated rule: the create request it was sent as, and the attribute values a
/// narrowing can select it by.
struct GeneratedRule {
    request: Value,
    resource: &'static str,
    event_type_ids: Vec<&'static str>,
    target_addresses: Vec<&'static str>,
}

/// Draw one rule under the name the caller gives it: a resource, one or two event types
/// out of the published catalog, zero to two targets, and either detail type and status.
fn generate_rule(rng: &mut StdRng, name: &str) -> GeneratedRule {
    let resource = GENERATED_RESOURCES[rng.random_range(0..GENERATED_RESOURCES.len())];
    let mut event_type_ids: Vec<&'static str> = Vec::new();
    for _ in 0..rng.random_range(1..=2) {
        let drawn = GENERATED_EVENT_TYPE_IDS[rng.random_range(0..GENERATED_EVENT_TYPE_IDS.len())];
        if !event_type_ids.contains(&drawn) {
            event_type_ids.push(drawn);
        }
    }
    let mut target_addresses: Vec<&'static str> = Vec::new();
    for _ in 0..rng.random_range(0..=2) {
        let drawn =
            GENERATED_TARGET_ADDRESSES[rng.random_range(0..GENERATED_TARGET_ADDRESSES.len())];
        if !target_addresses.contains(&drawn) {
            target_addresses.push(drawn);
        }
    }
    let request = json!({
        "Name": name,
        "Resource": resource,
        "DetailType": if rng.random_bool(0.5) { "BASIC" } else { "FULL" },
        "Status": if rng.random_bool(0.5) { "ENABLED" } else { "DISABLED" },
        "EventTypeIds": event_type_ids,
        "Targets": target_addresses
            .iter()
            .map(|address| json!({
                "TargetType": narrow_target_type(address),
                "TargetAddress": address,
            }))
            .collect::<Vec<Value>>(),
    });
    GeneratedRule {
        request,
        resource,
        event_type_ids,
        target_addresses,
    }
}

/// A drawn narrowing: how it is reported, the `Filters` entries it sends, and the ARN of
/// the rule it was built from — the one rule it must name, because every value it ORs for
/// a filter name includes that rule's own value for that name.
struct GeneratedNarrowing {
    label: String,
    filters: Vec<Value>,
    anchor_arn: String,
}

/// Draw a narrowing of `population`: a random subset of the filter names the enum lists,
/// each carrying one anchor rule's own value and, half the time, a second value ORed
/// beside it.
fn generate_narrowing(
    rng: &mut StdRng,
    population: &[(GeneratedRule, String)],
    created_by: &str,
) -> GeneratedNarrowing {
    let (anchor, anchor_arn) = &population[rng.random_range(0..population.len())];
    let mut filters: Vec<Value> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    for name in FILTER_NAMES {
        if !rng.random_bool(0.5) {
            continue;
        }
        let mut values: Vec<String> = match name {
            "RESOURCE" => vec![anchor.resource.to_string()],
            "EVENT_TYPE_ID" => vec![anchor.event_type_ids
                [rng.random_range(0..anchor.event_type_ids.len())]
            .to_string()],
            "CREATED_BY" => vec![created_by.to_string()],
            // A rule with no target carries no address a listing can select it by.
            "TARGET_ADDRESS" if !anchor.target_addresses.is_empty() => {
                vec![anchor.target_addresses
                    [rng.random_range(0..anchor.target_addresses.len())]
                .to_string()]
            }
            _ => continue,
        };
        if rng.random_bool(0.5) {
            // A second value for the same name: the OR `Filters` documents within one
            // name, which can only widen what the narrowing selects.
            let extra = match name {
                "RESOURCE" => GENERATED_RESOURCES[rng.random_range(0..GENERATED_RESOURCES.len())],
                "EVENT_TYPE_ID" => {
                    GENERATED_EVENT_TYPE_IDS[rng.random_range(0..GENERATED_EVENT_TYPE_IDS.len())]
                }
                "CREATED_BY" => UNMATCHED_PRINCIPAL,
                _ => GENERATED_TARGET_ADDRESSES
                    [rng.random_range(0..GENERATED_TARGET_ADDRESSES.len())],
            };
            if !values.iter().any(|value| value == extra) {
                values.push(extra.to_string());
            }
        }
        parts.push(format!("{name}={values:?}"));
        filters.extend(
            values
                .into_iter()
                .map(|value| json!({ "Name": name, "Value": value })),
        );
    }
    GeneratedNarrowing {
        label: if parts.is_empty() {
            "no filter".to_string()
        } else {
            parts.join(" AND ")
        },
        filters,
        anchor_arn: anchor_arn.clone(),
    }
}

/// RULE (as an invariant, over generated input): the identity a listing reports for a
/// rule is that rule's OWN drawn identifier — a canonical version-4 UUID, byte-for-byte
/// the resource segment of that entry's `Arn`, issued to one rule of one store and to no
/// other — whatever the store holds and however the listing is narrowed, and reading the
/// same listing again reports it unchanged.
#[tokio::test]
async fn a_generated_population_reports_each_rules_own_identity_under_any_narrowing() {
    // Drawn per run and carried into every assertion message below, so a failure names
    // the draw that produced it and the run is reproducible by pinning this value.
    let seed: u64 = rand::rng().random();
    let mut rng = StdRng::seed_from_u64(seed);
    // Across every independently generated store: an identifier is drawn per rule, so no
    // two rules anywhere are issued the same one.
    let mut issued_anywhere: HashSet<String> = HashSet::new();

    for population_index in 0..GENERATED_POPULATIONS {
        // A fresh store per population, so two stores answering generated creates have
        // to mint no identifier in common either.
        let emulator = Emulator::new();
        let size = rng.random_range(GENERATED_RULES_PER_POPULATION);
        let mut population: Vec<(GeneratedRule, String)> = Vec::with_capacity(size);
        for rule_index in 0..size {
            let case = format!("seed {seed}, population {population_index}, rule {rule_index}");
            let generated = generate_rule(
                &mut rng,
                &format!("generated-{population_index}-{rule_index}"),
            );
            let created = emulator
                .ok(&case, "/createNotificationRule", &generated.request)
                .await;
            let arn = string(&created, "Arn");
            let id = id_of_arn(&case, &arn).to_string();
            assert_canonical_rule_id(&case, &id);
            assert!(
                issued_anywhere.insert(id.clone()),
                "case {case:?}: {id:?} was already issued to another rule, so the identity is \
                 not drawn per rule"
            );
            population.push((generated, arn));
        }

        let case = format!("seed {seed}, population {population_index}");
        // `CreatedBy` is the caller's own principal, recorded by the service rather than
        // sent, so the CREATED_BY narrowings filter on what the store holds.
        let described = emulator
            .ok(
                &case,
                "/describeNotificationRule",
                &json!({ "Arn": population[0].1 }),
            )
            .await;
        let created_by = string(&described, "CreatedBy");
        assert!(
            !created_by.is_empty(),
            "case {case:?}: the store must record the caller as CreatedBy, got {described}"
        );

        let minted: Vec<(String, String)> = population
            .iter()
            .map(|(_, arn)| (id_of_arn(&case, arn).to_string(), arn.clone()))
            .collect();
        let issued_here: HashSet<String> = minted.iter().map(|(id, _)| id.clone()).collect();

        let listed = list_pairs(&emulator, &case, &json!({}), None).await;
        assert_eq!(
            listed, minted,
            "case {case:?}: an unfiltered listing must name the whole population under the \
             identities it was minted, got {listed:?}"
        );

        for narrowing_index in 0..GENERATED_NARROWINGS {
            let narrowing = generate_narrowing(&mut rng, &population, &created_by);
            let max_results = GENERATED_PAGE_SIZES[rng.random_range(0..GENERATED_PAGE_SIZES.len())];
            let case = format!(
                "seed {seed}, population {population_index}, narrowing {narrowing_index} \
                 ({}) at MaxResults={max_results:?}",
                narrowing.label
            );
            let body = if narrowing.filters.is_empty() {
                json!({})
            } else {
                json!({ "Filters": narrowing.filters })
            };

            let pairs = list_pairs(&emulator, &case, &body, max_results).await;
            for (id, arn) in &pairs {
                assert_eq!(
                    id.as_str(),
                    id_of_arn(&case, arn),
                    "case {case:?}: the summary's Id must be its own ARN's resource segment, \
                     got {id:?} for {arn:?}"
                );
                assert_canonical_rule_id(&case, id);
                assert!(
                    issued_here.contains(id),
                    "case {case:?}: {id:?} is an identity this store never issued"
                );
            }
            let distinct: HashSet<&String> = pairs.iter().map(|(id, _)| id).collect();
            assert_eq!(
                distinct.len(),
                pairs.len(),
                "case {case:?}: no rule may be named twice in one listing, got {pairs:?}"
            );
            assert!(
                pairs
                    .iter()
                    .any(|(_, arn)| arn == &narrowing.anchor_arn),
                "case {case:?}: the rule every one of these filters carries a value of must be \
                 named, got {pairs:?}"
            );
            // A listing is a read: the next caller is told the same identities.
            let again = list_pairs(&emulator, &case, &body, max_results).await;
            assert_eq!(
                again, pairs,
                "case {case:?}: reading the listing again must report the same identities, \
                 got {again:?}"
            );
        }
    }
}
