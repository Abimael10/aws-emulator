// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the resource tagging surface (`TagResource`,
//! `ListTagsForResource`) over every Cost Explorer resource type that carries tags.
//!
//! Every test drives the service over its real wire protocol through the shared
//! [`crate::harness`], so nothing here reaches past the HTTP surface a client sees.

use serde_json::{json, Value};
use service_com_amazonaws_costexplorer_awsinsightsindexservice::state::{
    MAX_USER_TAGS, RESERVED_TAG_KEY_PREFIX,
};

use crate::harness::{error_code, error_message, Emulator};

/// The only value `CostCategoryRuleVersion` admits (model enum
/// `CostCategoryRuleVersion`).
const RULE_VERSION: &str = "CostCategoryExpression.v1";

/// An email subscriber address, in the form `SubscriberAddress`'s modelled pattern admits
/// for the email channel.
const EMAIL_ADDRESS: &str = "cost.ops@example.com";

/// A tag list as a sorted `(key, value)` table, so an assertion does not depend on the
/// JSON member order of one response.
type Tags = Vec<(String, String)>;

/// `(key, value)` pairs as the modelled `ResourceTagList` carries them.
fn tag_list(tags: &[(&str, &str)]) -> Value {
    Value::Array(
        tags.iter()
            .map(|(key, value)| json!({"Key": key, "Value": value}))
            .collect(),
    )
}

/// One row of a tag-merge table: the case name, the tags the request writes, and the tag
/// set the resource must report once the request has been served.
type MergeCase<'a> = (&'a str, Vec<(&'a str, &'a str)>, Vec<(&'a str, &'a str)>);

/// One row of a refusal table: the case name, the tags the request writes, and the ARN it
/// is made against.
type RefusalCase<'a> = (&'a str, Vec<(&'a str, &'a str)>, &'a str);

/// One row of a tag-removal table: the case name, the keys the request names, and the tag
/// set the resource must report once the request has been served.
type RemovalCase<'a> = (&'a str, Vec<&'a str>, Vec<(&'a str, &'a str)>);

