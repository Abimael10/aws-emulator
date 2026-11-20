// SPDX-License-Identifier: CC0-1.0

//! Promoting an identity to an `AppInstanceAdmin`, end to end over HTTP.
//!
//! The RULE pinned here: `CreateAppInstanceAdmin` promotes an identity OF the named app
//! instance and persists the promotion, so `DescribeAppInstanceAdmin` reads the same
//! record back — the promoted identity, the app instance it administers and the instant
//! the promotion was recorded — while an ARN that is not an identity of that app
//! instance, an `AppInstanceUser` the service does not hold, and an app instance that
//! does not exist are all refused rather than recorded.
//!
//! The table spans what the rule generalises over: the kind of identity promoted (a
//! stored `AppInstanceUser`, an `AppInstanceBot` whose record this service does not keep),
//! each way the request can name something absent, and the number of times the same
//! promotion is requested.
//!
//! The second test here pins the other half of that rule: only the two identity KINDS of
//! the app instance — an `AppInstanceUser` and an `AppInstanceBot` — may be promoted, so an
//! ARN that hangs under the same app instance under any other segment names no identity and
//! is refused.

use serde_json::{json, Value};

use crate::app_instance_lifecycle::{uri_encode, Emulator};

/// One row of the promotion table: what is promoted over which app instance, and the
/// status the service answers with.
#[derive(Debug)]
struct PromotionCase {
    label: &'static str,
    /// How the admin ARN is built from the app instance ARN and the stored user's ARN.
    admin_arn: fn(&str, &str) -> String,
    /// How the app instance ARN is built from the stored one.
    app_instance_arn: fn(&str) -> String,
    expected_status: u16,
    /// The error `Code` the refusal carries, for a row that is refused.
    expected_code: Option<&'static str>,
    /// Whether the promoted identity's `Name` is one this service holds.
    named: bool,
}

async fn promote(emulator: &Emulator, app_instance_arn: &str, admin_arn: &str) -> (u16, Value) {
    emulator
        .call(
            "POST",
            &format!("/app-instances/{}/admins", uri_encode(app_instance_arn)),
            Some(json!({ "AppInstanceAdminArn": admin_arn })),
        )
        .await
}

async fn describe_admin(
    emulator: &Emulator,
    app_instance_arn: &str,
    admin_arn: &str,
) -> (u16, Value) {
    emulator
        .call(
            "GET",
            &format!(
                "/app-instances/{}/admins/{}",
                uri_encode(app_instance_arn),
                uri_encode(admin_arn)
            ),
            None,
        )
        .await
}

