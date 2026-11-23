// SPDX-License-Identifier: CC0-1.0

//! The Parameter Store semantics a filter, a policy document, a KMS key and a name's depth
//! are judged by — each suite a TABLE whose rows differ along what the rule generalises
//! over rather than the single parameter one recorded call happened to use.
//!
//! The rules under test:
//!
//! * A `tag:<key>` filter compares the WHOLE tag value whichever option it names, so a
//!   `BeginsWith` on a strict prefix of the stored value matches nothing; on a hierarchy
//!   read the same filter is accepted and narrows nothing at all
//!   (`smithy_query(query="doc",shape="ParameterStringFilter",member="Key")` /
//!   `member="Option"`).
//! * A filter on an attribute whose modelled target is a closed enum (`Type`, `Tier`) can
//!   only carry a value of that enum, whatever comparison option it names, and a value
//!   outside it is `InvalidFilterValue` naming the attribute's whole vocabulary.
//! * A parameter name carries at most the published number of hierarchy LEVELS beside its
//!   leaf ("Parameter hierarchies are limited to a maximum depth of fifteen levels",
//!   `smithy_query(query="doc",shape="PutParameterRequest",member="Name")`; "the hierarchy
//!   is the parameter name except the last part of the parameter",
//!   `member="Path"` of `GetParametersByPathRequest`), and a deeper name is
//!   `HierarchyLevelLimitExceededException` carrying the fixed sentence naming that limit.
//! * `Policies` and `KeyId` belong to the PARAMETER: "All existing policies are preserved
//!   until you send new policies or an empty policy"
//!   (`smithy_query(query="doc",shape="PutParameterRequest",member="Policies")`), and an
//!   update of a `SecureString` that names no key keeps writing under the key the parameter
//!   already carries.
//! * A policy set that cannot stand together is `IncompatiblePolicyException` and nothing
//!   is written: an `ExpirationNotification` is "notification before or after the
//!   expiration time is reached", a time only an `Expiration` policy sets.
//!
//! Every call goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol") — so nothing here reads a state
//! struct and every read follows a write.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{
    call, error_code, error_message, kms_key_with_alias, router, router_with_kms,
};

/// The `ParameterType` values, in the order the modelled enum declares them
/// (`smithy_query(query="shape",shape="ParameterType")`) — which is the order the service
/// lists them in when it refuses a filter value.
const PARAMETER_TYPES: &[&str] = &["String", "StringList", "SecureString"];

/// The `ParameterTier` values, in the same declaration order
/// (`smithy_query(query="shape",shape="ParameterTier")`).
const PARAMETER_TIERS: &[&str] = &["Standard", "Advanced", "Intelligent-Tiering"];

/// The published hierarchy-depth limit (`PutParameterRequest$Name`: "Parameter hierarchies
/// are limited to a maximum depth of fifteen levels").
const MAX_LEVELS: usize = 15;

/// The key a `SecureString` is encrypted under when no put of it ever named one ("the
/// system uses the default key associated with your Amazon Web Services account",
/// `PutParameterRequest$KeyId`), which Parameter Store reaches through this alias.
const DEFAULT_KEY_ALIAS: &str = "alias/aws/ssm";

/// One operation that must succeed, answering its parsed body.
async fn ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, response) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} {body} must succeed, got {status} {response}"
    );
    response
}

/// `PutParameter`, asserting it succeeded and answering the version it created.
async fn put_ok(router: &MultiServiceRouter, case: &str, body: &Value) -> i64 {
    let created = ok(router, case, "PutParameter", body).await;
    created["Version"]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: PutParameter reports a version, got {created}"))
}

