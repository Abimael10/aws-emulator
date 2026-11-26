Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: e822fa4ca12543e97642b0a71000bedf3ee319c6

# PutAuditEvents accepts any JSON object as eventData: the audit-event content check is absent, so FieldNotFound/ChannelUnsupportedSchema are unreachable

_Filed: 2026-11-18T23:54:39Z_
_Operation: PutAuditEvents | Class: validation_

> _Note: this issue was identified against the code base at commit `e822fa4ca12543e97642b0a71000bedf3ee319c6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`rejection()` is the whole per-event verdict for `PutAuditEvents`, and it only looks at three things: the `eventData` byte length, the supplied `eventDataChecksum`, and whether `eventData` parses as a JSON *object*. Nothing looks at the object's CONTENT. An event whose `eventData` is `{}` — or `{"not":"an audit event"}` — therefore passes, is appended to the channel's ingest log and is reported back in `successful` with a freshly minted `eventID`, exactly as a real audit-event payload is.

Two consequences, both of them gaps the handler's own comment admits:

* the `errorCode` catalog the model documents for `ResultErrorEntry` includes `FieldNotFound`, and no path in this crate can ever produce it: an event missing the members an audit event is made of is reported as ingested instead of failed;
* `ChannelUnsupportedSchema`, one of the six errors the operation declares, is constructed nowhere in the crate — `grep` finds it only in the generated struct, the error-enum variant and doc comments, never in a `return Err(..)`.

So the only report a caller gets for a payload that is not an audit event at all is a success. The defect is reported here, not a schema: this issue does not say which members CloudTrail's open audit event schema requires, only that the handler performs no content verdict at all and therefore cannot refuse an event on its content.

## Evidence

The handler's own comment admits exactly this gap — services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/ops/put_audit_events.rs:236-243:

```
/// DEFERRED, and so deliberately not decided here: whether the JSON object carries the
/// members of CloudTrail's open audit event schema (and therefore `FieldNotFound` for a
/// missing one, or the operation's `ChannelUnsupportedSchema` for a schema version this
/// channel does not take). The bootstrap issue puts `ChannelUnsupportedSchema` schema
/// validation out of this slice, ... so
/// this slice validates `eventData` only as far as it can without inventing a schema —
/// the schema check is its own gap issue.
```

The code under that comment (put_audit_events.rs:244-274) ends with the only content test it makes, which any JSON object passes:

```
    if !matches!(
        serde_json::from_str::<serde_json::Value>(&event.event_data),
        Ok(serde_json::Value::Object(_))
    ) {
        return Some(("InvalidData", ...));
    }
    None
```

Model side — `smithy_query(query="doc", shape="ResultErrorEntry", member="errorCode")`: "The error code for events that could not be ingested by CloudTrail. Possible error codes include: FieldTooLong, FieldNotFound, InvalidChecksum, InvalidData, InvalidRecipient, InvalidEventSource, AccountNotSubscribed, Throttling, and InternalFailure." `smithy_query(query="doc", shape="AuditEvent", member="eventData")`: "The content of an audit event that comes from the event, such as userIdentity, userAgent, and eventSource." The operation declares `ChannelUnsupportedSchema (HTTP 400) — The schema type of the event is not supported.` (OPERATIONS.md:30, and put_audit_events.rs:95).

Unreachable in the crate: grep for `ChannelUnsupportedSchema` over the crate's `*.rs` returns only src/ops/types/put_audit_events_types.rs:64-66 (the struct), src/ops/put_audit_events.rs:121 (the enum variant) and src/ops/put_audit_events.rs:95/237-239 (doc comments) — no construction site. `FieldNotFound` appears only in put_audit_events.rs:237, inside the deferral comment.

Test coverage: the per-event table in tests/all/put_audit_events.rs:376-496 spans checksum form and payload size and the NON-object shapes (`not json at all`, a JSON array, a JSON string, a truncated object → `InvalidData`), but every accepted row carries a real audit-event payload (`let good = r#"{"eventSource":"custom.app","eventName":"Login"}"#`, line 381, and `json_object_of_len` at lines 69-80, which also writes `eventSource`/`eventName`). No test pins the acceptance of a content-less object, so nothing in the crate states a reason for it.

## Steps to Reproduce

1. Register a channel: `Channel::from_arn(&channel_arn("0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9"), "Custom")` into this service's store on the shared engine (as tests/all/put_audit_events.rs:207-212 does).
2. POST /PutAuditEvents?channelArn=<that ARN> with auditEvents=[{"id":"empty-1","eventData":"{}"}].
3. Observe HTTP 200 with successful=[{"id":"empty-1","eventID":<minted id>}] and failed=[] — the payload carries none of an audit event's content yet is acknowledged as ingested.
4. Read the channel's ingest log (`Channel::events`): the `{}` payload is stored.
5. Grep the crate for `ChannelUnsupportedSchema` and `FieldNotFound`: neither is ever constructed/returned, so no request can elicit either.
