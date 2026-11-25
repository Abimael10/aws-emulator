Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 266e6f7f0ec6488ec821cc522a9effa1af99936b

# Handler-applied `@pattern` refusals are not combined: a request whose teamId AND channelName are both outside their declared expressions reports only one clause

_Filed: 2026-11-25T05:28:44Z_
_Operation: CreateSlackChannelConfiguration | Class: error-response_

> _Note: this issue was identified against the code base at commit `266e6f7f0ec6488ec821cc522a9effa1af99936b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The create and update handlers apply three modelled constraints by hand, each through its own early return: `channel_pair_refusal` (teamId + channelId), `channel_name_refusal` (channelName) and `channel_role_arn_refusal` (channelRoleArn's `@length` maximum). Each of those renders its OWN validation envelope and the handler returns on the first one that fires, so a single request that breaks two of them is refused with a `1 validation error detected: ...` envelope naming only the member whose rule happened to run first; the other clause is dropped.

That contradicts the rule this crate states for exactly this situation, and the rule its own suite pins: `declared_pattern_violation` is deliberately kept apart from the rendering "so a request whose SEVERAL members are each outside their own declared expression is refused in ONE counted envelope naming every clause, exactly as the walk refuses several violations it caught itself" (state.rs:718-722). The split IS used inside `channel_pair_refusal`, which reports teamId and channelId together, but never across the three by-hand rules.

The result is that the SAME request shape is refused two different ways depending only on which characters it carries — i.e. on which layer catches it, which is precisely what the suite says must not happen. A teamId carrying a space plus a channelName carrying a line feed is caught by the boundary walk and reported in ONE envelope with two clauses; a teamId carrying U+FEFF plus a channelName carrying U+2028 is caught by the handler rules and reported with ONE clause about teamId only, silently losing the channelName clause. Combining the by-hand violations before rendering (the composition `declared_pattern_violation` already exists for) leaves every existing single-member and pair test byte-identical, since one violation still renders `1 validation error detected: ` and the pair still renders both clauses in model order.

## Evidence

The rule the crate states — services/service-com-amazonaws-supportapp-supportapp/src/state.rs:718-722:
```
/// The single clause a value outside its member's own declared `@pattern` is refused with,
/// as the framework's own violation — the one [`declared_pattern_refusal`] renders, kept
/// apart from the rendering so a request whose SEVERAL members are each outside their own
/// declared expression is refused in ONE counted envelope naming every clause, exactly as
/// the walk refuses several violations it caught itself.
fn declared_pattern_violation(
```

The code that breaks it — src/ops/create_slack_channel_configuration.rs:223-251, two independent early returns, each rendering its own envelope:
```
        if let Some(message) =
            crate::state::channel_pair_refusal(&input.team_id, &input.channel_id)
        {
            return Err(CreateSlackChannelConfigurationErrorModeled::ValidationException(
...
        if let Some(message) = input
            .channel_name
            .as_deref()
            .and_then(crate::state::channel_name_refusal)
        {
            return Err(CreateSlackChannelConfigurationErrorModeled::ValidationException(
```
The same two-return shape is in src/ops/update_slack_channel_configuration.rs:238-268, followed by a third independent return for `channelRoleArn`'s by-hand `@length` clause (create: lines 259-277; update: lines 277-299).

The renderers each close over a single violation — state.rs:691-696 (`channel_name_refusal` -> `declared_pattern_refusal(CHANNEL_NAME_PATH, ...)`) and state.rs:713-716 (`envelope(::std::slice::from_ref(&violation))`), while state.rs:799-806 (`slack_id_refusal`) collects a Vec and is the only call site that ever passes more than one.

The suite states the invariant this breaks — tests/all/slack_id_pattern.rs:377-382:
```
async fn a_create_whose_whole_pair_is_outside_the_pattern_states_both_clauses() {
    // Two members of one request, each outside its own declared expression: one counted
    // envelope naming both clauses, in the order the model declares the members — the same
    // answer whichever layer read the expression, so the boundary's own engine (the space
    // row) and the rule the handler applies (the U+FEFF row) agree on the shape of it.
```
and the boundary demonstrably DOES combine across members — tests/all/error_message_templates.rs:656-667 (`"2 validation errors detected: "` for `{"teamId": "T012 ABCDEFG", "channelId": "C0123 45"}`) and tests/all/channel_name_pattern.rs:386-398 (`"2 validation errors detected: Value '' at 'channelName' ..."`). No test covers a request whose teamId and channelName are BOTH caught by the handler rules, which is why the divergence passes unnoticed.

## Steps to Reproduce

1. Register the workspace `T012ABCDEFG` so the create reaches its input rules.
2. POST /control/create-slack-channel-configuration with a teamId and a channelName that are each caught by the BOUNDARY walk: `{"teamId":"T012 ABCDEFG","channelId":"C01234A5BCD","channelName":"aws\nsupport","notifyOnCaseSeverity":"none","channelRoleArn":"arn:aws:iam::000000000000:role/SupportAppRole"}` and observe one envelope counting clauses for both members (`2 validation errors detected: ...`), per tests/all/error_message_templates.rs:656-667.
3. Now POST the same request with the two values that are caught by the HANDLER rules instead: `{"teamId":"T012\uFEFFABCDEFG","channelId":"C01234A5BCD","channelName":"aws\u2028support","notifyOnCaseSeverity":"none","channelRoleArn":"arn:aws:iam::000000000000:role/SupportAppRole"}`.
4. Observe `1 validation error detected: ...` naming only `teamId`: `channel_pair_refusal` (src/ops/create_slack_channel_configuration.rs:223-232) returns first and the `channelName` clause from state.rs:691-696 is never rendered.
5. Repeat against /control/update-slack-channel-configuration (src/ops/update_slack_channel_configuration.rs:238-268) for the same result, and with an over-long `channelRoleArn` added to see that the third by-hand clause (state.rs:926-939) is dropped as well.
6. Compare with the invariant stated in tests/all/slack_id_pattern.rs:377-382 ("the same answer whichever layer read the expression") and with state.rs:718-722.