/// The `Name` of every parameter a read reported, in the order it reported them.
fn reported_names(response: &Value) -> Vec<String> {
    response["Parameters"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|parameter| {
            parameter["Name"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// A name of `segments` slash-separated segments, the last of them the leaf.
fn name_of_segments(prefix: &str, segments: usize) -> String {
    (1..=segments)
        .map(|segment| format!("/{prefix}{segment}"))
        .collect()
}

/// A `Policies` document carrying one policy per type in `types`.
fn policy_document(types: &[&str]) -> String {
    json!(types
        .iter()
        .map(|policy_type| json!({
            "Type": policy_type,
            "Version": "1.0",
            "Attributes": { "Timestamp": "2035-01-01T00:00:00.000Z" }
        }))
        .collect::<Vec<Value>>())
    .to_string()
}

/// The `PolicyType` of every policy a describe reports for `name`, and the `KeyId` it
/// reports — the metadata an update must or must not have changed.
async fn described(router: &MultiServiceRouter, case: &str, name: &str) -> (Vec<String>, Value) {
    let response = ok(
        router,
        case,
        "DescribeParameters",
        &json!({ "ParameterFilters": [{ "Key": "Name", "Values": [name] }] }),
    )
    .await;
    let entry = response["Parameters"]
        .as_array()
        .and_then(|held| held.first().cloned())
        .unwrap_or_else(|| panic!("case {case:?}: the describe must report {name}, got {response}"));
    let types = entry["Policies"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|policy| {
            policy["PolicyType"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    (types, entry["KeyId"].clone())
}

/// RULE: a `tag:<key>` filter is satisfied by the WHOLE tag value whichever option it
/// names, so `BeginsWith` on a strict prefix of that value matches nothing — while the same
/// filter on a hierarchy read is accepted and narrows nothing at all.
#[tokio::test]
async fn a_tag_filter_matches_the_whole_value_on_a_describe_and_nothing_on_a_hierarchy_read() {
    let router = router();
    let tagged = "/Tags/tagged";
    let plain = "/Tags/plain";
    put_ok(
        &router,
        "seed the tagged parameter",
        &json!({
            "Name": tagged,
            "Type": "String",
            "Value": "t",
            "Tags": [
                { "Key": "env", "Value": "release-candidate" },
                { "Key": "owner", "Value": "platform" },
            ],
        }),
    )
    .await;
    put_ok(
        &router,
        "seed the untagged parameter",
        &json!({ "Name": plain, "Type": "String", "Value": "p" }),
    )
    .await;

    // One row: the case, the tag key, the option to name, the value to search for, and
    // what a describe must report. Rows differ along the option, how much of the tag value
    // the filter carries, and which tag of the two it names.
    type TagRow<'a> = (&'a str, &'a str, Option<&'a str>, &'a str, Vec<&'a str>);
    let rows: Vec<TagRow<'_>> = vec![
        (
            "the whole value, no option",
            "tag:env",
            None,
            "release-candidate",
            vec![tagged],
        ),
        (
            "the whole value, Equals",
            "tag:env",
            Some("Equals"),
            "release-candidate",
            vec![tagged],
        ),
        (
            "the whole value, BeginsWith",
            "tag:env",
            Some("BeginsWith"),
            "release-candidate",
            vec![tagged],
        ),
        (
            "a strict prefix of the value, BeginsWith",
            "tag:env",
            Some("BeginsWith"),
            "release",
            vec![],
        ),
        (
            "one character of the value, BeginsWith",
            "tag:env",
            Some("BeginsWith"),
            "r",
            vec![],
        ),
        (
            "a strict prefix of the value, Equals",
            "tag:env",
            Some("Equals"),
            "release",
            vec![],
        ),
        (
            "the whole value of the other tag, BeginsWith",
            "tag:owner",
            Some("BeginsWith"),
            "platform",
            vec![tagged],
        ),
        (
            "a strict prefix of the other tag's value, BeginsWith",
            "tag:owner",
            Some("BeginsWith"),
            "plat",
            vec![],
        ),
        (
            "a value nothing carries",
            "tag:env",
            Some("Equals"),
            "nothing-is-tagged-this",
            vec![],
        ),
        (
            "a tag key nobody set",
            "tag:absent",
            Some("BeginsWith"),
            "release-candidate",
            vec![],
        ),
    ];

    for (case, key, option, value, expected) in rows {
        let mut filter = json!({ "Key": key, "Values": [value] });
        if let Some(option) = option {
            filter["Option"] = json!(option);
        }
        let narrowed = ok(
            &router,
            case,
            "DescribeParameters",
            &json!({ "ParameterFilters": [filter.clone()] }),
        )
        .await;
        let actual = reported_names(&narrowed);
        assert_eq!(
            actual, expected,
            "case {case:?}: a describe filtered by {filter} must report {expected:?}, got \
             {actual:?}"
        );

        // The same filter on a hierarchy read is accepted and selects nothing: the whole
        // subtree comes back whether the value matches a tag or not.
        let served = ok(
            &router,
            case,
            "GetParametersByPath",
            &json!({ "Path": "/Tags", "ParameterFilters": [filter.clone()] }),
        )
        .await;
        let whole_subtree = vec![plain.to_string(), tagged.to_string()];
        let actual = reported_names(&served);
        assert_eq!(
            actual, whole_subtree,
            "case {case:?}: a hierarchy read filtered by {filter} must serve the whole \
             subtree {whole_subtree:?}, got {actual:?}"
        );
    }
}

/// RULE: a filter on an attribute the model types as a closed enum carries only a value of
/// that enum, whatever comparison option it names; a value outside it is
/// `InvalidFilterValue` naming the attribute's whole published vocabulary.
#[tokio::test]
async fn a_filter_value_outside_a_closed_enum_is_refused_whatever_the_option() {
    let router = router();
    put_ok(
        &router,
        "seed the hierarchy the filters read",
        &json!({ "Name": "/Enum/one", "Type": "String", "Value": "v" }),
    )
    .await;
    put_ok(
        &router,
        "seed a second type",
        &json!({ "Name": "/Enum/list", "Type": "StringList", "Value": "a,b" }),
    )
    .await;

    // Each published value of each enum-typed key is accepted under either option, so the
    // refusals below are about the VALUE and not about the key or the option.
    for (key, catalog) in [("Type", PARAMETER_TYPES), ("Tier", PARAMETER_TIERS)] {
        for value in catalog {
            for option in ["Equals", "BeginsWith"] {
                let case = format!("{key} {option} {value}");
                ok(
                    &router,
                    &case,
                    "DescribeParameters",
                    &json!({
                        "ParameterFilters": [{
                            "Key": key,
                            "Option": option,
                            "Values": [value],
                        }],
                    }),
                )
                .await;
            }
        }
    }

    // One row: the case, the operation, the key, the option, and the value no enum member
    // can be.
    type EnumRow<'a> = (&'a str, &'a str, &'a str, Option<&'a str>, &'a str);
    let rows: Vec<EnumRow<'_>> = vec![
        (
            "a prefix two types share",
            "DescribeParameters",
            "Type",
            Some("BeginsWith"),
            "Str",
        ),
        (
            "the same prefix on a hierarchy read",
            "GetParametersByPath",
            "Type",
            Some("BeginsWith"),
            "Str",
        ),
        (
            "one character of a type",
            "GetParametersByPath",
            "Type",
            Some("BeginsWith"),
            "S",
        ),
        (
            "a type in the wrong case",
            "DescribeParameters",
            "Type",
            Some("Equals"),
            "string",
        ),
        (
            "a type nothing models",
            "GetParametersByPath",
            "Type",
            Some("Equals"),
            "Secret",
        ),
        (
            "a type with no option named",
            "DescribeParameters",
            "Type",
            None,
            "Str",
        ),
        (
            "a prefix of a tier",
            "DescribeParameters",
            "Tier",
            Some("BeginsWith"),
            "Stand",
        ),
        (
            "a tier nothing models",
            "DescribeParameters",
            "Tier",
            None,
            "Premium",
        ),
    ];

    for (case, operation, key, option, value) in rows {
        let mut filter = json!({ "Key": key, "Values": [value] });
        if let Some(option) = option {
            filter["Option"] = json!(option);
        }
        let mut body = json!({ "ParameterFilters": [filter.clone()] });
        if operation == "GetParametersByPath" {
            body["Path"] = json!("/Enum");
        }
        let (status, response) = call(&router, operation, &body).await;
        let catalog = if key == "Type" {
            PARAMETER_TYPES
        } else {
            PARAMETER_TIERS
        };
        let expected = format!(
            "The following filter value is not valid: {value}. Valid values include: [{}].",
            catalog.join(", ")
        );
        assert_eq!(
            (status, error_code(&response), error_message(&response)),
            (400, "InvalidFilterValue".to_string(), expected.clone()),
            "case {case:?}: {operation} {body} must be refused with {expected:?}, got {status} \
             {response}"
        );
    }
}

/// RULE: a name carries at most [`MAX_LEVELS`] hierarchy levels BESIDE its leaf, so the
/// deepest accepted name has one segment more than that; a deeper one is refused with the
/// fixed sentence naming the limit.
#[tokio::test]
async fn a_name_may_carry_the_published_number_of_levels_beside_its_leaf() {
    let router = router();
    let refusal = format!("Depth of the tree cannot exceed {MAX_LEVELS}");

    // (case, how many segments the name carries, whether the put is served)
    let rows: Vec<(&str, usize, bool)> = vec![
        ("a top-level name: no levels at all", 1, true),
        ("one level beside the leaf", 2, true),
        ("half the published depth", MAX_LEVELS / 2 + 1, true),
        ("one level below the limit", MAX_LEVELS, true),
        ("exactly the published depth", MAX_LEVELS + 1, true),
        ("one level past the limit", MAX_LEVELS + 2, false),
        ("far past the limit", MAX_LEVELS + 6, false),
    ];

    for (case, segments, served) in rows {
        let name = name_of_segments(&format!("d{segments}l"), segments);
        let (status, response) = call(
            &router,
            "PutParameter",
            &json!({ "Name": name, "Type": "String", "Value": "v" }),
        )
        .await;
        if served {
            assert_eq!(
                (status, response["Version"].as_i64()),
                (200, Some(1)),
                "case {case:?}: putting the {segments}-segment name {name} must be served at \
                 version 1, got {status} {response}"
            );
            // Served means STORED: the name reads back, so no row passes on a put that
            // answered 200 and wrote nothing.
            let read = ok(&router, case, "GetParameter", &json!({ "Name": name })).await;
            assert_eq!(
                read.pointer("/Parameter/Name").and_then(Value::as_str),
                Some(name.as_str()),
                "case {case:?}: the stored name reads back, got {read}"
            );
        } else {
            assert_eq!(
                (
                    status,
                    error_code(&response),
                    error_message(&response)
                ),
                (
                    400,
                    "HierarchyLevelLimitExceededException".to_string(),
                    refusal.clone()
                ),
                "case {case:?}: putting the {segments}-segment name {name} must be refused \
                 with {refusal:?}, got {status} {response}"
            );
        }
    }
}

/// RULE: a read of a named parameter echoes the `Name` the request asked with — the
/// padding the service trims before it resolves anything included, and the `name:selector`
/// suffix stripped into `Selector` — while a read asked by ARN reports the name the store
/// holds.
#[tokio::test]
async fn a_read_echoes_the_name_it_was_asked_with() {
    let router = router();
    let name = "/Echo/leaf";
    put_ok(
        &router,
        "seed the parameter the reads echo",
        &json!({ "Name": name, "Type": "String", "Value": "first" }),
    )
    .await;
    put_ok(
        &router,
        "a second version to select",
        &json!({ "Name": name, "Value": "second", "Overwrite": true }),
    )
    .await;
    ok(
        &router,
        "label the first version",
        "LabelParameterVersion",
        &json!({ "Name": name, "ParameterVersion": 1, "Labels": ["pinned"] }),
    )
    .await;
    let arn = ok(&router, "the reported ARN", "GetParameter", &json!({ "Name": name }))
        .await
        .pointer("/Parameter/ARN")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // One row: the case, the value to ask with, the `Name` the read must report, and the
    // `Selector` it must report. Rows differ along the padding, the separator, the selector
    // form and whether the value is a name or an ARN.
    type EchoRow<'a> = (&'a str, String, String, Option<&'a str>);
    let rows: Vec<EchoRow<'_>> = vec![
        ("the name as created", name.to_string(), name.to_string(), None),
        (
            "the name padded on both sides",
            format!("  {name}  "),
            format!("  {name}  "),
            None,
        ),
        (
            "the name padded in front",
            format!("\t{name}"),
            format!("\t{name}"),
            None,
        ),
        (
            "a version selector",
            format!("{name}:1"),
            name.to_string(),
            Some(":1"),
        ),
        (
            "a label selector",
            format!("{name}:pinned"),
            name.to_string(),
            Some(":pinned"),
        ),
        (
            "the parameter's own ARN, which names the parameter rather than itself",
            arn.clone(),
            name.to_string(),
            None,
        ),
        (
            "the ARN with a selector",
            format!("{arn}:1"),
            name.to_string(),
            Some(":1"),
        ),
    ];

    for (case, asked, expected, selector) in rows {
        let single = ok(&router, case, "GetParameter", &json!({ "Name": asked })).await;
        assert_eq!(
            (
                single.pointer("/Parameter/Name").and_then(Value::as_str),
                single.pointer("/Parameter/Selector").and_then(Value::as_str),
            ),
            (Some(expected.as_str()), selector),
            "case {case:?}: GetParameter({asked:?}) must report the name {expected:?} and the \
             selector {selector:?}, got {single}"
        );

        let batch = ok(&router, case, "GetParameters", &json!({ "Names": [asked] })).await;
        let reported = batch["Parameters"]
            .as_array()
            .and_then(|held| held.first().cloned())
            .unwrap_or_else(|| panic!("case {case:?}: the batch reports one parameter: {batch}"));
        assert_eq!(
            (
                reported["Name"].as_str(),
                reported["Selector"].as_str(),
            ),
            (Some(expected.as_str()), selector),
            "case {case:?}: GetParameters([{asked:?}]) must report the name {expected:?} and \
             the selector {selector:?}, got {batch}"
        );
    }
}

/// RULE: a name that ENDS in the hierarchy separator names no parameter at all, so it is
/// refused as a malformed request value carrying the fixed sentence that states the
/// mistake.
#[tokio::test]
async fn a_name_that_ends_in_the_separator_is_refused_as_malformed() {
    let router = router();
    let refusal = "Parameter name must not end with slash.";

    // (case, the name) — one level, two, a top-level name spelled without the leading
    // separator, and a doubled trailing one.
    let rows: Vec<(&str, &str)> = vec![
        ("a one-level name", "/Leaf/"),
        ("a two-level name", "/Leaf/sub/"),
        ("a top-level name with no leading separator", "Leaf2/"),
        ("a doubled trailing separator", "/Leaf3//"),
    ];

    for (case, name) in rows {
        let (status, response) = call(
            &router,
            "PutParameter",
            &json!({ "Name": name, "Type": "String", "Value": "v" }),
        )
        .await;
        assert_eq!(
            (status, error_code(&response), error_message(&response)),
            (
                400,
                "ValidationException".to_string(),
                refusal.to_string()
            ),
            "case {case:?}: putting {name} must be refused with {refusal:?}, got {status} \
             {response}"
        );
        // Refused means nothing was written under the name it would have trimmed to.
        let (status, read) = call(
            &router,
            "GetParameter",
            &json!({ "Name": name.trim_end_matches('/') }),
        )
        .await;
        assert_eq!(
            (status, error_code(&read)),
            (400, "ParameterNotFound".to_string()),
            "case {case:?}: a refused put must create nothing, got {status} {read}"
        );
    }
}

/// RULE: `Policies` is applied only when the request CARRIES it — an update that sends none
/// preserves the document the parameter holds, an empty array clears it, and a new array
/// replaces it.
#[tokio::test]
async fn an_update_that_sends_no_policies_preserves_them_and_an_empty_array_clears_them() {
    let router = router();

    // (case, the policy types the create carries) — one policy, two, and the whole
    // supported set, so the rule is not read off a single document.
    let rows: Vec<(&str, &[&str])> = vec![
        ("one policy", &["Expiration"]),
        ("two policies", &["Expiration", "NoChangeNotification"]),
        (
            "every supported policy type",
            &["Expiration", "ExpirationNotification", "NoChangeNotification"],
        ),
    ];

    for (case, created_with) in rows {
        let name = format!("/Policies/{}", created_with.len());
        put_ok(
            &router,
            case,
            &json!({
                "Name": name,
                "Type": "String",
                "Value": "first",
                "Tier": "Advanced",
                "Policies": policy_document(created_with),
            }),
        )
        .await;
        let (held, _) = described(&router, case, &name).await;
        assert_eq!(
            held,
            created_with.to_vec(),
            "case {case:?}: the create must store {created_with:?}, got {held:?}"
        );

        // A value-only update: no `Policies` member at all.
        put_ok(
            &router,
            case,
            &json!({ "Name": name, "Value": "second", "Overwrite": true }),
        )
        .await;
        let (held, _) = described(&router, case, &name).await;
        assert_eq!(
            held,
            created_with.to_vec(),
            "case {case:?}: an update sending no Policies must preserve {created_with:?}, got \
             {held:?}"
        );

        // A new array replaces the set.
        let replacement: &[&str] = &["Expiration"];
        put_ok(
            &router,
            case,
            &json!({
                "Name": name,
                "Value": "third",
                "Overwrite": true,
                "Policies": policy_document(replacement),
            }),
        )
        .await;
        let (held, _) = described(&router, case, &name).await;
        assert_eq!(
            held,
            replacement.to_vec(),
            "case {case:?}: an update sending new policies must replace them, got {held:?}"
        );

        // An EMPTY array clears them, which is the one way to.
        put_ok(
            &router,
            case,
            &json!({
                "Name": name,
                "Value": "fourth",
                "Overwrite": true,
                "Policies": "[]",
            }),
        )
        .await;
        let (held, _) = described(&router, case, &name).await;
        assert!(
            held.is_empty(),
            "case {case:?}: an update sending an empty policy array must clear them, got \
             {held:?}"
        );

        // And once cleared, a value-only update keeps them cleared.
        put_ok(
            &router,
            case,
            &json!({ "Name": name, "Value": "fifth", "Overwrite": true }),
        )
        .await;
        let (held, _) = described(&router, case, &name).await;
        assert!(
            held.is_empty(),
            "case {case:?}: nothing reinstates a cleared policy set, got {held:?}"
        );
    }
}

/// RULE: a policy set that cannot stand together is refused with
/// `IncompatiblePolicyException` naming the policies in conflict, and NOTHING is written — a
/// notification about an expiration needs the expiration whose time it is measured from, and
/// the expiration that sets the one time a parameter expires may not appear twice (a
/// notification type may, because a parameter may be notified about one event more than once).
#[tokio::test]
async fn a_policy_set_that_cannot_stand_together_is_refused_and_writes_nothing() {
    let router = router();

    // One row: the case, the policy document's types, and the sentence the put is refused
    // with (`None` for a set that stands).
    type PolicyRow<'a> = (&'a str, &'a [&'a str], Option<&'a str>);
    let rows: Vec<PolicyRow<'_>> = vec![
        ("an expiration alone", &["Expiration"], None),
        (
            "a no-change notification alone, which is measured from the parameter itself",
            &["NoChangeNotification"],
            None,
        ),
        (
            "an expiration notification beside its expiration",
            &["Expiration", "ExpirationNotification"],
            None,
        ),
        (
            "every supported type at once",
            &["Expiration", "ExpirationNotification", "NoChangeNotification"],
            None,
        ),
        (
            "an expiration notification with no expiration",
            &["ExpirationNotification"],
            Some("Expiration notification policies are missing expiration policy"),
        ),
        (
            "an expiration notification beside only a no-change notification",
            &["ExpirationNotification", "NoChangeNotification"],
            Some("Expiration notification policies are missing expiration policy"),
        ),
        (
            "two expirations",
            &["Expiration", "Expiration"],
            Some("There can be only one expiration policy"),
        ),
        (
            // A parameter may be notified about one expiration more than once — several leads
            // before it — so a repeated notification configures two notifications rather than
            // one configuration stated twice.
            "two notifications of one type",
            &["Expiration", "ExpirationNotification", "ExpirationNotification"],
            None,
        ),
        (
            "two no-change notifications, each its own period",
            &["NoChangeNotification", "NoChangeNotification"],
            None,
        ),
    ];

    for (index, (case, types, refusal)) in rows.iter().enumerate() {
        let name = format!("/Incompatible/{index}");
        let body = json!({
            "Name": name,
            "Type": "String",
            "Value": "v",
            "Tier": "Advanced",
            "Policies": policy_document(types),
        });
        let (status, response) = call(&router, "PutParameter", &body).await;
        let Some(refusal) = refusal else {
            assert_eq!(
                (status, response["Version"].as_i64()),
                (200, Some(1)),
                "case {case:?}: {body} must be served, got {status} {response}"
            );
            continue;
        };
        assert_eq!(
            (status, error_code(&response), error_message(&response)),
            (
                400,
                "IncompatiblePolicyException".to_string(),
                (*refusal).to_string()
            ),
            "case {case:?}: {body} must be refused with {refusal:?}, got {status} {response}"
        );
        // Refused means nothing was created: the name resolves to no parameter at all.
        let (status, read) = call(&router, "GetParameter", &json!({ "Name": name })).await;
        assert_eq!(
            (status, error_code(&read)),
            (400, "ParameterNotFound".to_string()),
            "case {case:?}: a refused put must create nothing, got {status} {read}"
        );
    }

    // The same refusal leaves an EXISTING parameter as it was: no version is appended and
    // the policies it holds stay.
    let name = "/Incompatible/existing";
    put_ok(
        &router,
        "an existing parameter",
        &json!({
            "Name": name,
            "Type": "String",
            "Value": "first",
            "Tier": "Advanced",
            "Policies": policy_document(&["Expiration"]),
        }),
    )
    .await;
    let (status, response) = call(
        &router,
        "PutParameter",
        &json!({
            "Name": name,
            "Value": "second",
            "Overwrite": true,
            "Policies": policy_document(&["ExpirationNotification"]),
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&response)),
        (400, "IncompatiblePolicyException".to_string()),
        "case \"an update to an incompatible set\": must be refused, got {status} {response}"
    );
    let read = ok(
        &router,
        "an existing parameter",
        "GetParameter",
        &json!({ "Name": name }),
    )
    .await;
    assert_eq!(
        (
            read.pointer("/Parameter/Version").and_then(Value::as_i64),
            read.pointer("/Parameter/Value").and_then(Value::as_str),
        ),
        (Some(1), Some("first")),
        "case \"an update to an incompatible set\": the parameter must be untouched, got {read}"
    );
    let (held, _) = described(&router, "an existing parameter", name).await;
    assert_eq!(
        held,
        vec!["Expiration".to_string()],
        "case \"an update to an incompatible set\": the stored policies must be untouched, got \
         {held:?}"
    );
}

