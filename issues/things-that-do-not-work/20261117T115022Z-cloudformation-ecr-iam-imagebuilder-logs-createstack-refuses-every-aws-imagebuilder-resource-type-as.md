Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecr__iam__imagebuilder__logs.deploy-imagebuilder-container-pipeline-and-distribution-round-trip-and-delete cloudformation__ecr__iam__imagebuilder__logs.deploy-imagebuilder-image-pipeline-with-both-recipe-arns-rejected-and-rolled-back
Issue-Key: cloudformation__ecr__iam__imagebuilder__logs|CreateStack|ValidationError
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# CreateStack refuses every AWS::ImageBuilder::* resource type as unrecognized, so no Image Builder pipeline stack can deploy

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: CloudFormation does not know the Image Builder resource types.
AWS: recorded behavior only — CloudFormation registers AWS::ImageBuilder::Component, ::ContainerRecipe, ::ImageRecipe, ::InfrastructureConfiguration, ::DistributionConfiguration and ::ImagePipeline as resource types, so a template declaring them is accepted at CreateStack and each resource is provisioned through its handler against the imagebuilder service (Smithy imagebuilder CreateImagePipeline/CreateContainerRecipe/CreateInfrastructureConfiguration/CreateDistributionConfiguration are all modeled). The ValidationError 'Template format error: Unrecognized resource types: [...]' is AWS's answer only for types that genuinely do not exist.
Emulator: CreateStack answers HTTP 400 ValidationError 'Template format error: Unrecognized resource types: [AWS::ImageBuilder::Component, AWS::ImageBuilder::ContainerRecipe, AWS::ImageBuilder::InfrastructureConfiguration, AWS::ImageBuilder::DistributionConfiguration, AWS::ImageBuilder::ImagePipeline]' (the same refusal, minus DistributionConfiguration, for the image-pipeline template). The fixed text and code match AWS's wording for a genuinely unknown type; the defect is that these types ARE AWS types, so the refusal must not happen at all. No stack is created, so neither the container-pipeline round trip nor the both-recipe-ARNs rollback negative can run.
Why: Image Builder is only ever driven from CloudFormation/CDK in practice; every such template is rejected at submit time, and the negative case (a pipeline naming both imageRecipeArn and containerRecipeArn, which must produce a resource-level failure and ROLLBACK_COMPLETE) can never be exercised.
Related open issues that look similar but do not carry these replay keys: 20261109T114319Z-cloudformation-ecr-imagebuilder-createstack-refuses-aws-imagebuilder-component.md, 20261109T114319Z-cloudformation-iam-imagebuilder-createstack-refuses-aws-imagebuilder-infrastructureconfiguration.md and 20261109T114319Z-cloudformation-imagebuilder-createstack-refuses-every-aws-imagebuilder-resource.md.
Fix: the CloudFormation resource-type registry plus resource handlers for the AWS::ImageBuilder::* types.
Failing checks: cloudformation__ecr__iam__imagebuilder__logs.deploy-imagebuilder-container-pipeline-and-distribution-round-trip-and-delete, cloudformation__ecr__iam__imagebuilder__logs.deploy-imagebuilder-image-pipeline-with-both-recipe-arns-rejected-and-rolled-back

## Evidence
[error] cloudformation__ecr__iam__imagebuilder__logs.deploy-imagebuilder-container-pipeline-and-distribution-round-trip-and-delete — botocore.exceptions.ClientError: An error occurred (ValidationError) when calling the CreateStack operation: Template format error: Unrecognized resource types: [AWS::ImageBuilder::Component, AWS::ImageBuilder::ContainerRecipe, AWS::ImageBuilder::InfrastructureConfiguration, AWS::ImageBuilder::DistributionConfiguration, AWS::ImageBuilder::ImagePipeline].
[error] cloudformation__ecr__iam__imagebuilder__logs.deploy-imagebuilder-image-pipeline-with-both-recipe-arns-rejected-and-rolled-back — same refusal for [AWS::ImageBuilder::Component, AWS::ImageBuilder::ContainerRecipe, AWS::ImageBuilder::InfrastructureConfiguration, AWS::ImageBuilder::ImagePipeline]. Expected in both cases: CreateStack accepted, the stack reaching CREATE_COMPLETE (positive) or ROLLBACK_COMPLETE with a resource-handler failure reason (negative).

## Steps to Reproduce
1. CreateStack with a template declaring AWS::ImageBuilder::Component, AWS::ImageBuilder::ContainerRecipe, AWS::ImageBuilder::InfrastructureConfiguration, AWS::ImageBuilder::DistributionConfiguration and AWS::ImageBuilder::ImagePipeline; the call is refused ValidationError instead of being accepted.
2. Repeat with an ImagePipeline naming both ImageRecipeArn and ContainerRecipeArn and observe that the stack never reaches ROLLBACK_COMPLETE because it is never created.
