Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 93aea946e1be9d371a5c94c0146ea55c9b1afa4f

# The documented 1 MB budget is charged PER EVENT, not per request: a multi-megabyte batch is ingested whole

_Filed: 2026-11-19T05:47:30Z_
_Operation: PutAuditEvents | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `93aea946e1be9d371a5c94c0146ea55c9b1afa4f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`PutAuditEvents` documents TWO bounds on one request — at most 100 events and at most 1 MB — and the second one is explicitly stated per request: "You can add up to 100 of these events (or up to 1 MB) per PutAuditEvents request." The crate implements the 100-event bound (the member's modelled `@length`, enforced at the boundary) but never computes the request's payload total at all. The ONLY size verdict in the crate is `rejection()`'s per-event test `event.event_data.len() > MAX_EVENT_DATA_BYTES` (1 MiB), applied independently to each entry of the batch.

Consequence: a request whose entries are each at or below 1 MB is accepted whatever the request sums to. 100 entries of 1 MB each (~100 MB of `eventData`) are all reported in `successful` with freshly minted `eventID`s and appended to the channel's ingest log, while the model's budget for that one request is 1 MB. The per-request budget is not merely rendered differently — it is never calculated anywhere in the crate (`grep` over `src/` finds `MAX_EVENT_DATA_BYTES` only at its definition and inside the per-event check; no sum of `event_data.len()` over the batch exists; the only other `len()` accumulation is the channel's retention accounting in `state.rs:276/285`).

This is distinct from the open validation issue on this operation (20261118T235439Z-cloudtrail-data-putauditevents-validation), which is about `eventData` CONTENT (no audit-event schema check, so `FieldNotFound`/`ChannelUnsupportedSchema` are unreachable). Nothing is proposed here about which error class the over-budget request should take; what is filed is that the request-scoped bound the model states is applied in event scope instead, so the handler's accept/reject computation for a batch is wrong.

## Evidence

Model side — `smithy_query(query="doc", shape="PutAuditEvents")`: "Ingests your application events into CloudTrail Lake. A required parameter, auditEvents, accepts the JSON records (also called payload) of events that you want CloudTrail to ingest. You can add up to 100 of these events (or up to 1 MB) per PutAuditEvents request." The same sentence is restated in the handler's own modeled-contract header, services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/ops/put_audit_events.rs:67-70.

Code side — the only size verdict, put_audit_events.rs:244-252:
```
fn rejection(event: &AuditEvent) -> Option<(&'static str, String)> {
    if event.event_data.len() > MAX_EVENT_DATA_BYTES {
        return Some((
            "FieldTooLong",
            format!(
                "eventData is {} bytes, which exceeds the {MAX_EVENT_DATA_BYTES} byte limit.",
                event.event_data.len()
            ),
        ));
    }
```
It is called once per entry and nothing aggregates across the batch — put_audit_events.rs:420-421:
```
        let rejections: Vec<Option<(&'static str, String)>> =
            input.audit_events.iter().map(rejection).collect();
```

The crate ADMITS the gap rather than justifying the divergence — services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/state.rs:31-39:
```
/// The largest `eventData` payload a single event may carry.
///
/// From `PutAuditEvents`' own doc: "You can add up to 100 of these events (or up to
/// 1 MB) per PutAuditEvents request" — so an event whose `eventData` alone already
/// exceeds 1 MB can never be ingested, whatever else the request holds, and is reported
/// for that event as `FieldTooLong`. (Charging a whole BATCH against the same 1 MB
/// budget is a separate gap, deferred by the bootstrap issue.)
pub const MAX_EVENT_DATA_BYTES: usize = 1024 * 1024;
```
The fixed bootstrap issue it defers to says the same: "Any behaviour beyond this slice (checksum algorithm edge cases, 1 MB total-payload limit, throttling, ChannelUnsupportedSchema schema validation, UnsupportedOperationException region gating) stays out of scope and will be filed as individual gap issues later." (20261021T213812Z-bootstrap-service-com-amazonaws-cloudtraildata-cloudtraildataservice.md).

Test coverage — no test charges a request total. tests/all/put_audit_events.rs:383-434 builds ONE batch out of all its rows, including `("eventData exactly at the 1 MB limit", ev("ok-3", json_object_of_len(MAX_EVENT_DATA_BYTES)), None)` and `("eventData one byte past the 1 MB limit", ev("bad-8", json_object_of_len(MAX_EVENT_DATA_BYTES + 1)), Some("FieldTooLong"))` — a request already past 2 MB — and asserts only the PER-EVENT verdicts ("Per-event validation is a PARTIAL failure", line 372-374). tests/all/put_audit_events.rs:931-943 exercises the byte retention cap with "20 single-event calls of 1 MB each", i.e. one event per request, never a multi-megabyte batch. So no test states a reason for accepting a request past the documented per-request budget.

## Steps to Reproduce

1. Build the harness of tests/all/put_audit_events.rs on a fresh engine and register a channel: `harness.register(Channel::from_arn(&channel_arn("1b2c3d4e-5f60-7182-93a4-b5c6d7e8f901"), "Custom").unwrap())`.
2. POST /PutAuditEvents?channelArn=<that ARN> with three entries of distinct ids, each carrying `json_object_of_len(MAX_EVENT_DATA_BYTES)` as `eventData` — a single request of roughly 3 MB of payload, i.e. three times the documented 1 MB per-request budget.
3. Observe HTTP 200 with all three ids in `successful[]` (each with a minted `eventID`) and `failed[]` empty: the request's total payload is never computed, only each entry's own length.
4. Read the channel's ingest log (`Channel::events` / `harness.ingest_log`): all three ~1 MB payloads are stored.
5. Repeat with 100 entries of 1 MB each (the modelled batch maximum): the whole ~100 MB request is still answered 200 with 100 successful entries.
6. Grep the crate: `MAX_EVENT_DATA_BYTES` occurs only at src/state.rs:39 (definition) and src/ops/put_audit_events.rs:143/145/245/249 (the per-event check) — no sum of `event_data.len()` over `input.audit_events` exists anywhere.