#[tokio::test]
async fn an_app_instance_admin_is_promoted_and_read_back() {
    let cases = [
        PromotionCase {
            label: "a stored AppInstanceUser of the app instance",
            admin_arn: |_instance, user| user.to_string(),
            app_instance_arn: |instance| instance.to_string(),
            expected_status: 201,
            expected_code: None,
            named: true,
        },
        PromotionCase {
            label: "an AppInstanceBot of the app instance, whose record this service does not keep",
            admin_arn: |instance, _user| format!("{instance}/bot/helper"),
            app_instance_arn: |instance| instance.to_string(),
            expected_status: 201,
            expected_code: None,
            named: false,
        },
        PromotionCase {
            label: "an AppInstanceUser the service does not hold",
            admin_arn: |instance, _user| format!("{instance}/user/nobody"),
            app_instance_arn: |instance| instance.to_string(),
            expected_status: 400,
            expected_code: Some("NotFound"),
            named: false,
        },
        PromotionCase {
            label: "an identity of another app instance",
            admin_arn: |_instance, _user| {
                "arn:aws:chime:us-west-2:111122223333:app-instance/other/user/alice".to_string()
            },
            app_instance_arn: |instance| instance.to_string(),
            expected_status: 400,
            expected_code: Some("BadRequest"),
            named: false,
        },
        PromotionCase {
            label: "an app instance that does not exist",
            admin_arn: |instance, _user| format!("{instance}/user/alice"),
            app_instance_arn: |_instance| {
                "arn:aws:chime:us-west-2:111122223333:app-instance/absent".to_string()
            },
            expected_status: 400,
            expected_code: Some("NotFound"),
            named: false,
        },
    ];

    let emulator = Emulator::new();
    let instance = emulator.app_instance("admins", "admins-token").await;
    let alice = emulator.user(&instance, "alice", "alice-token").await;

    for case in &cases {
        let app_instance_arn = (case.app_instance_arn)(&instance);
        let admin_arn = (case.admin_arn)(&instance, &alice);
        let (status, body) = promote(&emulator, &app_instance_arn, &admin_arn).await;
        assert_eq!(
            status, case.expected_status,
            "case {:?}: CreateAppInstanceAdmin must answer {}, got {status} {body:?}",
            case.label, case.expected_status
        );
        if let Some(code) = case.expected_code {
            assert_eq!(
                body["Code"].as_str(),
                Some(code),
                "case {:?}: wrong refusal Code, body {body:?}",
                case.label
            );
            // A refused promotion is not recorded, so nothing reads it back: the read of
            // a grant this service holds no record of is the permanent refusal.
            let (described, described_body) =
                describe_admin(&emulator, &app_instance_arn, &admin_arn).await;
            assert_eq!(
                (described, described_body["Code"].as_str()),
                (403, Some("Forbidden")),
                "case {:?}: a refused promotion must not be readable, got {described} {described_body:?}",
                case.label
            );
            continue;
        }

        assert_eq!(
            body["AppInstanceArn"].as_str(),
            Some(app_instance_arn.as_str()),
            "case {:?}: wrong AppInstanceArn in the promotion, body {body:?}",
            case.label
        );
        assert_eq!(
            body["AppInstanceAdmin"]["Arn"].as_str(),
            Some(admin_arn.as_str()),
            "case {:?}: wrong promoted identity, body {body:?}",
            case.label
        );

        let (described, described_body) =
            describe_admin(&emulator, &app_instance_arn, &admin_arn).await;
        assert_eq!(
            described, 200,
            "case {:?}: DescribeAppInstanceAdmin must answer 200, got {described} {described_body:?}",
            case.label
        );
        let admin = &described_body["AppInstanceAdmin"];
        assert_eq!(
            admin["Admin"]["Arn"].as_str(),
            Some(admin_arn.as_str()),
            "case {:?}: wrong Admin.Arn, got {:?}",
            case.label,
            admin["Admin"]
        );
        assert_eq!(
            admin["AppInstanceArn"].as_str(),
            Some(app_instance_arn.as_str()),
            "case {:?}: wrong AppInstanceArn, got {:?}",
            case.label,
            admin["AppInstanceArn"]
        );
        assert!(
            admin["CreatedTimestamp"].is_number(),
            "case {:?}: CreatedTimestamp must be set, got {:?}",
            case.label,
            admin["CreatedTimestamp"]
        );
        let expected_name = case.named.then_some("alice");
        assert_eq!(
            admin["Admin"].get("Name").and_then(Value::as_str),
            expected_name,
            "case {:?}: wrong Admin.Name, got {:?}",
            case.label,
            admin["Admin"].get("Name")
        );

        // Promotion is a SET: repeating the request neither duplicates the record nor
        // moves the instant it reports.
        let first_timestamp = admin["CreatedTimestamp"].clone();
        for repeat in 1..=2 {
            let (again, again_body) = promote(&emulator, &app_instance_arn, &admin_arn).await;
            assert_eq!(
                again, 201,
                "case {:?}: repeat {repeat} of the promotion must answer 201, got {again} {again_body:?}",
                case.label
            );
        }
        let (_, after) = describe_admin(&emulator, &app_instance_arn, &admin_arn).await;
        assert_eq!(
            after["AppInstanceAdmin"]["CreatedTimestamp"], first_timestamp,
            "case {:?}: a repeated promotion must keep the first CreatedTimestamp, got {:?}",
            case.label, after["AppInstanceAdmin"]["CreatedTimestamp"]
        );
    }
}

