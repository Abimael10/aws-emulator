// SPDX-License-Identifier: CC0-1.0

//! What a delete leaves behind of the alarm mute rule it removed: nothing.
//!
//! ONE rule, asserted over a table: **`DeleteAlarmMuteRule` removes the WHOLE record the store
//! held under that name in that Region, so the name is free again — a `PutAlarmMuteRule` under it
//! afterwards CREATES a rule, which carries only that write's configuration (every optional
//! member the write omits reads back absent rather than carrying the removed rule's value) and a
//! creation identity of its own, and the Region's listing holds exactly one rule of that name.**
//!
//! "Deletes a specific alarm mute rule." / "This operation is idempotent. If you delete a mute
//! rule that does not exist, the operation succeeds without returning an error."
//! (`smithy_query(query="doc",shape="DeleteAlarmMuteRule")`), while a write under a name the store
//! still holds is an UPDATE — "If the rule already exists, this operation updates the existing
//! rule with the new configuration." (`smithy_query(query="doc",shape="PutAlarmMuteRule")`) — so
//! what separates the two is whether the record is there, and a delete is what takes it away. The
//! name is the rule's identity "within your Amazon Web Services account and region"
//! (`smithy_query(query="doc",shape="PutAlarmMuteRuleInput",member="Name")`), so the Region the
//! calls are signed for is part of which record is removed and which is created.
//!
//! This is the half `alarm_mute_rule_creation_identity.rs` states and leaves to the delete: a rule
//! deleted and written again is a new rule, so it carries a new creation identity rather than the
//! one the removed rule had. What the delete does to the alarms the removed rule was muting is
//! `alarm_mute_rule_deletion.rs`, and the not-found a read of a removed rule answers with is
//! `alarm_mute_rules.rs`.
//!
//! The rows differ along what the rule generalises over: which optional members the removed rule
//! carried and which the re-created one carries, which expression form each states its window in
//! (and so the `MuteType` and `Status` derived from it), the name's length at both bounds the model
//! admits (`AlarmMuteRuleName` is `@length 1..255`,
//! `smithy_query(query="operation",operation="DeleteAlarmMuteRule")`), the Region the cycle runs
//! in, and whether the name ever held a rule before the delete.

use std::collections::BTreeSet;

use ::chrono::{TimeDelta, Utc};
use ::chrono_tz::Tz;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::alarm_mute_rules::{
    at_expression, listed_arns, minute_aligned, put_request, rule_arn, scheduled_in_its_timezone,
    text, with_members,
};
use crate::harness::{
    build_router, call_in_region, creation_identity_millis, error_code, reported_creation_id,
    DEFAULT_REGION,
};

/// Another Region, for the row whose whole cycle runs outside the default one.
const OTHER_REGION: &str = "us-west-2";

/// The error class a read of a rule the store does not hold answers with
/// (`smithy_query(query="operation",operation="GetAlarmMuteRule")`).
const NOT_FOUND: &str = "ResourceNotFoundException";

/// A schedule whose window is open at every instant: a cron expression opening a window every
/// minute and holding it open for an hour, so the rule it configures is ACTIVE whenever the suite
/// runs rather than at one time of day.
const ALWAYS_OPEN: (&str, &str) = ("cron(* * * * *)", "PT1H");

/// The token a read reports a one-time window in, for the `at` expression form documented "For
/// one-time mute windows" (`Schedule$Expression`). The two tokens the service emits are the rule
/// `alarm_mute_rules.rs` owns; this suite only asserts that a re-created rule reports the one its
/// OWN expression names.
const ONE_TIME: &str = "one-time";

/// The token a read reports a recurring window in — the `cron` expression form ([`ONE_TIME`]).
const RECURRING: &str = "recurring";

/// One create-delete-create cycle over one name.
struct Row {
    /// What this row is about, named by every assertion it makes.
    case: &'static str,
    /// The name the cycle runs on.
    name: String,
    /// The Region every call of the row is signed for.
    region: &'static str,
    /// The rule written before the delete, or `None` for the row whose name never held one.
    before: Option<Value>,
    /// The rule written after the delete.
    after: Value,
    /// The status the clock makes of that re-created rule.
    status: &'static str,
    /// The token it reports its kind of window in.
    mute_type: &'static str,
}

