Issue-Source: trace-sweep-divergence
Issue-Trace: imagebuilder-create-image-builds-an-available-ami
Issue-Step: task#20
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: imagebuilder-create-image-builds-an-available-ami imagebuilder-ibp-traj-infra-config-before-instance-profile
Issue-Shape-Id: com.amazonaws.imagebuilder#imagebuilder
Issue-Key: imagebuilder|GetImage|2xx->2xx|$.image.executionId
Issue-Key: imagebuilder|GetImage|2xx->2xx|$.image.executionRole
Issue-Key: imagebuilder|GetImage|2xx->2xx|$.image.imageRecipe.parentImage
Issue-Key: imagebuilder|GetImage|2xx->2xx|$.image.osVersion
Issue-Key: imagebuilder|GetImage|2xx->2xx|$.image.workflows
Issue-Key: imagebuilder|CreateInfrastructureConfiguration|4xx->4xx|$.message
Issue-Commit: e014230fb9b53a8f1acc538e4bd0b4dbf598fc56

# Image Builder GetImage omits modeled Image fields and re-words the instance-profile refusal

> _Note: this issue was identified against the code base at commit `e014230fb9b53a8f1acc538e4bd0b4dbf598fc56`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetImage

## Description
This bundle carries TWO distinct causes in the Image Builder emulator. Both must be closed before this issue is submitted.

(Related but NOT covering: open issue 20261114T230016Z-imagebuilder-2-net-new-refusal-message-divergences-distribution.md describes the same wording fault as Cause 2 below, but it is grounded in the BEHAVIORAL corpus and carries no wire replay key, so the wire fixture step named here is not covered by it. Open issues 20261115T043558Z-ec2-iam-imagebuilder-image-builder-image-builds-never-run-startimagepipelineexecution-lands.md and 20261114T011705Z-ec2-iam-imagebuilder-implement-imagebuilder-listimages-the-operation-answers-501-notimplemented.md touch the same image-build path but neither carries this GetImage step.)

