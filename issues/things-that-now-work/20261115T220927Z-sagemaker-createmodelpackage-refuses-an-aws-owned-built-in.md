Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sagemaker.e2e-model-package-approval-to-endpoint-deploy-flow sagemaker.e2e-model-package-to-endpoint-serving-flow
Issue-Key: sagemaker|CreateModelPackage|ValidationException
Issue-Commit: 26e3724e83756038c0478f8426d4bacc8a1fccc5

# sagemaker CreateModelPackage refuses an AWS-owned built-in/framework ECR image with ValidationException "Access denied for registry ID", so no model package can be registered for a SageMaker-provided container

> _Note: this issue was identified against the code base at commit `26e3724e83756038c0478f8426d4bacc8a1fccc5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateModelPackage

## Description
Cause 1 of 1: CreateModelPackage rejects every InferenceSpecification container image that does not live in the caller's OWN ECR registry in the request's Region, which rejects the AWS-owned SageMaker algorithm and framework registries that real model-registry flows always use.

AWS: CreateModelPackage registers a package from "The Amazon ECR paths of containers that contain the inference code and model artifacts" (Smithy: CreateModelPackage.InferenceSpecification -> InferenceSpecification.Containers[].ModelPackageContainerDefinition.Image, "The Amazon Elastic Container Registry (Amazon ECR) path where inference code is stored"). Nothing in the model restricts that path to the caller's own registry, and CreateModelPackage's modelled errors are only ConflictException and ResourceLimitExceeded (Smithy: CreateModelPackage errors = ConflictException, ResourceLimitExceeded). AWS's own SageMaker built-in-algorithm and framework images are held in AWS-owned accounts, one per Region, whose ECR repositories carry resource policies granting SageMaker pull on any customer account's behalf — e.g. the legacy built-in algorithm repository `<algo-registry-account>.dkr.ecr.<region>.amazonaws.com/xgboost:1` and the framework repository `<framework-registry-account>.dkr.ecr.<region>.amazonaws.com/sagemaker-xgboost:<version>`. Against AWS, a CreateModelPackage naming one of those images is ACCEPTED: it returns 200 with ModelPackageArn (pattern `^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]{9,16}:[0-9]{12}:model-package/[\S]{1,2048}$`), and the package validates to ModelPackageStatus Completed. Recorded behavior in the two checks below confirms this: both expect the create to succeed and then poll the package to Completed and deploy it.

Emulator: the request is refused with HTTP 400 ValidationException and message `Access denied for registry ID: <registryId>, repository name: <repositoryName>. Please check if your ECR image exists and has proper pull permissions for SageMaker.` In that message the fixed text is `Access denied for registry ID: `, `, repository name: `, and `. Please check if your ECR image exists and has proper pull permissions for SageMaker.`; the variable parts are the registry account id and the repository name parsed out of the request's Image path (the two recordings surfaced the AWS algorithm registry with repository `xgboost`, and the AWS framework registry with repository `sagemaker-xgboost` — those particular account ids and repository names are request values, not part of the requirement). The refusal is unconditional for any ECR path whose registry account is not this emulator's own ECR registry id, or whose Region is not the request's Region.

Why: a client that registers a trained artifact against an AWS-provided inference container — the normal SageMaker Model Registry path, and what every SDK/Studio template emits — can never create a model package at all. The whole downstream flow is unreachable: no ModelPackageArn, so no CreateModel from a package, no EndpointConfig, no endpoint, no InvokeEndpoint. The client also sees a ValidationException that is not a modelled error of the operation and whose text asserts an ECR permission problem that does not exist on AWS, so it will chase an IAM/ECR policy that is already correct.

Fix: the CreateModelPackage handler's image-pull admission check and the predicate it calls — services/service-com-amazonaws-sagemaker-sagemaker/src/ops/create_model_package.rs (the `first_unpullable_image` guard that returns `Scaffolded::validation(image_pull_access_denied(..))`) and services/service-com-amazonaws-sagemaker-sagemaker/src/state/image_pull.rs (`unpullable_image`, which treats pullability as `registry_id == ecr_store::registry_id() && reference.region == request_region`). The rule needs to admit the AWS-owned SageMaker algorithm/framework registries (and generally any registry whose repository grants SageMaker pull) rather than only the caller's own in-Region registry, so that a package naming an AWS-provided container registers and reaches ModelPackageStatus Completed.

Traces: sagemaker.e2e-model-package-approval-to-endpoint-deploy-flow (CreateModelPackage with InferenceSpecification.Containers[0].Image = the Region's built-in `xgboost:1` algorithm image, ModelApprovalStatus=Approved, inside a model package group) and sagemaker.e2e-model-package-to-endpoint-serving-flow (CreateModelPackage, unversioned, with the Region's `sagemaker-xgboost` framework image). Both die at the CreateModelPackage call with the same refusal.

Note: no open issue covers this refusal. The nearest model-package issues (20261107T174830Z-sagemaker-batchdescribemodelpackage-reports-a-created-model-package-as.md and 20261109T020211Z-sagemaker-2-net-new-causes-batchdescribemodelpackage-and-describemodelpackage.md) are about the STATUS reported for a package that was successfully created; they do not cover CreateModelPackage refusing the create outright.

## Evidence
Expected (AWS): CreateModelPackage{ModelPackageGroupName|ModelPackageName, InferenceSpecification.Containers=[{Image: "<aws-algo-registry-account>.dkr.ecr.<region>.amazonaws.com/xgboost:1", ModelDataUrl: "s3://.../model.tar.gz"}], SupportedContentTypes=["text/csv"], SupportedResponseMIMETypes=["text/csv"], ModelApprovalStatus="Approved"} -> HTTP 200 {"ModelPackageArn": "arn:aws:sagemaker:<region>:<account>:model-package/..."}, then DescribeModelPackage reports ModelPackageStatus Pending -> Completed.

Actual (emulator), from the replay:
  botocore.exceptions.ClientError: An error occurred (ValidationException) when calling the CreateModelPackage operation: Access denied for registry ID: 433757028032, repository name: xgboost. Please check if your ECR image exists and has proper pull permissions for SageMaker.
  botocore.exceptions.ClientError: An error occurred (ValidationException) when calling the CreateModelPackage operation: Access denied for registry ID: 246618743249, repository name: sagemaker-xgboost. Please check if your ECR image exists and has proper pull permissions for SageMaker.
(HTTP 400 ValidationException; CreateModelPackage models only ConflictException and ResourceLimitExceeded.)

Source of the refusal: services/service-com-amazonaws-sagemaker-sagemaker/src/state/image_pull.rs lines 75-101 — `let pullable = registry_id == ecr_store::registry_id() && reference.region == request_region;` and `image_pull_access_denied`, invoked from services/service-com-amazonaws-sagemaker-sagemaker/src/ops/create_model_package.rs lines 303-309.

## Steps to Reproduce
1. Create an execution role trusting sagemaker.amazonaws.com and a bucket in the test Region; stage a model artifact (or run a training job) so an s3:// ModelDataUrl exists.
2. Call CreateModelPackageGroup (or skip, for the unversioned form).
3. Call CreateModelPackage with InferenceSpecification.Containers=[{"Image": "<the Region's AWS-owned SageMaker XGBoost registry account>.dkr.ecr.<region>.amazonaws.com/xgboost:1", "ModelDataUrl": "<the s3 artifact>"}], SupportedContentTypes=["text/csv"], SupportedResponseMIMETypes=["text/csv"], ModelApprovalStatus="Approved".
4. Observe HTTP 400 ValidationException "Access denied for registry ID: ..., repository name: xgboost. ..." instead of 200 with a ModelPackageArn. Repeat with the framework repository "<framework-registry-account>.dkr.ecr.<region>.amazonaws.com/sagemaker-xgboost:1.7-1" for the same refusal.
5. Replay: sagemaker.e2e-model-package-approval-to-endpoint-deploy-flow, sagemaker.e2e-model-package-to-endpoint-serving-flow.
