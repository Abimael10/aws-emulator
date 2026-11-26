Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__stepfunctions.events-putevents-task-without-permission-is-caught-as-access-denied
Issue-Key: events__stepfunctions.events-putevents-task-without-permission-is-caught-as-access-denied|run#8|AssertionError|assert desc["status"] == "SUCCEEDED", (
Issue-Commit: 6e2d7185f30747a2356e601c1d412ac8e0be11c4

# events__stepfunctions: a denied events:putEvents Task reports the ASL error name 'EventBridge.AccessDeniedException' instead of 'EventBridge.AmazonEventBridgeException', and its relayed cause names the wrong service and drops the Request ID (2 causes)

> _Note: this issue was identified against the code base at commit `6e2d7185f30747a2356e601c1d412ac8e0be11c4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
Two net-new causes in how the optimized EventBridge PutEvents task integration (Resource arn:<partition>:states:::events:putEvents) relays EventBridge's refusal into the task failure. The denial itself IS now enforced — the open issue 20261111T122149Z-events-stepfunctions-events-putevents-task-integration-ignores-the-execution-role.md recorded that the same task used to SUCCEED with a 200 PutEvents envelope — so what remains is the failure's identity and wording, which is what a Catch and any client-side classification depend on. Both causes are grounded in the same failing check: events__stepfunctions.events-putevents-task-without-permission-is-caught-as-access-denied.

Cause 1 of 2: the ASL error name of the relayed refusal.
AWS: for an OPTIMIZED service integration, Step Functions names a task failure after the integration's service prefix plus the SDK exception class it relays, and for the EventBridge PutEvents integration a refused call is reported as error 'EventBridge.AmazonEventBridgeException' (recorded behavior only — the sfn Smithy model carries the failure in DescribeExecutionOutput$error / $cause and does not enumerate integration error names). A Catch with ErrorEquals ['EventBridge.AmazonEventBridgeException'] therefore matches, the handler branch runs, and the execution ends SUCCEEDED with that branch's output.
Emulator: the task fails with error 'EventBridge.AccessDeniedException' — the raw API error code is substituted for the integration's exception-class name — so the Catch on the documented name does not match and the execution ends 'FAILED' instead of SUCCEEDED (the handler state never runs, so the output has no handled member). Failing check: events__stepfunctions.events-putevents-task-without-permission-is-caught-as-access-denied.

Cause 2 of 2: the text of the relayed cause.
AWS: the cause is the relayed SDK exception message, whose fixed text is "User: <assumed-role ARN> is not authorized to perform: events:PutEvents on resource: <bus ARN> because no identity-based policy allows the events:PutEvents action (Service: AmazonEventBridge; Status Code: 400; Error Code: AccessDeniedException; Request ID: <id>; Proxy: null)". The fixed parts are that whole sentence including the trailer's member names and the service name AmazonEventBridge, the status 400 and the code AccessDeniedException; the variable parts come from the request and from state — the partition and account, the execution role's name and its session name inside the arn:<p>:sts::<account>:assumed-role/<role>/<session> principal, the event-bus ARN named by the entry, and the per-call request id.
Emulator: the trailer is rendered as "(Service: AmazonCloudWatchEvents; Status Code: 400; Error Code: AccessDeniedException; Proxy: null)" — it names the service AmazonCloudWatchEvents instead of AmazonEventBridge and omits the "Request ID: <id>;" segment entirely. The sentence before the trailer matches AWS. Failing check: events__stepfunctions.events-putevents-task-without-permission-is-caught-as-access-denied.

Why: a client that handles this denial the documented way — a Catch on 'EventBridge.AmazonEventBridgeException', or a parser/alarm keyed on the relayed cause's service name and Request ID — gets the opposite verdict from the emulator: the catcher never fires, the workflow ends FAILED instead of taking its error-handling branch, and any log/ticket built from the cause carries the wrong service attribution and no request id to correlate with. A positive control holding events:PutEvents is unaffected, so the divergence shows up only on the failure path, exactly where it is hardest to notice.

Fix: the Step Functions task-failure serializer for optimized integrations (the EventBridge putEvents integration's error-mapping step in the task-dispatch path): map a relayed EventBridge refusal to the error name 'EventBridge.AmazonEventBridgeException' rather than 'EventBridge.<ApiErrorCode>', and render the relayed cause trailer with Service: AmazonEventBridge and the Request ID segment of the underlying call.

Traces: events__stepfunctions.events-putevents-task-without-permission-is-caught-as-access-denied (its sibling positive-control role, which does hold events:PutEvents on the default bus, is not what diverges).

## Evidence
Expected (AWS): the execution ends SUCCEEDED through the Catch branch, output {"handled": "denied", "err": {...}} with err.Error == "EventBridge.AmazonEventBridgeException" and err.Cause == "User: arn:<partition>:sts::<account>:assumed-role/<role>/<session> is not authorized to perform: events:PutEvents on resource: arn:<partition>:events:<region>:<account>:event-bus/default because no identity-based policy allows the events:PutEvents action (Service: AmazonEventBridge; Status Code: 400; Error Code: AccessDeniedException; Request ID: <id>; Proxy: null)".

Actual (emulator), from the check's assertion on DescribeExecution:
AssertionError: the Catch on 'EventBridge.AmazonEventBridgeException' must handle the denial and end SUCCEEDED, got 'FAILED': error='EventBridge.AccessDeniedException' cause='User: arn:aws:sts::<account>:assumed-role/<role>/<session> is not authorized to perform: events:PutEvents on resource: arn:aws:events:<region>:<account>:event-bus/default because no identity-based policy allows the events:PutEvents action (Service: AmazonCloudWatchEvents; Status Code: 400; Error Code: AccessDeniedException; Proxy: null)'

Divergences, isolated: error name 'EventBridge.AccessDeniedException' vs 'EventBridge.AmazonEventBridgeException'; cause trailer 'Service: AmazonCloudWatchEvents' vs 'Service: AmazonEventBridge'; cause trailer missing the 'Request ID: <id>;' segment. Status 400 and code AccessDeniedException agree. The account id, region, role name, session id and request id in the recorded message are per-run values filling the template, not requirements.

## Steps to Reproduce
1. iam CreateRole with a trust policy allowing states.amazonaws.com to sts:AssumeRole and NO permission policy at all.
2. stepfunctions CreateStateMachine (STANDARD) with that roleArn and definition: StartAt "Put"; state "Put" = Type Task, Resource "arn:<partition>:states:::events:putEvents", Parameters {"Entries": [{"Source": "sim2real.check", "DetailType": "<any>", "Detail": {"marker": "ok"}}]}, Catch [{ErrorEquals: ["EventBridge.AmazonEventBridgeException"], ResultPath: "$.err", Next: "Denied"}], End true; state "Denied" = Pass emitting {"handled": "denied", "err.$": "$.err"}.
3. StartExecution with input {} and poll DescribeExecution to a terminal status.
Observed: status FAILED, error "EventBridge.AccessDeniedException", cause trailer "(Service: AmazonCloudWatchEvents; Status Code: 400; Error Code: AccessDeniedException; Proxy: null)". Expected: status SUCCEEDED via the Catch, output.handled == "denied", err.Error == "EventBridge.AmazonEventBridgeException", and the cause trailer "(Service: AmazonEventBridge; Status Code: 400; Error Code: AccessDeniedException; Request ID: <id>; Proxy: null)".
4. Positive control: repeat with a sibling role allowing events:PutEvents on arn:<partition>:events:<region>:<account>:event-bus/default — the execution must SUCCEED with FailedEntryCount 0 and no handled member.
