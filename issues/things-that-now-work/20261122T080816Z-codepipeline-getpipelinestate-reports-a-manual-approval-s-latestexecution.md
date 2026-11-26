Issue-Source: trace-sweep-divergence
Issue-Trace: codepipeline-s3-source-manual-approval-run-succeeds
Issue-Step: task#12
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codepipeline-s3-source-manual-approval-run-succeeds
Issue-Shape-Id: com.amazonaws.codepipeline#CodePipeline_20150709
Issue-Key: codepipeline|GetPipelineState|2xx->2xx|$.stageStates[*].actionStates[*].latestExecution.actionExecutionId
Issue-Commit: 7bae8c9b940c06e39768c519c51a98e77cc49156

# CodePipeline GetPipelineState reports a manual approval's latestExecution.actionExecutionId as a uuid unrelated to the approval token it published

> _Note: this issue was identified against the code base at commit `7bae8c9b940c06e39768c519c51a98e77cc49156`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetPipelineState

## Description
ONE cause.

Cause 1 of 1: GetPipelineState, read after a manual-approval action has been answered with PutApprovalResult, reports that action's latestExecution.actionExecutionId as a freshly generated uuid that has no relation to the approval token the same action had published and that the client submitted to PutApprovalResult (fixture codepipeline-s3-source-manual-approval-run-succeeds, step task#12).

