Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: groundstation.config-s3-recording-nonexistent-bucket-rejected
Issue-Shape-Id: com.amazonaws.groundstation#GroundStation
Issue-Key: groundstation.config-s3-recording-nonexistent-bucket-rejected|run#28|AssertionError|assert refused == (
Issue-Commit: 19f0e5a096ed637da91415d21b3a547e7492acec

# CreateConfig refuses a nonexistent S3 recording bucket with the s3:GetBucketLocation-permission message instead of "bucket does not exist."

> _Note: this issue was identified against the code base at commit `19f0e5a096ed637da91415d21b3a547e7492acec`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateConfig

## Description
One root cause.

Cause 1 of 1: an `s3RecordingConfig` whose `bucketArn` names a bucket that DOES NOT EXIST is refused with the wrong error message. Failing check: groundstation.config-s3-recording-nonexistent-bucket-rejected.

AWS: Ground Station resolves an `s3RecordingConfig`'s bucket before it stores the config, and it distinguishes the *reasons* the resolution failed in the message it returns. For a bucket that is not there, `CreateConfig` is refused with HTTP 431 / `InvalidParameterException` and the message `Unable to create new S3 endpoint config - bucket does not exist.` — entirely fixed text, interpolating no bucket name, ARN, account or region. The `s3:GetBucketLocation` remedy text (`Unable to create new S3 endpoint config - unable to get location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on the bucket.`) belongs to the different condition where the bucket exists but S3 would not hand Ground Station its location, and naming a permission the caller has not actually failed on is misleading. Smithy: `CreateConfig` models `InvalidParameterException` (HTTP 431) — "One or more parameters are not valid" — with members `message` and `parameterName`; the model carries no message text, so the two message templates above are recorded behavior only, and the recording this check replays shows the "bucket does not exist." wording for the missing-bucket case. The status (431) and the error code (`InvalidParameterException`) match and are not at issue; the message is.

Emulator: the emulator answers HTTP 431 / `InvalidParameterException` but with the message `Unable to create new S3 endpoint config - unable to get location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on the bucket.` It deliberately collapses every bucket-resolution failure into ONE fixed refusal string, so a bucket S3 reports as absent, a bucket S3 refuses the location read on, and a `bucketArn` that is not an S3 bucket ARN at all are all reported with the permission wording. The control check in the same replay — `CreateConfig` against a real, permissioned bucket — succeeds and returns `configType` `s3-recording`, so only the refusal branch's text diverges.

Why: a client that branches on the refusal message (and a human reading it) is told the wrong thing. The AWS message says the bucket is missing, and the remedy is to create it or fix the ARN; the emulator's message says Ground Station lacks `s3:GetBucketLocation`, sending the caller to edit an IAM role policy that is already correct. Anything that asserts on, logs, or routes retries by this message — including the two conditions being distinguishable from each other — behaves differently against the emulator than against AWS.

Fix: the `s3RecordingConfig` bucket-resolution refusal in this service's state helper (`unresolvable_recording_bucket` and the single `RECORDING_BUCKET_UNRESOLVABLE` constant it returns), reached from the `CreateConfig` handler's validation step. The resolution already has the information it needs — it distinguishes "S3 answered about the bucket" from "S3 never answered" — so it needs a second refusal text selected by WHICH S3 answer came back: a no-such-bucket answer (and a `bucketArn` that names no bucket at all, which is the same thing settled without asking) must carry the "bucket does not exist." wording, while the location-withheld/access-denied answer keeps the existing `s3:GetBucketLocation` wording. The same helper backs the update path, so correcting it there fixes both callers.

Traces: groundstation.config-s3-recording-nonexistent-bucket-rejected — creates a real bucket plus a Ground Station trust role, confirms `CreateConfig` accepts the good `s3RecordingConfig`, then calls `CreateConfig` with a `bucketArn` for a bucket that was never created and asserts the (code, message) pair; it also asserts the rejected config never appears in `ListConfigs` (that part is satisfied — nothing was stored).

## Evidence
Check: groundstation.config-s3-recording-nonexistent-bucket-rejected [mismatch]

AssertionError raised at check_groundstation_config_s3_recording_nonexistent_bucket_rejected.py:143:
  unexpected outcome for nonexistent S3 bucket: ('InvalidParameterException', 'Unable to create new S3 endpoint config - unable to get location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on the bucket.')

Expected (AWS), on CreateConfig with configData.s3RecordingConfig.bucketArn naming a bucket that does not exist:
  HTTP 431
  Code:    InvalidParameterException
  Message: "Unable to create new S3 endpoint config - bucket does not exist."
           (fixed text in full; no request or state values are interpolated)

Actual (emulator), same call:
  HTTP 431
  Code:    InvalidParameterException
  Message: "Unable to create new S3 endpoint config - unable to get location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on the bucket."
           (fixed text in full; this is AWS's wording for a DIFFERENT condition — bucket present, location read refused)

Status and error code agree; the message text is the divergence. The emulator returns one unconditional refusal string for every bucket-resolution failure, so the missing-bucket case is indistinguishable from the denied-permission case.

Control in the same check passes: CreateConfig on the real, permissioned bucket returns 200 with configType == "s3-recording", and the refused config does not appear in ListConfigs.

## Steps to Reproduce
1. Create an S3 bucket in the emulator's region (name prefixed `aws-groundstation-`), and tag it.
2. Create an IAM role trusted by `groundstation.amazonaws.com` with an inline policy granting `s3:GetBucketLocation`, `s3:PutObject` and `s3:ListBucket` on that bucket and its objects.
3. Call groundstation CreateConfig with name=<any SafeName> and configData={"s3RecordingConfig": {"bucketArn": "arn:aws:s3:::<that-bucket>", "roleArn": <role arn>, "prefix": "sim2real/"}}. It succeeds and returns configType "s3-recording" (control).
4. Call groundstation CreateConfig again with a different name and the SAME roleArn, but bucketArn="arn:aws:s3:::<a-bucket-name-that-was-never-created>".
5. Observe the ClientError: Code is InvalidParameterException (correct) but Message is "Unable to create new S3 endpoint config - unable to get location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on the bucket."
   AWS returns "Unable to create new S3 endpoint config - bucket does not exist." for this condition.
6. ListConfigs shows no config under the rejected name (this part already matches AWS).
