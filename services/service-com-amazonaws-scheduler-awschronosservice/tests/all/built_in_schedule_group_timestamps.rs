// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the two optional timestamps a schedule group
//! reports: `CreationDate` and `LastModificationDate`.
//!
//! The rule under test: those members report the instants a REQUEST made on the
//! group. A group a create call minted carries both; the group the service
//! provisions with the account — which no call created — carries neither, so
//! the serializer omits the keys entirely rather than writing a stamped value or
//! a null. Once a call does change that built-in group it reports the
//! modification instant it then has, and still no creation instant. Both
//! readers of a group are held to the rule: the `ScheduleGroupSummary` entries
//! of `ListScheduleGroups` and the `GetScheduleGroup` output, whose
//! `CreationDate` and `LastModificationDate` members are optional in both
//! shapes (`smithy_query(query="shape",
//! shape="ScheduleGroupSummary,GetScheduleGroupOutput")`).
//!
//! Every request goes through the real restJson1 front door
//! (`MultiServiceRouter::dispatch`), so URI binding, the handler and response
//! serialization are all exercised.

use smithy_server_runtime::MultiServiceRouter;

use crate::wire_harness::{
    call, call_in, encode_label, group_arn, now_epoch_seconds, scheduler_router, REGION,
};

/// The group every account holds without having created it.
const BUILT_IN_GROUP: &str = "default";

/// Names spanning the modelled `ScheduleGroupName` class — length 1..64,
/// pattern `^[0-9a-zA-Z-_.]+$` (`smithy_query(query="shape",
/// shape="ScheduleGroupName")`): one character, a leading digit, mixed case,
/// every punctuation mark the pattern allows, a name that merely starts with the
/// built-in group's name, and the 64-character maximum.
const CREATED_NAMES: &[(&str, &str)] = &[
    ("one character", "g"),
    ("leading digit", "9lives"),
    ("mixed case", "ZuluGroup"),
    ("dots, dashes and underscores", "a.b-c_d"),
    ("the built-in name with a suffix", "default-2"),
    (
        "at the 64-character maximum",
        "g0123456789012345678901234567890123456789012345678901234567890a",
    ),
];

/// Whether a group projection carries each timestamp, and its value when it
/// does. A member that is present must be a number — restJson1 renders a
/// `timestamp` as epoch seconds — so a `null` fails here too.
fn timestamps(case: &str, group: &serde_json::Value) -> (Option<f64>, Option<f64>) {
    let read = |member: &str| {
        group.get(member).map(|value| {
            value.as_f64().unwrap_or_else(|| {
                panic!("case {case:?}: {member} must be an epoch-seconds number, got {group}")
            })
        })
    };
    (read("CreationDate"), read("LastModificationDate"))
}

/// `GetScheduleGroup` for `name`, signed for `region`.
async fn get_group(
    router: &MultiServiceRouter,
    name: &str,
    region: &str,
) -> (u16, serde_json::Value) {
    call_in(
        router,
        "GET",
        &format!("/schedule-groups/{name}"),
        region,
        None,
    )
    .await
}

/// The `ListScheduleGroups` summary for `name`, signed for `region`.
async fn listed_group(router: &MultiServiceRouter, name: &str, region: &str) -> serde_json::Value {
    let (status, body) = call_in(router, "GET", "/schedule-groups", region, None).await;
    assert_eq!(
        status, 200,
        "group {name:?}: ListScheduleGroups got {status} {body}"
    );
    body.get("ScheduleGroups")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("ScheduleGroups is a required output member, got {body}"))
        .iter()
        .find(|summary| summary.get("Name").and_then(serde_json::Value::as_str) == Some(name))
        .unwrap_or_else(|| panic!("group {name:?}: no summary for it in {body}"))
        .clone()
}

