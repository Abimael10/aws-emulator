Issue-Source: trace-sweep-divergence
Issue-Trace: groundstation-config-other-capability-types
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: groundstation-config-other-capability-types
Issue-Shape-Id: com.amazonaws.groundstation#GroundStation
Issue-Key: groundstation|CreateConfig|4xx->4xx|$.message
Issue-Commit: 58859c14105019dd1602a64ae7de76ec6c79e390

# CreateConfig refuses an unresolvable s3RecordingConfig bucket with "bucket does not exist" instead of Ground Station's GetBucketLocation wording

> _Note: this issue was identified against the code base at commit `58859c14105019dd1602a64ae7de76ec6c79e390`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateConfig

## Description
One net-new root cause in groundstation.

Cause 1 of 1: CreateConfig with an `s3RecordingConfig` whose `bucketArn` Ground Station cannot resolve answers the right error class and the right status, but with the wrong refusal text: the emulator serves its "bucket does not exist" wording where Ground Station serves the wording that names the s3:GetBucketLocation permission.

AWS: Smithy (services/service-com-amazonaws-groundstation-groundstation/smithy-model/com-amazonaws-groundstation-groundstation.json) CreateConfig: input `name` (SafeName, @length 1..256, pattern ^[ a-zA-Z0-9_:-]{1,256}$) and `configData` (ConfigTypeData union, one member set — here `s3RecordingConfig`) are both @required; modeled errors are InvalidParameterException (HTTP 431), ResourceNotFoundException (HTTP 434), ResourceLimitExceededException (HTTP 429) and DependencyException (HTTP 531). The model says nothing about how the recording bucket is resolved or what text a failed resolution carries, so that part is recorded behavior only: the recorded CreateConfig for an s3-recording config whose bucket Ground Station could not resolve is refused with HTTP 431, code InvalidParameterException, and the message `Unable to create new S3 endpoint config - unable to get location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on the bucket.` That message is ENTIRELY fixed text — it interpolates no bucket name, ARN, account or region — so a fix reproduces it verbatim and has no value from the request or the store to fill in. Hypothesis: Ground Station reads the bucket's location with its own service role, so S3 answering absent and S3 refusing the read collapse into the single location-unreadable answer the caller sees, which is why the one wording names the permission as the remedy; the recording establishes only that this is the text that comes back for an unresolvable bucket.

Emulator: the same request is refused with HTTP 431 and InvalidParameterException — status and code are correct — but the message is `Unable to create new S3 endpoint config - bucket does not exist.` The handler resolves the bucket through S3 and then CHOOSES between two refusal texts by what S3 answered: a HeadBucket `NotFound` (and a `bucketArn` that parses as no S3 bucket at all, settled without asking S3) takes the "bucket does not exist." wording, while only a 403 on the HeadBucket takes the location/permission wording. On this step the resolution lands on the not-found branch, so the caller is told the bucket is absent instead of being told what Ground Station tells it.

Why: the message is part of the refusal contract — it is what a client surfaces, logs and matches on, and it is the only part of this answer that tells the operator what to DO. A client relying on AWS's behavior is told to grant Ground Station s3:GetBucketLocation on the bucket; against the emulator it is told the bucket is not there, so the operator chases a bucket name or re-creates a bucket that already exists and never looks at the policy. Any assertion, log-grep or user-facing mapping keyed on AWS's sentence (or on the substring `s3:GetBucketLocation`) never matches.

Fix: the recording-bucket refusal selection in this service's state helpers — `unresolvable_recording_bucket` and the `unresolved_bucket_condition` classifier it calls, which pick between the two message constants — not the CreateConfig handler's own code path (the handler only lifts the returned InvalidParameterException into its error enum) and not the status or error code, both of which are already right. An unresolvable recording bucket must carry Ground Station's location/permission wording; keep the two conditions distinguishable internally only if a recording shows AWS distinguishing them, which this corpus does not.

Traces: groundstation-config-other-capability-types#task#6.

## Evidence
Cause 1 — fixture groundstation-config-other-capability-types, step task#6 (operation CreateConfig, verdict mismatch):
  expected status: 431   actual status: 431   (status correct)
  expected error code: InvalidParameterException   actual: InvalidParameterException   (code correct)
  divergence detail, verbatim from the replay: `$.message: value_mismatch (expected 'Unable to create new S3 endpoint config - unable to get location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on the bucket.', got 'Unable to create new S3 endpoint config - bucket does not exist.')`
  divergence key: service=groundstation|CreateConfig|status=431->431|keys=$.message

Message anatomy: both the expected and the actual message are 100% fixed text. Nothing in either is taken from the request or from emulator state — no bucket name, no ARN, no account id, no region, no timestamp — so there is no per-run value in this divergence and the whole expected sentence is the requirement:
  `Unable to create new S3 endpoint config - unable to get location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on the bucket.`

Source of the emulator's text: services/service-com-amazonaws-groundstation-groundstation/src/state.rs:482-483 defines RECORDING_BUCKET_DOES_NOT_EXIST ("Unable to create new S3 endpoint config - bucket does not exist.") and lines 492-494 define RECORDING_BUCKET_LOCATION_UNREADABLE (the expected sentence). `unresolvable_recording_bucket` (lines 529-570) returns the DOES_NOT_EXIST text for a bucketArn that names no bucket (lines 547-549) and otherwise defers to `unresolved_bucket_condition` (lines 622-636), which returns DOES_NOT_EXIST on a HeadBucket NotFound and LOCATION_UNREADABLE only on a raw 403. services/service-com-amazonaws-groundstation-groundstation/src/ops/create_config.rs:180-184 lifts whatever that helper returns into InvalidParameterException, so the handler itself needs no change.

Not covered by any open issue: 20261105T083003Z-groundstation-tag-mutations-omit-content-length-on-their.md owns the two Content-Length divergences on Tag/UntagResource in this same replay (both skipped here); 20261030T170921Z-groundstation-deletedataflowendpointgroup-on-an-already-deleted-id-reuses-getdataflowendpointgroup.md is a ResourceNotFoundException wording defect on DeleteDataflowEndpointGroup; 20261029T004927Z (uuid minting) and 20261109T155230Z (CloudFormation resource types) are unrelated. None of them mentions the s3RecordingConfig bucket-resolution refusal text.

## Steps to Reproduce
1. Boot the emulator and replay fixture `groundstation-config-other-capability-types`; the divergence is at step `task#6`.
2. Equivalently, by hand: call CreateConfig with name=<any SafeName> and configData={"s3RecordingConfig": {"bucketArn": "arn:aws:s3:::<a bucket Ground Station cannot resolve the location of>", "roleArn": <a role trusted by groundstation.amazonaws.com>}}.
3. Read the HTTP status, Error.Code and Error.Message of the refusal.
   AWS: 431 / InvalidParameterException / `Unable to create new S3 endpoint config - unable to get location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on the bucket.`
   Emulator: 431 / InvalidParameterException / `Unable to create new S3 endpoint config - bucket does not exist.`
4. Repeat with a bucketArn that is syntactically not an S3 bucket ARN (e.g. `arn:aws:sqs:us-east-1:123456789012:q`): the emulator short-circuits to the same "bucket does not exist." wording without asking S3, so the divergence is not specific to one bucket's presence in the emulator's S3 state.
5. Confirm no config was created either way (ListConfigs shows nothing new) — only the message text diverges.
