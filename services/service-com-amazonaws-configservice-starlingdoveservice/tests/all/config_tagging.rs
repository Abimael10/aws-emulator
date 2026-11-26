// SPDX-License-Identifier: CC0-1.0

//! Config's tagging loop, end to end over the wire: what `TagResource` writes, what
//! `ListTagsForResource` reports back, what `UntagResource` removes, and which
//! requests each of them refuses.
//!
//! Every case here is a TABLE spanning the class the rule generalises over — the
//! resource types this emulator persists, the tag counts, the key and value lengths at
//! the published bounds, the page sizes and the shapes a `ResourceArn` can take — not
//! one recorded request. The rules pinned are:
//!
//! 1. tagging a resource MERGES: a key the request names takes the value it carries,
//!    a key it does not name keeps the value it had, and the tags of every other
//!    resource are untouched;
//! 2. the tags a resource carries are reported by the tag read path, page by page over
//!    the tag-key ordering, whether they were given at creation or by `TagResource`;
//! 3. untagging removes exactly the keys the request names;
//! 4. a `ResourceArn` that is not an ARN is a malformed request, and a well-formed ARN
//!    naming a resource this account does not hold in this Region is a missing
//!    resource — each with its own modelled error, from every tagging operation;
//! 5. a resource carries at most the published number of tags, and a request that
//!    would take it past that is refused and persists nothing;
//! 6. deleting a resource deletes its tags with it;
//! 7. a submitted tag list whose content Config will not store is refused with
//!    `ValidationException` before anything is written: a key in the namespace AWS
//!    reserves for its own tags, and a key the one list names twice.
//!
//! The harness (`Emulator`, `managed_rule`, `with_recorder_and_channel`) is the one
//! `config_setup_and_rule_loop` already builds.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use service_com_amazonaws_configservice_starlingdoveservice::state::SERVICE_LINKED_PRINCIPALS;

use crate::config_setup_and_rule_loop::{
    managed_rule, recorder_request, with_recorder_and_channel, Emulator, ROLE_ARN,
};

/// The most tags one Config resource can carry: the tags-per-resource limit, which the
/// model corroborates with the `TagList` length maximum of 50 on `TagResource$Tags`
/// (`smithy_query(query="shape", shape="TagList")`).
const TAGS_PER_RESOURCE: usize = 50;

/// The largest `Limit` `ListTagsForResource` accepts
/// (`ListTagsForResourceRequest$Limit`: "The limit maximum is 50. You cannot specify a
/// number greater than 50"). The MODELLED range is 0..100.
const LIMIT_MAXIMUM: i64 = 50;
/// See [`LIMIT_MAXIMUM`]: the top of the modelled `Limit` range.
const LIMIT_MODELLED_MAXIMUM: i64 = 100;

/// `Tag$Key` length is 1..128 and `Tag$Value` length is 0..256
/// (`smithy_query(query="shape", shape="Tag")`).
const TAG_KEY_MAXIMUM_LENGTH: usize = 128;
/// See [`TAG_KEY_MAXIMUM_LENGTH`].
const TAG_VALUE_MAXIMUM_LENGTH: usize = 256;

/// The name every case gives the Config rule it tags.
const RULE_NAME: &str = "tagged-rule";

/// The name every case gives the configuration aggregator it tags.
const AGGREGATOR_NAME: &str = "tagged-aggregator";

/// A twelve-digit source account id for the aggregator's account aggregation source,
/// the form `AccountId`'s `^\d{12}$` pattern allows.
const SOURCE_ACCOUNT: &str = "111122223333";

/// One taggable resource: the resource type Config spells in its tagging
/// documentation, and the ARN this emulator minted for it.
struct Taggable {
    kind: &'static str,
    arn: String,
}

/// A tag set as a table row writes it: key/value pairs, in request order.
type Tags = Vec<(String, String)>;

/// One row of the merge table: the case, the first request's tags, the second
/// request's tags, and the tag set the resource must carry afterwards.
type MergeCase = (&'static str, Tags, Tags, Tags);

/// One row of the untag table: the case, the tags written first, the keys the untag
/// request names, and the tags that must remain.
type UntagCase = (&'static str, Tags, Vec<String>, Tags);

/// One row of the creation-tags table: the case, the resource's ARN, and the tags its
/// create carried.
type CreationCase = (&'static str, String, Tags);

/// A fresh emulator holding one of each resource type this emulator persists and can
/// tag — a configuration recorder, a Config rule and a configuration aggregator — plus
/// the ARNs the service minted for them. Of the other resource types `TagResource`
/// documents, `OrganizationConfigRule` and `OrganizationConformancePack` have no write
/// operation in this emulator yet, so neither can be in the store; a `ConformancePack`,
/// a `StoredQuery`, an `AggregationAuthorization` and a `Connector` do reach the store,
/// and `conformance_packs`, `config_stored_queries` and
/// `aggregation_authorization_tags` cover their tags.
async fn taggable_resources() -> (Emulator, Vec<Taggable>) {
    let emulator = Emulator::new();
    with_recorder_and_channel(&emulator).await;
    emulator.ok("PutConfigRule", &managed_rule(RULE_NAME)).await;
    let aggregator = emulator
        .ok(
            "PutConfigurationAggregator",
            &json!({
                "ConfigurationAggregatorName": AGGREGATOR_NAME,
                "AccountAggregationSources": [
                    { "AccountIds": [SOURCE_ACCOUNT], "AllAwsRegions": true },
                ],
            }),
        )
        .await;
    let aggregator_arn = aggregator["ConfigurationAggregator"]["ConfigurationAggregatorArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the aggregator must carry an ARN, got {aggregator}"))
        .to_string();

    let recorders = emulator
        .ok("DescribeConfigurationRecorders", &json!({}))
        .await;
    let recorder_arn = recorders["ConfigurationRecorders"][0]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("the recorder must carry an arn, got {recorders}"))
        .to_string();

    let rule_arn = described_rule_arn(&emulator, RULE_NAME).await;

    (
        emulator,
        vec![
            Taggable {
                kind: "ConfigurationRecorder",
                arn: recorder_arn,
            },
            Taggable {
                kind: "ConfigRule",
                arn: rule_arn,
            },
            Taggable {
                kind: "ConfigurationAggregator",
                arn: aggregator_arn,
            },
        ],
    )
}

/// The ARN `DescribeConfigRules` reports for the rule `name`.
async fn described_rule_arn(emulator: &Emulator, name: &str) -> String {
    let described = emulator
        .ok("DescribeConfigRules", &json!({ "ConfigRuleNames": [name] }))
        .await;
    described["ConfigRules"][0]["ConfigRuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the rule must carry a ConfigRuleArn, got {described}"))
        .to_string()
}

