Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kinesis.at-sequence-number-from-foreign-shard-rejected
Issue-Shape-Id: com.amazonaws.kinesis#Kinesis_20131202
Issue-Key: kinesis|GetShardIterator|InvalidArgumentException
Issue-Commit: 6210f6c7b7517e23e500cf0aa115bf547e8deaf0

# kinesis GetShardIterator's foreign-shard AT_SEQUENCE_NUMBER InvalidArgumentException message is an emulator-authored sentence, not AWS's "Invalid StartingSequenceNumber. It encodes ..." frame

> _Note: this issue was identified against the code base at commit `6210f6c7b7517e23e500cf0aa115bf547e8deaf0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetShardIterator

## Description
One net-new root cause in the kinesis data-plane read path, grounded in the one failing behavioral check of this replay that is neither already-filed nor owned by an open issue.

Cause 1 of 1: GetShardIterator now correctly REFUSES an AT_SEQUENCE_NUMBER / AFTER_SEQUENCE_NUMBER StartingSequenceNumber that was minted on a different shard of the same stream, and it refuses it with the right status and the right error code — but the refusal MESSAGE is a sentence of the emulator's own invention instead of the sentence AWS words this refusal class with.
Failing check: kinesis.at-sequence-number-from-foreign-shard-rejected.

AWS: Smithy (services/service-com-amazonaws-kinesis-kinesis-20131202/smithy-model/com-amazonaws-kinesis-kinesis-20131202.json) models GetShardIterator input { ShardId required (len 1..128, pattern ^[a-zA-Z0-9_.-]+$), ShardIteratorType required (one of AFTER_SEQUENCE_NUMBER | AT_SEQUENCE_NUMBER | AT_TIMESTAMP | LATEST | TRIM_HORIZON), StartingSequenceNumber (pattern ^(0|([1-9]\d{0,128}))$), StreamName / StreamARN } and declares InvalidArgumentException, errorKind client, HTTP 400, documented "A specified parameter exceeds its restrictions, is not supported, or can't be used. For more information, see the returned message." The model does not state any message text, so for the TEXT this is recorded behavior only. A Kinesis sequence number is shard-scoped — its value encodes the shard that minted it — and the recording shows that addressing shard B with a StartingSequenceNumber minted on shard A of the same stream is refused HTTP 400, code InvalidArgumentException, message: "Invalid StartingSequenceNumber. It encodes <shard id the sequence number encodes>, while it was used in a call to a shard with <ShardId from the request>". The FIXED text of that frame is "Invalid StartingSequenceNumber. It encodes " / ", while it was used in a call to a shard with " — and there is NO terminating period. Its VARIABLE parts are two shard ids: the shard the supplied sequence number decodes to (from the emulator's own shard map / sequence-number encoding), and the ShardId taken verbatim from the request. The sequence-number VALUE itself does not appear in AWS's sentence at all. The particular shard ids and sequence number of any one run only fill that template and are not part of it.

Emulator: the status (400) and the error code (InvalidArgumentException) are correct, and the control arm — the same sequence number used on its OWN shard — still returns a working iterator, so only the sentence diverges. The emulator answers a different sentence with a different shape: "<member name> <sequence number value> is not a sequence number of shard <ShardId>" — it names the input MEMBER and the raw sequence-number VALUE, names only the shard that was ADDRESSED, and never names the shard the value actually encodes (the single most useful fact in AWS's sentence). The replayed check asserts the AWS frame and reports: AssertionError: GetShardIterator foreign-shard message: 'StartingSequenceNumber 491795207780499000000000000000000 is not a sequence number of shard shardId-000000000001' where AWS would have said 'Invalid StartingSequenceNumber. It encodes shardId-000000000000, while it was used in a call to a shard with shardId-000000000001'.

Why: the error message is part of the contract of an error class. GetShardIterator's InvalidArgumentException is a catch-all class — the same code covers an unsupported iterator type, an inconsistent type/member combination, a bad timestamp and this cross-shard checkpoint mistake — so the only way a client, a KCL-style consumer or a test suite can tell WHICH argument rule it tripped is the message, and AWS's documentation for the error says exactly that ("For more information, see the returned message"). Code that branches on, logs or asserts AWS's "Invalid StartingSequenceNumber. It encodes ... while it was used in a call to a shard with ..." wording succeeds against AWS and fails against the emulator; and an operator debugging a mis-shared checkpoint loses the diagnosis AWS hands them, namely which shard the checkpoint really belongs to.

Fix: the sequence-number branch of the shard-position resolver the GetShardIterator handler calls, and the sentence it returns. services/service-com-amazonaws-kinesis-kinesis-20131202/src/ops/get_shard_iterator.rs::requested_position maps that resolver's Err string straight into the modeled InvalidArgumentException via invalid_argument(); the sentence itself is formatted in services/service-com-amazonaws-kinesis-kinesis-20131202/src/state/shard_reading.rs::requested_shard_position, in the `if !inside { return Err(format!("{} {sequence_number} is not a sequence number of shard {}", ...)) }` branch (and its unit test a_sequence_number_names_a_position_only_in_its_own_shard asserts that same emulator-authored frame, so the test's expectation must move with it). That branch currently collapses TWO distinct conditions into one sentence — a value belonging to a DIFFERENT shard (what AWS words with the frame above, naming the encoded shard) and a value inside no shard's published range — so the resolver needs to distinguish them and word the foreign-shard case with AWS's frame, filled from the shard id decoded out of the supplied sequence number and the ShardId the request addressed.

Related but NOT covering this: open issue 20261027T011654Z-kinesis-9-net-new-root-causes-cloudformation-aws.md carries this same check id in its Issue-Trace-Ids, but its Cause 3 is the over-permissive defect — GetShardIterator ACCEPTING a foreign shard's sequence number and handing out an iterator — and its replay key for this check is the acceptance assertion (`...|run#15|AssertionError|raise AssertionError(`). That acceptance no longer happens: the call is now refused with the right code, and the check fails later, on the message assertion. The remaining divergence is the refusal's wording, which that issue neither states nor requires, so it is filed here. Open issue 20261115T054254Z-kinesis-getsharditerator-missing-shard-resourcenotfoundexception-uses-the-wrong.md is also adjacent but concerns GetShardIterator's missing-shard ResourceNotFoundException sentence, a different error class and a different code path.

