Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__codepipeline__iam__s3.deploy-codepipeline-pipeline-malformed-role-arn-rejected
Issue-Key: cloudformation__codepipeline__iam__s3.deploy-codepipeline-pipeline-malformed-role-arn-rejected|failed_reason#11|AssertionError|assert len(rows) == 1, f"expected one {status} row for {logical_id}, got {rows!r}"
Issue-Commit: dce52ec55fc752bbcad968bb3711eedbc16de828

# cloudformation__codepipeline__iam__s3: a pattern-violating AWS::CodePipeline::Pipeline RoleArn rolls the stack back with NO stack-level CREATE_FAILED event, so the validation failure is undiagnosable

> _Note: this issue was identified against the code base at commit `dce52ec55fc752bbcad968bb3711eedbc16de828`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: CloudFormation's pre-provisioning property validation of AWS::CodePipeline::Pipeline.RoleArn produces no stack-level CREATE_FAILED event row, so the stack rolls back with nothing that says why. Failing check: cloudformation__codepipeline__iam__s3.deploy-codepipeline-pipeline-malformed-role-arn-rejected.

AWS: Recorded behavior plus the resource type's schema. CloudFormation validates a resource's properties against its resource-type schema BEFORE it provisions anything. AWS::CodePipeline::Pipeline.RoleArn is constrained to the IAM role ARN pattern arn:aws(-[\w]+)*:iam::[0-9]{12}:role/.* (the same role-ARN pattern the CodePipeline Smithy model carries on ActionDeclaration.roleArn: @pattern "^arn:aws(-[\w]+)*:iam::[0-9]{12}:role/"). A CreateStack whose RoleArn violates that pattern ends ROLLBACK_COMPLETE and AWS records the refusal in three places:
  1. A stack-level event whose LogicalResourceId is the STACK's own name, ResourceStatus CREATE_FAILED, DetailedStatus "VALIDATION_FAILED", and ResourceStatusReason exactly: "Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation." Fixed text: the whole sentence pair; the only variable part is the error COUNT ("1 error(s)" here because one property failed), which comes from how many property validations failed in the request.
  2. One DescribeEvents operation event with EventType=VALIDATION_ERROR, ValidationName=PROPERTY_VALIDATION, ValidationStatus=FAILED, ValidationFailureMode=FAIL, ValidationPath="/Resources/<logical id>/Properties/RoleArn" and ValidationStatusReason "Property value [<value>] does not match pattern: <pattern>". Fixed text: "Property value [", "] does not match pattern: "; the bracketed value is the template's own property value and the pattern is the resource schema's; the path is built from the failing resource's logical id and property name.
  3. NO events at all for the Pipeline resource — the resource is never attempted, because validation runs before provisioning.

Emulator: the stack does end ROLLBACK_COMPLETE, but DescribeStackEvents carries NO event whose LogicalResourceId is the stack name with ResourceStatus CREATE_FAILED — the replay found an empty list where exactly one such row is owed (AssertionError: expected one CREATE_FAILED row for <stack name>, got []). Because that row is absent there is no ResourceStatusReason and no DetailedStatus=VALIDATION_FAILED to read, and the replay stopped there, so the VALIDATION_ERROR operation event and the "Pipeline never attempted" requirements above were not reached and remain unverified in this run; they must be satisfied together with the missing row.

Why: a client that deploys a pipeline with a malformed RoleArn sees only "ROLLBACK_COMPLETE" and can learn nothing further. The documented AWS diagnosis path — read the stack's CREATE_FAILED reason, then call DescribeEvents for the per-property detail — returns nothing, so CI systems and operators cannot distinguish a property-validation refusal from an infrastructure failure, and cannot point at the offending property.

Fix: CloudFormation's pre-provisioning property-validation stage and the stack-event writer that records its outcome: the validation failure must emit the stack-level CREATE_FAILED row (with reason and DetailedStatus) and the matching VALIDATION_ERROR operation event, and must short-circuit before the Pipeline resource handler is invoked.

Traces: cloudformation__codepipeline__iam__s3.deploy-codepipeline-pipeline-malformed-role-arn-rejected. Note: this check is listed under a different service heading than the already-filed 20261109T175016Z-cloudformation-codepipeline-4-net-new-causes-customactiontype-ref.md, and that issue does not carry this replay key. The same missing-PROPERTY_VALIDATION shape is filed for other resource types (e.g. 20261113T232314Z-cloudformation-cloudfront-an-invalid-aws-cloudfront-originaccesscontrol-signingprotocol.md, 20261113T225447Z-cloudformation-sns-sqs-a-stack-rejected-by-pre.md); neither covers AWS::CodePipeline::Pipeline.RoleArn.

## Evidence
Check: cloudformation__codepipeline__iam__s3.deploy-codepipeline-pipeline-malformed-role-arn-rejected

Request: CreateStack with a template whose AWS::CodePipeline::Pipeline has RoleArn = "not-an-arn".

Expected (AWS):
  - Stack status ROLLBACK_COMPLETE (emulator matches).
  - Exactly one DescribeStackEvents row with LogicalResourceId == <stack name> and ResourceStatus == "CREATE_FAILED", carrying
      ResourceStatusReason == "Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation."
      DetailedStatus == "VALIDATION_FAILED"
  - DescribeEvents row: (LogicalResourceId="Pipeline", ValidationName="PROPERTY_VALIDATION", ValidationStatus="FAILED", ValidationFailureMode="FAIL", ValidationStatusReason="Property value [not-an-arn] does not match pattern: arn:aws(-[\\w]+)*:iam::[0-9]{12}:role/.*", ValidationPath="/Resources/Pipeline/Properties/RoleArn")
  - No events whatsoever for LogicalResourceId "Pipeline".

Actual (emulator):
  File "check_cloudformation__codepipeline__iam__s3_deploy_codepipeline_pipeline_malformed_role_arn_rejected.py", line 158, in failed_reason
    assert len(rows) == 1, f"expected one {status} row for {logical_id}, got {rows!r}"
  AssertionError: expected one CREATE_FAILED row for sim2real-cppiprl-bad-stack-0f0014b2aa1a, got []

The stack-level CREATE_FAILED event is entirely absent (the stack name in the message is a per-run generated name and is not part of the requirement).

## Steps to Reproduce
1. Create an S3 bucket for the pipeline artifact store.
2. CreateStack with a template containing one AWS::CodePipeline::Pipeline whose Properties.RoleArn is the plain string "not-an-arn" (ArtifactStore pointing at the bucket, one S3 Source stage).
3. Poll DescribeStacks until terminal — it reaches ROLLBACK_COMPLETE.
4. Call DescribeStackEvents (all pages) for the stack id and filter for rows where LogicalResourceId equals the stack's own name and ResourceStatus == "CREATE_FAILED".
   Observed: no such row exists.
   Expected: exactly one, with ResourceStatusReason "Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation." and DetailedStatus "VALIDATION_FAILED".
5. Call DescribeEvents for the stack and filter EventType == "VALIDATION_ERROR": expect one PROPERTY_VALIDATION/FAILED/FAIL row at path /Resources/Pipeline/Properties/RoleArn whose reason names the violated pattern.
6. Confirm no event has LogicalResourceId "Pipeline" (the resource must never be attempted).
