// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the configuration a stack reports for the members its create
//! request did not set.
//!
//! Every case drives the awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! The rule under test, asserted over a TABLE of create requests rather than over one
//! recorded body: a created stack reports the EFFECTIVE value of every configuration
//! member the service settles itself — which actions a streaming session permits, whether
//! application settings persist, the streaming protocol the stack prefers — so a member
//! the request carries is reported as given and a member it omits is reported with the
//! service's own value, from `CreateStack` and from every later `DescribeStacks` alike.
//! A member the service settles nothing for stays absent.

use serde_json::{json, Value};

use crate::stack_fleet_lifecycle::Emulator;

/// The permission a stack reports for each action when the create named no `UserSettings`,
/// in the order the resource reports them.
///
/// The action names are the `Action` enum's (`smithy_query(query="shape", shape="Action")`,
/// kept as `ACTION_VALUES` in `src/ops/types/shared_types.rs`) and the permissions are the
/// `Permission` enum's (`PERMISSION_VALUES`); `Stack.UserSettings` documents that these
/// actions are enabled by default, apart from the smart-card sign-in and the time-zone
/// redirection the service leaves off.
const DEFAULT_USER_SETTINGS: &[(&str, &str)] = &[
    ("CLIPBOARD_COPY_FROM_LOCAL_DEVICE", "ENABLED"),
    ("CLIPBOARD_COPY_TO_LOCAL_DEVICE", "ENABLED"),
    ("FILE_UPLOAD", "ENABLED"),
    ("FILE_DOWNLOAD", "ENABLED"),
    ("PRINTING_TO_LOCAL_DEVICE", "ENABLED"),
    ("DOMAIN_PASSWORD_SIGNIN", "ENABLED"),
    ("DOMAIN_SMART_CARD_SIGNIN", "DISABLED"),
    ("AUTO_TIME_ZONE_REDIRECTION", "DISABLED"),
];

/// The `UserSettings` list a stack whose create named none must report.
fn default_user_settings() -> Value {
    Value::Array(
        DEFAULT_USER_SETTINGS
            .iter()
            .map(|(action, permission)| json!({ "Action": action, "Permission": permission }))
            .collect(),
    )
}

/// The persistent application settings a stack whose create named none must report.
fn default_application_settings() -> Value {
    json!({ "Enabled": false })
}

/// The streaming experience a stack whose create named none must report. The two protocols
/// are `PreferredProtocol`'s (`PREFERRED_PROTOCOL_VALUES`); TCP is the one every client can
/// stream over, so it is the one a stack prefers until it asks for the other.
fn default_streaming_experience_settings() -> Value {
    json!({ "PreferredProtocol": "TCP" })
}

/// One create case: the request, and what the created stack must report for each of the
/// three members the service settles itself. `None` means "the service's own value".
struct DefaultsRow {
    case: &'static str,
    create: Value,
    user_settings: Option<Value>,
    application_settings: Option<Value>,
    streaming_experience_settings: Option<Value>,
}

