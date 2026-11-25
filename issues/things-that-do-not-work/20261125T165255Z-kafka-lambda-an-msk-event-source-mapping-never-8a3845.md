Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka__lambda.msk-lambda-esm-filter-criteria-delivers-only-matching-records-and-a-cleared-filter-releases-the-rest kafka__lambda.msk-lambda-event-source-mapping-disabled-delivers-nothing-then-reenabled-delivers-the-missed-records
Issue-Key: kafka__lambda.msk-lambda-esm-filter-criteria-delivers-only-matching-records-and-a-cleared-filter-releases-the-rest|_poll#5|Timeout|time.sleep(interval_s)
Issue-Key: kafka__lambda.msk-lambda-event-source-mapping-disabled-delivers-nothing-then-reenabled-delivers-the-missed-records|_poll#3|Timeout|return last
Issue-Commit: 6e4695f2e2e0380b6e4530904193a77f767a96ef

# kafka__lambda: an MSK event source mapping never delivers a record, so neither FilterCriteria selection nor the disable/re-enable catch-up reaches a verdict

> _Note: this issue was identified against the code base at commit `6e4695f2e2e0380b6e4530904193a77f767a96ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
Cause 1 of 1: an event source mapping over an MSK topic never invokes the target function, so both Kafka ESM scenarios spend their whole 60 s budget on the first delivery wait and reach no verdict. Failing checks: kafka__lambda.msk-lambda-esm-filter-criteria-delivers-only-matching-records-and-a-cleared-filter-releases-the-rest and kafka__lambda.msk-lambda-event-source-mapping-disabled-delivers-nothing-then-reenabled-delivers-the-missed-records.

AWS: Smithy (lambda) models CreateEventSourceMapping POST /2015-03-31/event-source-mappings -> 202 with EventSourceMappingConfiguration (UUID, State, Topics, FilterCriteria, AmazonManagedKafkaEventSourceConfig) and UpdateEventSourceMapping PUT /2015-03-31/event-source-mappings/{UUID} -> 202 for Enabled/FilterCriteria changes. For an MSK source, an Enabled mapping makes Lambda consume the named topic and invoke the function with the Kafka records event; FilterCriteria is matched against the record value parsed as JSON and a discarded record's offset is still committed; a mapping switched to Enabled=false delivers nothing and, when re-enabled, delivers the records produced while it was disabled from the committed offset. In AWS the first delivery after the mapping is Enabled is observed within a client-sized poll. Recorded behavior only for the exact delivery latency.

Emulator: the mappings are accepted and read back, but no record ever reaches the function — neither the matching record of the filter scenario nor the first record of the disable/re-enable scenario — and both checks end with "Check timed out after 60.0s". Hypothesis: the emulator stores the MSK event source mapping as control-plane state but runs no Kafka consumer for it, so neither filter selection nor the disabled/re-enabled distinction can be observed at all.

Why: a client that wires an MSK topic to a function sees its records silently never processed while the control plane reports a healthy mapping. FilterCriteria (including the documented way to clear criteria with an empty FilterCriteria object) and the Enabled toggle have no observable effect, so neither can be relied on or tested.

Fix: the Lambda event-source-mapping poller for Amazon MSK sources (the component that consumes the mapping's Topics for an Enabled mapping, applies FilterCriteria to the record value, commits offsets for discarded records, invokes the target function with the Kafka records event, and honours Enabled=false / re-enable catch-up).

Related open issue 20261116T210056Z-kafka-lambda-an-msk-event-source-mapping-never.md describes the same missing delivery for this service group but does not carry these checks' replay keys.

Traces: the two failing checks listed in trace_ids.

## Evidence
Expected (AWS): with an Enabled MSK event source mapping on topic T, producing a record whose JSON value matches FilterCriteria invokes the function with that record (and a non-matching record is discarded while its offset advances); with Enabled=false no invocation occurs, and after UpdateEventSourceMapping Enabled=true the records produced while disabled are delivered.

Actual (emulator): no invocation ever occurs in either scenario; the first delivery wait never satisfies and each check ends with "Error: Check timed out after 60.0s". The 60 s figure is the harness budget; the requirement is that an Enabled MSK mapping delivers its topic's records.

## Steps to Reproduce
1. Create an MSK cluster and a topic, and a Lambda function that records the Kafka records event it receives.
2. CreateEventSourceMapping with the cluster ARN as EventSourceArn, Topics=[T], Enabled=true (and FilterCriteria for the filter scenario); wait until the mapping reads back Enabled.
3. Produce a record to T whose value is JSON matching the filter.
4. Poll for the function's invocation record.
5. Observe nothing is ever delivered; repeat with Enabled=false then true and observe no catch-up delivery either.