/// Read the rule `name` of `region`, requiring that the store holds it.
async fn stored(router: &MultiServiceRouter, case: &str, region: &str, name: &str) -> Value {
    let (status, body) = call_in_region(
        router,
        region,
        "GetAlarmMuteRule",
        &json!({"AlarmMuteRuleName": name}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the rule {name:?} to read back in {region}, got {status} \
         with {body}"
    );
    body
}

/// Assert that `region` holds no rule under `name`: the reader answers the not-found it models and
/// the listing carries nothing of that name.
async fn assert_gone(router: &MultiServiceRouter, case: &str, region: &str, name: &str) {
    let (status, body) = call_in_region(
        router,
        region,
        "GetAlarmMuteRule",
        &json!({"AlarmMuteRuleName": name}),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (404, NOT_FOUND),
        "case {case:?}: expected HTTP 404 {NOT_FOUND} for {name:?} in {region}, got {status} \
         with {body}"
    );
    assert_eq!(
        listed_in(router, region, name).await,
        Vec::<String>::new(),
        "case {case:?}: expected no listing of {name:?} in {region}"
    );
}

/// The ARNs `ListAlarmMuteRules` reports in `region` for the rule `name` — none when the Region
/// holds no such rule, and more than one only if the store had let a name hold two rules.
async fn listed_in(router: &MultiServiceRouter, region: &str, name: &str) -> Vec<String> {
    let (status, body) =
        call_in_region(router, region, "ListAlarmMuteRules", &json!({})).await;
    assert_eq!(status, 200, "listing {region} answered {status} with {body}");
    let wanted = rule_arn(region, name);
    listed_arns(&body)
        .into_iter()
        .filter(|arn| *arn == wanted)
        .collect()
}

/// Delete the rule `name` in `region`, requiring the success the operation documents for every
/// name — one the store holds and one it does not alike.
async fn delete_rule(router: &MultiServiceRouter, case: &str, region: &str, name: &str) {
    let (status, body) = call_in_region(
        router,
        region,
        "DeleteAlarmMuteRule",
        &json!({"AlarmMuteRuleName": name}),
    )
    .await;
    assert_eq!(
        (status, &body),
        (200, &json!({})),
        "case {case:?}: deleting {name:?} in {region} must answer HTTP 200 and an empty body, \
         got {status} with {body}"
    );
}

/// A timestamp member of a body, which the awsJson protocols carry as epoch seconds.
fn stamp(value: &Value, member: &str) -> Option<f64> {
    value.get(member).and_then(Value::as_f64)
}

/// Assert that `read` reports exactly the configuration `request` wrote: the schedule as written
/// (read in the zone it is evaluated in), and each optional member either as the request carried
/// it or ABSENT when the request omitted it — which is what says nothing of an earlier rule of
/// that name survived.
fn assert_configuration(case: &str, phase: &str, name: &str, read: &Value, request: &Value) {
    assert_eq!(
        text(read, "Name").as_deref(),
        Some(name),
        "case {case:?} {phase}: expected the rule to name itself {name:?}, got {read}"
    );
    assert_eq!(
        read.get("Rule"),
        Some(&scheduled_in_its_timezone(request)),
        "case {case:?} {phase}: expected the schedule this write configured, got {read}"
    );
    for member in ["Description", "MuteTargets"] {
        assert_eq!(
            read.get(member),
            request.get(member),
            "case {case:?} {phase}: expected {member} to be this write's, got {read}"
        );
    }
    for member in ["StartDate", "ExpireDate"] {
        assert_eq!(
            stamp(read, member),
            stamp(request, member),
            "case {case:?} {phase}: expected {member} to be this write's, got {read}"
        );
    }
}

/// The creation identity `read` reports, asserted to be of the form every creation identity of
/// this service takes.
fn identity_of(case: &str, phase: &str, read: &Value) -> String {
    let reported = reported_creation_id(read);
    assert!(
        reported.and_then(creation_identity_millis).is_some(),
        "case {case:?} {phase}: expected a CreationId of the form <uuid>/<epoch-millis>, got \
         {reported:?} in {read}"
    );
    reported.unwrap_or_default().to_string()
}

#[tokio::test]
async fn a_deleted_mute_rule_frees_its_name_and_leaves_nothing_of_itself_behind() {
    let now = Utc::now();
    // A name at each bound the model admits for `AlarmMuteRuleName` (`@length 1..255`): one
    // character, and 255 of them.
    let shortest = "r".to_string();
    let longest = format!("reused-long-{}", "x".repeat(243));
    assert_eq!(
        (shortest.len(), longest.len()),
        (1, 255),
        "the two bound rows must name the bounds of AlarmMuteRuleName"
    );

    let rows: Vec<Row> = vec![
        Row {
            case: "a described, targeted, dated rule replaced by a bare recurring one",
            name: "reused-bare".to_string(),
            region: DEFAULT_REGION,
            before: Some(with_members(
                put_request("reused-bare", ALWAYS_OPEN.0, ALWAYS_OPEN.1),
                json!({
                    "Description": "the window the first rule muted",
                    "MuteTargets": {"AlarmNames": ["WebServerCPUAlarm", "QueueDepthAlarm"]},
                    "StartDate": minute_aligned(now - TimeDelta::hours(1)),
                    "ExpireDate": minute_aligned(now + TimeDelta::days(2)),
                }),
            )),
            after: put_request("reused-bare", ALWAYS_OPEN.0, ALWAYS_OPEN.1),
            status: "ACTIVE",
            mute_type: RECURRING,
        },
        Row {
            case: "a recurring rule replaced by a one-time window still to come, in a named zone",
            name: "reused-one-time".to_string(),
            region: DEFAULT_REGION,
            before: Some(with_members(
                put_request("reused-one-time", ALWAYS_OPEN.0, ALWAYS_OPEN.1),
                json!({"MuteTargets": {"AlarmNames": ["WebServerCPUAlarm"]}}),
            )),
            after: with_members(
                put_request("reused-one-time", ALWAYS_OPEN.0, ALWAYS_OPEN.1),
                json!({
                    "Rule": {"Schedule": {
                        "Expression": at_expression(
                            now + TimeDelta::hours(3),
                            Tz::America__New_York,
                        ),
                        "Duration": "PT1H",
                        "Timezone": "America/New_York",
                    }},
                    "Description": "the window the second rule mutes",
                }),
            ),
            status: "SCHEDULED",
            mute_type: ONE_TIME,
        },
        Row {
            case: "a closed one-time window replaced by a rule past its expiry, in another Region",
            name: "reused-regional".to_string(),
            region: OTHER_REGION,
            before: Some(with_members(
                put_request("reused-regional", ALWAYS_OPEN.0, ALWAYS_OPEN.1),
                json!({"Rule": {"Schedule": {
                    "Expression": at_expression(now - TimeDelta::hours(3), Tz::UTC),
                    "Duration": "PT1H",
                }}}),
            )),
            after: with_members(
                put_request("reused-regional", ALWAYS_OPEN.0, ALWAYS_OPEN.1),
                json!({"ExpireDate": minute_aligned(now - TimeDelta::days(1))}),
            ),
            status: "EXPIRED",
            mute_type: RECURRING,
        },
        Row {
            case: "a name at the shortest the model admits",
            name: shortest.clone(),
            region: DEFAULT_REGION,
            before: Some(put_request(&shortest, ALWAYS_OPEN.0, ALWAYS_OPEN.1)),
            after: with_members(
                put_request(&shortest, ALWAYS_OPEN.0, ALWAYS_OPEN.1),
                json!({"Rule": {"Schedule": {
                    "Expression": at_expression(now + TimeDelta::hours(3), Tz::UTC),
                    "Duration": "PT1H",
                }}}),
            ),
            status: "SCHEDULED",
            mute_type: ONE_TIME,
        },
        Row {
            case: "a name at the longest the model admits",
            name: longest.clone(),
            region: DEFAULT_REGION,
            before: Some(with_members(
                put_request(&longest, ALWAYS_OPEN.0, ALWAYS_OPEN.1),
                json!({"Description": "a rule under a name of 255 characters"}),
            )),
            after: with_members(
                put_request(&longest, ALWAYS_OPEN.0, ALWAYS_OPEN.1),
                json!({"MuteTargets": {"AlarmNames": ["WebServerCPUAlarm"]}}),
            ),
            status: "ACTIVE",
            mute_type: RECURRING,
        },
        Row {
            case: "a name deleted before it ever held a rule",
            name: "never-held-then-written".to_string(),
            region: DEFAULT_REGION,
            before: None,
            after: put_request("never-held-then-written", ALWAYS_OPEN.0, ALWAYS_OPEN.1),
            status: "ACTIVE",
            mute_type: RECURRING,
        },
    ];

    let router = build_router();
    // Every creation identity this suite sees, so no re-created rule reports one already reported.
    let mut identities: BTreeSet<String> = BTreeSet::new();

    for row in &rows {
        let (case, name, region) = (row.case, row.name.as_str(), row.region);

        let removed = match &row.before {
            Some(request) => {
                let (status, body) =
                    call_in_region(&router, region, "PutAlarmMuteRule", request).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: writing the first rule answered {status} with {body}"
                );
                let read = stored(&router, case, region, name).await;
                assert_configuration(case, "before the delete", name, &read, request);
                let identity = identity_of(case, "before the delete", &read);
                assert!(
                    identities.insert(identity.clone()),
                    "case {case:?}: every created rule carries its own identity, got the \
                     repeated {identity:?}"
                );
                Some(identity)
            }
            None => None,
        };

        delete_rule(&router, case, region, name).await;
        assert_gone(&router, case, region, name).await;

        let (status, body) = call_in_region(&router, region, "PutAlarmMuteRule", &row.after).await;
        assert_eq!(
            status, 200,
            "case {case:?}: writing the rule again answered {status} with {body}"
        );
        let read = stored(&router, case, region, name).await;
        assert_configuration(case, "after the delete", name, &read, &row.after);
        assert_eq!(
            text(&read, "Status").as_deref(),
            Some(row.status),
            "case {case:?}: expected the re-created rule to be {}, got {read}",
            row.status
        );
        assert_eq!(
            text(&read, "MuteType").as_deref(),
            Some(row.mute_type),
            "case {case:?}: expected the re-created rule's window to be {}, got {read}",
            row.mute_type
        );
        assert_eq!(
            text(&read, "AlarmMuteRuleArn"),
            Some(rule_arn(region, name)),
            "case {case:?}: expected the rule's own ARN, got {read}"
        );

        // The write after the delete CREATED a rule rather than updating the one that was
        // removed: the identity it reports is a new one.
        let identity = identity_of(case, "after the delete", &read);
        assert!(
            identities.insert(identity.clone()),
            "case {case:?}: a rule written after the delete is a new rule, so its identity must \
             be new, got the repeated {identity:?}"
        );
        assert_ne!(
            Some(identity.as_str()),
            removed.as_deref(),
            "case {case:?}: expected an identity other than the removed rule's"
        );

        // The name holds exactly ONE rule in its Region, and none in any other.
        assert_eq!(
            listed_in(&router, region, name).await,
            vec![rule_arn(region, name)],
            "case {case:?}: expected {name:?} to be listed exactly once in {region}"
        );
        let elsewhere = if region == DEFAULT_REGION {
            OTHER_REGION
        } else {
            DEFAULT_REGION
        };
        assert_gone(&router, case, elsewhere, name).await;
    }
}