/// Create requests spanning the class: the rows differ along WHICH of the three members the
/// request sets (none, one of each, all three) and along the kind and size of the value it
/// sets them to — one entry, an entry carrying the optional clipboard limit, an entry per
/// modelled action, each protocol value, settings that persist and settings that do not.
///
/// Member names and the enum values come from `smithy_query(query="operation",
/// operation="CreateStack")` and the `*_VALUES` constants in
/// `src/ops/types/shared_types.rs`.
fn defaults_rows() -> Vec<DefaultsRow> {
    vec![
        DefaultsRow {
            case: "name only: every member the service settles is reported",
            create: json!({ "Name": "defaults-only" }),
            user_settings: None,
            application_settings: None,
            streaming_experience_settings: None,
        },
        DefaultsRow {
            case: "unrelated configuration only: the three members are still reported",
            create: json!({
                "Name": "defaults-beside-storage",
                "Description": "a stack that configures its storage and nothing else",
                "StorageConnectors": [{ "ConnectorType": "HOMEFOLDERS" }],
                "EmbedHostDomains": ["a.example.invalid"],
            }),
            user_settings: None,
            application_settings: None,
            streaming_experience_settings: None,
        },
        DefaultsRow {
            case: "one user setting given: the other two members are the service's",
            create: json!({
                "Name": "given-one-user-setting",
                "UserSettings": [{ "Action": "FILE_DOWNLOAD", "Permission": "DISABLED" }],
            }),
            user_settings: Some(
                json!([{ "Action": "FILE_DOWNLOAD", "Permission": "DISABLED" }]),
            ),
            application_settings: None,
            streaming_experience_settings: None,
        },
        DefaultsRow {
            case: "a user setting carrying the clipboard limit",
            create: json!({
                "Name": "given-clipboard-limit",
                "UserSettings": [{
                    "Action": "CLIPBOARD_COPY_FROM_LOCAL_DEVICE",
                    "Permission": "ENABLED",
                    "MaximumLength": 2048,
                }],
            }),
            user_settings: Some(json!([{
                "Action": "CLIPBOARD_COPY_FROM_LOCAL_DEVICE",
                "Permission": "ENABLED",
                "MaximumLength": 2048,
            }])),
            application_settings: None,
            streaming_experience_settings: None,
        },
        DefaultsRow {
            case: "every modelled action given the opposite permission",
            create: json!({ "Name": "given-every-action", "UserSettings": flipped_user_settings() }),
            user_settings: Some(flipped_user_settings()),
            application_settings: None,
            streaming_experience_settings: None,
        },
        DefaultsRow {
            case: "persistent application settings given: the other two are the service's",
            create: json!({
                "Name": "given-application-settings",
                "ApplicationSettings": { "Enabled": true, "SettingsGroup": "group-a" },
            }),
            user_settings: None,
            application_settings: Some(
                json!({ "Enabled": true, "SettingsGroup": "group-a" }),
            ),
            streaming_experience_settings: None,
        },
        DefaultsRow {
            case: "application settings given as not persisting",
            create: json!({
                "Name": "given-application-settings-off",
                "ApplicationSettings": { "Enabled": false },
            }),
            user_settings: None,
            application_settings: Some(json!({ "Enabled": false })),
            streaming_experience_settings: None,
        },
        DefaultsRow {
            case: "the other streaming protocol given",
            create: json!({
                "Name": "given-udp",
                "StreamingExperienceSettings": { "PreferredProtocol": "UDP" },
            }),
            user_settings: None,
            application_settings: None,
            streaming_experience_settings: Some(json!({ "PreferredProtocol": "UDP" })),
        },
        DefaultsRow {
            case: "the service's own protocol given explicitly",
            create: json!({
                "Name": "given-tcp",
                "StreamingExperienceSettings": { "PreferredProtocol": "TCP" },
            }),
            user_settings: None,
            application_settings: None,
            streaming_experience_settings: Some(json!({ "PreferredProtocol": "TCP" })),
        },
        DefaultsRow {
            case: "all three members given: nothing is settled by the service",
            create: json!({
                "Name": "given-all-three",
                "UserSettings": [
                    { "Action": "PRINTING_TO_LOCAL_DEVICE", "Permission": "DISABLED" },
                    { "Action": "DOMAIN_SMART_CARD_SIGNIN", "Permission": "ENABLED" },
                ],
                "ApplicationSettings": { "Enabled": true, "SettingsGroup": "group-b" },
                "StreamingExperienceSettings": { "PreferredProtocol": "UDP" },
            }),
            user_settings: Some(json!([
                { "Action": "PRINTING_TO_LOCAL_DEVICE", "Permission": "DISABLED" },
                { "Action": "DOMAIN_SMART_CARD_SIGNIN", "Permission": "ENABLED" },
            ])),
            application_settings: Some(json!({ "Enabled": true, "SettingsGroup": "group-b" })),
            streaming_experience_settings: Some(json!({ "PreferredProtocol": "UDP" })),
        },
    ]
}

/// Every modelled action, each carrying the OTHER permission than the one the service
/// settles for it — so a request that names every action is reported back as given rather
/// than as the service's own list.
fn flipped_user_settings() -> Value {
    Value::Array(
        DEFAULT_USER_SETTINGS
            .iter()
            .map(|(action, permission)| {
                let flipped = if *permission == "ENABLED" {
                    "DISABLED"
                } else {
                    "ENABLED"
                };
                json!({ "Action": action, "Permission": flipped })
            })
            .collect(),
    )
}

