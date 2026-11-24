Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 8cab40901755f628c70a04240a31b0594cdae3ad

# A delete made before any Slack workspace is added answers 404 ResourceNotFoundException, although ConflictException documents that exact request as the conflict

_Filed: 2026-11-19T05:44:09Z_
_Operation: DeleteSlackChannelConfiguration | Class: error-response_

> _Note: this issue was identified against the code base at commit `8cab40901755f628c70a04240a31b0594cdae3ad`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ConflictException`'s own documentation enumerates the conditions that draw it, and the FIRST one names all three channel-configuration writes together: "Add, update, or delete a Slack channel configuration before you add a Slack workspace to your Amazon Web Services account." `DeleteSlackChannelConfiguration` binds `ConflictException` (HTTP 409) in its modelled error list, and this crate already owns the check for exactly that condition — `ComAmazonawsSupportappSupportappState::unregistered_workspace_conflict`, which `CreateSlackChannelConfiguration` calls to implement the "Add" half of the same sentence.

The delete handler never calls that helper and never constructs `ConflictException` at all (grep over `src/ops/` finds the variant only in its error enum declaration). Its only account-state decision is the `(teamId, channelId)` lookup, so a delete issued against an account whose workspace registry holds nothing for the request's `teamId` — precisely "delete a Slack channel configuration before you add a Slack workspace to your account" — is answered with the modelled `ResourceNotFoundException` (404) instead of the documented `ConflictException` (409). The modelled 409 is therefore unreachable on this operation: no input can produce it.

The handler states a reason for skipping the registry ("cannot add an answer here"), but the reason is an invented precedence rule — "whether that configuration EXISTS is the first thing settled" — and the model's sentence is what it contradicts: the model does not say the absent configuration is reported as absent, it says that request is the conflict. A client branches on 409 vs 404 (one tells it to add a workspace first, the other that the channel was never configured), so the two are not interchangeable.

The same omission is present in `UpdateSlackChannelConfiguration`, which the same documented sentence names ("update"); it is filed separately because it is a separate handler.

## Evidence

Smithy, the documented trigger — `smithy_query(query="doc",shape="ConflictException")`:
  "Your request has a conflict. For example, you might receive this error if you try the following: <li>Add, update, or delete a Slack channel configuration before you add a Slack workspace to your Amazon Web Services account.</li> ..."
and the operation binds it: OPERATIONS.md:44 — "## DeleteSlackChannelConfiguration ... **Errors:** `AccessDeniedException (HTTP 403)`, `ConflictException (HTTP 409)`, `InternalServerException (HTTP 500)`, `ResourceNotFoundException (HTTP 404)`, `ValidationException (HTTP 400)`".

Side 1 — the helper this crate already has, and the sibling that calls it.
`services/service-com-amazonaws-supportapp-supportapp/src/state.rs:358-360`:
```
    pub fn unregistered_workspace_conflict(&self, team_id: &str) -> Option<String> {
        (!self.workspace_is_registered(team_id)).then(|| workspace_not_registered_message(team_id))
    }
```
`services/service-com-amazonaws-supportapp-supportapp/src/ops/create_slack_channel_configuration.rs:298-312`:
```
        // The precondition `ConflictException` documents: "Add, update, or delete a
        // Slack channel configuration before you add a Slack workspace to your Amazon
        // Web Services account." ...
        if let Some(message) = store.unregistered_workspace_conflict(&input.team_id) {
            return Err(CreateSlackChannelConfigurationErrorModeled::ConflictException(
                ConflictException { message: Some(message) },
            )
            .into());
        }
```

Side 2 — the delete path, which skips it and answers the miss instead.
`services/service-com-amazonaws-supportapp-supportapp/src/ops/delete_slack_channel_configuration.rs:186-206` (the whole of its account-state logic):
```
        let mut store = state.lock();
        // The resource this delete names is looked up FIRST, so an absent configuration
        // is reported as absent rather than behind an account-level precondition: a
        // pair this account holds nothing for is the modelled
        // `ResourceNotFoundException`, which is also what a repeated delete answers.
        let key = (input.team_id.clone(), input.channel_id.clone());
        if store.slack_channel_configurations.remove(&key).is_none() {
            return Err(
                DeleteSlackChannelConfigurationErrorModeled::ResourceNotFoundException(
                    ResourceNotFoundException { message: Some(
                        crate::state::slack_channel_configuration_not_found(
                            &input.team_id, &input.channel_id,
                        ),
                    ) },
                )
                .into(),
            );
        }
```
The stated reason the model contradicts — same file, `:15-22`:
```
//! The account-level precondition `ConflictException` documents ("Add, update, or
//! delete a Slack channel configuration before you add a Slack workspace to your
//! Amazon Web Services account") cannot add an answer here: ... so
//! the registry is consulted where that conflict is the applicable answer, on the
//! create path, and a configuration that is simply absent is reported as absent.
```
and `src/state.rs:352-357`: "An operation that acts on an EXISTING configuration settles that configuration's existence first instead, and answers the miss with [`slack_channel_configuration_not_found`] ... so this registry has no answer to add once the lookup has missed."

Proof the modelled 409 is unreachable on this operation: grep for `ConflictException` under `src/ops/` lists `delete_slack_channel_configuration.rs:111` (`ConflictException(ConflictException),` — the error-enum variant) and the two doc lines only; no construction site exists in that file.

Test coverage that currently pins the 404 (and would need the same correction): `tests/all/channel_configuration_workspace_precondition.rs:94-120`:
```
/// Assert what each of the three channel-configuration writes answers for a
/// `(team_id, channel_id)` pair that is NOT configured and whose workspace the account
/// has not registered: the create has no workspace to add a configuration to, so it is
/// the documented conflict, while the update and the delete name a configuration that
/// does not exist, which is the miss.
...
    assert_channel_not_found(
        &format!("{label} / DeleteSlackChannelConfiguration"),
```
with `assert_channel_not_found` at `:63-70` asserting `refusal(case, reply, 404, "ResourceNotFoundException")`.

## Steps to Reproduce

1. Mount the Support App service on the shared engine and sign requests with credentials the IAM gate allows.
2. Do NOT register any Slack workspace, so `state.slack_workspace_configurations` is empty (the account has added no Slack workspace).
3. POST /control/create-slack-channel-configuration with {"teamId":"T012ABCDEFG","channelId":"C01234A5BCD","notifyOnCaseSeverity":"none","channelRoleArn":"arn:aws:iam::<emulator-account>:role/SupportAppRole"} and observe HTTP 409 ConflictException "Workspace installation not found for account <account> and team T012ABCDEFG" — the crate implementing the "Add" half of the documented sentence.
4. POST /control/delete-slack-channel-configuration with {"teamId":"T012ABCDEFG","channelId":"C01234A5BCD"} — the "delete a Slack channel configuration before you add a Slack workspace to your account" case the SAME documented sentence names.
5. Observe HTTP 404 ResourceNotFoundException "Slack channel configuration for team T012ABCDEFG and channel C01234A5BCD not found" instead of the modelled 409 ConflictException, even though `DeleteSlackChannelConfiguration` binds ConflictException and the crate already holds the check (`src/state.rs:358-360`) that the sibling create path calls.
