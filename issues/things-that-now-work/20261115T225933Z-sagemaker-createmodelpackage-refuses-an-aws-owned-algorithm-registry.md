Issue-Source: trace-sweep-divergence
Issue-Trace: sagemaker-model-package-create-and-verify
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sagemaker-model-package-create-and-verify sagemaker-model-package-metric-numeric-precision-preserved
Issue-Shape-Id: com.amazonaws.sagemaker#SageMaker
Issue-Key: sagemaker|CreateModelPackage|2xx->4xx|$.ModelPackageArn
Issue-Key: sagemaker|CreateModelPackage|2xx->4xx|status
Issue-Key: sagemaker|DescribeModelPackage|2xx->4xx|$
Issue-Key: sagemaker|DescribeModelPackage|2xx->4xx|$.CertifyForMarketplace
Issue-Key: sagemaker|DescribeModelPackage|2xx->4xx|$.CreatedBy
Issue-Key: sagemaker|DescribeModelPackage|2xx->4xx|$.CreationTime
Issue-Key: sagemaker|DescribeModelPackage|2xx->4xx|$.InferenceSpecification
Issue-Key: sagemaker|DescribeModelPackage|2xx->4xx|$.ModelPackageArn
Issue-Key: sagemaker|DescribeModelPackage|2xx->4xx|$.ModelPackageDescription
Issue-Key: sagemaker|DescribeModelPackage|2xx->4xx|$.ModelPackageName
Issue-Key: sagemaker|DescribeModelPackage|2xx->4xx|$.ModelPackageRegistrationType
Issue-Key: sagemaker|DescribeModelPackage|2xx->4xx|$.ModelPackageStatus
Issue-Key: sagemaker|DescribeModelPackage|2xx->4xx|$.ModelPackageStatusDetails
Issue-Key: sagemaker|DescribeModelPackage|2xx->4xx|status
Issue-Key: sagemaker|DeleteModelPackage|2xx->4xx|status
Issue-Commit: a8b5efd4a7d15f36db8efaf56cc150cd0dc8890c

# sagemaker CreateModelPackage refuses an AWS-owned algorithm-registry ECR image, so the whole model-package fixture flow fails