/// `pairs` as a `TagList` body member.
fn tag_list(pairs: &[(String, String)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(key, value)| json!({ "Key": key, "Value": value }))
            .collect(),
    )
}

/// `count` distinct tags, keyed `key-00`, `key-01`, ... from `first`, each carrying
/// `value-<index>`.
fn tags(first: usize, count: usize) -> Vec<(String, String)> {
    (first..first + count)
        .map(|index| (format!("key-{index:02}"), format!("value-{index}")))
        .collect()
}

/// The same keys as [`tags`], each carrying a DIFFERENT value, so a merge that
/// updates them is observable.
fn retagged(first: usize, count: usize) -> Vec<(String, String)> {
    tags(first, count)
        .into_iter()
        .map(|(key, _)| (key, "revalued".to_string()))
        .collect()
}

/// `arn` with its `index`-th colon-separated ARN field replaced by `value`.
fn with_field(arn: &str, index: usize, value: &str) -> String {
    let mut fields: Vec<String> = arn.splitn(6, ':').map(str::to_string).collect();
    fields[index] = value.to_string();
    fields.join(":")
}

/// Tag `arn` with `pairs`, requiring a 200. An empty list is nothing to send: the
/// modelled `TagList` length minimum of 1 refuses it at the boundary.
async fn tag(emulator: &Emulator, arn: &str, pairs: &[(String, String)]) {
    if pairs.is_empty() {
        return;
    }
    emulator
        .ok(
            "TagResource",
            &json!({ "ResourceArn": arn, "Tags": tag_list(pairs) }),
        )
        .await;
}

