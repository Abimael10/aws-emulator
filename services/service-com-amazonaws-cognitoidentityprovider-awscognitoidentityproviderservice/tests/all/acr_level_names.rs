// SPDX-License-Identifier: CC0-1.0

//! The authentication context class reference (ACR) level names a user pool
//! reports, over the wire (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! RULE: a user pool always reports a name for every one of the four fixed ACR
//! levels — the configuration names only the levels it overrides, and every
//! level a request leaves out carries the service's default name
//! `urn:cognito:loa:<level>` — so the create's own answer and every later read
//! of the pool report all four.
//!
//! `UserPoolType.AcrConfiguration` targets `AcrConfigurationType`, a map
//! `@length 0..4` keyed by `AcrLevelKeyType` (`@pattern ^Level[1-4]$`) with
//! `AcrLevelConfigType` values whose `AcrValue` is `@length 1..64` and
//! `@pattern ^[\x21\x23-\x5B\x5D-\x7E]+$`
//! (`smithy_query(query="shape",shape="AcrConfigurationType,AcrLevelConfigType,AcrLevelKeyType")`);
//! the map "Defines the custom names that Amazon Cognito reports for the four
//! fixed authentication context class reference (ACR) levels in a user pool. The
//! map is keyed by level, from `Level1` through `Level4`", and the input member
//! states "You can override a subset of the levels. By default, the levels are
//! named `urn:cognito:loa:1` through `urn:cognito:loa:4`, and Amazon Cognito
//! applies the default name to any level that you don't specify"
//! (`smithy_query(query="doc",shape="CreateUserPoolRequest",member="AcrConfiguration")`).
//!
//! The rows span what the rule generalises over, so no row is the rule: HOW MANY
//! levels a request overrides (none at all, an empty configuration, one, two,
//! all four), WHICH levels it overrides (the first, a middle one, the last), the
//! SIZE of the name it overrides one with (including the longest `AcrValue`'s
//! `@length` admits), and the PATH the configuration is read back through (the
//! create's own answer, a read of the pool, and a read after an update).

use serde_json::{json, Map, Value};

use crate::error_wire::{ok, router};

/// The four fixed ACR levels, in the order `AcrLevelKeyType`'s pattern
/// (`^Level[1-4]$`) admits — the keys `AcrConfigurationType` is "keyed by
/// level, from `Level1` through `Level4`" with.
const ACR_LEVEL_KEYS: [&str; 4] = ["Level1", "Level2", "Level3", "Level4"];

/// The longest name a level may carry: `AcrValueType` is `@length 1..64`.
const LONGEST_ACR_VALUE: usize = 64;

/// A custom name of exactly [`LONGEST_ACR_VALUE`] characters, every one of them
/// inside `AcrValueType`'s pattern.
fn longest_custom_name() -> String {
    let prefix = "urn:example:acr:";
    format!(
        "{prefix}{}",
        "x".repeat(LONGEST_ACR_VALUE - prefix.chars().count())
    )
}

/// The configuration a pool reports when it overrides `overrides` and nothing
/// else: all four levels, the overridden ones under the name given and the rest
/// under the service default.
fn expected_configuration(overrides: &[(&str, String)]) -> Value {
    let mut levels = Map::new();
    for (index, key) in ACR_LEVEL_KEYS.iter().enumerate() {
        let level = index + 1;
        let name = overrides
            .iter()
            .find(|(overridden, _)| overridden == key)
            .map_or_else(|| format!("urn:cognito:loa:{level}"), |(_, name)| name.clone());
        levels.insert((*key).to_string(), json!({ "AcrValue": name }));
    }
    Value::Object(levels)
}

/// The `AcrConfiguration` member of a request that overrides `overrides`.
fn requested_configuration(overrides: &[(&str, String)]) -> Value {
    let mut levels = Map::new();
    for (key, name) in overrides {
        levels.insert((*key).to_string(), json!({ "AcrValue": name }));
    }
    Value::Object(levels)
}

/// One row: what a request asks for, and what every read of the pool then
/// reports.
struct Row {
    case: &'static str,
    /// `None` when the request carries no `AcrConfiguration` member at all.
    overrides: Option<Vec<(&'static str, String)>>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a pool created with no ACR configuration at all",
            overrides: None,
        },
        Row {
            case: "a pool created with an empty ACR configuration",
            overrides: Some(Vec::new()),
        },
        Row {
            case: "a pool that overrides a middle level only",
            overrides: Some(vec![("Level2", "urn:example:acr:two".to_string())]),
        },
        Row {
            case: "a pool that overrides the first and the last level",
            overrides: Some(vec![
                ("Level1", "urn:example:acr:lowest".to_string()),
                ("Level4", "urn:example:acr:highest".to_string()),
            ]),
        },
        Row {
            case: "a pool that overrides a level with the longest name admitted",
            overrides: Some(vec![("Level3", longest_custom_name())]),
        },
        Row {
            case: "a pool that overrides every level",
            overrides: Some(vec![
                ("Level1", "urn:example:acr:1".to_string()),
                ("Level2", "urn:example:acr:2".to_string()),
                ("Level3", "urn:example:acr:3".to_string()),
                ("Level4", "urn:example:acr:4".to_string()),
            ]),
        },
    ]
}

