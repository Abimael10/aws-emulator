// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a Slack channel configuration write draws: the resource the request
//! IDENTIFIES is settled before any precondition on the calling ACCOUNT.
//!
//! The rule under test: `UpdateSlackChannelConfiguration` and
//! `DeleteSlackChannelConfiguration` act on the `(teamId, channelId)` configuration their
//! request names, so a pair this account holds no configuration for is the modelled
//! `ResourceNotFoundException` (HTTP 404) — "The specified resource is missing or doesn't
//! exist, such as an account alias, Slack channel configuration, or Slack workspace
//! configuration" (`smithy_query(query="operation",
//! operation="UpdateSlackChannelConfiguration,DeleteSlackChannelConfiguration",
//! model_path="services/service-com-amazonaws-supportapp-supportapp/smithy-model/com-amazonaws-supportapp-supportapp.json")`)
//! — whatever the account's Slack workspace registry holds for that `teamId`. The
//! documented `ConflictException` (409) condition — "Add, update, or delete a Slack channel
//! configuration before you add a Slack workspace to your Amazon Web Services account"
//! (`smithy_query(query="doc", shape="ConflictException")`) — is the answer on the ADD
//! path, where the configuration the request names does not and cannot exist, and it never
//! pre-empts the answer that the channel was never configured.
//!
//! The rows differ along what the rule generalises over: which of the three writes is
//! called, what the account holds about the named workspace (nothing, a registration, a
//! registration it deleted), what it holds BESIDES it, and the form and length of the ids,
//! at both published `len 1..256` bounds. The last test shows the same calls on the same
//! ids answering 200 once the configuration IS stored, so each refusal is read from the
//! account's state rather than assumed from the request.

use serde_json::{json, Value};

use crate::channel_configuration_workspace_precondition::{create_body, update_body};
use crate::error_message_templates::{not_found, refusal};
use crate::slack_channel_configurations::{role_arn, Emulator, Reply};

/// `teamId` and `channelId` are `len 1..256, pattern ^\S+$`
/// (`smithy_query(query="operation", operation="DeleteSlackChannelConfiguration")`).
const SLACK_ID_MAX_LEN: usize = 256;

/// What the account's workspace registry holds about the workspace a row names.
#[derive(Debug, Clone, Copy)]
enum Registry {
    /// The account never registered it.
    Never,
    /// It is registered.
    Registered,
    /// It was registered, held a configuration for the pair the row names, and the account
    /// then deleted the workspace — which took that configuration with it.
    Deleted,
}

/// One row: the pair a write names, which the account holds no configuration for, and the
/// registry state it is looked up in.
struct Case {
    label: &'static str,
    team_id: String,
    channel_id: String,
    registry: Registry,
    /// How many OTHER workspaces the account has registered and configured, so no row's
    /// answer can come from an empty account.
    neighbours: usize,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "nothing registered at all",
            team_id: "T99999ZZZZZ".to_string(),
            channel_id: "C01234A5BCD".to_string(),
            registry: Registry::Never,
            neighbours: 0,
        },
        Case {
            label: "nothing registered for this team, two others registered",
            team_id: "T33A749D3D8".to_string(),
            channel_id: "C-other".to_string(),
            registry: Registry::Never,
            neighbours: 2,
        },
        Case {
            label: "the shortest legal ids, nothing registered",
            team_id: "T".to_string(),
            channel_id: "C".to_string(),
            registry: Registry::Never,
            neighbours: 0,
        },
        Case {
            label: "the longest legal ids, nothing registered",
            team_id: "T".repeat(SLACK_ID_MAX_LEN),
            channel_id: "C".repeat(SLACK_ID_MAX_LEN),
            registry: Registry::Never,
            neighbours: 1,
        },
        Case {
            label: "the workspace is registered, the channel is not configured",
            team_id: "T012ABCDEFG".to_string(),
            channel_id: "C99999ZZZZZ".to_string(),
            registry: Registry::Registered,
            neighbours: 1,
        },
        Case {
            label: "a punctuated pair, the workspace registered",
            team_id: "T-mixed.Case_9".to_string(),
            channel_id: "C#9/x".to_string(),
            registry: Registry::Registered,
            neighbours: 0,
        },
        Case {
            label: "the configuration went with its deleted workspace",
            team_id: "T0123456789".to_string(),
            channel_id: "C01234A5BCD".to_string(),
            registry: Registry::Deleted,
            neighbours: 1,
        },
    ]
}

/// The team id of the `index`th neighbouring workspace, prefixed so it cannot collide with
/// an id a row names.
fn neighbour_team(index: usize) -> String {
    format!("TOTHER{index}")
}

