// SPDX-License-Identifier: CC0-1.0

//! A Slack channel configuration write made BEFORE the account adds the Slack workspace
//! it names.
//!
//! `ConflictException`'s own documentation states that condition first: "Add, update, or
//! delete a Slack channel configuration before you add a Slack workspace to your Amazon
//! Web Services account" (`smithy_query(query="doc", shape="ConflictException")`). Each of
//! `CreateSlackChannelConfiguration`, `UpdateSlackChannelConfiguration` and
//! `DeleteSlackChannelConfiguration` binds that error (HTTP 409)
//! (`smithy_query(query="operation",
//! operation="CreateSlackChannelConfiguration,UpdateSlackChannelConfiguration,DeleteSlackChannelConfiguration",
//! model_path="services/service-com-amazonaws-supportapp-supportapp/smithy-model/com-amazonaws-supportapp-supportapp.json")`),
//! and it is the answer of the write that ADDS a configuration: a `teamId` the account's
//! registry does not hold has no workspace to add one to. The two writes that act on a
//! configuration which must already EXIST settle their answer on the resource the request
//! identifies instead, so an unregistered workspace — which holds no configuration at all
//! — draws the modelled `ResourceNotFoundException` (404) from them.
//! A client branches on the two: the 409 tells it to add the workspace first, the 404
//! that the channel was never configured.
//!
//! The rows run that rule over what it generalises over — which of the three writes is
//! called, what the registry holds besides the workspace the request names (nothing, one
//! other, the documented quota's worth) and whether that workspace was never registered
//! or registered and then removed, over `teamId` forms at both published `len 1..256`
//! bounds — and the second test drives each pair through the whole lifecycle, so the
//! answer is read from the registry at every call rather than decided once. The third
//! pins what still precedes both: a request the model itself refuses.
//!
//! Every assertion goes over the wire through the same router the production emulator
//! mounts, reusing the harness in [`crate::slack_channel_configurations`], so an answer
//! that is not computed from the persisted registry cannot pass.

use serde_json::{json, Value};
use service_com_amazonaws_supportapp_supportapp::state::MAX_SLACK_WORKSPACE_CONFIGURATIONS;

use crate::channel_configuration_workspace_precondition::{
    assert_channel_not_found, assert_workspace_conflict,
    assert_writes_on_an_unregistered_workspace, create_body, team_ids, update_body,
};
use crate::error_message_templates::{not_found, refusal};
use crate::slack_channel_configurations::{role_arn, Emulator};

/// `teamId` and `channelId` are `len 1..256, pattern ^\S+$`
/// (`smithy_query(query="operation", operation="DeleteSlackChannelConfiguration")`), so
/// the rows sit at both published bounds and one row goes past the upper one.
const SLACK_ID_MAX_LEN: usize = 256;

/// What the account's workspace registry holds when the write arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Registry {
    /// The named workspace was never registered.
    Never,
    /// It was registered, held a configuration for the very pair the write names, and
    /// the account then removed the workspace.
    Removed,
}

/// A team id for the `index`th OTHER workspace: prefixed so it can never collide with a
/// team id a row names.
fn other_team_id(index: usize) -> String {
    format!("TNEIGHBOUR{index}")
}

/// Register `others` other workspaces, each holding a configuration of its own, so no
/// row's answer can come from an account that holds nothing at all.
async fn arrange_neighbours(emulator: &Emulator, label: &str, others: usize) {
    for index in 0..others {
        let team_id = other_team_id(index);
        assert_eq!(
            emulator.register_workspace(&team_id).await.status,
            200,
            "case {label:?}: the other workspace {team_id:?} must be registered"
        );
        assert_eq!(
            emulator
                .create(create_body(&team_id, "CNEIGHBOUR"))
                .await
                .status,
            200,
            "case {label:?}: the other workspace's configuration must be created"
        );
    }
}

/// Bring the account to the state the row describes and hand back the emulator.
async fn arrange(label: &str, team_id: &str, channel_id: &str, state: Registry, others: usize) -> Emulator {
    let emulator = Emulator::new();
    arrange_neighbours(&emulator, label, others).await;

    if state == Registry::Removed {
        assert_eq!(
            emulator.register_workspace(team_id).await.status,
            200,
            "case {label:?}: the named workspace must be registrable"
        );
        assert_eq!(
            emulator.create(create_body(team_id, channel_id)).await.status,
            200,
            "case {label:?}: the configuration must be created before its workspace goes"
        );
        assert_eq!(
            emulator.delete_workspace(team_id).await.status,
            200,
            "case {label:?}: the named workspace must be removable"
        );
    }

    emulator
}

