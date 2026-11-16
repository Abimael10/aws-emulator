Issue-Source: trace-sweep-divergence
Issue-Trace: cfn-describe-events-operation-id-and-changeset-name-params
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cfn-describe-events-operation-id-and-changeset-name-params cloudformation-describestackevents-pagination cloudformation-kbdeps-export-index-importers cloudformation-kbdeps-replace-kb-export-while-imported
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation|DescribeEvents|2xx->2xx|$.DescribeEventsResponse.DescribeEventsResult.OperationEvents
Issue-Key: cloudformation|DescribeStackEvents|2xx->2xx|$.DescribeStackEventsResponse.DescribeStackEventsResult.StackEvents.member[*]
Issue-Key: cloudformation|ListImports|2xx->4xx|$.ListImportsResponse
Issue-Key: cloudformation|ListImports|2xx->4xx|status
Issue-Commit: 76d00de73023d709f64dd9c70d1a3cce4abd8214

# cloudformation: 4 net-new causes — change-set operation events empty, stack events mis-ordered, export-in-use rollback reason misworded, ListImports refuses a live importer

> _Note: this issue was identified against the code base at commit `76d00de73023d709f64dd9c70d1a3cce4abd8214`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEvents

## Description
Four net-new root causes in CloudFormation's operation/stack event stream and in its export-import index. All four replay clusters reported owned_by=[] and no stub, so nothing already filed carries their fixture#step bindings. The nearest open neighbours are "cloudformation change-set previews, operation events and the update lifecycle diverge from the recording" (20261029T171940Z-cloudformation-change-set-previews-operation-events-and-the.md), which covers DescribeEvents EventType/ordering/ResourceProperties for OTHER fixtures but not a change-set-scoped DescribeEvents that returns NOTHING, and "cloudformation: 5 net-new causes — custom-resource ResponseURL ..." (20261109T223435Z) — neither covers DescribeStackEvents ordering, the export-in-use rollback reason, or ListImports.

Cause 1 of 4: DescribeEvents scoped to a change set returns an empty OperationEvents list.
AWS: Smithy: DescribeEvents input ChangeSetName (target ChangeSetNameOrId, @length 1..1600) / OperationId, output OperationEvents (list); the operation is paginated on NextToken/OperationEvents. When a change set has been created on a stack, DescribeEvents filtered to that change set returns the operation's events. Recorded behavior for the contents: two stack-level members, each with EventType 'STACK_EVENT' and OperationType 'CREATE_CHANGESET', carrying StackId (the stack ARN), OperationId, EventId, Timestamp, StartTime, and OperationStatus 'SUCCEEDED' for the completed operation and 'IN_PROGRESS' (with no EndTime) for the one still running.
Emulator: OperationEvents is serialized as the empty string, i.e. no members at all (replay verdict type_mismatch: expected a member list, got '').
Why: a client that drives a change-set workflow and then follows the change set through DescribeEvents (the only per-operation event view) sees no events whatsoever, so it can neither observe CREATE_CHANGESET progress nor attribute a change-set failure; a paginator over OperationEvents terminates immediately.
Fix: the DescribeEvents handler's filtering/selection of operation events (change-set-scoped operations are not recorded as operation events, or are recorded but not matched when the request names a ChangeSetName), and the operation-event state it reads.
Traces: cfn-describe-events-operation-id-and-changeset-name-params#task#6.

