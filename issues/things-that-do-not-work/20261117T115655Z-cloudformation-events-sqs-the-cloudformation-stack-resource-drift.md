Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__events__sqs.xsvc-cloudformation-1-pattern-precision
Issue-Key: cloudformation__events__sqs.xsvc-cloudformation-1-pattern-precision|_receive_into#44|Timeout|resp = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=wait_s, VisibilityTimeout=60)
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# cloudformation__events__sqs: the CloudFormation stack/resource/drift status-change pattern-precision matrix never converges, so no CloudFormation event is shown reaching its matching default-bus rule

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
Cause 1 of 1 — the CloudFormation → default-bus → SQS delivery matrix never completes.
AWS: CloudFormation publishes `CloudFormation Resource Status Change`, `CloudFormation Stack Status Change` and `CloudFormation Drift Detection Status Change` events (source aws.cloudformation) to the account's default event bus, and EventBridge delivers each only to rules whose pattern matches it: a create that fails and rolls back reaches a broad rule watching both resource and stack status changes and reaches neither a rule scoped to `status: CREATE_COMPLETE` nor a drift-only rule; a create that succeeds delivers exactly one event to the CREATE_COMPLETE rule; and only a DetectStackDrift delivers DETECTION_IN_PROGRESS and DETECTION_COMPLETE (the latter carrying stack-id) to the drift rule. Each near-miss queue is then shown reachable by the same run. Recorded behavior only (the detail shapes are not modeled).
Emulator: the check never reaches any of those assertions — the replay reports `Check timed out after 60.0s`. Hypothesis: the delivery wait on the positive-control rule never converges, i.e. no CloudFormation status-change event arrives on the default bus (or none is delivered from it to the rule's SQS target), so the matrix cannot be evaluated at all.
Why: a client building stack-lifecycle automation on EventBridge gets no events to react to, and the near-miss guarantees (a drift rule staying quiet for a stack status change, a CREATE_COMPLETE rule staying quiet for a rollback) are unestablished — the quiet queue cannot be distinguished from a dead one.
Fix: CloudFormation's event publication to the default bus for resource-, stack- and drift-detection status changes, and the default-bus rule → SQS target delivery path.
Traces: cloudformation__events__sqs.xsvc-cloudformation-1-pattern-precision.

## Evidence
`Check timed out after 60.0s` — no assertion reached. The check's own budgets (STACK_TIMEOUT_S 420, DELIVERY_TIMEOUT_S 120, MIN_QUIET_S 20) mean it never got past the first delivery wait on the positive-control rule, which must receive ~15 events for a failing create before any near-miss queue is judged quiet.

## Steps to Reproduce
1. Create three default-bus rules with SQS targets: one on detail-type [`CloudFormation Resource Status Change`, `CloudFormation Stack Status Change`], one on `CloudFormation Stack Status Change` with detail.status [`CREATE_COMPLETE`], one on `CloudFormation Drift Detection Status Change`; attach queue policies allowing events.amazonaws.com for each rule ARN.
2. CreateStack with a template that fails server-side (e.g. an AWS::SSM::Parameter whose Value violates its AllowedPattern) so the stack rolls back.
3. CreateStack a succeeding twin, then DetectStackDrift on it.
4. Poll each queue: the broad rule must receive the rollback's events, the CREATE_COMPLETE rule exactly one event for the succeeding create, and the drift rule only the two drift events.
