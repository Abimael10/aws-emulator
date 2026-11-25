// SPDX-License-Identifier: CC0-1.0

//! A target naming an EVENT BUS has to carry a `RoleArn`, end to end over the wire
//! (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`) through the
//! real `MultiServiceRouter` with Amazon SQS mounted beside EventBridge — so both
//! the refusal and what the rule holds afterwards are read where a client reads
//! them: off the error of the call, and off `ListTargetsByRule`.
//!
//! The RULE asserted here: EventBridge reaches a bus target by making an
//! `events:PutEvents` on the bus the target names, and it makes that call as "the
//! Amazon Resource Name (ARN) of the IAM role to be used for this target when the
//! rule is triggered" (`Target$RoleArn`) — so a target whose `Arn` names an event
//! bus and carries no `RoleArn` names nothing for EventBridge to publish as,
//! whichever account and Region that bus belongs to. Such a request is refused
//! WHOLE: HTTP 400 `ValidationException` naming the offending target's own ARN,
//! and not one target of the request is written, so a valid target submitted
//! beside it does not reach the rule either and the rule still lists exactly the
//! targets it had.
//!
//! `PutTargets` models no client-input error (`ConcurrentModificationException`,
//! `InternalException`, `LimitExceededException`, `ManagedRuleException` and
//! `ResourceNotFoundException` are its modelled errors), so the refusal travels on
//! this service's unmodelled 400 `ValidationException` channel.
//!
//! The table spans the class rather than one recorded case: the rows differ along
//! WHICH bus the target names (a bus of the Region the rule lives in, a bus of
//! another Region, a bus of another account) and along WHERE the bus target sits in
//! the request (after a valid target, before one), against the positive control —
//! the same target WITH a `RoleArn`, which is accepted and read back carrying that
//! role.
//!
//! The harness (the router with EventBridge and Amazon SQS on one engine, and the
//! queue fixtures) is `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};

use crate::target_dispatch::{emulator, events, given_queue, Emulator, REGION};

/// The rule every row writes its targets onto, and the target it already carries.
const RULE: &str = "bus-target-role";
const KEPT_TARGET: &str = "kept";

/// An account that is not the one these calls are signed for.
const OTHER_ACCOUNT: &str = "210987654321";

/// A Region that is not the one these calls are signed for.
const OTHER_REGION: &str = "us-east-1";

/// The ARN of the bus `name` of `region` in `account`, as a target names it.
fn bus_arn(region: &str, account: &str, name: &str) -> String {
    format!("arn:aws:events:{region}:{account}:event-bus/{name}")
}

/// The role a bus target hands EventBridge. It is created nowhere: a `RoleArn` the
/// account holds no role for is still the role the target names, and whether the
/// role exists is not what `PutTargets` decides.
fn role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/events-forwarding",
        engine::emulator_account_id()
    )
}