Cause 2 of 4: DescribeStackEvents returns the stack's events in the wrong order, and omits ResourceProperties.
AWS: Smithy doc for DescribeStackEvents: "Returns all stack related events for a specified stack in reverse chronological order"; output StackEvents (list), paginated on NextToken/StackEvents. That ordering is a total, stable order, so the same page boundary yields the same elements in the same positions on every call and across pages. Recorded behavior for the sequence: after the leading stack-level event, the per-resource CREATE_COMPLETE events for the stack's AWS::CloudFormation::WaitConditionHandle resources follow in a deterministic sequence (Handle000 at index 1, and so on), and every resource event carries ResourceProperties — '{}' for a resource that declares no properties.
Emulator: only index 0 matches; every one of the 180 following positions holds a different event than the recording (index 1 is Handle059 where the recording has Handle000), i.e. the events are emitted in an arbitrary order rather than the stable reverse-chronological one, and the resource events carry no ResourceProperties member at all.
Why: a client paginating DescribeStackEvents to find the first failure, or diffing event pages, reads an event sequence that does not correspond to the order in which the stack progressed and that can differ between pages/calls, so "the most recent event" and "the event that caused the failure" are both wrong; a client reading ResourceProperties finds no member instead of the resource's properties document.
Fix: the DescribeStackEvents handler's ordering/pagination over the stack-event store (sort by event time, newest first, with a stable tiebreak on emission sequence) and the serializer's population of ResourceProperties.
Traces: cloudformation-describestackevents-pagination#task#4.

Cause 3 of 4: ListImports refuses with HTTP 400 for an export that IS imported by a live stack.
AWS: Smithy: ListImports input ExportName (required), output Imports (list) + NextToken; doc: "Lists all stacks that are importing an exported output value." With a stack B that imports an export published by stack A, ListImports(ExportName=<that export>) answers HTTP 200 with Imports containing the importing stack's name. AWS's documented 400 ValidationError case ("Export '<name>' is not imported by any stack.") applies only when NO stack imports the export, which is not the case here.
Emulator: HTTP 400 with no ListImportsResponse body — the refusal is returned even though the recording's preceding steps established a live Fn::ImportValue consumer, so the status itself (400 instead of 200) and the absent Imports list are both wrong. The fixed text of AWS's refusal for the genuinely-unimported case is "Export '" + <ExportName> + "' is not imported by any stack." with the export name taken from the request; the emulator must not emit it while an importer exists.
Why: "call ListImports before you modify or remove an exported output" is the documented safety check; a client gets a hard error instead of the list of dependent stacks, so dependency discovery is impossible and tooling that treats the 400 as "no importers" will go on to break the importing stack.
Fix: the ListImports handler and the export/import index in CloudFormation state (Fn::ImportValue consumers are not recorded against the export they resolve).
Traces: cloudformation-kbdeps-export-index-importers#task#9.