> _Note: this issue was identified against the code base at commit `a8b5efd4a7d15f36db8efaf56cc150cd0dc8890c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateModelPackage

## Description
ONE root cause, FOUR failing replay clusters. A fixer must close all four (the representative plus the three listed in trace_ids); the three dependent ones are expected to pass once the root cause is fixed, but each must be re-replayed to confirm.

Cause 1 of 1 (root cause) — CreateModelPackage rejects any InferenceSpecification container image whose ECR registry is not the emulator's own registry in the request Region, which is exactly how AWS-provided built-in algorithm images are addressed, so no model package is ever created.
Fixture/step: sagemaker-model-package-create-and-verify#task#2 (also sagemaker-model-package-metric-numeric-precision-preserved#setup#3).

AWS: CreateModelPackage registers a package whose InferenceSpecification.Containers[].Image is "The Amazon Elastic Container Registry (Amazon ECR) path where inference code is stored" (Smithy: CreateModelPackage.InferenceSpecification -> InferenceSpecification.Containers -> ModelPackageContainerDefinition.Image). Nothing in the model restricts that path to the caller's own registry or Region, and the operation models only two errors (Smithy: CreateModelPackage errors = ConflictException, ResourceLimitExceeded — ValidationException is not among them). AWS's built-in algorithm images live in AWS-owned, per-Region ECR accounts whose repository policies let SageMaker pull them on any account's behalf, e.g. a path of the form <aws-algorithm-registry-account>.dkr.ecr.<region>.amazonaws.com/<algorithm>:<version>. Recorded behavior: the fixture's CreateModelPackage with such a container returns HTTP 200 carrying ModelPackageArn (output member ModelPackageArn, pattern ^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]{9,16}:[0-9]{12}:model-package/[\S]{1,2048}$), and the package then describes with ModelPackageStatus Completed.

Emulator: the create is refused with HTTP 400 and error code ValidationException, message "Access denied for registry ID: <registryId>, repository name: <repositoryName>. Please check if your ECR image exists and has proper pull permissions for SageMaker." In that message the FIXED text is "Access denied for registry ID: ", ", repository name: ", and ". Please check if your ECR image exists and has proper pull permissions for SageMaker."; the VARIABLE parts are the registry account id and the repository name parsed out of the request's Image path (the particular account id and repository name a recording shows are request values, not part of the requirement). The refusal fires for every ECR path whose registry account is not the emulator's own ECR registry id or whose Region is not the request Region, so an AWS-owned algorithm registry is always refused.

Why: a client doing the ordinary SageMaker Model Registry flow — register a trained artifact against an AWS-provided inference container — can never create a model package. It gets an error code that is not a modelled error of the operation, with text asserting an ECR pull-permission problem that does not exist on AWS, so it will chase an IAM/ECR policy that is already correct, and every downstream step (DescribeModelPackage, DeleteModelPackage, CreateModel from the package, endpoint deploy) is unreachable.

Fix: the CreateModelPackage handler's image-pull admission guard and the predicate behind it — services/service-com-amazonaws-sagemaker-sagemaker/src/ops/create_model_package.rs (the first_unpullable_image guard that answers with a validation error) and services/service-com-amazonaws-sagemaker-sagemaker/src/state/image_pull.rs (unpullable_image, which equates pullability with registry_id == own registry && reference.region == request_region). Pullability must admit AWS-owned SageMaker algorithm/framework registries, not only the caller's own in-Region registry.

Dependent cluster A — DescribeModelPackage, sagemaker-model-package-create-and-verify#task#3: AWS returns HTTP 200 with the created package (ModelPackageArn, ModelPackageName, CreationTime, InferenceSpecification, ModelPackageStatus Completed, ModelPackageStatusDetails, CertifyForMarketplace false; Smithy DescribeModelPackage has no modelled errors). Emulator answers HTTP 400 ValidationException "ModelPackage <modelPackageArn> does not exist." (fixed text "ModelPackage " and " does not exist.", variable part the package ARN built from the request) and the fixture's poll to Completed caps out, because the package was never created. Fix: none of its own — it follows from Cause 1.

Dependent cluster B — DeleteModelPackage, sagemaker-model-package-create-and-verify#teardown#5: AWS returns HTTP 200 with an empty body for the package created in the same fixture (Smithy DeleteModelPackage output Unit, errors = ConflictException only). Emulator answers HTTP 400 because the package does not exist. Fix: none of its own — it follows from Cause 1.

Dependent cluster C — CreateModelPackage, sagemaker-model-package-metric-numeric-precision-preserved#setup#3: the same refusal on a second fixture, whose whole point is the numeric precision of ModelMetrics; the setup create answers HTTP 400 instead of 200, so that fixture cannot exercise its subject at all.

Related open issue: 20261115T220927Z-sagemaker-createmodelpackage-refuses-an-aws-owned-built-in.md describes the same refusal from the BEHAVIORAL corpus, but it carries no wire replay key (no Issue-Trace/Issue-Step), so it does not cover these fixture steps; the replay report lists them as unowned. The other model-package issues (20261107T174830Z-sagemaker-batchdescribemodelpackage-reports-a-created-model-package-as.md, 20261109T020211Z-sagemaker-2-net-new-causes-batchdescribemodelpackage-and-describemodelpackage.md) concern the status reported for a package that WAS created and do not cover this.

Traces: sagemaker-model-package-create-and-verify#task#2 (representative), sagemaker-model-package-create-and-verify#task#3, sagemaker-model-package-create-and-verify#teardown#5, sagemaker-model-package-metric-numeric-precision-preserved#setup#3.

## Evidence
Replay of fixture sagemaker-model-package-create-and-verify:
- step task#2 CreateModelPackage: expected HTTP 200 with $.ModelPackageArn; actual HTTP 400, $.ModelPackageArn missing. The emulator's body is ValidationException with message "Access denied for registry ID: <registryId>, repository name: <repositoryName>. Please check if your ECR image exists and has proper pull permissions for SageMaker." (source: services/service-com-amazonaws-sagemaker-sagemaker/src/state/image_pull.rs lines 75-101, where `let pullable = registry_id == ecr_store::registry_id() && reference.region == request_region;` and `image_pull_access_denied` live).
- step task#3 DescribeModelPackage: expected HTTP 200 with CertifyForMarketplace=false, CreatedBy, CreationTime, InferenceSpecification (Containers[0].Image = the Region's AWS-owned linear-learner algorithm path, with ImageDigest), ModelPackageArn, ModelPackageDescription, ModelPackageName, ModelPackageRegistrationType=Registered, ModelPackageStatus=Completed, ModelPackageStatusDetails; actual HTTP 400 {"__type":"ValidationException","message":"ModelPackage <arn> does not exist."}, and the poll for Completed capped at 180s.
- step teardown#5 DeleteModelPackage: expected HTTP 200; actual HTTP 400.
Replay of fixture sagemaker-model-package-metric-numeric-precision-preserved:
- step setup#3 CreateModelPackage: expected HTTP 200; actual HTTP 400 (same refusal).

Smithy (services/service-com-amazonaws-sagemaker-sagemaker/smithy-model/com-amazonaws-sagemaker-sagemaker.json): CreateModelPackage errors = ConflictException, ResourceLimitExceeded; output member ModelPackageArn required, pattern ^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]{9,16}:[0-9]{12}:model-package/[\S]{1,2048}$. DescribeModelPackage declares no errors. DeleteModelPackage output Unit, errors = ConflictException.

## Steps to Reproduce
1. Boot the emulator from this checkout and point an AWS SDK client at its data listener.
2. Create an S3 bucket in the request Region and stage a model artifact so an s3:// ModelDataUrl exists.
3. Call CreateModelPackage with ModelPackageName=<a name>, ModelPackageDescription=<any>, InferenceSpecification={"Containers":[{"Image":"<the Region's AWS-owned built-in algorithm registry account>.dkr.ecr.<region>.amazonaws.com/linear-learner:1","ModelDataUrl":"<the staged artifact>"}],"SupportedContentTypes":["text/csv"],"SupportedResponseMIMETypes":["text/csv"],"SupportedRealtimeInferenceInstanceTypes":["ml.m5.large"],"SupportedTransformInstanceTypes":["ml.m5.large"]}.
4. Observe HTTP 400 ValidationException "Access denied for registry ID: ..., repository name: linear-learner. Please check if your ECR image exists and has proper pull permissions for SageMaker." instead of HTTP 200 with ModelPackageArn.
5. Call DescribeModelPackage for that name: HTTP 400 ValidationException "ModelPackage <arn> does not exist." instead of HTTP 200 with ModelPackageStatus Completed. Call DeleteModelPackage for it: HTTP 400 instead of HTTP 200.
6. Re-replay to confirm a fix: sagemaker-model-package-create-and-verify#task#2, sagemaker-model-package-create-and-verify#task#3, sagemaker-model-package-create-and-verify#teardown#5, sagemaker-model-package-metric-numeric-precision-preserved#setup#3.
