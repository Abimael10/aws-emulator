Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: db121cdfeb6bb4ca388d7e0b7667da800f851816

# The pre-handler refusal layer answers a request without emitting the access record the crate says every refusal emits

_Filed: 2026-11-22T15:49:34Z_
_Operation: PutAuditEvents | Class: error-response_

> _Note: this issue was identified against the code base at commit `db121cdfeb6bb4ca388d7e0b7667da800f851816`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate states one rule about its access record: EVERY request `PutAuditEvents` answers — served or refused, and whichever layer refused it — emits exactly one structured record through `record_access`. Two of the three layers honour it: the authorization gate (`src/service/authorization.rs:197-203`) calls `crate::ops::put_audit_events::record_access` before answering a denial, and the handler calls it on all eight of its own exits (`src/ops/put_audit_events.rs:356, 383, 404, 427, 444, 464, 490, 547`).

The third layer does not. `StatingSubmittedValues::run` in `src/service/validation.rs` is what serves this service's one operation (`src/service.rs:110-119`), and when it finds a pre-handler violation it builds and RETURNS the refusal response itself, never reaching `self.operation.run(..)` and never calling `record_access`. That path answers the two most common client faults of this operation — a `channelArn` outside the modelled `^arn:.*$` pattern, which this service answers as `InvalidChannelARN` (`validation.rs:156-164`), and every other constraint refusal, answered as `ValidationException` (`validation.rs:165-172`) — so a whole class of refused requests leaves no access record at all, while the identically-classed refusal the handler states for an `arn:` value that names no channel does leave one. The crate's own grep census confirms it: `record_access` is referenced only from `authorization.rs` and `put_audit_events.rs`, never from `validation.rs`.

This is the one refusal path that skips a helper the crate already has and that its sibling refusal layer calls; no comment or test in the crate states a reason for the omission. No wire response changes, hence LOW: the defect is that the record the crate declares to be emitted for every refused request is not emitted for the requests this layer refuses.

## Evidence

The rule, stated twice in this crate.

services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/ops/put_audit_events.rs:29-32 (module doc):
```
//! The channel's ingest log is the emulator's own record of what it accepted, bounded by
//! the retention limits in [`crate::state`] (oldest evicted first) so repeated calls
//! cannot grow the store without limit, and every request — ingested or refused — emits
//! one structured access record (see `record_access`).
```

services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/ops/put_audit_events.rs:288-290 (`record_access`'s own doc):
```
/// Reached by the authorization gate in front of this operation
/// ([`crate::service::authorization`]) as well as by the handler below, so every request
/// this service answers — served or refused, and whichever layer refused it — is recorded
/// in one shape.
```

The sibling layer that DOES call it — src/service/authorization.rs:197-207:
```
            if let Some(denial) = refusal(&*self.engine, ctx, self.operation, &resource) {
                crate::ops::put_audit_events::record_access(
                    ctx,
                    &resource,
                    ACCESS_DENIED_CODE,
                    DENIED_DETAIL,
                );
                ...
                return denied_response(svc.definition().protocol(), &denial);
            }
```

The layer that does NOT — src/service/validation.rs:119-128, which answers the refusal and returns before the operation (and therefore before any `record_access`) runs:
```
            if let Some(refused) = refusal_of(&collected) {
                let violations: Vec<Violation> =
                    collected.into_iter().map(|(violation, _)| violation).collect();
                return svc.definition().refusal_response(
                    aws_smithy_schema_server::ProtocolError::Invalid(violations),
                    Some(refused),
                    req,
                );
            }
            self.operation.run(svc, req, ctx).await
```
and the refusals it states there — validation.rs:156-172: `InvalidChannelARN` for a `^arn:.*$` pattern violation (`refuses_channel_arn`), `ValidationException` for everything else.

Census of the helper's call sites (grep `record_access` over the crate): src/service/authorization.rs:198 and src/ops/put_audit_events.rs:{32 doc, 291 definition, 356, 383, 404, 427, 444, 464, 490, 547} — nothing in src/service/validation.rs.

Test coverage: the pre-handler refusal path is pinned on the WIRE only — tests/all/channel_arn_refusals.rs:13-15 ("A channel reference outside the modelled `@pattern` of `ChannelArn` is refused by the SERVICE"), tests/all/constraint_refusals.rs, tests/all/clause_value_slot.rs and tests/all/batch_length_clause_value.rs assert status/code/message and nothing about the access record, so no test states a reason for the omission and none has to change when the call is added.

## Steps to Reproduce

1. Register a channel in this service's store on the shared engine, as tests/all/put_audit_events.rs:207-212 does.
2. With a tracing subscriber capturing `tracing::info!` events, POST /PutAuditEvents?channelArn=not-an-arn with one valid event. Observe HTTP 400 `InvalidChannelARN` and NO "PutAuditEvents access record" event: the refusal was stated by src/service/validation.rs:119-126, which never calls `record_access`.
3. Now POST /PutAuditEvents?channelArn=arn:aws:s3:::a-bucket-name with the same body — the other spelling of the same fault, which the handler refuses at src/ops/put_audit_events.rs:382-393. Observe the same HTTP 400 `InvalidChannelARN` AND one access record with outcome `InvalidChannelARN`.
4. Repeat step 2 with a registered channel ARN and 101 events (past `auditEvents`' `@length` maximum): HTTP 400 `ValidationException` is answered with no access record either, although put_audit_events.rs:31-32 says "every request — ingested or refused — emits one structured access record".
