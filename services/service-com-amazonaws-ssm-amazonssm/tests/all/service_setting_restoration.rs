// SPDX-License-Identifier: CC0-1.0

//! RESTORING a service setting: what `ResetServiceSetting` leaves behind, and what every
//! later `GetServiceSetting` of the same setting reports.
//!
//! Three rules, each asserted over a SET of the provisioned catalogue rather than over the
//! one setting a recorded call happened to name:
//!
//! 1. A setting back at the value its service team provisioned is no longer overwritten,
//!    and `ServiceSetting$LastModifiedUser` is "populated only if the setting value was
//!    overwritten", so a restored setting names the provisioning service team as its last
//!    modifier — not the principal that asked for the restore — while a setting the
//!    account's own value stands on names that principal.
//! 2. A reset that would move a setting off the account's value only once the service team
//!    approves it is a change request: the account's value still stands and the setting
//!    reports `PendingUpdate`, "a setting change request is pending approval".
//! 3. The value each setting reports before the account changes anything is the one its
//!    service team provisioned, and a reset restores exactly that value — so the reading a
//!    fresh account gets and the reading a reset account gets are the same one.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router through this crate's one test harness
//! (`all/parameter_store.rs`). Nothing reaches into the state struct, so every assertion
//! is made on a response that was computed from what an earlier call persisted.
//!
//! `all/service_settings.rs` covers the slice's other rules (which ids resolve, which
//! values a setting accepts, per-Region holding, the store's bound) and
//! `all/service_setting_overwrite.rs` the sequence of overwrites.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ssm_amazonssm::state::service_settings::{
    modifying_user, SettingDefinition, PROVISIONING_USER, SETTINGS, STATUS_CUSTOMIZED,
    STATUS_DEFAULT, STATUS_PENDING_UPDATE,
};

use super::parameter_store::{call, call_in_region, router, REGION};
// The slice's own table helpers live with the suite that introduced them: which value a
// setting is customized to, the ARN spelling of a setting id, and the asserted read.
use super::service_settings::{custom_value, get_ok, setting_arn};

/// The value each provisioned setting reports for an account that has changed nothing,
/// written out HERE rather than read from the catalogue, so a change to a provisioned
/// value fails this suite.
///
/// The spellings are the ones `UpdateServiceSettingRequest$SettingValue` documents for
/// each setting
/// (`smithy_query(query="doc", shape="com.amazonaws.ssm#UpdateServiceSettingRequest", member="SettingValue")`);
/// WHICH of a setting's documented values its service team provisions is stated neither by
/// the model nor by the API documentation, so each one here is what Systems Manager is
/// observed to report for an unchanged account: a feature an account pays for is
/// provisioned off, a permission the account restricts is provisioned allowing what it
/// governs, and a setting that names a resource reports the sentinel for "none named".
const PROVISIONED: &[(&str, &str)] = &[
    ("/ssm/appmanager/appmanager-enabled", "False"),
    ("/ssm/automation/customer-script-log-destination", "$None"),
    ("/ssm/automation/customer-script-log-group-name", "$None"),
    ("/ssm/automation/enable-adaptive-concurrency", "false"),
    ("/ssm/documents/console/public-sharing-permission", "Enable"),
    ("/ssm/managed-instance/activation-tier", "standard"),
    (
        "/ssm/managed-instance/default-ec2-instance-management-role",
        "$None",
    ),
    ("/ssm/opsinsights/opscenter", "Disabled"),
    ("/ssm/parameter-store/default-parameter-tier", "Standard"),
    ("/ssm/parameter-store/high-throughput-enabled", "false"),
];

/// The `ServiceSetting` a successful `ResetServiceSetting` reports for `id`.
async fn reset_ok(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, body) = call(router, "ResetServiceSetting", &json!({ "SettingId": id })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ResetServiceSetting {id:?} must succeed, got {status} {body}"
    );
    body["ServiceSetting"].clone()
}

