Issue-Source: trace-sweep-divergence
Issue-Trace: firehose-kfi-traj-stream-before-glue-table
Issue-Step: task#12
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: firehose-kfi-traj-stream-before-glue-table
Issue-Shape-Id: com.amazonaws.firehose#Firehose_20150804
Issue-Key: firehose|CreateDeliveryStream|2xx->4xx|$.DeliveryStreamARN
Issue-Key: firehose|CreateDeliveryStream|2xx->4xx|status
Issue-Commit: d56f3e72fadd2bcc773c18998c38d59f68ab7320

# firehose CreateDeliveryStream refuses the retry that AWS accepts once the Glue table exists

> _Note: this issue was identified against the code base at commit `d56f3e72fadd2bcc773c18998c38d59f68ab7320`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeliveryStream

## Description
One cause.

Cause 1 of 1: after the Glue database/table the Iceberg destination names has been created, CreateDeliveryStream for that stream must succeed, but the emulator still refuses it with HTTP 400 and returns no DeliveryStreamARN.

AWS: Smithy (services/service-com-amazonaws-firehose-firehose-20150804/smithy-model/com-amazonaws-firehose-firehose-20150804.json) models CreateDeliveryStream with required DeliveryStreamName (@length 1..64, @pattern ^[a-zA-Z0-9_.-]+$), an optional IcebergDestinationConfiguration, and CreateDeliveryStreamOutput carrying DeliveryStreamARN (@length 1..512, @pattern ^arn:.*:firehose:[a-zA-Z0-9\-]+:\d{12}:deliverystream/[a-zA-Z0-9._-]+$); modeled errors are InvalidArgumentException, InvalidKMSResourceException, LimitExceededException and ResourceInUseException (all client, HTTP 400). AWS resolves the Iceberg destination's Glue table at create time with the configuration's RoleARN; while that table is absent the create is REFUSED (InvalidArgumentException) and no stream is left behind, so once the table is created the very same create request is accepted with HTTP 200 and a DeliveryStreamARN of the modeled ARN form for the requested name. The recording for this step is exactly that: HTTP 200 with DeliveryStreamARN arn:aws:firehose:<region>:<accountId>:deliverystream/<deliveryStreamName> (region, account id and stream name all taken from the request/context — only the ARN shape is the requirement).

Emulator: HTTP 400 with no $.DeliveryStreamARN. The earlier creates of this stream — which AWS refused while the Glue table did not exist — were accepted (or charged against the name), so the name is already taken in emulator state and the name-uniqueness check in the CreateDeliveryStream handler rejects the retry; the ARN the caller needs is never minted.

Why: the documented recovery path for an Iceberg stream whose catalog target does not yet exist — create the Glue table, then create the stream — is unavailable. A client that provisions catalog and stream in that order can never get the stream created under its chosen name, and it cannot distinguish its own duplicate from the emulator's leftover ghost, so the whole downstream delivery configuration is unreachable.

Fix: the CreateDeliveryStream handler and the delivery-stream state it writes (services/service-com-amazonaws-firehose-firehose-20150804, the create handler's Iceberg validation and the name-uniqueness/registration ordering): a create that is refused for an unresolvable Glue table must register NOTHING, so a later create of the same name after the table exists is accepted, returns 200 and mints the modeled DeliveryStreamARN.

Related but not covering this: open issue 20261102T123530Z-firehose-iceberg-create-update-validation-misses-the-glue.md owns the earlier steps of this fixture (task#6 and task#9 — the missing glue:GetTable check and the ResourceInUseException mis-identity) but carries no replay key for task#12; replay reports this cluster as unowned (owned_by empty), so the successful-retry step is not exercised by that issue.

Traces: firehose-kfi-traj-stream-before-glue-table#task#12 (CreateDeliveryStream), affected_count 1.

## Evidence
Fixture firehose-kfi-traj-stream-before-glue-table, step task#12, service firehose, operation CreateDeliveryStream, verdict mismatch.

  expected: HTTP 200, $.DeliveryStreamARN present, of the modeled form arn:aws:firehose:<region>:<accountId>:deliverystream/<deliveryStreamName> (recorded instance: arn:aws:firehose:us-west-2:000000000000:deliverystream/sim2real-probe-kfi-ds-ebfc1ad7 — the region, account id and name are request/context values, not requirements)
  actual:   HTTP 400, $.DeliveryStreamARN missing

Replay detail, verbatim: "status: expected 200, got 400" and "$.DeliveryStreamARN: missing".

Model citation: Smithy firehose#CreateDeliveryStream -> CreateDeliveryStreamOutput.DeliveryStreamARN (@length 1..512, @pattern ^arn:.*:firehose:[a-zA-Z0-9\-]+:\d{12}:deliverystream/[a-zA-Z0-9._-]+$); errors InvalidArgumentException / InvalidKMSResourceException / LimitExceededException / ResourceInUseException, all HTTP 400.

## Steps to Reproduce
1. Replay the wire corpus: replay_wire_corpus() against the emulator built from this checkout.
2. Fixture firehose-kfi-traj-stream-before-glue-table: steps task#6 and task#9 attempt CreateDeliveryStream with an IcebergDestinationConfiguration whose Glue table does not exist yet (AWS refuses both, leaving no stream); the fixture then creates the Glue database/table.
3. Step task#12 re-issues the same CreateDeliveryStream. It must answer HTTP 200 with a DeliveryStreamARN of the form arn:aws:firehose:<region>:<accountId>:deliverystream/<deliveryStreamName>; the emulator answers HTTP 400 with no ARN.
4. Minimal repro: CreateDeliveryStream with an Iceberg destination naming a nonexistent Glue table (expect 400 and no stream registered), create that Glue table, then re-issue the identical CreateDeliveryStream and check for 200 + DeliveryStreamARN.
