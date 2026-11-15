// SPDX-License-Identifier: CC0-1.0

//! Tagging a Config resource type whose tags only its OWN create and the tagging
//! surface reach: an aggregation authorization, and a connector.
//!
//! The rule pinned here: a tagging request's `ResourceArn` resolves to the tag set of
//! whichever taggable Config resource this emulator holds under that ARN — so the tags
//! a create carried are reported by `ListTagsForResource`, `TagResource` and
//! `UntagResource` change them, and the not-found refusal answers the ARN only once no
//! such resource is held any more.
//!
//! Rows span what the rule generalises over: the resource TYPE behind the ARN (an
//! aggregation authorization, whose ARN Config builds from the authorized pair, and a
//! connector, whose ARN it builds from the derived name), the authorized pair and the
//! Region the request is signed for, how many tags the create carried (none, the
//! modelled minimum and maximum of `TagsList`, and in between), the key and value
//! lengths at the published bounds, and which keys a later tag or untag names.
//!
//! The harness (`Emulator`) is the one `config_setup_and_rule_loop` builds and the
//! connector seed is the one `config_connectors` builds, because `PutConnector` creates
//! no connector in this emulator (`connector_credential_verification` pins why). Every
//! case drives the emulator the way a client does, through the `MultiServiceRouter`.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::config_connectors::seed_connector;
use crate::config_setup_and_rule_loop::Emulator;

/// Twelve-digit aggregator account ids, the form `AccountId`'s `^\d{12}$` pattern
/// allows (`smithy_query(query="shape", shape="AccountId")`).
const ACCOUNT_A: &str = "111122223333";
/// See [`ACCOUNT_A`].
const ACCOUNT_B: &str = "444455556666";

/// The most tags one `PutAggregationAuthorization` request can carry
/// (`smithy_query(query="shape", shape="TagsList")`: `length 0..50`), which is also the
/// most tags one Config resource carries.
const TAGS_LIST_MAXIMUM: usize = 50;

/// `Tag$Key`'s length maximum (`smithy_query(query="shape", shape="Tag")`: `Key` 1..128,
/// `Value` 0..256).
const TAG_KEY_MAXIMUM_LENGTH: usize = 128;
/// See [`TAG_KEY_MAXIMUM_LENGTH`].
const TAG_VALUE_MAXIMUM_LENGTH: usize = 256;

/// The Azure directory and application identifiers a seeded connector carries: GUIDs,
/// the form `AzureTenantIdentifier`/`AzureClientIdentifier` take.
const AZURE_TENANT: &str = "11111111-2222-3333-4444-555555555555";
/// See [`AZURE_TENANT`].
const AZURE_CLIENT: &str = "66666666-7777-8888-9999-aaaaaaaaaaaa";

/// A tag set as a row writes it.
type Tags = Vec<(String, String)>;

/// A `(key, value)` pair from two string literals.
fn pair(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// `count` distinct tags from `first`, keyed `key-00`, `key-01`, ...
fn numbered_tags(first: usize, count: usize) -> Tags {
    (first..first + count)
        .map(|index| (format!("key-{index:02}"), format!("value-{index}")))
        .collect()
}

/// `pairs` as a `TagsList`/`TagList` body member.
fn tag_list(pairs: &[(String, String)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(key, value)| json!({ "Key": key, "Value": value }))
            .collect(),
    )
}

/// `pairs` as the map a read is expected to report.
fn expected_map(pairs: &[(String, String)]) -> BTreeMap<String, String> {
    pairs.iter().cloned().collect()
}

/// Grant the authorization for `account`/`region` with `tags` (`None` sends no `Tags`
/// member at all) and answer the ARN the service built for it.
async fn granted_authorization(
    emulator: &Emulator,
    case: &str,
    account: &str,
    region: &str,
    tags: Option<&Tags>,
) -> String {
    let mut request = json!({
        "AuthorizedAccountId": account,
        "AuthorizedAwsRegion": region,
    });
    if let Some(tags) = tags {
        request["Tags"] = tag_list(tags);
    }
    let response = emulator.ok("PutAggregationAuthorization", &request).await;
    response["AggregationAuthorization"]["AggregationAuthorizationArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the authorization to carry an ARN, got {response}")
        })
        .to_string()
}