/// Build the account state `case` describes, leaving it holding no configuration for the
/// pair the row names.
async fn arrange(case: &Case) -> Emulator {
    let emulator = Emulator::new();
    for index in 0..case.neighbours {
        let team_id = neighbour_team(index);
        assert_eq!(
            emulator.register_workspace(&team_id).await.status,
            200,
            "case {:?}: the neighbouring workspace {team_id:?} must be registered",
            case.label
        );
        assert_eq!(
            emulator
                .create(create_body(&team_id, "COTHER"))
                .await
                .status,
            200,
            "case {:?}: the neighbour's configuration must be created",
            case.label
        );
    }
    match case.registry {
        Registry::Never => {}
        Registry::Registered => {
            assert_eq!(
                emulator.register_workspace(&case.team_id).await.status,
                200,
                "case {:?}: the named workspace must be registrable",
                case.label
            );
        }
        Registry::Deleted => {
            assert_eq!(
                emulator.register_workspace(&case.team_id).await.status,
                200,
                "case {:?}: the named workspace must be registrable",
                case.label
            );
            assert_eq!(
                emulator
                    .create(create_body(&case.team_id, &case.channel_id))
                    .await
                    .status,
                200,
                "case {:?}: the configuration must be created before its workspace goes",
                case.label
            );
            assert_eq!(
                emulator.delete_workspace(&case.team_id).await.status,
                200,
                "case {:?}: the named workspace must be removable",
                case.label
            );
        }
    }
    emulator
}

/// Assert `reply` is the modelled miss for `(team_id, channel_id)`: HTTP 404, the error
/// type on the wire, and the one template this service words a miss with.
fn assert_miss(case: &str, team_id: &str, channel_id: &str, reply: &Reply) {
    let message = refusal(case, reply, 404, "ResourceNotFoundException");
    let expected = not_found(team_id, channel_id);
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?}, got {message:?}"
    );
}

#[tokio::test]
async fn a_configuration_the_account_does_not_hold_is_the_modelled_miss() {
    for case in cases() {
        let emulator = arrange(&case).await;

        // Twice, and through both writes: the refusal changed nothing, so every call
        // answers the same.
        for attempt in 1..=2 {
            assert_miss(
                &format!("{} / delete, attempt {attempt}", case.label),
                &case.team_id,
                &case.channel_id,
                &emulator.delete(&case.team_id, &case.channel_id).await,
            );
            assert_miss(
                &format!("{} / update, attempt {attempt}", case.label),
                &case.team_id,
                &case.channel_id,
                &emulator
                    .update(update_body(&case.team_id, &case.channel_id))
                    .await,
            );
        }

        // A refused write stored nothing, and took nothing with it.
        assert!(
            emulator
                .listed(&case.team_id, &case.channel_id)
                .await
                .is_none(),
            "case {:?}: a refused write must not create the configuration it refused, got {}",
            case.label,
            emulator.list(None).await.body
        );
        assert_eq!(
            emulator.list(None).await.configurations().len(),
            case.neighbours,
            "case {:?}: expected exactly {} stored configurations, got {}",
            case.label,
            case.neighbours,
            emulator.list(None).await.body
        );
    }
}

#[tokio::test]
async fn the_add_path_still_answers_the_documented_conflict() {
    // The ADD is the write the conflict condition is about: it has no workspace to add a
    // configuration to, so it is the modelled 409 — and once the workspace is registered
    // the very same request is served, which is what makes the two answers distinguishable
    // to a caller.
    for case in cases() {
        let label = case.label;
        let emulator = arrange(&case).await;
        let created = emulator
            .create(create_body(&case.team_id, &case.channel_id))
            .await;
        match case.registry {
            Registry::Never | Registry::Deleted => {
                let message = refusal(label, &created, 409, "ConflictException");
                let expected = format!(
                    "Workspace installation not found for account {} and team {}",
                    engine::emulator_account_id(),
                    case.team_id
                );
                assert_eq!(
                    message, expected,
                    "case {label:?}: expected {expected:?}, got {message:?}"
                );
            }
            Registry::Registered => {
                assert_eq!(
                    created.status, 200,
                    "case {label:?}: expected the add in a registered workspace to be \
                     served, got {} / {}",
                    created.status, created.body
                );
            }
        }
    }
}

#[tokio::test]
async fn the_answers_are_read_from_the_account_state() {
    // The same ids, the same calls: once the workspace is registered and the pair IS
    // configured, both writes answer 200 — and the delete of what is gone is the miss
    // again. So the refusals above are computed from the account's state.
    for case in cases() {
        let label = case.label;
        let emulator = Emulator::new();
        assert_eq!(
            emulator.register_workspace(&case.team_id).await.status,
            200,
            "case {label:?}: the workspace must be registrable"
        );
        let mut body = create_body(&case.team_id, &case.channel_id);
        body["channelRoleArn"] = json!(role_arn("SupportAppRole"));
        assert_eq!(
            emulator.create(body).await.status,
            200,
            "case {label:?}: the configuration must be creatable"
        );
        assert_eq!(
            emulator
                .update(update_body(&case.team_id, &case.channel_id))
                .await
                .status,
            200,
            "case {label:?}: expected the update of the stored configuration to be served"
        );
        assert_eq!(
            emulator.delete(&case.team_id, &case.channel_id).await.status,
            200,
            "case {label:?}: expected the delete of the stored configuration to be served"
        );
        assert_miss(
            &format!("{label} / delete after the delete"),
            &case.team_id,
            &case.channel_id,
            &emulator.delete(&case.team_id, &case.channel_id).await,
        );
        assert_eq!(
            emulator.list(None).await.configurations(),
            Vec::<Value>::new(),
            "case {label:?}: expected the account to hold no configuration after the delete, \
             got {}",
            emulator.list(None).await.body
        );
    }
}
