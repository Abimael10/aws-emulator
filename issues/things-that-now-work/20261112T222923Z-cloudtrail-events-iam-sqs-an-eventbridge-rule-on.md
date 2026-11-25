Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudtrail__events__iam__sqs.xsvc-iam-1-iam-role-events-route-by-event-name-and-only-in-us-east-1
Issue-Key: cloudtrail__events__iam__sqs.xsvc-iam-1-iam-role-events-route-by-event-name-and-only-in-us-east-1|_collect#86|Timeout|msgs = sqs.receive_message(
Issue-Commit: a0165563333d7f6521b0bb32e0431915bfb4444e

# cloudtrail__events__iam__sqs: an EventBridge rule on an IAM CloudTrail event never delivers to its SQS target, so the leader-region routing check never completes

> _Note: this issue was identified against the code base at commit `a0165563333d7f6521b0bb32e0431915bfb4444e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRule

## Description
Cause 1 of 1: a CloudTrail-sourced IAM management event is never published onto the EventBridge default bus, so a rule matching it never delivers and the check runs out of time instead of reaching any assertion.
AWS: IAM is a global service — its management events are recorded by CloudTrail, and delivered by EventBridge, in the partition leader region (us-east-1) only. A multi-region trail that is logging makes each accepted IAM write (e.g. CreateRole/TagRole/DeleteRole) available on the us-east-1 default event bus as an event with source 'aws.iam' and detail-type 'AWS API Call via CloudTrail', whose detail carries the CloudTrail record verbatim (including detail.eventName). A rule whose event pattern names those fields delivers the rendered InputTransformer body to its SQS target in that region, and no bus in any other region ever sees the same event. Recorded behavior only: this delivery path is not expressed in any Smithy model; the recording shows the first delivery arriving well inside the check's own cold budget.
Emulator: nothing is ever delivered. The check is cut off by the runner with 'Check timed out after 60.0s' — it blocks on the first wait for a delivered message and never returns a verdict, even though the emulator answers every control-plane call instantly (the check's own budgets — 1000 s cold, 240 s warm — exist for real AWS's trail warm-up, so against an emulator a live channel should deliver in seconds). Hypothesis: the CloudTrail-to-EventBridge bridge does not exist for iam.amazonaws.com events, so the rule has nothing to match; the adjacent open issues 20261112T213312Z-cloudtrail-cognito-idp-events-sqs-no-cloudtrail-sourced.md (no CloudTrail-sourced 'AWS API Call via CloudTrail' event ever reaches an EventBridge rule, for cognito-idp) and 20261030T182917Z-cloudtrail-lookupevents-never-serves-the-iam-amazonaws-com.md (LookupEvents serves no iam.amazonaws.com record in the leader region) describe the same shape for other services/surfaces but do not carry this check.
Why: a client that builds audit-driven automation — reacting to IAM writes by routing the CloudTrail event to a queue, and relying on that traffic existing only in the leader region — observes an empty queue forever and cannot distinguish 'no such event' from 'routing broken'. The region-scoping half of the contract (the same rule in another region must receive nothing) is also unobservable, so a client cannot validate its single-region deployment.
Fix: the CloudTrail-to-EventBridge publication path (management events recorded for global services in the partition leader region must be published onto that region's default bus as source 'aws.iam', detail-type 'AWS API Call via CloudTrail') and the event-pattern match/target delivery for that detail-type.
Traces: cloudtrail__events__iam__sqs.xsvc-iam-1-iam-role-events-route-by-event-name-and-only-in-us-east-1.

## Evidence
Expected: within the check's delivery budget, the SQS target of the us-east-1 rule receives one message per matching IAM CloudTrail event, bodies rendered by the InputTransformer template '{"rule": "<aws.events.rule-name>", "event": <aws.events.event.json>}', with detail-type 'AWS API Call via CloudTrail'; the comparison region's twin rule receives nothing. Actual: Error: Check timed out after 60.0s — no delivery, no assertion reached.

## Steps to Reproduce
In us-east-1: create an S3 bucket with the CloudTrail service policy, create and start a multi-region trail writing to it, create an SQS queue and an EventBridge rule whose pattern matches source 'aws.iam' / detail-type 'AWS API Call via CloudTrail' / detail.eventName of an IAM write, with the queue as target and the InputTransformer template above. Perform the IAM writes, then long-poll the queue for the delivered bodies; also poll a twin rule+queue in another region and expect nothing there.

## Resolution
The channel this report describes as absent is served by the emulator, for a GLOBAL service's
records as much as for a regional one, and the reported wiring is now driven end to end by tests
in both configurations — so the report is filed here rather than left open:

- `core/router/tests/all/cloudtrail_iam_api_call_events_on_a_started_emulator.rs` drives exactly
  these steps against the emulator a customer STARTS (the built binary on its own port, request
  authentication on, every call signed and made over TLS, the authority an SDK pointed at the
  emulator sends): the bucket and its CloudTrail policy, a multi-Region trail with
  `IncludeGlobalServiceEvents` that is logging, one rule pinned to `CreateRole`/`TagRole`/`DeleteRole`
  with an SQS target delivering through
  `{"rule": "<aws.events.rule-name>", "event": <aws.events.event.json>}`, and its TWIN — the same
  rule and queue names on a second Region's bus. The three IAM writes land as exactly three
  envelopes on the leader Region's queue, one per matching call, each carrying `source` `aws.iam`,
  `detail-type` `AWS API Call via CloudTrail`, the account, `region`/`detail.awsRegion` `us-east-1`,
  `detail.eventSource` `iam.amazonaws.com`, `detail.readOnly` false, the role the call named and —
  for the create — the `RoleId` its own response answered; the comparison Region's twin stays empty.
  The whole flow, emulator start included, takes a few seconds.
- `core/router/tests/all/events_queue_target_grant_forms.rs` pins the document that decides that
  delivery, over the forms a client writes one grant in (the service principal as a string or a
  one-element list, the action or resource as a list, a wildcard action, `Resource` `*`, the action's
  service prefix in capitals as the console writes it, `ArnEquals`/`ArnLike` on `aws:SourceArn`,
  `StringEquals` on `aws:SourceAccount`, every principal scoped to the one rule, one `Statement`
  written as an object) beside the three that grant nothing: no policy, a grant scoped to another
  rule, and an explicit deny.
- The sibling suites already pinned the rest of the channel this check leans on:
  `core/router/tests/all/cloudtrail_api_call_events_by_region.rs` (the global/regional Region split
  in process), `core/router/tests/all/cloudtrail_api_call_events_on_a_started_emulator.rs` (the
  regional half on the started binary) and
  `core/router/tests/all/dynamodb_api_call_events_on_a_started_emulator.rs` (the envelope reaching a
  long poll that was ALREADY in flight when the call was made, which is the wait shape this check
  uses).

The replay unit still ends `Check timed out after 60.0s`. That is the replay harness's own ceiling
on a whole check — the check's own delivery budget is 1000 s cold — and not an observation of a
missing delivery: on the emulator a client starts, every one of these envelopes reaches its SQS
target in well under a second. So this record is kept here as "the behaviour works, and is pinned",
not as a confirmed pass of that check.
