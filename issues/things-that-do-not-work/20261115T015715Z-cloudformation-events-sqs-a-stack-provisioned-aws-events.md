Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__events__sqs.cfn-events-event-bus-in-place-update-retained-replacement-then-not-found
Issue-Key: cloudformation__events__sqs.cfn-events-event-bus-in-place-update-retained-replacement-then-not-found|_assert_routed#28|TimeoutError|raise TimeoutError(f"run budget ended before {bus} routed the event")
Issue-Commit: 2f00469c6b1c115e8270b5c2778d03323e9ec131

# cloudformation__events__sqs: a stack-provisioned AWS::Events::Rule on the stack's own AWS::Events::EventBus never routes a PutEvents event to the stack's SQS target

> _Note: this issue was identified against the code base at commit `2f00469c6b1c115e8270b5c2778d03323e9ec131`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new root cause of service `cloudformation__events__sqs`, grounded in one failing behavioral check.

Note on a related open issue: 20261110T232651Z-cloudformation-events-sqs-a-stack-s-aws-events.md reports this same check failing EARLIER — the stack rolling back because the sibling rule could not be created on the stack's bus (`Event bus <name> does not exist.`). That is no longer what the replay shows: the stack now reaches CREATE_COMPLETE, the bus's Ref/GetAtt Arn outputs are correct and DescribeEventBus reports the bus, and the check fails at the NEXT step. That open issue does not carry this failure's replay key and its description does not describe the routing gap, so this cause is filed here.

