// SPDX-License-Identifier: CC0-1.0

//! What a campaign-name collision READS like, end to end over the wire.
//!
//! Every case drives real HTTP through the router the production entrypoint serves,
//! so each assertion exercises the restJson1 wire protocol and the handler together.
//! The rule under test: a write refused because the campaign name it asks for is
//! already held by another campaign of the same Connect instance is answered with the
//! modelled `ConflictException` (HTTP 409) worded with that error's one fixed
//! sentence — the refusal carries no value taken from the request or from the store,
//! neither the name, nor the instance id, nor a campaign id. Both writers of a
//! campaign name are driven here: `CreateCampaign` and `UpdateCampaignName`.
//!
//! `ConflictException` has a single `message` member and no per-resource template
//! (`smithy_query(query="shape",shape="ConflictException")`), and `CreateCampaign`
//! models it at httpError 409
//! (`smithy_query(query="operation",operation="CreateCampaign")`).

use std::sync::Arc;

use serde_json::{json, Value};

use super::common::{call, error_code, error_message, onboard_instance, router, Harness};

/// The sentence AWS words a campaign-name collision with, pinned here as a literal
/// rather than read back from the crate under test: the wording is the requirement, so
/// a test that imported the constant the handler carries would pass for any wording
/// that constant happened to hold.
const EXPECTED_CONFLICT_MESSAGE: &str = "Failed due to conflict in current state of the resource.";

/// A minimal telephony `channelSubtypeConfig`: `outboundMode` and
/// `defaultOutboundConfig` are both required by the model.
fn telephony_config() -> Value {
    json!({
        "telephony": {
            "outboundMode": {"agentless": {}},
            "defaultOutboundConfig": {"connectContactFlowId": "flow-0001"}
        }
    })
}

/// `CreateCampaign` for `instance` under `name`, returning its status and body.
async fn create(router: &Harness, region: &str, instance: &str, name: &str) -> (u16, Value) {
    call(
        router,
        region,
        "PUT",
        "/v2/campaigns",
        &json!({
            "name": name,
            "connectInstanceId": instance,
            "channelSubtypeConfig": telephony_config(),
        }),
    )
    .await
}

/// Assert `body` is the conflict refusal of a name collision: HTTP 409,
/// `ConflictException`, the fixed message, and nothing of `values` (the request's own
/// members) anywhere in that message.
fn assert_conflict(case: &str, status: u16, body: &Value, values: &[&str]) {
    assert_eq!(
        status, 409,
        "case {case:?}: expected HTTP 409, got {status} {body}"
    );
    assert!(
        error_code(body).ends_with("ConflictException"),
        "case {case:?}: expected ConflictException, got {} ({body})",
        error_code(body)
    );
    assert_eq!(
        error_message(body),
        EXPECTED_CONFLICT_MESSAGE,
        "case {case:?}: expected the fixed conflict message {EXPECTED_CONFLICT_MESSAGE:?}, got {:?}",
        error_message(body)
    );
    // A value only a handful of characters long can occur inside ordinary English
    // text by coincidence, so the "nothing was interpolated" check is made on the
    // distinctive values; the equality above is what pins the sentence exactly.
    for value in values.iter().filter(|value| value.chars().count() > 3) {
        assert!(
            !error_message(body).contains(value),
            "case {case:?}: expected no request value in the conflict message, got {:?} naming {value:?}",
            error_message(body)
        );
    }
}