/// The ARNs of the administrators `ListAppInstanceAdmins` reports for one app instance.
async fn list_admin_arns(emulator: &Emulator, app_instance_arn: &str) -> Vec<String> {
    let (status, body) = emulator
        .call(
            "GET",
            &format!("/app-instances/{}/admins", uri_encode(app_instance_arn)),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "ListAppInstanceAdmins must answer 200, got {status} {body:?}"
    );
    body["AppInstanceAdmins"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|entry| entry["Admin"]["Arn"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the promotable-ARN table: an ARN a promotion names, and whether it names an
/// identity of the app instance at all.
#[derive(Debug)]
struct IdentityKindCase {
    label: &'static str,
    /// The admin ARN, built from the app instance's ARN and the stored user's ARN.
    admin_arn: fn(&str, &str) -> String,
    /// Whether the ARN names an identity of that app instance, and so may be promoted.
    promotable: bool,
}

/// RULE: only an ARN that names an IDENTITY of the given app instance — an
/// `AppInstanceUser` (`<AppInstanceArn>/user/<id>`) or an `AppInstanceBot`
/// (`<AppInstanceArn>/bot/<id>`) — may be promoted to an administrator of it; every other
/// ARN, including one that hangs under the same app instance beneath a segment belonging
/// to another service, is refused and recorded nowhere.
///
/// The table spans what the rule generalises over: the child segment (the two identity
/// segments, a sibling service's segment, a segment of neither kind, near misses of the
/// identity segments), the DEPTH of the path under the instance (the instance itself, a
/// segment with no id, an identity path with a deeper child), and whether this service
/// holds a record for the identity named.
#[tokio::test]
async fn only_an_identity_of_the_app_instance_can_be_promoted() {
    // The identity segments an app instance's children hang off — `user` and `bot` — are
    // this service's own, named by the crate's `state::names_identity_resource`; a sibling
    // service's resource (Chime SDK Messaging's `channel`) hangs off the same app instance
    // under a segment of its own and names no identity.
    let cases = [
        IdentityKindCase {
            label: "a stored AppInstanceUser of the app instance",
            admin_arn: |_instance, user| user.to_string(),
            promotable: true,
        },
        IdentityKindCase {
            label: "an AppInstanceBot of the app instance",
            admin_arn: |instance, _user| format!("{instance}/bot/helper"),
            promotable: true,
        },
        IdentityKindCase {
            label: "a Chime SDK Messaging channel under the same app instance",
            admin_arn: |instance, _user| format!("{instance}/channel/abc"),
            promotable: false,
        },
        IdentityKindCase {
            label: "a resource nested under a sibling service's channel",
            admin_arn: |instance, _user| format!("{instance}/channel/abc/message/1"),
            promotable: false,
        },
        IdentityKindCase {
            label: "a child segment of neither identity kind",
            admin_arn: |instance, _user| format!("{instance}/admin/alice"),
            promotable: false,
        },
        IdentityKindCase {
            label: "a near miss of the user segment",
            admin_arn: |instance, _user| format!("{instance}/users/alice"),
            promotable: false,
        },
        IdentityKindCase {
            label: "a near miss of the bot segment",
            admin_arn: |instance, _user| format!("{instance}/bots/helper"),
            promotable: false,
        },
        IdentityKindCase {
            label: "a path deeper than an identity of the app instance",
            admin_arn: |_instance, user| format!("{user}/endpoint/e1"),
            promotable: false,
        },
        IdentityKindCase {
            label: "the user segment with no identity id",
            admin_arn: |instance, _user| format!("{instance}/user"),
            promotable: false,
        },
        IdentityKindCase {
            label: "the bot segment with an empty identity id",
            admin_arn: |instance, _user| format!("{instance}/bot/"),
            promotable: false,
        },
        IdentityKindCase {
            label: "the app instance's own ARN",
            admin_arn: |instance, _user| instance.to_string(),
            promotable: false,
        },
    ];

    let emulator = Emulator::new();
    let instance = emulator.app_instance("identity-kinds", "tok-ik").await;
    let alice = emulator.user(&instance, "alice", "tok-ik-alice").await;

    let mut promoted: Vec<String> = Vec::new();
    for case in &cases {
        let admin_arn = (case.admin_arn)(&instance, &alice);
        let (status, body) = promote(&emulator, &instance, &admin_arn).await;
        if case.promotable {
            assert_eq!(
                status, 201,
                "case {:?}: an identity of the app instance must be promoted, got {status} {body:?}",
                case.label
            );
            assert_eq!(
                body["AppInstanceAdmin"]["Arn"].as_str(),
                Some(admin_arn.as_str()),
                "case {:?}: wrong promoted identity, body {body:?}",
                case.label
            );
            promoted.push(admin_arn.clone());
        } else {
            assert_eq!(
                status, 400,
                "case {:?}: an ARN that names no identity of the app instance must be refused, got {status} {body:?}",
                case.label
            );
            assert_eq!(
                body["Code"].as_str(),
                Some("BadRequest"),
                "case {:?}: wrong refusal Code, body {body:?}",
                case.label
            );

            // A refused promotion is recorded nowhere, so neither read reports it.
            let (described, described_body) = describe_admin(&emulator, &instance, &admin_arn).await;
            assert_eq!(
                (described, described_body["Code"].as_str()),
                (403, Some("Forbidden")),
                "case {:?}: a refused promotion must not be readable, got {described} {described_body:?}",
                case.label
            );
        }

        let mut listed = list_admin_arns(&emulator, &instance).await;
        listed.sort();
        let mut expected = promoted.clone();
        expected.sort();
        assert_eq!(
            listed, expected,
            "case {:?}: expected exactly the promoted identities listed, got {listed:?}",
            case.label
        );
    }
}
