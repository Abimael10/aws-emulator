Issue-Source: trace-sweep-divergence
Issue-Trace: kinesis-describe-limits-channel-fields-and-account-settings-raw-wire-validation
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: kinesis-describe-limits-channel-fields-and-account-settings-raw-wire-validation kinesis-stream-retention-raised-to-its-default-tags-put-and-get-record-and-not-found
Issue-Shape-Id: com.amazonaws.kinesis#Kinesis_20131202
Issue-Key: kinesis|DescribeLimits|2xx->2xx|$.ChannelCount
Issue-Key: kinesis|DescribeLimits|2xx->2xx|$.ChannelCountLimit
Issue-Key: kinesis|UpdateAccountSettings|4xx->4xx|$.message
Issue-Key: kinesis|DescribeStreamSummary|2xx->2xx|$.StreamDescriptionSummary.StreamCreationTimestamp
Issue-Key: kinesis|GetRecords|2xx->2xx|$.Records[*].ApproximateArrivalTimestamp
Issue-Commit: 9af7726e852eadd1ad5f5bb12067c11cebe7515a

# kinesis: 4 net-new causes — DescribeLimits omits ChannelCount/ChannelCountLimit, enum value set mis-ordered, epoch timestamps carry sub-millisecond precision

> _Note: this issue was identified against the code base at commit `9af7726e852eadd1ad5f5bb12067c11cebe7515a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeLimits

## Description
Four net-new wire divergences in this service's awsJson1_1 response and error payloads: a success payload drops two modelled output members, an enum refusal message orders its value set wrongly, and the service's one timestamp helper emits a precision AWS never puts on the wire (observed on two different operations). All four are in the kinesis service and must each be closed and re-replayed.

Cause 1 of 4: DescribeLimits never reports ChannelCount or ChannelCountLimit. Fixture kinesis-describe-limits-channel-fields-and-account-settings-raw-wire-validation, step task#1.
AWS: Smithy DescribeLimits (services/service-com-amazonaws-kinesis-kinesis-20131202/smithy-model/com-amazonaws-kinesis-kinesis-20131202.json) models DescribeLimitsOutput with ShardLimit, OpenShardCount, OnDemandStreamCount, OnDemandStreamCountLimit (all required) plus ChannelCount (ChannelCountObject, range 0..1000000) "The number of channels in the account" and ChannelCountLimit (ChannelCountObject, range 0..1000000) "The maximum number of channels allowed in the account". The recorded AWS response for this account-level read is HTTP 200 and carries both members as integers (the recording asserts their presence and shape, not a particular count), i.e. an account that holds no channels is still told its channel usage and its channel ceiling.
Emulator: answers HTTP 200 with ChannelCount and ChannelCountLimit ABSENT from the JSON body ($.ChannelCount missing, $.ChannelCountLimit missing). The handler hard-codes both to None (services/service-com-amazonaws-kinesis-kinesis-20131202/src/ops/describe_limits.rs, the `channel_count: None, channel_count_limit: None` tail of `handle`), justified by a comment that the emulator "holds no channels at all" — which is stale: the service does hold channel records (crate::state::channels, CreateChannel/ListChannels).
Why: a client that reads the account's channel usage and headroom from DescribeLimits (capacity planners, dashboards, admission checks before CreateChannel) gets no value at all where AWS always supplies one, so an SDK deserialises Optional/absent and the caller either crashes on the unwrap or silently treats the account as having no channel quota information. There is no way to discover the channel ceiling the emulator itself enforces.
Fix: the DescribeLimits handler — populate both members from the channel store the same way the shard and on-demand figures are populated: ChannelCount from the channels the request's account/region actually holds at `now`, ChannelCountLimit from the one constant every channel-creating operation is refused past, so the reported ceiling is the enforced ceiling.
Traces: kinesis-describe-limits-channel-fields-and-account-settings-raw-wire-validation#task#1.

Cause 2 of 4: the UpdateAccountSettings enum ValidationException lists the allowed values in alphabetical order instead of the model's declaration order. Fixture kinesis-describe-limits-channel-fields-and-account-settings-raw-wire-validation, step task#4.
AWS: Smithy UpdateAccountSettingsInput.MinimumThroughputBillingCommitment (required) -> MinimumThroughputBillingCommitmentInput.Status (required) -> enum MinimumThroughputBillingCommitmentInputStatus, whose values in DECLARATION order are ENABLED then DISABLED; UpdateAccountSettings models ValidationException (client error, HTTP 400). A value outside the modelled enum is refused with HTTP 400 ValidationException and the standard counted constraint message. Its fixed text is "<N> validation error[s] detected: Value '<value>' at '<memberPath>' failed to satisfy constraint: Member must satisfy enum value set: [<values>]"; its variable parts are N (number of violations), <value> and <memberPath> taken from the request, and <values> taken from the model — the enum's members comma-and-space separated IN DECLARATION ORDER, i.e. "ENABLED, DISABLED" for this member.
Emulator: HTTP 400 and error code ValidationException are correct, but the message's value set is emitted lexicographically: "... failed to satisfy constraint: Member must satisfy enum value set: [DISABLED, ENABLED]" where AWS emits "[ENABLED, DISABLED]". Count, quoted value and member path match. The set is built in services/service-com-amazonaws-kinesis-kinesis-20131202/src/ops/update_account_settings.rs (`off_catalog_status`, ViolationKind::EnumValue collected from MINIMUM_THROUGHPUT_BILLING_COMMITMENT_INPUT_STATUS_VALUES) and rendered by the shared constraint renderer's `envelope`, one of which re-orders the members.
Why: the message text of a refusal is part of the contract — a client that asserts on or diffs the exact AWS message (contract tests, golden files, support tooling that parses the bracketed list to present the allowed choices in the service's documented order) sees a message AWS never emits. The defect is in a shared renderer, so every operation whose enum order is not alphabetical diverges the same way.
Note: open behavioral issue 20261112T165429Z-kinesis-updateaccountsettings-enum-validationexception-lists-the-allowed-values describes this same fault from the behavioral corpus, but it carries no wire replay key, so this wire step is not covered by it and is filed here.
Fix: the enum-value-set rendering of the constraint violation path used by UpdateAccountSettings' validation — emit the members in the order the Smithy enum declares them (preserve the schema's `values` sequence) instead of sorting them or round-tripping them through an ordered-by-key/unordered set.
Traces: kinesis-describe-limits-channel-fields-and-account-settings-raw-wire-validation#task#4.

Cause 3 of 4: DescribeStreamSummary serialises StreamCreationTimestamp with sub-millisecond precision. Fixture kinesis-stream-retention-raised-to-its-default-tags-put-and-get-record-and-not-found, steps task#2 and task#4.
AWS: Smithy DescribeStreamSummaryOutput.StreamDescriptionSummary (required) -> StreamDescriptionSummary.StreamCreationTimestamp (required, target com.amazonaws.kinesis#Timestamp) "The approximate time that the stream was created". The protocol is awsJson1_1, so a timestamp travels as an epoch-SECONDS JSON number; recorded behavior only for the resolution: AWS's recorded Kinesis wire carries these epoch-second numbers at MILLISECOND resolution at most (at most three fractional digits, e.g. 1.441215410867E9), which is the resolution of the instant the service stores, and the fixture asserts that shape rather than a particular instant.
Emulator: answers HTTP 200 with $.StreamDescriptionSummary.StreamCreationTimestamp as an epoch-second number carrying sub-microsecond fractional digits (observed shape: a value like <epoch_seconds>.4445777, seven fractional digits), which fails the recorded shape. The whole-float conversion is the service's single timestamp helper, `pub fn timestamp(at: SystemTime)` in services/service-com-amazonaws-kinesis-kinesis-20131202/src/state.rs, which builds `DateTime::from_secs_f64(since.as_secs_f64())` from the raw SystemTime; StreamDescriptionSummary is built through it (same file, the Stream -> StreamDescriptionSummary conversion).
Why: a client that round-trips or compares the creation instant against anything else the service hands out at millisecond resolution (the millisecond-stamped sequence numbers, pagination and shard-iterator tokens, its own records of the create call) sees instants that never compare equal and that re-serialise with digits AWS never emits; strict parsers and golden-file comparisons of the raw JSON reject the extra precision outright.
Fix: the shared timestamp conversion in this service's state module (`timestamp`) — construct the modelled DateTime from whole milliseconds (the same epoch-millisecond basis `epoch_millis` already provides) so every timestamp this service serialises is truncated to AWS's millisecond resolution; the operations themselves need no change.
Traces: kinesis-stream-retention-raised-to-its-default-tags-put-and-get-record-and-not-found#task#2 (and task#4 in the same cluster).

Cause 4 of 4: GetRecords serialises Records[].ApproximateArrivalTimestamp with the same sub-millisecond precision. Fixture kinesis-stream-retention-raised-to-its-default-tags-put-and-get-record-and-not-found, step task#15. Same root defect as Cause 3 (the one `timestamp` helper), reported on a second operation and a second output shape, so it must be re-replayed separately.
AWS: Smithy GetRecordsOutput.Records (required) -> Record.ApproximateArrivalTimestamp (target com.amazonaws.kinesis#Timestamp) "The approximate time that the record was inserted into the stream", serialised by awsJson1_1 as an epoch-seconds JSON number; recorded behavior only for the resolution — AWS's recorded value carries at most millisecond precision, and the fixture asserts that shape, not a particular instant.
Emulator: answers HTTP 200 with $.Records[0].ApproximateArrivalTimestamp as an epoch-second number with sub-microsecond fractional digits (observed shape: a value like <epoch_seconds>.5311885), failing the recorded shape. Path: services/service-com-amazonaws-kinesis-kinesis-20131202/src/ops/get_records.rs builds each Record via `record.to_shape()`, which sets `approximate_arrival_timestamp: Some(timestamp(self.arrived_at))` through the same helper in state.rs.
Why: consumers derive lag and ordering from ApproximateArrivalTimestamp and compare it with MillisBehindLatest and with their own millisecond clocks; a value with precision AWS never emits re-serialises differently than AWS's and breaks exact-wire comparisons and strict deserialisers, while telling a consumer the stream has sub-millisecond arrival resolution that the service does not actually keep.
Fix: same component as Cause 3 — the `timestamp` helper in this service's state module; once it truncates to whole milliseconds, Record's arrival timestamp is correct without touching the GetRecords handler.
Traces: kinesis-stream-retention-raised-to-its-default-tags-put-and-get-record-and-not-found#task#15.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 30 fixtures, 187 steps, 182 passed, 5 mismatched, 0 errored, 0 blocked, score 97.3%. All 4 divergence clusters are service=kinesis; none carried a stub_path or an owned_by.

Cause 1 — fixture kinesis-describe-limits-channel-fields-and-account-settings-raw-wire-validation, step task#1, operation DescribeLimits, verdict mismatch.
  Expected: HTTP 200 with $.ChannelCount and $.ChannelCountLimit present (shape asserted, value not asserted).
  Actual:   HTTP 200, $.ChannelCount: missing (got None); $.ChannelCountLimit: missing (got None).

Cause 2 — same fixture, step task#4, operation UpdateAccountSettings, verdict mismatch.
  Expected: HTTP 400, $.message = "1 validation error detected: Value 'sim2real-bogus' at 'minimumThroughputBillingCommitment.status' failed to satisfy constraint: Member must satisfy enum value set: [ENABLED, DISABLED]"
  Actual:   HTTP 400, $.message = "1 validation error detected: Value 'sim2real-bogus' at 'minimumThroughputBillingCommitment.status' failed to satisfy constraint: Member must satisfy enum value set: [DISABLED, ENABLED]"
  Message anatomy — fixed text: "1 validation error detected: Value '", "' at '", "' failed to satisfy constraint: Member must satisfy enum value set: [", "]". Variable from the request: the rejected value ('sim2real-bogus' in this recording — any rejected value fills that slot) and the member path 'minimumThroughputBillingCommitment.status'. Variable from the model, and the only diverging part: the bracketed member list, which must be the enum's declaration order ENABLED, DISABLED (Smithy MinimumThroughputBillingCommitmentInputStatus values: ["ENABLED", "DISABLED"]). Status 400 and code ValidationException already match.

Cause 3 — fixture kinesis-stream-retention-raised-to-its-default-tags-put-and-get-record-and-not-found, steps task#2 and task#4, operation DescribeStreamSummary, verdict mismatch.
  Expected: HTTP 200, $.StreamDescriptionSummary.StreamCreationTimestamp of the recorded shape (epoch-seconds number, millisecond resolution; value not asserted).
  Actual:   HTTP 200, value_mismatch — a number with seven fractional digits (this run: 1794964001.4445777). The digits past the third are the divergence; the instant itself is per-run and is not a requirement.

Cause 4 — same fixture, step task#15, operation GetRecords, verdict mismatch.
  Expected: HTTP 200, $.Records[0].ApproximateArrivalTimestamp of the recorded shape (epoch-seconds number, millisecond resolution; value not asserted).
  Actual:   HTTP 200, value_mismatch — a number with seven fractional digits (this run: 1794964006.5311885).

Code grounding: services/service-com-amazonaws-kinesis-kinesis-20131202/src/ops/describe_limits.rs sets channel_count/channel_count_limit to None unconditionally; src/ops/update_account_settings.rs `off_catalog_status` builds ViolationKind::EnumValue from MINIMUM_THROUGHPUT_BILLING_COMMITMENT_INPUT_STATUS_VALUES and renders it through the shared `envelope`; src/state.rs `timestamp()` returns DateTime::from_secs_f64(duration.as_secs_f64()) and is the single source of both diverging timestamps (Record::to_shape and the StreamDescriptionSummary conversion).

## Steps to Reproduce
1. Clone the repo and build/boot the emulator from this checkout; point a kinesis client at the emulator's data listener.
2. Cause 1: call DescribeLimits with no input. Observe HTTP 200 whose JSON body has no ChannelCount and no ChannelCountLimit member, where AWS returns both as integers.
3. Cause 2: call UpdateAccountSettings with MinimumThroughputBillingCommitment={"Status": "<any value outside the enum>"} (disable client-side parameter validation so the value reaches the service). Observe HTTP 400 ValidationException whose message ends "enum value set: [DISABLED, ENABLED]" where AWS emits "[ENABLED, DISABLED]".
4. Cause 3: CreateStream, wait for ACTIVE, then call DescribeStreamSummary and read the raw JSON body. Observe StreamDescriptionSummary.StreamCreationTimestamp as an epoch-seconds number with more than three fractional digits.
5. Cause 4: PutRecord into that stream, GetShardIterator, then GetRecords and read the raw JSON body. Observe Records[0].ApproximateArrivalTimestamp as an epoch-seconds number with more than three fractional digits.
6. Re-replay the wire corpus and confirm these steps pass: kinesis-describe-limits-channel-fields-and-account-settings-raw-wire-validation#task#1 and #task#4, and kinesis-stream-retention-raised-to-its-default-tags-put-and-get-record-and-not-found#task#2, #task#4 and #task#15.