/// `UpdateServiceSetting`, asserted to succeed.
async fn update_ok(router: &MultiServiceRouter, case: &str, id: &str, value: &str) {
    let (status, body) = call(
        router,
        "UpdateServiceSetting",
        &json!({ "SettingId": id, "SettingValue": value }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: UpdateServiceSetting {id:?}={value:?} must succeed, got {status} {body}"
    );
}

/// Every setting whose restore is immediate: the restored setting names the provisioning
/// service team as its last modifier, and only a setting the account's own value stands on
/// names the calling principal.
///
/// The rows are every catalogue entry a reset restores at once, so they differ along the
/// form of the value held (a boolean word, a documented word, a free-text name), the
/// spelling the calls address the setting by (the documented path or its ARN), and whether
/// the setting documents a value list at all.
#[tokio::test]
async fn a_restored_setting_names_the_provisioning_team_as_its_modifier() {
    let router = router();
    let caller = modifying_user();
    let restorable: Vec<(usize, &SettingDefinition)> = SETTINGS
        .iter()
        .enumerate()
        .filter(|(_, setting)| !setting.reset_awaits_approval)
        .collect();
    assert!(
        restorable.len() > 1,
        "the catalogue must hold more than one setting a reset restores at once"
    );

    for (index, setting) in restorable {
        let case = setting.id;
        // Half the rows address the setting by its documented path and half by its ARN.
        let addressed = if index % 2 == 0 {
            setting.id.to_string()
        } else {
            setting_arn(REGION, setting.id)
        };
        let custom = custom_value(index, setting);

        update_ok(&router, case, &addressed, &custom).await;
        let customized = get_ok(&router, case, &addressed).await;
        assert_eq!(
            customized["LastModifiedUser"],
            json!(caller),
            "case {case:?}: an overwritten setting names the principal that overwrote it, got \
             {customized}"
        );
        assert_eq!(
            customized["Status"],
            json!(STATUS_CUSTOMIZED),
            "case {case:?}: an overwritten setting is customized, got {customized}"
        );

        let restored = reset_ok(&router, case, &addressed).await;
        assert_eq!(
            restored["LastModifiedUser"],
            json!(PROVISIONING_USER),
            "case {case:?}: a restored setting names the provisioning service team, got {restored}"
        );
        assert_eq!(
            restored["Status"],
            json!(STATUS_DEFAULT),
            "case {case:?}: a restored setting is back at its provisioned value, got {restored}"
        );
        assert_ne!(
            restored["LastModifiedUser"],
            json!(caller),
            "case {case:?}: a restore must not be attributed to the principal that asked for it, \
             got {restored}"
        );

        // The read after the reset reports what the reset answered, so the restored
        // modifier is persisted rather than only serialized into that one response.
        let after = get_ok(&router, case, &addressed).await;
        assert_eq!(
            after, restored,
            "case {case:?}: the read must report what the reset answered, got {after} after \
             {restored}"
        );

        // And the account can take the setting back: a later overwrite names the calling
        // principal again, so the modifier follows who owns the value rather than being
        // fixed either way.
        update_ok(&router, case, &addressed, &custom).await;
        let again = get_ok(&router, case, &addressed).await;
        assert_eq!(
            again["LastModifiedUser"],
            json!(caller),
            "case {case:?}: an overwrite after a restore names the calling principal, got {again}"
        );
    }
}

/// A setting nobody has modified names the provisioning service team too — in every
/// Region a request can sign for, because nothing is recorded for a setting no call has
/// modified.
#[tokio::test]
async fn an_unmodified_setting_names_the_provisioning_team_in_every_region() {
    let router = router();
    // Rows along the axis the store is keyed by: the Region the request signs for.
    for region in [REGION, "eu-west-1", "ap-southeast-2"] {
        for setting in SETTINGS {
            let case = format!("{region} {}", setting.id);
            let (status, body) = call_in_region(
                &router,
                region,
                "GetServiceSetting",
                &json!({ "SettingId": setting.id }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the read must succeed, got {status} {body}"
            );
            let reading = &body["ServiceSetting"];
            assert_eq!(
                reading["LastModifiedUser"],
                json!(PROVISIONING_USER),
                "case {case:?}: an unmodified setting names the provisioning service team, got \
                 {reading}"
            );
            assert_eq!(
                reading["Status"],
                json!(STATUS_DEFAULT),
                "case {case:?}: an unmodified setting is at its provisioned value, got {reading}"
            );
        }
    }
}

/// A reset of a setting whose move off the account's value awaits the service team's
/// approval leaves that value standing and reports `PendingUpdate`.
///
/// The rows are every catalogue entry marked that way, each walked through EVERY value it
/// documents that differs from the provisioned one, so the table spans the values such a
/// setting can be customized to rather than the one a recorded call used; the final rows
/// are the two readings for which such a reset is NOT a pending change — a setting at its
/// provisioned value, and one whose own value is that provisioned value in another letter
/// case.
#[tokio::test]
async fn a_reset_that_awaits_approval_holds_the_accounts_value_pending() {
    let router = router();
    let caller = modifying_user();
    let awaiting: Vec<&SettingDefinition> = SETTINGS
        .iter()
        .filter(|setting| setting.reset_awaits_approval)
        .collect();
    assert!(
        !awaiting.is_empty(),
        "the catalogue must hold a setting whose reset awaits approval"
    );

    for setting in awaiting {
        for custom in setting
            .values
            .iter()
            .filter(|value| !value.eq_ignore_ascii_case(setting.default_value))
        {
            let case = format!("{} = {custom}", setting.id);
            update_ok(&router, &case, setting.id, custom).await;

            let answered = reset_ok(&router, &case, setting.id).await;
            assert_eq!(
                answered["SettingValue"],
                json!(custom),
                "case {case:?}: the account's value must still stand while the change is pending, \
                 got {answered}"
            );
            assert_eq!(
                answered["Status"],
                json!(STATUS_PENDING_UPDATE),
                "case {case:?}: a reset awaiting approval reports PendingUpdate, got {answered}"
            );
            assert_eq!(
                answered["LastModifiedUser"],
                json!(caller),
                "case {case:?}: the overwrite still in force names its principal, got {answered}"
            );

            // The pending change is what is PERSISTED: the read after it reports the same
            // pair, and a client that resets again is told the same thing rather than
            // having the change applied the second time.
            let after = get_ok(&router, &case, setting.id).await;
            assert_eq!(
                after, answered,
                "case {case:?}: the read must report what the reset answered, got {after} after \
                 {answered}"
            );
            let twice = reset_ok(&router, &case, setting.id).await;
            assert_eq!(
                twice, answered,
                "case {case:?}: a second reset must report the pending change, got {twice} after \
                 {answered}"
            );

            // The account can still overwrite it: a pending change does not wedge the
            // setting at the value it holds.
            update_ok(&router, &case, setting.id, setting.default_value).await;
            let overwritten = get_ok(&router, &case, setting.id).await;
            assert_eq!(
                overwritten["SettingValue"],
                json!(setting.default_value),
                "case {case:?}: expected the value the overwrite stored, got {overwritten}"
            );
            assert_eq!(
                overwritten["Status"],
                json!(STATUS_CUSTOMIZED),
                "case {case:?}: a value the account wrote is customized, got {overwritten}"
            );

            // A reset that does not move the setting's value is not a pending change: the
            // value the account wrote IS the provisioned one, so the setting goes back to
            // the provisioned reading at once — whatever letter case it was written in.
            let settled = reset_ok(&router, &case, setting.id).await;
            assert_eq!(
                settled["Status"],
                json!(STATUS_DEFAULT),
                "case {case:?}: a reset that moves no value restores at once, got {settled}"
            );
            assert_eq!(
                settled["LastModifiedUser"],
                json!(PROVISIONING_USER),
                "case {case:?}: the restored setting names the provisioning team, got {settled}"
            );

            update_ok(
                &router,
                &case,
                setting.id,
                &setting.default_value.to_ascii_uppercase(),
            )
            .await;
            let other_case = reset_ok(&router, &case, setting.id).await;
            assert_eq!(
                other_case["SettingValue"],
                json!(setting.default_value),
                "case {case:?}: the provisioned value in another letter case is the provisioned \
                 value, got {other_case}"
            );
            assert_eq!(
                other_case["Status"],
                json!(STATUS_DEFAULT),
                "case {case:?}: a reset that moves no value restores at once, got {other_case}"
            );

            // A reset of a setting already at its provisioned value restores nothing, so
            // it reports the same reading again.
            let idempotent = reset_ok(&router, &case, setting.id).await;
            assert_eq!(
                idempotent, other_case,
                "case {case:?}: resetting a provisioned setting must report the same reading, got \
                 {idempotent} after {other_case}"
            );
        }
    }
}

/// The value a setting reports before the account changes anything is the value its
/// service team provisioned, and a reset restores exactly that value.
///
/// The rows are every entry of [`PROVISIONED`] — the whole catalogue, each provisioned
/// value written out independently of the code under test — so a flipped provisioned value
/// fails here whichever setting it belongs to.
#[tokio::test]
async fn every_setting_reports_and_is_restored_to_its_provisioned_value() {
    let router = router();
    assert_eq!(
        PROVISIONED.len(),
        SETTINGS.len(),
        "the table must cover every provisioned setting, got {} rows for {} settings",
        PROVISIONED.len(),
        SETTINGS.len()
    );

    for (index, (id, provisioned)) in PROVISIONED.iter().enumerate() {
        let setting = SETTINGS
            .iter()
            .find(|setting| setting.id == *id)
            .unwrap_or_else(|| panic!("case {id:?}: the catalogue must provision this setting"));

        let fresh = get_ok(&router, id, id).await;
        assert_eq!(
            fresh["SettingValue"],
            json!(provisioned),
            "case {id:?}: expected the provisioned value {provisioned:?}, got {fresh}"
        );
        assert_eq!(
            fresh["Status"],
            json!(STATUS_DEFAULT),
            "case {id:?}: an unchanged setting is at its provisioned value, got {fresh}"
        );

        // Overwrite it, then restore it: what comes back is the same value the unchanged
        // account read, so the restore resolves through the provisioned value rather than
        // through the request.
        let custom = custom_value(index, setting);
        update_ok(&router, id, id, &custom).await;
        if setting.reset_awaits_approval {
            // Such a setting is restored only once its change request is approved, which
            // no API operation does; writing the provisioned value back is how an account
            // reaches it, and a reset then restores the provisioned reading.
            update_ok(&router, id, id, provisioned).await;
        }
        let restored = reset_ok(&router, id, id).await;
        assert_eq!(
            restored["SettingValue"],
            json!(provisioned),
            "case {id:?}: expected the provisioned value {provisioned:?} restored, got {restored}"
        );
        assert_eq!(
            restored["Status"],
            json!(STATUS_DEFAULT),
            "case {id:?}: a restored setting reports the provisioned status, got {restored}"
        );
        assert_eq!(
            restored["SettingId"],
            json!(id),
            "case {id:?}: expected the documented setting id, got {restored}"
        );
        assert_eq!(
            restored["ARN"],
            json!(setting_arn(REGION, id)),
            "case {id:?}: expected the setting's ARN at the signed Region, got {restored}"
        );
    }
}
