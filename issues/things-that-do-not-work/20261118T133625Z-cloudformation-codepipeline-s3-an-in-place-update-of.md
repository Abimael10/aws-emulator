Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__codepipeline__s3.cfn-codepipeline-pipeline-in-place-update-deploys-retained-replacement-then-not-found
Issue-Key: cloudformation__codepipeline__s3.cfn-codepipeline-pipeline-in-place-update-deploys-retained-replacement-then-not-found|_update#28|AssertionError|assert stack["StackStatus"] == "UPDATE_COMPLETE", (
Issue-Commit: 4de176942ad9f309e2f2fa330d84b5df9adf2b94

# cloudformation__codepipeline__s3: an in-place update of AWS::CodePipeline::Pipeline answers NotImplemented, so the stack rolls back instead of rewriting the pipeline's stages

> _Note: this issue was identified against the code base at commit `4de176942ad9f309e2f2fa330d84b5df9adf2b94`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
One net-new root cause in the CloudFormation-provisioned CodePipeline surface.

=== Cause 1 of 1: updating the properties of an existing AWS::CodePipeline::Pipeline is not implemented, so the stack rolls the update back ===

AWS: the rule is the CloudFormation template reference for AWS::CodePipeline::Pipeline (https://docs.aws.amazon.com/AWSCloudFormation/latest/TemplateReference/aws-resource-codepipeline-pipeline.html), where Stages, ArtifactStore, RoleArn, DisableInboundStageTransitions and Tags are updatable ("Update requires: No interruption") and only Name is create-only ("Update requires: Replacement"). The underlying control-plane call is modelled: Smithy codepipeline UpdatePipeline(pipeline: PipelineDeclaration, required) -> UpdatePipelineOutput{pipeline}, errors InvalidActionDeclarationException / InvalidStageDeclarationException / InvalidStructureException / InvalidBlockerDeclarationException / LimitExceededException / ValidationException. So an UpdateStack whose only change is an action Configuration value inside Stages (here the Deploy action's ObjectKey, i.e. the path the artifact is extracted under) is applied in place: the resource keeps its physical id (the same pipeline name, so DescribeStackResources still tracks that logical id to it), GetPipeline reads back the new declaration with its pipeline version incremented by one (1 -> 2, which is also what Fn::GetAtt <Logical>.Version resolves to in the stack Outputs), a subsequent StartPipelineExecution runs that new version to Succeeded, and the stack reaches UPDATE_COMPLETE with an UPDATE_IN_PROGRESS -> UPDATE_COMPLETE pair of events for the logical id and no UPDATE_FAILED. Recorded behavior only for the exact event payloads.

Emulator: the update is refused at the resource handler. The stack settles UPDATE_ROLLBACK_COMPLETE with StackStatusReason "The following resource(s) failed to update: [Pipeline]. " and DescribeStackEvents carries ('Pipeline', 'UPDATE_FAILED', 'NotImplemented: this emulator does not change the properties of an existing AWS::CodePipeline::Pipeline in place; the resource was left as it is.'). In that reason the only request-derived part is the logical id (Pipeline, taken from the template) quoted in the stack reason; the sentence itself is fixed, emulator-authored text that no AWS stack event ever carries — AWS has no NotImplemented resource-status reason for this, and no error code or status of AWS's is being reproduced here.

Why: a client cannot evolve a pipeline through its own stack at all. Editing a stage, an action's Configuration, the artifact store or the pipeline role in the template — the ordinary way a pipeline is changed under CloudFormation — rolls the whole update back, leaves the live pipeline at its old declaration and old version, and reports a failure that tells the caller it is an emulator limitation rather than anything a template change can fix. Because this in-place step fails first, everything downstream of it in the same stack is unreachable too: the execution that must run the new pipeline version, the Name-change replacement that must honour UpdateReplacePolicy: Retain (keeping the old pipeline while the stack tracks the new one), and the post-DeleteStack reads that must refuse the deleted pipeline while the retained one still answers.

Fix: the AWS::CodePipeline::Pipeline resource provider in the cloudformation service must implement the update path — in place for the updatable properties, by issuing codepipeline UpdatePipeline with the new PipelineDeclaration (which also bumps the stored pipeline version and is what GetAtt .Version must read), and replacement for the create-only Name — instead of returning a NotImplemented resource failure.

Related but NOT covering this: open issue 20261117T084005Z-cloudformation-ec2-an-in-place-update-of-aws.md reports the same NotImplemented shape for AWS::EC2::SecurityGroup, and open issues 20261111T111216Z-cloudformation-codepipeline-s3-fn-getatt-pipeline-version-on-aws-codepipeline-pipeline.md (GetAtt Pipeline.Version resolving to null) and 20261117T111420Z-cloudformation-codepipeline-s3-a-source-action-configuration-value.md (a JSON-boolean source-action Configuration value) touch the same resource type but neither names this check nor the provider's update path; the replay report does not mark this check as owned by any of them.

Traces: cloudformation__codepipeline__s3.cfn-codepipeline-pipeline-in-place-update-deploys-retained-replacement-then-not-found.

## Evidence
Failing check cloudformation__codepipeline__s3.cfn-codepipeline-pipeline-in-place-update-deploys-retained-replacement-then-not-found [mismatch]:

  File "check_cloudformation__codepipeline__s3_cfn_codepipeline_pipeline_in_place_update_deploys_retained_replacement_then_not_found.py", line 118, in run
    rows = _update(ctx, cfn, stack_id, "Pipeline", body, Capabilities=_IAM)
  File "...", line 620, in _update
    assert stack["StackStatus"] == "UPDATE_COMPLETE"
  AssertionError: update ended 'UPDATE_ROLLBACK_COMPLETE': 'The following resource(s) failed to update: [Pipeline]. '; [('Pipeline', 'NotImplemented: this emulator does not change the properties of an existing AWS::CodePipeline::Pipeline in place; the resource was left as it is.')]

Expected (AWS): DescribeStacks StackStatus UPDATE_COMPLETE; DescribeStackResources still maps logical id Pipeline to the same pipeline name; GetPipeline returns the new declaration with pipeline version 2 and the stack Output from Fn::GetAtt Pipeline.Version is "2"; a following StartPipelineExecution finishes Succeeded at pipelineVersion 2.
Actual (emulator): StackStatus UPDATE_ROLLBACK_COMPLETE, StackStatusReason "The following resource(s) failed to update: [Pipeline]. ", and a resource event UPDATE_FAILED whose reason is the fixed emulator sentence "NotImplemented: this emulator does not change the properties of an existing AWS::CodePipeline::Pipeline in place; the resource was left as it is." (only the logical id quoted in the stack reason comes from the request). No AWS error code or HTTP status is involved — AWS never emits this reason.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite so the emulator is serving.
2. Create a versioned S3 bucket and put a zip under source.zip in it.
3. CreateStack (Capabilities CAPABILITY_IAM) with an AWS::IAM::Role trusted by codepipeline.amazonaws.com and an AWS::CodePipeline::Pipeline (logical id Pipeline, UpdateReplacePolicy: Retain) whose Stages are an S3 Source action (S3Bucket=<bucket>, S3ObjectKey=source.zip) and an S3 Deploy action (BucketName=<bucket>, Extract=true, ObjectKey=site-a). Wait for CREATE_COMPLETE; GetPipeline shows version 1.
4. UpdateStack with the identical template except the Deploy action's ObjectKey changed to site-b, then poll DescribeStacks.
5. Emulator: the stack settles UPDATE_ROLLBACK_COMPLETE and DescribeStackEvents shows Pipeline UPDATE_FAILED with the NotImplemented reason; the live pipeline is still at version 1 deploying to site-a. AWS: UPDATE_COMPLETE, the same pipeline name, GetPipeline at version 2 with ObjectKey site-b, and a StartPipelineExecution that succeeds on version 2 and extracts the artifact under site-b/.