Cause 4 of 4: the rollback event for an update blocked by an in-use export carries the wrong ResourceStatusReason wording.
AWS: recorded behavior only. When a stack update would change or remove an exported output that another stack imports, the update is cancelled and the stack's UPDATE_ROLLBACK_IN_PROGRESS stack event (ResourceType AWS::CloudFormation::Stack, LogicalResourceId = the stack name) carries ResourceStatusReason whose fixed text is "Delete canceled. Cannot delete export " + <exportName> + " as it is in use by " + <importingStackName> + "." — the two variable parts are the export's name and the importing stack's name, taken from the template/state; everything else is fixed text.
Emulator: the same event carries "Update canceled. Cannot update export <exportName> as it is in use by <importingStackName>." — the fixed text is rewritten ("Update canceled. Cannot update export" instead of "Delete canceled. Cannot delete export"), and three further events in the same response ([9], [10], [11] of StackEvents) also hold different content than the recording.
Why: the status reason is the only machine- and human-readable explanation of why the update rolled back; a client (or operator) matching on AWS's wording to detect the "export still in use" condition never matches, and the emulator's wording also misstates the action that was cancelled.
Fix: the update-rollback path that cancels an export change (the message template it writes into the stack event's ResourceStatusReason) and the stack-event records emitted alongside it.
Traces: cloudformation-kbdeps-replace-kb-export-while-imported#task#13.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false, score 33.4%). 4 mismatch clusters, all with owned_by=[] and stub_path="":

1. cfn-describe-events-operation-id-and-changeset-name-params#task#6 — DescribeEvents, 200 -> 200. $.DescribeEventsResponse.DescribeEventsResult.OperationEvents: type_mismatch — expected {'member': [{'EventType': 'STACK_EVENT', 'OperationType': 'CREATE_CHANGESET', 'OperationStatus': 'SUCCEEDED', 'StackId': 'arn:aws:cloudformation:us-west-2:000000000000:stack/<stack>/<uuid>', 'OperationId'/'StartTime'/'EndTime'/'Timestamp'/'EventId': shaped}, {'EventType': 'STACK_EVENT', 'OperationType': 'CREATE_CHANGESET', 'OperationStatus': 'IN_PROGRESS', ...}]}, got '' (no members).

2. cloudformation-describestackevents-pagination#task#4 — DescribeStackEvents, 200 -> 200. StackEvents.member[1] .. member[180] all report "missing"/positional mismatch. member[1] expected {'LogicalResourceId': 'Handle000', 'ResourceType': 'AWS::CloudFormation::WaitConditionHandle', 'ResourceStatus': 'CREATE_COMPLETE', 'ResourceProperties': '{}', 'StackName': '<stack>', EventId/PhysicalResourceId/Timestamp/ClientRequestToken/OperationId shaped}; got {'LogicalResourceId': 'Handle059', 'ResourceType': 'AWS::CloudFormation::WaitConditionHandle', 'ResourceStatus': 'CREATE_COMPLETE', ... } with NO ResourceProperties member. Index 0 matched; divergence_key enumerates keys member[1]..member[180].

3. cloudformation-kbdeps-export-index-importers#task#9 — ListImports. "status: expected 200, got 400" and "$.ListImportsResponse: missing (expected {'ListImportsResult': {'Imports': {'member': 'sim2real-probe-kbdeps-agent-e65f8430'}}, 'ResponseMetadata': {'RequestId': shaped}}, got None)" — i.e. the recording returns the importing stack's name while the emulator refuses the call.

4. cloudformation-kbdeps-replace-kb-export-while-imported#task#13 — DescribeStackEvents, 200 -> 200. StackEvents.member[6] expected ResourceStatus 'UPDATE_ROLLBACK_IN_PROGRESS' with ResourceStatusReason "Delete canceled. Cannot delete export <stack>-EscalationWriteupsKBKnowledgeBaseId as it is in use by <importing-stack>."; got the same event with ResourceStatusReason "Update canceled. Cannot update export <stack>-EscalationWriteupsKBKnowledgeBaseId as it is in use by <importing-stack>.". members[9], [10], [11] also diverge (divergence_key keys: member[6], member[9], member[10], member[11]).

(Per-run values above — stack ids, UUIDs, timestamps, request ids, host/port in generated WaitConditionHandle URLs — are not part of any requirement and are not asserted by the corpus.)

## Steps to Reproduce
1. Build the emulator from this checkout and replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
2. Cause 1: create a stack, then CreateChangeSet on it, then call DescribeEvents with ChangeSetName set to that change set (fixture cfn-describe-events-operation-id-and-changeset-name-params, step task#6) — OperationEvents comes back empty instead of the CREATE_CHANGESET STACK_EVENT members.
3. Cause 2: create a stack with many AWS::CloudFormation::WaitConditionHandle resources and page DescribeStackEvents (fixture cloudformation-describestackevents-pagination, step task#4) — the events after the first are not in reverse chronological order (index 1 is Handle059 instead of Handle000) and resource events carry no ResourceProperties.
4. Cause 3: create stack A exporting an output, create stack B that consumes it with Fn::ImportValue, then call ListImports with that ExportName (fixture cloudformation-kbdeps-export-index-importers, step task#9) — HTTP 400 instead of 200 with Imports=[B].
5. Cause 4: update stack A so the in-use export would change, then call DescribeStackEvents on A (fixture cloudformation-kbdeps-replace-kb-export-while-imported, step task#13) — the UPDATE_ROLLBACK_IN_PROGRESS event's ResourceStatusReason reads "Update canceled. Cannot update export ..." instead of AWS's "Delete canceled. Cannot delete export ... as it is in use by ...".
