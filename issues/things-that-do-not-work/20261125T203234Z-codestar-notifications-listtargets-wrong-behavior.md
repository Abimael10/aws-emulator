Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: ac876aa83f4d95da3df81ad77985626059f5d170

# ListTargets reports the last subscriber's TargetType for a shared address, disagreeing with the rules' own stored targets

_Filed: 2026-11-25T20:32:34Z_
_Operation: ListTargets | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `ac876aa83f4d95da3df81ad77985626059f5d170`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`TargetType` is stored TWICE: once per rule (`RuleTarget.target_type`, what `DescribeNotificationRule` reports) and once account-wide in the target registry (`TargetRegistryEntry.target_type`, what `ListTargets` reports and what the `TARGET_TYPE` filter matches). `register_target` unconditionally overwrites the registry copy with whatever type the newest subscriber gave, so the two copies drift on input the model allows (`Target$TargetType` is a free string with pattern `^[A-Za-z]+$`, no enum).

Two rules of one region subscribing to ONE address with different types (e.g. rule R1 with `SNS`, rule R2 with `AWSChatbotSlack`) leaves `ListTargets` reporting that address with `AWSChatbotSlack` only, while `DescribeNotificationRule` on R1 keeps reporting `SNS` — the listing is wrong for R1, and `Filters=[{TARGET_TYPE, SNS}]` no longer returns the address R1 subscribed as SNS. Worse, the overwrite survives the writer: delete R2 (or unsubscribe it), and `unregister_target` only drops R2 from `subscribed_rules` — it never restores the type — so the registry keeps reporting `AWSChatbotSlack` for an address only R1 (SNS) now subscribes to. That is stored state lost by a later unrelated call.

## Evidence

The overwrite — `src/state.rs:144-163`:
```
    pub fn register_target(&mut self, rule_arn: &str, target: &RuleTarget) {
        ...
        entry.target_type = target.target_type.clone();
        entry.subscribed_rules.insert(rule_arn.to_string());
    }
```
and the release path that does not undo it — `src/state.rs:173-180`:
```
    pub fn unregister_target(&mut self, rule_arn: &str, address: &str) {
        if let Some(entry) = self.targets.get_mut(address) {
            entry.subscribed_rules.remove(rule_arn);
```
The two readers that then disagree — `src/ops/list_targets.rs:196-204` reports the registry copy:
```
            .map(|(address, entry)| (entry.sequence, TargetSummary {
                        target_address: Some(address.to_string()),
                        target_type: Some(entry.target_type.clone()),
```
while `src/ops/describe_notification_rule.rs:208-216` reports the per-rule copy:
```
            .map(|target| TargetSummary {
                target_address: Some(target.target_address.clone()),
                target_type: Some(target.target_type.clone()),
```
The crate's own stated rule for the listing, which this breaks — `tests/all/list_targets.rs:164-166`:
```
/// RULE: a listing reports every target the region's rules subscribe to, each ONCE, with
/// the address and type it was subscribed with and the status the registry holds for it —
/// however many rules share the address.
```
and the design note that such divergence is what the registry exists to prevent — `src/state.rs:61-63`:
```
/// A target subscribed to one rule. The target's STATUS is not stored here: it belongs
/// to the account-wide [`TargetRegistryEntry`], so a rule and `ListTargets` cannot
/// report two different statuses for one address.
```
Test coverage: `tests/all/list_targets.rs:171-219` (`every_subscribed_target_is_reported_once_with_its_stored_members`) only shares an address between rules with the SAME type (`"TargetType": seeded[0].target_type`, line 193), and `tests/all/notification_rule_lifecycle.rs:1050-1093` pins re-typing within ONE rule; the cross-rule case is unpinned.

## Steps to Reproduce

1. POST /createNotificationRule (us-east-1) for rule R1 with Targets=[{TargetType: "SNS", TargetAddress: "arn:aws:sns:us-east-1:123456789012:shared"}].
2. POST /createNotificationRule (us-east-1) for rule R2 with Targets=[{TargetType: "AWSChatbotSlack", TargetAddress: "arn:aws:sns:us-east-1:123456789012:shared"}].
3. POST /listTargets (us-east-1) — the shared address is reported with TargetType AWSChatbotSlack only; POST /describeNotificationRule for R1 still reports SNS for the same address.
4. POST /listTargets with Filters=[{Name: TARGET_TYPE, Value: SNS}] — the address R1 subscribed as SNS is missing.
5. POST /deleteNotificationRule for R2, then POST /listTargets again — the address is still reported as AWSChatbotSlack although only R1 (SNS) subscribes to it.
