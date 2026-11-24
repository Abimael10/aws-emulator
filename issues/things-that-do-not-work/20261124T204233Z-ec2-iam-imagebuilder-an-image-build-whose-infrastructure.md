Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__iam__imagebuilder.ibp-image-build-version-arn-derives-from-the-recipe-and-counts-across-pipelines
Issue-Key: ec2__iam__imagebuilder.ibp-image-build-version-arn-derives-from-the-recipe-and-counts-across-pipelines|run#13|AssertionError|assert state["status"] == "FAILED", f"{img} ended {state!r}"
Issue-Commit: 51580daa099a6a502e147b76b77645918dba2576

# ec2__iam__imagebuilder: an image build whose infrastructure configuration names a nonexistent security group ends AVAILABLE instead of FAILED

> _Note: this issue was identified against the code base at commit `51580daa099a6a502e147b76b77645918dba2576`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartImagePipelineExecution

## Description
One net-new root cause.

--- Cause 1 of 1: an Image Builder image build version reaches a terminal status of AVAILABLE where AWS ends it FAILED ---
Failing check: ec2__iam__imagebuilder.ibp-image-build-version-arn-derives-from-the-recipe-and-counts-across-pipelines.
AWS: recorded behavior only — Image Builder's model carries the image state as a status enum and does not state which builds fail, so the terminal status is taken from the recording. The recording builds two pipelines over one image recipe whose infrastructure configuration names a security group id that does not exist in the account, starts each pipeline, and then polls each image build version to a terminal status. In the recording both build versions end with status `FAILED`: a build whose infrastructure configuration cannot be realised (here, a nonexistent securityGroupIds entry) cannot produce an image, so Image Builder fails the build version rather than publishing one. The earlier legs of the same check pin the surrounding contract, which the emulator does satisfy: the build version ARN is derived from the RECIPE and not from the pipeline (arn:aws:imagebuilder:<region>:<account>:image/<recipe-name>/<version>/<build-number>), the build number counts across every pipeline sharing that recipe (so two pipelines started in turn yield build 1 then build 2), each pipeline's ListImagePipelineImages lists only its own build version, and GetImagePipeline reports dateLastRun after a start.
Emulator: each image build version reaches terminal status `AVAILABLE`. The replay reports the build version arn:aws:imagebuilder:<region>:<account>:image/<recipe>/1.0.0/1 ending {'status': 'AVAILABLE'} where FAILED was required — i.e. the build is published as a usable image even though the infrastructure it was told to build on does not exist.
Why: a client cannot distinguish a build that worked from one that could not run. Code that branches on the terminal status — surfacing the failure reason, retrying with a corrected infrastructure configuration, or refusing to distribute an image that never built — takes the success branch against the emulator and the failure branch against AWS. Downstream consumers are then handed an image ARN whose AMI does not exist, so the divergence propagates past Image Builder into whatever consumes the image.
Fix: the image-build execution/state machine behind StartImagePipelineExecution in the imagebuilder service (services/service-com-amazonaws-imagebuilder-imagebuilder) — the build must validate the referenced infrastructure configuration's network resources (here securityGroupIds) and drive the image build version to FAILED with a failure reason when they cannot be resolved, instead of settling on AVAILABLE.
Note on existing issues: the open issue 20261115T043558Z-ec2-iam-imagebuilder-image-builds-never-run-startimagepipelineexecution-lands.md covers builds landing straight on AVAILABLE and is closely related; it does not carry this check's replay key, so this finding is filed separately and should be closed together with it if a single fix covers both.

## Evidence
Expected (AWS, from the recording): both image build versions created over a recipe whose infrastructure configuration names a nonexistent security group reach terminal status `FAILED`.
Actual (emulator): AssertionError: arn:aws:imagebuilder:us-west-2:000000000000:image/sim2real-ibp-48e64159d648/1.0.0/1 ended {'status': 'AVAILABLE'}.
The region, account, recipe name and build version in that ARN are per-run values; the requirement is the terminal status, not the ARN text. The ARN shape itself (image/<recipe-name>/<version>/<build-number>, numbered across pipelines sharing the recipe), the per-pipeline ListImagePipelineImages listing and GetImagePipeline.dateLastRun were all satisfied before this assertion and must keep passing.

## Steps to Reproduce
Run the behavioral suite (suite simparity-behavioral-aws-v1) and replay ec2__iam__imagebuilder.ibp-image-build-version-arn-derives-from-the-recipe-and-counts-across-pipelines.
1. Create an Image Builder component and an image recipe over it.
2. Create an infrastructure configuration whose securityGroupIds names a security group id that does not exist in the account (the check uses sg-0123456789abcdef0), with an instance profile and the run's subnet.
3. Create two image pipelines over that same recipe and infrastructure configuration.
4. StartImagePipelineExecution on each pipeline in turn; note the two returned image build version ARNs (.../1 and .../2).
5. Poll GetImage on each build version until its status is terminal. AWS ends each one `FAILED`; the emulator ends each one `AVAILABLE`.