#[tokio::test]
async fn create_stack_reports_the_configuration_the_service_settles() {
    let emulator = Emulator::new();

    for row in defaults_rows() {
        let DefaultsRow {
            case,
            create,
            user_settings,
            application_settings,
            streaming_experience_settings,
        } = row;
        let name = create["Name"]
            .as_str()
            .expect("every row names a stack")
            .to_string();

        let created = emulator.ok(case, "CreateStack", create).await;
        let stack = &created["Stack"];

        let expected_user_settings = user_settings.unwrap_or_else(default_user_settings);
        assert_eq!(
            stack["UserSettings"], expected_user_settings,
            "case {case:?}: expected UserSettings {expected_user_settings}, got {stack}"
        );
        let expected_application_settings =
            application_settings.unwrap_or_else(default_application_settings);
        assert_eq!(
            stack["ApplicationSettings"], expected_application_settings,
            "case {case:?}: expected ApplicationSettings {expected_application_settings}, \
             got {stack}"
        );
        let expected_streaming = streaming_experience_settings
            .unwrap_or_else(default_streaming_experience_settings);
        assert_eq!(
            stack["StreamingExperienceSettings"], expected_streaming,
            "case {case:?}: expected StreamingExperienceSettings {expected_streaming}, \
             got {stack}"
        );

        // A member the service settles nothing for is ABSENT, not invented: the
        // storage-connector authentication's documented default is how a REQUEST that
        // omits it is interpreted, not a value the resource reports.
        assert!(
            stack.get("StorageConnectorAuthentication").is_none(),
            "case {case:?}: expected no StorageConnectorAuthentication key, got {stack}"
        );

        // ROUND TRIP: the reader answers from the stored record, so a client that did not
        // perform the create sees the same configuration.
        let described = emulator
            .ok(case, "DescribeStacks", json!({ "Names": [&name] }))
            .await;
        assert_eq!(
            described["Stacks"][0], *stack,
            "case {case:?}: expected DescribeStacks to report the created stack {stack}, \
             got {described}"
        );
    }
}

#[tokio::test]
async fn an_update_of_another_member_leaves_the_settled_configuration_in_place() {
    let emulator = Emulator::new();
    let case = "a stack updated on an unrelated member";

    emulator
        .ok(case, "CreateStack", json!({ "Name": "settled", "Description": "before" }))
        .await;

    let updated = emulator
        .ok(
            case,
            "UpdateStack",
            json!({ "Name": "settled", "Description": "after" }),
        )
        .await;
    let stack = &updated["Stack"];
    assert_eq!(
        stack["Description"],
        json!("after"),
        "case {case:?}: expected the update to replace Description, got {stack}"
    );
    for (member, expected) in [
        ("UserSettings", default_user_settings()),
        ("ApplicationSettings", default_application_settings()),
        (
            "StreamingExperienceSettings",
            default_streaming_experience_settings(),
        ),
    ] {
        assert_eq!(
            stack[member], expected,
            "case {case:?}: expected {member} {expected} to survive the update, got {stack}"
        );
    }

    let described = emulator
        .ok(case, "DescribeStacks", json!({ "Names": ["settled"] }))
        .await;
    assert_eq!(
        described["Stacks"][0], *stack,
        "case {case:?}: expected DescribeStacks to report the updated stack {stack}, \
         got {described}"
    );
}

#[tokio::test]
async fn a_replaced_configuration_member_is_reported_from_the_stored_record() {
    let emulator = Emulator::new();
    let case = "a settled member replaced by an update";

    emulator
        .ok(case, "CreateStack", json!({ "Name": "replaced" }))
        .await;

    let replacement = json!([{ "Action": "FILE_UPLOAD", "Permission": "DISABLED" }]);
    let updated = emulator
        .ok(
            case,
            "UpdateStack",
            json!({ "Name": "replaced", "UserSettings": replacement.clone() }),
        )
        .await;
    assert_eq!(
        updated["Stack"]["UserSettings"], replacement,
        "case {case:?}: expected UserSettings {replacement}, got {}",
        updated["Stack"]
    );

    let described = emulator
        .ok(case, "DescribeStacks", json!({ "Names": ["replaced"] }))
        .await;
    assert_eq!(
        described["Stacks"][0], updated["Stack"],
        "case {case:?}: expected DescribeStacks to report the updated stack {}, got {described}",
        updated["Stack"]
    );
}
