Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-patch-baseline-and-compliance-workflow
Issue-Step: task#14
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-patch-baseline-and-compliance-workflow ssm-automation-execution-preview-workflow
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|DeletePatchBaseline|4xx->2xx|$.Message
Issue-Key: ssm|DeletePatchBaseline|4xx->2xx|$.__type
Issue-Key: ssm|DeletePatchBaseline|4xx->2xx|status
Issue-Key: ssm|GetExecutionPreview|2xx->2xx|$.ExecutionPreview.Automation.TargetPreviews[*]
Issue-Commit: 0a8d788699eb3929d2d95f97973e466fa561ffef

# SSM DeletePatchBaseline and GetExecutionPreview ignore registered state

> _Note: this issue was identified against the code base at commit `0a8d788699eb3929d2d95f97973e466fa561ffef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeletePatchBaseline

## Description
2 causes. Both operations answer without consulting state the emulator already holds: one deletes a resource AWS protects, the other reports a preview with its target summary dropped.

Cause 1 of 2: DeletePatchBaseline succeeds on a baseline that is registered for a patch group, where AWS refuses with ResourceInUseException (fixture ssm-patch-baseline-and-compliance-workflow, step task#14).
AWS: Smithy: DeletePatchBaseline errors [ResourceInUseException] with httpError 400; the shape's own documentation is 'Error returned if an attempt is made to delete a patch baseline that is registered for a patch group.' The recorded refusal is HTTP 400 with $.__type 'ResourceInUseException' and Message 'PatchBaseline to be deleted is used for Patch Group: <patch group>' - fixed text 'PatchBaseline to be deleted is used for Patch Group: ' followed by the name of the patch group the baseline is registered to, taken from the emulator's state (in the recording that value was the probe's patch group name). Deletion must only be permitted after the group registration is removed.
Emulator: HTTP 200 with a DeletePatchBaselineResult - no __type, no Message - and the baseline is deleted while the patch-group registration still references it.
Why: the guard AWS uses to keep a patch group from losing its baseline is absent, so a client that relies on the refusal (teardown ordering, 'delete only if unused' logic, or a user protected from a destructive mistake) proceeds and silently orphans the patch group's registration; a client that catches ResourceInUseException to drive deregister-then-delete never takes that branch.
Fix: the DeletePatchBaseline handler's precondition check against patch-group registrations in the ssm patch state, plus the error path that serializes ResourceInUseException with AWS's wording.
Traces: ssm-patch-baseline-and-compliance-workflow#task#14.

Cause 2 of 2: GetExecutionPreview omits the Automation preview's TargetPreviews (fixture ssm-automation-execution-preview-workflow, step task#2).
AWS: Smithy: GetExecutionPreview output ExecutionPreview -> Automation -> AutomationExecutionPreview{StepPreviews, Regions, TargetPreviews (0..50 of TargetPreview{Count (integer), TargetType (string)}), TotalAccounts}; TargetPreview is documented as 'Information about the resources that would be included in the actual runbook execution, if it were to be run' and TargetPreviews as the preview of the runbook's impact. For a preview whose targets resolve to managed EC2 instances AWS returns one TargetPreview per resource type with the matched Count - the recording shows a single entry {Count: 1, TargetType: 'AWS::EC2::Instance'}.
Emulator: the Automation preview is returned with TargetPreviews absent (null), so the response states no targets at all even though the preview resolved against a managed node.
Why: the whole point of an execution preview is the blast-radius answer - how many resources of which type a runbook would touch. A client (or an approval gate) reading TargetPreviews sees nothing to approve and either blocks or proceeds believing the runbook affects no resources. Open issue 20261113T104835Z ('SSM execution-preview and document operations diverge from AWS on refusals and acceptance') covers neighbouring execution-preview behavior but does not carry this fixture step's replay key.
Fix: the GetExecutionPreview handler's projection of AutomationExecutionPreview - it must aggregate the resolved targets held in automation/execution-preview state into TargetPreviews entries ({Count, TargetType}) instead of leaving the member unset.
Traces: ssm-automation-execution-preview-workflow#task#2.

## Evidence
Cause 1 (ssm-patch-baseline-and-compliance-workflow#task#14, DeletePatchBaseline): status: expected 400, got 200; $.__type: missing (expected 'ResourceInUseException', got None); $.Message: missing (expected 'PatchBaseline to be deleted is used for Patch Group: sim2real-probe-ssm-patch-group-9b1f38ab', got None). The group name in that message is the request/state value; the fixed text is 'PatchBaseline to be deleted is used for Patch Group: '.
Cause 2 (ssm-automation-execution-preview-workflow#task#2, GetExecutionPreview, 200 vs 200): $.ExecutionPreview.Automation.TargetPreviews[0]: missing (expected {'Count': 1, 'TargetType': 'AWS::EC2::Instance'}, got None).

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Cause 1: replay fixture ssm-patch-baseline-and-compliance-workflow up to step task#14 - create a patch baseline, register it to a patch group, then call DeletePatchBaseline with that BaselineId; observe 200 instead of 400 ResourceInUseException.
4. Cause 2: replay fixture ssm-automation-execution-preview-workflow step task#2 - StartExecutionPreview for a runbook targeting a managed instance, then GetExecutionPreview with the returned ExecutionPreviewId; observe ExecutionPreview.Automation.TargetPreviews absent.