#[tokio::test]
async fn a_name_written_and_deleted_again_and_again_accumulates_nothing() {
    // The same rule as the table above, over repetition: each cycle leaves the store exactly as
    // the one before it found it, so a name does not accumulate rules, identities or members.
    let router = build_router();
    let name = "cycled-rule";
    let mut identities: BTreeSet<String> = BTreeSet::new();

    // The cycles differ along the configuration each one writes, so no cycle reads back the
    // members of the one before it.
    let cycles: Vec<(&str, Value)> = vec![
        (
            "the first cycle, a bare recurring rule",
            put_request(name, ALWAYS_OPEN.0, ALWAYS_OPEN.1),
        ),
        (
            "the second cycle, described and targeting an alarm",
            with_members(
                put_request(name, ALWAYS_OPEN.0, ALWAYS_OPEN.1),
                json!({
                    "Description": "the second cycle's rule",
                    "MuteTargets": {"AlarmNames": ["WebServerCPUAlarm"]},
                }),
            ),
        ),
        (
            "the third cycle, bare again",
            put_request(name, ALWAYS_OPEN.0, ALWAYS_OPEN.1),
        ),
    ];

    for (case, request) in &cycles {
        let (status, body) =
            call_in_region(&router, DEFAULT_REGION, "PutAlarmMuteRule", request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: writing the rule answered {status} with {body}"
        );
        let read = stored(&router, case, DEFAULT_REGION, name).await;
        assert_configuration(case, "as written", name, &read, request);
        let identity = identity_of(case, "as written", &read);
        assert!(
            identities.insert(identity.clone()),
            "case {case:?}: each cycle creates the rule afresh, so its identity must be new, got \
             the repeated {identity:?}"
        );
        assert_eq!(
            listed_in(&router, DEFAULT_REGION, name).await,
            vec![rule_arn(DEFAULT_REGION, name)],
            "case {case:?}: expected {name:?} to be listed exactly once"
        );

        // Two deletes in a row: the second names a rule the store no longer holds, which the
        // operation answers for exactly as the first.
        delete_rule(&router, case, DEFAULT_REGION, name).await;
        delete_rule(&router, case, DEFAULT_REGION, name).await;
        assert_gone(&router, case, DEFAULT_REGION, name).await;
    }
}
