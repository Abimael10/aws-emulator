Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 47635055653444c8189ab0ed1a8047834cbe5b1c

# The incomplete-target refusal echoes the @sensitive TargetAddress, which this crate states three times it never does

_Filed: 2026-11-26T08:39:23Z_
_Operation: UpdateNotificationRule | Class: error-response_

> _Note: this issue was identified against the code base at commit `47635055653444c8189ab0ed1a8047834cbe5b1c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::rule_targets_from` is the one helper that turns a wire `Target` list into stored targets, and it is called by CreateNotificationRule, Subscribe and UpdateNotificationRule. When a `Target` carries a `TargetAddress` but no `TargetType`, the reason it returns interpolates the submitted `TargetAddress` into the error message, and each caller lifts that string verbatim into its modelled `ConfigurationException` body (HTTP 400).

`Target$TargetAddress` targets the `TargetAddress` shape, which the model marks `@sensitive`. This crate states the resulting rule in three separate places — the validation formatter that withholds a sensitive value from every framework refusal, the DeleteTarget handler that deliberately keeps the address out of its response, and the rule-name conflict constant that keeps `NotificationRuleName` out of its text — and this one path breaks it. The result is that the same address is withheld when it breaks a modelled `@length` bound (`Value at 'target.targetAddress' failed to satisfy constraint: ...`) but disclosed when its sibling member is missing, so whether a caller's address ends up in the error body (and therefore in every client log that records one) depends on which of two adjacent checks fired.

The fix is local to the helper: state the member that is missing without interpolating the address, the way the sibling paths state their own refusals. No modelled error code or status changes.

## Evidence

Smithy, `Target` shape (smithy_query query="shape" shape="Target"):
  "TargetAddress": {"target": "TargetAddress", "length": {"min": 1, "max": 320}, "sensitive": true, ...}

The offending interpolation — services/.../src/state.rs:743-749:
```
        let target_type = target
            .target_type
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                format!("Target.TargetType is required for every target (address {address})")
            })?;
```
(`address` is bound at state.rs:738-742 from `target.target_address`.)

The three places this crate states the opposite rule:
* services/.../src/service/validation.rs:18-21 — "a member the model marks `@sensitive` is reported WITHOUT the value that broke the constraint ([`SENSITIVE_MEMBERS`]): the clause reads `Value at '<path>' …`. The member and the constraint are what a caller acts on, and echoing the value would put a value the model says not to disclose into every client log that records an error body"
* services/.../src/service/validation.rs:51-60 — `SENSITIVE_MEMBERS: &[&str] = &["name", "targetAddress"];` with "the `NotificationRuleName` and `TargetAddress` string shapes both carry `sensitive: true` … as `Target$TargetAddress` nested inside the create and subscribe requests"
* services/.../src/ops/delete_target.rs:96-98 — "It does NOT repeat the address: `TargetAddress` is `@sensitive`, so it stays out of the response."
* services/.../src/state.rs:517-520 (same rule for the other sensitive member) — "it states the name-uniqueness rule and names neither the submitted name — `NotificationRuleName` is `@sensitive`, so its value is not echoed back to a caller"

The three handlers that lift the string into a 400 body:
* services/.../src/ops/update_notification_rule.rs:196-199 — `Some(targets) => Some(st::rule_targets_from(targets).map_err(misconfigured)?),` with `misconfigured` at update_notification_rule.rs:122-127 building `ConfigurationException { message: Some(...) }`
* services/.../src/ops/create_notification_rule.rs:217 — `let targets = st::rule_targets_from(&input.targets).map_err(misconfigured)?;`
* services/.../src/ops/subscribe.rs:166-167 — `st::rule_targets_from(std::slice::from_ref(&input.target)).map_err(misconfigured)?`

Test coverage: the only test over this path asserts the status and error code and never the message — services/.../tests/all/notification_rule_lifecycle.rs:1129-1148 drives `("no type", json!({ "TargetAddress": "arn:aws:sns:us-east-1:123456789012:t" }))` and checks only `status == 400` and `error_code(&parsed) == "ConfigurationException"`. So nothing pins the disclosure, while error_message_rules.rs:128-146 pins the withholding for the same member under `@length` (`"1 validation error detected: Value at 'target.targetAddress' failed to satisfy constraint: Member must have length greater than or equal to 1"`).

## Steps to Reproduce

1. Create a rule: POST /createNotificationRule with a valid Name/Resource/DetailType/EventTypeIds and Targets: [] — note the returned Arn.
2. POST /updateNotificationRule with {"Arn": "<that arn>", "Targets": [{"TargetAddress": "arn:aws:sns:us-east-1:123456789012:secret-topic"}]} (TargetType omitted; TargetType is not @required so the framework walk does not refuse it).
3. Observe HTTP 400 ConfigurationException whose message is 'Target.TargetType is required for every target (address arn:aws:sns:us-east-1:123456789012:secret-topic)' — the @sensitive TargetAddress is in the response body.
4. For contrast, POST /unsubscribe with {"Arn": "<that arn>", "TargetAddress": "<a 321-character address>"} and observe the same member reported with its value withheld: "Value at 'targetAddress' failed to satisfy constraint: Member must have length less than or equal to 320".
5. Repeat step 2 against /createNotificationRule and /subscribe: both disclose the address the same way, because all three call crate::state::rule_targets_from.
