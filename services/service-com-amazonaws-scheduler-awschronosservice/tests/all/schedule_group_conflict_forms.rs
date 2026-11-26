// SPDX-License-Identifier: CC0-1.0

//! The two forms `CreateScheduleGroup` refuses a name a group already holds in,
//! driven over the real `restJson1` front door.
//!
//! RULE: the conflict is always 409 `ConflictException`, and which SENTENCE it carries
//! depends on what the request would have done to the group it collided with. A
//! request that carries TAGS would have written the held group's tags as well as
//! minting it, so it is reported as a concurrent modification of that group and names
//! it by its ARN: `Schedule group <ARN> was modified concurrently.` A request that
//! carries none asks only for the name, so it is reported as the name being taken:
//! `Schedule group <Name> already exists.`
//!
//! A client that tells a durable name collision (do not retry) from a concurrent
//! modification (retry) reads the two apart, and the ARN the tagged form carries is
//! the identity of the group the request met.
//!
//! Neither refusal changes the held group: its ARN, its two instants and its tags are
//! the ones the accepted create left on it, whichever form refused the collision. The
//! table walks the tag sets a request may carry — none, the member absent, an empty
//! list, one tag, several — over several group names rather than one recorded request.

use smithy_server_runtime::MultiServiceRouter;

use crate::wire_harness::{
    call, error_code, error_message, group_arn, list_tags, scheduler_router, tags_of, REGION,
};

/// The group a row holds before it collides with it, created with one tag of its own.
async fn held(router: &MultiServiceRouter, name: &str) -> serde_json::Value {
    let (status, created) = call(
        router,
        "POST",
        &format!("/schedule-groups/{name}"),
        Some(serde_json::json!({ "Tags": [{ "Key": "origin", "Value": "held" }] })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateScheduleGroup {name} must succeed, got {status} {created}"
    );
    let (status, read) = call(router, "GET", &format!("/schedule-groups/{name}"), None).await;
    assert_eq!(
        status, 200,
        "setup: GetScheduleGroup {name} must succeed, got {status} {read}"
    );
    read
}

#[tokio::test]
async fn the_conflict_form_depends_on_whether_the_request_carries_tags() {
    let router = scheduler_router();

    // (case, the group's name, the body the colliding create carries, whether it
    // carries tags)
    let rows: [(&str, &str, Option<serde_json::Value>, bool); 5] = [
        ("no body at all", "collide.none", None, false),
        (
            "a body stating no tags",
            "collide.absent",
            Some(serde_json::json!({})),
            false,
        ),
        (
            // An empty list writes no tag, so the request would modify nothing.
            "an empty tag list",
            "collide.empty",
            Some(serde_json::json!({ "Tags": [] })),
            false,
        ),
        (
            "one tag",
            "collide.one",
            Some(serde_json::json!({ "Tags": [{ "Key": "intruder", "Value": "rejected" }] })),
            true,
        ),
        (
            "several tags",
            "collide.several",
            Some(serde_json::json!({ "Tags": [
                { "Key": "intruder", "Value": "rejected" },
                { "Key": "team", "Value": "orders" }
            ] })),
            true,
        ),
    ];

    for (case, name, body, carries_tags) in rows {
        let before = held(&router, name).await;
        let (_, tags_before) = list_tags(&router, &group_arn(REGION, name)).await;

        let (status, answer) = call(
            &router,
            "POST",
            &format!("/schedule-groups/{name}"),
            body.clone(),
        )
        .await;
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (409, "ConflictException"),
            "case {case:?}: expected 409 ConflictException, got {status} {answer}"
        );

        let expected = match carries_tags {
            true => format!(
                "Schedule group {} was modified concurrently.",
                group_arn(REGION, name)
            ),
            false => format!("Schedule group {name} already exists."),
        };
        assert_eq!(
            error_message(&answer),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            error_message(&answer)
        );

        // Neither refusal touches the group it collided with.
        let (status, after) = call(&router, "GET", &format!("/schedule-groups/{name}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the held group must still read back, got {status} {after}"
        );
        for member in ["Arn", "Name", "State", "CreationDate", "LastModificationDate"] {
            assert_eq!(
                after.get(member),
                before.get(member),
                "case {case:?}: the refusal must leave {member} as it was, got {after}"
            );
        }
        let (_, tags_after) = list_tags(&router, &group_arn(REGION, name)).await;
        assert_eq!(
            tags_of(&tags_after),
            tags_of(&tags_before),
            "case {case:?}: the refusal must leave the held group's tags alone"
        );
    }
}

#[tokio::test]
async fn the_tagged_conflict_names_the_group_it_met_by_its_arn() {
    let router = scheduler_router();
    const NAME: &str = "held.by_arn";
    let before = held(&router, NAME).await;
    let arn = before
        .get("Arn")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();

    let (status, answer) = call(
        &router,
        "POST",
        &format!("/schedule-groups/{NAME}"),
        Some(serde_json::json!({ "Tags": [{ "Key": "k", "Value": "v" }] })),
    )
    .await;
    assert_eq!(
        (status, error_code(&answer).as_str()),
        (409, "ConflictException"),
        "case a tagged collision: expected 409 ConflictException, got {status} {answer}"
    );
    // The ARN in the sentence is the HELD group's own, as GetScheduleGroup reports it.
    assert!(
        error_message(&answer).contains(&arn),
        "case a tagged collision: expected the held group's ARN {arn} in {:?}",
        error_message(&answer)
    );
    assert_eq!(
        error_message(&answer),
        format!("Schedule group {arn} was modified concurrently."),
        "case a tagged collision: got {:?}",
        error_message(&answer)
    );
}
