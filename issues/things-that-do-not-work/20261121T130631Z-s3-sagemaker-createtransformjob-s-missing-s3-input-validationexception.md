Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3__sagemaker.sagemaker-batch-transform-nonexistent-s3-input-negative
Issue-Key: sagemaker|CreateTransformJob|ValidationException
Issue-Commit: 014025136ea556b785f0af1deb0ae2c5bf2fdf97

# s3__sagemaker: CreateTransformJob's missing-S3-input ValidationException message is emulator-authored — it drops the region, the role ARN and the s3:ListBucket guidance AWS names

> _Note: this issue was identified against the code base at commit `014025136ea556b785f0af1deb0ae2c5bf2fdf97`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTransformJob

## Description
One net-new root cause, grounded in a failing behavioral check the replay report left unmarked (no ALREADY-FILED and no OWNED-by marker).

Cause 1 of 1: CreateTransformJob refuses an S3Prefix input that lists no objects with the right error code but a re-authored message — one short emulator sentence in place of AWS's four-part diagnostic.
Failing check id: s3__sagemaker.sagemaker-batch-transform-nonexistent-s3-input-negative

  AWS:      Smithy (services/service-com-amazonaws-sagemaker-sagemaker/smithy-model/com-amazonaws-sagemaker-sagemaker.json): CreateTransformJob input members TransformJobName, ModelName, TransformInput (-> TransformInput$DataSource -> S3DataSource with S3DataType and S3Uri), TransformOutput and TransformResources are required; modeled errors ResourceInUse, ResourceLimitExceeded and ResourceNotFound (all httpError 400). The missing-prefix refusal itself is recorded behavior: SageMaker lists the input prefix synchronously in the CreateTransformJob call and, when nothing is under it, refuses with HTTP 400 and error code ValidationException, creating no job (ListTransformJobs(NameContains=<name>) stays empty) and no output objects. Its message is: 'No S3 objects found under S3 URL "<S3Uri>" given in input data source. Please ensure that the bucket exists in the selected region (<region>), that objects exist under that S3 prefix, and that the role "<RoleArn>" has "s3:ListBucket" permissions on bucket "<bucket>".' The fixed text of that template is everything outside the four substitutions: 'No S3 objects found under S3 URL "', '" given in input data source. Please ensure that the bucket exists in the selected region (', '), that objects exist under that S3 prefix, and that the role "', '" has "s3:ListBucket" permissions on bucket "', '".'. The four variable parts are taken from the request and from state: <S3Uri> is TransformInput.DataSource.S3DataSource.S3Uri verbatim, <region> is the region the call was made in, <RoleArn> is the execution role of the named Model, and <bucket> is the bucket parsed out of that S3Uri.
  Emulator: the status and the error code are right (400 / ValidationException) and no job is created, but the message is 'The S3 location "<S3Uri>" does not exist or has no objects.' — emulator-authored wording. It names only the S3 URI, and omits all of AWS's other content: the region, the model's role ARN, the bucket, and the 's3:ListBucket' hint that tells the caller the second thing this refusal can mean.
  Why:      This refusal is the one place AWS tells a caller that "nothing under the prefix" and "the role cannot list the bucket" are indistinguishable from the service's side, and it hands back the region, role ARN and bucket needed to tell them apart. A client that matches on AWS's wording (retry/remediation branches, CI assertions, operator-facing surfacing) does not match the emulator's string, and one that parses the role ARN or bucket out of the message to drive its diagnosis finds neither.
  Fix:      CreateTransformJob's input validation and refusal composition (services/service-com-amazonaws-sagemaker-sagemaker/src/ops/create_transform_job.rs) — the ValidationException raised by the synchronous S3Prefix listing must carry AWS's template, interpolating the request's S3Uri, the call's region, the named Model's execution role ARN and the bucket from the S3 URI. Fixed issue 20261110T153718Z-s3-sagemaker-createtransformjob-does-not-list-transforminput-s.md added the synchronous prefix listing and this refusal; the message text it now answers is not AWS's, which is this net-new cause (the call is correctly refused here — AWS refuses it too — so this is not a re-refusal of a call that must succeed).
  Traces:   s3__sagemaker.sagemaker-batch-transform-nonexistent-s3-input-negative

## Evidence
Failing check s3__sagemaker.sagemaker-batch-transform-nonexistent-s3-input-negative [verdict: mismatch].

The check's positive control passes: a transform job over an object that does exist completes and writes transform-output-real/real.csv.out. The error code assertion also passes (ValidationException) and no job is created for the refused request.

Request: sagemaker:CreateTransformJob(TransformJobName=<unique>, ModelName=<model>, TransformInput={DataSource:{S3DataSource:{S3DataType:"S3Prefix", S3Uri:"s3://<bucket>/transform-input/does-not-exist.csv"}}, ContentType:"text/csv", SplitType:"Line"}, TransformOutput={S3OutputPath:"s3://<bucket>/transform-output-missing/", Accept:"text/csv", AssembleWith:"Line"}, TransformResources={InstanceType:"ml.m5.large", InstanceCount:1}) where that key was never uploaded.

Expected (AWS): HTTP 400, Error.Code "ValidationException", Error.Message 'No S3 objects found under S3 URL "s3://<bucket>/transform-input/does-not-exist.csv" given in input data source. Please ensure that the bucket exists in the selected region (<region>), that objects exist under that S3 prefix, and that the role "<model execution role arn>" has "s3:ListBucket" permissions on bucket "<bucket>".'

Actual (emulator): HTTP 400, Error.Code "ValidationException", Error.Message 'The S3 location "s3://<bucket>/transform-input/does-not-exist.csv" does not exist or has no objects.' Replay assertion: AssertionError: expected the missing-object validation message, got: 'The S3 location "s3://sim2real-sm-s3-<id>/transform-input/does-not-exist.csv" does not exist or has no objects.' (the bucket/object names in that line are this run's generated values, not part of the requirement).

## Steps to Reproduce
1. git_clone() the project and boot the emulator built from the checkout.
2. s3:CreateBucket <bucket>; iam:CreateRole trusting sagemaker.amazonaws.com with s3:GetObject/s3:ListBucket on the bucket and s3:PutObject on its objects.
3. Train a model and sagemaker:CreateModel so a transform-capable Model exists with that execution role.
4. s3:PutObject(Bucket=<bucket>, Key="transform-input/real.csv", Body=<two csv rows>); run CreateTransformJob over s3://<bucket>/transform-input/real.csv and confirm it Completes and writes transform-output-real/real.csv.out (positive control).
5. sagemaker:CreateTransformJob with TransformInput.DataSource.S3DataSource = {S3DataType:"S3Prefix", S3Uri:"s3://<bucket>/transform-input/does-not-exist.csv"} — a key never uploaded.
6. Observe Error.Code ValidationException (correct) but Error.Message 'The S3 location "<S3Uri>" does not exist or has no objects.' instead of AWS's 'No S3 objects found under S3 URL "<S3Uri>" given in input data source. Please ensure that the bucket exists in the selected region (<region>), that objects exist under that S3 prefix, and that the role "<RoleArn>" has "s3:ListBucket" permissions on bucket "<bucket>".'
7. Confirm (both on AWS and the emulator) that ListTransformJobs(NameContains=<name>) is empty and no transform-output-missing/ object exists.
8. Re-replay with: s3__sagemaker.sagemaker-batch-transform-nonexistent-s3-input-negative