/// Every tag on `arn`, read back through `ListTagsForResource` at `limit` tags per
/// page (0 asks for the service's default page size), with the number of pages it
/// took. Panics, naming `case`, if a key is served on two pages.
async fn listed_tags(
    emulator: &Emulator,
    case: &str,
    arn: &str,
    limit: i64,
) -> (BTreeMap<String, String>, usize) {
    let mut collected: BTreeMap<String, String> = BTreeMap::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut body = json!({ "ResourceArn": arn, "Limit": limit });
        if let Some(next) = token.as_deref() {
            body["NextToken"] = json!(next);
        }
        let page = emulator.ok("ListTagsForResource", &body).await;
        pages += 1;
        // "The tags for the resource": the member is always served, so a page with no
        // tags on it carries an EMPTY list rather than omitting it, and a caller reads
        // it unconditionally.
        let served = page["Tags"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected a Tags list, got {page}"));
        for entry in served {
            let key = entry["Key"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a tag Key, got {entry}"))
                .to_string();
            let value = entry["Value"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a tag Value, got {entry}"))
                .to_string();
            if let Some(already) = collected.insert(key.clone(), value) {
                panic!(
                    "case {case:?}: key {key:?} was served twice across pages \
                     (first value {already:?})"
                );
            }
        }
        token = page["NextToken"].as_str().map(str::to_string);
        if token.is_none() {
            break;
        }
        assert!(
            pages <= TAGS_PER_RESOURCE + 1,
            "case {case:?}: ListTagsForResource kept handing out page tokens after \
             {pages} pages"
        );
    }
    (collected, pages)
}

/// Every tag on `arn`, read back at the service's default page size.
async fn all_tags(emulator: &Emulator, case: &str, arn: &str) -> BTreeMap<String, String> {
    listed_tags(emulator, case, arn, 0).await.0
}

/// `pairs` as the map a read is expected to report.
fn expected_map(pairs: &[(String, String)]) -> BTreeMap<String, String> {
    pairs.iter().cloned().collect()
}

/// A `(key, value)` pair from two string literals.
fn pair(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

#[tokio::test]
async fn tagging_merges_the_keys_the_request_names_and_leaves_the_rest() {
    // Rows differ along what the merge rule generalises over: how many keys each
    // request carries, whether the second request repeats a key of the first, the
    // value it gives it (including the empty value `Tag$Value`'s length minimum of 0
    // allows), and the key and value lengths at the published maximums.
    let long_key = "k".repeat(TAG_KEY_MAXIMUM_LENGTH);
    let long_value = "v".repeat(TAG_VALUE_MAXIMUM_LENGTH);
    let cases: Vec<MergeCase> = vec![
        (
            "one tag, then a disjoint one",
            vec![pair("owner", "platform")],
            vec![pair("tier", "gold")],
            vec![pair("owner", "platform"), pair("tier", "gold")],
        ),
        (
            "the same key again updates its value",
            vec![pair("owner", "platform")],
            vec![pair("owner", "security")],
            vec![pair("owner", "security")],
        ),
        (
            "an overlapping subset updates only the keys it names",
            vec![pair("a", "1"), pair("b", "2"), pair("c", "3")],
            vec![pair("b", "22"), pair("d", "4")],
            vec![
                pair("a", "1"),
                pair("b", "22"),
                pair("c", "3"),
                pair("d", "4"),
            ],
        ),
        (
            "an empty value is a value",
            vec![pair("owner", "platform")],
            vec![pair("owner", "")],
            vec![pair("owner", "")],
        ),
        (
            "a key with surrounding whitespace is a distinct key",
            vec![pair(" a ", "1")],
            vec![pair("a", "2")],
            vec![pair(" a ", "1"), pair("a", "2")],
        ),
        (
            "key and value at the published maximum lengths",
            vec![(long_key.clone(), long_value.clone())],
            vec![pair("tier", "gold")],
            vec![(long_key.clone(), long_value.clone()), pair("tier", "gold")],
        ),
        ("a request carrying many keys at once", tags(0, 25), retagged(20, 10), {
            let mut merged = expected_map(&tags(0, 25));
            merged.extend(expected_map(&retagged(20, 10)));
            merged.into_iter().collect()
        }),
    ];

    for (case, first, second, expected) in cases {
        let (emulator, resources) = taggable_resources().await;
        for target in &resources {
            let case = format!("{case} on a {}", target.kind);

            tag(&emulator, &target.arn, &first).await;
            tag(&emulator, &target.arn, &second).await;

            let actual = all_tags(&emulator, &case, &target.arn).await;
            assert_eq!(
                actual,
                expected_map(&expected),
                "case {case:?}: expected the merged tag set {:?}, got {actual:?}",
                expected_map(&expected)
            );

            // Tags belong to ONE resource: nothing landed on the other one.
            for other in resources.iter().filter(|other| other.arn != target.arn) {
                let untouched = all_tags(&emulator, &case, &other.arn).await;
                assert!(
                    untouched.is_empty(),
                    "case {case:?}: expected the {} to carry no tags, got {untouched:?}",
                    other.kind
                );
            }

            // Undo this subject's tags so the next subject starts clean.
            let keys: Vec<String> = all_tags(&emulator, &case, &target.arn)
                .await
                .into_keys()
                .collect();
            emulator
                .ok(
                    "UntagResource",
                    &json!({ "ResourceArn": target.arn, "TagKeys": keys }),
                )
                .await;
        }
    }
}

#[tokio::test]
async fn tags_given_at_creation_are_reported_by_the_tag_read_path() {
    // The tags a create carries and the tags `TagResource` adds are ONE set: the
    // read path reports both, and tagging after creation merges into what creation
    // persisted.
    let emulator = Emulator::new();
    emulator
        .ok(
            "PutConfigurationRecorder",
            &json!({
                "ConfigurationRecorder": {
                    "roleARN": "arn:aws:iam::000000000000:role/config-role",
                },
                "Tags": tag_list(&[pair("owner", "platform"), pair("tier", "gold")]),
            }),
        )
        .await;
    emulator
        .ok(
            "PutDeliveryChannel",
            &json!({ "DeliveryChannel": { "s3BucketName": "config-bucket" } }),
        )
        .await;
    let mut rule = managed_rule(RULE_NAME);
    rule["Tags"] = tag_list(&[pair("owner", "security")]);
    emulator.ok("PutConfigRule", &rule).await;
    let aggregator = emulator
        .ok(
            "PutConfigurationAggregator",
            &json!({
                "ConfigurationAggregatorName": AGGREGATOR_NAME,
                "AccountAggregationSources": [
                    { "AccountIds": [SOURCE_ACCOUNT], "AllAwsRegions": true },
                ],
                "Tags": tag_list(&[pair("team", "platform")]),
            }),
        )
        .await;
    let aggregator_arn = aggregator["ConfigurationAggregator"]["ConfigurationAggregatorArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the aggregator must carry an ARN, got {aggregator}"))
        .to_string();

    let recorders = emulator
        .ok("DescribeConfigurationRecorders", &json!({}))
        .await;
    let recorder_arn = recorders["ConfigurationRecorders"][0]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("the recorder must carry an arn, got {recorders}"))
        .to_string();
    let rule_arn = described_rule_arn(&emulator, RULE_NAME).await;

    let cases: [CreationCase; 3] = [
        (
            "tags given to PutConfigurationRecorder",
            recorder_arn,
            vec![pair("owner", "platform"), pair("tier", "gold")],
        ),
        (
            "tags given to PutConfigRule",
            rule_arn,
            vec![pair("owner", "security")],
        ),
        (
            "tags given to PutConfigurationAggregator",
            aggregator_arn,
            vec![pair("team", "platform")],
        ),
    ];

    for (case, arn, expected) in cases {
        let arn = arn.as_str();
        let actual = all_tags(&emulator, case, arn).await;
        assert_eq!(
            actual,
            expected_map(&expected),
            "case {case:?}: expected the creation tags {:?}, got {actual:?}",
            expected_map(&expected)
        );

        tag(&emulator, arn, &[pair("added", "later")]).await;
        let merged = all_tags(&emulator, case, arn).await;
        let mut wanted = expected_map(&expected);
        wanted.insert("added".to_string(), "later".to_string());
        assert_eq!(
            merged, wanted,
            "case {case:?}: expected tagging to merge into the creation tags, giving \
             {wanted:?}, got {merged:?}"
        );
    }
}

#[tokio::test]
async fn a_tag_that_names_no_key_is_refused() {
    // A tag identifies its value by its key, so an entry with nothing usable there is
    // a malformed request. Rows differ along HOW the key is absent: the member omitted
    // (the model leaves `Tag$Key` optional, so the boundary validator passes it),
    // empty (`TagKey`'s length minimum of 1), and whitespace only.
    let (emulator, resources) = taggable_resources().await;
    let cases: [(&str, Value); 3] = [
        ("no Key member at all", json!([{ "Value": "platform" }])),
        ("an empty Key", json!([{ "Key": "", "Value": "platform" }])),
        (
            "a Key of whitespace only",
            json!([{ "Key": "   ", "Value": "platform" }]),
        ),
    ];

    for (case, tags) in cases {
        for target in &resources {
            let case = format!("{case} on a {}", target.kind);
            let (status, code) = emulator
                .refused(
                    "TagResource",
                    &json!({ "ResourceArn": target.arn, "Tags": tags }),
                )
                .await;
            assert_eq!(
                (status, code.as_str()),
                (400, "ValidationException"),
                "case {case:?}: expected HTTP 400 ValidationException, got {status} {code}"
            );
            let listed = all_tags(&emulator, &case, &target.arn).await;
            assert!(
                listed.is_empty(),
                "case {case:?}: expected the refused request to persist nothing, got \
                 {listed:?}"
            );
        }
    }
}

#[tokio::test]
async fn untagging_removes_exactly_the_keys_the_request_names() {
    // Rows differ along which keys the request names: a subset, every key, a key the
    // resource never carried, the same key twice, a key whose value is empty, many at
    // once, and a key carrying surrounding whitespace — a key is matched VERBATIM, as
    // the write stored it, so the padded key removes the padded key and leaves the
    // unpadded one.
    let cases: Vec<UntagCase> = vec![
        (
            "a key whose own characters include whitespace",
            vec![pair(" a ", "1"), pair("a", "2")],
            vec![" a ".to_string()],
            vec![pair("a", "2")],
        ),
        (
            "the unpadded key does not remove the padded one",
            vec![pair(" a ", "1"), pair("b", "2")],
            vec!["a".to_string()],
            vec![pair(" a ", "1"), pair("b", "2")],
        ),
        (
            "one key of three",
            vec![pair("a", "1"), pair("b", "2"), pair("c", "3")],
            vec!["b".to_string()],
            vec![pair("a", "1"), pair("c", "3")],
        ),
        (
            "every key",
            vec![pair("a", "1"), pair("b", "2")],
            vec!["a".to_string(), "b".to_string()],
            vec![],
        ),
        (
            "a key the resource does not carry",
            vec![pair("a", "1")],
            vec!["absent".to_string()],
            vec![pair("a", "1")],
        ),
        (
            "the same key twice in one request",
            vec![pair("a", "1"), pair("b", "2")],
            vec!["a".to_string(), "a".to_string()],
            vec![pair("b", "2")],
        ),
        (
            "a key whose value is empty",
            vec![pair("a", ""), pair("b", "2")],
            vec!["a".to_string()],
            vec![pair("b", "2")],
        ),
        (
            "many keys at once",
            tags(0, 30),
            tags(0, 25).into_iter().map(|(key, _)| key).collect(),
            tags(25, 5),
        ),
    ];

    for (case, initial, removed, expected) in cases {
        let (emulator, resources) = taggable_resources().await;
        for target in &resources {
            let case = format!("{case} on a {}", target.kind);
            tag(&emulator, &target.arn, &initial).await;
            emulator
                .ok(
                    "UntagResource",
                    &json!({ "ResourceArn": target.arn, "TagKeys": removed }),
                )
                .await;
            let actual = all_tags(&emulator, &case, &target.arn).await;
            assert_eq!(
                actual,
                expected_map(&expected),
                "case {case:?}: expected the remaining tags {:?}, got {actual:?}",
                expected_map(&expected)
            );
        }
    }
}

#[tokio::test]
async fn a_resource_arn_that_names_no_resource_is_refused_by_every_tagging_operation() {
    let (emulator, resources) = taggable_resources().await;
    let rule_arn = resources
        .iter()
        .find(|resource| resource.kind == "ConfigRule")
        .map(|resource| resource.arn.clone())
        .expect("the fixture holds a Config rule");
    let recorder_arn = resources
        .iter()
        .find(|resource| resource.kind == "ConfigurationRecorder")
        .map(|resource| resource.arn.clone())
        .expect("the fixture holds a configuration recorder");

    // A rule that existed and was deleted: its ARN is well formed and names nothing.
    emulator
        .ok("DeleteConfigRule", &json!({ "ConfigRuleName": RULE_NAME }))
        .await;

    // Rows differ along HOW the ARN fails to name a resource of this account in this
    // Region: a resource that never existed, one that was deleted, another Region,
    // another account, another service — against ARNs that are not ARNs at all.
    let cases: [(&str, String, &str); 10] = [
        (
            "a rule id this account never minted",
            "arn:aws:config:us-east-1:000000000000:config-rule/config-rule-zzzzzz".to_string(),
            "ResourceNotFoundException",
        ),
        (
            "the ARN of a deleted rule",
            rule_arn.clone(),
            "ResourceNotFoundException",
        ),
        (
            "a recorder that was never created",
            "arn:aws:config:us-east-1:000000000000:configuration-recorder/default/0000000000000000"
                .to_string(),
            "ResourceNotFoundException",
        ),
        (
            "a well formed ARN in another Region",
            with_field(&rule_arn, 3, "eu-west-1"),
            "ResourceNotFoundException",
        ),
        (
            // The canonical account every ARN this emulator mints carries is
            // `emulator_server::emulator_account_id()`, so this is another account's.
            "a well formed ARN in another account",
            with_field(&rule_arn, 4, "111111111111"),
            "ResourceNotFoundException",
        ),
        (
            "a well formed ARN of another service, with no Region or account",
            "arn:aws:s3:::config-bucket".to_string(),
            "ResourceNotFoundException",
        ),
        (
            "a bare resource name, not an ARN",
            RULE_NAME.to_string(),
            "ValidationException",
        ),
        (
            "an ARN missing its resource field",
            "arn:aws:config:us-east-1:000000000000".to_string(),
            "ValidationException",
        ),
        (
            "an ARN whose resource field is empty",
            "arn:aws:config:us-east-1:000000000000:".to_string(),
            "ValidationException",
        ),
        (
            // The ARN is matched verbatim, the way a tag key is, so the padded form of
            // a LIVE resource's ARN is not that resource's ARN.
            "a live resource's ARN with surrounding whitespace",
            format!(" {recorder_arn} "),
            "ValidationException",
        ),
    ];

    for (case, arn, expected) in cases {
        let requests: [(&str, Value); 3] = [
            (
                "TagResource",
                json!({ "ResourceArn": arn, "Tags": [{ "Key": "owner", "Value": "platform" }] }),
            ),
            (
                "UntagResource",
                json!({ "ResourceArn": arn, "TagKeys": ["owner"] }),
            ),
            ("ListTagsForResource", json!({ "ResourceArn": arn })),
        ];
        for (operation, body) in requests {
            let (status, code) = emulator.refused(operation, &body).await;
            assert_eq!(
                (status, code.as_str()),
                (400, expected),
                "case {case:?} on {operation}: expected HTTP 400 {expected}, got \
                 {status} {code}"
            );
        }
    }
}

#[tokio::test]
async fn list_tags_pages_at_the_requested_limit_and_refuses_one_past_the_published_maximum() {
    let (emulator, resources) = taggable_resources().await;
    let written = tags(0, TAGS_PER_RESOURCE);

    for target in &resources {
        tag(&emulator, &target.arn, &written).await;

        // Rows differ along the page size asked for: the default (0), each end of the
        // accepted range, and sizes in between. Whatever the page size, the pages add
        // up to the whole tag set with no key served twice.
        for limit in [0i64, 1, 2, 7, 25, LIMIT_MAXIMUM] {
            let case = format!("limit {limit} on a {}", target.kind);
            let (listed, pages) = listed_tags(&emulator, &case, &target.arn, limit).await;
            assert_eq!(
                listed,
                expected_map(&written),
                "case {case:?}: expected every tag across the pages, got {} of {}",
                listed.len(),
                written.len()
            );
            let page_size = if limit == 0 { LIMIT_MAXIMUM } else { limit } as usize;
            let expected_pages = written.len().div_ceil(page_size);
            assert_eq!(
                pages, expected_pages,
                "case {case:?}: expected {expected_pages} pages of at most {page_size} \
                 tags, got {pages}"
            );
        }

        // One past the published limit maximum still clears the modelled range of
        // `Limit` (0..100), so the operation itself refuses it.
        for limit in [LIMIT_MAXIMUM + 1, 77, LIMIT_MODELLED_MAXIMUM] {
            let case = format!("limit {limit} on a {}", target.kind);
            let (status, code) = emulator
                .refused(
                    "ListTagsForResource",
                    &json!({ "ResourceArn": target.arn, "Limit": limit }),
                )
                .await;
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidLimitException"),
                "case {case:?}: expected HTTP 400 InvalidLimitException, got {status} {code}"
            );
        }

        // A token this service did not mint names no page to resume at.
        for token in ["not a page token", "%%%", "~~~~"] {
            let case = format!("next token {token:?} on a {}", target.kind);
            let (status, code) = emulator
                .refused(
                    "ListTagsForResource",
                    &json!({ "ResourceArn": target.arn, "NextToken": token }),
                )
                .await;
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidNextTokenException"),
                "case {case:?}: expected HTTP 400 InvalidNextTokenException, got \
                 {status} {code}"
            );
        }
    }
}