Cause 1 of 1: the rule and target CloudFormation provisions are not wired into the live EventBridge router, so an event put on the stack's custom bus never reaches the stack's queue. Failing check: cloudformation__events__sqs.cfn-events-event-bus-in-place-update-retained-replacement-then-not-found.
AWS:      recorded behavior only (resource-provisioner behavior; no Smithy shape describes CloudFormation provisioning). A template declaring `Bus: AWS::Events::EventBus {Name: <name>}`, `Queue: AWS::SQS::Queue`, `Rule: AWS::Events::Rule {EventBusName: {"Ref": "Bus"}, EventPattern: {"source": [<source>]}, State: ENABLED, Targets: [{Arn: {"Fn::GetAtt": ["Queue","Arn"]}, Id: "queue"}]}` and an `AWS::SQS::QueuePolicy` allowing `sqs:SendMessage` to `events.amazonaws.com` with `ArnEquals aws:SourceArn = {"Fn::GetAtt": ["Rule","Arn"]}` reaches CREATE_COMPLETE AND is functionally live: a `PutEvents` on that bus with that `Source` matches the rule and the event envelope is delivered to the queue, with `source` the published source, `detail-type` the published detail type, `detail` the published detail, and `account`/`region` the caller's account and region. The same must hold after an in-place Description update and after a Name change that replaces the bus under `UpdateReplacePolicy: Retain` (the new bus routes; the retained old bus survives), and after the stack delete `DescribeEventBus` on the replacement must answer `ResourceNotFoundException` with the message `Event bus <name> does not exist.` (fixed text with the requested name taken from the request).
Emulator: the create side of CloudFormation now looks right — the stack settles CREATE_COMPLETE, `Bus`'s physical id is its Name, the Outputs are the bus name and `arn:aws:events:<region>:<account>:event-bus/<name>`, and DescribeEventBus reports Name/Arn/Description — but the deployment does not route. `PutEvents` on the stack's bus is accepted (`FailedEntryCount == 0`) and re-issued every 8 s, yet the stack's queue receives nothing; the check is cut off in that loop with `TimeoutError: run budget ended before <bus> routed the event` (its own receive cap is 60 s). Everything the check grades after step 1 — the in-place update keeping the bus and still routing, the retained replacement routing on the new bus, and the post-delete ResourceNotFoundException — is therefore unreachable.
Why:      a client that deploys an event-driven stack (custom bus + rule + SQS target + queue policy) gets a stack CloudFormation reports as CREATE_COMPLETE but that silently drops every event: nothing is delivered, no error is raised, and the deployment looks healthy. Hypothesis: the AWS::Events::Rule provisioner records the rule/target in its own resource state without performing the equivalent PutRule/PutTargets against the EventBridge state the router reads (or records it against a bus identity other than the one the AWS::Events::EventBus resource published), and/or the AWS::SQS::QueuePolicy the stack creates is not the document the EventBridge SQS target authorizes against.
Fix:      the AWS::Events::Rule resource provisioner (its create/update path, which must register the rule and its Targets in the EventBridge routing state keyed by the bus name the AWS::Events::EventBus resource's Ref publishes) and the AWS::SQS::QueuePolicy provisioner's interaction with the EventBridge SQS-target authorization, in the cloudformation crate.
Traces:   cloudformation__events__sqs.cfn-events-event-bus-in-place-update-retained-replacement-then-not-found

## Evidence
Check cloudformation__events__sqs.cfn-events-event-bus-in-place-update-retained-replacement-then-not-found, verdict error:
  File "check_cloudformation__events__sqs_cfn_events_event_bus_in_place_update_retained_replacement_then_not_found.py", line 143, in run -> _assert_routed(ctx, events, sqs, first, source, queue_url)
  line 231, in _assert_routed -> raise TimeoutError(f"run budget ended before {bus} routed the event")
  TimeoutError: run budget ended before <bus-name> routed the event

What succeeded against the emulator before that line (so the failure is isolated to routing): CreateStack with the four-resource template; DescribeStacks settling on CREATE_COMPLETE; the Bus resource's physical id == its template Name; Outputs == {BusName: <name>, BusArn: arn:aws:events:<region>:<account>:event-bus/<name>}; DescribeEventBus(<name>) reporting {Name, Arn, Description: "first"}; and every events:PutEvents on that bus returning FailedEntryCount == 0.

Expected (real AWS): within the check's 60 s receive cap, one of the probe events put on the stack's bus arrives on the stack's queue and its envelope satisfies {source: <the published source>, detail-type: "sim2real-probe", detail: {"probe": <the probe marker>}, account: <account>, region: <region>}.
Actual (emulator): the queue receives zero messages for every probe; the loop runs until the run budget is spent.

## Steps to Reproduce
1. Boot the emulator and replay cloudformation__events__sqs.cfn-events-event-bus-in-place-update-retained-replacement-then-not-found.
2. CreateStack with: Bus = AWS::Events::EventBus {Name: <bus>, Description: "first", UpdateReplacePolicy: Retain}; Queue = AWS::SQS::Queue; Rule = AWS::Events::Rule {EventBusName: {"Ref": "Bus"}, EventPattern: {"source": [<source>]}, State: "ENABLED", Targets: [{Arn: {"Fn::GetAtt": ["Queue","Arn"]}, Id: "queue"}]}; QueuePolicy = AWS::SQS::QueuePolicy allowing sqs:SendMessage to Service events.amazonaws.com on the queue ARN with Condition ArnEquals aws:SourceArn = {"Fn::GetAtt": ["Rule","Arn"]}. Outputs: Ref Bus and Fn::GetAtt Bus.Arn.
3. Wait for CREATE_COMPLETE (the emulator reaches it) and confirm DescribeEventBus(<bus>) reports the bus.
4. events:PutEvents Entries=[{EventBusName: <bus>, Source: <source>, DetailType: "sim2real-probe", Detail: {"probe": <marker>}}] and long-poll the stack's queue. Expect the envelope within seconds; the emulator delivers nothing however many times the event is re-published.
5. (Unreachable today) UpdateStack with only the Description changed and re-assert routing on the same bus; UpdateStack with a new Name and assert the replacement bus routes while the old bus is retained; delete the stack and assert DescribeEventBus on the replacement answers ResourceNotFoundException "Event bus <name> does not exist."
