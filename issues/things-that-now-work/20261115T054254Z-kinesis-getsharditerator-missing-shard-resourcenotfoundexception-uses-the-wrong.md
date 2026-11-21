Issue-Source: trace-sweep-divergence
Issue-Trace: kinesis-get-shard-iterator-types-and-errors
Issue-Step: task#14
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: kinesis-get-shard-iterator-types-and-errors
Issue-Shape-Id: com.amazonaws.kinesis#Kinesis_20131202
Issue-Key: kinesis|GetShardIterator|4xx->4xx|$.message
Issue-Commit: 8c07d03ee8e6ef72a66ac6585ff6b90e8cfabb45

# kinesis GetShardIterator missing-shard ResourceNotFoundException uses the wrong message frame

> _Note: this issue was identified against the code base at commit `8c07d03ee8e6ef72a66ac6585ff6b90e8cfabb45`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetShardIterator

## Description
One net-new root cause in the kinesis data-plane read path, from the wire corpus replay. The cluster is marked neither already-filed (no stub_path) nor owned by an open issue.

Cause 1 of 1: the ResourceNotFoundException GetShardIterator raises for a ShardId that is not in the addressed stream's shard map carries the sentence this service words SubscribeToShard's equivalent refusal with, not the sentence AWS words GetShardIterator's with.

AWS: Smithy: GetShardIterator input { ShardId required (len 1..128, pattern ^[a-zA-Z0-9_.-]+$), ShardIteratorType required, StreamName/StreamARN }; modeled error ResourceNotFoundException, errorKind client, HTTP 400, documented "The requested resource could not be found. The stream might not be specified correctly." The model does not state the message text, so for the text this is recorded behavior only: the recording shows HTTP 400 with error code ResourceNotFoundException and message "Could not find shard <ShardId> in stream <StreamName> under account <accountId>." The FIXED text of that frame is "Could not find shard " / " in stream " / " under account " / "." — including the terminating period. Its VARIABLE parts are the ShardId taken verbatim from the request, the name of the stream the request resolved (by StreamName, or the resource segment of StreamARN), and the 12-digit account id of the caller, from the emulator's own state. The shard id, stream name and account digits in any one recording only fill that template and are not part of it.

Emulator: the HTTP status (400) and the error code (ResourceNotFoundException) are correct, but the message is a different sentence: "Shard <ShardId> in stream <StreamName> under account <accountId> does not exist" — a different leading clause, trailing clause "does not exist", and no terminating period. Replay of fixture kinesis-get-shard-iterator-types-and-errors step task#14 reports exactly one diverging field, $.message: expected 'Could not find shard shardId-999999999999 in stream sim2real-probe-kinesis-dp-89d04269 under account 000000000000.', got 'Shard shardId-999999999999 in stream sim2real-probe-kinesis-dp-89d04269 under account 000000000000 does not exist'. Every other step of that fixture — the valid TRIM_HORIZON/LATEST/AT_SEQUENCE_NUMBER/AFTER_SEQUENCE_NUMBER/AT_TIMESTAMP iterator types and the InvalidArgumentException refusals — passed, so only the sentence frame of this one refusal class diverges.

Why: the error message is part of the contract for an error class. A client, SDK retry/branching helper or test suite that matches the refusal sentence to distinguish "this shard id does not exist" (and surfaces it as the operator-facing reason a read could not be positioned) reads a sentence AWS never emits for GetShardIterator: a match keyed on AWS's wording succeeds against AWS and fails against the emulator, and diagnostics differ between the two.

Fix: the missing-shard refusal branch of the GetShardIterator handler and the message helper it calls. services/service-com-amazonaws-kinesis-kinesis-20131202/src/ops/get_shard_iterator.rs builds this refusal via crate::state::shard_not_found_message, and services/service-com-amazonaws-kinesis-kinesis-20131202/src/state.rs formats that helper with the "Shard {shard_id} in stream {stream} under account {account} does not exist" frame. That helper is documented as ONE sentence for the whole class, but AWS does not word the class identically across operations: SubscribeToShard's missing-shard refusal is the "Shard ... does not exist" sentence (see open issue 20261114T101112Z-kinesis-subscribetoshard-s-resourcenotfoundexception-for-a-shardid-that.md, which asserts exactly that frame for SubscribeToShard and does NOT carry this fixture or this operation), while GetShardIterator's is the "Could not find shard ... ." sentence. GetShardIterator must therefore format its own ResourceNotFoundException with AWS's "Could not find shard {shard_id} in stream {stream_name} under account {account_id}." frame, filled from the request's ShardId, the resolved stream's name and the caller's account, rather than sharing a single cross-operation sentence.

Traces: kinesis-get-shard-iterator-types-and-errors#task#14.

## Evidence
Cause 1 — wrong ResourceNotFoundException message frame (1 cluster, 1 step):
  fixture kinesis-get-shard-iterator-types-and-errors, step task#14, operation GetShardIterator, verdict mismatch.
  Expected status 400, actual status 400 (status and error code agree).
  Diverging field reported by replay:
    $.message: value_mismatch (expected 'Could not find shard shardId-999999999999 in stream sim2real-probe-kinesis-dp-89d04269 under account 000000000000.', got 'Shard shardId-999999999999 in stream sim2real-probe-kinesis-dp-89d04269 under account 000000000000 does not exist')

  Requirement: HTTP 400, error code 'ResourceNotFoundException', message frame "Could not find shard <ShardId> in stream <stream name> under account <account id>." — fixed text "Could not find shard ", " in stream ", " under account " and the terminating "."; variable parts = ShardId from the request, the resolved stream's name, the caller's 12-digit account id. The shard id 'shardId-999999999999', the stream name 'sim2real-probe-kinesis-dp-89d04269' and the account '000000000000' above are per-run values filling that template, not requirements.
  Actual: the same status and code, but the frame "Shard <ShardId> in stream <stream> under account <account> does not exist" (no trailing period).

  Related but not covering this: 20261114T101112Z-kinesis-subscribetoshard-s-resourcenotfoundexception-for-a-shardid-that.md is about SubscribeToShard and requires the "Shard ... does not exist" frame there; it carries no GetShardIterator fixture/step. The four other kinesis clusters in this replay (fixture kinesis-cbor-error-encoding-and-serialization-exception, steps task#3..task#7) are owned by 20261029T174012Z-kinesis-cbor-requests-are-neither-decoded-nor-answered.md and are not filed here.

## Steps to Reproduce
1. CreateStream(StreamName=s, ShardCount=1) and wait until DescribeStreamSummary reports StreamStatus ACTIVE.
2. ListShards(StreamName=s) to confirm the real shard id (shardId-000000000000).
3. Call GetShardIterator(StreamName=s, ShardId='shardId-999999999999', ShardIteratorType='TRIM_HORIZON') — a well-formed shard id (matching ^[a-zA-Z0-9_.-]+$, length 1..128) that is not in the stream's shard map.
4. Observe HTTP 400 with __type/Code 'ResourceNotFoundException' (correct) but message 'Shard shardId-999999999999 in stream s under account <account> does not exist' instead of AWS's 'Could not find shard shardId-999999999999 in stream s under account <account>.'
5. Replay of fixture kinesis-get-shard-iterator-types-and-errors, step task#14, reports the same single $.message mismatch.
