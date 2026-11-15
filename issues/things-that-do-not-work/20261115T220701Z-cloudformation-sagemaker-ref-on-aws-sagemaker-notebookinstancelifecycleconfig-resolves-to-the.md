Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sagemaker.cfn-notebook-lifecycle-config-round-trip
Issue-Key: cloudformation__sagemaker.cfn-notebook-lifecycle-config-round-trip|run#12|AssertionError|assert listed == [(config_name, outputs["RefValue"])], (
Issue-Commit: d002f219c3d54248c7a982776c27dbc5a39e1f3c

# Ref on AWS::SageMaker::NotebookInstanceLifecycleConfig resolves to the config name instead of the lifecycle-configuration ARN

> _Note: this issue was identified against the code base at commit `d002f219c3d54248c7a982776c27dbc5a39e1f3c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStacks

## Description
Cause 1 of 1: CloudFormation's AWS::SageMaker::NotebookInstanceLifecycleConfig handler publishes the bare configuration name as the resource's Ref value (physical resource id), where AWS publishes the lifecycle configuration's ARN.

AWS: recorded behavior only (CloudFormation resource return values are not in the Smithy service model). For AWS::SageMaker::NotebookInstanceLifecycleConfig, Ref returns the Amazon Resource Name of the lifecycle configuration — the same ARN SageMaker itself reports for that name. Smithy (sagemaker): NotebookInstanceLifecycleConfigSummary.NotebookInstanceLifecycleConfigArn is required and documented as "The Amazon Resource Name (ARN) of the lifecycle configuration"; the name member is a separate required member constrained to ^[a-zA-Z0-9](-*[a-zA-Z0-9])*$ (max 63). The ARN has the general form arn:<partition>:sagemaker:<region>:<account-id>:notebook-instance-lifecycle-config/<config-name>, where partition, region, account id and config name all come from the request/stack and are never fixed values.

Emulator: for a CREATE_COMPLETE stack declaring one AWS::SageMaker::NotebookInstanceLifecycleConfig, the stack output built from {"Ref": "LifecycleConfig"} is the configuration NAME, while SageMaker's own ListNotebookInstanceLifecycleConfigs correctly reports the pair (name, arn) for the same config. The replayed check therefore sees ListNotebookInstanceLifecycleConfigs [(<config-name>, 'arn:<partition>:sagemaker:<region>:<account>:notebook-instance-lifecycle-config/<config-name>')] while Ref is '<config-name>' (AssertionError raised on the Ref-vs-list comparison, before the Fn::GetAtt NotebookInstanceLifecycleConfigArn comparison was reached, so only the Ref value is shown to diverge).

Why: an ARN is this resource's identity in CloudFormation. A template that feeds the Ref into another resource's property, an IAM policy Resource element, a tagging call or any cross-service reference then passes a bare name where an ARN is required, so the dependent create is refused or silently matches nothing; a client that compares the stack's Ref/physical id against the ARN SageMaker reports sees a mismatch for a resource that exists and is healthy.

Fix: the AWS::SageMaker::NotebookInstanceLifecycleConfig resource handler in the CloudFormation provisioner — return the lifecycle configuration ARN (as SageMaker mints it for the created config) as the resource's physical resource id / Ref value, not the name. Note the open issue 20261111T111216Z-cloudformation-sagemaker-aws-sagemaker-model-physical-resource-id-is-the.md reports the same name-instead-of-ARN mistake for AWS::SageMaker::Model; it names only that resource type and does not cover this handler, so both need the fix.

Traces: cloudformation__sagemaker.cfn-notebook-lifecycle-config-round-trip (the only failing check grounding this issue).

## Evidence
Expected: the stack's Ref output for the AWS::SageMaker::NotebookInstanceLifecycleConfig equals the ARN SageMaker lists for that name, i.e. arn:<partition>:sagemaker:<region>:<account-id>:notebook-instance-lifecycle-config/<config-name>, so that Ref == ListNotebookInstanceLifecycleConfigs[0].NotebookInstanceLifecycleConfigArn.
Actual: AssertionError: ListNotebookInstanceLifecycleConfigs [('<config-name>', 'arn:aws:sagemaker:<region>:<account>:notebook-instance-lifecycle-config/<config-name>')] for Ref '<config-name>' — the Ref output carries the bare name (recorded run: config name sim2real-nblc-e693d9f8c736, listed ARN arn:aws:sagemaker:us-west-2:000000000000:notebook-instance-lifecycle-config/sim2real-nblc-e693d9f8c736; the name, region and account are per-run values, the divergence is name vs ARN).

## Steps to Reproduce
1. CreateStack a template whose single resource is an AWS::SageMaker::NotebookInstanceLifecycleConfig with NotebookInstanceLifecycleConfigName set to a chosen name and one OnCreate entry carrying a base64 Content, with Outputs RefValue = {"Ref": "LifecycleConfig"} and ArnAttr = {"Fn::GetAtt": "LifecycleConfig.NotebookInstanceLifecycleConfigArn"}.
2. Wait for CREATE_COMPLETE, then DescribeStacks and read Outputs.
3. Call sagemaker ListNotebookInstanceLifecycleConfigs with NameContains=<config-name> and compare: the listed entry's NotebookInstanceLifecycleConfigArn is the ARN, but the stack's RefValue output is the bare config name instead of that ARN.