/// A repeat `CreateCampaign` of a name an instance already holds is refused with the
/// fixed conflict sentence, whatever the name and whatever the instance id.
///
/// Rows differ along the name (length at both published bounds of `CampaignName`'s
/// `@length` 1..127, character class, whitespace, digits) and along the
/// `connectInstanceId` spelling `InstanceId`'s `@pattern ^[-_.a-zA-Z0-9]+$` admits
/// (uuid, dotted, underscored); none is the name or the instance the issue cited.
#[tokio::test]
async fn a_duplicate_create_reads_as_the_fixed_conflict_sentence() {
    let region = "us-east-1";
    let router = router();

    // (case, connectInstanceId, name)
    let cases: &[(&str, &str, String)] = &[
        (
            "uuid instance, shortest admissible name",
            "7b0f4a2e-11c3-4e5a-9d31-0ca2f5b7e801",
            "a".to_string(),
        ),
        (
            "dotted instance, longest admissible name",
            "conflict.message.1",
            "n".repeat(127),
        ),
        (
            "underscored instance, spaces and punctuation",
            "conflict_message_2",
            "Spring Sale (2026) — wave #1".to_string(),
        ),
        (
            "hyphenated instance, non-ascii name",
            "conflict-message-3",
            "campagne-été".to_string(),
        ),
        (
            "digit instance, digits-only name",
            "40721",
            "4242".to_string(),
        ),
    ];

    for (case, instance, name) in cases {
        onboard_instance(&router, region, instance).await;
        let (status, created) = create(&router, region, instance, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first CreateCampaign expected 200, got {status} {created}"
        );
        let id = created["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateCampaign returned no id: {created}"))
            .to_string();

        let (status, refused) = create(&router, region, instance, name).await;
        assert_conflict(case, status, &refused, &[name.as_str(), instance, &id]);
    }
}

/// `UpdateCampaignName` refuses a rename onto a name another campaign of the same
/// instance holds with the SAME fixed sentence: one text for the invariant both
/// writers of a campaign name uphold.
#[tokio::test]
async fn a_rename_onto_a_held_name_reads_as_the_fixed_conflict_sentence() {
    let region = "eu-west-1";
    let router = router();

    // (case, connectInstanceId, the name the other campaign holds)
    let cases: &[(&str, &str, &str)] = &[
        (
            "uuid instance",
            "c41d9b7a-2f58-4a60-8e12-5d3b7c9ae400",
            "autumn-wave",
        ),
        ("dotted instance", "rename.conflict.1", "winter wave #2"),
        ("underscored instance", "rename_conflict_2", "été-2026"),
    ];

    for (index, (case, instance, held)) in cases.iter().enumerate() {
        onboard_instance(&router, region, instance).await;
        let (status, body) = create(&router, region, instance, held).await;
        assert_eq!(
            status, 200,
            "case {case:?}: creating the holder expected 200, got {status} {body}"
        );

        let mine = format!("mine-{index}");
        let (status, body) = create(&router, region, instance, &mine).await;
        assert_eq!(
            status, 200,
            "case {case:?}: creating the renamer expected 200, got {status} {body}"
        );
        let id = body["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateCampaign returned no id: {body}"))
            .to_string();

        let (status, refused) = call(
            &router,
            region,
            "POST",
            &format!("/v2/campaigns/{id}/name"),
            &json!({"name": held}),
        )
        .await;
        assert_conflict(case, status, &refused, &[held, instance, &id]);
    }
}

/// Concurrent creates of one name on one instance: exactly one is admitted, and every
/// loser reads the same fixed conflict sentence — the refusal does not depend on how
/// many racers there were or on which of them lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_duplicate_creates_all_read_the_same_sentence() {
    let region = "ap-southeast-2";
    let router = Arc::new(router());
    let instance = "race.conflict.1";
    onboard_instance(&router, region, instance).await;

    // (case, how many creates race for the name, the name they race for). One row is
    // the degenerate race where nobody loses.
    let cases: &[(&str, usize, &str)] = &[
        ("one racer", 1, "race-one"),
        ("two racers", 2, "race-two"),
        ("five racers", 5, "race five"),
        ("nine racers", 9, "race-nine-ét"),
    ];

    for (case, racers, name) in cases {
        let mut running = Vec::with_capacity(*racers);
        for _ in 0..*racers {
            let router = Arc::clone(&router);
            let name = (*name).to_string();
            running.push(tokio::spawn(async move {
                create(&router, region, instance, &name).await
            }));
        }
        let mut answers = Vec::with_capacity(*racers);
        for handle in running {
            answers.push(
                handle
                    .await
                    .unwrap_or_else(|err| panic!("case {case:?}: a racer panicked: {err}")),
            );
        }

        let winners = answers.iter().filter(|(status, _)| *status == 200).count();
        assert_eq!(
            winners, 1,
            "case {case:?}: expected exactly one admitted create of {racers}, got {winners} ({answers:?})"
        );
        for (status, body) in answers.iter().filter(|(status, _)| *status != 200) {
            assert_conflict(case, *status, body, &[name, instance]);
        }
    }
}
