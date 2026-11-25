Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 6efdcc2c184ddba11bf2d42c3ae06c16d4d6253e

# CreateNotificationRule idempotency ledger is not region-scoped, so a replayed token answers with another region's rule and creates nothing

_Filed: 2026-11-25T16:42:47Z_
_Operation: CreateNotificationRule | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `6efdcc2c184ddba11bf2d42c3ae06c16d4d6253e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every other index in this crate is keyed per region, and every lookup is region-filtered: `rule_name_key` mixes the region into the name index, and `rule_in_region` refuses a rule that belongs to another region. The idempotency ledger is the one exception — its key is `(operation, token)` only, and `CreateNotificationRule`'s request fingerprint is built from `Name`, `Resource`, `DetailType`, `Status`, `EventTypeIds`, `Targets` and `Tags`, with no region in it.

Consequence: the first branch the handler takes on a token it has already seen is taken no matter which regional endpoint served the original request. A `CreateNotificationRule` signed for eu-west-1 that reuses a token first used in us-east-1 either

* answers 200 with the us-east-1 ARN while persisting NOTHING in eu-west-1 — and the very next `DescribeNotificationRule` on that ARN against the same eu-west-1 endpoint answers 404, because `rule_in_region` does not hold it (the create is silently lost and the returned ARN is unusable at the endpoint that minted the answer); or

* when the body differs only in a region-bearing member (e.g. a `Resource` ARN in the new region), is refused 400 `ValidationException` "ClientRequestToken … was already used with different parameters" — a refusal for a token that was never used against this region at all.

The sibling path shows the crate's own intent: `Subscribe` puts the region-bearing rule ARN into its fingerprint, so its replay check cannot cross a region boundary; `CreateNotificationRule` has no such field and the ledger key supplies none.

## Evidence

The crate's rule, stated for the name index — `src/state.rs:328-334`:
```
/// The key [`...State::rule_names`]
/// is indexed by: a rule name is unique within one region of the account, so the same
/// name may be taken once per region.
pub fn rule_name_key(region: &str, name: &str) -> String {
```
and for every lookup — `src/state.rs:199-203`:
```
/// The rule `arn` names, if this REGION holds it. A rule is addressed through a
/// regional endpoint, so an ARN from another region is not found here.
pub fn rule_in_region(&self, region: &str, arn: &str) -> Option<&NotificationRule> {
    self.rules.get(arn).filter(|rule| rule.region == region)
```
The exception — `src/state.rs:692-697`:
```
/// The ledger key for an idempotency token: the OPERATION as well as the token, so one
/// token used on two operations cannot hand either of them the other's resource.
fn idempotency_key(operation: &str, token: &str) -> String {
    // `\u{1f}` (unit separator) cannot appear in `ClientRequestToken`'s `^[\w:/-]+$`.
    format!("{operation}\u{1f}{token}")
}
```
The fingerprint that also omits the region — `src/ops/create_notification_rule.rs:237-252`:
```
let mut fields = vec![
    input.name.clone(),
    input.resource.clone(),
    input.detail_type.clone(),
    status.clone(),
];
... fields.extend(sorted_event_type_ids); ... targets ... tags ...
let fingerprint = st::fingerprint(&fields);
```
and the replay that answers with it — `src/ops/create_notification_rule.rs:264-276`:
```
if let Some(previous) = store.idempotent_request(Self::NAME, token) {
    if previous.fingerprint != fingerprint { return Err(invalid(format!(
        "ClientRequestToken {token} was already used with different parameters"))); }
    // Same token, same parameters: report the rule that token created.
    return Ok(CreateNotificationRuleOutput { arn: Some(previous.arn.clone()) });
}
```
The sibling that DOES carry the region, through the rule ARN — `src/ops/subscribe.rs:171-175`:
```
let fingerprint = st::fingerprint(&[
    input.arn.clone(),
    target.target_type.clone(),
    target.target_address.clone(),
]);
```
Test coverage: `tests/all/notification_rule_lifecycle.rs:668` (`an_idempotency_token_replays_the_first_rule`) exercises replay in ONE region only; no test in the crate signs a replay for a second region, while the crate's multi-region behaviour is pinned elsewhere (`tests/all/rule_arn_authorization.rs:276` `a_rule_arn_from_another_region_is_absent_rather_than_denied`, `tests/all/delete_target.rs:354`).

## Steps to Reproduce

1. POST /createNotificationRule signed for us-east-1 with `ClientRequestToken: "token-1"` and a well-formed body (Name `x`, Resource `arn:aws:codecommit:us-east-1:123456789012:MyRepo`, DetailType BASIC, one event type, one SNS target). Note the returned `Arn` (region us-east-1).
2. POST the SAME body with the SAME token, but signed for eu-west-1 (as `Emulator::post_in` does in tests/all/delete_target.rs).
3. Observe 200 with the us-east-1 `Arn` — the eu-west-1 endpoint minted no rule.
4. POST /describeNotificationRule signed for eu-west-1 with that `Arn`: 404 ResourceNotFoundException (`rule_in_region`, src/state.rs:201). POST /listNotificationRules for eu-west-1: the rule is absent.
5. Variant: repeat step 2 with `Resource` changed to `arn:aws:codecommit:eu-west-1:123456789012:MyRepo`. Observe 400 ValidationException "ClientRequestToken token-1 was already used with different parameters", although that token was never used against eu-west-1.