/// `CreateScheduleGroup` for `name`.
async fn create_group(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = call(router, "POST", &format!("/schedule-groups/{name}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateScheduleGroup({name}) got {status} {body}"
    );
}

/// `TagResource` on the group `name` owns in [`REGION`].
async fn tag_group(router: &MultiServiceRouter, case: &str, name: &str, key: &str, value: &str) {
    let (status, body) = call(
        router,
        "POST",
        &format!("/tags/{}", encode_label(&group_arn(REGION, name))),
        Some(serde_json::json!({ "Tags": [{ "Key": key, "Value": value }] })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: TagResource({name}) got {status} {body}"
    );
}

#[tokio::test]
async fn a_created_group_reports_both_instants_and_the_built_in_group_reports_neither() {
    let router = scheduler_router();

    // The built-in group, before anything is created: no call made either
    // instant, so neither key is in the response at all.
    for (case, projection) in [
        ("built-in, GetScheduleGroup, fresh store", {
            let (status, got) = get_group(&router, BUILT_IN_GROUP, REGION).await;
            assert_eq!(status, 200, "the built-in group must be readable, got {got}");
            got
        }),
        (
            "built-in, ListScheduleGroups, fresh store",
            listed_group(&router, BUILT_IN_GROUP, REGION).await,
        ),
    ] {
        assert_eq!(
            timestamps(case, &projection),
            (None, None),
            "case {case:?}: a group no call created or changed reports neither \
             timestamp, got {projection}"
        );
    }

    // One row per class of group name: each is created, then read back through
    // both readers, and both instants are the create call's.
    for (case, name) in CREATED_NAMES {
        assert!(
            !name.is_empty() && name.len() <= 64,
            "case {case:?}: the test's own name is outside the modelled bound"
        );
        let before = now_epoch_seconds();
        create_group(&router, case, name).await;
        let after = now_epoch_seconds();

        let (status, got) = get_group(&router, name, REGION).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetScheduleGroup({name}) got {status} {got}"
        );
        let summary = listed_group(&router, name, REGION).await;

        for (reader, projection) in [("GetScheduleGroup", &got), ("ListScheduleGroups", &summary)] {
            let label = format!("{case} via {reader}");
            let (creation, modification) = timestamps(&label, projection);
            let creation = creation.unwrap_or_else(|| {
                panic!("case {label:?}: a created group must report CreationDate, got {projection}")
            });
            let modification = modification.unwrap_or_else(|| {
                panic!(
                    "case {label:?}: a created group must report LastModificationDate, \
                     got {projection}"
                )
            });
            assert!(
                creation >= before - 1.0 && creation <= after + 1.0,
                "case {label:?}: expected CreationDate at the create call \
                 (between {before} and {after}), got {creation}"
            );
            assert_eq!(
                creation, modification,
                "case {label:?}: nothing has changed this group, so \
                 LastModificationDate is {creation}, got {modification}"
            );
        }
    }

    // Creating groups changes nothing about the built-in one, and reading it
    // twice answers the same thing: a read reports, it never stamps.
    let (_, first) = get_group(&router, BUILT_IN_GROUP, REGION).await;
    let (_, second) = get_group(&router, BUILT_IN_GROUP, REGION).await;
    assert_eq!(
        (first.clone(), timestamps("built-in, read twice", &second)),
        (second.clone(), (None, None)),
        "case \"built-in, read twice\": expected one unchanging record without \
         timestamps, got {first} then {second}"
    );
}

#[tokio::test]
async fn the_built_in_group_reports_no_instants_in_any_region() {
    // The built-in group belongs to every Region of the account, and no call
    // created it in any of them.
    for region in ["us-east-1", "eu-west-2", "ap-southeast-2", "us-gov-west-1"] {
        let router = scheduler_router();
        let (status, got) = get_group(&router, BUILT_IN_GROUP, region).await;
        assert_eq!(
            status, 200,
            "case {region:?}: the built-in group must be readable, got {got}"
        );
        let summary = listed_group(&router, BUILT_IN_GROUP, region).await;
        for (reader, projection) in [("GetScheduleGroup", &got), ("ListScheduleGroups", &summary)] {
            let label = format!("{region} via {reader}");
            assert_eq!(
                timestamps(&label, projection),
                (None, None),
                "case {label:?}: expected neither timestamp, got {projection}"
            );
        }
    }
}

#[tokio::test]
async fn changing_the_built_in_group_gives_it_a_modification_instant_but_no_creation_instant() {
    let router = scheduler_router();

    // Tagging is a change to the group, so it is an instant a call made — while
    // no call has created the built-in group, then or ever.
    let before = now_epoch_seconds();
    tag_group(&router, "tag the built-in group", BUILT_IN_GROUP, "env", "p").await;
    let after = now_epoch_seconds();

    let (status, got) = get_group(&router, BUILT_IN_GROUP, REGION).await;
    assert_eq!(status, 200, "GetScheduleGroup(default) got {status} {got}");
    let summary = listed_group(&router, BUILT_IN_GROUP, REGION).await;
    for (reader, projection) in [("GetScheduleGroup", &got), ("ListScheduleGroups", &summary)] {
        let label = format!("tagged built-in group via {reader}");
        let (creation, modification) = timestamps(&label, projection);
        assert_eq!(
            creation, None,
            "case {label:?}: no call created this group, so CreationDate stays \
             absent, got {projection}"
        );
        let modification = modification.unwrap_or_else(|| {
            panic!("case {label:?}: a changed group reports LastModificationDate, got {projection}")
        });
        assert!(
            modification >= before - 1.0 && modification <= after + 1.0,
            "case {label:?}: expected LastModificationDate at the tag call \
             (between {before} and {after}), got {modification}"
        );
    }

    // A created group that is changed reports both: the create call's instant
    // and the later change's.
    let name = "tagged-group";
    create_group(&router, "created then tagged", name).await;
    let (_, created) = get_group(&router, name, REGION).await;
    let (creation, at_create) = timestamps("created then tagged, before the tag", &created);
    assert_eq!(
        creation, at_create,
        "case \"created then tagged\": a group nothing has changed reports one \
         instant twice, got {created}"
    );
    tag_group(&router, "created then tagged", name, "env", "p").await;
    let (_, changed) = get_group(&router, name, REGION).await;
    let (still_created, modification) = timestamps("created then tagged", &changed);
    assert_eq!(
        still_created, creation,
        "case \"created then tagged\": a change must not move CreationDate, got {changed}"
    );
    assert!(
        modification >= creation,
        "case \"created then tagged\": expected LastModificationDate at or after \
         CreationDate {creation:?}, got {modification:?}"
    );
}