/// RULE: `KeyId` names the key the PARAMETER is encrypted under, so an update of a
/// `SecureString` that names none keeps writing under the key it already carries, and only
/// one that names another key re-keys it.
#[tokio::test]
async fn an_update_that_names_no_key_keeps_the_key_the_parameter_carries() {
    let router = router_with_kms();
    let key_id = kms_key_with_alias(&router, "alias/parameter-cmk").await;
    let second_key = kms_key_with_alias(&router, "alias/parameter-cmk-2").await;

    // One row: the case, the name, the key the create names, and the key every later read
    // must report.
    type KeyRow = (&'static str, &'static str, Option<String>, String);
    let rows: Vec<KeyRow> = vec![
        (
            "a customer managed key by id",
            "/Keys/by-id",
            Some(key_id.clone()),
            key_id.clone(),
        ),
        (
            "a customer managed key by alias",
            "/Keys/by-alias",
            Some("alias/parameter-cmk".to_string()),
            "alias/parameter-cmk".to_string(),
        ),
        (
            "no key at all, which is the account's default",
            "/Keys/default",
            None,
            DEFAULT_KEY_ALIAS.to_string(),
        ),
    ];

    for (case, name, created_with, expected) in rows {
        let mut create = json!({ "Name": name, "Type": "SecureString", "Value": "first" });
        if let Some(key) = &created_with {
            create["KeyId"] = json!(key);
        }
        put_ok(&router, case, &create).await;
        let (_, reported) = described(&router, case, name).await;
        assert_eq!(
            reported.as_str(),
            Some(expected.as_str()),
            "case {case:?}: the create must store the key {expected:?}, got {reported}"
        );

        // A value-only update, twice, so the key is seen to survive more than one write.
        for value in ["second", "third"] {
            put_ok(
                &router,
                case,
                &json!({ "Name": name, "Value": value, "Overwrite": true }),
            )
            .await;
            let (_, reported) = described(&router, case, name).await;
            assert_eq!(
                reported.as_str(),
                Some(expected.as_str()),
                "case {case:?}: an update naming no KeyId must keep {expected:?}, got {reported}"
            );
        }

        // The value still opens under that key, so the retention is the real key and not
        // just a reported string.
        let read = ok(
            &router,
            case,
            "GetParameter",
            &json!({ "Name": name, "WithDecryption": true }),
        )
        .await;
        assert_eq!(
            read.pointer("/Parameter/Value").and_then(Value::as_str),
            Some("third"),
            "case {case:?}: the newest value must open under the retained key, got {read}"
        );

        // Naming another key re-keys the parameter: the member is applied when it is sent.
        put_ok(
            &router,
            case,
            &json!({
                "Name": name,
                "Value": "fourth",
                "Overwrite": true,
                "KeyId": second_key,
            }),
        )
        .await;
        let (_, reported) = described(&router, case, name).await;
        assert_eq!(
            reported.as_str(),
            Some(second_key.as_str()),
            "case {case:?}: an update naming a key must store it, got {reported}"
        );

        // And the parameter then stays on the key that update named.
        put_ok(
            &router,
            case,
            &json!({ "Name": name, "Value": "fifth", "Overwrite": true }),
        )
        .await;
        let (_, reported) = described(&router, case, name).await;
        assert_eq!(
            reported.as_str(),
            Some(second_key.as_str()),
            "case {case:?}: the key the last update named is the one kept, got {reported}"
        );
    }
}
