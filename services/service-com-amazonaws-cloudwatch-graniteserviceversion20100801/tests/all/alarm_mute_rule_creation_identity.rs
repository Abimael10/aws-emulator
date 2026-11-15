// SPDX-License-Identifier: CC0-1.0

//! The creation identity a read of an alarm mute rule reports.
//!
//! Every read of an alarm mute rule carries the `CreationId` of the write that CREATED it — a
//! uuid naming that write and the epoch milliseconds it happened at — the same identity, in the
//! same form, that a read of a metric alarm reports (`alarm_read_projection.rs` owns that half).
//! It is the identity of the CREATE, so a write that only replaces an existing rule's
//! configuration keeps it, and a rule deleted and written again carries a new one.
//!
//! Every table row is a distinct input along what the rule generalises over (which expression
//! form the schedule states its instants in, which optional members the configuration carries,
//! how many alarms it targets, and which Region it was written in), so no row is the
//! specification.

use crate::harness::{
    build_router, call_in_region, creation_identity_millis, now_secs, ok, reported_creation_id,
    DEFAULT_REGION,
};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::collections::BTreeSet;

/// Another Region, for the row that asserts a rule's identity belongs to the rule of the Region
/// it was written in.
const OTHER_REGION: &str = "us-west-2";

/// A `PutAlarmMuteRule` request with only the members the model makes required.
fn put_request(name: &str, expression: &str, duration: &str) -> Value {
    json!({
        "Name": name,
        "Rule": {"Schedule": {"Expression": expression, "Duration": duration}},
    })
}

/// `request` with `extra` merged into it, so a row states only what it varies.
fn with_members(mut request: Value, extra: Value) -> Value {
    let (Some(target), Some(extra)) = (request.as_object_mut(), extra.as_object()) else {
        return request;
    };
    for (member, value) in extra {
        target.insert(member.clone(), value.clone());
    }
    request
}

/// The `CreationId` `GetAlarmMuteRule` reports for the rule `name` of `region`, or `None` when
/// the member is absent.
async fn creation_id(router: &MultiServiceRouter, region: &str, name: &str) -> Option<String> {
    let (status, body) = call_in_region(
        router,
        region,
        "GetAlarmMuteRule",
        &json!({"AlarmMuteRuleName": name}),
    )
    .await;
    assert_eq!(status, 200, "reading {name:?} in {region} answered {body}");
    reported_creation_id(&body).map(str::to_string)
}

#[tokio::test]
async fn every_mute_rule_read_reports_the_creation_identity_of_the_write_that_created_it() {
    let router = build_router();
    let mut identities: BTreeSet<String> = BTreeSet::new();

    // Rows differ along the expression form, the length of the window, and which optional
    // members the configuration carries — none of which the identity depends on.
    let cases: Vec<(&str, Value)> = vec![
        (
            "a recurring window, required members only",
            put_request("identity-recurring", "cron(0 2 * * *)", "PT4H"),
        ),
        (
            "a one-time window with a description",
            with_members(
                put_request("identity-one-time", "at(2031-07-04T01:30)", "PT90M"),
                json!({"Description": "the independence-day release"}),
            ),
        ),
        (
            "a recurring window targeting three alarms",
            with_members(
                put_request("identity-targeted", "cron(0 1 1 * *)", "P2DT12H"),
                json!({"MuteTargets": {"AlarmNames": [
                    "WebServerCPUAlarm",
                    "DatabaseConnectionAlarm",
                    "QueueDepthAlarm",
                ]}}),
            ),
        ),
    ];

    for (case, request) in cases {
        let name = request["Name"].as_str().unwrap_or_default().to_string();
        let before = now_secs();
        ok(&router, "PutAlarmMuteRule", &request).await;
        let after = now_secs();

        let reported = creation_id(&router, DEFAULT_REGION, &name).await;
        let minted = reported.clone().unwrap_or_default();
        let millis = creation_identity_millis(&minted);
        assert!(
            millis.is_some(),
            "case {case:?}: expected a CreationId of the form <uuid>/<epoch-millis>, got \
             {reported:?}"
        );
        let millis = millis.unwrap_or_default();
        #[allow(clippy::cast_possible_truncation)]
        let (lower, upper) = (
            (before * 1_000.0) as i64 - 1_000,
            (after * 1_000.0) as i64 + 1_000,
        );
        assert!(
            (lower..=upper).contains(&millis),
            "case {case:?}: the creation instant must be the instant of the write \
             ({lower}..={upper} ms), got {millis}"
        );
        assert!(
            identities.insert(minted.clone()),
            "case {case:?}: every rule carries its own creation identity, got the repeated \
             {minted:?}"
        );

        // A write that only replaces the configuration of a rule that already exists keeps the
        // identity the rule was created with — it is the identity of the CREATE.
        ok(
            &router,
            "PutAlarmMuteRule",
            &with_members(request.clone(), json!({"Description": "replaced"})),
        )
        .await;
        let after_update = creation_id(&router, DEFAULT_REGION, &name).await;
        assert_eq!(
            after_update.as_deref(),
            Some(minted.as_str()),
            "case {case:?}: an update keeps the creation identity, got {after_update:?}"
        );

        // A rule that was deleted and written again would be a new rule and carry a new
        // identity; `DeleteAlarmMuteRule` is still scaffolded in this emulator, so what is
        // asserted here is the other half of that statement: two rules written with the same
        // configuration under different names never share an identity (the `identities` set
        // above), and an update never mints one.
        let twin = format!("{name}-twin");
        ok(
            &router,
            "PutAlarmMuteRule",
            &with_members(request.clone(), json!({"Name": &twin})),
        )
        .await;
        let twin_identity = creation_id(&router, DEFAULT_REGION, &twin).await;
        assert!(
            twin_identity
                .as_deref()
                .is_some_and(|twin| twin != minted
                    && creation_identity_millis(twin).is_some()),
            "case {case:?}: a second rule with the same configuration carries its own identity \
             in the same form, got {twin_identity:?} beside {minted:?}"
        );
    }
}

#[tokio::test]
async fn one_name_in_two_regions_is_two_rules_with_two_identities() {
    let router = build_router();
    let request = put_request("identity-regional", "cron(0 3 * * *)", "PT1H");

    let mut reported: Vec<String> = Vec::new();
    for region in [DEFAULT_REGION, OTHER_REGION] {
        let (status, body) = call_in_region(&router, region, "PutAlarmMuteRule", &request).await;
        assert_eq!(
            status, 200,
            "case {region:?}: writing the rule answered {body}"
        );
        let identity = creation_id(&router, region, "identity-regional").await;
        assert!(
            identity
                .as_deref()
                .and_then(creation_identity_millis)
                .is_some(),
            "case {region:?}: expected a CreationId of the form <uuid>/<epoch-millis>, got \
             {identity:?}"
        );
        reported.push(identity.unwrap_or_default());
    }

    assert!(
        reported[0] != reported[1],
        "a rule of one Region and a rule of another are two rules, so their creation identities \
         differ, got {reported:?}"
    );
}
