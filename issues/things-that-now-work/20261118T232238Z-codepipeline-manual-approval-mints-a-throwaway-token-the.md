Issue-Source: trace-sweep-divergence
Issue-Trace: codepipeline-s3-source-manual-approval-run-succeeds
Issue-Step: task#11
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codepipeline-s3-source-manual-approval-run-succeeds
Issue-Shape-Id: com.amazonaws.codepipeline#CodePipeline_20150709
Issue-Key: codepipeline|ListActionExecutions|2xx->2xx|$.actionExecutionDetails[*]
Issue-Commit: a92645eede9bdba76713a9434199688f9400027f

# CodePipeline manual approval mints a throwaway token: the approval token never becomes the action execution's id

> _Note: this issue was identified against the code base at commit `a92645eede9bdba76713a9434199688f9400027f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListActionExecutions

## Description
ONE cause. A manual-approval action's identity is split in two in the emulator: the token it publishes for the pending approval request and the id it records for the action execution are two independently generated uuids, so the handle a client obtained from GetPipelineState and submitted to PutApprovalResult appears nowhere in the resulting execution record. The call answers 200, so the loss of correlation is silent.

Cause 1 of 1: ListActionExecutions reports an approval action whose actionExecutionId (and whose output.executionResult.externalExecutionId, which the emulator copies from it) is a fresh uuid unrelated to the approval token the same action published (fixture codepipeline-s3-source-manual-approval-run-succeeds, step task#11).

AWS: Smithy (com.amazonaws.codepipeline#CodePipeline_20150709) models ListActionExecutions -> actionExecutionDetails[]: ActionExecutionDetail {actionExecutionId: ActionExecutionId 'The action execution ID', output: ActionExecutionOutput -> executionResult: ActionExecutionResult whose externalExecutionId is "The action provider's external ID for the action execution"}, and ActionExecution.token as "The system-generated token used to identify a unique approval request. The token for each open approval request can be obtained using the GetPipelineState command. It is used to validate that the approval request corresponding to this token is still valid." The model does not state the relation between them, so for the relation this is recorded behavior only: in the recording of an approval that was obtained from GetPipelineState and answered with PutApprovalResult, the approval request's token, the action execution's actionExecutionId as reported by ListActionExecutions, that same action's latestExecution.actionExecutionId as reported by GetPipelineState, and output.executionResult.externalExecutionId are ALL ONE value - the approval provider is the service's own approval record, so the token that identified the request IS the id of the execution that answering it produced. The requirement is that identity, not any particular uuid: the value is the token the emulator itself published for that approval request, read from its own state.
Emulator: the approval action is planned with action_execution_id = random_uuid_v4() and then, when it is reached, publishes a SECOND, unrelated token = Some(random_uuid_v4()); PutApprovalResult validates the submitted token against it, clears it (action.token = None) and sets external_execution_id = action_execution_id. The token is therefore discarded and never surfaces as an id. In this replay the step's whole actionExecutionDetails[0] element failed to match because its actionExecutionId and its output.executionResult.externalExecutionId carry the planner's uuid instead of the token the action had published for the request that was answered (every other member of the element - pipelineExecutionId, pipelineVersion, stageName, actionName, status, input.actionTypeId/configuration/resolvedConfiguration/region/inputArtifacts, output.outputArtifacts, output.executionResult.externalExecutionSummary, outputVariables - matched).
Why: the approval token is the only handle a client holds for a manual approval: it is what GetPipelineState hands out and what PutApprovalResult consumes. A release tool, an audit trail or a notification consumer that approved a request and then looks up the resulting action execution by that token - the standard way to prove WHICH approval request a recorded decision answers - finds no execution with that id and cannot correlate the decision with the request it answered, while the response looks complete.
Fix: the approval branch of the codepipeline crate's execution driver (state::driver, the CATEGORY_APPROVAL branch that publishes the token) together with the action-execution record in state::execution - the pending approval's token and the action execution's id must be the same value, so that GetPipelineState's latestExecution.token, GetPipelineState's latestExecution.actionExecutionId, ListActionExecutions' actionExecutionId and output.executionResult.externalExecutionId all report it. No value from any recording is a requirement; only the identity between those members is.
Traces: codepipeline-s3-source-manual-approval-run-succeeds#task#11.

Already-filed issues that do NOT cover this: 20261111T061123Z-codepipeline-s3-source-read-path-loses-the-source.md owns the task#10/task#12 clusters of the SAME fixture (revisionSummary, triggerDetail, currentRevision, entityUrl, summary wording) and explicitly set the actionExecutionId divergence aside as "a per-run identifier and not a defect"; the replay reports THIS task#11 cluster as unowned, and the source shows the id is not merely per-run - it is a second uuid that discards the token, which is the defect claimed here. 20261104T082423Z-codepipeline-3-net-new-root-causes-listactionexecutions-omits.md concerns ListActionExecutions' missing resolvedConfiguration map (which is present and correct on this step), not the execution's identity.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator_up=true, /__admin/health 200; 344 steps, 331 passed, 13 mismatched). Cluster: fixture codepipeline-s3-source-manual-approval-run-succeeds, step task#11, operation ListActionExecutions, verdict mismatch, expected status 200, actual status 200, affected_count 1, stub_path empty, owned_by empty.

Reported divergence, verbatim:
  $.actionExecutionDetails[0]: missing (expected {'actionExecutionId': 'c4837370-6270-426c-a004-c8b88475b0db', 'actionName': 'ReleaseApproval', 'input': {'actionTypeId': {'category': 'Approval', 'owner': 'AWS', 'provider': 'Manual', 'version': '1'}, 'configuration': {'CustomData': 'Approve release candidate 1.4.2'}, 'inputArtifacts': [], 'region': 'us-west-2', 'resolvedConfiguration': {'CustomData': 'Approve release candidate 1.4.2'}}, 'lastUpdateTime': '<shaped-not-asserted>', 'output': {'executionResult': {'externalExecutionId': 'c4837370-6270-426c-a004-c8b88475b0db', 'externalExecutionSummary': 'Release candidate 1.4.2 approved'}, 'outputArtifacts': [], 'outputVariables': {}}, 'pipelineExecutionId': '9e7dfdca-ae53-450b-9945-863ac1020823', 'pipelineVersion': 1, 'stageName': 'Approve', 'startTime': '<shaped-not-asserted>', 'status': 'Succeeded', 'updatedBy': '<shaped-not-asserted>'}, got {... 'actionExecutionId': '1b0d0b32-3c02-4536-bf25-4b64a50e7b83', ... 'output': {'executionResult': {'externalExecutionId': '1b0d0b32-3c02-4536-bf25-4b64a50e7b83', 'externalExecutionSummary': 'Release candidate 1.4.2 approved'}, ...}})

Field-by-field, the ONLY differing members are actionExecutionId and output.executionResult.externalExecutionId; startTime, lastUpdateTime and updatedBy are not asserted, and pipelineExecutionId ('9e7dfdca-...') matched, so ids ARE correlated across the replayed steps rather than compared as raw recorded literals. Expected: both members hold the one value that identified this approval request (the token the emulator published for it and that the replayed PutApprovalResult was accepted with). Observed: both hold a second, unrelated uuid.

The same split shows up on the sibling step task#12 (GetPipelineState latestExecution.actionExecutionId expected the approval request's value, got '1b0d0b32-...'); that cluster is reported OWNED by 20261111T061123Z-codepipeline-s3-source-read-path-loses-the-source.md and is NOT claimed here.

Source of the split, in this checkout:
  services/service-com-amazonaws-codepipeline-codepipeline-20150709/src/state/execution.rs:625 - the action record is planned with action_execution_id: super::random_uuid_v4().
  services/service-com-amazonaws-codepipeline-codepipeline-20150709/src/state/driver.rs:930-935 - "if category == CATEGORY_APPROVAL { action.token = Some(random_uuid_v4()); ... }": a SECOND uuid, unrelated to action_execution_id.
  services/service-com-amazonaws-codepipeline-codepipeline-20150709/src/ops/put_approval_result.rs:284-303 - the submitted token is matched against action.token, then action.external_execution_id = Some(action.action_execution_id.clone()) and action.token = None: the token that identified the request is dropped and the planner's uuid is published as the provider's external id.

Model consulted: services/service-com-amazonaws-codepipeline-codepipeline-20150709/smithy-model/com-amazonaws-codepipeline-codepipeline-20150709.json - shapes ActionExecutionDetail (actionExecutionId, output), ActionExecutionResult (externalExecutionId), ActionExecution (token, actionExecutionId, externalExecutionId). The uuids above are this recording's/run's values; they fill the slots and are not themselves requirements.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay fixture codepipeline-s3-source-manual-approval-run-succeeds: it creates a versioned artifact/source bucket and a pipeline role, creates a pipeline with an AWS/S3 Source action and an 'Approve' stage holding an AWS/Manual Approval action named 'ReleaseApproval' (CustomData 'Approve release candidate 1.4.2'), starts an execution, reads GetPipelineState to obtain the pending approval's token, and answers it with PutApprovalResult(summary='Release candidate 1.4.2 approved', status='Approved').
3. Keep the token T that GetPipelineState reported as stageStates[Approve].actionStates[ReleaseApproval].latestExecution.token - it is the value PutApprovalResult is accepted with.
4. Step task#11: cp.list_action_executions(pipelineName=P, filter={'pipelineExecutionId': E}) and take the entry with actionName == 'ReleaseApproval'.
   EXPECT: entry['actionExecutionId'] == T and entry['output']['executionResult']['externalExecutionId'] == T (one identifier for the approval request, its execution and the provider's external id).
   OBSERVE: both carry a different, freshly generated uuid; nothing in the response contains T.
5. Cross-check on step task#12: cp.get_pipeline_state(name=P) reports the approval action's latestExecution.actionExecutionId as that same unrelated uuid rather than T.
6. Both calls answer 200, so a client that approved via T cannot find the execution it produced.
