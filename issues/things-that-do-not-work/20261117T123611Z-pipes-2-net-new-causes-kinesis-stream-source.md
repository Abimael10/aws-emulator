Issue-Source: trace-sweep-divergence
Issue-Trace: pipes-kinesis-source-parallelization-factor-bounds
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: pipes-kinesis-source-parallelization-factor-bounds
Issue-Shape-Id: com.amazonaws.pipes#Pipes
Issue-Key: pipes|DescribePipe|2xx->2xx|$.SourceParameters.KinesisStreamParameters.BatchSize
Issue-Key: pipes|DescribePipe|2xx->2xx|$.StateReason
Issue-Key: pipes|CreatePipe|4xx->4xx|$.message
Issue-Commit: 4a5d043b6a599ebc980e6bc8209185e125d568f2

# pipes: 2 net-new causes — Kinesis stream source parameters are dropped from the pipe record and the range-violation refusal is misworded

> _Note: this issue was identified against the code base at commit `4a5d043b6a599ebc980e6bc8209185e125d568f2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribePipe

## Description
Two distinct root causes in the EventBridge Pipes Kinesis-stream source surface, both reached by creating a pipe whose SourceParameters.KinesisStreamParameters carry a BatchSize and a ParallelizationFactor and then reading the pipe back / probing the factor's lower bound.

Cause 1 of 2: DescribePipe does not echo the configured KinesisStreamParameters.BatchSize, and the StateReason it reports for a pipe that is running and polling is the lifecycle-transition reason rather than the poller's reason.
AWS: DescribePipe returns the pipe record including SourceParameters, and SourceParameters.KinesisStreamParameters.BatchSize is the value the pipe was configured with — Smithy: pipes#DescribePipeResponse.SourceParameters -> PipeSourceParameters, pipes#PipeSourceKinesisStreamParameters.BatchSize targets LimitMax10000 @range(min 1, max 10000) ("The maximum number of records to include in each batch"), and the member carries no @default, so an echoed value is the configured one and never absent once supplied. DescribePipeResponse.StateReason (pipes#PipeStateReason, @length max 512) is documented as "The reason the pipe is in its current state"; the specific reason strings are service behaviour — recorded behavior only, and the recording shows a Kinesis-sourced pipe that has reached its running state with nothing on the stream reporting the poller's reason, the fixed text 'No records processed' (no variable part).
Emulator: for the same pipe, DescribePipe answers 200 but SourceParameters.KinesisStreamParameters.BatchSize is absent from the serialized record (expected the configured value, e.g. 10, got None), and StateReason is 'USER_INITIATED' — the reason for the user-requested state change — instead of the poller's reason.
Why: a client that reads its pipe back cannot see the batch size it configured, so any read-modify-write round trip (DescribePipe -> UpdatePipe) silently drops BatchSize and the pipe's batching behaviour changes without the client asking; and a client that polls StateReason to tell "running, idle, nothing on the stream" from "running because a user started it" gets the wrong answer for the whole life of the pipe.
Fix: the pipe-record projection used by DescribePipe (the source-parameters serializer for the Kinesis variant) must emit every configured KinesisStreamParameters member, and the state machine that owns StateReason must set the poller's reason once the pipe is running rather than leaving the transition reason in place.
Traces: pipes-kinesis-source-parallelization-factor-bounds#task#7 (also #task#9).

Cause 2 of 2: a ParallelizationFactor below its modeled minimum is refused with the generic Coral constraint summary instead of AWS's schema-validation message.
AWS: CreatePipe refuses a SourceParameters.KinesisStreamParameters.ParallelizationFactor outside its range with HTTP 400 and the modeled error ValidationException — Smithy: pipes#PipeSourceKinesisStreamParameters.ParallelizationFactor targets LimitMax10 @range(min 1, max 10); CreatePipe declares ValidationException (httpError 400). The message AWS emits for a numeric range violation in the request body is the request-schema validator's bracketed finding list: the fixed text is '[numeric instance is lower than the required minimum (minimum: <modeled minimum>, found: <submitted value>)]', whose only variable parts are the modeled minimum for that member and the value taken from the request (here minimum 1, found 0) — recorded behavior for the message shape, model for the bound.
Emulator: status 400 with the right error code, but the message is "1 validation error detected: Value '<value>' at 'sourceParameters.kinesisStreamParameters.parallelizationFactor' failed to satisfy constraint: Member must have value greater than or equal to 1" — the generic member-constraint summary, with none of the bracketed schema-validator framing. This is the same Coral-style wording already reported for the InputTemplate length refusal in open issue 20261109T163411Z-iam-pipes-sqs-4-net-new-causes-the.md (cause 1), but that issue is scoped to InputTemplate and does not cover numeric range refusals in SourceParameters, so this cause is net-new.
Why: a client that matches on the refusal text, surfaces it to a user, or asserts message equality against AWS sees a different sentence for every out-of-range numeric source parameter; the offending value and the bound are reported in a different vocabulary than AWS's.
Fix: the CreatePipe request validation path for SourceParameters numeric ranges must emit the schema-validator message form, not the generic constraint summary.
Traces: pipes-kinesis-source-parallelization-factor-bounds#task#10 (also #task#12).

## Evidence
Cause 1 (fixture pipes-kinesis-source-parallelization-factor-bounds, step task#7, DescribePipe, 200 -> 200):
  Expected $.SourceParameters.KinesisStreamParameters.BatchSize = 10 (the configured value); Actual: member missing (None).
  Expected $.StateReason = 'No records processed'; Actual $.StateReason = 'USER_INITIATED'.
  Same divergence at task#9 (2 steps in cluster).

Cause 2 (fixture pipes-kinesis-source-parallelization-factor-bounds, step task#10, CreatePipe, 400 -> 400):
  Expected $.message = '[numeric instance is lower than the required minimum (minimum: 1, found: 0)]'
  Actual   $.message = "1 validation error detected: Value '0' at 'sourceParameters.kinesisStreamParameters.parallelizationFactor' failed to satisfy constraint: Member must have value greater than or equal to 1"
  Fixed text: '[numeric instance is lower than the required minimum (minimum: , found: )]'; variable parts: the modeled minimum (1, from pipes#LimitMax10 @range) and the value submitted in the request (0).
  Same divergence at task#12 (2 steps in cluster).

## Steps to Reproduce
Cause 1: CreatePipe with Source = a Kinesis stream ARN and SourceParameters.KinesisStreamParameters = {StartingPosition, BatchSize: 10, ParallelizationFactor: <in range>}; start the pipe and let it reach its running state with nothing on the stream; call DescribePipe and inspect SourceParameters.KinesisStreamParameters.BatchSize and StateReason. Replay fixture pipes-kinesis-source-parallelization-factor-bounds, step task#7.

Cause 2: CreatePipe with the same Kinesis source but SourceParameters.KinesisStreamParameters.ParallelizationFactor = 0 (below the modeled minimum of 1); compare the HTTP 400 ValidationException message. Replay fixture pipes-kinesis-source-parallelization-factor-bounds, step task#10.
