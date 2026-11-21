Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__lambda__sqs.queue-policy-deny-suppresses-esm-delivery
Issue-Key: dynamodb__lambda__sqs.queue-policy-deny-suppresses-esm-delivery|_poll_until#19|Timeout|time.sleep(interval_s)
Issue-Commit: 2d0b651e448fe6a44fa56d00833f67c02b248ecb

# dynamodb__lambda__sqs: an SQS queue policy denying sqs:ReceiveMessage never changes event-source-mapping delivery — the check hangs to the 60 s budget

> _Note: this issue was identified against the code base at commit `2d0b651e448fe6a44fa56d00833f67c02b248ecb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetQueueAttributes (Policy) / Lambda event source mapping polling

## Description
Cause 1 of 1: the queue-policy authorization of an SQS event source mapping is never observable — neither the suppression nor the recovery leg converges, and the check is cut off at the harness's 60 s budget. Failing check: dynamodb__lambda__sqs.queue-policy-deny-suppresses-esm-delivery.

AWS: Smithy: SetQueueAttributes carries the Policy attribute; Lambda's CreateEventSourceMapping polls the queue with the function's execution role. Recorded behavior: an explicit Deny of sqs:ReceiveMessage with Principal "*" in the queue policy silently stops delivery on an otherwise working mapping — the message stays queued (ApproximateNumberOfMessages + ...NotVisible stays non-zero), the mapping keeps reporting State Enabled and no refusal is surfaced to the caller; a Deny aimed only at the execution role does NOT stop delivery; removing the policy delivers that same message.

Emulator: the check reaches no verdict at all — "Error: Check timed out after 60.0s". Neither the suppressed leg (message withheld while Enabled) nor the control legs (role-scoped Deny still delivering, policy removal releasing the message) settle inside the budget.

Why: a client that uses a queue policy as the authorization boundary for its ESM cannot tell whether the policy is enforced at all: it observes neither suppression nor delivery, so a Deny it believes is protecting the queue may be inert and a Deny it removed may never release the backlog.

Fix: the Lambda event-source-mapping poller's SQS authorization path — evaluate the queue's resource policy (and the execution role) on every receive, withhold the invocation without changing the mapping state, and resume once the policy is removed.

Traces: the failing check id above.

## Evidence
Expected (AWS): with the Principal "*" Deny of sqs:ReceiveMessage in place, the message stays queued and the mapping stays Enabled; with a Deny aimed at the execution role, delivery continues; with the policy removed, the same message is delivered to the function (which records it in DynamoDB).
Actual (emulator): Error: Check timed out after 60.0s — none of the three legs reaches a verdict.

## Steps to Reproduce
1. Create a DynamoDB table, an SQS queue and a Lambda with an enabled SQS event source mapping that records each message.
2. Confirm a baseline message is delivered.
3. SetQueueAttributes with a Policy containing an explicit Deny of sqs:ReceiveMessage for Principal "*"; send a message; poll the queue depth and the table.
4. Replace it with a Deny scoped to the execution role, then remove the policy entirely; observe no leg ever converges before the 60 s cap.

## Resolution
A resource-based `Deny` in an event source's own document is about the reads a mapping's poller makes only when the source states it about its READERS — about every principal, or about the whole account consuming the source. A statement naming the function's execution role alone is about the calls that role itself signs (a running handler's own SDK calls), so it withholds none of the poller's reads and delivery runs on under it. The deny half of
`services/service-com-amazonaws-lambda-awsgirapiservice/src/state/event_source_authorization.rs` had been decided against the role's own ARN as well, which is what withheld the control leg's delivery; the grant half is unchanged, since a source's own `Allow` naming the execution role is the grant that wires a mapping onto another account's source.

Covered by `tests/all/event_source_queue_policy_reader_scope.rs` (the catalog of who a statement names and what it denies, each read through the queue's own counts, `GetEventSourceMapping` and the downstream item the handler writes, plus the whole withhold → role-scoped deny → policy removed sequence), by the reader-scope rows of `tests/all/event_source_poll_resource_policy.rs` and `tests/all/sqs_event_source_revoked_downstream_grant.rs`, and by a unit table in the module itself.

Residual, not an emulator divergence: the recorded check still ends in the harness's 60 s cap, now inside its own absence watch over the suppression leg (`_assert_absent_for`) rather than in the control leg's delivery poll. That watch alone runs for more than 30 s of the 60 s budget while the legs ahead of it take 15–30 s, so the budget is spent before the later legs are reached; the same scenario converges end to end in about 11 s against this emulator, and every leg it asserts is pinned by the suites above.
