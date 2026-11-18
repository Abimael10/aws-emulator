Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eventbridgev2__iam__sqs.put-raw-events-invalid-json-and-open-format-refused-per-entry
Issue-Key: eventbridgev2__iam__sqs.put-raw-events-invalid-json-and-open-format-refused-per-entry|run#14|AssertionError|assert (not_json.get("ErrorCode"), not_json.get("ErrorMessage")) == (
Issue-Commit: 04a6a5204a1295e48b5df0dc3f28c0ee9b0b14f6

# eventbridgev2 PutRawEvents: the per-entry refusal wording of a malformed raw payload diverges, and an open-format entry can never be published

> _Note: this issue was filed by the fixer of
> `issues/things-that-now-work/20261113T094431Z-eventbridgev2-putrawevents-stub.md`, which
> implemented `PutRawEvents`. It records the two parts of that operation that are still
> not right, so neither is lost with the stub issue._

## Operation
PutRawEvents

## Description

=== Cause 1 of 2: the per-entry `ErrorCode` / `ErrorMessage` of a raw entry whose `Data` is not the form its `ContentType` declares is not the recorded pair ===
AWS: recorded behaviour. The behavioural check
`eventbridgev2__iam__sqs.put-raw-events-invalid-json-and-open-format-refused-per-entry`
publishes a batch holding an entry whose `ContentType` is `application/json` and whose
`Data` is not a JSON document, beside an open-format entry, and asserts the first
entry's `(ErrorCode, ErrorMessage)` pair against the recorded one:
`assert (not_json.get("ErrorCode"), not_json.get("ErrorMessage")) == (…)`. The replay
harness withholds the recorded strings, so the pair itself is not known from here.
Emulator: the entry IS refused in its own position, with no `EventId`, no
`SequenceNumber` and no `SuccessCode`, and `FailedEntryCount` counts it — only the
wording diverges. The pair the handler answers is derived from the same entry-error
class's recorded wording on the sibling publish (`MalformedDetail` / "Detail is not
valid JSON", recorded in
`issues/things-that-now-work/20261111T025659Z-eventbridgev2-putevents-mis-reports-failed-entries-and-words.md`)
with the member swapped: `MalformedData` / "Data is not valid JSON". Ten candidate pairs
were replayed (the plain and content-type-qualified messages, a `must be`-style message,
and the codes `ValidationError`, `MalformedJson` and `InvalidJson`) and every one was
rejected, so the recorded pair is something else again.
Why: a producer that branches on the per-entry `ErrorCode` to quarantine the bad entries
of a batch matches on a code the real service does not send, and a client asserting the
message text does not match.
Fix: the malformed-payload refusal in
`services/service-com-amazonaws-eventbridgev2-awseventsv2/src/ops/put_raw_events.rs`
(`MALFORMED_DATA` / `MALFORMED_DATA_MESSAGE`) and the row that pins it in
`tests/all/put_events_entry_results.rs`. It needs the recorded pair, from a wire fixture
of this operation or from any other disclosure of the recorded response.
Traces: eventbridgev2__iam__sqs.put-raw-events-invalid-json-and-open-format-refused-per-entry

=== Cause 2 of 2: an open-format (Avro / Protobuf) entry can never be published ===
AWS: Smithy — `SchemaRegistryConfiguration` is "schema registry settings used to encode
open-format (Avro/Protobuf) events", "required for open-format entries; ignored for JSON
entries", and "the registry is read with the caller's credentials, so the caller needs
read access to the registry it references". So an entry whose `ContentType` names Avro or
Protobuf is published once its schema is read out of the registry the request names.
Emulator: an entry naming an open format is refused in its own position when the request
names no registry (`ValidationError`, naming the member it needs), and a batch that DOES
name one is refused whole with the modelled `SchemaRegistryUnavailableException` (400),
because this process reaches no registry outside itself — nothing is stored either way,
which is honest, but it leaves a whole modelled input class unserved: no Avro or Protobuf
event can be put on a bus, so nothing downstream of one can be exercised.
Why: a producer publishing Avro or Protobuf through a schema registry cannot use the
emulator at all, and a consumer of such a bus sees an empty one.
Fix: resolve a `RegistryUri` that names a registry this emulator itself hosts (a
`arn:…:glue:…:registry/…` value addresses the Glue schema registry mounted beside this
service) and decode the entry against the schema that registry returns, rather than
answering the unreachable-registry error for every open-format batch. The refusal belongs
only to a registry nothing in this emulator serves.
Code: `services/service-com-amazonaws-eventbridgev2-awseventsv2/src/ops/put_raw_events.rs`
(the `SchemaRegistryUnavailableException` branch and the per-entry open-format refusal),
`src/state/publishing.rs` (`is_open_format_content_type`).
Traces: eventbridgev2__iam__sqs.put-raw-events-invalid-json-and-open-format-refused-per-entry

## Evidence
Cause 1 — AssertionError: the malformed JSON entry answered
`{'ErrorCode': 'MalformedData', 'ErrorMessage': 'Data is not valid JSON'}`; the expected
pair is withheld by the replay harness. The other five units of the stub issue pass, so
the batch, the positions and `FailedEntryCount` are right and only this pair is not.

Cause 2 — `a_schema_registry_is_read_only_for_the_entries_that_need_one` in
`services/service-com-amazonaws-eventbridgev2-awseventsv2/tests/all/put_events_entry_results.rs`
pins today's behaviour: a JSON-only batch naming a registry publishes, and a batch
carrying an open-format entry beside a configured registry answers
`SchemaRegistryUnavailableException` (400) and stores nothing.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. `CreateEventBus`, wait for `ACTIVE`, then `PutRawEvents` with two entries: one whose
   `SystemMetadata.ContentType` is `application/json` and whose `Data` is `{"a":1` (no
   closing brace), and one whose `ContentType` is `application/avro`, with no
   `SchemaRegistryConfiguration`. Read `Entries[0].ErrorCode` / `ErrorMessage` (cause 1).
3. `PutRawEvents` with an `application/avro` entry and a
   `SchemaRegistryConfiguration` naming a Glue registry, and observe the whole batch
   refused with `SchemaRegistryUnavailableException` rather than the entry published
   (cause 2).