/// The create body for `row`, named after `pool_name`.
fn create_body(pool_name: &str, row: &Row) -> Value {
    let mut body = Map::new();
    body.insert("PoolName".to_string(), json!(pool_name));
    if let Some(overrides) = row.overrides.as_ref() {
        body.insert(
            "AcrConfiguration".to_string(),
            requested_configuration(overrides),
        );
    }
    Value::Object(body)
}

#[tokio::test]
async fn every_pool_reports_all_four_acr_levels() {
    let router = router();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let pool_name = format!("acr-levels-{index}");
        let expected = expected_configuration(row.overrides.as_deref().unwrap_or(&[]));

        let created = ok(&router, "CreateUserPool", &create_body(&pool_name, row)).await;
        let reported = &created["UserPool"]["AcrConfiguration"];
        assert_eq!(
            reported, &expected,
            "case {case:?}: expected the create to report {expected}, got {reported:?}"
        );

        let user_pool_id = created["UserPool"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the create mints a pool id, got {created}"))
            .to_string();
        let described = ok(
            &router,
            "DescribeUserPool",
            &json!({ "UserPoolId": &user_pool_id }),
        )
        .await;
        let read_back = &described["UserPool"]["AcrConfiguration"];
        assert_eq!(
            read_back, &expected,
            "case {case:?}: expected the read to report {expected}, got {read_back:?}"
        );
    }
}

#[tokio::test]
async fn an_update_leaves_every_level_named() {
    let router = router();
    let created = ok(
        &router,
        "CreateUserPool",
        &json!({ "PoolName": "acr-levels-updated" }),
    )
    .await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .expect("the create mints a pool id")
        .to_string();

    // Each row updates the SAME pool, so a later row also shows that what the
    // previous one stored does not survive an update that does not name it: the
    // configuration a pool reports is the one its last update asked for, with
    // the defaults filled in.
    for row in rows() {
        let case = row.case;
        let expected = expected_configuration(row.overrides.as_deref().unwrap_or(&[]));
        let mut body = Map::new();
        body.insert("UserPoolId".to_string(), json!(&user_pool_id));
        if let Some(overrides) = row.overrides.as_ref() {
            body.insert(
                "AcrConfiguration".to_string(),
                requested_configuration(overrides),
            );
        }
        ok(&router, "UpdateUserPool", &Value::Object(body)).await;

        let described = ok(
            &router,
            "DescribeUserPool",
            &json!({ "UserPoolId": &user_pool_id }),
        )
        .await;
        let read_back = &described["UserPool"]["AcrConfiguration"];
        assert_eq!(
            read_back, &expected,
            "case {case:?}: expected the updated pool to report {expected}, got {read_back:?}"
        );
    }
}

/// The level KEYS and the default names, read off the response rather than
/// assembled by the helper above, so a failure there is about the service and
/// not about the table: a pool that configures nothing reports exactly the four
/// modelled keys, and each one's name ends in that level's own number.
#[tokio::test]
async fn the_default_names_are_the_four_modelled_levels() {
    let router = router();
    let created = ok(
        &router,
        "CreateUserPool",
        &json!({ "PoolName": "acr-levels-defaults" }),
    )
    .await;
    let reported = created["UserPool"]["AcrConfiguration"]
        .as_object()
        .unwrap_or_else(|| panic!("the create reports an ACR configuration, got {created}"));

    let mut keys: Vec<&str> = reported.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ACR_LEVEL_KEYS.to_vec(),
        "case \"the four fixed levels\": expected exactly {ACR_LEVEL_KEYS:?}, got {keys:?}"
    );

    for (index, key) in ACR_LEVEL_KEYS.iter().enumerate() {
        let level = index + 1;
        let name = reported[*key]["AcrValue"]
            .as_str()
            .unwrap_or_else(|| panic!("case {key:?}: the level carries a name, got {reported:?}"));
        assert_eq!(
            name,
            format!("urn:cognito:loa:{level}"),
            "case {key:?}: expected the default name for level {level}, got {name:?}"
        );
        assert!(
            (1..=LONGEST_ACR_VALUE).contains(&name.chars().count()),
            "case {key:?}: expected a name of 1..={LONGEST_ACR_VALUE} characters, got {name:?}"
        );
    }
}
