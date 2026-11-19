Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 8cab40901755f628c70a04240a31b0594cdae3ad

# An update made before any Slack workspace is added answers 404 ResourceNotFoundException, although ConflictException documents that exact request as the conflict

_Filed: 2026-11-19T05:44:09Z_
_Operation: UpdateSlackChannelConfiguration | Class: error-response_

> _Note: this issue was identified against the code base at commit `8cab40901755f628c70a04240a31b0594cdae3ad`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The same documented `ConflictException` trigger that names "Add" and "delete" also names "update": "Add, update, or delete a Slack channel configuration before you add a Slack workspace to your Amazon Web Services account." `UpdateSlackChannelConfiguration` binds `ConflictException` (HTTP 409) in its modelled error list, and the crate already owns the check for that condition in `ComAmazonawsSupportappSupportappState::unregistered_workspace_conflict`, which `CreateSlackChannelConfiguration` calls.

The update handler never calls that helper and never constructs `ConflictException` anywhere, so the modelled 409 is unreachable on this operation. An update issued against an account whose workspace registry holds nothing for the request's `teamId` is answered with `ResourceNotFoundException` (404) instead of the documented 409, collapsing two answers a client branches on ("add a Slack workspace to your account first" vs "that channel was never configured") into one.

The handler states a reason — the configuration's existence is "the first thing settled", so the registry "cannot add an answer here" — but that is an invented precedence rule, and it is exactly what the model's sentence contradicts: the model does not say the absent configuration is reported absent, it says that request is the conflict.

This is a different defect from the open issue on this operation (`20261110T061300Z-...-updateslackchannelconfiguration-wrong-behavior`, which is about the `notifyOnCaseSeverity` `none` clause being applied from the stored severity); it concerns which error class an unregistered-workspace update draws.

## Evidence

Smithy, the documented trigger — `smithy_query(query="doc",shape="ConflictException")`:
  "Your request has a conflict. For example, you might receive this error if you try the following: <li>Add, update, or delete a Slack channel configuration before you add a Slack workspace to your Amazon Web Services account.</li> ..."
and the operation binds it: OPERATIONS.md:96 — "**Errors:** `AccessDeniedException (HTTP 403)`, `ConflictException (HTTP 409)`, `InternalServerException (HTTP 500)`, `ResourceNotFoundException (HTTP 404)`, `ValidationException (HTTP 400)`" for `## UpdateSlackChannelConfiguration`.

Side 1 — the check the crate already has, called by the sibling write.
`services/service-com-amazonaws-supportapp-supportapp/src/state.rs:358-360`:
```
    pub fn unregistered_workspace_conflict(&self, team_id: &str) -> Option<String> {
        (!self.workspace_is_registered(team_id)).then(|| workspace_not_registered_message(team_id))
    }
```
`services/service-com-amazonaws-supportapp-supportapp/src/ops/create_slack_channel_configuration.rs:305-312`:
```
        if let Some(message) = store.unregistered_workspace_conflict(&input.team_id) {
            return Err(CreateSlackChannelConfigurationErrorModeled::ConflictException(
                ConflictException { message: Some(message) },
            )
            .into());
        }
```

Side 2 — the update path, which skips it.
`services/service-com-amazonaws-supportapp-supportapp/src/ops/update_slack_channel_configuration.rs:292-312`:
```
        // The resource this update names is looked up FIRST, so an absent configuration
        // is reported as absent rather than behind an account-level precondition: a
        // pair this account holds nothing for is the modelled
        // `ResourceNotFoundException`, which is what an update-or-create caller branches
        // on.
        let key = (input.team_id.clone(), input.channel_id.clone());
        let Some(stored) = store.slack_channel_configurations.get(&key) else {
            return Err(
                UpdateSlackChannelConfigurationErrorModeled::ResourceNotFoundException(
                    ResourceNotFoundException { message: Some(
                        crate::state::slack_channel_configuration_not_found(
                            &input.team_id, &input.channel_id,
                        ),
                    ) },
                )
                .into(),
            );
        };
```
The stated reason the model contradicts — same file, `:28-35`:
```
//! The account-level precondition `ConflictException` documents ("Add, update, or
//! delete a Slack channel configuration before you add a Slack workspace to your
//! Amazon Web Services account") cannot add an answer here: a stored configuration
//! always lives in a workspace this account has registered ... so
//! the registry is consulted where that conflict is the applicable answer, on the
//! create path.
```
Proof the modelled 409 is unreachable here: grep for `ConflictException` under `src/ops/` lists `update_slack_channel_configuration.rs:28`, `:129` and `:164` only — a doc line, the generated error-doc line, and the enum variant `ConflictException(ConflictException),`; there is no construction site in the handler.

Test coverage that currently pins the 404: `tests/all/channel_configuration_workspace_precondition.rs:94-115` — "the create has no workspace to add a configuration to, so it is the documented conflict, while the update and the delete name a configuration that does not exist, which is the miss", driving `assert_channel_not_found(... emulator.update(update_body(team_id, channel_id)) ...)`, with `assert_channel_not_found` at `:63-70` asserting 404 `ResourceNotFoundException`.

## Steps to Reproduce

1. Mount the Support App service on the shared engine and sign requests with credentials the IAM gate allows.
2. Register no Slack workspace, so `state.slack_workspace_configurations` is empty — the account has added no Slack workspace.
3. POST /control/update-slack-channel-configuration with {"teamId":"T012ABCDEFG","channelId":"C01234A5BCD","notifyOnCaseSeverity":"none"} — the "update a Slack channel configuration before you add a Slack workspace to your account" case ConflictException's documentation names.
4. Observe HTTP 404 ResourceNotFoundException "Slack channel configuration for team T012ABCDEFG and channel C01234A5BCD not found" instead of the modelled 409 ConflictException.
5. Contrast the create on the same empty registry (POST /control/create-slack-channel-configuration with the same teamId plus a valid channelRoleArn), which answers 409 ConflictException from `store.unregistered_workspace_conflict` at src/ops/create_slack_channel_configuration.rs:305 — the same sentence, implemented on one of the two paths it names.