Traces: kinesis.at-sequence-number-from-foreign-shard-rejected.

## Evidence
Cause 1 — wrong InvalidArgumentException message frame for a cross-shard StartingSequenceNumber (1 failing check).
  Check: kinesis.at-sequence-number-from-foreign-shard-rejected, verdict mismatch, service heading `### kinesis`.
  Failure reported by the replay:
    File "check_kinesis_at_sequence_number_from_foreign_shard_rejected.py", line 216, in run
      assert message == (...)
    AssertionError: GetShardIterator foreign-shard message: 'StartingSequenceNumber 491795207780499000000000000000000 is not a sequence number of shard shardId-000000000001'

  Status and error code AGREE with AWS: the check's preceding assertion (`code == "InvalidArgumentException"`) passed, i.e. the emulator answered HTTP 400 with code InvalidArgumentException, and the control arm (the same sequence number on its own shard, drained via GetRecords, returning b"owned-by-a") passed as well. The only divergence is $.message.

  Requirement: HTTP 400, error code 'InvalidArgumentException', message frame
    "Invalid StartingSequenceNumber. It encodes <shard id encoded in the supplied sequence number>, while it was used in a call to a shard with <ShardId from the request>"
  Fixed text: "Invalid StartingSequenceNumber. It encodes " and ", while it was used in a call to a shard with "; no terminating period. Variable parts: (a) the shard id the supplied StartingSequenceNumber decodes to, from the emulator's own shard map/sequence encoding; (b) the ShardId copied verbatim from the request. The sequence-number value does NOT appear.
  The values 'shardId-000000000000', 'shardId-000000000001' and '491795207780499000000000000000000' seen in this run are per-run values filling that template, not requirements.

  Actual: same status and code, but the sentence "<member> <sequence number> is not a sequence number of shard <ShardId>" — a different frame that names the request member and the raw value instead of the encoding shard, and that omits the shard the value actually belongs to.

  Source of the diverging sentence in the checkout: services/service-com-amazonaws-kinesis-kinesis-20131202/src/state/shard_reading.rs, requested_shard_position, the `if !inside` branch (format!("{} {sequence_number} is not a sequence number of shard {}", members.sequence_number, shard.shard_id)), lifted into the modeled error by services/service-com-amazonaws-kinesis-kinesis-20131202/src/ops/get_shard_iterator.rs::requested_position -> invalid_argument.

  Post-refusal invariants the check goes on to assert (shard set unchanged, both shards still open, TRIM_HORIZON on the foreign shard still works) were never reached, so they are ungraded until the message is corrected.

## Steps to Reproduce
1. kinesis.create_stream(StreamName=s, ShardCount=2, StreamModeDetails={'StreamMode': 'PROVISIONED'}); poll describe_stream_summary until StreamStatus == ACTIVE and list_shards(StreamName=s) returns both shards.
2. put_record(StreamName=s, Data=b'owned-by-a', PartitionKey='pk-a'); note the response's ShardId (call it A) and SequenceNumber (S). The other shard of the map is B.
3. Control: get_shard_iterator(StreamName=s, ShardId=A, ShardIteratorType='AT_SEQUENCE_NUMBER', StartingSequenceNumber=S) and get_records on it — the record is returned (this passes today, proving S is well-formed).
4. get_shard_iterator(StreamName=s, ShardId=B, ShardIteratorType='AT_SEQUENCE_NUMBER', StartingSequenceNumber=S).
5. Observe HTTP 400 with code InvalidArgumentException (correct) but message 'StartingSequenceNumber <S> is not a sequence number of shard B' instead of AWS's 'Invalid StartingSequenceNumber. It encodes A, while it was used in a call to a shard with B'.
6. Re-replay behavioral check kinesis.at-sequence-number-from-foreign-shard-rejected to confirm; after the message is corrected the check also grades the post-refusal invariants (shard set unchanged, both shards still open, TRIM_HORIZON on B still issues an iterator).
