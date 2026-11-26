Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 832e9f5059543777b388ffa865d01db1cd157680

# Subscribe answers a replayed ClientRequestToken with 200 from an endpoint that holds no such rule, skipping the documented ResourceNotFoundException

_Filed: 2026-11-26T02:37:43Z_
_Operation: Subscribe | Class: error-response_

> _Note: this issue was identified against the code base at commit `832e9f5059543777b388ffa865d01db1cd157680`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`Subscribe`'s idempotency-replay branch runs BEFORE the region-scoped rule lookup, and neither the ledger key nor the request fingerprint carries the region the request was signed for. The ledger key is `(operation, token)` only (`src/state.rs:694-697`) and the fingerprint is built from the submitted ARN TEXT plus the target's type and address (`src/ops/subscribe.rs:171-175`) — all three values a caller resends byte-for-byte when it repeats the request against a different regional endpoint. The region that decides whether the rule exists is read separately from the request's SigV4 scope (`src/ops/subscribe.rs:157`) and appears in neither the key nor the fingerprint.

Consequence: a `Subscribe` that reuses a token first used against us-east-1, but is signed for eu-west-1, takes the replay branch at `src/ops/subscribe.rs:187-203` and answers 200 with the us-east-1 rule's ARN. The eu-west-1 endpoint holds no such rule (`rule_in_region`, `src/state.rs:201-203`), nothing is attached to anything, and the identical request WITHOUT the token is answered 404 `ResourceNotFoundException` by the very next lines of the same handler (`src/ops/subscribe.rs:209-217`). A documented failure mode — the modelled `ResourceNotFoundException` (HTTP 404), "CodeStar Notifications can't find a resource that matches the provided ARN" — is therefore answered as a success that persisted nothing, purely because an idempotency token was present.

This is NOT covered by the open issue `20261125T164247Z-codestar-notifications-createnotificationrule-wrong-behavior`: that issue asserts the opposite for this operation ("`Subscribe` puts the region-bearing rule ARN into its fingerprint, so its replay check cannot cross a region boundary"). The ARN text is identical in both requests, so it distinguishes nothing about the endpoint that served them; the crossing happens through the signing region, which the fingerprint never sees.

## Evidence

The replay returns before any lookup — `src/ops/subscribe.rs:187-203`:
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
The check it skips, in the same handler — `src/ops/subscribe.rs:205-217`:
```
        let arn = input.arn.clone();
        if store.rule_in_region(&region, &arn).is_none() {
            // This operation names the missing rule by the whole ARN it was given.
            return Err(SubscribeErrorModeled::ResourceNotFoundException(
                ResourceNotFoundException {
                    message: Some(st::no_rule_with_identifier(&arn)),
                },
            )
```
The fingerprint that carries no signing region — `src/ops/subscribe.rs:157` and `:171-175`:
```
        let region = st::request_region(ctx.parts());
...
        let fingerprint = st::fingerprint(&[
            input.arn.clone(),
            target.target_type.clone(),
            target.target_address.clone(),
        ]);
```
The ledger key that carries no region either — `src/state.rs:692-697`:
```
/// The ledger key for an idempotency token: the OPERATION as well as the token, so one
/// token used on two operations cannot hand either of them the other's resource.
fn idempotency_key(operation: &str, token: &str) -> String {
    format!("{operation}\u{1f}{token}")
}
```
The crate's own rule about which endpoint holds a rule — `src/state.rs:199-203`:
```
/// The rule `arn` names, if this REGION holds it. A rule is addressed through a
/// regional endpoint, so an ARN from another region is not found here.
pub fn rule_in_region(&self, region: &str, arn: &str) -> Option<&NotificationRule> {
    self.rules.get(arn).filter(|rule| rule.region == region)
```
The modelled failure mode being skipped — `smithy_query(query="operation",operation="Subscribe")` lists `{"shape": "ResourceNotFoundException", "errorKind": "client", "httpError": 404}`, and `smithy_query(query="doc",shape="ResourceNotFoundException")`: "CodeStar Notifications can't find a resource that matches the provided ARN." The handler's own doc restates it — `src/ops/subscribe.rs:51-52`:
```
//! * `ResourceNotFoundException` (HTTP 404) — CodeStar Notifications can't find a resource
//!   that matches the provided ARN.
```
Test coverage: the 404 is pinned only for token-less bodies — `tests/all/rule_arn_authorization.rs:222`:
```
        ("Subscribe", "/subscribe", 404, "ResourceNotFoundException"),
```
and `request_for` builds that body with no `ClientRequestToken` (`tests/all/rule_arn_authorization.rs:69-72`). The only cross-operation token test uses one region (`tests/all/notification_rule_lifecycle.rs:1548-1570`), and `an_idempotency_token_replays_the_first_rule` (`tests/all/notification_rule_lifecycle.rs:668`) never signs a replay for a second region — while the crate's multi-region behaviour is pinned elsewhere (`tests/all/delete_target.rs:354`, `tests/all/rule_arn_authorization.rs:276`). So nothing pins the current answer.

## Steps to Reproduce

1. POST /createNotificationRule signed for us-east-1 with a well-formed body (Name `sub-seed`, Resource `arn:aws:codecommit:us-east-1:123456789012:MyRepo`, DetailType BASIC, one catalogued event type, one SNS target). Note the returned `Arn`.
2. POST /subscribe signed for us-east-1 with {"Arn": <that Arn>, "Target": {"TargetType": "SNS", "TargetAddress": "arn:aws:sns:us-east-1:123456789012:topic-a"}, "ClientRequestToken": "tok-1"} — 200, and the target is attached.
3. POST the IDENTICAL /subscribe body (same Arn, same Target, same token `tok-1`) but signed for eu-west-1 (as `Emulator::post_in` does in tests/all/delete_target.rs). Observe 200 with that same `Arn`.
4. Repeat step 3 with the token removed. Observe 404 `ResourceNotFoundException` — the answer src/ops/subscribe.rs:209-217 gives for an ARN this endpoint does not hold.
5. Confirm nothing was created or attached in eu-west-1: POST /describeNotificationRule signed for eu-west-1 with that Arn answers 404, and POST /listTargets signed for eu-west-1 does not report the address.
