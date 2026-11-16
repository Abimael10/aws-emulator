Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: imagebuilder.create-image-full-build-lifecycle imagebuilder.ibp-parent-image-wildcard-resolves-to-a-dated-amazon-linux-2-build imagebuilder.retry-image-on-active-build-rejected
Issue-Shape-Id: com.amazonaws.imagebuilder#imagebuilder
Issue-Key: imagebuilder.create-image-full-build-lifecycle|run#14|AssertionError|assert len(amis) >= 1, f"expected at least one output AMI, got {amis!r}"
Issue-Key: imagebuilder.ibp-parent-image-wildcard-resolves-to-a-dated-amazon-linux-2-build|run#12|AssertionError|assert [a["region"] for a in amis] == [ctx.region], (
Issue-Key: imagebuilder.retry-image-on-active-build-rejected|run#17|AssertionError|assert code == "InvalidParameterValueException", f"expected InvalidParameterValueException, got {code!r}"
Issue-Commit: bcfb02b63b9a0fdb9bee01e1437bdea83d16d1e1

# Image Builder: CreateImage builds produce no output AMI, AWS-managed parent images carry no outputResources.amis, and RetryImage answers 501 NotImplemented

> _Note: this issue was identified against the code base at commit `bcfb02b63b9a0fdb9bee01e1437bdea83d16d1e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateImage

## Description
3 net-new root causes in the Image Builder image build/read path and in RetryImage, all reached through the `imagebuilder` behavioral checks. None of the open imagebuilder issues carries these checks' replay keys: 20261115T043558Z-ec2-iam-imagebuilder-image-builder-image-builds-never-run-startimagepipelineexecution-lands.md is bound to the three ec2__iam__imagebuilder pipeline checks (StartImagePipelineExecution state machine and a KeyError on a missing outputResources member) and overlaps Cause 2 below in subject matter — it does not cover the CreateImage path, the empty-list form, or RetryImage; 20261114T011705Z-...-implement-imagebuilder-listimages covers ListImages; the distribution-configuration / teardown / refusal-wording issues (20261105T162014Z, 20261106T010945Z, 20261107T064925Z, 20261114T230016Z, 20261115T021705Z) are about other operations; no open issue mentions RetryImage at all.

Cause 1 of 3: a user-initiated CreateImage build reaches AVAILABLE but produces no output AMI.
AWS: Smithy com.amazonaws.imagebuilder — Image.outputResources targets OutputResources ("The output resources that Image Builder produces for this image"), whose `amis` member is an AmiList documented as "The Amazon EC2 AMIs created by this image. The list contains one entry per AMI, including copies that distribution created in each target Amazon Web Services Region and account"; each Ami carries region, image, name, description, state and accountId. Image.state.status targets the ImageStatus enum (PENDING, CREATING, BUILDING, TESTING, DISTRIBUTING, INTEGRATING, AVAILABLE, CANCELLED, FAILED, DEPRECATED, DELETED, DISABLED). So for an AMI-type image that CreateImage drove to AVAILABLE, GetImage(imageBuildVersionArn) returns image.outputResources.amis with at least one entry, whose `region` is the build Region and whose `image` is an AMI ID of the form `ami-<hex>` that ec2:DescribeImages resolves in that Region with State "available" (recorded behavior for the EC2 cross-check; the Image Builder side is modeled).
Emulator: the build settles on AVAILABLE, and GetImage then returns image.outputResources.amis as an EMPTY list — the build registers no AMI anywhere, so nothing is produced for EC2 to describe either.
Why: a client that calls CreateImage to obtain an AMI gets a build that reports complete success and hands back no artifact. Any code that reads outputResources.amis[0].image to launch, copy, share or deregister the produced AMI fails on an AVAILABLE image, and an operator believes an AMI exists when none does.
Fix: the CreateImage handler and the image-build state it writes (the AMI-producing side of the build lifecycle) plus the GetImage serializer that projects OutputResources — a build that reaches AVAILABLE must have recorded at least one Ami (region, image, name, state, accountId) and registered a corresponding EC2 image.
Traces: imagebuilder.create-image-full-build-lifecycle (AssertionError: expected at least one output AMI, got []).

Cause 2 of 3: GetImage on an AWS-managed parent image resolves the wildcard but serves no output AMI for it.
AWS: Smithy com.amazonaws.imagebuilder — GetImage's imageBuildVersionArn pattern admits the `aws` owner alternative (^arn:aws[^:]*:imagebuilder:[^:]+:(?:[0-9]{12}|aws(?:-[a-z-]+)?|third-party):...image/...), i.e. the AWS-owned catalog images such as arn:aws:imagebuilder:<region>:aws:image/amazon-linux-2-x86/x.x.x. GetImage resolves that wildcard to a concrete dated build and, because the resolved image is an AMI-type image in state AVAILABLE, image.outputResources.amis lists exactly one Ami for the caller's Region whose `image` is an `ami-` ID (recorded behavior for the one-entry/Region scoping; the member and its meaning are modeled as in Cause 1).
Emulator: the resolution itself is right (arn, name "Amazon Linux 2 x86", osVersion "Amazon Linux 2", type AMI, platform Linux, state AVAILABLE all match), but image.outputResources.amis is EMPTY — the region list read off it is [] where AWS reports the caller's Region. (Under the other read path the member is absent entirely, which is what 20261115T043558Z-...-startimagepipelineexecution-lands Cause 3 records; the catalog images served to the `imagebuilder` checks return the member as an empty list instead, and that check id is not carried by any open issue.)
Why: a client cannot resolve an Image Builder parent image to the EC2 AMI it is backed by, so it cannot launch from it, cannot verify which Region it lives in, and cannot assert that a parent image is usable before building on it.
Fix: the AWS-managed image catalog the GetImage handler reads, and the GetImage serializer's OutputResources projection — each catalog image must expose one Ami entry for the Region it is served in.
Traces: imagebuilder.ibp-parent-image-wildcard-resolves-to-a-dated-amazon-linux-2-build (AssertionError: output AMIs by region [], expected ['us-west-2']).

Cause 3 of 3: RetryImage is not implemented — it answers HTTP 501 NotImplemented instead of refusing a retry of a build that is not FAILED/CANCELLED.
AWS: Smithy com.amazonaws.imagebuilder#RetryImage is documented "Retries a failed or canceled image build... The image must be in the FAILED or CANCELLED state" and is modeled with required clientToken and imageBuildVersionArn and errors CallRateLimitExceededException (429), ClientException (400), ForbiddenException (403), IdempotentParameterMismatchException (400), InvalidRequestException (400), ResourceInUseException (400), ServiceException (500), ServiceUnavailableException (503). The recorded refusal for a build in an active state (PENDING/CREATING/BUILDING/TESTING) is HTTP 400 with error code InvalidParameterValueException (a service-common code not enumerated for this operation in the model) and message "The value supplied for parameter 'imageArn' is not valid. The provided image is not in a failed or cancelled state and cannot be retried." — that whole text is fixed for this error class, including the parameter name 'imageArn', which is this operation's parameter and not a value from the request; the rejected retry leaves the build's status untouched.
Emulator: RetryImage returns the scaffold's 501 `NotImplemented` ("operation RetryImage is scaffolded but not implemented in this emulator"), so neither the FAILED/CANCELLED-only rule nor the success path exists.
Why: a client retrying a failed build gets a 501 that no AWS SDK models as retryable or as a validation failure, and a client that wrongly retries an in-flight build is never told why; the "only retryable from FAILED/CANCELLED" invariant is unobservable.
Fix: the RetryImage handler (services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/retry_image.rs is still the generated scaffold whose handle() returns Scaffolded::not_implemented) — implement the operation against the image build state, refusing a non-FAILED/non-CANCELLED build with the 400 InvalidParameterValueException above and otherwise re-running the build version in place without creating a new one.
Traces: imagebuilder.retry-image-on-active-build-rejected (AssertionError: expected InvalidParameterValueException, got 'NotImplemented').

## Evidence
Cause 1 — check imagebuilder.create-image-full-build-lifecycle:
  Expected: after CreateImage(infrastructureConfigurationArn, imageRecipeArn, imageTestsConfiguration={imageTestsEnabled: False}) the build polls to state.status == "AVAILABLE" and GetImage(imageBuildVersionArn)["image"]["outputResources"]["amis"] has >= 1 entry whose "image" starts with "ami-", and ec2:DescribeImages(ImageIds=[that id]) returns exactly that image with State == "available".
  Actual: AssertionError: expected at least one output AMI, got [] — the image is AVAILABLE with an empty outputResources.amis, so the EC2 cross-check is never reachable.

Cause 2 — check imagebuilder.ibp-parent-image-wildcard-resolves-to-a-dated-amazon-linux-2-build:
  Expected: GetImage(imageBuildVersionArn="arn:aws:imagebuilder:<region>:aws:image/amazon-linux-2-x86/x.x.x") returns an image whose arn is a dated build under arn:aws:imagebuilder:<region>:aws:image/amazon-linux-2-x86/, name "Amazon Linux 2 x86", osVersion "Amazon Linux 2", type "AMI", platform "Linux", state.status "AVAILABLE", and outputResources.amis == a one-entry list whose region == the caller's Region and whose image starts with "ami-".
  Actual: AssertionError: output AMIs by region [], expected ['us-west-2'] — every other asserted field matched; outputResources.amis came back empty.

Cause 3 — check imagebuilder.retry-image-on-active-build-rejected:
  Expected: with the CreateImage build in an active status (PENDING/CREATING/BUILDING/TESTING), RetryImage(imageBuildVersionArn, clientToken) is refused HTTP 400, Error.Code "InvalidParameterValueException", Error.Message "The value supplied for parameter 'imageArn' is not valid. The provided image is not in a failed or cancelled state and cannot be retried.", and the build remains in an active status afterwards.
  Actual: AssertionError: expected InvalidParameterValueException, got 'NotImplemented' — RetryImage answers the scaffold 501 ("operation RetryImage is scaffolded but not implemented in this emulator").

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (run_behavioral_suite); look at the ### imagebuilder section.
2. Cause 1: create an IAM role + instance profile, CreateInfrastructureConfiguration over it, CreateImageRecipe with parentImage "arn:aws:imagebuilder:<region>:aws:image/amazon-linux-2-x86/x.x.x", then CreateImage with imageTestsConfiguration={"imageTestsEnabled": false}; poll GetImage to a terminal status (AVAILABLE) and read image.outputResources.amis — observe []. Replay check imagebuilder.create-image-full-build-lifecycle.
3. Cause 2: call GetImage(imageBuildVersionArn="arn:aws:imagebuilder:<region>:aws:image/amazon-linux-2-x86/x.x.x") and read image.outputResources.amis — observe an empty list instead of one entry for <region>. Replay check imagebuilder.ibp-parent-image-wildcard-resolves-to-a-dated-amazon-linux-2-build.
4. Cause 3: with the CreateImage build from step 2 still in an active status, call RetryImage(imageBuildVersionArn=<that arn>, clientToken=<uuid>) — observe HTTP 501 with code NotImplemented instead of HTTP 400 InvalidParameterValueException and the message quoted above. Replay check imagebuilder.retry-image-on-active-build-rejected.
