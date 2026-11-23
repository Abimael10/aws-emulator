Issue-Source: trace-sweep-divergence
Issue-Trace: mq-rmq-traj-rabbitmq-configuration-review
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: mq-rmq-traj-rabbitmq-configuration-review
Issue-Shape-Id: com.amazonaws.mq#mq
Issue-Key: mq|DescribeConfigurationRevision|2xx->2xx|$.data
Issue-Related: 20261103T045546Z-mq-response-bodies-carry-wrong-revision-state-and.md
Issue-Commit: 4c77ade3d5e6aed90c9f3ce2b3b42ec7a5a8e0f0

# mq DescribeConfigurationRevision does not report the revision's own stored document and description

> _Note: this issue was identified against the code base at commit `4c77ade3d5e6aed90c9f3ce2b3b42ec7a5a8e0f0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeConfigurationRevision

## Description
TWO causes in ONE response path: DescribeConfigurationRevision answers 200 with the right shape, but the `data` / `description` it serves are not the ones the revision it was asked for holds. A fixer must close BOTH before submitting. Model references are to services/service-com-amazonaws-mq-mq/smithy-model/com-amazonaws-mq-mq.json.

Cause 1 of 2: the revision a CreateConfiguration mints reports NO configuration document at all — `data` is omitted from the response (fixture mq-rmq-traj-rabbitmq-configuration-review, step task#3).
AWS: Smithy models DescribeConfigurationRevision as GET /v1/configurations/{ConfigurationId}/revisions/{ConfigurationRevision} -> 200 with output members ConfigurationId (jsonName "configurationId"), Created, Data (jsonName "data") and Description (jsonName "description"); Data's own documentation is "Amazon MQ for ActiveMQ: the base64-encoded XML configuration. Amazon MQ for RabbitMQ: base64-encoded Cuttlefish." (smithy_query doc shape=DescribeConfigurationRevisionResponse member=Data). The model states no rule about the FIRST revision, so for that part this is recorded behavior only: reading revision 1 of a freshly created configuration returns `data` present and non-empty, carrying the base64 of the engine's OWN default document for that EngineType — for RabbitMQ the default Cuttlefish settings (the recording decodes to the commented defaults `secure.management.http.headers.enabled = true` and `consumer_timeout = 1800000`), for ActiveMQ the default broker XML. The document body is an engine-level constant, not a per-run value; the requirement is that revision 1 reports the engine default rather than nothing.
Emulator: the member is absent — replay reports `$.data: missing (expected '<base64 cuttlefish default>', got None)`. The behavior is deliberate in the code: state::ConfigurationRevisionRecord.data is documented as "`None` for the revision a create makes: that one holds the engine's own default settings, which no request supplied" (services/service-com-amazonaws-mq-mq/src/state.rs, the ConfigurationRevisionRecord declaration), and DescribeConfigurationRevision's handler forwards that None straight out (services/service-com-amazonaws-mq-mq/src/ops/describe_configuration_revision.rs, the `data: revision.data.clone()` line in `handle`).
Why: a client cannot read the baseline configuration of any configuration or broker it did not itself write. The standard read-modify-write loop (fetch the current document, edit one setting, UpdateConfiguration) has nothing to fetch on a new configuration; SDK code that base64-decodes `data` unconditionally gets None/null and raises, and drift detection comparing the live document against a desired one sees no document and reports every setting as missing.
Fix: the configuration state plus this operation's handler — seed the create-time revision record with the engine's default document for the configuration's EngineType (CreateConfiguration's mint path and state::ConfigurationRevisionRecord in services/service-com-amazonaws-mq-mq/src/state.rs), and have DescribeConfigurationRevision always serve a `data` for a revision it resolves.

Cause 2 of 2: for a revision an UpdateConfiguration created, the read serves a DIFFERENT revision's document and an empty `description` instead of the document and description that write supplied (fixture mq-rmq-traj-rabbitmq-configuration-review, step task#6).
AWS: Smithy models UpdateConfiguration as PUT /v1/configurations/{ConfigurationId} with input Data (required, jsonName "data") and Description (jsonName "description"), and DescribeConfigurationRevision returns Data and Description for the revision named in the path. Recorded behavior: reading revision N returns exactly what the write that created revision N sent — in this recording the base64 of a RabbitMQ Cuttlefish document raising the consumer timeout and the description the update carried ('raise consumer timeout'). Those literals are this fixture's request values; the requirement is that the read reflects the creating write, for whatever document and description that write sent.
Emulator: serves the base64 of a default ActiveMQ XML broker document on a RabbitMQ configuration, and `description` as the empty string — replay reports `$.data: value_mismatch` and `$.description: value_mismatch (expected 'raise consumer timeout', got '')`. Hypothesis: the revision chain the emulator stores does not line up with AWS's, so the number the client was handed resolves to a neighbouring record — the emulator appends a revision for an update AWS refuses with 400 (open issue 20261103T045546Z-mq-refuses-invalid-requests-with-the-wrong-status.md owns that acceptance) and therefore reports latestRevision one too high (open issue 20261103T045546Z-mq-response-bodies-carry-wrong-revision-state-and.md, Cause 1). The empty `description` is independent of the numbering: a revision record stores `input.description.clone().unwrap_or_default()` (services/service-com-amazonaws-mq-mq/src/ops/update_configuration.rs, the ConfigurationRevisionRecord::accepted call) and the read emits that empty String rather than the description of the write being read. Note: 20261103T045546Z-mq-response-bodies-carry-wrong-revision-state-and.md describes this same symptom as its Cause 2, but its replay key covers only `$.description` for this operation, so the replay reported THIS cluster ($.data plus $.description) as unowned — it must be re-verified against this step, not assumed closed by that issue.
Why: a client that writes a configuration revision and reads it back gets neither its document nor its label, and on a RabbitMQ configuration it gets an ActiveMQ XML body that no Cuttlefish parser accepts. Config-as-code reconciliation, revision diffing and rollback-to-revision all operate on the wrong bytes, and a broker pinned to that revision number runs a document the operator never wrote.
Fix: the configuration-revision state plus this operation's handler — persist the accepted Data AND Description on the revision record keyed by the number that write was assigned, assign a number only for a write the service accepted, and serve both on read (services/service-com-amazonaws-mq-mq/src/state.rs, services/service-com-amazonaws-mq-mq/src/ops/update_configuration.rs, services/service-com-amazonaws-mq-mq/src/ops/describe_configuration_revision.rs).

Traces: mq-rmq-traj-rabbitmq-configuration-review#task#3 (Cause 1), mq-rmq-traj-rabbitmq-configuration-review#task#6 (Cause 2). Both clusters came back with owned_by empty and stub_path empty.

## Evidence
Replay summary: trace_set simparity-wire-aws-v1, emulator_up true, 9 fixtures replayed, 68 steps (46 passed, 22 mismatched), score 67.6%, 15 divergence clusters.

Cause 1 — mq-rmq-traj-rabbitmq-configuration-review#task#3, DescribeConfigurationRevision, expected 200 / actual 200:
  $.data: missing (expected 'IyBNYW5hZ2VtZW50IGhlYWRlciBpcyBzZXQgdG8gdHJ1ZSB0byBlbmFibGUgQ29udGVudC1UeXBlLU9wdGlvbnMsIEZyYW1lLU9wdGlvbnMsIGFuZCBIU1RTIGhlYWRlcnMgYnkgZGVmYXVsdApzZWN1cmUubWFuYWdlbWVudC5odHRwLmhlYWRlcnMuZW5hYmxlZCA9IHRydWUKIyBEZWZhdWx0IFJhYmJpdE1RIGRlbGl2ZXJ5IGFja25vd2xlZGdlbWVudCB0aW1lb3V0IGlzIDMwIG1pbnV0ZXMKY29uc3VtZXJfdGltZW91dCA9IDE4MDAwMDAK', got None)
  That base64 decodes to the RabbitMQ engine-default Cuttlefish document:
    # Management header is set to true to enable Content-Type-Options, Frame-Options, and HSTS headers by default
    secure.management.http.headers.enabled = true
    # Default RabbitMQ delivery acknowledgement timeout is 30 minutes
    consumer_timeout = 1800000
  The requirement is that revision 1 carries the engine default for the configuration's EngineType; the exact default text is per-engine, not per-run.

Cause 2 — mq-rmq-traj-rabbitmq-configuration-review#task#6, DescribeConfigurationRevision, expected 200 / actual 200:
  $.data: value_mismatch (expected 'Y29uc3VtZXJfdGltZW91dCA9IDM2MDAwMDAK' = 'consumer_timeout = 3600000\n', got 'PD94bWwgdmVyc2lvbj0iMS4wIj8+PGJyb2tlciB4bWxucz0iaHR0cDovL2FjdGl2ZW1xLmFwYWNoZS5vcmcvc2NoZW1hL2NvcmUiLz4K' = '<?xml version="1.0"?><broker xmlns="http://activemq.apache.org/schema/core"/>\n')
  $.description: value_mismatch (expected 'raise consumer timeout', got '')
  The document text and the description are this fixture's request values; the requirement is that the read returns whatever the write that created that revision supplied, and never an ActiveMQ XML body on a RabbitMQ configuration.

Neither cluster carried stub_path or owned_by, so neither is covered by an auto-filed stub issue or by an open issue's replay key.

## Steps to Reproduce
1. git clone the repo, boot the emulator built from that checkout, and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Cause 1 — fixture mq-rmq-traj-rabbitmq-configuration-review, step task#3: CreateConfiguration with EngineType=RABBITMQ, then GET /v1/configurations/{id}/revisions/1. AWS answers 200 with `data` present, carrying the base64 of the engine's default Cuttlefish document; the emulator answers 200 with no `data` member at all.
3. Cause 2 — same fixture, step task#6: submit an UpdateConfiguration whose Data the engine cannot parse (AWS refuses it 400), then submit a valid UpdateConfiguration carrying a Cuttlefish document and a Description, note the latestRevision.revision it returns, and GET /v1/configurations/{id}/revisions/{that revision}. AWS returns the document and description that update sent; the emulator returns a default ActiveMQ XML body and an empty `description`.
4. Base64-decode `data` in both reads and compare with the document each revision was created from.
