Issue-Source: trace-sweep-divergence
Issue-Trace: imagebuilder-create-image-pipeline-rejects-an-invalid-cron-schedule
Issue-Step: teardown#12
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: imagebuilder-create-image-pipeline-rejects-an-invalid-cron-schedule imagebuilder-distribution-config-multi-region
Issue-Shape-Id: com.amazonaws.imagebuilder#imagebuilder
Issue-Key: imagebuilder|DeleteInfrastructureConfiguration|2xx->4xx|status
Issue-Key: imagebuilder|DeleteImageRecipe|2xx->4xx|status
Issue-Key: imagebuilder|DeleteComponent|2xx->4xx|status
Issue-Key: imagebuilder|GetDistributionConfiguration|4xx->4xx|$.message
Issue-Commit: ecb7c9e75f06daa929a50368dc1947c6627941cd

# Image Builder teardown paths diverge: legitimate deletes refused 400 and distribution-configuration not-found wording

> _Note: this issue was identified against the code base at commit `ecb7c9e75f06daa929a50368dc1947c6627941cd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteInfrastructureConfiguration

## Description
This bundle carries FOUR distinct causes in the Image Builder emulator, all on the teardown/clean-up path of a recorded fixture. ALL FOUR must be closed before this issue is submitted, and every fixture#step listed in Issue-Trace / Issue-Step / Issue-Trace-Ids must be re-replayed to confirm it.

Causes 1-3 share one root (a resource the emulator holds that AWS never created) but are three separate wire clusters on three different operations; cause 4 is an independent message-wording defect.

(Related but NOT covering - none of these carry the wire replay keys above: open issue 20261103T104956Z-imagebuilder-4-net-new-wire-divergences-on-image.md and 20261103T041241Z-ec2-iam-imagebuilder-image-builder-createimagepipeline-accepts-an-invalid-cron-schedule.md own the CreateImagePipeline invalid-cron acceptance itself (task#7/task#8 of the same fixture), not the three delete refusals below; 20261114T230016Z-imagebuilder-2-net-new-refusal-message-divergences-distribution.md carries the same distribution-configuration wording cause but only as BEHAVIORAL check ids from simparity-behavioral-aws-v1, with no wire fixture step, so the wire cluster below is unowned; 20261105T162014Z-imagebuilder-image-builder-refusal-error-types-and-messages-are.md and 20261107T064925Z-imagebuilder-3-net-new-refusal-message-divergences-resourcealreadyexistsexception.md cover other refusal templates; 20261103T091826Z-imagebuilder-2-net-new-delete-path-divergences-deleteinfrastructureconfiguration.md and 20261105T163213Z-ec2-iam-imagebuilder-image-builder-deleteimagerecipe-ignores-the-image-pipeline-dependency.md describe the OPPOSITE defect - a delete that does NOT enforce the dependency - not a delete that refuses when no dependency exists.)

Cause 1 of 4: DeleteInfrastructureConfiguration refuses a configuration no pipeline references.
AWS: Smithy: DeleteInfrastructureConfiguration, http DELETE /DeleteInfrastructureConfiguration, success code 200, output {requestId, infrastructureConfigurationArn}; modelled errors CallRateLimitExceededException(429), ClientException(400), ForbiddenException(403), InvalidRequestException(400), ResourceDependencyException(400, "You have attempted to mutate or delete a resource with a dependency that prohibits this action. See the error message for more details."), ServiceException(500), ServiceUnavailableException(503). A delete of an existing configuration that NO image pipeline references succeeds with HTTP 200 and echoes the deleted ARN. In the recording the preceding CreateImagePipeline (whose schedule expression was invalid) was refused by AWS with HTTP 400, so no pipeline ever referenced this configuration and the delete is a plain success.
Emulator: HTTP 400 instead of 200 on the same request.
Hypothesis: the handler's dependency pre-check (services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/delete_infrastructure_configuration.rs, the pipeline_depending_on_infrastructure_configuration branch raising ResourceDependencyException) is itself correct, but the emulator ACCEPTED the invalid-schedule CreateImagePipeline earlier in the same fixture with HTTP 200, so its state holds a pipeline AWS never created, and that phantom pipeline makes the dependency check fire. The replay asserts only the status here, so the error code is not captured in the divergence.
Why: a client that creates a pipeline with a bad schedule, is told it failed, and then cleans up its infrastructure configuration gets a hard 400 from the emulator and cannot delete a resource AWS lets it delete; teardown leaks state and every later run of the same fixture starts dirty.
Fix: CreateImagePipeline's schedule-expression validation in the imagebuilder service crate (the create handler's input validation), so a refused create writes nothing into the pipeline store; the delete handler's dependency check and its 200 path need no change. Confirm by re-replaying this step, not only the create step.
Traces: imagebuilder-create-image-pipeline-rejects-an-invalid-cron-schedule#teardown#12 (status: expected 200, got 400). Code: services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/delete_infrastructure_configuration.rs lines 211-227.

Cause 2 of 4: DeleteImageRecipe refuses a recipe no pipeline references.
AWS: Smithy: DeleteImageRecipe, http DELETE /DeleteImageRecipe, success code 200, output {requestId, imageRecipeArn}; modelled errors as above including ResourceDependencyException(400). With no pipeline referencing the recipe, the delete succeeds with HTTP 200.
Emulator: HTTP 400 instead of 200.
Hypothesis: same root as cause 1 - the phantom pipeline accepted by CreateImagePipeline still references this image recipe, so the recipe delete's dependency check refuses it.
Why: same as cause 1 - an unbounded 400 on a legitimate delete; the recipe cannot be removed and the component beneath it stays pinned.
Fix: as cause 1 (CreateImagePipeline validation must not persist a refused pipeline). The DeleteImageRecipe dependency check itself is per AWS.
Traces: imagebuilder-create-image-pipeline-rejects-an-invalid-cron-schedule#teardown#13 (status: expected 200, got 400).

Cause 3 of 4: DeleteComponent refuses a component build version no stored recipe reaches.
AWS: Smithy: DeleteComponent, http DELETE /DeleteComponent, success code 200, output {requestId, componentBuildVersionArn}; doc "Deletes a component build version. The request fails with ResourceDependencyException if an image recipe or container recipe references this component version." ResourceDependencyException is client, httpError 400. Once the referencing recipe is gone the component delete succeeds with HTTP 200.
Emulator: HTTP 400 instead of 200.
Hypothesis: cascades from cause 2 - the image recipe could not be deleted, so it still references this component build version and DeleteComponent's recipe_depending_on_component check refuses.
Why: the whole three-step teardown (infrastructure configuration -> image recipe -> component) is unavailable after any refused pipeline create, so a client cannot release its Image Builder resources at all.
Fix: as causes 1-2. The DeleteComponent dependency check matches the modelled contract.
Traces: imagebuilder-create-image-pipeline-rejects-an-invalid-cron-schedule#teardown#14 (status: expected 200, got 400). Code: services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/delete_component.rs lines 215-232.

Cause 4 of 4: GetDistributionConfiguration's not-found refusal uses the generic ARN template instead of AWS's resource-labelled one.
AWS: recorded behavior only for the message (the model does not bind a not-found error to GetDistributionConfiguration; it defines the shape com.amazonaws.imagebuilder#ResourceNotFoundException, client, httpError 404, member message, doc "At least one of the resources referenced by your request does not exist."). The recorded refusal is HTTP 404 with message "The following required resource 'DistributionConfiguration' cannot be found: '<distributionConfigurationArn>'." The FIXED text is "The following required resource 'DistributionConfiguration' cannot be found: '" ... "'."; the ONLY variable part is the distribution-configuration ARN taken from the request. (The ARN in the recording is one run's generated value and is not part of the template.)
Emulator: correct status 404 and correct error class, but the message is "The resource ARN '<distributionConfigurationArn>' was not found." - the generic ARN template (crate::state::resource_arn_not_found_message) applied to the distribution-configuration read path, where the component/recipe paths already use the resource-labelled template (crate::state::required_resource_not_found_message).
Why: the error message is a requirement. A client or operator reading AWS's resource-labelled not-found text cannot tell WHICH referenced resource is missing from the emulator's wording - AWS names the resource type, the emulator names only the ARN - so dependency resolution and user-facing error reporting diverge even though status and error code agree.
Fix: the lookup-miss branch of the GetDistributionConfiguration handler in the imagebuilder service crate (and the shared not-found message formatter it selects) must emit the resource-labelled 'DistributionConfiguration' template.
Traces: imagebuilder-distribution-config-multi-region#teardown#6 ($.message value_mismatch). Code: services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/get_distribution_configuration.rs lines 185-196 (calls resource_arn_not_found_message); compare services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/delete_component.rs lines 205-213 (calls required_resource_not_found_message).

## Evidence
Replay of the simparity-wire-aws-v1 wire fixture corpus against the emulator built from this checkout (emulator_up=true, 339 steps, 288 passed, score 85.0%).

Cause 1 - fixture imagebuilder-create-image-pipeline-rejects-an-invalid-cron-schedule, step teardown#12, operation DeleteInfrastructureConfiguration, verdict mismatch: "status: expected 200, got 400" (expected_status 200, actual_status 400, affected_count 1). Earlier in the SAME fixture, task#7 CreateImagePipeline (invalid cron "NOT..." schedule expression) recorded 400 but the emulator answered 200, and task#8's valid CreateImagePipeline recorded 200 but the emulator answered 400 - both of those steps are owned by 20261103T104956Z-imagebuilder-4-net-new-wire-divergences-on-image.md, which is why the leftover pipeline record is the likely cause of the three delete refusals.

Cause 2 - same fixture, step teardown#13, operation DeleteImageRecipe, verdict mismatch: "status: expected 200, got 400" (expected 200, actual 400).

Cause 3 - same fixture, step teardown#14, operation DeleteComponent, verdict mismatch: "status: expected 200, got 400" (expected 200, actual 400).

Cause 4 - fixture imagebuilder-distribution-config-multi-region, step teardown#6, operation GetDistributionConfiguration, verdict mismatch, status 404 both sides: "$.message: value_mismatch (expected \"The following required resource 'DistributionConfiguration' cannot be found: '<distributionConfigurationArn>'.\", got \"The resource ARN '<distributionConfigurationArn>' was not found.\")". Only the message text diverges; the status and the error class match.

## Steps to Reproduce
1. Build the emulator from this checkout and boot it.
2. Replay the simparity-wire-aws-v1 wire fixture corpus.
3. Causes 1-3: replay fixture imagebuilder-create-image-pipeline-rejects-an-invalid-cron-schedule. It creates a component, an image recipe and an infrastructure configuration, then calls CreateImagePipeline with an invalid cron schedule expression (recorded 400), then creates a valid pipeline, then tears down. Inspect steps teardown#12 (DeleteInfrastructureConfiguration), teardown#13 (DeleteImageRecipe) and teardown#14 (DeleteComponent): each must answer HTTP 200 and echo the deleted ARN, not HTTP 400.
4. Equivalent minimal repro: create a component, an image recipe referencing it and an infrastructure configuration; call CreateImagePipeline with a schedule.scheduleExpression the service rejects; then DELETE /DeleteInfrastructureConfiguration, /DeleteImageRecipe and /DeleteComponent for those three ARNs - all three must succeed with 200 because the refused create must leave no pipeline behind.
5. Cause 4: replay fixture imagebuilder-distribution-config-multi-region and inspect step teardown#6 - GET /GetDistributionConfiguration?distributionConfigurationArn=<arn of a deleted/unknown distribution configuration> must answer 404 with message "The following required resource 'DistributionConfiguration' cannot be found: '<that arn>'."
6. Re-replay imagebuilder-create-image-pipeline-rejects-an-invalid-cron-schedule#teardown#12, #teardown#13, #teardown#14 and imagebuilder-distribution-config-multi-region#teardown#6 to confirm all four causes are closed.
