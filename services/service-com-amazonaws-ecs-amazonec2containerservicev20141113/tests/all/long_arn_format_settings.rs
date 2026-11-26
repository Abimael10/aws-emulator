// SPDX-License-Identifier: CC0-1.0

//! The ACCOUNT SETTINGS that choose the ARN and resource-ID format, driven end to end over
//! the shared wire harness in [`crate::ecs_wire`].
//!
//! The rule under test:
//!
//! > The long ARN and resource-ID format is permanently in effect for an account, so a write
//! > that would turn one of those settings off is refused with `InvalidParameterException`
//! > and the one sentence Amazon ECS states for the class, and the setting is left `enabled`.
//! > The rule is about the SETTINGS that choose that format — the `SettingName` members whose
//! > names end in `LongArnFormat`, one per resource type — and about every value that states
//! > the format is not in effect (`PutAccountSetting$value`: "Accepted values are `enabled`,
//! > `disabled`, `enhanced`, `on`, and `off`"), whichever of the two writes carries it
//! > (`PutAccountSetting` for one principal, `PutAccountSettingDefault` for the account). A
//! > write that turns one of them ON is accepted, and every other toggle setting still takes
//! > both of its values.
//!
//! The table spans the three format settings, both spellings of off and both spellings of
//! on, and both write operations; a setting that is NOT one of them is the control row.
//! Nothing here reaches into the state struct: what each write left is read back through
//! `ListAccountSettings`.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{list, ok, refusal, service};

/// The refusal class a write Amazon ECS will not apply is reported under.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The sentence such a write earns (`state`'s `LONG_ARN_FORMAT_PERMANENT_MESSAGE`).
const CANNOT_DISABLE_LONG_ARN: &str = "You can no longer disable Long Arn settings";

/// The settings that choose the ARN and resource-ID format, from `SettingName`
/// (`smithy_query(query="shape",shape="SettingName")`): the three members whose names end in
/// `LongArnFormat`, one per resource type.
const LONG_ARN_SETTINGS: [&str; 3] = [
    "serviceLongArnFormat",
    "taskLongArnFormat",
    "containerInstanceLongArnFormat",
];

/// The two writes of an account setting: the account-wide default and one principal's own.
const WRITES: [&str; 2] = ["PutAccountSettingDefault", "PutAccountSetting"];

/// What `ListAccountSettings` reports for `name` — the effective value the account holds.
async fn effective_value(router: &MultiServiceRouter, name: &str, case: &str) -> String {
    let listed = ok(
        router,
        "ListAccountSettings",
        json!({ "name": name, "effectiveSettings": true }),
        case,
    )
    .await;
    list(&listed, "settings", case)
        .into_iter()
        .find(|setting| setting["name"] == json!(name))
        .and_then(|setting| setting["value"].as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("case {case:?}: expected a value for {name}"))
}

#[tokio::test]
async fn a_long_arn_format_setting_can_no_longer_be_turned_off() {
    let router = service();

    // (the value the write carries, whether it turns the format off)
    let values: [(&str, bool); 4] = [
        ("disabled", true),
        ("off", true),
        ("enabled", false),
        ("on", false),
    ];
    for setting in LONG_ARN_SETTINGS {
        for (value, turns_off) in values {
            for write in WRITES {
                let case = format!("{write} {setting}={value}");
                let request = json!({ "name": setting, "value": value });
                if turns_off {
                    let (code, message) = refusal(&router, write, request, &case).await;
                    assert_eq!(
                        code, INVALID_PARAMETER,
                        "case {case:?}: expected an {INVALID_PARAMETER}, got {code:?} with \
                         {message:?}"
                    );
                    assert_eq!(
                        message, CANNOT_DISABLE_LONG_ARN,
                        "case {case:?}: expected the fixed sentence \
                         {CANNOT_DISABLE_LONG_ARN:?}, got {message:?}"
                    );
                } else {
                    let answered = ok(&router, write, request, &case).await;
                    assert_eq!(
                        answered["setting"]["value"], json!(value),
                        "case {case:?}: a write that leaves the format on reports the value \
                         it stored, got {answered}"
                    );
                }
                // Whatever the write did, the format is still on: a refused write stored
                // nothing and an accepted one left it enabled.
                let effective = effective_value(&router, setting, &case).await;
                assert!(
                    effective == "enabled" || effective == "on",
                    "case {case:?}: expected {setting} to remain on, got {effective:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn another_toggle_setting_still_takes_both_of_its_values() {
    let router = service();
    // The control: the rule belongs to the format settings, so a setting that is not one of
    // them is turned off and back on through the same two operations.
    for write in WRITES {
        for value in ["disabled", "enabled"] {
            let case = format!("{write} awsvpcTrunking={value}");
            let answered = ok(
                &router,
                write,
                json!({ "name": "awsvpcTrunking", "value": value }),
                &case,
            )
            .await;
            assert_eq!(
                answered["setting"]["value"], json!(value),
                "case {case:?}: expected the value the write stored back, got {answered}"
            );
            let effective = effective_value(&router, "awsvpcTrunking", &case).await;
            assert_eq!(
                effective, value,
                "case {case:?}: expected {value:?} to be what the account holds, got \
                 {effective:?}"
            );
        }
    }
}
