// SPDX-License-Identifier: CC0-1.0

//! `UpdateServiceSetting` OVERWRITING a setting, again and again: the value a later read
//! reports is the one the account asked for LAST, and the modification the write recorded
//! is the one that read reports.
//!
//! "You can't create a new SettingId, but you can overwrite the default value", so an
//! update is a repeatable overwrite of one catalogue entry rather than a create: each
//! accepted write replaces the value the setting held, records the principal that wrote it
//! and the instant it was written, and answers a result the model gives no members — and
//! no write makes a setting's `LastModifiedDate` move backwards, whatever order the values
//! arrive in.
//!
//! `all/service_settings.rs` covers the slice's other rules (which ids resolve, which
//! values a setting accepts, per-Region holding, the store's bound). This suite is about
//! the SEQUENCE: what a second, third and repeated write to the same setting leaves
//! behind.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router through this crate's one test harness
//! (`all/parameter_store.rs`). Nothing reaches into the state struct, so a value that is
//! not persisted cannot pass: every assertion reads the setting back.
//!
//! The suite is a table whose rows differ along what the rule generalises over: WHICH
//! setting of the provisioned catalogue, how many successive writes it takes, the form of
//! the values written (a documented word, a boolean, a free-text name), and whether the
//! last write repeats the value already held.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ssm_amazonssm::state::service_settings::{
    modifying_user, SettingDefinition, SETTINGS, STATUS_CUSTOMIZED,
};

use super::parameter_store::{call, router, REGION};

/// The values written to a setting the API documents NO value list for — those settings
/// take free text (`UpdateServiceSettingRequest$SettingValue` states a list only for the
/// settings that take a fixed word), so the rows differ in the form of the text persisted:
/// a CloudWatch Logs log-group path, an IAM role name, a boolean word.
const FREE_TEXT_SPELLINGS: &[&str] = &[
    "/aws/ssm/automation-customer-scripts",
    "SSMDefaultEC2InstanceManagementRole",
    "true",
];

/// The successive values one row writes to `setting`.
///
/// A setting the API documents values for is walked through EVERY value it documents, in
/// the order the member lists them, so the row's length and the forms it spans come from
/// the catalogue rather than from a value written into this test; a setting that documents
/// none is walked through [`FREE_TEXT_SPELLINGS`].
fn value_sequence(setting: &SettingDefinition) -> Vec<String> {
    if setting.values.is_empty() {
        FREE_TEXT_SPELLINGS
            .iter()
            .map(|value| (*value).to_string())
            .collect()
    } else {
        setting
            .values
            .iter()
            .map(|value| (*value).to_string())
            .collect()
    }
}

/// `UpdateServiceSetting` for `id`, returning `(status, body)`.
async fn update(router: &MultiServiceRouter, id: &str, value: &str) -> (u16, Value) {
    call(
        router,
        "UpdateServiceSetting",
        &json!({ "SettingId": id, "SettingValue": value }),
    )
    .await
}

/// The `ServiceSetting` a successful `GetServiceSetting` reports for `id`.
async fn get_ok(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, body) = call(router, "GetServiceSetting", &json!({ "SettingId": id })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetServiceSetting {id:?} must succeed, got {status} {body}"
    );
    body["ServiceSetting"].clone()
}

/// The `LastModifiedDate` of a reported setting. `awsJson1_1` renders a timestamp as
/// epoch seconds, so the reading is a number and two readings are comparable.
fn modified_at(case: &str, reading: &Value) -> f64 {
    reading["LastModifiedDate"].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: a setting reports a LastModifiedDate, got {reading}")
    })
}

