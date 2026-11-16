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
//! to the rule that replaces it.
//!
//! The harness (`Emulator`, `string`, ...) is the one `notification_rule_lifecycle`
//! already builds: one shared engine behind the same `MultiServiceRouter` dispatch the
//! emulator serves production requests with.

use std::collections::{BTreeSet, HashSet};

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
