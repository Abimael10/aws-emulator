Issue-Source: trace-sweep-divergence
Issue-Trace: groundstation-config-other-capability-types
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: groundstation-config-other-capability-types
Issue-Shape-Id: com.amazonaws.groundstation#GroundStation
Issue-Key: groundstation|CreateConfig|4xx->4xx|$.message
Issue-Commit: a2eeef02644035fcd268308a7ea8a9cbaaf0f92f

# CreateConfig answers an unresolvable s3RecordingConfig bucket with the bucket-does-not-exist message instead of AWS's GetBucketLocation wording

> _Note: this issue was identified against the code base at commit `a2eeef02644035fcd268308a7ea8a9cbaaf0f92f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateConfig

## Description
One root cause.

Cause 1 of 1: CreateConfig refuses an `s3RecordingConfig` whose bucket it cannot resolve with the WRONG one of the two refusal texts. The recorded fixture step groundstation-config-other-capability-types#task#6 drives CreateConfig with an `s3RecordingConfig` whose `bucketArn` cannot be resolved; AWS answers HTTP 431 / `InvalidParameterException` with the `s3:GetBucketLocation` wording, and the emulator answers the same status and error code with the "bucket does not exist." wording.

AWS: `CreateConfig` resolves an `s3RecordingConfig`'s recording bucket before it stores the config. Smithy (services/service-com-amazonaws-groundstation-groundstation/smithy-model): CreateConfig input `configData` (ConfigTypeData, required) / `name` (SafeName, @length 1..256, pattern ^[ a-zA-Z0-9_:-]{1,256}$), modeled errors DependencyException (531), InvalidParameterException (431, "One or more parameters are not valid", members `message` + `parameterName`), ResourceLimitExceededException (429), ResourceNotFoundException (434). The model carries no message text, so the refusal WORDING is recorded behavior only. For the bucket this step names, the recording shows HTTP 431, code `InvalidParameterException`, and message:
  "Unable to create new S3 endpoint config - unable to get location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on the bucket."
That message is fixed text in full: every word of it is constant and it interpolates no value from the request or from state — no bucket name, ARN, account, region or prefix. (Hypothesis, not stated by the model or the recording: AWS reaches for the bucket's location in Ground Station's own name, so a bucket it is not allowed to read the location of — a bucket in another account, or one its role has no `s3:GetBucketLocation` on — lands in this condition rather than in a no-such-bucket one, which is why the recorded wording for this step names the permission.)

Emulator: same status (431) and same error code (`InvalidParameterException`) — those are not at issue — but the message is
  "Unable to create new S3 endpoint config - bucket does not exist."
The replay reports only `$.message: value_mismatch` for this step. The emulator decides WHICH of its two refusal texts to use from what its own in-process S3 answers a `HeadBucket` with (state.rs `unresolvable_recording_bucket`): a `bucketArn` that its ARN parser does not read as an S3 bucket ARN, and a `HeadBucket` that comes back `NotFound`, both select `RECORDING_BUCKET_ABSENT` ("bucket does not exist."), while only a `HeadBucket` refused with 403 selects `RECORDING_BUCKET_UNRESOLVABLE` (the `s3:GetBucketLocation` wording). For the bucket in this recorded request the emulator's S3 holds no such bucket, so the absent branch fires and the caller is told the bucket is missing where AWS tells it the location read failed. Note the open behavioral issue 20261117T061313Z-groundstation-createconfig-refuses-a-nonexistent-s3-recording-bucket-with: it introduced the split into these two texts; it does NOT cover this step (this divergence carries no `owned_by`), and the branch selection it specified sends this recorded request to the wrong text.

Why: a client that branches on the refusal message — or a human reading it — is sent to the wrong remedy. AWS's message says Ground Station could not read the bucket's location and names the permission to grant; the emulator's says the bucket is not there, so the caller creates a bucket or edits the ARN that was never the problem. Any code that asserts on, logs, or routes retries by this message behaves differently against the emulator than against AWS, and the two conditions stop being distinguishable in the direction AWS distinguishes them.

Fix: the `s3RecordingConfig` bucket-resolution refusal in this service's state helper (the branch selection inside `unresolvable_recording_bucket`, between the `RECORDING_BUCKET_ABSENT` and `RECORDING_BUCKET_UNRESOLVABLE` texts), reached from the CreateConfig handler's validation step before anything is stored; the same helper backs the update path, so correcting the selection there fixes both callers. The condition this fixture records must answer the `s3:GetBucketLocation` wording verbatim, with the "bucket does not exist." wording reserved for whatever condition the corpus actually records it for.

Traces: groundstation-config-other-capability-types#task#6 (CreateConfig, verdict mismatch, expected 431 / actual 431, diverging key `$.message`).

## Evidence
Fixture: groundstation-config-other-capability-types, step task#6, operation CreateConfig, verdict mismatch.

Expected (recorded AWS):
  HTTP 431
  Code:    InvalidParameterException
  Message: "Unable to create new S3 endpoint config - unable to get location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on the bucket."

Actual (emulator):
  HTTP 431
  Code:    InvalidParameterException
  Message: "Unable to create new S3 endpoint config - bucket does not exist."

Replay detail, verbatim:
  $.message: value_mismatch (expected 'Unable to create new S3 endpoint config - unable to get location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on the bucket.', got 'Unable to create new S3 endpoint config - bucket does not exist.')
  divergence_key: service=groundstation|CreateConfig|status=431->431|keys=$.message

Status and error code agree; the message text is the whole divergence. Both texts are fixed in full — neither interpolates a request or state value — so the requirement is the expected sentence word for word, not a per-run value.

Code sites behind it (citations only): services/service-com-amazonaws-groundstation-groundstation/src/state.rs lines 486-500 (the two refusal constants RECORDING_BUCKET_UNRESOLVABLE / RECORDING_BUCKET_ABSENT) and lines 534-579 (`unresolvable_recording_bucket`, which picks between them from the HeadBucket answer: `bucket_name_of` returning None and a NotFound both pick ABSENT, only a 403 picks UNRESOLVABLE); services/service-com-amazonaws-groundstation-groundstation/src/ops/create_config.rs lines 176-184 (the handler's call site, before the config is stored).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Fixture groundstation-config-other-capability-types, step task#6, calls groundstation CreateConfig with configData={"s3RecordingConfig": {...}} whose bucketArn the service cannot resolve to a readable bucket.
3. Observe the response: HTTP 431 with Code InvalidParameterException (correct) but Message "Unable to create new S3 endpoint config - bucket does not exist."
4. The recording for that step has the same status and code with Message "Unable to create new S3 endpoint config - unable to get location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on the bucket."
5. Equivalently, by hand: call CreateConfig with an s3RecordingConfig whose bucketArn names a bucket the emulator's S3 does not hold (or a bucketArn that is not an S3 bucket ARN at all) and compare the message against the recorded one.
