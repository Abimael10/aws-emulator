Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: bdd754b371059bd004b1c5439a9cb53071a4d02c

# externalId has no effect on any channel reachable through the API: the registration seam never carries the channel's external-id requirement

_Filed: 2026-11-20T11:17:12Z_
_Operation: PutAuditEvents | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `bdd754b371059bd004b1c5439a9cb53071a4d02c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

PutAuditEvents implements the model's conditional `externalId` requirement against a single stored field, `Channel::required_external_id`, but NO path a caller can drive ever sets that field: it is `None` for every channel the CloudTrail control plane registers, and the only setter (`Channel::requiring_external_id`) is reached from one test. The consequence is that `externalId` is accepted on every request and is always inconsequential: for a channel created through `cloudtrail:CreateChannel` and then given a resource policy through `cloudtrail:PutResourcePolicy` (which this emulator does store), the comparison at put_audit_events.rs:461-482 is dead code, so a request carrying no `externalId` — or any arbitrary one — is ingested with HTTP 200 and the modelled `ChannelInsufficientPermission` for a missing/mismatched external id is unreachable outside the crate's own test seeding.

This is not an unattempted feature: the field, the check and the builder helper all exist in this crate, and the registration function this crate documents as the control plane's seam explicitly claims to carry 'what this service's own checks need beyond the ARN' while carrying nothing for the one check that needs more than the ARN. The defect is that seam: `register_channel(engine, arn, source)` takes only the ARN and the source and builds the channel through `Channel::from_arn`, which hard-codes `required_external_id: None`.

Not in scope here, and deliberately not reported: a channel whose policy requires nothing ignoring a supplied `externalId` — the handler states that rule at put_audit_events.rs:459-460 and tests/all/put_audit_events.rs:708-714 pins it.

## Evidence

Model (smithy_query(query="operation", operation="PutAuditEvents") / the handler's own quoted contract, services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/ops/put_audit_events.rs:78-80):
```
//! * `externalId` (ExternalId, len 2..1224, pattern ^[\w+=,.@:\/-]*$) — A unique identifier
//!   that is conditionally required when the channel's resource policy includes an external
//!   ID. This value can be any string, such as a passphrase or account number.
```

The enforcement, and the only reader of the field — src/ops/put_audit_events.rs:458-462:
```
        // `externalId` is "conditionally required when the channel's resource policy
        // includes an external ID": absent or different is insufficient permission. A
        // channel whose policy includes none ignores whatever the request supplies.
        if let Some(required) = channel.required_external_id.as_deref() {
            if input.external_id.as_deref() != Some(required) {
```

The seam that is supposed to carry what the checks need — src/state.rs:382-385 (doc of `register_channel`):
```
/// The registration carries what this service's own checks need beyond the ARN: the owner
/// account (read off the ARN, so it can never disagree with it) and the channel's event
/// `source`.
```
and what it actually builds — src/state.rs:405-425:
```
pub fn register_channel<Eng: Send + Sync + 'static>(
    engine: &Eng,
    arn: &str,
    source: &str,
) -> bool {
...
    let Some(channel) = Channel::from_arn(arn, source) else {
```
with src/state.rs:252-262 (`Channel::from_arn`) hard-coding `required_external_id: None` (src/state.rs:258).

The only setter and its only caller: src/state.rs:267 `pub fn requiring_external_id(mut self, external_id: impl Into<String>) -> Self`, called nowhere in `src/` — a repo-wide grep for `requiring_external_id|required_external_id` returns only src/state.rs:235/258/267-268, src/ops/put_audit_events.rs:461 and tests/all/put_audit_events.rs:695 (`.requiring_external_id("secret-123")`, inside `ownership_and_external_id_decide_permission`, which seeds the channel directly through `harness.register(..)`, tests/all/put_audit_events.rs:692-696).

The production registration path carries no external id either: services/service-com-amazonaws-cloudtrail-cloudtrail-20131101/src/ops/create_channel.rs:325 `if !channels::register_channel_in_data_plane(&*self.engine, &channel_arn, &source) {` → src/state/channels.rs:406-411 → this crate's `register_channel(engine, arn, source)`. The document that would carry the external ID IS held by the emulator: services/service-com-amazonaws-cloudtrail-cloudtrail-20131101/src/ops/put_resource_policy.rs:243-245 `store.resource_policies.insert(resource.arn.clone(), input.resource_policy.clone());`, and channels take such a policy (src/state.rs:3265-3270 `takes_resource_policy` → `Self::EventDataStore | Self::Dashboard | Self::Channel => true`). Nothing reads it into this service's registry.

Test coverage: the external-id rows of tests/all/put_audit_events.rs:685-783 only ever exercise a channel seeded in-process; no test drives CreateChannel/PutResourcePolicy and then PutAuditEvents, so nothing in the crate states a reason for the gap.

## Steps to Reproduce

1. Boot the emulator with both the cloudtrail and cloudtrail-data services mounted on the shared engine.
2. cloudtrail:CreateChannel(Name=..., Source='Custom', Destinations=[...]) and keep the returned ChannelArn — services/service-com-amazonaws-cloudtrail-cloudtrail-20131101/src/ops/create_channel.rs:325 registers it in this service's registry as `required_external_id: None` (src/state.rs:258).
3. cloudtrail:PutResourcePolicy(ResourceArn=<that channel ARN>, ResourcePolicy=<a policy whose condition carries an external ID>) — accepted and stored (put_resource_policy.rs:243-245).
4. cloudtrail-data:PutAuditEvents(channelArn=<that channel ARN>, auditEvents=[one valid event]) with NO externalId query parameter. Observe HTTP 200 and the event in `successful`: the branch at src/ops/put_audit_events.rs:461 never fires because `required_external_id` is `None`.
5. Repeat with externalId='anything-at-all' and observe the same HTTP 200 — the value is never compared to anything.
6. grep the repo for `requiring_external_id`: the only caller is tests/all/put_audit_events.rs:695, confirming no API path can make a channel require an external id.
