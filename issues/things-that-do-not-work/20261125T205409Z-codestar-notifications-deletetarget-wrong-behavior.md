Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 919514db1c4457ecce0a02ad6f08fc1eff91bbf9

# ForceUnsubscribeAll only drops the associations of the request's region, though the model says every notification rule in the account

_Filed: 2026-11-25T20:54:09Z_
_Operation: DeleteTarget | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `919514db1c4457ecce0a02ad6f08fc1eff91bbf9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DeleteTarget` resolves the rules that hold the address with `rules_subscribed_to(&region, address)`, which is documented as "The ARNs of the rules of THIS REGION that subscribe to `address`". With `ForceUnsubscribeAll` set to true, only those region-local associations are dropped; a rule of any other region keeps the address, the account-wide registry entry survives (its `subscribed_rules` set still holds the other region's ARNs), and `ListTargets` in that other region still reports the target. The model's own sentence for the member is unconditional about scope: all associations with "every notification rule in your Amazon Web Services account" are deleted.

The same region filter also makes the refusal branch and the success branch disagree with the stored state: an address that only rules of ANOTHER region subscribe to yields `subscribed.is_empty()`, so the call answers 200 having deleted nothing at all — a hollow success for an address the account demonstrably still associates with rules. The data needed for the documented behaviour is already in the store: `TargetRegistryEntry.subscribed_rules` is the account-wide set of subscribing rule ARNs, so the region filter is a narrowing of an existing capability, not a missing one.

## Evidence

Model, `DeleteTargetRequest$ForceUnsubscribeAll` (smithy_query doc): "A Boolean value that can be used to delete all associations with this Amazon Q Developer in chat applications topic. The default value is FALSE. If set to TRUE, all associations between that target and every notification rule in your Amazon Web Services account are deleted."

The handler's region-scoped resolution — `src/ops/delete_target.rs:162-182`:
```
// Every rule of THIS region subscribing to the address, resolved once. The
// region scoping lives in `state.rs`, beside the registry it indexes.
let subscribed = store.rules_subscribed_to(&region, &input.target_address);

if !subscribed.is_empty() && !input.force_unsubscribe_all {
    return Err(invalid(target_still_in_use(subscribed.len())));
}
...
for arn in &subscribed {
    if let Some(rule) = store.rule_in_region_mut(&region, arn) { ... }
    store.unregister_target(arn, &input.target_address);
}
```

The helper it calls — `src/state.rs:182-197`:
```
/// The ARNs of the rules of THIS REGION that subscribe to `address`.
///
/// The registry entry itself is account-wide, so it cannot answer this: a target of
/// one region is addressed through that region's endpoint only, like every rule.
pub fn rules_subscribed_to(&self, region: &str, address: &str) -> Vec<String> {
    self.rules.values().filter(|rule| rule.region == region) ...
```
while the account-wide set the documented behaviour needs is right there — `src/state.rs:74-85`: `pub struct TargetRegistryEntry { ... pub subscribed_rules: HashSet<String>, ... }`.

The crate's own expectation for the force path, stated in the state module — `src/state.rs:168-172`:
```
/// Dropping the empty entry is what keeps the registry bounded ... `DeleteTarget`
/// removes a target that rules DO still subscribe to (with `ForceUnsubscribeAll`),
/// one subscription at a time, so the entry leaves with the last of them.
```
with cross-region subscribers the entry does NOT leave.

Test coverage: `tests/all/delete_target.rs:269-344` (`deleting_an_unsubscribed_address_succeeds_and_deletes_nothing`) pins the divergence for every value of the member, including `Some(true)`, for an address "held only by another region" (line 273, 281, 316-327, 341-344); the stated reason is the suite's "a delete is served by a regional endpoint, so it touches only the rules of its own region" (`tests/all/delete_target.rs:15-16`) — the reason the quoted model sentence contradicts.

## Steps to Reproduce

1. POST /createNotificationRule signed for eu-west-1 with Targets [{"TargetType": "SNS", "TargetAddress": "arn:aws:sns:us-east-1:123456789012:shared"}] -> 200; note ARN E.
2. POST /createNotificationRule signed for us-east-1 with the SAME TargetAddress -> 200; note ARN U.
3. POST /deleteTarget signed for us-east-1 with {"TargetAddress": "arn:aws:sns:us-east-1:123456789012:shared", "ForceUnsubscribeAll": true} -> 200.
4. POST /describeNotificationRule signed for eu-west-1 with {"Arn": E}: the address is still in Targets, although the request asked for every association in the account to be deleted; POST /listTargets signed for eu-west-1: the address is still listed (the registry entry survived).
5. Variant (hollow success): with only rule E (eu-west-1) subscribing, POST /deleteTarget signed for us-east-1 with ForceUnsubscribeAll false -> 200 and nothing deleted, instead of the still-in-use refusal the handler raises when the subscriber happens to be region-local (src/ops/delete_target.rs:166-168).
