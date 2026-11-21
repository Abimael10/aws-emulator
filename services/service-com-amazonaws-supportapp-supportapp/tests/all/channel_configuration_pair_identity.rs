// SPDX-License-Identifier: CC0-1.0

//! The ONE identity rule every operation that names a single Slack channel
//! configuration follows, driven through each of them over the same account.
//!
//! A Slack channel configuration is identified by its whole `(teamId, channelId)` pair:
//! the pair `CreateSlackChannelConfiguration` files a configuration under is the pair
//! `UpdateSlackChannelConfiguration` and `DeleteSlackChannelConfiguration` find it by,
//! and a pair the account holds no configuration for is the modelled
//! `ResourceNotFoundException` (HTTP 404) — "The specified resource is missing or
//! doesn't exist, such as an account alias, Slack channel configuration, or Slack
//! workspace configuration"
//! (`smithy_query(query="operation", operation="CreateSlackChannelConfiguration,UpdateSlackChannelConfiguration,DeleteSlackChannelConfiguration",
//! model_path="services/service-com-amazonaws-supportapp-supportapp/smithy-model/com-amazonaws-supportapp-supportapp.json")`)
//! — worded with the one template this service states a channel miss with, while a pair
//! the account DOES hold is the modelled `ConflictException` (409) on the create path.
//!
//! The three operations answer from one state accessor rather than from three
//! hand-written lookups, so this suite drives the rule through all three on each row and
//! compares their answers to each other: the create's presence test must agree with what
//! the delete removed, and the update's and the delete's miss must be the same message
//! for the same pair. A copy that drifted — keying a configuration differently, or
//! wording a miss differently — makes two of these answers disagree.
//!
//! The rows differ along what the rule generalises over: the form and the length of each
//! id, at both published `@length` bounds, a pair whose two members are the same string,
//! and the ORDER of the pair, which the swap rows pin by naming the mirrored pair. The
//! sibling suites run partial matches ([`crate::channel_configuration_pair_lookup`]) and
//! the order the lookup is settled in ([`crate::channel_configuration_lookup_order`]).
//!
//! Every assertion goes over the wire through the same router the production emulator
//! mounts, reusing the harness in [`crate::slack_channel_configurations`], so an answer
//! that is not computed from persisted state cannot pass.

use crate::channel_configuration_workspace_precondition::{create_body, update_body};
use crate::error_message_templates::{not_found, refusal};
use crate::slack_channel_configurations::{Emulator, Reply};

/// `teamId` and `channelId` are `len 1..256, pattern ^\S+$`
/// (`smithy_query(query="operation", operation="DeleteSlackChannelConfiguration")`), so
/// the rows below sit at both published bounds.
const SLACK_ID_MAX_LEN: usize = 256;

/// One row: the pair a configuration is created, read back, updated and deleted under.
#[derive(Debug, Clone)]
struct Case {
    label: &'static str,
    team_id: String,
    channel_id: String,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            // The pair this API's own documentation names ("such as T012ABCDEFG"): one
            // row of many.
            label: "the documented workspace id",
            team_id: "T012ABCDEFG".to_string(),
            channel_id: "C01234A5BCD".to_string(),
        },
        Case {
            label: "the shortest legal ids",
            team_id: "T".to_string(),
            channel_id: "C".to_string(),
        },
        Case {
            label: "the longest legal ids",
            team_id: "T".repeat(SLACK_ID_MAX_LEN),
            channel_id: "C".repeat(SLACK_ID_MAX_LEN),
        },
        Case {
            label: "two ids differing only in case",
            team_id: "Workspace-9".to_string(),
            channel_id: "workspace-9".to_string(),
        },
        Case {
            label: "the same string in both members",
            team_id: "S012ABCDEFG".to_string(),
            channel_id: "S012ABCDEFG".to_string(),
        },
        Case {
            label: "ids built from punctuation",
            team_id: "T#9/a.b_c".to_string(),
            channel_id: "C#9/a.b_c".to_string(),
        },
    ]
}

/// The `ConflictException` message a create of a pair the account already holds a
/// configuration for is refused with ("Add a Slack channel configuration that already
/// exists in your Amazon Web Services account" —
/// `smithy_query(query="doc", shape="ConflictException")`), naming the resource with the
/// same subject [`not_found`] names it with.
fn already_exists(team_id: &str, channel_id: &str) -> String {
    format!(
        "Slack channel configuration for team {team_id} and channel {channel_id} already exists"
    )
}

/// Assert `reply` is the modelled miss for `(team_id, channel_id)` and return the
/// message it carried, so two operations' misses can be compared to each other.
fn miss_message(case: &str, team_id: &str, channel_id: &str, reply: &Reply) -> String {
    let message = refusal(case, reply, 404, "ResourceNotFoundException");
    let expected = not_found(team_id, channel_id);
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?}, got {message:?}"
    );
    message
}

