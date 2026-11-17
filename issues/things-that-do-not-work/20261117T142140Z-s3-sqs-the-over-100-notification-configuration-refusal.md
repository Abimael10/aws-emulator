Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3__sqs.bucket-notification-configurations-over-hundred-rejected s3__sqs.notification-prefix-filter-url-decodes-before-key-matching
Issue-Key: s3__sqs.bucket-notification-configurations-over-hundred-rejected|run#16|AssertionError|assert got == _TOO_MANY_CONFIGS, f"expected {_TOO_MANY_CONFIGS!r} for 101 configurations, got {got!r}"
Issue-Key: s3__sqs.notification-prefix-filter-url-decodes-before-key-matching|run#15|AssertionError|assert found_hit, (
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# s3__sqs: the over-100 notification-configuration refusal uses the wrong code and message, and a URL-encoded prefix filter is matched literally instead of url-decoded (2 causes)

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
Two net-new root causes in the S3 bucket-notification path. A fixer must close both separately before submitting.

Cause 1 of 2: PutBucketNotificationConfiguration refuses a configuration set past the 100-configuration cap with the wrong error code and an emulator-authored message.
AWS: the per-bucket event-notification configuration count is capped at a fixed, non-adjustable 100. The S3 model carries no such constraint, so this is recorded behavior only: a set of 101 configurations is refused with Code 'InvalidRequest' and Message 'Too many configurations submitted. The maximum number of configurations is 100' — the whole sentence is fixed text for this class (the limit 100 is the fixed cap, not a value from the request), and nothing of the request is echoed.
Emulator: the call is refused, but with Code 'InvalidArgument' and Message 'A notification configuration may carry at most 100 configurations, got 101' — a different code and emulator-authored prose that echoes the submitted count.
Why: S3 clients branch on the error Code; 'InvalidArgument' and 'InvalidRequest' are distinct classes with distinct retry/report handling, so a caller that handles InvalidRequest for over-limit configuration sets does not recognise the refusal, and a caller asserting AWS's text sees a mismatch.
Fix: the PutBucketNotificationConfiguration handler's configuration-count validation — raise the InvalidRequest class with AWS's fixed sentence.
Traces: s3__sqs.bucket-notification-configurations-over-hundred-rejected.

Cause 2 of 2: a notification key prefix filter is matched against object keys literally instead of url-decoded first, so the matching and non-matching cases are exactly inverted.
AWS: a Filter.Key.FilterRules prefix value is stored and echoed back verbatim by GetBucketNotificationConfiguration, but is URL-DECODED before being matched against object keys. So a prefix configured as 'folder%3Dtag/' matches the key 'folder=tag/<name>' (the decoded prefix) and does NOT match 'folder%3Dtag/<name>' (a key whose literal text is the encoded form). The model does not state the decoding rule, so this is recorded behavior only; AWS documents prefix/suffix filtering over the (decoded) object key.
Emulator: the opposite — the key under the literal encoded text is delivered and the key under the decoded prefix is withheld. In the replay the only key that reached the queue was 'folder%3Dtag/literal-encoded-miss.txt', while 'folder=tag/decoded-hit.txt' produced no notification.
Why: every client whose keys contain characters an SDK or console percent-encodes when the filter is authored (=, +, space, non-ASCII) gets notifications for exactly the wrong objects: the objects it wants silently produce none, and objects whose keys literally contain '%xx' produce spurious ones. The failure is silent — the control-plane read-back is identical in both behaviours, so nothing warns the client.
Fix: the S3 notification filter matcher — url-decode the FilterRules prefix (and suffix) value before comparing it to the object key, while continuing to store and echo the configured value verbatim.
Traces: s3__sqs.notification-prefix-filter-url-decodes-before-key-matching.

## Evidence
Cause 1 — s3__sqs.bucket-notification-configurations-over-hundred-rejected (verdict: mismatch):
  Expected (AWS): ('InvalidRequest', 'Too many configurations submitted. The maximum number of configurations is 100')
  Actual (emulator): `AssertionError: expected ('InvalidRequest', 'Too many configurations submitted. The maximum number of configurations is 100') for 101 configurations, got ('InvalidArgument', 'A notification configuration may carry at most 100 configurations, got 101')` (check file line 124).

Cause 2 — s3__sqs.notification-prefix-filter-url-decodes-before-key-matching (verdict: mismatch):
  Setup: PutBucketNotificationConfiguration with one QueueConfiguration whose Filter.Key.FilterRules is [{'Name': 'prefix', 'Value': 'folder%3Dtag/'}]; the control-plane read-back assertion (FilterRules echoed as [{'Name': 'Prefix', 'Value': 'folder%3Dtag/'}]) passed. Then PutObject 'folder%3Dtag/literal-encoded-miss.txt' followed by PutObject 'folder=tag/decoded-hit.txt'.
  Expected (AWS): the decoded-prefix key 'folder=tag/decoded-hit.txt' notifies the queue; the literal-encoded key does not.
  Actual (emulator): `AssertionError: expected 'folder=tag/decoded-hit.txt' (under the decoded prefix 'folder=tag/') to notify the queue; keys seen while draining: ['folder%3Dtag/literal-encoded-miss.txt']` (check file line 89) — the suppressed key was delivered and the matching key was not.

## Steps to Reproduce
Against the emulator's S3/SQS endpoints (any region/account):

Cause 1:
1. CreateBucket; CreateQueue and set its Policy to allow s3.amazonaws.com sqs:SendMessage with aws:SourceArn the bucket ARN.
2. PutBucketNotificationConfiguration with 101 QueueConfigurations (distinct Ids, the same QueueArn, Events ['s3:ObjectCreated:*']) -> observe Code 'InvalidArgument' with 'A notification configuration may carry at most 100 configurations, got 101'. AWS answers Code 'InvalidRequest' with 'Too many configurations submitted. The maximum number of configurations is 100'.
3. The same call with 100 configurations must succeed.

Cause 2:
1. Same bucket/queue/policy setup.
2. PutBucketNotificationConfiguration with one QueueConfiguration {Id: 'prefix-filter', QueueArn: <q>, Events: ['s3:ObjectCreated:*'], Filter: {Key: {FilterRules: [{Name: 'prefix', Value: 'folder%3Dtag/'}]}}}.
3. GetBucketNotificationConfiguration -> FilterRules must echo [{'Name': 'Prefix', 'Value': 'folder%3Dtag/'}] verbatim (this already holds).
4. PutObject Key='folder%3Dtag/literal-encoded-miss.txt', then PutObject Key='folder=tag/decoded-hit.txt'.
5. Drain the queue: AWS delivers only the 'folder=tag/decoded-hit.txt' event; the emulator delivers only the 'folder%3Dtag/literal-encoded-miss.txt' event.
