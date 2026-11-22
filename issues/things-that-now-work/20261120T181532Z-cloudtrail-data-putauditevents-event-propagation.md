Issue-Severity: MEDIUM
Issue-Gap-Class: event-propagation
Issue-Commit: c9b7fc7795b284972b7c862846d545eef7a83f7a

# A successful PutAuditEvents is invisible to the control plane: the ingest log never feeds GetChannel's IngestionStatus, and refused events are not retained at all

_Filed: 2026-11-20T18:15:32Z_
_Operation: PutAuditEvents | Class: event-propagation_

> _Note: this issue was identified against the code base at commit `c9b7fc7795b284972b7c862846d545eef7a83f7a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

PutAuditEvents writes every accepted event into the channel's private ingest log in this crate's own state and writes nothing anywhere the CloudTrail control plane — the crate that registered the channel through this crate's own `register_channel` seam — can observe. The receiving side (cloudtrail `GetChannel`) therefore answers `ingestionStatus` ABSENT for a channel that has just ingested events successfully, and its own handler comment states that it is waiting for exactly this crate to report the log. The producing handler also drops every refused event on the floor (`failed` entries are reported to the caller and stored nowhere), so the 'most recent FAILED attempt' half of that table can never be answered either, even once the log is read. The two crates are already wired in this direction — `cloudtrail:CreateChannel` calls `service_com_amazonaws_cloudtraildata_cloudtraildataservice::state::register_channel` — and this crate already exposes the readers (`Channel::events`, `Channel::event_id_for`), so the delivery of the ingest facts is the missing half of an existing seam rather than an unattempted feature.

## Evidence

Producing side stores the accepted event only in this service's private log, and explicitly discards refusals — services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/ops/put_audit_events.rs:505-507: "// Per-event outcome: an accepted event is appended to the channel's ingest log / // under a freshly minted `eventID`; a rejected one is reported in `failed` and / // is NOT stored." and :522 `channel.record_ingest(crate::state::IngestedEvent {`.

The log is declared to change no response at all — services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/state.rs:20-21: "/// The ingest log is the emulator's own record of what it accepted, not a modelled read / /// API, so retaining a bounded window of it changes no response this service renders" — while the readers that could deliver it already exist: state.rs:291 `pub fn events(&self) -> impl ExactSizeIterator<Item = &IngestedEvent>` and state.rs:307 `pub fn event_id_for(&self, source_id: &str) -> Option<&str>`.

Receiving side admits the gap — services/service-com-amazonaws-cloudtrail-cloudtrail-20131101/src/ops/get_channel.rs:231-236: "// `IngestionStatus` reports the most recent ingestion attempts, and / // ingestion happens in the CloudTrail Lake data plane / // (`PutAuditEvents`, a different service). Until this channel's ingest / // log is reported here there is nothing to claim, and a synthesised / // timestamp would be a claim that events arrived. / ingestion_status: None," — the stated reason covers only SYNTHESISING a timestamp, not reporting the real ingests this crate already holds.

The seam already runs in the other direction — services/service-com-amazonaws-cloudtrail-cloudtrail-20131101/src/ops/create_channel.rs:325: `if !channels::register_channel_in_data_plane(&*self.engine, &channel_arn, &source) {`.

Modelled contract of the field left unreported (cloudtrail model) — `smithy_query(query="doc", shape="IngestionStatus")`: "A table showing information about the most recent successful and failed attempts to ingest events."; member `LatestIngestionSuccessEventID`: "The event ID of the most recent successful ingestion of events."

Test coverage: this crate's tests assert only the data-plane side (services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/tests/all/put_audit_events.rs, state_roundtrip.rs); a grep of services/service-com-amazonaws-cloudtrail-cloudtrail-20131101/tests for `ingestion_status|ingestionStatus|latestIngestion` returns "No matches", so no test pins the absent status.

## Steps to Reproduce

1. Call cloudtrail `CreateChannel` (Source `Custom`, a valid event-data-store destination) and keep the returned `ChannelArn`; this registers the channel in this service through `register_channel_in_data_plane` (create_channel.rs:325).
2. Call `cloudtrail-data` PutAuditEvents on that `channelArn` with one well-formed event and observe HTTP 200 with the event listed in `successful` (an `eventID` was minted and appended by put_audit_events.rs:519-532).
3. Call cloudtrail `GetChannel` for the same `ChannelArn`: the response carries no `IngestionStatus` at all (get_channel.rs:236 `ingestion_status: None`), so neither `LatestIngestionSuccessTime` nor `LatestIngestionSuccessEventID` reflects the ingest that just succeeded.
4. Repeat PutAuditEvents with an event whose `eventDataChecksum` does not match (answered in `failed` with `InvalidChecksum`): nothing about that refused attempt is persisted anywhere (put_audit_events.rs:506-507), so no later call can report a failed ingestion attempt.
