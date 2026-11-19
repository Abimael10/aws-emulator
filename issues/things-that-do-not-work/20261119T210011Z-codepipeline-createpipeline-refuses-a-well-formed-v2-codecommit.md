Issue-Source: trace-sweep-divergence
Issue-Trace: codepipeline-pipeline-source-and-deploy-configuration-round-trips
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codepipeline-pipeline-source-and-deploy-configuration-round-trips
Issue-Shape-Id: com.amazonaws.codepipeline#CodePipeline_20150709
Issue-Key: codepipeline|CreatePipeline|2xx->4xx|$.pipeline
Issue-Key: codepipeline|CreatePipeline|2xx->4xx|status
Issue-Commit: 75a37340e7120e3d2bc225949e80d7a2293fb0a0

# CodePipeline CreatePipeline refuses a well-formed V2 CodeCommit-source / Lambda-invoke declaration with HTTP 400 instead of creating the pipeline

> _Note: this issue was identified against the code base at commit `75a37340e7120e3d2bc225949e80d7a2293fb0a0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePipeline

## Description
Cause 1 of 1: CreatePipeline rejects a valid pipeline declaration, so the pipeline is never created and every later read of it in the same session has nothing to read.

The declaration the recording sends is well formed against every rule CodePipeline states for it: a V2 pipeline (pipelineType 'V2', executionMode 'SUPERSEDED'), a single S3 artifactStore (not artifactStores), a service roleArn of this account, two stages, a first stage holding exactly one Source action (Source/AWS/CodeCommit/1, no inputArtifacts, one outputArtifact 'src', configuration RepositoryName/BranchName/PollForSourceChanges all string-valued), and a second stage holding exactly one non-Source action (Invoke/AWS/Lambda/1, inputArtifacts ['src'] — the artifact the source action produces — no outputArtifacts, configuration FunctionName/UserParameters). No stage or action name repeats, no artifact is consumed before it is produced, and both actions stay inside the artifact bounds their action types publish (CodeCommit source: 0 input / 1 output; Lambda invoke: up to 5 input / up to 5 output).

AWS: CreatePipeline stores such a declaration and answers HTTP 200 with CreatePipelineOutput (Smithy: com.amazonaws.codepipeline#CreatePipeline, input {pipeline: PipelineDeclaration (required), tags}, output {pipeline: PipelineDeclaration, tags}, SUCCESS status 200). The echoed pipeline is the stored declaration — the name, roleArn, artifactStore, pipelineType, executionMode and both stages as sent, with version 1 — which is the same projection a later GetPipeline reports. Every error CreatePipeline models is a 400 that names a specific defect (ConcurrentModificationException, InvalidActionDeclarationException "The action declaration was specified in an invalid format.", InvalidBlockerDeclarationException, InvalidStageDeclarationException "The stage declaration was specified in an invalid format.", InvalidStructureException "The structure was specified in an invalid format.", InvalidTagsException, LimitExceededException, PipelineNameInUseException, TooManyTagsException, ValidationException); this declaration exhibits none of them, so none of those refusals is owed and the call must succeed.

Emulator: the call is refused with HTTP 400 and a body carrying no `pipeline` member at all ($.pipeline: missing, got None), so the pipeline is not stored and the round-trip the fixture goes on to make (read the declaration back, read its configuration) has no pipeline to address.

Hypothesis (the replay records the status, not the error body, so the gate is not identified by the recording): the refusal comes from one of the pre-store gates CreatePipeline runs in services/service-com-amazonaws-codepipeline-codepipeline-20150709/src/ops/create_pipeline.rs — crate::state::role_is_unassumable (InvalidStructureException, message "CodePipeline is not authorized to perform AssumeRole on role <roleArn>", which fires when IAM is read as withholding the assume for codepipeline.amazonaws.com on the role the declaration names) or crate::state::validate_declaration (state.rs, which maps a defect onto InvalidStructureException / InvalidStageDeclarationException / InvalidActionDeclarationException / LimitExceededException / ValidationException). Neither is owed for this declaration, so whichever gate answers is answering a question this declaration does not fail — a trust document the role check reads too narrowly, or a declaration rule applied to a shape it does not govern.

Why: a client that creates a CodeCommit-sourced, Lambda-invoking V2 pipeline — the plainest two-stage pipeline there is — cannot create it at all. The SDK raises a client error on a request AWS accepts, so the pipeline never exists, nothing downstream (GetPipeline, GetPipelineState, StartPipelineExecution, the artifact round-trip) can run, and the failure looks like the caller's declaration is malformed when it is not.

Fix: the CreatePipeline handler's pre-store gates in services/service-com-amazonaws-codepipeline-codepipeline-20150709/src/ops/create_pipeline.rs — the service-role assumability check (crate::state::role_is_unassumable / service-common's iam_role + iam_trust_policy reading) and crate::state::validate_declaration in src/state.rs together with the per-action rules in src/state/action_type_resolution.rs. One of them must stop refusing this declaration; a declaration satisfying every modeled and documented rule has to reach the store and be echoed back at 200. UpdatePipeline shares validate_declaration and the role check, so the same gate has to accept it there.

Traces: codepipeline-pipeline-source-and-deploy-configuration-round-trips#task#7 (the only step in the corpus that exercises this declaration shape; the fixture's remaining steps depend on the create having succeeded).

Not covered by any open issue: 20261111T064249Z-codepipeline-pipeline-declaration-validation-2-net-new-causes.md is about the WORDING of InvalidActionDeclarationException for declarations AWS also refuses (a dangling inputArtifacts reference, an output-artifact count over an action type's maximum), not about a valid declaration being refused; 20261117T111420Z-cloudformation-codepipeline-s3-a-source-action-configuration-value.md is a CloudFormation property-conversion defect for a configuration value sent as a JSON boolean, whereas every configuration value here is already a string; and 20261118T133625Z-cloudformation-codepipeline-s3-an-in-place-update-of.md is about UpdatePipeline through CloudFormation.

## Evidence
Fixture codepipeline-pipeline-source-and-deploy-configuration-round-trips, step task#7, operation CreatePipeline (service codepipeline).

Expected (recorded AWS): HTTP 200, body $.pipeline = the stored declaration —
  artifactStore {type: S3, location: <artifact bucket>}
  executionMode 'SUPERSEDED', pipelineType 'V2', version 1
  name <pipeline name>, roleArn arn:aws:iam::<account>:role/<pipeline role>
  stages[0] 'Source': one action 'Source', actionTypeId {category Source, owner AWS, provider CodeCommit, version 1},
    configuration {BranchName: 'main', PollForSourceChanges: 'false', RepositoryName: <repo>},
    inputArtifacts [], outputArtifacts [{name: 'src'}], runOrder 1
  stages[1] 'Deploy': one action 'Deploy', actionTypeId {category Invoke, owner AWS, provider Lambda, version 1},
    configuration {FunctionName: <function>, UserParameters: <JSON string>},
    inputArtifacts [{name: 'src'}], outputArtifacts [], runOrder 1

Actual (emulator): HTTP 400, and the response body carries no `pipeline` member —
  status: expected 200, got 400
  $.pipeline: missing (expected the declaration above, got None)

The bucket, repository, function, role and pipeline names in the recording are per-run generated values and are not part of the requirement; the requirement is that a declaration of this SHAPE is accepted with 200 and echoed as stored. The emulator's error code and message are not visible in the replay detail (only the status and the missing member are), so no code or wording is quoted here as the diverging value — the divergence is the 400 itself.

## Steps to Reproduce
1. Create an S3 bucket for the artifact store, a CodeCommit repository with a 'main' branch, a Lambda function, and an IAM role trusted by codepipeline.amazonaws.com for the pipeline to run as.
2. Call codepipeline CreatePipeline with a declaration carrying:
   - name: any unused pipeline name; roleArn: the role from step 1; pipelineType 'V2'; executionMode 'SUPERSEDED';
   - artifactStore {type: 'S3', location: the bucket from step 1} (and no artifactStores);
   - stage 'Source' with one action 'Source' of Source/AWS/CodeCommit/1, configuration {RepositoryName: the repo, BranchName: 'main', PollForSourceChanges: 'false'} (string values), inputArtifacts [], outputArtifacts [{name: 'src'}], runOrder 1;
   - stage 'Deploy' with one action 'Deploy' of Invoke/AWS/Lambda/1, configuration {FunctionName: the function, UserParameters: a JSON string}, inputArtifacts [{name: 'src'}], outputArtifacts [], runOrder 1.
3. Observe the response.
   Expected: HTTP 200 with pipeline echoing the declaration at version 1 (and a later GetPipeline reporting the same stages and actions).
   Actual: HTTP 400 with no pipeline member, so the pipeline is never created.
4. Replay with fixture codepipeline-pipeline-source-and-deploy-configuration-round-trips, step task#7.
