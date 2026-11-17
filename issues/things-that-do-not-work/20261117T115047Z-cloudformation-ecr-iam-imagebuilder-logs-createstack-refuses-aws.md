Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecr__iam__imagebuilder__logs.deploy-imagebuilder-container-pipeline-and-distribution-round-trip-and-delete cloudformation__ecr__iam__imagebuilder__logs.deploy-imagebuilder-image-pipeline-with-both-recipe-arns-rejected-and-rolled-back
Issue-Key: cloudformation__ecr__iam__imagebuilder__logs|CreateStack|ValidationError
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# cloudformation__ecr__iam__imagebuilder__logs: CreateStack refuses AWS::ImageBuilder::Component / ContainerRecipe / InfrastructureConfiguration / DistributionConfiguration / ImagePipeline as unrecognized resource types, so no Image Builder container pipeline can be deployed

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: the CloudFormation template-type registry does not know the AWS::ImageBuilder::* resource types, so the whole stack is rejected at template-validation time.
AWS: CreateStack validates the resource types in a template against the registered type catalog and refuses only types that are genuinely not registered, with ValidationError / "Template format error: Unrecognized resource types: [...]". AWS::ImageBuilder::Component, AWS::ImageBuilder::ContainerRecipe, AWS::ImageBuilder::InfrastructureConfiguration, AWS::ImageBuilder::DistributionConfiguration and AWS::ImageBuilder::ImagePipeline are all registered AWS public types, so a template declaring them is ACCEPTED: CreateStack returns a StackId (HTTP 200) and the stack proceeds to CREATE_COMPLETE, provisioning the corresponding Image Builder resources (recorded behavior — the model for CloudFormation does not enumerate resource types; the imagebuilder service itself is bootstrapped and serves these resources through its own API).
Emulator: CreateStack is refused ValidationError, "Template format error: Unrecognized resource types: [AWS::ImageBuilder::Component, AWS::ImageBuilder::ContainerRecipe, AWS::ImageBuilder::InfrastructureConfiguration, AWS::ImageBuilder::DistributionConfiguration, AWS::ImageBuilder::ImagePipeline]" (and the same minus DistributionConfiguration for the image-pipeline template). The message's fixed text is `Template format error: Unrecognized resource types: [` + the comma-separated type names from the template + `]`; the type names are the variable part, taken from the submitted template. Because the refusal happens before any resource is created, nothing downstream of the stack is exercised: no component, container recipe, infrastructure configuration, distribution configuration or image pipeline is provisioned, no ECR repository is wired as the container output, and the negative control (an image pipeline declaring BOTH ImageRecipeArn and ContainerRecipeArn, which AWS must reject at the resource level and roll back) can never be reached either.
Why: every Image Builder deployment path through CloudFormation is unavailable. A client gets a hard 400 at CreateStack for a template real AWS accepts, so stacks cannot be created, updated or deleted, and the resource-level validation and rollback behaviors behind these types are untestable and unusable.
Fix: CloudFormation's template resource-type registry / template validator, plus the AWS::ImageBuilder::* resource provisioners it dispatches to (Component, ContainerRecipe, InfrastructureConfiguration, DistributionConfiguration, ImagePipeline) wired to the imagebuilder service's Create/Get/Delete operations, and the resource-level rejection that must fail the ImagePipeline resource and roll the stack back when both ImageRecipeArn and ContainerRecipeArn are supplied.
Traces: cloudformation__ecr__iam__imagebuilder__logs.deploy-imagebuilder-container-pipeline-and-distribution-round-trip-and-delete, cloudformation__ecr__iam__imagebuilder__logs.deploy-imagebuilder-image-pipeline-with-both-recipe-arns-rejected-and-rolled-back.
Related open issues report the same registry gap under OTHER service headings and for partly different type sets — 20261109T114319Z-cloudformation-ecr-imagebuilder-createstack-refuses-aws-imagebuilder-component.md (Component / ContainerRecipe), 20261109T114319Z-cloudformation-iam-imagebuilder-createstack-refuses-aws-imagebuilder-infrastructureconfiguration.md (InfrastructureConfiguration / ImageRecipe / ImagePipeline / LifecyclePolicy) and 20261109T114319Z-cloudformation-imagebuilder-createstack-refuses-every-aws-imagebuilder-resource.md — none of them covers these checks, which additionally require DistributionConfiguration in the same template and the ECR/logs wiring.

## Evidence
cloudformation__ecr__iam__imagebuilder__logs.deploy-imagebuilder-container-pipeline-and-distribution-round-trip-and-delete:
  botocore.exceptions.ClientError: An error occurred (ValidationError) when calling the CreateStack operation: Template format error: Unrecognized resource types: [AWS::ImageBuilder::Component, AWS::ImageBuilder::ContainerRecipe, AWS::ImageBuilder::InfrastructureConfiguration, AWS::ImageBuilder::DistributionConfiguration, AWS::ImageBuilder::ImagePipeline]
cloudformation__ecr__iam__imagebuilder__logs.deploy-imagebuilder-image-pipeline-with-both-recipe-arns-rejected-and-rolled-back:
  botocore.exceptions.ClientError: An error occurred (ValidationError) when calling the CreateStack operation: Template format error: Unrecognized resource types: [AWS::ImageBuilder::Component, AWS::ImageBuilder::ContainerRecipe, AWS::ImageBuilder::InfrastructureConfiguration, AWS::ImageBuilder::ImagePipeline]
Expected in both: CreateStack succeeds (HTTP 200, StackId returned); the first stack reaches CREATE_COMPLETE with the component, container recipe, infrastructure configuration, distribution configuration and image pipeline provisioned and then deletes cleanly, and the second fails at the ImagePipeline RESOURCE (both recipe ARNs supplied) and rolls back to ROLLBACK_COMPLETE.
Actual in both: HTTP 400 ValidationError at CreateStack; no stack is ever created, so no resource-level behavior is reached.

## Steps to Reproduce
1. Submit cloudformation.create_stack with a template declaring AWS::ImageBuilder::Component, AWS::ImageBuilder::ContainerRecipe (targeting an ECR repository), AWS::ImageBuilder::InfrastructureConfiguration, AWS::ImageBuilder::DistributionConfiguration and AWS::ImageBuilder::ImagePipeline.
Expected: 200 with a StackId, then CREATE_COMPLETE. Actual: 400 ValidationError "Template format error: Unrecognized resource types: [...]" naming all five types.
2. Submit a second template whose AWS::ImageBuilder::ImagePipeline declares BOTH ImageRecipeArn and ContainerRecipeArn.
Expected: CreateStack accepted, the ImagePipeline resource fails and the stack rolls back to ROLLBACK_COMPLETE. Actual: the same 400 ValidationError at CreateStack, naming four types.
