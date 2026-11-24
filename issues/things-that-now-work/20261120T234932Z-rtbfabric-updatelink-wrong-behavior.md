Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: 88a2aafce1820f3382751d00e5eb796e6a7a0acd

# UpdateLink moves a live link to PENDING_UPDATE and bumps updatedAt even when the request states neither of its two optional members

_Filed: 2026-11-20T23:49:32Z_
_Operation: UpdateLink | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `88a2aafce1820f3382751d00e5eb796e6a7a0acd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Both of `UpdateLink`'s body members (`logSettings`, `timeoutInMillis`) are optional, so `PATCH /gateway/{gatewayId}/link/{linkId}` with an empty object is a valid request that asks for no change at all. The handler guards each assignment on the member being stated — and then unconditionally executes `link.updated_at = now;` and, for a link that is up, flips the record to `PENDING_UPDATE` with a fresh `settles_at`. So a request that changed nothing reports a configuration change in flight, moves the link's `updatedAt`, and keeps the link out of `ACTIVE` for the whole provisioning delay (which, with `link_counts` comparing `status == ACTIVE` literally, also drops the gateways' `activeLinksCount` to 0 for that span).

The handler's own comment states the transition as a consequence of a change being applied — "The link is carrying traffic, so the change is applied to a live link: this call reports the update as pending" — but on this path there is no change to apply, so the record is moved for a change that was never asked for.

Every sibling handler in this crate that moves a record's status guards the move on the call actually changing something: `AcceptLink` and `RejectLink` on `link.request_outstanding()`, `DeleteLinkRoutingRule` on `!rule.deleting()`, `DisassociateCertificate` on `association.association_in_force()`. `UpdateLink` is the one that writes unconditionally.

## Evidence

The handler, services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/update_link.rs:194-209 — the two writes are guarded, the status move and the timestamp bump are not:
```
        // A member the caller states replaces what the record holds; one it
        // leaves out is not a request to clear the stored value.
        if let Some(log_settings) = &input.log_settings {
            link.log_settings = log_settings.clone();
        }
        if input.timeout_in_millis.is_some() {
            link.timeout_in_millis = input.timeout_in_millis;
        }
        link.updated_at = now;
        if link.is_up() {
            // The link is carrying traffic, so the change is applied to a live
            // link: this call reports the update as pending, and the clock owes
            // the link its way back up.
            link.status = crate::state::LINK_STATUS_PENDING_UPDATE.to_string();
            link.settles_at = Some(now + crate::state::PROVISIONING_DELAY);
        }
```
That both members are optional (so an empty body reaches the handler) — OPERATIONS.md:117: "**Input:** `gatewayId` (..., @httpLabel, required), `linkId` (..., @httpLabel, required), `logSettings` (LinkLogSettings), `timeoutInMillis` (long, range 100..5000)"; confirmed by `smithy_query(query="operation", operation="UpdateLink")`, where neither body member carries `required`.

The sibling handlers that do guard the move on the call changing something:
src/ops/accept_link.rs:200 — `if link.request_outstanding() {`
src/ops/reject_link.rs:195 — `if link.request_outstanding() {`
src/ops/delete_link_routing_rule.rs:184-186:
```
        if !rule.deleting() {
            rule.begin_deletion(now);
        }
```
src/ops/disassociate_certificate.rs:200-205:
```
        if association.association_in_force() {
            association.status =
                crate::state::CERTIFICATE_STATUS_PENDING_DISASSOCIATION.to_string();
            association.settles_at = Some(now + crate::state::PROVISIONING_DELAY);
            association.updated_at = now;
        }
```
Why the status move is observable in the counters, src/state.rs:555-562 (`link_counts` compares the literal ACTIVE):
```
            if link.status == LINK_STATUS_ACTIVE {
                counts.active = counts.active.saturating_add(1);
            }
```
Test coverage: the UpdateLink cover always states at least one member — tests/all/gateway_link_lifecycle.rs drives the PATCH with a `timeoutInMillis`/`logSettings` body — so no test pins the behaviour of a PATCH that states neither member, and no comment states a reason for moving the record on such a call.

## Steps to Reproduce

1. POST /requester-gateway and POST /responder-gateway — note gateway ids R and P.
2. POST /gateway/R/create-link with {"peerGatewayId": P, "logSettings": {...}} — note linkId L.
3. POST /gateway/P/link/L/accept with the required logSettings, then poll GET /gateway/R/link/L until status is ACTIVE.
4. GET /responder-gateway/P — activeLinksCount is 1; note updatedAt from GET /gateway/R/link/L.
5. PATCH /gateway/R/link/L with the body {} (neither logSettings nor timeoutInMillis stated).
6. Observed: HTTP 200 with status PENDING_UPDATE; GET /gateway/R/link/L shows a moved updatedAt and PENDING_UPDATE, and GET /responder-gateway/P reports activeLinksCount 0 until the clock settles the link back to ACTIVE — all for a request that stated no change.
7. Expected: a request that states neither optional member changes nothing, as the guarded assignments above it already do.