#[tokio::test]
async fn every_channel_configuration_write_before_the_workspace_is_refused() {
    // Rows: which registry state the named workspace is in, how much the account holds
    // besides it, and the form and length of the ids. Each row drives all three writes.
    let rows: [(&str, String, String, Registry, usize); 7] = [
        (
            "nothing registered at all",
            "T012ABCDEFG".to_string(),
            "C01234A5BCD".to_string(),
            Registry::Never,
            0,
        ),
        (
            "one other workspace registered",
            "T99999ZZZZZ".to_string(),
            "C99999ZZZZZ".to_string(),
            Registry::Never,
            1,
        ),
        (
            "the registry filled to the documented workspace quota",
            "T-mixed.Case_9".to_string(),
            "C#9/x".to_string(),
            Registry::Never,
            MAX_SLACK_WORKSPACE_CONFIGURATIONS,
        ),
        (
            "the shortest legal ids",
            "T".to_string(),
            "C".to_string(),
            Registry::Never,
            0,
        ),
        (
            "the longest legal ids",
            "T".repeat(SLACK_ID_MAX_LEN),
            "C".repeat(SLACK_ID_MAX_LEN),
            Registry::Never,
            1,
        ),
        (
            "the workspace was registered and then removed",
            "T0123456789".to_string(),
            "C01234A5BCD".to_string(),
            Registry::Removed,
            1,
        ),
        (
            "the shortest legal ids, the workspace removed again",
            "t".to_string(),
            "c".to_string(),
            Registry::Removed,
            0,
        ),
    ];

    for (label, team_id, channel_id, state, others) in rows {
        let emulator = arrange(label, &team_id, &channel_id, state, others).await;

        // Twice: a refused write changes nothing, so the second call answers the same.
        for attempt in 1..=2 {
            assert_writes_on_an_unregistered_workspace(
                &emulator,
                &format!("{label} / attempt {attempt}"),
                &team_id,
                &channel_id,
            )
            .await;
        }

        // Nothing was written for the pair, and the other workspaces' configurations
        // are untouched: each refusal is a refusal, not a partial write.
        assert!(
            emulator.listed(&team_id, &channel_id).await.is_none(),
            "case {label:?}: a refused write must store nothing for the pair, got {}",
            emulator.list(None).await.body
        );
        assert_eq!(
            emulator.list(None).await.configurations().len(),
            others,
            "case {label:?}: expected exactly {others} stored configurations, got {}",
            emulator.list(None).await.body
        );
    }
}

#[tokio::test]
async fn adding_the_workspace_is_what_makes_the_create_reachable() {
    // The registry is read at EVERY create, so one pair walks the whole lifecycle: the
    // conflict before the workspace is added, the miss from the two writes that need a
    // configuration, HTTP 200 for all three once the workspace is there and the channel
    // configured, the miss again once it is deleted, and the create's conflict again once
    // the workspace itself goes. Rows differ in the team id form and length.
    for (label, team_id) in team_ids() {
        let emulator = Emulator::new();
        let channel_id = "C01234A5BCD";

        assert_writes_on_an_unregistered_workspace(
            &emulator,
            &format!("{label}, before the workspace"),
            &team_id,
            channel_id,
        )
        .await;

        assert_eq!(
            emulator.register_workspace(&team_id).await.status,
            200,
            "case {label:?}: the workspace must be registrable"
        );

        // Inside the registered workspace the two writes that act on an EXISTING
        // configuration report the same miss they reported before it was registered.
        for (operation, reply) in [
            (
                "UpdateSlackChannelConfiguration",
                emulator.update(update_body(&team_id, channel_id)).await,
            ),
            (
                "DeleteSlackChannelConfiguration",
                emulator.delete(&team_id, channel_id).await,
            ),
        ] {
            let case = format!("{label} / {operation}, workspace registered");
            let message = refusal(&case, &reply, 404, "ResourceNotFoundException");
            let expected = not_found(&team_id, channel_id);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }

        for (operation, reply) in [
            (
                "CreateSlackChannelConfiguration",
                emulator.create(create_body(&team_id, channel_id)).await,
            ),
            (
                "UpdateSlackChannelConfiguration",
                emulator.update(update_body(&team_id, channel_id)).await,
            ),
            (
                "DeleteSlackChannelConfiguration",
                emulator.delete(&team_id, channel_id).await,
            ),
        ] {
            assert_eq!(
                reply.status, 200,
                "case {label:?} / {operation}: expected HTTP 200 inside the registered \
                 workspace, got {} / {}",
                reply.status, reply.body
            );
        }

        // The workspace is still registered, so the delete of the pair that is gone is
        // the miss and not the conflict.
        let repeated = emulator.delete(&team_id, channel_id).await;
        let case = format!("{label} / repeated delete, workspace registered");
        let message = refusal(&case, &repeated, 404, "ResourceNotFoundException");
        let expected = not_found(&team_id, channel_id);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        // And removing the workspace takes the create's precondition back with it.
        assert_eq!(
            emulator.delete_workspace(&team_id).await.status,
            200,
            "case {label:?}: the workspace must be removable"
        );
        assert_writes_on_an_unregistered_workspace(
            &emulator,
            &format!("{label}, after the workspace went"),
            &team_id,
            channel_id,
        )
        .await;
    }
}

