// SPDX-License-Identifier: CC0-1.0

//! One vocabulary for "that schedule group does not exist", however the group was
//! addressed.
//!
//! A schedule group is addressed by NAME on the group and schedule operations and by ARN
//! on the tagging operations (`TagResourceArn` is modelled
//! `^arn:aws[a-z-]*:scheduler:[a-z0-9\-]+:\d{12}:schedule-group\/[0-9a-zA-Z-_.]+$`), but
//! the resource is the same resource: a request naming a group this account does not hold
//! is answered HTTP 404 `ResourceNotFoundException` whose message names the GROUP — the
//! name, parsed out of the ARN where that is how the request carried it — and not the ARN
//! it travelled as. A message of any other shape is one this service never produces, so a
//! caller matching the service's own text reads it as a different condition.
//!
//! The rows span what the rule generalises over: the operation (every tagging operation,
//! beside the operations addressed by name), the name's form across the class
//! `ScheduleGroupName` admits (`^[0-9a-zA-Z-_.]+$`, `length 1..64`), and how the ARN
//! misses — a group never created, a group that was deleted, and a well-formed ARN whose
//! Region or partition this endpoint does not serve.
//!
//! Every request goes through the shared [`crate::wire_harness`], the real restJson1 front
//! door.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use crate::wire_harness::{
    account_id, call, encode_label, error_code, error_message, group_arn, list_tags,
    scheduler_router, REGION,
};

/// The sentence a request addressed at a missing schedule group is refused with, as the
/// service words it. Spelled out here rather than read from the crate, so the text is
/// asserted against a literal and not against itself.
fn group_does_not_exist(name: &str) -> String {
    format!("Schedule group {name} does not exist.")
}

/// `POST /tags/{ResourceArn}` with one key/value pair.
async fn tag_one(router: &MultiServiceRouter, arn: &str) -> (u16, serde_json::Value) {
    call(
        router,
        "POST",
        &format!("/tags/{}", encode_label(arn)),
        Some(serde_json::json!({ "Tags": [{ "Key": "k", "Value": "v" }] })),
    )
    .await
}

/// `DELETE /tags/{ResourceArn}?TagKeys=k`.
async fn untag_one(router: &MultiServiceRouter, arn: &str) -> (u16, serde_json::Value) {
    call(
        router,
        "DELETE",
        &format!("/tags/{}?TagKeys=k", encode_label(arn)),
        None,
    )
    .await
}

/// Group names spanning the class `ScheduleGroupName` admits: a single character, every
/// punctuation character the pattern allows, digits only, and the 64-character maximum.
const MISSING_NAMES: &[(&str, &str)] = &[
    ("a single character", "g"),
    ("dots, dashes and underscores", "a.b-c_d"),
    ("digits only", "1234567890"),
    (
        "at the 64-character maximum",
        "g0123456789012345678901234567890123456789012345678901234567890a",
    ),
];

#[tokio::test]
async fn a_tagging_request_naming_a_missing_group_names_that_group() {
    let router = scheduler_router();

    // Every answer must carry the group's own sentence; collecting them is what proves the
    // service has ONE such sentence rather than three independent equalities.
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for (case, name) in MISSING_NAMES {
        let arn = group_arn(REGION, name);
        let expected = group_does_not_exist(name);
        for (operation, (status, body)) in [
            ("TagResource", tag_one(&router, &arn).await),
            ("UntagResource", untag_one(&router, &arn).await),
            ("ListTagsForResource", list_tags(&router, &arn).await),
        ] {
            let message = error_message(&body);
            assert_eq!(
                (status, error_code(&body).as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?} ({operation}): expected a not-found refusal, got {status} {body}"
            );
            assert_eq!(
                message, expected,
                "case {case:?} ({operation}): expected {expected:?}, got {message:?}"
            );
            assert!(
                !message.contains("arn:"),
                "case {case:?} ({operation}): the refusal must name the group, not the ARN it \
                 travelled as, got {message:?}"
            );
            seen.insert(message);
        }
    }

    assert_eq!(
        seen.len(),
        MISSING_NAMES.len(),
        "each group must be named by its own sentence and nothing else, got {seen:?}"
    );
}

#[tokio::test]
async fn a_group_addressed_by_arn_and_by_name_is_missing_in_the_same_words() {
    let router = scheduler_router();
    let name = "shared-vocabulary";

    // Addressed by ARN (the tagging surface) and by name (the group and schedule
    // surface), for a group that was never created.
    let arn = group_arn(REGION, name);
    let by_arn = [
        ("TagResource", tag_one(&router, &arn).await),
        ("UntagResource", untag_one(&router, &arn).await),
        ("ListTagsForResource", list_tags(&router, &arn).await),
    ];
    let by_name = [
        (
            "GetScheduleGroup",
            call(&router, "GET", &format!("/schedule-groups/{name}"), None).await,
        ),
        (
            "DeleteScheduleGroup",
            call(&router, "DELETE", &format!("/schedule-groups/{name}"), None).await,
        ),
        (
            "GetSchedule",
            call(&router, "GET", &format!("/schedules/s?groupName={name}"), None).await,
        ),
        (
            "DeleteSchedule",
            call(
                &router,
                "DELETE",
                &format!("/schedules/s?groupName={name}"),
                None,
            )
            .await,
        ),
    ];

    let expected = group_does_not_exist(name);
    for (operation, (status, body)) in by_arn.into_iter().chain(by_name) {
        let message = error_message(&body);
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {operation:?}: expected a not-found refusal, got {status} {body}"
        );
        assert_eq!(
            message, expected,
            "case {operation:?}: expected {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_well_formed_arn_this_endpoint_does_not_serve_names_the_group_it_carries() {
    let router = scheduler_router();
    let name = "served-group";
    let (status, body) = call(&router, "POST", &format!("/schedule-groups/{name}"), None).await;
    assert_eq!(status, 200, "CreateScheduleGroup answered {status} {body}");

    // An ARN is a resource's identity: one naming another Region or partition addresses no
    // group reachable from this endpoint, even where the NAME is a group this account
    // holds. The refusal still names the group the ARN carries.
    let elsewhere: [(&str, String); 3] = [
        (
            "another Region",
            format!(
                "arn:aws:scheduler:us-east-1:{}:schedule-group/{name}",
                account_id()
            ),
        ),
        (
            "another partition",
            format!(
                "arn:aws-us-gov:scheduler:{REGION}:{}:schedule-group/{name}",
                account_id()
            ),
        ),
        (
            "another account",
            format!("arn:aws:scheduler:{REGION}:111122223333:schedule-group/{name}"),
        ),
    ];

    let expected = group_does_not_exist(name);
    for (case, arn) in elsewhere {
        for (operation, (status, body)) in [
            ("TagResource", tag_one(&router, &arn).await),
            ("UntagResource", untag_one(&router, &arn).await),
            ("ListTagsForResource", list_tags(&router, &arn).await),
        ] {
            let message = error_message(&body);
            assert_eq!(
                (status, error_code(&body).as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?} ({operation}): expected a not-found refusal, got {status} {body}"
            );
            assert_eq!(
                message, expected,
                "case {case:?} ({operation}): expected {expected:?}, got {message:?}"
            );
        }
    }

    // And the group this endpoint does serve is still served, by its own ARN.
    let (status, body) = list_tags(&router, &group_arn(REGION, name)).await;
    assert_eq!(
        status, 200,
        "the group this endpoint serves must still answer, got {status} {body}"
    );
}
