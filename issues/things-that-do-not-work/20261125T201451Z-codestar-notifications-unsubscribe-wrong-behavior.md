Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 1315824e0589e1b16157ccfcca9c563287319f33

# Removing a subscription leaves the Subscribe idempotency record, so replaying that token answers 200 and attaches nothing

_Filed: 2026-11-25T20:14:51Z_
_Operation: Unsubscribe | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `1315824e0589e1b16157ccfcca9c563287319f33`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`Subscribe` records `(Subscribe, ClientRequestToken) -> {arn, fingerprint}` in the idempotency ledger and, on a replay whose fingerprint matches, returns the stored ARN BEFORE it touches the rule's target list. Nothing removes that record when the subscription it created is dropped: `Unsubscribe` detaches the target and unregisters it, and `DeleteTarget` / `UpdateNotificationRule` do the same, but none of them forgets the Subscribe tokens that served the detached address. A caller that unsubscribes an address and then re-issues the identical Subscribe with the same token gets HTTP 200 with the rule ARN while the target is NOT re-attached: the subsequent DescribeNotificationRule/ListTargets show the address absent. A mutating call reports success and persists nothing — a silently lost write.

The crate already implements exactly this invariant for the rule resource and says so: `remove_rule` calls `forget_idempotent_requests_for` because "once the rule is gone, no token may report it as if it were still there", and `remember_idempotent_request` documents the intended consequence of having no memory left — "a replayed subscribe upserts the same target again". The subscription-removal paths skip the rule the rule-removal path applies.

## Evidence

The intent, stated in the state module — `src/state.rs:272-279`:
```
/// Forget every token that served `arn`: once the rule is gone, no token may report it
/// as if it were still there.
pub fn forget_idempotent_requests_for(&mut self, arn: &str) {
```
and `src/state.rs:250-254`:
```
/// Record what this `(operation, token)` pair served, forgetting the oldest entries
/// once the ledger is at [`MAX_IDEMPOTENCY_TOKENS`].
///
/// A forgotten token simply has no memory left: a replayed create then meets the
/// name-uniqueness check, and a replayed subscribe upserts the same target again.
```
The path that DOES apply it — `src/state.rs:232-242` (`remove_rule`): `self.forget_idempotent_requests_for(&removed.arn);` (line 241), reached from `src/ops/delete_notification_rule.rs:164`.

The subscription-removal path that does NOT — `src/ops/unsubscribe.rs:179-192`:
```
let before = rule.targets.len();
rule.targets
    .retain(|target| target.target_address != input.target_address);
let detached = rule.targets.len() != before;
if detached {
    rule.last_modified_timestamp = st::now_timestamp();
}
...
if detached {
    // The account-wide registry keeps the address only while some rule still
    // subscribes to it.
    store.unregister_target(&arn, &input.target_address);
}
```
(no `forget_*` call; the same omission at `src/ops/delete_target.rs:173-182` and `src/ops/update_notification_rule.rs:290-292`).

The replay that then claims success without touching the rule — `src/ops/subscribe.rs:187-203`:
```
if let Some(token) = input.client_request_token.as_deref() {
    if let Some(previous) = store.idempotent_request(Self::NAME, token) {
        if previous.fingerprint != fingerprint { ... }
        return Ok(SubscribeOutput {
            arn: Some(previous.arn.clone()),
        });
    }
}
```
The fingerprint is only `(Arn, TargetType, TargetAddress)` (`src/ops/subscribe.rs:171-175`), so the identical re-subscribe matches and the target push at `src/ops/subscribe.rs:227-241` is never reached.

Test coverage: `tests/all/notification_rule_lifecycle.rs:1550` (`one_token_used_on_create_and_subscribe_does_not_collide`) and `:668` (`an_idempotency_token_replays_the_first_rule`) are the only token suites; neither replays a Subscribe token after the subscription was removed, so the behaviour is unpinned. `src/state.rs:1047-1069` (`a_target_entry_is_released_when_its_last_rule_unsubscribes`) covers the registry release only.

## Steps to Reproduce

1. POST /createNotificationRule with a well-formed body and no targets; note the returned Arn R.
2. POST /subscribe with {"Arn": R, "Target": {"TargetType": "SNS", "TargetAddress": "arn:aws:sns:us-east-1:123456789012:topic"}, "ClientRequestToken": "sub-token-1"} -> 200.
3. POST /describeNotificationRule {"Arn": R}: Targets holds the address.
4. POST /unsubscribe with {"Arn": R, "TargetAddress": "arn:aws:sns:us-east-1:123456789012:topic"} -> 200; describe now shows Targets empty.
5. Re-POST the exact /subscribe body from step 2, same ClientRequestToken -> 200 with Arn R (the replay branch, src/ops/subscribe.rs:199-201).
6. POST /describeNotificationRule {"Arn": R} and POST /listTargets {}: the address is still absent — the subscribe reported success and attached nothing.
