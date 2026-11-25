// SPDX-License-Identifier: CC0-1.0

//! What a `SettingId` no Amazon Web Services service team provisions is refused with: the
//! error CODE alone, with no message beside it.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target:
//! AmazonSSM.<Operation>` through the multi-service router.
//!
//! The table's rows differ along the WAY an id names no provisioned setting — an unknown
//! leaf, a namespace with no leaf, a setting ARN whose path names nothing, another
//! service's ARN, a spelling of a provisioned id in the wrong letter case — and each is
//! asserted against all three operations that resolve a `SettingId`, because they share
//! the one resolution.

use serde_json::{json, Value};

use crate::parameter_store::{call, error_code, error_message, router, ACCOUNT, REGION};

/// The operations that resolve a `SettingId`, with the body each takes for `id`.
fn reads(id: &str) -> [(&'static str, Value); 3] {
    [
        ("GetServiceSetting", json!({ "SettingId": id })),
        (
            "UpdateServiceSetting",
            json!({ "SettingId": id, "SettingValue": "true" }),
        ),
        ("ResetServiceSetting", json!({ "SettingId": id })),
    ]
}

/// RULE: a `SettingId` Systems Manager provisions no setting for is refused 400
/// `ServiceSettingNotFound` carrying NO message — the code alone states the setting is not
/// one the service holds — while a refusal of the REQUEST itself still says what is wrong
/// with it.
#[tokio::test]
async fn an_unprovisioned_setting_is_refused_with_the_code_alone() {
    let router = router();
    let unprovisioned = [
        (
            "an unknown leaf under a provisioned namespace",
            "/ssm/parameter-store/does-not-exist".to_string(),
        ),
        (
            "a namespace with no leaf",
            "/ssm/parameter-store".to_string(),
        ),
        (
            "a provisioned id in the wrong letter case",
            "/SSM/Parameter-Store/High-Throughput-Enabled".to_string(),
        ),
        (
            "a setting ARN whose path names nothing",
            format!("arn:aws:ssm:{REGION}:{ACCOUNT}:servicesetting/ssm/parameter-store/nope"),
        ),
        (
            "another service's ARN",
            format!("arn:aws:s3:{REGION}:{ACCOUNT}:servicesetting/ssm/opsinsights/opscenter"),
        ),
    ];
    for (case, id) in unprovisioned {
        for (operation, body) in reads(&id) {
            let (status, response) = call(&router, operation, &body).await;
            assert_eq!(
                status, 400,
                "case {case:?} / {operation}: expected the refusal of an unprovisioned \
                 setting, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "ServiceSettingNotFound",
                "case {case:?} / {operation}: expected ServiceSettingNotFound, got {response}"
            );
            assert_eq!(
                error_message(&response),
                "",
                "case {case:?} / {operation}: the refusal carries no message, got {response}"
            );
        }
    }

    // A request the MODEL cannot carry is a different refusal, and that one does say what
    // is wrong with it — so the empty message above belongs to this error class and is not
    // this service dropping every diagnostic.
    for (operation, body) in reads("") {
        let (status, response) = call(&router, operation, &body).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {operation}: an id the model cannot carry is an invalid request, got \
             {status} {response}"
        );
        assert!(
            !error_message(&response).is_empty(),
            "case {operation}: a refusal of the request itself must say why, got {response}"
        );
    }

    // And a setting a service team DOES provision is still answered, so the refusals above
    // are about the id and not about the operation refusing everything.
    let (status, response) = call(
        &router,
        "GetServiceSetting",
        &json!({ "SettingId": "/ssm/parameter-store/high-throughput-enabled" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a provisioned setting is still answered, got {status} {response}"
    );
    assert_eq!(
        response["ServiceSetting"]["SettingId"],
        json!("/ssm/parameter-store/high-throughput-enabled"),
        "the reading reports the setting it was asked for, got {response}"
    );
}