/// The rule with the one target it already carries, so every refusal below is read
/// against a target list that must not change.
async fn given_rule(emulator: &Emulator, case: &str, queue_arn: &str) {
    let pattern = json!({ "source": ["com.demo.orders"] }).to_string();
    let (status, body) = events(
        emulator,
        "PutRule",
        &json!({ "Name": RULE, "EventPattern": pattern, "State": "ENABLED" }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: PutRule answered {status} {body}");
    let (status, body) = events(
        emulator,
        "PutTargets",
        &json!({
            "Rule": RULE,
            "Targets": [{ "Id": KEPT_TARGET, "Arn": queue_arn }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutTargets answered {status} {body}"
    );
}

/// The targets the rule lists, by `Id`, in the order it holds them.
async fn listed_ids(emulator: &Emulator, case: &str) -> Vec<String> {
    let (status, body) = events(emulator, "ListTargetsByRule", &json!({ "Rule": RULE })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListTargetsByRule answered {status} {body}"
    );
    body["Targets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|target| target["Id"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// A bus target carrying no `RoleArn` refuses the whole request, over a TABLE whose
/// rows differ along the bus the target names and along where it sits in the
/// request.
#[tokio::test]
async fn a_bus_target_without_a_role_arn_refuses_the_whole_request() {
    let account = engine::emulator_account_id();
    // (case, the bus the target names)
    let buses = [
        (
            "a bus of the Region the rule lives in",
            bus_arn(REGION, account, "other"),
        ),
        (
            "a bus of another Region",
            bus_arn(OTHER_REGION, account, "other"),
        ),
        (
            "a bus of another account",
            bus_arn(REGION, OTHER_ACCOUNT, "other"),
        ),
    ];
    // (case, whether the bus target is submitted after the valid one)
    let positions = [("after a valid target", true), ("before one", false)];

    for (bus_case, bus) in buses {
        for (position_case, last) in positions {
            let case = format!("{bus_case}, {position_case}");
            let emulator = emulator();
            let (_, queue_arn) = given_queue(&emulator, "role-required").await;
            given_rule(&emulator, &case, &queue_arn).await;

            let queue_target = json!({ "Id": "q", "Arn": queue_arn });
            let bus_target = json!({ "Id": "bus", "Arn": bus });
            let targets = if last {
                json!([queue_target, bus_target])
            } else {
                json!([bus_target, queue_target])
            };
            let (status, body) = events(
                &emulator,
                "PutTargets",
                &json!({ "Rule": RULE, "Targets": targets }),
            )
            .await;

            assert_eq!(
                status, 400,
                "case {case:?}: expected the request to be refused, got {status} {body}"
            );
            let kind = body["__type"].as_str().unwrap_or_default();
            assert!(
                kind.ends_with("ValidationException"),
                "case {case:?}: expected a ValidationException, got {kind:?} in {body}"
            );
            let message = body["message"].as_str().unwrap_or_default();
            let expected = format!("RoleArn is required for target {bus}.");
            assert_eq!(
                message, expected,
                "case {case:?}: expected the refusal {expected:?}, got {message:?}"
            );
            // Refused WHOLE: the valid queue target of the same request is not
            // written either.
            let ids = listed_ids(&emulator, &case).await;
            assert_eq!(
                ids,
                vec![KEPT_TARGET.to_string()],
                "case {case:?}: a refused request leaves the rule's targets alone, got {ids:?}"
            );
        }
    }
}

/// The positive control of the same rule: the same bus target WITH a `RoleArn` is
/// accepted alongside the queue target, and both are read back — the bus target
/// carrying the role it was written with.
#[tokio::test]
async fn a_bus_target_naming_a_role_is_accepted_and_read_back_with_it() {
    let account = engine::emulator_account_id();
    let buses = [
        (
            "a bus of the Region the rule lives in",
            bus_arn(REGION, account, "other"),
        ),
        (
            "a bus of another Region",
            bus_arn(OTHER_REGION, account, "other"),
        ),
        (
            "a bus of another account",
            bus_arn(REGION, OTHER_ACCOUNT, "other"),
        ),
    ];

    for (case, bus) in buses {
        let emulator = emulator();
        let (_, queue_arn) = given_queue(&emulator, "role-named").await;
        given_rule(&emulator, case, &queue_arn).await;

        let (status, body) = events(
            &emulator,
            "PutTargets",
            &json!({
                "Rule": RULE,
                "Targets": [
                    { "Id": "q", "Arn": queue_arn },
                    { "Id": "bus", "Arn": bus, "RoleArn": role_arn() },
                ],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutTargets answered {status} {body}"
        );
        assert_eq!(
            body["FailedEntryCount"],
            json!(0),
            "case {case:?}: PutTargets reported a failed entry: {body}"
        );

        let (status, listed) = events(&emulator, "ListTargetsByRule", &json!({ "Rule": RULE })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListTargetsByRule answered {status} {listed}"
        );
        let stored: Vec<Value> = listed["Targets"].as_array().cloned().unwrap_or_default();
        let ids: Vec<String> = stored
            .iter()
            .map(|target| target["Id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            ids,
            vec![
                KEPT_TARGET.to_string(),
                "q".to_string(),
                "bus".to_string()
            ],
            "case {case:?}: expected both submitted targets to be stored, got {ids:?}"
        );
        let written = stored
            .iter()
            .find(|target| target["Id"] == json!("bus"))
            .unwrap_or_else(|| panic!("case {case:?}: the bus target was just asserted to exist"));
        assert_eq!(
            written["RoleArn"],
            json!(role_arn()),
            "case {case:?}: expected the bus target to be read back with its role, got {written}"
        );
    }
}