#[tokio::test]
async fn a_request_the_model_refuses_is_refused_before_the_precondition() {
    // The account-level precondition is judged against the account's state, so it is
    // judged only for a request the service accepts at all: a value outside a modelled
    // constraint is the modelled `ValidationException` (400) even though the workspace
    // the request names is not registered. Rows span the three writes and the members
    // each of them carries.
    let rows: [(&str, &str, Value); 6] = [
        (
            "delete with an empty teamId",
            "delete-slack-channel-configuration",
            json!({ "teamId": "", "channelId": "C01234A5BCD" }),
        ),
        (
            "delete with whitespace in the channelId",
            "delete-slack-channel-configuration",
            json!({ "teamId": "T012ABCDEFG", "channelId": "C0123 A5BCD" }),
        ),
        (
            "delete with a teamId past the published length bound",
            "delete-slack-channel-configuration",
            json!({
                "teamId": "T".repeat(SLACK_ID_MAX_LEN + 1),
                "channelId": "C01234A5BCD",
            }),
        ),
        (
            "update with a channelName outside its declared pattern",
            "update-slack-channel-configuration",
            json!({
                "teamId": "T012ABCDEFG",
                "channelId": "C01234A5BCD",
                "channelName": "aws\rsupport",
            }),
        ),
        (
            "create with the documented severity rule broken",
            "create-slack-channel-configuration",
            json!({
                "teamId": "T012ABCDEFG",
                "channelId": "C01234A5BCD",
                "notifyOnCaseSeverity": "none",
                "notifyOnResolveCase": true,
                "channelRoleArn": role_arn("SupportAppRole"),
            }),
        ),
        (
            "create with a severity the enum does not name",
            "create-slack-channel-configuration",
            json!({
                "teamId": "T012ABCDEFG",
                "channelId": "C01234A5BCD",
                "notifyOnCaseSeverity": "urgent",
                "channelRoleArn": role_arn("SupportAppRole"),
            }),
        ),
    ];

    for (label, operation, body) in rows {
        // A fresh account: nothing is registered, so the precondition WOULD refuse each
        // of these requests if it were judged first.
        let emulator = Emulator::new();
        let reply = emulator.call(operation, body).await;
        assert_eq!(
            reply.status, 400,
            "case {label:?}: expected HTTP 400, got {} / {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_type, "ValidationException",
            "case {label:?}: expected ValidationException, got {:?} / {}",
            reply.error_type, reply.body
        );
    }
}

#[tokio::test]
async fn each_refusal_is_stated_over_the_resource_the_request_named() {
    // The conflict's message is built from the request's `teamId` and the account the
    // emulator presents, and the miss's from the pair the request named, so two writes
    // naming different unregistered workspaces in the same account draw two different
    // messages, each naming its own team.
    let emulator = Emulator::new();
    let registered = "T012ABCDEFG";
    assert_eq!(
        emulator.register_workspace(registered).await.status,
        200,
        "the workspace the account does hold must be registered"
    );

    for team_id in ["T99999ZZZZZ", "t012abcdefg", &"T".repeat(SLACK_ID_MAX_LEN)] {
        let channel_id = "C01234A5BCD";
        assert_workspace_conflict(
            &format!("create naming {team_id:?}"),
            team_id,
            &emulator.create(create_body(team_id, channel_id)).await,
        );
        assert_channel_not_found(
            &format!("delete naming {team_id:?}"),
            team_id,
            channel_id,
            &emulator.delete(team_id, channel_id).await,
        );
        assert_channel_not_found(
            &format!("update naming {team_id:?}"),
            team_id,
            channel_id,
            &emulator.update(update_body(team_id, channel_id)).await,
        );
    }
}
