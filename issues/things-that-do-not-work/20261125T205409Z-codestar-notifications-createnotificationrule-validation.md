Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 919514db1c4457ecce0a02ad6f08fc1eff91bbf9

# Rule-name uniqueness is enforced per region, while the model and the service's own refusal text both state it is account-wide

_Filed: 2026-11-25T20:54:09Z_
_Operation: CreateNotificationRule | Class: validation_

> _Note: this issue was identified against the code base at commit `919514db1c4457ecce0a02ad6f08fc1eff91bbf9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The model states twice that a notification rule name is unique in the ACCOUNT, and the text this service answers the conflict with repeats it ("Notification rule names must be unique within an AWS account"). The implementation, however, indexes names per REGION: `rule_name_key` mixes the request's region into the key, so the second `CreateNotificationRule` with an already-taken name succeeds whenever it is signed for a different region, and no `ResourceAlreadyExistsException` is raised. The same per-region key is used by `UpdateNotificationRule`'s rename check (`src/ops/update_notification_rule.rs:223-234`), so a rename onto a name another region's rule holds is accepted too.

The result is a self-contradiction a caller can observe: the service tells one caller the name is refused because "names must be unique within an AWS account", and tells the next caller — same account, same name, another regional endpoint — that the name is free.

## Evidence

Model, `CreateNotificationRuleRequest$Name` (smithy_query doc): "The name for the notification rule. Notification rule names must be unique in your Amazon Web Services account."

Model, `ResourceAlreadyExistsException` (smithy_query doc): "A resource with the same name or ID already exists. Notification rule names must be unique in your Amazon Web Services account."

The text the service actually emits for the class — `src/state.rs:531-535`:
```
pub const RULE_NAME_ALREADY_IN_USE: &str =
    "The notification rule was not created because another notification rule already \
     exists with the same name. Notification rule names must be unique within an AWS \
     account. This helps prevent exceeding the maximum allowed number of notification \
     rules.";
```

The check that contradicts it — `src/state.rs:328-335`:
```
/// The key [`...State::rule_names`]
/// is indexed by: a rule name is unique within one region of the account, so the same
/// name may be taken once per region.
pub fn rule_name_key(region: &str, name: &str) -> String {
    format!("{region}\u{1f}{name}")
}
```
and its one use on the create path — `src/ops/create_notification_rule.rs:278-293`:
```
if store
    .rule_names
    .contains_key(&st::rule_name_key(&region, &input.name))
{
    return Err(... ResourceAlreadyExistsException { message: Some(st::RULE_NAME_ALREADY_IN_USE.to_string()) } ...);
}
```

Test coverage: `tests/all/notification_rule_lifecycle.rs:943-973` (`rules_are_scoped_to_the_region_they_were_created_in`) pins the divergence — "case {region:?}: the same name is free in each region" for us-east-1, eu-west-1 and ap-southeast-2 — with the stated reason "A notification rule is a REGIONAL resource", which is exactly what the two model sentences above contradict. `tests/all/error_message_rules.rs:33-36` pins the account-wide sentence in the emitted message.

## Steps to Reproduce

1. POST /createNotificationRule signed for us-east-1 with Name "shared-name", Resource arn:aws:codecommit:us-east-1:123456789012:MyRepo, DetailType BASIC, one catalogued EventTypeId, Targets [] -> 200.
2. POST the identical body signed for eu-west-1 -> 200 (a second rule now holds the same name in the account), where the model's `CreateNotificationRuleRequest$Name` sentence requires 409 ResourceAlreadyExistsException.
3. Repeat step 1's request in us-east-1 -> 409 with the message "… Notification rule names must be unique within an AWS account …" — the same service asserting account-wide uniqueness it did not enforce in step 2.
4. Variant on the rename path: create rule A named "a" in us-east-1 and rule B named "b" in eu-west-1, then POST /updateNotificationRule for B with Name "a" signed for eu-west-1 -> 200 (src/ops/update_notification_rule.rs:223-234 consults the same per-region key).