Cause 1 of 2: GetImage returns an Image object that is missing the workflow/OS/execution members AWS populates for a built image, and reports the recipe's parent image as a wildcard version instead of the resolved one.
AWS: Smithy (com.amazonaws.imagebuilder) GetImage -> GetImageResponse.image : Image. The Image structure models member `osVersion` (target OsVersion, length min 1, doc "The operating system version for instances that launch from this image"), `workflows` (target WorkflowConfigurationList of WorkflowConfiguration{workflowArn required, parameters, parallelGroup, onFailure}, doc "The build, test, and distribution workflow configurations that are associated with the image"), `executionRole` (target RoleNameOrArn, length 1..2048, pattern ^(?:arn:aws(?:-[a-z]+)*:iam::[0-9]{12}:role/)?[a-zA-Z_0-9+=,.@\\-_/]+$, doc "The name or Amazon Resource Name (ARN) for the IAM role you create that grants Image Builder access to perform workflow actions") and `imageRecipe` : ImageRecipe, whose `parentImage` member (NonEmptyString, length 1..1024) is documented as "The base image for customizations specified in the image recipe" and may be an AMI ID, an Image Builder image ARN, an SSM parameter or a Marketplace product id. For `image.executionId` the on-disk Image shape lists no such member, so that one is recorded behavior only: the recording shows it present on a completed build (its value is not asserted, only its presence). Behavior AWS shows for a build that reached AVAILABLE: osVersion carries the resolved operating-system version of the parent image; workflows carries one entry per workflow the build ran (the service-owned build and test workflow ARNs, each a fully-versioned workflow ARN matching WorkflowConfiguration.workflowArn's pattern); executionRole carries the role Image Builder assumed (the service-linked role when the caller set none); executionId carries the id of the build execution; and imageRecipe.parentImage is the RESOLVED parent-image reference the build actually used, i.e. a concrete version, not the `x.x.x` wildcard the recipe may have been created with.
Emulator: for the same completed build, GetImage answers 200 with image.osVersion absent, image.workflows absent, image.executionId absent, image.executionRole absent, and image.imageRecipe.parentImage echoed back as the wildcard-versioned ARN form (`.../amazon-linux-2023-x86/x.x.x`) rather than the resolved concrete version the build ran against. Status, and every other asserted member of the response, matched.
Why: a client that drives an Image Builder build and then reads the image back cannot see what ran or on what. Anything keyed on image.workflows (which build/test workflows executed, used to correlate workflow executions and to decide whether a test workflow ran at all) sees an empty image; anything keyed on image.executionId/executionRole (log correlation, IAM troubleshooting) has nothing to key on; osVersion is the documented way to learn the built OS without launching an instance; and a consumer that resolves imageRecipe.parentImage to pin or audit the base image is handed a wildcard that identifies no image version, so pinning and provenance both break.
Fix: the GetImage read path in the imagebuilder service crate — the Image projection/serializer that builds GetImageResponse.image from stored image state, plus the image state the build path records (the resolved parent image, the workflow configurations run, the execution id and the execution role must be persisted when the image is created so the read path can emit them).
Traces: imagebuilder-create-image-builds-an-available-ami#task#20.

Cause 2 of 2: CreateInfrastructureConfiguration's unknown-instance-profile refusal uses the emulator's own invalid-parameter prefix instead of AWS's.
AWS: recorded behavior only for the message text (Smithy: CreateInfrastructureConfiguration.instanceProfileName is required, target InstanceProfileNameType, length 1..256, pattern ^[\\w+=,.@-]+$; the model defines com.amazonaws.imagebuilder#InvalidParameterValueException, errorKind client, httpError 400, member `message`, doc "The value that you provided for the specified parameter is invalid."). The recorded refusal is HTTP 400 with error code InvalidParameterValueException and message "Invalid parameter value for instanceProfileName: The provided instance profile does not exist. Please specify a different instance profile and try again." Fixed text: "Invalid parameter value for " + <member name> + ": The provided instance profile does not exist. Please specify a different instance profile and try again." The only variable part is the input member name (here `instanceProfileName`); the profile name the caller supplied does NOT appear in AWS's message.
Emulator: same status 400 and same error code InvalidParameterValueException, but the message reads "The value supplied for parameter 'instanceProfileName' is not valid. The provided instance profile does not exist. Please specify a different instance profile and try again." — the trailing sentence is correct; the leading clause substitutes the emulator's generic "The value supplied for parameter '<member>' is not valid." prefix for AWS's "Invalid parameter value for <member>:" prefix.
Why: the error message is a requirement. A client, SDK retry/branching layer, test harness or operator that matches AWS's fixed refusal text on an otherwise correct 400 sees a mismatch, and prose authored by the emulator is not portable back to real AWS.
Fix: the CreateInfrastructureConfiguration handler's instance-profile existence validation in the imagebuilder service crate — specifically the message template it passes to the InvalidParameterValueException it raises (the same operation's other invalid-parameter refusals should use AWS's prefix too).
Traces: imagebuilder-ibp-traj-infra-config-before-instance-profile#task#1.

## Evidence
Replay of the simparity-wire-aws-v1 wire fixture corpus against the emulator built from this checkout (emulator up, 339 steps, 307 passed).

Cause 1 — fixture imagebuilder-create-image-builds-an-available-ami, step task#20, operation GetImage, verdict mismatch, expected status 200 / actual status 200:
  $.image.osVersion: missing (expected 'Amazon Linux 2023', got None)
  $.image.imageRecipe.parentImage: value_mismatch (expected a resolved, concretely-versioned parent image ARN ('.../amazon-linux-2023-x86/2026.10.30/1' in the recording), got the wildcard form '.../amazon-linux-2023-x86/x.x.x')
  $.image.workflows: missing (expected a list of WorkflowConfiguration entries — in the recording the service-owned build and test workflow ARNs 'arn:aws:imagebuilder:<region>:aws:workflow/build/build-image/<ver>/<build>' and '.../workflow/test/test-image/<ver>/<build>' — got None)
  $.image.executionId: missing (expected present, value not asserted, got None)
  $.image.executionRole: missing (expected the role the build assumed — the Image Builder service-linked role in the recording — got None)

Cause 2 — fixture imagebuilder-ibp-traj-infra-config-before-instance-profile, step task#1, operation CreateInfrastructureConfiguration, verdict mismatch, expected status 400 / actual status 400 (status and error code agree; only the message diverges):
  $.message: value_mismatch
    expected: "Invalid parameter value for instanceProfileName: The provided instance profile does not exist. Please specify a different instance profile and try again."
    got:      "Invalid parameter value for instanceProfileName" prefix absent — "The value supplied for parameter 'instanceProfileName' is not valid. The provided instance profile does not exist. Please specify a different instance profile and try again."

## Steps to Reproduce
1. Build the emulator from this checkout and boot it.
2. Replay the simparity-wire-aws-v1 wire corpus.
3. Cause 1: replay fixture imagebuilder-create-image-builds-an-available-ami and stop at step task#20 — it creates a component, an image recipe (parentImage given with a wildcard version), an infrastructure configuration and an image, waits for the image to reach AVAILABLE, then calls GetImage(imageBuildVersionArn=<the built image version ARN>). Inspect the returned image object for osVersion, workflows, executionId, executionRole and imageRecipe.parentImage.
4. Cause 2: replay fixture imagebuilder-ibp-traj-infra-config-before-instance-profile and stop at step task#1 — CreateInfrastructureConfiguration(name=<name>, instanceProfileName=<a profile no IAM instance profile matches>, clientToken=<uuid>). Confirm the 400/InvalidParameterValueException and inspect the message's leading clause.
5. Re-replay both fixture#step bindings listed in trace_ids (plus the representative above) to confirm both causes are closed.
