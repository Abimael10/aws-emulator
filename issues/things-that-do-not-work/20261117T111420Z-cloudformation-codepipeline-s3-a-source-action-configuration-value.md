Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__codepipeline__s3.deploy-codepipeline-pipeline-source-poll-for-source-changes-boolean-round-trip-and-delete
Issue-Key: cloudformation__codepipeline__s3.deploy-codepipeline-pipeline-source-poll-for-source-changes-boolean-round-trip-and-delete|_create_stack_retrying_role_propagation#9|AssertionError|raise AssertionError(
Issue-Commit: dce52ec55fc752bbcad968bb3711eedbc16de828

# cloudformation__codepipeline__s3: a source-action Configuration value declared as a JSON boolean (PollForSourceChanges: false) fails the AWS::CodePipeline::Pipeline create and rolls the stack back instead of being coerced to the string "false"

> _Note: this issue was identified against the code base at commit `dce52ec55fc752bbcad968bb3711eedbc16de828`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: an AWS::CodePipeline::Pipeline whose action Configuration carries a JSON boolean where CodePipeline expects a string is refused by the resource handler, so the stack never reaches CREATE_COMPLETE. Failing check: cloudformation__codepipeline__s3.deploy-codepipeline-pipeline-source-poll-for-source-changes-boolean-round-trip-and-delete.

AWS: Smithy: CodePipeline ActionDeclaration.configuration targets ActionConfigurationMap, a map whose key is ActionConfigurationKey (@length 1..50) and whose value is ActionConfigurationValue (@length 1..1000) — i.e. the configuration values are STRINGS on the wire. CloudFormation converts a JSON scalar supplied for a string-typed property into its string form before handing it to the resource handler, so a template that writes
    "Configuration": { "S3Bucket": ..., "S3ObjectKey": "source.zip", "PollForSourceChanges": false }
with a JSON boolean (not the quoted string "false") is accepted: the stack reaches CREATE_COMPLETE, the pipeline is created, and CodePipeline's own GetPipeline returns that configuration entry as the string "false" — configuration == {"S3Bucket": <bucket>, "S3ObjectKey": "source.zip", "PollForSourceChanges": "false"}. Recorded behavior: the same template written with the quoted string "false" is accepted by this emulator today (several other passing checks use that form), so the only difference that triggers the refusal is the JSON scalar type.

Emulator: the Pipeline resource create fails and the stack rolls back. DescribeStackEvents decisive rows are [('PipelineRole','CREATE_COMPLETE'), ('Pipeline','CREATE_FAILED'), ('Pipeline','DELETE_COMPLETE'), ('PipelineRole','DELETE_COMPLETE'), (<stack>, 'ROLLBACK_COMPLETE')] — the stack ends ROLLBACK_COMPLETE where CREATE_COMPLETE is owed. The Pipeline CREATE_FAILED reason did not match the harness's known role-propagation reason, so the refusal is the handler's own, not an IAM trust race. Hypothesis: the JSON boolean is not coerced to a string before the configuration map is built, so either the CloudFormation AWS::CodePipeline::Pipeline property conversion or CreatePipeline's configuration-map deserialization rejects a non-string value instead of stringifying it.

Why: a client whose template declares an action configuration flag as a YAML/JSON boolean — the natural way to write PollForSourceChanges in YAML, and a form AWS accepts — cannot deploy a pipeline at all. The whole stack is rolled back, so every resource in it is lost, and the error surfaces as a generic resource-create failure rather than anything actionable.

Fix: the AWS::CodePipeline::Pipeline resource handler's property conversion for ActionDeclaration.configuration (and/or CreatePipeline's configuration-map deserialization): a JSON boolean or number supplied for an ActionConfigurationValue must be converted to its string form (false -> "false") and stored, so GetPipeline reads it back as that string.

Traces: cloudformation__codepipeline__s3.deploy-codepipeline-pipeline-source-poll-for-source-changes-boolean-round-trip-and-delete. Not covered by the already-filed 20261111T111216Z-cloudformation-codepipeline-s3-fn-getatt-pipeline-version-on-aws-codepipeline-pipeline.md, which is about Fn::GetAtt Pipeline.Version resolving to null and does not carry this replay key.

## Evidence
Check: cloudformation__codepipeline__s3.deploy-codepipeline-pipeline-source-poll-for-source-changes-boolean-round-trip-and-delete

Template (excerpt) — the Configuration value is a JSON boolean, not a quoted string:
  "Configuration": {
    "S3Bucket": {"Ref": "BucketName"},
    "S3ObjectKey": "source.zip",
    "PollForSourceChanges": false
  }

Expected (AWS): stack status CREATE_COMPLETE, and GetPipeline's source action configuration ==
  {"S3Bucket": <bucket>, "S3ObjectKey": "source.zip", "PollForSourceChanges": "false"}
(the JSON boolean false stored and returned as the string "false").

Actual (emulator):
  AssertionError: stack 'sim2real-cppipb-stack-8710e13a0de4' ended 'ROLLBACK_COMPLETE', expected CREATE_COMPLETE: [('PipelineRole', 'CREATE_COMPLETE'), ('Pipeline', 'CREATE_FAILED'), ('Pipeline', 'DELETE_COMPLETE'), ('PipelineRole', 'DELETE_COMPLETE'), ('sim2real-cppipb-stack-8710e13a0de4', 'ROLLBACK_COMPLETE')]

The PipelineRole reached CREATE_COMPLETE, so the IAM role existed; only the Pipeline resource was refused. The retry predicate for a fresh-role AssumeRole propagation race did not match this CREATE_FAILED reason, so the refusal is not a trust-propagation timing artifact. (The stack and bucket names are per-run generated values and are not part of the requirement.)

## Steps to Reproduce
1. Create an S3 bucket to serve as both the source and the artifact store.
2. CreateStack (Capabilities=[CAPABILITY_IAM]) with a template containing:
   - an AWS::IAM::Role trusted by codepipeline.amazonaws.com with the artifact-store S3 permissions, and
   - an AWS::CodePipeline::Pipeline whose RoleArn is Fn::GetAtt of that role, with one S3 Source action whose Configuration is {"S3Bucket": <bucket>, "S3ObjectKey": "source.zip", "PollForSourceChanges": false} — the JSON boolean false, NOT the string "false".
3. Poll DescribeStacks until terminal.
   Observed: ROLLBACK_COMPLETE, with the Pipeline resource CREATE_FAILED.
   Expected: CREATE_COMPLETE.
4. (Once the create succeeds) call codepipeline GetPipeline on the stack's pipeline and read the Source action's configuration.
   Expected: {"S3Bucket": <bucket>, "S3ObjectKey": "source.zip", "PollForSourceChanges": "false"} — the boolean normalized to the string "false".
5. Contrast: the identical template with "PollForSourceChanges": "false" (quoted string) deploys successfully today, isolating the JSON scalar type as the trigger.