/// Whether `DescribeAggregationAuthorizations` still reports the authorization `arn`
/// names.
async fn authorization_is_reported(emulator: &Emulator, arn: &str) -> bool {
    let reported = emulator
        .ok("DescribeAggregationAuthorizations", &json!({}))
        .await;
    reported["AggregationAuthorizations"]
        .as_array()
        .is_some_and(|list| {
            list.iter()
                .any(|entry| entry["AggregationAuthorizationArn"].as_str() == Some(arn))
        })
}

/// Every tag `ListTagsForResource` reports for `arn`, walked across its pages.
async fn listed_tags(emulator: &Emulator, case: &str, arn: &str) -> BTreeMap<String, String> {
    let mut collected: BTreeMap<String, String> = BTreeMap::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut body = json!({ "ResourceArn": arn });
        if let Some(next) = token.as_deref() {
            body["NextToken"] = json!(next);
        }
        let page = emulator.ok("ListTagsForResource", &body).await;
        pages += 1;
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
                    "case {case:?}: key {key:?} was served twice across pages (first value \
                     {already:?})"
                );
            }
        }
        token = page["NextToken"].as_str().map(str::to_string);
        if token.is_none() {
            return collected;
        }
        assert!(
            pages <= TAGS_LIST_MAXIMUM + 1,
            "case {case:?}: ListTagsForResource kept handing out page tokens after {pages} pages"
        );
    }
}

/// The status, error code and message of a refused call.
async fn refusal(emulator: &Emulator, operation: &str, body: &Value) -> (u16, String, String) {
    let (status, parsed) = emulator.call(operation, body).await;
    let code = parsed["__type"]
        .as_str()
        .map(|wire| wire.rsplit('#').next().unwrap_or(wire).to_string())
        .unwrap_or_else(|| format!("<no __type in {parsed}>"));
    let message = parsed["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected the refusal to carry a message, got {parsed}"))
        .to_string();
    (status, code, message)
}

