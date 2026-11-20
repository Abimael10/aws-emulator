Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: 88a2aafce1820f3382751d00e5eb796e6a7a0acd

# A repeated AcceptLink silently drops the required logSettings (and attributes/timeoutInMillis) it carries instead of storing or refusing them

_Filed: 2026-11-20T23:49:32Z_
_Operation: AcceptLink | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `88a2aafce1820f3382751d00e5eb796e6a7a0acd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`AcceptLink` stores the settings the call states ONLY inside the `link.request_outstanding()` branch. When the link is already `ACCEPTED` or `ACTIVE` — the state the first accept left, and the state the clock then moves it to — the handler takes the `else if !link.request_accepted()` branch, finds the condition false, and falls straight through to the response. On that path `logSettings` (which the model marks `@required` on this operation, so every caller must send it) is never stored, never compared, and never refused: the handler answers HTTP 200 and reports the OLD stored `logSettings`/`attributes`/`timeoutInMillis` back, so the caller is told its call succeeded while the values it just sent were discarded.

That is the opposite of the rule this crate applies everywhere else it answers a repeat of a call that already took effect. The three creates that take an `@idempotencyToken` compute a fingerprint of the request's other parameters and REFUSE a repeat whose parameters differ (`ValidationException`: "clientToken ... was already used for a ... request with different parameters") precisely so that a repeat never silently answers for parameters the caller did not send — the comment in `create_requester_gateway.rs` states that intent in those words. `AcceptLink`'s repeat path neither stores the stated settings nor refuses them; it quietly answers for the first call's settings.

The effect is a required input member that is dropped on a reachable path: nothing in the response distinguishes "your settings were applied" from "your settings were thrown away".

## Evidence

The handler, services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/accept_link.rs:200-224 — the stores live only in the first branch, and the second branch's condition is false for an already-accepted link, so control reaches the response with nothing stored:
```
        if link.request_outstanding() {
            // The request is answered: the link is accepted now, and coming up
            // is what the clock still owes it.
            link.status = crate::state::LINK_STATUS_ACCEPTED.to_string();
            link.settles_at = Some(now + crate::state::PROVISIONING_DELAY);
            link.updated_at = now;
            // The accepting end states the link's settings on this call, so they
            // are stored rather than echoed: `logSettings` is required here, and
            // an optional member replaces what the record holds only when the
            // caller states it.
            link.log_settings = input.log_settings.clone();
            if input.attributes.is_some() {
                link.attributes = input.attributes.clone();
            }
            if input.timeout_in_millis.is_some() {
                link.timeout_in_millis = input.timeout_in_millis;
            }
        } else if !link.request_accepted() {
            // Anything other than an outstanding request, or the acceptance this
            // operation already recorded, is a status this call cannot move.
            return Err(AcceptLinkErrorModeled::ConflictException(ConflictException {
                message: link.status_conflict_message(&gateway_id, "accepted"),
            })
            .into());
        }
```
and the response that reports the stored (old) values regardless, src/ops/accept_link.rs:237-238:
```
            attributes: link.attributes.clone(),
            log_settings: Some(link.log_settings.clone()),
```
Which statuses reach that fall-through, src/state.rs:310-316:
```
    /// Whether this link's request has already been accepted — the status an
    /// accept leaves, and the one the clock then moves it on to. A repeat of the
    /// accept asks for the state the link is already in, so it is answered with
    /// this record rather than refused.
    pub fn request_accepted(&self) -> bool {
        self.status == LINK_STATUS_ACCEPTED || self.status == LINK_STATUS_ACTIVE
    }
```
The rule the crate applies to a repeat carrying DIFFERENT parameters, src/ops/create_requester_gateway.rs:179-205:
```
        // `clientToken` is an `@idempotencyToken`: the same token replays the first
        // call's answer, and the same token with different parameters is refused
        // rather than silently answering for parameters the caller did not send.
...
            if prior.fingerprint != fingerprint {
                return Err(CreateRequesterGatewayErrorModeled::ValidationException(
                    ValidationException {
                        message: format!(
                            "clientToken {} was already used for a {} request with different parameters",
```
(the same block appears in src/ops/create_responder_gateway.rs:274-285 and src/ops/create_link_routing_rule.rs:224-235).

That the member is `@required`: `smithy_query(query="operation", operation="AcceptLink")` gives `"logSettings": {"target": "LinkLogSettings", "required": true}`.

Test coverage: tests/all/link_rejection.rs:638-649 repeats the accept with the SAME body (`fabric.send("POST", &accept, Some(body.clone()))`) and asserts only the status, so no test states a reason for discarding a repeat's settings; tests/all/link_rejection.rs:709-719 forces an already ACCEPTED/ACTIVE link and asserts only "expected the stored status". Nothing pins what happens to the settings a repeat carries.

## Steps to Reproduce

1. POST /requester-gateway and POST /responder-gateway (each with a clientToken, vpcId, subnetIds, securityGroupIds; the responder also a domainName) — note gateway ids R and P.
2. POST /gateway/R/create-link with {"peerGatewayId": P, "logSettings": {"applicationLogs":{"sampling":{"errorLog":100.0,"filterLog":0.0}}}} — note linkId L.
3. POST /gateway/P/link/L/accept with logSettings A = {"applicationLogs":{"sampling":{"errorLog":100.0,"filterLog":0.0}}} — 200, status ACCEPTED.
4. Poll GET /gateway/P/link/L until status is ACTIVE (the clock settles the accept after the provisioning delay).
5. POST /gateway/P/link/L/accept again, this time with logSettings B = {"applicationLogs":{"sampling":{"errorLog":0.0,"filterLog":50.0}}} (and e.g. "timeoutInMillis": 3000).
6. Observed: HTTP 200 whose logSettings is A, and GET /gateway/P/link/L still reports A with no timeoutInMillis — the required logSettings B and the stated timeoutInMillis were neither stored nor refused.
7. Expected (the crate's own rule for a repeat carrying different parameters, create_requester_gateway.rs:179-205): the differing parameters are refused rather than silently answered for with the first call's values.