/// Writing a setting again and again leaves the value written LAST, each write recording
/// the modification the next read reports.
#[tokio::test]
async fn successive_writes_leave_the_value_written_last() {
    let router = router();
    let modifier = modifying_user();
    for setting in SETTINGS {
        let case = setting.id;
        let sequence = value_sequence(setting);
        assert!(
            !sequence.is_empty(),
            "case {case:?}: the table must write at least one value"
        );

        // The provisioned reading dates the setting before any write, so no write may
        // report a modification instant earlier than it.
        let provisioned = get_ok(&router, case, setting.id).await;
        let mut previous_instant = modified_at(case, &provisioned);

        // The last value is written TWICE: an overwrite with the value already held is
        // still an accepted overwrite, not a no-op the store may reject or an error.
        let last = sequence
            .last()
            .expect("the sequence is not empty")
            .to_string();
        for (step, value) in sequence.iter().chain(std::iter::once(&last)).enumerate() {
            let (status, body) = update(&router, setting.id, value).await;
            assert_eq!(
                status, 200,
                "case {case:?} / write {step} of {value:?}: the overwrite must succeed, got \
                 {status} {body}"
            );
            // `UpdateServiceSettingResult` declares no members
            // (`smithy_query(query="operation", operation="UpdateServiceSetting")`), so the
            // success reports nothing of its own.
            let members: Vec<&String> = body
                .as_object()
                .map(|fields| fields.keys().collect())
                .unwrap_or_default();
            assert!(
                members.is_empty(),
                "case {case:?} / write {step}: the result models no members, got {body}"
            );

            let reading = get_ok(&router, case, setting.id).await;
            assert_eq!(
                reading["SettingValue"],
                json!(value),
                "case {case:?} / write {step}: expected the value this write stored, got {reading}"
            );
            assert_eq!(
                reading["Status"],
                json!(STATUS_CUSTOMIZED),
                "case {case:?} / write {step}: a setting the account wrote is customized, got \
                 {reading}"
            );
            assert_eq!(
                reading["LastModifiedUser"],
                json!(modifier),
                "case {case:?} / write {step}: expected the writing principal as the modifier, \
                 got {reading}"
            );
            let instant = modified_at(case, &reading);
            assert!(
                instant >= previous_instant,
                "case {case:?} / write {step}: a modification instant must not move backwards, \
                 got {instant} after {previous_instant} in {reading}"
            );
            previous_instant = instant;
        }

        // What stands at the end is the value written last — and the earlier values the
        // row wrote are gone, so the store holds one value per setting rather than a
        // history of them.
        let settled = get_ok(&router, case, setting.id).await;
        assert_eq!(
            settled["SettingValue"],
            json!(last),
            "case {case:?}: expected the value written last, got {settled}"
        );
        assert_eq!(
            settled["SettingId"],
            json!(setting.id),
            "case {case:?}: expected the documented setting id, got {settled}"
        );
        assert_eq!(
            settled["ARN"],
            json!(format!(
                "arn:aws:ssm:{REGION}:{}:servicesetting{}",
                engine::EMULATOR_ACCOUNT_ID, setting.id
            )),
            "case {case:?}: expected the setting's ARN at the signed Region, got {settled}"
        );
    }
}

/// The value one setting is overwritten to is the one IT holds: writing every setting of
/// the catalogue in turn leaves each at its own value, not at the one written last of all.
///
/// A sequence of writes across the catalogue is what distinguishes a store keyed by the
/// setting from one that keeps a single most-recent value, so this row is the whole
/// catalogue written in one pass and then read back in a second.
#[tokio::test]
async fn each_setting_keeps_the_value_written_to_it() {
    let router = router();
    let mut expected: Vec<(&str, String)> = Vec::new();

    for setting in SETTINGS {
        let value = value_sequence(setting)
            .last()
            .expect("the sequence is not empty")
            .to_string();
        let (status, body) = update(&router, setting.id, &value).await;
        assert_eq!(
            status, 200,
            "case {:?}: the overwrite must succeed, got {status} {body}",
            setting.id
        );
        expected.push((setting.id, value));
    }

    for (id, value) in &expected {
        let reading = get_ok(&router, id, id).await;
        assert_eq!(
            reading["SettingValue"],
            json!(value),
            "case {id:?}: expected the value written to this setting, got {reading}"
        );
        assert_eq!(
            reading["Status"],
            json!(STATUS_CUSTOMIZED),
            "case {id:?}: a setting the account wrote is customized, got {reading}"
        );
    }
}
