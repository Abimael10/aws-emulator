Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: a2ee2ac1ad73d3bf094a20118f09dc75a352d305

# Hold the channel-configuration `(teamId, channelId)` presence rule in one place, as the sibling workspace-registry rule already is

_Filed: 2026-11-18T05:58:38Z_
_Operation: DeleteSlackChannelConfiguration | Class: maintainability_

> _Note: this issue was identified against the code base at commit `a2ee2ac1ad73d3bf094a20118f09dc75a352d305`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate deliberately holds its resource-presence rules in `src/state.rs` so two handlers cannot drift apart about them: the workspace-registry rule is a single method, `ComAmazonawsSupportappSupportappState::workspace_is_registered`, and its doc says in so many words that it exists so "the two handlers cannot come to disagree about whether a workspace is registered".

The sibling rule for the OTHER resource this service stores — "a Slack channel configuration is identified by its `(teamId, channelId)` pair, and a pair the account holds no configuration for is the modelled `ResourceNotFoundException`" — is not held anywhere. It is re-typed inline in every handler that needs it: `UpdateSlackChannelConfiguration` and `DeleteSlackChannelConfiguration` each build the key tuple themselves and each carry their own copy of the identical miss branch (same lookup on the same map, same `crate::state::slack_channel_configuration_not_found(&input.team_id, &input.channel_id)` message, same modelled error shape), and `CreateSlackChannelConfiguration` builds the same key a third time for its own `contains_key` test.

So the one rule exists as three copies of the key construction and two copies of the miss branch. They can drift — a change to how a configuration is keyed, or to which error a miss draws, has to be made in two or three files and nothing makes them agree — which is exactly the drift `workspace_is_registered` was written to prevent for the other resource. This crate has already folded two such inline copies back into state helpers (see the fixed issue "Fold RegisterSlackWorkspaceForOrganization's inline registry-presence test into the state helper that already implements it").

The fix is a state-module accessor for the pair-keyed lookup/miss (alongside `workspace_is_registered`), called by all three handlers. Every response, every status code and every message stays byte-identical, and no existing test changes.

## Evidence

The rule that IS held once, with the stated reason — services/service-com-amazonaws-supportapp-supportapp/src/state.rs:324-337:
```
    /// Whether this account's Slack workspace registry holds the workspace `team_id`
    /// identifies.
    ///
    /// The registry-presence rule itself, owned where the registry is owned: a workspace
    /// is registered to this account exactly when
    /// [`Self::slack_workspace_configurations`] holds its `teamId`. Both decisions that
    /// turn on it read it from here ... so the two handlers
    /// cannot come to disagree about whether a workspace is registered.
    pub fn workspace_is_registered(&self, team_id: &str) -> bool {
        self.slack_workspace_configurations.contains_key(team_id)
    }
```
and the module doc that claims the same ownership for the shared rules — src/state.rs:5-14: "this module holds the few rules the Support App handlers share ... so the operation files agree on one implementation of each instead of carrying copies that can drift apart."

Copy 1 of the pair lookup + miss — src/ops/delete_slack_channel_configuration.rs:191-206:
```
        let key = (input.team_id.clone(), input.channel_id.clone());
        if store.slack_channel_configurations.remove(&key).is_none() {
            return Err(
                DeleteSlackChannelConfigurationErrorModeled::ResourceNotFoundException(
                    ResourceNotFoundException {
                        message: Some(
                            crate::state::slack_channel_configuration_not_found(
                                &input.team_id,
                                &input.channel_id,
                            ),
                        ),
                    },
                )
                .into(),
            );
        }
```

Copy 2, the same rule re-typed — src/ops/update_slack_channel_configuration.rs:268-283:
```
        let key = (input.team_id.clone(), input.channel_id.clone());
        let Some(stored) = store.slack_channel_configurations.get(&key) else {
            return Err(
                UpdateSlackChannelConfigurationErrorModeled::ResourceNotFoundException(
                    ResourceNotFoundException {
                        message: Some(
                            crate::state::slack_channel_configuration_not_found(
                                &input.team_id,
                                &input.channel_id,
                            ),
                        ),
                    },
                )
                .into(),
            );
        };
```

The third inline copy of the key construction and presence test — src/ops/create_slack_channel_configuration.rs:289-293:
```
        let key = (input.team_id.clone(), input.channel_id.clone());
        // A channel configuration is identified by its `(teamId, channelId)` pair:
        // configuring a channel that is already configured is a conflict, not a
        // second configuration and not an overwrite.
        if store.slack_channel_configurations.contains_key(&key) {
```

That all four touches of the map are hand-written in handlers rather than behind a state accessor — `grep 'slack_channel_configurations\.(get|remove|contains_key|insert|retain)' src` matches only update_slack_channel_configuration.rs:269, delete_slack_channel_configuration.rs:192, create_slack_channel_configuration.rs:293 and :326 (plus the cascade `retain` in delete_slack_workspace_configuration.rs:202-204).

Test coverage that pins the behaviour the fix must leave unchanged: tests/all/channel_configuration_lookup_order.rs:6-18 ("`UpdateSlackChannelConfiguration` and `DeleteSlackChannelConfiguration` name one Slack channel configuration by its `(teamId, channelId)` pair, so a pair the account holds no configuration for is the modelled `ResourceNotFoundException` (HTTP 404) ... with the message template every operation of this service words a miss with"), tests/all/channel_configuration_pair_lookup.rs:6-17 (the partial-match rows for both operations), plus tests/all/resource_preconditions.rs and tests/all/slack_channel_configurations.rs.

## Steps to Reproduce

1. Open services/service-com-amazonaws-supportapp-supportapp/src/state.rs:324-337 and note that the workspace-registry presence rule is a single accessor whose doc says it exists so two handlers cannot disagree about it.
2. Grep the crate for `slack_channel_configurations.` and observe there is no equivalent accessor for the channel-configuration map: every touch is inline in a handler (create_slack_channel_configuration.rs:293/:326, delete_slack_channel_configuration.rs:192, update_slack_channel_configuration.rs:269).
3. Compare delete_slack_channel_configuration.rs:191-206 with update_slack_channel_configuration.rs:268-283: the key construction and the whole miss branch (same map, same shared message helper, same modelled `ResourceNotFoundException`) are two copies of one rule.
4. Change how a configuration is keyed, or which error a miss draws, and observe the edit has to be repeated in two (for the miss) or three (for the key) handler files, with nothing holding them in agreement — the drift `workspace_is_registered` prevents for the other resource.
5. Confirm the consolidation is behaviour-preserving: tests/all/channel_configuration_lookup_order.rs and tests/all/channel_configuration_pair_lookup.rs already assert the 404 status, the error type and the message for both operations, so folding the rule into one state accessor must leave every response and both suites unchanged.