AWS: Smithy (com.amazonaws.codepipeline#CodePipeline_20150709) models GetPipelineState -> stageStates[].actionStates[]: ActionState {latestExecution: ActionExecution}, where ActionExecution.actionExecutionId is documented as 'ID of the workflow action execution in the current stage. Use the GetPipelineState action to retrieve the current action execution details of the current stage' and ActionExecution.token as 'The system-generated token used to identify a unique approval request. The token for each open approval request can be obtained using the GetPipelineState command. It is used to validate that the approval request corresponding to this token is still valid.' The model does not state the relation between the two members, so for the relation this is recorded behavior only: in the recording, for a Manual/Approval action whose pending request was read out of GetPipelineState and answered with PutApprovalResult, the token that identified the approval request and the actionExecutionId subsequently reported for that action's latestExecution are ONE value (the same value ListActionExecutions reports as actionExecutionId and as output.executionResult.externalExecutionId for that action). The approval provider is the service's own approval record, so the token that identified the request IS the id of the execution that answering it produced. The requirement is that identity, read from the emulator's own state - never the particular uuid of any one recording.
Emulator: the approval action's execution record is planned with one random uuid, and when the action is reached a SECOND, independent random uuid is published as the pending approval's token; PutApprovalResult validates the submitted token against that second uuid and then discards it, so GetPipelineState's latestExecution.actionExecutionId surfaces the planner's uuid and the token appears nowhere in the response. In this replay the step answered 200 with every other asserted member matching, and only $.stageStates[1].actionStates[0].latestExecution.actionExecutionId diverged: the replay expected the value correlated with the approval request from the earlier steps of the same fixture (reported as 'e53d8545-a425-4c27-83c3-31d7a31915dd' for that run) and got an unrelated uuid ('6903fa15-9f3f-49b2-b261-81b0ddf59c89' for this run). Those two uuids are per-run values filling the slot; the defect is that the emulator's two values are unrelated to each other, which is reproducible on every run. That the correlated pipelineExecutionId on the same step matched confirms the comparison correlates ids across replayed steps rather than comparing recorded literals.
Why: the approval token is the only handle a client holds for a manual approval - GetPipelineState hands it out, PutApprovalResult consumes it. A release tool, an audit trail or an approval-notification consumer that approved request T and then re-reads GetPipelineState to confirm WHICH execution its decision produced finds an actionExecutionId it has never seen, and cannot join the recorded decision to the request it answered or to the matching ListActionExecutions entry. The response is a complete-looking 200, so the lost correlation is silent.
Fix: the codepipeline crate's approval handling - the execution driver's approval branch that publishes the pending token, together with the action-execution record it is planned into (state::driver / state::execution) - must mint ONE identifier for a manual-approval action and use it for both the pending request's token and the action execution's id, so that GetPipelineState's latestExecution.token, GetPipelineState's latestExecution.actionExecutionId and ListActionExecutions' actionExecutionId / output.executionResult.externalExecutionId all report the same value. GetPipelineState's own projection in the GetPipelineState handler then needs no special case.
Traces: codepipeline-s3-source-manual-approval-run-succeeds#task#12.

Already-filed open issues that do NOT cover this step: 20261118T232238Z-codepipeline-manual-approval-mints-a-throwaway-token-the.md describes the same token/id split but is bound to step task#11 (ListActionExecutions) and explicitly states that the task#12 GetPipelineState cluster 'is NOT claimed here'; the replay now reports task#12 with an empty owned_by, so no open issue carries its replay key. 20261104T082423Z-codepipeline-3-net-new-root-causes-listactionexecutions-omits.md concerns a missing resolvedConfiguration map, not the execution's identity. The earlier issue that once owned this cluster (20261111T061123Z-codepipeline-s3-source-read-path-loses-the-source.md) is no longer open and dismissed the id as 'a per-run identifier', which the cross-step correlation above shows it is not.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true ('/__admin/health answered 200 and the :40785 data listener accepts'), 60 fixtures replayed, 344 steps total, 332 passed, 12 mismatched, 0 errored, 0 blocked, score 96.5%, divergence_count 6, bootstrap_signal=false.

Cluster, verbatim from the replay output:
  fixture_id: codepipeline-s3-source-manual-approval-run-succeeds
  step_key: task#12
  service: codepipeline, operation: GetPipelineState, verdict: mismatch
  expected_status: 200, actual_status: 200
  affected: ["codepipeline-s3-source-manual-approval-run-succeeds#task#12"], affected_count: 1
  stub_path: "" (empty), owned_by: [] (empty)
  detail: $.stageStates[1].actionStates[0].latestExecution.actionExecutionId: value_mismatch (expected 'e53d8545-a425-4c27-83c3-31d7a31915dd', got '6903fa15-9f3f-49b2-b261-81b0ddf59c89')

Expected: the approval action's latestExecution.actionExecutionId equals the identifier of the approval request that was answered - the value the emulator itself published as that action's latestExecution.token and that PutApprovalResult was accepted with, and the same value ListActionExecutions reports for that action's actionExecutionId / output.executionResult.externalExecutionId.
Observed: an unrelated, independently generated uuid; the published token appears nowhere in the response.

No HTTP status or error code diverges on this step (200 expected, 200 served) and no error message is involved; the single diverging member is the identifier above. The two uuids quoted are this recording's and this run's values respectively - they fill the slot and are not themselves requirements. Corroborating sibling step: task#11 (ListActionExecutions) shows the same split on actionExecutionId and output.executionResult.externalExecutionId and is owned by 20261118T232238Z-codepipeline-manual-approval-mints-a-throwaway-token-the.md.

Model consulted: services/service-com-amazonaws-codepipeline-codepipeline-20150709/smithy-model/com-amazonaws-codepipeline-codepipeline-20150709.json - shapes ActionState (latestExecution) and ActionExecution (actionExecutionId, token, externalExecutionId).

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay fixture codepipeline-s3-source-manual-approval-run-succeeds: it creates a versioned source/artifact bucket and a pipeline role, creates a V2 pipeline with an AWS/S3 Source action and an 'Approve' stage holding an AWS/Manual Approval action named 'ReleaseApproval' (CustomData 'Approve release candidate 1.4.2'), and starts an execution.
3. Call GetPipelineState(name=P) while the approval is pending and keep the token T reported at stageStates[Approve].actionStates[ReleaseApproval].latestExecution.token.
4. Answer the approval: PutApprovalResult(pipelineName=P, stageName='Approve', actionName='ReleaseApproval', token=T, result={summary:'Release candidate 1.4.2 approved', status:'Approved'}). It is accepted, so T is the identifier of the request that was answered.
5. Step task#12: call GetPipelineState(name=P) again and read stageStates[1].actionStates[0].latestExecution.actionExecutionId.
   EXPECT: that member equals T (and matches the actionExecutionId ListActionExecutions reports for the same action).
   OBSERVE: it is a different, freshly generated uuid; T does not appear anywhere in the response, and the call still answers 200.
6. Cross-check task#11: ListActionExecutions for the same execution reports the same unrelated uuid as actionExecutionId and as output.executionResult.externalExecutionId, confirming the emulator holds two unrelated identifiers for one approval.