#[tokio::test]
async fn a_resource_carries_at_most_the_published_number_of_tags() {
    // Rows differ along how the request lands relative to the limit: under it, exactly
    // on it, one past it, far past it, and a request that only updates keys already
    // there (which cannot grow the set at all).
    let cases: [(&str, usize, usize, usize, bool); 6] = [
        ("well under the limit", 0, 10, 10, true),
        ("the whole limit in one request", 0, TAGS_PER_RESOURCE, TAGS_PER_RESOURCE, true),
        ("exactly the limit in two requests", 30, 20, TAGS_PER_RESOURCE, true),
        ("one past the limit", 30, 21, 30, false),
        ("far past the limit", TAGS_PER_RESOURCE, TAGS_PER_RESOURCE, TAGS_PER_RESOURCE, false),
        (
            "updating keys already at the limit",
            TAGS_PER_RESOURCE,
            0,
            TAGS_PER_RESOURCE,
            true,
        ),
    ];

    for (case, first_count, second_count, expected_count, accepted) in cases {
        let (emulator, resources) = taggable_resources().await;
        for target in &resources {
            let case = format!("{case} on a {}", target.kind);
            tag(&emulator, &target.arn, &tags(0, first_count)).await;

            // The second request adds keys the first did not, except in the row that
            // re-tags keys already there, where it carries the first request's keys
            // with new values.
            let second: Vec<(String, String)> = if second_count == 0 {
                retagged(0, first_count.min(10))
            } else {
                tags(first_count, second_count)
            };
            let body = json!({ "ResourceArn": target.arn, "Tags": tag_list(&second) });

            if accepted {
                emulator.ok("TagResource", &body).await;
            } else {
                let (status, code) = emulator.refused("TagResource", &body).await;
                assert_eq!(
                    (status, code.as_str()),
                    (400, "TooManyTagsException"),
                    "case {case:?}: expected HTTP 400 TooManyTagsException, got \
                     {status} {code}"
                );
            }

            let listed = all_tags(&emulator, &case, &target.arn).await;
            assert_eq!(
                listed.len(),
                expected_count,
                "case {case:?}: expected the resource to carry {expected_count} tags, \
                 got {}",
                listed.len()
            );
            if accepted && second_count == 0 {
                // The accepted update changed values without growing the set.
                for (key, _) in &second {
                    assert_eq!(
                        listed.get(key).map(String::as_str),
                        Some("revalued"),
                        "case {case:?}: expected key {key:?} to carry the updated value, \
                         got {:?}",
                        listed.get(key)
                    );
                }
            }
            if !accepted {
                // A refused request persists NOTHING: not even the keys that fit.
                assert_eq!(
                    listed,
                    expected_map(&tags(0, first_count)),
                    "case {case:?}: expected the refused request to leave the tag set \
                     as it was, got {listed:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn deleting_a_resource_deletes_its_tags() {
    // A resource's tags live ON the resource, so they go when it goes: its ARN stops
    // naming anything, and a resource created again under the same NAME is a new
    // resource with a new ARN and no tags.
    let (emulator, resources) = taggable_resources().await;
    let written = vec![pair("owner", "platform"), pair("tier", "gold")];

    let deletes: [(&str, &str, Value); 2] = [
        (
            "ConfigRule",
            "DeleteConfigRule",
            json!({ "ConfigRuleName": RULE_NAME }),
        ),
        (
            "ConfigurationRecorder",
            "DeleteConfigurationRecorder",
            json!({ "ConfigurationRecorderName": "default" }),
        ),
    ];

    for (kind, operation, body) in deletes {
        let target = resources
            .iter()
            .find(|resource| resource.kind == kind)
            .unwrap_or_else(|| panic!("the fixture holds a {kind}"));
        tag(&emulator, &target.arn, &written).await;
        let before = all_tags(&emulator, kind, &target.arn).await;
        assert_eq!(
            before,
            expected_map(&written),
            "case {kind:?}: expected the tags to be readable before the delete, got \
             {before:?}"
        );

        emulator.ok(operation, &body).await;

        let (status, code) = emulator
            .refused(
                "ListTagsForResource",
                &json!({ "ResourceArn": target.arn }),
            )
            .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ResourceNotFoundException"),
            "case {kind:?}: expected the deleted resource's ARN to name nothing, got \
             {status} {code}"
        );
    }

    // Created again under the same name, a Config rule is a new resource: a new ARN
    // and an empty tag set.
    with_recorder_and_channel(&emulator).await;
    emulator.ok("PutConfigRule", &managed_rule(RULE_NAME)).await;
    let recreated_arn = described_rule_arn(&emulator, RULE_NAME).await;
    let previous = resources
        .iter()
        .find(|resource| resource.kind == "ConfigRule")
        .map(|resource| resource.arn.as_str())
        .expect("the fixture holds a Config rule");
    assert_ne!(
        recreated_arn, previous,
        "case \"recreated rule\": expected a rule created again to get its own ARN, got \
         {recreated_arn}"
    );
    let recreated_tags = all_tags(&emulator, "recreated rule", &recreated_arn).await;
    assert!(
        recreated_tags.is_empty(),
        "case \"recreated rule\": expected a rule created again to carry no tags, got \
         {recreated_tags:?}"
    );
}

/// How Config words the refusal of a tag key in the namespace AWS reserves for its own
/// tags: a fixed sentence, echoing nothing of the request.
const RESERVED_NAMESPACE_MESSAGE: &str = "One or more parameter value is invalid";

/// How Config words the refusal of a tag list that names one key twice: fixed as well,
/// naming the list rather than the key that was repeated.
const DUPLICATE_KEYS_MESSAGE: &str = "Duplicate tags exists in tags list";

/// The error code both refusals carry, which every operation they are answered from
/// models (`smithy_query(query="operation", operation="TagResource")` lists
/// `ValidationException`, client, httpError 400).
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The tag every target carries BEFORE a refused request reaches it, so the refusal can
/// be shown to leave the resource's tag set exactly as it was.
const BASELINE: (&str, &str) = ("owner", "platform");

/// Call `operation` and return its status, its error code and its message.
async fn refusal(emulator: &Emulator, operation: &str, body: &Value) -> (u16, String, String) {
    let (status, parsed) = emulator.call(operation, body).await;
    let code = parsed
        .get("__type")
        .and_then(Value::as_str)
        .map(|wire| wire.rsplit('#').next().unwrap_or(wire).to_string())
        .unwrap_or_else(|| format!("<no __type in {parsed}>"));
    let message = parsed
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (status, code, message)
}

/// Require that `operation` with `body` was refused with the 400 `ValidationException`
/// `expected_message` states.
async fn assert_refused(
    emulator: &Emulator,
    case: &str,
    operation: &str,
    body: &Value,
    expected_message: &str,
) {
    let (status, code, message) = refusal(emulator, operation, body).await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (400, VALIDATION_EXCEPTION, expected_message),
        "case {case:?}: expected {operation} to answer HTTP 400 {VALIDATION_EXCEPTION} \
         {expected_message:?}, got {status} {code} {message:?}"
    );
}

/// One row of the refused-content table: the case, the list the request carries, and
/// the message Config refuses it with.
type RefusedListCase = (&'static str, Tags, &'static str);

/// Every tag list Config will not store, with the sentence it refuses each with: the
/// case, the list the request carries, and the expected message.
///
/// The rows differ along what the rule generalises over — how the reserved prefix is
/// spelled (lower, upper and mixed case, and the prefix with nothing after it), where in
/// the list the offending entry sits, how a repeat is spaced out (adjacent, separated by
/// another key), what values the repeated entries carry (two values, the same value
/// twice), and the length of the repeated key, up to the published maximum. The last row
/// carries BOTH faults and is refused for the one the list breaks first.
fn refused_tag_lists() -> Vec<RefusedListCase> {
    let longest = "k".repeat(TAG_KEY_MAXIMUM_LENGTH);
    vec![
        (
            "a reserved key alone",
            vec![pair("aws:cloudformation:stack-name", "a-stack")],
            RESERVED_NAMESPACE_MESSAGE,
        ),
        (
            "a reserved key spelled in upper case",
            vec![pair("AWS:Name", "a-name")],
            RESERVED_NAMESPACE_MESSAGE,
        ),
        (
            "a reserved key spelled in mixed case",
            vec![pair("Aws:region", "eu-west-1")],
            RESERVED_NAMESPACE_MESSAGE,
        ),
        (
            "the reserved prefix with nothing after it",
            vec![pair("aws:", "nothing")],
            RESERVED_NAMESPACE_MESSAGE,
        ),
        (
            "a reserved key after an acceptable one",
            vec![pair("team", "sre"), pair("aws:createdBy", "someone")],
            RESERVED_NAMESPACE_MESSAGE,
        ),
        (
            "a reserved key at the longest spelling a key may have",
            vec![pair(
                &format!("aws:{}", "x".repeat(TAG_KEY_MAXIMUM_LENGTH - "aws:".len())),
                "v",
            )],
            RESERVED_NAMESPACE_MESSAGE,
        ),
        (
            "a key repeated with two values",
            vec![pair("dupe", "first"), pair("dupe", "second")],
            DUPLICATE_KEYS_MESSAGE,
        ),
        (
            "a key repeated with the same value",
            vec![pair("dupe", "same"), pair("dupe", "same")],
            DUPLICATE_KEYS_MESSAGE,
        ),
        (
            "a key repeated either side of another key",
            vec![
                pair("dupe", "first"),
                pair("other", "value"),
                pair("dupe", "second"),
            ],
            DUPLICATE_KEYS_MESSAGE,
        ),
        (
            "the key the resource already carries, repeated",
            vec![pair(BASELINE.0, "first"), pair(BASELINE.0, "second")],
            DUPLICATE_KEYS_MESSAGE,
        ),
        (
            "a key at the published maximum length repeated",
            vec![
                (longest.clone(), "first".to_string()),
                (longest, "second".to_string()),
            ],
            DUPLICATE_KEYS_MESSAGE,
        ),
        (
            "a reserved key that is also repeated",
            vec![pair("aws:both", "first"), pair("aws:both", "second")],
            RESERVED_NAMESPACE_MESSAGE,
        ),
    ]
}

#[tokio::test]
async fn tag_resource_refuses_a_reserved_or_repeated_key_and_changes_nothing() {
    let (emulator, resources) = taggable_resources().await;
    let carried = vec![pair(BASELINE.0, BASELINE.1)];
    for target in &resources {
        tag(&emulator, &target.arn, &carried).await;
    }

    for (row, list, expected_message) in refused_tag_lists() {
        for target in &resources {
            let case = format!("{row} on a {}", target.kind);
            let body = json!({ "ResourceArn": target.arn, "Tags": tag_list(&list) });
            assert_refused(&emulator, &case, "TagResource", &body, expected_message).await;
            // The refusal is ATOMIC: no key of the refused list is present afterwards,
            // and the tag the resource already carried is untouched.
            let listed = all_tags(&emulator, &case, &target.arn).await;
            assert_eq!(
                listed,
                expected_map(&carried),
                "case {case:?}: expected the refused request to change nothing, got \
                 {listed:?}"
            );
        }
    }
}

#[tokio::test]
async fn tag_resource_stores_keys_that_only_resemble_a_reserved_or_repeated_key() {
    // The other half of the rule, so the checks refuse requests rather than tags. Rows
    // differ along HOW a key comes close without breaking either rule: the reserved
    // letters without the separator, the prefix somewhere other than the start, and two
    // keys that differ only in case or in padding — a tag key is matched verbatim and is
    // case-sensitive, so each pair is two tags rather than one key repeated.
    let cases: [(&str, Vec<(String, String)>); 6] = [
        (
            "the reserved letters with no separator",
            vec![pair("aws", "v")],
        ),
        (
            "keys opening with the reserved letters",
            vec![pair("awsome", "v"), pair("awsaccount", "w")],
        ),
        (
            "the reserved prefix somewhere other than the start",
            vec![pair("my:aws:thing", "v")],
        ),
        (
            "two keys differing only in case",
            vec![pair("dupe", "lower"), pair("Dupe", "upper")],
        ),
        (
            "two keys differing only in padding",
            vec![pair("padded", "bare"), pair(" padded", "leading")],
        ),
        (
            "many distinct keys in one list",
            vec![pair("a", "1"), pair("b", "2"), pair("c", "3")],
        ),
    ];

    for (row, list) in cases {
        let (emulator, resources) = taggable_resources().await;
        for target in &resources {
            let case = format!("{row} on a {}", target.kind);
            emulator
                .ok(
                    "TagResource",
                    &json!({ "ResourceArn": target.arn, "Tags": tag_list(&list) }),
                )
                .await;
            let listed = all_tags(&emulator, &case, &target.arn).await;
            assert_eq!(
                listed,
                expected_map(&list),
                "case {case:?}: expected the written tags to read back, got {listed:?}"
            );
        }
    }
}

/// A `PutStoredQuery` body whose query is a Config query, so only the tags it carries
/// can refuse it.
fn stored_query_request(name: &str) -> Value {
    json!({
        "StoredQuery": {
            "QueryName": name,
            "Expression": "SELECT resourceId WHERE resourceType = 'AWS::S3::Bucket'",
        }
    })
}

#[tokio::test]
async fn a_create_carrying_a_reserved_or_repeated_key_is_refused_and_creates_nothing() {
    // The same content rules on the OTHER operations that write a caller-supplied tag
    // list and model this refusal: the list is judged before the resource is minted, so
    // a refused create leaves nothing behind.
    for (row, list, expected_message) in refused_tag_lists() {
        let emulator = Emulator::new();

        let mut query = stored_query_request("tag-content-query");
        query["Tags"] = tag_list(&list);
        let case = format!("{row} on PutStoredQuery");
        assert_refused(&emulator, &case, "PutStoredQuery", &query, expected_message).await;
        let queries = emulator.ok("ListStoredQueries", &json!({})).await;
        let saved = queries["StoredQueryMetadata"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected a metadata list, got {queries}"));
        assert!(
            saved.is_empty(),
            "case {case:?}: expected the refused create to store no query, got {saved:?}"
        );

        let mut recorder = recorder_request(json!({ "roleARN": ROLE_ARN }));
        recorder["Tags"] = tag_list(&list);
        let case = format!("{row} on PutConfigurationRecorder");
        assert_refused(
            &emulator,
            &case,
            "PutConfigurationRecorder",
            &recorder,
            expected_message,
        )
        .await;

        let mut linked = json!({ "ServicePrincipal": SERVICE_LINKED_PRINCIPALS[0] });
        linked["Tags"] = tag_list(&list);
        let case = format!("{row} on PutServiceLinkedConfigurationRecorder");
        assert_refused(
            &emulator,
            &case,
            "PutServiceLinkedConfigurationRecorder",
            &linked,
            expected_message,
        )
        .await;

        let described = emulator
            .ok("DescribeConfigurationRecorders", &json!({}))
            .await;
        let recorders = described["ConfigurationRecorders"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected a recorder list, got {described}"));
        assert!(
            recorders.is_empty(),
            "case {case:?}: expected the refused creates to store no recorder, got \
             {recorders:?}"
        );
    }
}

#[tokio::test]
async fn a_create_carrying_an_acceptable_tag_list_stores_it() {
    // The accepting half on the create path: a list that breaks neither rule is stored
    // at creation and read back through the tag read path.
    let emulator = Emulator::new();
    let written = vec![
        pair("awsome", "v"),
        pair("dupe", "lower"),
        pair("Dupe", "upper"),
    ];
    let mut query = stored_query_request("tag-content-query");
    query["Tags"] = tag_list(&written);
    let saved = emulator.ok("PutStoredQuery", &query).await;
    let arn = saved["QueryArn"]
        .as_str()
        .unwrap_or_else(|| panic!("PutStoredQuery must answer a QueryArn, got {saved}"))
        .to_string();

    let case = "a create carrying acceptable tags";
    let listed = all_tags(&emulator, case, &arn).await;
    assert_eq!(
        listed,
        expected_map(&written),
        "case {case:?}: expected the created query to carry the tags its create named, \
         got {listed:?}"
    );
}