/// The three tagging requests that name `arn`, each with a body that would succeed on a
/// resource the account holds.
fn tagging_requests(arn: &str) -> [(&'static str, Value); 3] {
    [
        (
            "ListTagsForResource",
            json!({ "ResourceArn": arn }),
        ),
        (
            "TagResource",
            json!({ "ResourceArn": arn, "Tags": [{ "Key": "owner", "Value": "platform" }] }),
        ),
        (
            "UntagResource",
            json!({ "ResourceArn": arn, "TagKeys": ["owner"] }),
        ),
    ]
}

#[tokio::test]
async fn the_tags_an_authorization_was_granted_with_are_reported_by_the_tag_read_path() {
    let longest_key = "k".repeat(TAG_KEY_MAXIMUM_LENGTH);
    let longest_value = "v".repeat(TAG_VALUE_MAXIMUM_LENGTH);
    // Rows differ along what the create carried: no `Tags` member at all, the empty
    // list `TagsList`'s length minimum of 0 allows, one tag, two, a key and a value at
    // the published maximum lengths, an empty value (`Tag$Value`'s length minimum of
    // 0), and the full `TagsList` maximum.
    let cases: Vec<(&str, Option<Tags>)> = vec![
        ("no Tags member at all", None),
        ("an empty tag list", Some(Vec::new())),
        ("one tag", Some(vec![pair("team", "audit")])),
        (
            "two tags",
            Some(vec![pair("team", "audit"), pair("env", "dev")]),
        ),
        (
            "a key and a value at the published maximum lengths",
            Some(vec![(longest_key.clone(), longest_value.clone())]),
        ),
        ("an empty value", Some(vec![pair("team", "")])),
        (
            "the whole TagsList maximum",
            Some(numbered_tags(0, TAGS_LIST_MAXIMUM)),
        ),
    ];

    // The authorized pair and the signing Region vary with the row, so the ARN the
    // tags are read through differs in both halves of the pair and in its Region
    // segment.
    let pairs = [
        (ACCOUNT_A, "us-west-2", "us-east-1"),
        (ACCOUNT_B, "eu-west-1", "eu-west-1"),
    ];

    for (index, (case, tags)) in cases.iter().enumerate() {
        let (account, authorized_region, signing_region) = pairs[index % pairs.len()];
        let case = format!("{case} (signed for {signing_region})");
        let emulator = Emulator::in_region(signing_region);
        let arn = granted_authorization(
            &emulator,
            &case,
            account,
            authorized_region,
            tags.as_ref(),
        )
        .await;

        // The authorization the tags are read through is LIVE: its own read path
        // reports it.
        assert!(
            authorization_is_reported(&emulator, &arn).await,
            "case {case:?}: expected the authorization {arn} to be reported by its own read path"
        );

        let expected = expected_map(tags.as_ref().unwrap_or(&Vec::new()));
        let listed = listed_tags(&emulator, &case, &arn).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: expected the creation tags {expected:?} on {arn}, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn tagging_and_untagging_a_live_authorization_change_the_tags_it_reports() {
    // Rows differ along what the tag request names relative to the creation tags (a new
    // key, a key already there, both at once, many at once) and along which keys the
    // untag request then names (one, several, every one, and a key the resource does
    // not carry).
    let creation = vec![pair("team", "audit"), pair("env", "dev")];
    let cases: Vec<(&str, Tags, Vec<&str>, Tags)> = vec![
        (
            "a disjoint key added, then removed again",
            vec![pair("owner", "platform")],
            vec!["owner"],
            creation.clone(),
        ),
        (
            "a key already there is overwritten, and another removed",
            vec![pair("env", "prod")],
            vec!["team"],
            vec![pair("env", "prod")],
        ),
        (
            "an overwrite and an addition at once, then an untag naming neither",
            vec![pair("env", "prod"), pair("owner", "platform")],
            vec!["absent"],
            vec![
                pair("env", "prod"),
                pair("owner", "platform"),
                pair("team", "audit"),
            ],
        ),
        (
            "every key removed",
            vec![pair("owner", "platform")],
            vec!["team", "env", "owner"],
            Vec::new(),
        ),
        (
            "many keys at once",
            numbered_tags(0, 30),
            vec!["key-00", "key-01", "team"],
            {
                let mut remaining = expected_map(&numbered_tags(2, 28));
                remaining.extend(expected_map(&[pair("env", "dev")]));
                remaining.into_iter().collect()
            },
        ),
    ];

    for (case, added, removed, expected) in cases {
        let emulator = Emulator::new();
        // The SAME rule holds for every taggable type behind an ARN, so each row is
        // driven against an aggregation authorization — created with tags — and a
        // connector, which carries none until this surface gives it some.
        let authorization =
            granted_authorization(&emulator, case, ACCOUNT_A, "us-west-2", Some(&creation)).await;
        let connector = seed_connector(&emulator, AZURE_TENANT, AZURE_CLIENT);
        let subjects = [
            ("AggregationAuthorization", authorization, creation.clone()),
            ("Connector", connector, Vec::new()),
        ];

        for (kind, arn, carried) in subjects {
            let case = format!("{case} on a {kind}");
            emulator
                .ok(
                    "TagResource",
                    &json!({ "ResourceArn": arn, "Tags": tag_list(&added) }),
                )
                .await;

            // Tagging MERGES into whatever the resource carried.
            let mut merged = expected_map(&carried);
            merged.extend(expected_map(&added));
            let listed = listed_tags(&emulator, &case, &arn).await;
            assert_eq!(
                listed, merged,
                "case {case:?}: expected the merged tag set {merged:?} on {arn}, got {listed:?}"
            );

            emulator
                .ok(
                    "UntagResource",
                    &json!({ "ResourceArn": arn, "TagKeys": removed }),
                )
                .await;

            let mut remaining = merged;
            for key in &removed {
                remaining.remove(*key);
            }
            // For the authorization the row's own expectation is the one to beat; the
            // connector started from a different tag set, so its expectation is
            // computed the same way the merge was.
            let wanted = if kind == "AggregationAuthorization" {
                expected_map(&expected)
            } else {
                remaining
            };
            let listed = listed_tags(&emulator, &case, &arn).await;
            assert_eq!(
                listed, wanted,
                "case {case:?}: expected the remaining tags {wanted:?} on {arn}, got {listed:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_repeat_grant_leaves_the_tags_the_authorization_already_carries() {
    // "If a following request has different tags values, Config will ignore these
    // differences and treat it as an idempotent request of the previous"
    // (`PutAggregationAuthorization`). Rows differ along what the repeat carries, and
    // along whether the tagging surface changed the tags before it.
    let creation = vec![pair("team", "audit"), pair("env", "dev")];
    let cases: Vec<(&str, Option<Tags>, bool)> = vec![
        ("the repeat carries different values", Some(vec![pair("team", "security")]), false),
        ("the repeat carries more keys", Some(vec![pair("owner", "platform")]), false),
        ("the repeat carries no Tags member", None, false),
        ("the repeat follows a TagResource", Some(vec![pair("team", "security")]), true),
    ];

    for (case, repeat_tags, tag_first) in cases {
        let emulator = Emulator::new();
        let arn =
            granted_authorization(&emulator, case, ACCOUNT_A, "us-west-2", Some(&creation)).await;

        let mut expected = expected_map(&creation);
        if tag_first {
            let added = vec![pair("owner", "platform")];
            emulator
                .ok(
                    "TagResource",
                    &json!({ "ResourceArn": arn, "Tags": tag_list(&added) }),
                )
                .await;
            expected.extend(expected_map(&added));
        }

        let repeated = granted_authorization(
            &emulator,
            case,
            ACCOUNT_A,
            "us-west-2",
            repeat_tags.as_ref(),
        )
        .await;
        assert_eq!(
            repeated, arn,
            "case {case:?}: expected the repeat grant to answer the same ARN, got {repeated}"
        );

        let listed = listed_tags(&emulator, case, &arn).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: expected the tags to stay {expected:?}, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn the_authorizations_arn_names_nothing_only_once_the_authorization_is_revoked() {
    // Rows differ along the authorized pair and the Region the requests are signed for,
    // so the ARN that stops resolving differs in every segment it is built from.
    let cases: [(&str, &str, &str, &str); 3] = [
        ("the caller's own account and Region", ACCOUNT_A, "us-west-2", "us-west-2"),
        ("another account, another Region", ACCOUNT_B, "eu-west-1", "us-east-1"),
        ("a one-character authorized Region", ACCOUNT_A, "a", "eu-central-1"),
    ];

    // The sentence Config words the refusal with: fixed text, the ARN FROM THE REQUEST
    // and the account the lookup happened in.
    let account = emulator_server::emulator_account_id();

    for (case, authorized_account, authorized_region, signing_region) in cases {
        let emulator = Emulator::in_region(signing_region);
        let tags = vec![pair("team", "audit")];
        let arn = granted_authorization(
            &emulator,
            case,
            authorized_account,
            authorized_region,
            Some(&tags),
        )
        .await;

        // While the authorization is live, every tagging operation serves it.
        for (operation, body) in tagging_requests(&arn) {
            let (status, parsed) = emulator.call(operation, &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected {operation} to serve the live authorization, got \
                 {status} with body {parsed}"
            );
        }

        emulator
            .ok(
                "DeleteAggregationAuthorization",
                &json!({
                    "AuthorizedAccountId": authorized_account,
                    "AuthorizedAwsRegion": authorized_region,
                }),
            )
            .await;
        assert!(
            !authorization_is_reported(&emulator, &arn).await,
            "case {case:?}: expected the revoked authorization {arn} to be reported no more"
        );

        // Only now does the ARN name nothing — the same class and the same sentence
        // from every tagging operation.
        let expected = format!("Resource {arn} does not exist for account {account}");
        for (operation, body) in tagging_requests(&arn) {
            let (status, code, message) = refusal(&emulator, operation, &body).await;
            assert_eq!(
                (status, code.as_str(), message.as_str()),
                (400, "ResourceNotFoundException", expected.as_str()),
                "case {case:?}: expected {operation} to answer HTTP 400 \
                 ResourceNotFoundException with message {expected:?}, got {status} {code} with \
                 message {message:?}"
            );
        }

        // Granted again, the same pair is live again and carries no tags: the revoked
        // authorization's tags went with it.
        let regranted =
            granted_authorization(&emulator, case, authorized_account, authorized_region, None)
                .await;
        assert_eq!(
            regranted, arn,
            "case {case:?}: expected the regranted pair to carry the same ARN, got {regranted}"
        );
        let listed = listed_tags(&emulator, case, &arn).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected the regranted authorization to carry no tags, got {listed:?}"
        );
    }
}
