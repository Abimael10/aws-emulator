Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 1315824e0589e1b16157ccfcca9c563287319f33

# Subscribe overwrites the account-wide registry's TargetType, so ListTargets and DescribeNotificationRule report two different types for one address

_Filed: 2026-11-25T20:14:51Z_
_Operation: Subscribe | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `1315824e0589e1b16157ccfcca9c563287319f33`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

A target address's TYPE is stored twice: once per rule in `RuleTarget.target_type`, and once account-wide in `TargetRegistryEntry.target_type`. `state::register_target` — called by `Subscribe`, `CreateNotificationRule` and `UpdateNotificationRule` — unconditionally overwrites the registry copy with whatever the latest call said, while leaving every other rule's own copy alone. The two readers then disagree: `DescribeNotificationRule` reports `TargetType` from the rule's own `RuleTarget`, whereas `ListTargets` reports it from the registry entry. So after rule B subscribes an address that rule A already carries under a different type, `ListTargets` reports the address under B's type, `DescribeNotificationRule` on A still reports A's type, and a `ListTargets` call filtered by `TARGET_TYPE` with A's type no longer finds the address at all (`target_satisfies` compares `entry.target_type` exactly).

This is the exact failure mode the crate deliberately designed out for the target's STATUS: the status was deliberately NOT stored on `RuleTarget` so that "a rule and `ListTargets` cannot report two different statuses for one address", and `DescribeNotificationRule` duly reads the status through `store.target_status(..)` while reading the type from the rule. The type was left duplicated, so the stated invariant holds for one member of `TargetSummary` and is broken for the one beside it.

## Evidence

The crate's own rule, stated on the per-rule target struct — `src/state.rs:61-68`:
```
/// A target subscribed to one rule. The target's STATUS is not stored here: it belongs
/// to the account-wide [`TargetRegistryEntry`], so a rule and `ListTargets` cannot
/// report two different statuses for one address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleTarget {
    pub target_address: String,
    pub target_type: String,
}
```
The unconditional overwrite of the account-wide copy — `src/state.rs:152-162`:
```
let entry = self
    .targets
    .entry(target.target_address.clone())
    .or_insert_with(|| TargetRegistryEntry {
        target_type: target.target_type.clone(),
        status: TARGET_STATUS_ACTIVE.to_string(),
        subscribed_rules: HashSet::new(),
        sequence,
    });
entry.target_type = target.target_type.clone();
entry.subscribed_rules.insert(rule_arn.to_string());
```
reached from `src/ops/subscribe.rs:245` (`store.register_target(&arn, &target);`), `src/ops/create_notification_rule.rs:349-351` and `src/ops/update_notification_rule.rs:293-295`.

The two readers that then disagree. `src/ops/describe_notification_rule.rs:208-216` takes the TYPE from the rule but the STATUS from the registry:
```
let targets = rule
    .targets
    .iter()
    .map(|target| TargetSummary {
        target_address: Some(target.target_address.clone()),
        target_type: Some(target.target_type.clone()),
        target_status: Some(store.target_status(&target.target_address)),
    })
```
`src/ops/list_targets.rs:196-205` takes the TYPE from the registry:
```
.map(|(address, entry)| {
    (
        entry.sequence,
        TargetSummary {
            target_address: Some(address.to_string()),
            target_type: Some(entry.target_type.clone()),
            target_status: Some(entry.status.clone()),
        },
    )
})
```
and the filter compares that same overwritten copy — `src/ops/list_targets.rs:96`: `"TARGET_TYPE" => entry.target_type == value,`.

Test coverage: no existing test subscribes one address under two types — `tests/all/rule_id_identity.rs:559-567` deliberately derives the type FROM the address (`fn narrow_target_type(address: &str)`, "a chat configuration is a Chatbot target, everything else here is a topic"), and `tests/all/list_targets.rs:64,147` seeds one type per address and asserts it back. So the divergence is unpinned, and no test changes if the two copies are made to agree.

## Steps to Reproduce

1. POST /createNotificationRule (rule A) with Targets: [{"TargetType": "SNS", "TargetAddress": "arn:aws:sns:us-east-1:123456789012:shared"}]; note A's Arn.
2. POST /createNotificationRule (rule B) with a different Name and Targets: [] ; note B's Arn.
3. POST /subscribe with {"Arn": <B>, "Target": {"TargetType": "AWSChatbotSlack", "TargetAddress": "arn:aws:sns:us-east-1:123456789012:shared"}}.
4. POST /describeNotificationRule with {"Arn": <A>}: Targets[0].TargetType is "SNS" (read from the rule, src/ops/describe_notification_rule.rs:213).
5. POST /listTargets with {}: the same address is reported with TargetType "AWSChatbotSlack" (read from the overwritten registry entry, src/ops/list_targets.rs:201) — one address, two types.
6. POST /listTargets with {"Filters": [{"Name": "TARGET_TYPE", "Value": "SNS"}]}: the address rule A subscribes under SNS is not returned at all (src/ops/list_targets.rs:96).
