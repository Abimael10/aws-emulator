Issue-Source: trace-sweep-divergence
Issue-Trace: mediaconvert-account-metadata
Issue-Step: task#3
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: mediaconvert-account-metadata
Issue-Shape-Id: com.amazonaws.mediaconvert#MediaConvert
Issue-Key: mediaconvert|Probe|4xx->4xx|$.message
Issue-Commit: d68524ddfe39deacc36a1df634022e0b97d0c530

# MediaConvert Probe returns the wrong NotFoundException message for a missing media file

> _Note: this issue was identified against the code base at commit `d68524ddfe39deacc36a1df634022e0b97d0c530`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Probe

## Description
One cause.

Cause 1 of 1: Probe on an input file URL that does not exist answers 404 NotFoundException with a generic 'resource you specified in <member> was not found' message instead of the file-specific message AWS uses for a missing media file.

AWS: Smithy (services/service-com-amazonaws-mediaconvert-mediaconvert) models Probe as POST /2017-08-29/probe with input ProbeRequest.InputFiles -> list of ProbeInputFile{FileUrl (jsonName 'fileUrl'): 'Specify the S3, HTTP, or HTTPS URL for your media file.'} and declares NotFoundException (errorKind client, httpError 404, single member Message/jsonName 'message', doc 'The resource you requested doesn't exist.'). The model does not fix the message text, so the requirement is recorded behavior only: for a Probe whose fileUrl names an object that does not exist, AWS answers HTTP 404, errorType/x-amzn-errortype 'NotFoundException' and the message 'The file does not exist.' That message is entirely FIXED text for this error class — it interpolates nothing from the request or from service state (no URL, bucket, key, member name or id appears in it). AWS reserves the distinct 'The resource you specified in <member> was not found.' template — whose only variable part is the input member name — for a missing MediaConvert RESOURCE referenced by an input member (e.g. the ARN form seen on AssociateCertificate), not for an unreadable/absent media file.

Emulator: status 404 and errorType NotFoundException are correct, but the message body is 'The resource you specified in fileUrl was not found.' — the resource-not-found template, filled with the input member name 'fileUrl', rather than the fixed file-not-found text. Replay detail: `$.message: value_mismatch (expected 'The file does not exist.', got 'The resource you specified in fileUrl was not found.')`. Every other asserted field of the response matched.

Why: a client that keys off the error message cannot tell a missing media OBJECT from a missing MediaConvert resource, because the emulator collapses both onto one template. Probe is the operation callers use to pre-flight a file before CreateJob, and tooling/retry logic that branches on 'The file does not exist.' (surface a bad input URL to the user, skip the retry) silently takes the wrong branch or logs a misleading cause against the emulator while working against AWS. Message assertions in SDK-level tests and in user-facing error surfacing therefore fail only on the emulator.

Fix: the Probe handler's input-file resolution path in the mediaconvert service crate — where a non-existent fileUrl is turned into NotFoundException. It is reusing the shared 'resource you specified in <member> was not found' error constructor used for resource lookups; Probe's missing-file branch must emit the fixed message 'The file does not exist.' with HTTP 404 and errorType NotFoundException (headers and the remaining error envelope members are already correct and must not change).

Traces: mediaconvert-account-metadata#task#3 (Probe, expected 404 / actual 404, message only).

## Evidence
Fixture mediaconvert-account-metadata, step task#3, operation Probe, verdict mismatch.
expected_status 200-class: expected 404, actual 404 (status and error code agree).
detail: $.message: value_mismatch (expected 'The file does not exist.', got 'The resource you specified in fileUrl was not found.')
Expected (AWS, recorded): HTTP 404, x-amzn-errortype: NotFoundException, body message = 'The file does not exist.' (fixed text, no interpolated values).
Actual (emulator): HTTP 404, NotFoundException, body message = 'The resource you specified in fileUrl was not found.' (the resource-not-found template with the member name 'fileUrl' substituted).
Smithy citation: Probe -> POST /2017-08-29/probe, errors include NotFoundException (client, httpError 404, member Message jsonName 'message'); ProbeInputFile.FileUrl jsonName 'fileUrl'.
Related open issues that do NOT cover this: 20261029T210141Z-mediaconvert-three-operations-answer-success-where-aws-refuses.md (ListVersions/AssociateCertificate/CreateResourceShare answering success instead of refusing) and 20261110T032405Z-mediaconvert-account-endpoint-and-job-completion-results-diverge.md (DescribeEndpoints host, GetJob completion) — neither mentions Probe or its not-found message.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture mediaconvert-account-metadata.
2. At step task#3, call MediaConvert Probe (POST /2017-08-29/probe) with inputFiles = [{"fileUrl": "s3://<bucket>/<key-that-does-not-exist>"}] for a key that is absent from the bucket.
3. Observe the response: HTTP 404 with errorType NotFoundException, as expected.
4. Read the body's `message` member: the emulator returns 'The resource you specified in fileUrl was not found.' where AWS returns the fixed text 'The file does not exist.'
5. Re-replay mediaconvert-account-metadata#task#3 after the fix; the step must pass with the message compared byte-for-byte.