/// The tags a `ListTagsForResource` response reports, sorted by key.
fn reported_tags(listed: &Value) -> Tags {
    let mut tags: Tags = listed["ResourceTags"]
        .as_array()
        .unwrap_or_else(|| panic!("ResourceTags must be present, got {listed}"))
        .iter()
        .map(|tag| {
            (
                tag["Key"].as_str().unwrap_or_default().to_string(),
                tag["Value"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    tags.sort();
    tags
}

/// The same table form for an expectation written in the test.
fn expected_tags(tags: &[(&str, &str)]) -> Tags {
    let mut tags: Tags = tags
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    tags.sort();
    tags
}

/// The tags the tagging surface reports for `arn`.
async fn list_tags(emulator: &Emulator, arn: &str) -> Tags {
    let listed = emulator
        .ok("ListTagsForResource", &json!({"ResourceArn": arn}))
        .await;
    reported_tags(&listed)
}

/// Create one customer managed cost anomaly monitor carrying `tags`, and answer with its
/// ARN.
async fn create_monitor(emulator: &Emulator, name: &str, tags: Value) -> String {
    let created = emulator
        .ok(
            "CreateAnomalyMonitor",
            &json!({
                "AnomalyMonitor": {
                    "MonitorName": name,
                    "MonitorType": "CUSTOM",
                    "MonitorSpecification": {"Tags": {"Key": "Team", "Values": ["platform"]}},
                },
                "ResourceTags": tags,
            }),
        )
        .await;
    created["MonitorArn"]
        .as_str()
        .unwrap_or_else(|| panic!("MonitorArn must be present, got {created}"))
        .to_string()
}

/// Create one cost anomaly subscription alerting on `monitor_arn` and carrying `tags`, and
/// answer with its ARN.
async fn create_subscription(
    emulator: &Emulator,
    name: &str,
    monitor_arn: &str,
    tags: Value,
) -> String {
    let created = emulator
        .ok(
            "CreateAnomalySubscription",
            &json!({
                "AnomalySubscription": {
                    "SubscriptionName": name,
                    "MonitorArnList": [monitor_arn],
                    "Subscribers": [{"Address": EMAIL_ADDRESS, "Type": "EMAIL"}],
                    "Frequency": "DAILY",
                    "Threshold": 100.0,
                },
                "ResourceTags": tags,
            }),
        )
        .await;
    created["SubscriptionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("SubscriptionArn must be present, got {created}"))
        .to_string()
}

/// Create one cost category definition carrying `tags`, and answer with its ARN.
async fn create_cost_category(emulator: &Emulator, name: &str, tags: Value) -> String {
    let created = emulator
        .ok(
            "CreateCostCategoryDefinition",
            &json!({
                "Name": name,
                "RuleVersion": RULE_VERSION,
                "Rules": [{
                    "Value": "Engineering",
                    "Type": "REGULAR",
                    "Rule": {"Dimensions": {
                        "Key": "LINKED_ACCOUNT",
                        "Values": [engine::emulator_account_id()],
                        "MatchOptions": ["EQUALS"],
                    }},
                }],
                "ResourceTags": tags,
            }),
        )
        .await;
    created["CostCategoryArn"]
        .as_str()
        .unwrap_or_else(|| panic!("CostCategoryArn must be present, got {created}"))
        .to_string()
}

/// One live resource of each type `ResourceTag` documents tagging support for
/// (`smithy_query(query="shape", shape="ResourceTag")`: "Tagging is supported only for the
/// following Cost Explorer resource types: AnomalyMonitor, AnomalySubscription,
/// CostCategory"), each seeded at create time with one tag.
async fn taggable_resources(emulator: &Emulator, suffix: &str) -> Vec<(&'static str, String)> {
    let monitor = create_monitor(
        emulator,
        &format!("monitor-{suffix}"),
        tag_list(&[("seeded", "at-create")]),
    )
    .await;
    let subscription = create_subscription(
        emulator,
        &format!("subscription-{suffix}"),
        &monitor,
        tag_list(&[("seeded", "at-create")]),
    )
    .await;
    let cost_category = create_cost_category(
        emulator,
        &format!("costCategory{suffix}"),
        tag_list(&[("seeded", "at-create")]),
    )
    .await;
    vec![
        ("an anomaly monitor", monitor),
        ("an anomaly subscription", subscription),
        ("a cost category", cost_category),
    ]
}

/// RULE: a resource carries ONE tag set. `TagResource` merges the keys a request names
/// into it — a new key is appended, a key already held takes the new value, a key the
/// request does not name is untouched — and `ListTagsForResource` reports exactly that
/// set. The set belongs to the resource the ARN names: tagging one resource changes no
/// other resource's tags.
#[tokio::test]
async fn tag_resource_merges_by_key_and_scopes_the_tag_set_to_one_resource() {
    let emulator = Emulator::start();
    let resources = taggable_resources(&emulator, "merge").await;

    // The merge table: each row writes a different combination over the same seeded set,
    // so the rows differ along what the rule generalises over — a new key, a key already
    // held, several at once, and a request naming nothing at all.
    let cases: Vec<MergeCase> = vec![
        (
            "a new key is appended",
            vec![("owner", "platform")],
            vec![("seeded", "at-create"), ("owner", "platform")],
        ),
        (
            "a held key takes the new value",
            vec![("seeded", "rewritten")],
            vec![("seeded", "rewritten"), ("owner", "platform")],
        ),
        (
            "several keys at once, one new and one held",
            vec![("owner", "data"), ("stage", "prod")],
            vec![
                ("seeded", "rewritten"),
                ("owner", "data"),
                ("stage", "prod"),
            ],
        ),
        (
            "an empty value is a value",
            vec![("stage", "")],
            vec![("seeded", "rewritten"), ("owner", "data"), ("stage", "")],
        ),
        (
            "a request naming no tag leaves the set alone",
            vec![],
            vec![("seeded", "rewritten"), ("owner", "data"), ("stage", "")],
        ),
    ];

    for (kind, arn) in &resources {
        assert_eq!(
            list_tags(&emulator, arn).await,
            expected_tags(&[("seeded", "at-create")]),
            "case {kind:?}: the create's own tags are the resource's initial tag set"
        );
    }

    for (label, written, expected) in &cases {
        for (kind, arn) in &resources {
            let case = format!("{kind}, {label}");
            let tagged = emulator
                .ok(
                    "TagResource",
                    &json!({"ResourceArn": arn, "ResourceTags": tag_list(written)}),
                )
                .await;
            assert!(
                tagged.is_null() || tagged.as_object().is_some_and(|body| body.is_empty()),
                "case {case:?}: TagResource answers an empty body, got {tagged}"
            );
            let actual = list_tags(&emulator, arn).await;
            assert_eq!(
                actual,
                expected_tags(expected),
                "case {case:?}: expected {:?}, got {actual:?}",
                expected_tags(expected)
            );
        }
    }

    // Scoping: one further write per resource, each with its own value under the same key,
    // and every resource must still report only its own.
    for (index, (kind, arn)) in resources.iter().enumerate() {
        let case = format!("{kind}, scoped write");
        let value = format!("only-{index}");
        emulator
            .ok(
                "TagResource",
                &json!({
                    "ResourceArn": arn,
                    "ResourceTags": tag_list(&[("scoped", value.as_str())]),
                }),
            )
            .await;
        let actual = list_tags(&emulator, arn).await;
        assert!(
            actual.contains(&("scoped".to_string(), value.clone())),
            "case {case:?}: expected the scoped tag {value:?}, got {actual:?}"
        );
    }
    for (index, (kind, arn)) in resources.iter().enumerate() {
        let case = format!("{kind}, scoped read");
        let actual = list_tags(&emulator, arn).await;
        let scoped: Vec<&(String, String)> = actual
            .iter()
            .filter(|(key, _)| key == "scoped")
            .collect();
        assert_eq!(
            scoped,
            vec![&("scoped".to_string(), format!("only-{index}"))],
            "case {case:?}: a resource reports only the value written to it, got {actual:?}"
        );
    }
}

/// RULE: keys and values are trimmed of leading and trailing whitespace, and a key holds
/// one value per resource — so a key repeated inside one request keeps the last value it
/// was given there, and a key that is only whitespace is refused.
#[tokio::test]
async fn tag_keys_and_values_are_trimmed_and_hold_one_value_per_resource() {
    let emulator = Emulator::start();
    let arn = create_monitor(&emulator, "monitor-trim", tag_list(&[])).await;

    let cases: Vec<MergeCase> = vec![
        (
            "leading and trailing whitespace is trimmed off the key and the value",
            vec![("  owner  ", " platform  ")],
            vec![("owner", "platform")],
        ),
        (
            "a key repeated in one request keeps the last value given",
            vec![("stage", "dev"), ("stage", "prod")],
            vec![("owner", "platform"), ("stage", "prod")],
        ),
        (
            "a key that trims to one already held is the same key",
            vec![(" stage ", "staging")],
            vec![("owner", "platform"), ("stage", "staging")],
        ),
        (
            "a value that is only whitespace trims to the empty value",
            vec![("stage", "   ")],
            vec![("owner", "platform"), ("stage", "")],
        ),
    ];

    for (label, written, expected) in &cases {
        emulator
            .ok(
                "TagResource",
                &json!({"ResourceArn": arn, "ResourceTags": tag_list(written)}),
            )
            .await;
        let actual = list_tags(&emulator, &arn).await;
        assert_eq!(
            actual,
            expected_tags(expected),
            "case {label:?}: expected {:?}, got {actual:?}",
            expected_tags(expected)
        );
    }

    // A key with nothing but whitespace trims shorter than the one character
    // `ResourceTagKey`'s `@length` requires, so it is refused and nothing is written.
    let before = list_tags(&emulator, &arn).await;
    let (status, body) = emulator
        .call(
            "TagResource",
            &json!({"ResourceArn": arn, "ResourceTags": tag_list(&[("   ", "x")])}),
        )
        .await;
    assert_eq!(
        status, 400,
        "case \"a whitespace-only key\": expected 400, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "case \"a whitespace-only key\": got {body}"
    );
    assert_eq!(
        list_tags(&emulator, &arn).await,
        before,
        "case \"a whitespace-only key\": a refused call writes nothing"
    );
}

/// RULE: a tag key in the `aws:` namespace is reserved for Amazon Web Services' own tags,
/// so a caller-supplied one is refused and nothing is written — on the tagging operation
/// and on every create that seeds a resource's tags alike.
#[tokio::test]
async fn a_tag_key_in_the_reserved_namespace_is_refused() {
    let emulator = Emulator::start();
    let arn = create_monitor(&emulator, "monitor-reserved", tag_list(&[("env", "test")])).await;

    // Rows differ in what follows the reserved prefix, including the prefix alone, so the
    // rule is the namespace rather than any one key the issue cited.
    let keys = [
        format!("{RESERVED_TAG_KEY_PREFIX}cloudformation:stack-name"),
        format!("{RESERVED_TAG_KEY_PREFIX}reserved"),
        RESERVED_TAG_KEY_PREFIX.to_string(),
    ];
    for key in &keys {
        let (status, body) = emulator
            .call(
                "TagResource",
                &json!({
                    "ResourceArn": arn,
                    "ResourceTags": tag_list(&[(key.as_str(), "x")]),
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {key:?}: expected 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {key:?}: got {body}"
        );
        assert_eq!(
            list_tags(&emulator, &arn).await,
            expected_tags(&[("env", "test")]),
            "case {key:?}: a refused call leaves the resource's tags as they were"
        );

        // The same key on a create that seeds a resource's tags is the same refusal: the
        // create writes the same per-resource tag set.
        let (status, body) = emulator
            .call(
                "CreateAnomalyMonitor",
                &json!({
                    "AnomalyMonitor": {
                        "MonitorName": "monitor-reserved-create",
                        "MonitorType": "CUSTOM",
                        "MonitorSpecification": {"Tags": {"Key": "Team", "Values": ["platform"]}},
                    },
                    "ResourceTags": tag_list(&[(key.as_str(), "x")]),
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {key:?} on a create: expected 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {key:?} on a create: got {body}"
        );
    }

    // The reserved namespace is the prefix as documented, and tag keys are case sensitive
    // ("Keys and values are case sensitive"), so a key differing from it only in case is an
    // ordinary user tag and is stored.
    let cased = RESERVED_TAG_KEY_PREFIX.to_uppercase();
    emulator
        .ok(
            "TagResource",
            &json!({
                "ResourceArn": arn,
                "ResourceTags": tag_list(&[(format!("{cased}own").as_str(), "mine")]),
            }),
        )
        .await;
    let listed = list_tags(&emulator, &arn).await;
    assert!(
        listed.contains(&(format!("{cased}own"), "mine".to_string())),
        "case {cased:?}: a key differing from the reserved prefix in case is a user tag, got {listed:?}"
    );
}

/// RULE: a resource holds at most 50 user tags, so a merge that would take it past that is
/// refused with `TooManyTagsException` and writes nothing — while a request that only
/// rewrites keys the resource already holds is admitted at the limit, because the merge
/// does not grow the set.
#[tokio::test]
async fn the_user_tag_maximum_per_resource_is_enforced() {
    let emulator = Emulator::start();
    let arn = create_monitor(&emulator, "monitor-limit", tag_list(&[])).await;

    let keys: Vec<String> = (0..MAX_USER_TAGS).map(|index| format!("k{index}")).collect();
    let at_limit: Vec<(&str, &str)> = keys.iter().map(|key| (key.as_str(), "v")).collect();
    emulator
        .ok(
            "TagResource",
            &json!({"ResourceArn": arn, "ResourceTags": tag_list(&at_limit)}),
        )
        .await;
    let listed = list_tags(&emulator, &arn).await;
    assert_eq!(
        listed.len(),
        MAX_USER_TAGS,
        "a request of exactly the maximum is admitted in full, got {listed:?}"
    );

    // Rewriting keys already held stays at the limit, so it is still admitted.
    emulator
        .ok(
            "TagResource",
            &json!({
                "ResourceArn": arn,
                "ResourceTags": tag_list(&[(keys[0].as_str(), "rewritten")]),
            }),
        )
        .await;
    let listed = list_tags(&emulator, &arn).await;
    assert!(
        listed.contains(&(keys[0].clone(), "rewritten".to_string())),
        "a rewrite at the limit must land, got {listed:?}"
    );
    assert_eq!(
        listed.len(),
        MAX_USER_TAGS,
        "a rewrite does not grow the set, got {listed:?}"
    );

    // One key past the limit, from the merge and from a single request alike.
    let past: Vec<String> = (0..=MAX_USER_TAGS).map(|index| format!("p{index}")).collect();
    let one_more = vec![("one-too-many", "v")];
    let all_at_once: Vec<(&str, &str)> = past.iter().map(|key| (key.as_str(), "v")).collect();
    let cases: Vec<RefusalCase> = vec![
        (
            "one new key past the limit, merged into a full set",
            one_more,
            arn.as_str(),
        ),
        (
            "one request naming one key more than the limit",
            all_at_once.clone(),
            arn.as_str(),
        ),
    ];
    for (label, written, target) in &cases {
        let before = list_tags(&emulator, target).await;
        let (status, body) = emulator
            .call(
                "TagResource",
                &json!({"ResourceArn": target, "ResourceTags": tag_list(written)}),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "TooManyTagsException",
            "case {label:?}: got {body}"
        );
        assert_eq!(
            body["ResourceName"].as_str(),
            Some(*target),
            "case {label:?}: the refusal names the resource it was made against, got {body}"
        );
        assert_eq!(
            list_tags(&emulator, target).await,
            before,
            "case {label:?}: a refused call writes nothing"
        );
    }

    // The same maximum on the create side, where `ResourceTagList`'s own `@length` admits
    // 200 members: a create seeds the same per-resource tag set, so it is admitted at the
    // maximum and refused one past it.
    let seeded: Vec<(&str, &str)> = keys.iter().map(|key| (key.as_str(), "v")).collect();
    let at_create = create_monitor(&emulator, "monitor-created-full", tag_list(&seeded)).await;
    assert_eq!(
        list_tags(&emulator, &at_create).await.len(),
        MAX_USER_TAGS,
        "a create naming exactly the maximum seeds every tag"
    );
    let (status, body) = emulator
        .call(
            "CreateAnomalyMonitor",
            &json!({
                "AnomalyMonitor": {
                    "MonitorName": "monitor-created-over",
                    "MonitorType": "CUSTOM",
                    "MonitorSpecification": {"Tags": {"Key": "Team", "Values": ["platform"]}},
                },
                "ResourceTags": tag_list(&all_at_once),
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "case \"a create naming one key more than the maximum\": expected 400, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "case \"a create naming one key more than the maximum\": got {body}"
    );
}

/// RULE: an ARN this account holds no resource under is refused with
/// `ResourceNotFoundException` at the status this service serves that shape at (HTTP 400,
/// not the 404 its `@httpError` declares) and the service's own fixed sentence, by every
/// tagging operation — whether the ARN names a resource type that cannot be tagged, a
/// resource that was never created, or one that has been deleted. The ARN itself is
/// reported in the modelled `ResourceName` member, never spliced into `Message`.
#[tokio::test]
async fn an_arn_naming_no_resource_is_refused_by_both_tagging_operations() {
    let emulator = Emulator::start();
    let account = engine::emulator_account_id();

    // A live monitor and a live cost category, so the deleted rows below are the same ARNs
    // that resolved a moment earlier. A deleted anomaly SUBSCRIPTION is not a row here
    // because `DeleteAnomalySubscription` is still a scaffold, so nothing can remove one;
    // the never-created subscription row below covers that registry.
    let monitor = create_monitor(&emulator, "monitor-gone", tag_list(&[("env", "test")])).await;
    let cost_category =
        create_cost_category(&emulator, "costCategoryGone", tag_list(&[("env", "test")])).await;
    emulator
        .ok("DeleteAnomalyMonitor", &json!({"MonitorArn": monitor}))
        .await;
    emulator
        .ok(
            "DeleteCostCategoryDefinition",
            &json!({"CostCategoryArn": cost_category}),
        )
        .await;

    // Rows differ along WHY the ARN resolves to nothing: never minted (one per taggable
    // resource type), a resource type that carries no tags, another service's resource,
    // and a resource this account held until it was deleted.
    let never_minted = |resource: &str| {
        format!("arn:aws:ce::{account}:{resource}/00000000-0000-8000-8000-000000000000")
    };
    let cases: Vec<(&str, String)> = vec![
        ("an anomaly monitor that was never created", never_minted("anomalymonitor")),
        (
            "an anomaly subscription that was never created",
            never_minted("anomalysubscription"),
        ),
        ("a cost category that was never created", never_minted("costcategory")),
        (
            "a Cost Explorer resource type that carries no tags",
            never_minted("costallocationtag"),
        ),
        (
            "another service's resource",
            format!("arn:aws:sns:us-east-1:{account}:cost-anomaly-alerts"),
        ),
        ("a deleted anomaly monitor", monitor.clone()),
        ("a deleted cost category", cost_category.clone()),
    ];

    for (label, arn) in &cases {
        for operation in ["TagResource", "UntagResource", "ListTagsForResource"] {
            let case = format!("{label}, {operation}");
            let body = match operation {
                "TagResource" => json!({"ResourceArn": arn, "ResourceTags": tag_list(&[("env", "test")])}),
                "UntagResource" => json!({"ResourceArn": arn, "ResourceTagKeys": ["env"]}),
                _ => json!({"ResourceArn": arn}),
            };
            let (status, answered) = emulator.call(operation, &body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected 400, got {status}: {answered}"
            );
            assert_eq!(
                error_code(&answered),
                "ResourceNotFoundException",
                "case {case:?}: got {answered}"
            );
            assert_eq!(
                error_message(&answered),
                "Resource not found",
                "case {case:?}: got {answered}"
            );
            assert_eq!(
                answered["ResourceName"].as_str(),
                Some(arn.as_str()),
                "case {case:?}: the refusal names the ARN the request carried, got {answered}"
            );
        }
    }
}

/// RULE: the refusal a tagging operation answers an unresolvable `ResourceArn` with is the
/// SAME answer whatever the ARN was: HTTP 400 — the status this service serves
/// `ResourceNotFoundException` at, not the 404 the shape's `@httpError` declares —
/// `ResourceNotFoundException`, and the service's own wholly fixed sentence, with nothing
/// of the request interpolated into it. The ARN the caller sent is reported in the modelled
/// `ResourceNotFoundException$ResourceName` member instead.
///
/// The table spans what a well-formed ARN can differ along — account, partition, service
/// namespace, region, resource type, and the length of the whole ARN up to the published
/// bound (`smithy_query(query="operation", operation="ListTagsForResource")`: `ResourceArn`
/// targets `Arn`, `@length` 20..2048, pattern
/// `^arn:aws[-a-z0-9]*:[a-z0-9]+:[-a-z0-9]*:[0-9]{12}:[-a-zA-Z0-9/:_]+$`) — so an answer
/// that varied with any of them would show up as a row that differs from the rest.
#[tokio::test]
async fn an_unresolvable_arn_is_refused_with_one_fixed_answer_whatever_the_arn_was() {
    let emulator = Emulator::start();
    let account = engine::emulator_account_id();

    // The longest ARN the member admits: the published upper bound of `Arn`'s `@length`,
    // padded in the resource id so the value still matches the modelled pattern.
    let long_prefix = format!("arn:aws:ce::{account}:anomalymonitor/");
    let long_arn = format!("{long_prefix}{}", "a".repeat(2048 - long_prefix.len()));

    let cases: Vec<(&str, String)> = vec![
        (
            "the shortest resource id the pattern admits",
            format!("arn:aws:ce::{account}:a"),
        ),
        ("an ARN of the published maximum length", long_arn),
        (
            "an ARN in another partition",
            format!("arn:aws-us-gov:ce::{account}:anomalymonitor/gov-monitor"),
        ),
        (
            "an ARN carrying a region",
            format!("arn:aws:ce:eu-west-1:{account}:anomalysubscription/regional"),
        ),
        (
            "an ARN of another account",
            "arn:aws:ce::999999999999:costcategory/other-account".to_string(),
        ),
        (
            "an ARN of another service",
            format!("arn:aws:sns:us-east-1:{account}:cost-anomaly-alerts"),
        ),
        (
            "an ARN whose resource id reads like an id the service mints",
            format!("arn:aws:ce::{account}:anomalymonitor/00000000-0000-8000-8000-000000000000"),
        ),
    ];

    for (label, arn) in &cases {
        for operation in ["TagResource", "UntagResource", "ListTagsForResource"] {
            let case = format!("{label}, {operation}");
            let body = match operation {
                "TagResource" => {
                    json!({"ResourceArn": arn, "ResourceTags": tag_list(&[("env", "test")])})
                }
                "UntagResource" => json!({"ResourceArn": arn, "ResourceTagKeys": ["env"]}),
                _ => json!({"ResourceArn": arn}),
            };
            let (status, answered) = emulator.call(operation, &body).await;
            let message = error_message(&answered);
            assert_eq!(
                (status, error_code(&answered).as_str(), message.as_str()),
                (400, "ResourceNotFoundException", "Resource not found"),
                "case {case:?}: expected one 400 ResourceNotFoundException with the service's \
                 fixed sentence, got {status}: {answered}"
            );
            // The one sentence every row above answers with is what proves it is fixed
            // text: the ARN it would otherwise carry is reported here instead.
            assert_eq!(
                answered["ResourceName"].as_str(),
                Some(arn.as_str()),
                "case {case:?}: the ARN belongs in ResourceName, got {answered}"
            );
        }
    }
}

/// RULE: a resource that exists and carries no tag answers with the tag list present and
/// empty — the member is reported, not omitted, because the service knows this resource's
/// tags and there are none.
#[tokio::test]
async fn a_resource_with_no_tags_reports_an_empty_list() {
    let emulator = Emulator::start();
    let monitor = create_monitor(&emulator, "monitor-untagged", tag_list(&[])).await;
    let subscription =
        create_subscription(&emulator, "subscription-untagged", &monitor, tag_list(&[])).await;
    let cost_category =
        create_cost_category(&emulator, "costCategoryUntagged", tag_list(&[])).await;

    for (kind, arn) in [
        ("an anomaly monitor", monitor),
        ("an anomaly subscription", subscription),
        ("a cost category", cost_category),
    ] {
        let listed = emulator
            .ok("ListTagsForResource", &json!({"ResourceArn": arn}))
            .await;
        assert_eq!(
            listed["ResourceTags"].as_array().map(Vec::len),
            Some(0),
            "case {kind:?}: expected ResourceTags present and empty, got {listed}"
        );
    }
}

/// RULE: `UntagResource` drops exactly the keys the request names from the resource's tag
/// set, keeps every other key it holds, and ignores a key the resource does not hold.
#[tokio::test]
async fn untag_resource_drops_only_the_keys_it_names() {
    let emulator = Emulator::start();
    let resources = taggable_resources(&emulator, "untag").await;

    // The removal table: rows differ along which keys a request names relative to the set
    // the resource holds — one of several, several at once, one it never held, none at
    // all, and a key written with surrounding whitespace.
    let cases: Vec<RemovalCase> = vec![
        (
            "one key of several",
            vec!["owner"],
            vec![("seeded", "at-create"), ("stage", "prod"), ("team", "data")],
        ),
        (
            "several keys at once",
            vec!["stage", "team"],
            vec![("seeded", "at-create")],
        ),
        (
            "a key the resource never held",
            vec!["never-set"],
            vec![("seeded", "at-create")],
        ),
        ("no key at all", vec![], vec![("seeded", "at-create")]),
        (
            "a key named with surrounding whitespace",
            vec![" seeded "],
            vec![],
        ),
    ];

    for (kind, arn) in &resources {
        emulator
            .ok(
                "TagResource",
                &json!({
                    "ResourceArn": arn,
                    "ResourceTags": tag_list(&[
                        ("owner", "platform"),
                        ("stage", "prod"),
                        ("team", "data"),
                    ]),
                }),
            )
            .await;
        assert_eq!(
            list_tags(&emulator, arn).await,
            expected_tags(&[
                ("seeded", "at-create"),
                ("owner", "platform"),
                ("stage", "prod"),
                ("team", "data"),
            ]),
            "case {kind:?}: the set every removal below starts from"
        );
    }

    for (label, removed, expected) in &cases {
        for (kind, arn) in &resources {
            let case = format!("{kind}, {label}");
            let answered = emulator
                .ok(
                    "UntagResource",
                    &json!({"ResourceArn": arn, "ResourceTagKeys": removed}),
                )
                .await;
            assert!(
                answered.is_null() || answered.as_object().is_some_and(|body| body.is_empty()),
                "case {case:?}: UntagResource answers an empty body, got {answered}"
            );
            let actual = list_tags(&emulator, arn).await;
            assert_eq!(
                actual,
                expected_tags(expected),
                "case {case:?}: expected {:?}, got {actual:?}",
                expected_tags(expected)
            );
        }
    }
}