/// Create the configuration `case` names in a workspace this account has registered.
async fn arrange(case: &Case) -> Emulator {
    let label = case.label;
    let emulator = Emulator::new();
    let created = emulator
        .create_in_registered_workspace(create_body(&case.team_id, &case.channel_id))
        .await;
    assert_eq!(
        created.status, 200,
        "case {label:?}: expected the create to answer HTTP 200, got {} / {}",
        created.status, created.body
    );
    emulator
}

#[tokio::test]
async fn the_three_writes_name_one_configuration_by_the_same_pair() {
    for case in cases() {
        let label = case.label;
        let emulator = arrange(&case).await;

        // The pair the create filed the configuration under is the pair every reader
        // finds it by.
        assert!(
            emulator
                .listed(&case.team_id, &case.channel_id)
                .await
                .is_some(),
            "case {label:?}: expected the listing to report the created pair, got {}",
            emulator.list(None).await.body
        );

        // The create path's presence test, over that same stored pair: a channel this
        // account already holds a configuration for is the conflict.
        let duplicate = emulator
            .create(create_body(&case.team_id, &case.channel_id))
            .await;
        let conflict = refusal(
            &format!("{label} / create of the stored pair"),
            &duplicate,
            409,
            "ConflictException",
        );
        let expected_conflict = already_exists(&case.team_id, &case.channel_id);
        assert_eq!(
            conflict, expected_conflict,
            "case {label:?}: expected {expected_conflict:?}, got {conflict:?}"
        );

        // The update path's lookup, over the same stored pair: it finds the record and
        // answers with the pair it was named by.
        let updated = emulator
            .update(update_body(&case.team_id, &case.channel_id))
            .await;
        assert_eq!(
            updated.status, 200,
            "case {label:?}: expected the update of the stored pair to answer HTTP 200, \
             got {} / {}",
            updated.status, updated.body
        );
        assert_eq!(
            (updated.string("teamId"), updated.string("channelId")),
            (Some(case.team_id.clone()), Some(case.channel_id.clone())),
            "case {label:?}: expected the update to answer for the pair it was named by, \
             got {}",
            updated.body
        );

        // The delete path's lookup, over the same stored pair: it finds the record and
        // takes it out of the store the other two read.
        let deleted = emulator.delete(&case.team_id, &case.channel_id).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: expected the delete of the stored pair to answer HTTP 200, \
             got {} / {}",
            deleted.status, deleted.body
        );
        assert!(
            emulator
                .listed(&case.team_id, &case.channel_id)
                .await
                .is_none(),
            "case {label:?}: expected the deleted pair to be gone from the listing, got {}",
            emulator.list(None).await.body
        );

        // And now that the account holds nothing for the pair, the two operations that
        // act on an EXISTING configuration answer the SAME miss for it, word for word.
        let update_miss = miss_message(
            &format!("{label} / update of the deleted pair"),
            &case.team_id,
            &case.channel_id,
            &emulator
                .update(update_body(&case.team_id, &case.channel_id))
                .await,
        );
        let delete_miss = miss_message(
            &format!("{label} / delete of the deleted pair"),
            &case.team_id,
            &case.channel_id,
            &emulator.delete(&case.team_id, &case.channel_id).await,
        );
        assert_eq!(
            update_miss, delete_miss,
            "case {label:?}: expected both operations to word the miss identically, got \
             {update_miss:?} and {delete_miss:?}"
        );

        // The create path reads the same store: the pair is free again, so the
        // configuration can be created a second time.
        let recreated = emulator
            .create(create_body(&case.team_id, &case.channel_id))
            .await;
        assert_eq!(
            recreated.status, 200,
            "case {label:?}: expected the pair to be creatable again after the delete, \
             got {} / {}",
            recreated.status, recreated.body
        );
    }
}

#[tokio::test]
async fn the_two_members_of_the_pair_are_not_interchangeable() {
    // The pair is ordered: `teamId` names the workspace and `channelId` the channel
    // within it, so a request that swaps them names a configuration the account does not
    // hold — the miss — and leaves the stored one alone. Rows whose two members are the
    // same string have no distinct mirror, so the swap is their own pair and is skipped.
    for case in cases().into_iter().filter(|c| c.team_id != c.channel_id) {
        let label = case.label;
        let emulator = arrange(&case).await;

        miss_message(
            &format!("{label} / update of the mirrored pair"),
            &case.channel_id,
            &case.team_id,
            &emulator
                .update(update_body(&case.channel_id, &case.team_id))
                .await,
        );
        miss_message(
            &format!("{label} / delete of the mirrored pair"),
            &case.channel_id,
            &case.team_id,
            &emulator.delete(&case.channel_id, &case.team_id).await,
        );

        assert!(
            emulator
                .listed(&case.team_id, &case.channel_id)
                .await
                .is_some(),
            "case {label:?}: expected the stored pair to survive the mirrored writes, got {}",
            emulator.list(None).await.body
        );
        let listed = emulator.list(None).await;
        assert_eq!(
            listed.configurations().len(),
            1,
            "case {label:?}: expected exactly 1 stored configuration, got {}",
            listed.body
        );
    }
}
