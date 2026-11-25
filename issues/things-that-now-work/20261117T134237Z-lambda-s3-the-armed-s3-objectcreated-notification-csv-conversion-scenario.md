Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3.armed-notification-converts-csv-and-suppresses-uppercase-suffix
Issue-Key: lambda__s3.armed-notification-converts-csv-and-suppresses-uppercase-suffix|_watch_absent#26|Timeout|return found
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# The armed S3 ObjectCreated notification CSV-conversion scenario never completes within the 60 s budget

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
Cause 1 of 1: the end-to-end S3-notification-to-Lambda conversion scenario never reaches a verdict — the whole check is abandoned at the 60 s ceiling.

AWS: recorded behavior only for the end-to-end timing (the suite reports no response to compare). The shape AWS serves is modelled and documented: s3 PutBucketNotificationConfiguration with a LambdaFunctionConfigurations entry for `s3:ObjectCreated:*` and a suffix filter arms the bucket, and a PutObject matching that filter invokes the function promptly (seconds), while an object whose suffix differs only in case does NOT match — the suffix filter is case-sensitive, so an uppercase `.CSV` is suppressed while `.csv` fires.

Emulator: the check is abandoned at the 60 s ceiling. The scenario — arm the notification, put a matching CSV, observe the converted output, put an uppercase-suffixed twin and observe it suppressed — produces no verdict at all inside the budget, so either the notification never fires or some leg of the scenario blocks indefinitely.

Why: S3-event-driven Lambda is the most common serverless trigger there is. A client cannot observe whether an object arriving in a bucket invokes its function, nor whether the case-sensitive suffix filter suppresses the twin; the run hangs rather than answering, so there is nothing for a caller to time out on or retry against.

Fix: the S3 event-notification dispatch path — the ObjectCreated matcher (including case-sensitive suffix/prefix filtering) and the Lambda invocation it issues — and whichever leg of the arm/put/observe sequence is not converging.

Traces: lambda__s3.armed-notification-converts-csv-and-suppresses-uppercase-suffix

## Evidence
Expected: the `.csv` PutObject invokes the function within seconds and the converted object appears; the `.CSV` twin is suppressed by the case-sensitive suffix filter.
Actual, verbatim:
  Error: Check timed out after 60.0s
No assertion was ever reached — the check produced no status, body or error from the emulator inside the budget.

## Steps to Reproduce
1. CreateFunction with a handler that reads the notified object and writes a converted copy.
2. lambda AddPermission for principal s3.amazonaws.com with the bucket as SourceArn.
3. s3 PutBucketNotificationConfiguration with a LambdaFunctionConfigurations entry on `s3:ObjectCreated:*` filtered to suffix `.csv`.
4. PutObject `<key>.csv`, then PutObject `<key>.CSV`.
5. Observe: the check never reaches a verdict within 60 s. Expected the `.csv` put to invoke the function and produce the converted object, and the `.CSV` put to be suppressed by the case-sensitive suffix filter.

## Fix status

Cause 1 (the arm/put/observe sequence not converging) — NOT A DEFECT of the emulator's own
answers, and not a refusal: every call of the reported sequence is served and the Lambda data
plane RUNS. Driven end to end on the assembled router — `CreateBucket`, a CloudFormation
provisioned execution role plus `python3.12` converting function, `GetFunctionConfiguration`
polled to `Active`, lambda `AddPermission` for `s3.amazonaws.com` with the bucket ARN as
`SourceArn`, `PutBucketNotificationConfiguration` with one Lambda configuration on
`s3:ObjectCreated:*` filtered to suffix `.csv`, then `PutObject` of the `.csv` key and of the
`.CSV` twin — the bucket is armed (200), the `.csv` write invokes the function with no further
API call, and its converted object is readable 2.7 s after the write; the `.CSV` twin is answered
404 and the bucket holds exactly one derived object. The whole sequence, from assembling the
router to reading the twin's absence, completes in 6.6 s.

What the harness reports is its own wall-clock ceiling. This check's replay key moved from its
POSITIVE leg (`…|run#14|Timeout|time.sleep(5.0)`, filed and closed as
`20261103T031416Z-lambda-s3-5-net-new-causes-in-s3.md`) to its ABSENCE leg
(`…|_watch_absent#26|Timeout|return found`), so the positive control now arrives; the check's own
absence window is `_ABSENCE_WINDOW_S = 60.0` and "opens only AFTER the positive control has
arrived" (quoted from the check's docstring in the earlier closed filing
`20261110T153718Z-lambda-s3-an-armed-s3-objectcreated-notification-never.md`), so the check needs
more than the harness's 60 s ceiling of wall clock however fast the emulator answers.

The only gap this left was COVERAGE, not behaviour: every suite in the tree states its filter
rule value in lower case (`.csv`), so a byte-for-byte comparison and a matcher that folds one
side's case before comparing were indistinguishable. `core/router/tests/all/`
`s3_notification_suffix_rule_case_decides_delivery.rs` closes that — two armed buckets whose one
`suffix` rule is `.csv` and `.CSV`, each converting exactly the keys its own rule spells and no
key that differs from it only in case. The dispatch and the matcher it pins are
`crate::state::notifications::{key_matches, pending, publish, deliver}` in
`services/service-com-amazonaws-s3-amazons3/src/state/notifications.rs`.
