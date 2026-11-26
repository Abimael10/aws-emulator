Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock.deploy-inference-profile-cfn-routes-invocation
Issue-Key: bedrock-runtime|Converse|ResourceNotFoundException
Issue-Commit: c3aab41312baa47e690c4afe8f7c7d51a974747b

# bedrock: a CloudFormation-deployed AWS::Bedrock::ApplicationInferenceProfile ARN is refused by Converse with ResourceNotFoundException instead of routing to its copied-from model

> _Note: this issue was identified against the code base at commit `c3aab41312baa47e690c4afe8f7c7d51a974747b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Converse

## Description
REOPENS 20261117T122047Z-bedrock-runtime-https-listener-never-negotiates-alpn-h2.md: its fix left the call refused. That issue's Cause 2 asked the Converse/ConverseStream/InvokeModel model-id resolution stage to stop treating every syntactically valid identifier as invocable. Before it, Converse answered 200 for any well-formed modelId (that is exactly what the issue reported as too permissive), so an application-inference-profile ARN was served; after it, the resolver refuses the application-inference-profile ARN class outright. This is one cause.

Cause 1 of 1: an application inference profile that the emulator itself just created — deployed as a CloudFormation AWS::Bedrock::ApplicationInferenceProfile whose ModelSource.CopyFrom is a SYSTEM_DEFINED inference-profile ARN, with the stack reaching CREATE_COMPLETE and publishing the profile ARN as a stack Output — cannot be used as a Converse modelId. The inference profile is created and addressable on the control plane but is not resolvable on the data plane.

AWS: Smithy (services/service-com-amazonaws-bedrockruntime-amazonbedrockfrontendservice) models Converse as POST /model/{modelId}/converse -> 200, modelId a required @httpLabel ConversationalModelId (@length 1..2048) whose @pattern explicitly admits an inference-profile ARN alternative — `([0-9]{12}:(inference-profile|application-inference-profile)/[a-zA-Z0-9-:.]+)` — alongside bare foundation-model ids and custom/provisioned-model ARNs. The bedrock control plane models CreateInferenceProfile (POST /inference-profiles -> 201) returning an inferenceProfileArn constrained by `^arn:aws(...):bedrock:(...):(...):(inference-profile|application-inference-profile)/[a-zA-Z0-9-:.]+$`, and GetInferenceProfile (GET /inference-profiles/{inferenceProfileIdentifier} -> 200) whose identifier pattern accepts the same two profile types. On AWS, invoking an application inference profile by its ARN resolves the profile to the model named by its ModelSource.CopyFrom and answers 200 with the required output/stopReason/usage/metrics members and a non-empty text content block; ResourceNotFoundException (client, httpError 404) is modeled for an identifier that does NOT resolve, which is not the case for a profile the account just created. That a freshly created application inference profile is immediately invocable is recorded behavior only (the model does not state propagation semantics); the recording shows the deploy-then-invoke sequence returning a completion.

Emulator: the Converse call on the stack's own InferenceProfileArn output is refused with HTTP 404 ResourceNotFoundException, message 'Could not resolve the model named by the provided model identifier: <modelId>' — fixed text 'Could not resolve the model named by the provided model identifier: ' followed by the modelId taken verbatim from the request's httpLabel (in the recorded run the ARN arn:aws:bedrock:us-west-2:000000000000:application-inference-profile/000001btr5li, whose region, account and 12-character profile id are per-run values, not part of the requirement). The control plane reported success for the same resource moments earlier: the stack reached CREATE_COMPLETE (no CREATE_FAILED events) and Fn::GetAtt Profile.InferenceProfileArn produced that ARN, so the two planes disagree about whether the profile exists.

Why: a client that provisions an application inference profile — the documented way to tag/meter and route on-demand Bedrock invocations, and the only way to address a model through a customer-owned profile — can deploy it successfully and then not invoke it at all. The 404 is indistinguishable from a wrong or deleted identifier, so the caller is told its model id does not exist while the control plane and its CloudFormation stack both report the profile ACTIVE. Any infrastructure-as-code path that passes a profile ARN into application configuration (the usual CFN Output -> SSM/env wiring) breaks at the first inference call, and the profile-to-member-model routing path is never exercised.

Fix: the bedrock-runtime model-identifier resolution stage shared by the Converse / ConverseStream / InvokeModel / InvokeModelWithResponseStream handlers — it must accept an `application-inference-profile/<id>` ARN, look the profile up in the bedrock control-plane inference-profile state, and invoke the model named by the profile's ModelSource.CopyFrom (the same resolution it already performs for a SYSTEM_DEFINED `inference-profile/<id>`), rather than failing to resolve and raising ResourceNotFoundException. The adjacent possibility must be checked in the same change: if the CloudFormation AWS::Bedrock::ApplicationInferenceProfile resource provider mints the ARN without registering the profile in the bedrock control-plane inference-profile state, the resolver has nothing to find — then the provider (its create handler) is the defect and must create the profile through the same state CreateInferenceProfile writes, and GetInferenceProfile on the published ARN must answer 200.

Traces: bedrock.deploy-inference-profile-cfn-routes-invocation (verdict error — the check raised at the Converse call, after its CFN asserts had already passed).

Related open issues, neither covering this one: 20261124T234555Z-bedrock-listinferenceprofiles-serves-a-divergent-system-defined-inference.md concerns only the SYSTEM_DEFINED catalog ListInferenceProfiles projects; 20261028T194707Z-bedrock-runtime-generation-stage-serves-a-constant-emulator.md concerns the text the generation stage serves once a model has been resolved, which this call never reaches.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, check bedrock.deploy-inference-profile-cfn-routes-invocation, verdict error (not marked ALREADY FILED and not OWNED by any open issue).

Sequence the check drove (all CFN steps succeeded, so their asserts passed):
  1. bedrock.list_inference_profiles(typeEquals='SYSTEM_DEFINED', maxResults=1000) -> picked a SYSTEM_DEFINED profile ARN (prefers an amazon.nova-micro profile).
  2. cloudformation.create_stack with AWS::Bedrock::ApplicationInferenceProfile { InferenceProfileName: !Ref ProfileName, ModelSource: { CopyFrom: !Ref ModelArn } } and Output InferenceProfileArn = !GetAtt Profile.InferenceProfileArn -> stack_create_complete waiter returned with no *_FAILED events (the check asserts `not reasons`).
  3. cloudformation.describe_stacks -> Outputs carried a non-empty InferenceProfileArn (the check asserts `profile_arn`).
  4. bedrock-runtime.converse(modelId=<that ARN>, messages=[{'role':'user','content':[{'text':'Reply with the single word: ok'}]}], inferenceConfig={'maxTokens':16}).

Expected (AWS): HTTP 200, ConverseResponse with required output.message.content (a text block, non-empty after strip()), stopReason, usage and metrics — the profile resolves to the model named by ModelSource.CopyFrom.

Actual (emulator): HTTP 404 refusal —
  botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the Converse operation: Could not resolve the model named by the provided model identifier: arn:aws:bedrock:us-west-2:000000000000:application-inference-profile/000001btr5li

Message breakdown: fixed text = 'Could not resolve the model named by the provided model identifier: '; variable part = the request's own modelId. The region (us-west-2), account (000000000000) and profile id (000001btr5li) in that ARN are this run's values and are not part of the requirement; the requirement is that the call is not refused at all.

Model evidence (smithy_query): Converse — http {method: POST, uri: /model/{modelId}/converse, code: 200}; modelId required httpLabel, length 1..2048, pattern includes the alternative `([0-9]{12}:(inference-profile|application-inference-profile)/[a-zA-Z0-9-:.]+)`; errors include ResourceNotFoundException (client, 404) and ValidationException (client, 400). CreateInferenceProfile — POST /inference-profiles, 201, output inferenceProfileArn pattern `^arn:aws(|-us-gov|-cn|-iso|-iso-b):bedrock:(|[0-9a-z-]{0,20}):(|[0-9]{12}):(inference-profile|application-inference-profile)/[a-zA-Z0-9-:.]+$`. GetInferenceProfile — GET /inference-profiles/{inferenceProfileIdentifier}, 200, identifier pattern accepts both profile types.

Not grounded in the model (recorded behavior only): that a just-created application inference profile is immediately invocable, and the exact wording of AWS's ResourceNotFoundException message for a genuinely unresolvable identifier.

## Steps to Reproduce
1. git_clone the repo, then run_behavioral_suite so the emulator is built and booted.
2. Re-replay check bedrock.deploy-inference-profile-cfn-routes-invocation.
3. Or by hand: pick a SYSTEM_DEFINED profile ARN from bedrock.list_inference_profiles(typeEquals='SYSTEM_DEFINED').
4. cloudformation.create_stack with a template whose only resource is Type: AWS::Bedrock::ApplicationInferenceProfile, Properties.InferenceProfileName=<any name matching ^([0-9a-zA-Z][ _-]?)+$>, Properties.ModelSource.CopyFrom=<that ARN>, and Outputs.InferenceProfileArn = !GetAtt Profile.InferenceProfileArn. Wait for stack_create_complete — it succeeds.
5. Read the stack's InferenceProfileArn output (an arn:...:application-inference-profile/<id>).
6. bedrock-runtime.converse(modelId=<that ARN>, messages=[{'role':'user','content':[{'text':'Reply with the single word: ok'}]}], inferenceConfig={'maxTokens':16}). Observe HTTP 404 ResourceNotFoundException 'Could not resolve the model named by the provided model identifier: <that ARN>' instead of a 200 ConverseResponse with a non-empty text content block.
7. Cross-check which plane is missing the profile: bedrock.get_inference_profile(inferenceProfileIdentifier=<that ARN>). If it also answers 404, the CloudFormation AWS::Bedrock::ApplicationInferenceProfile provider never registered the profile; if it answers 200, the bedrock-runtime model-id resolver is not consulting the application-inference-profile state.
