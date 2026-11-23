Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: imagebuilder.update-distribution-configuration-persists-new-region
Issue-Shape-Id: com.amazonaws.imagebuilder#imagebuilder
Issue-Key: imagebuilder.update-distribution-configuration-persists-new-region|run#20|AssertionError|assert message == f"The following required resource 'DistributionConfiguration' cannot be found: '{resp['distributionConfigurationArn']}'.", (
Issue-Commit: 76cd101d4aa6a8011f19dfb6a2bd3d37c5bd8e14

# imagebuilder: GetDistributionConfiguration on a deleted distribution configuration returns the generic "resource ARN ... was not found" message instead of AWS's resource-labelled not-found text

> _Note: this issue was identified against the code base at commit `76cd101d4aa6a8011f19dfb6a2bd3d37c5bd8e14`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDistributionConfiguration

## Description
This issue carries ONE net-new root cause (Cause 1 of 1) in the Image Builder emulator's not-found refusal-message formatting for the distribution-configuration read path.

(Related but NOT covering: open issue 20261114T230016Z-imagebuilder-2-net-new-refusal-message-divergences-distribution.md describes the same wording defect, but it does not carry this check's replay key — the suite did not mark this failure as OWNED by it — so this failure is filed here. Also related, and likewise not covering: 20261106T010945Z-imagebuilder-2-net-new-distribution-configuration-refusal-divergences.md.)

Cause 1 of 1: GetDistributionConfiguration for a distribution-configuration ARN that no longer exists (it was just deleted by DeleteDistributionConfiguration) is refused with the correct error code and status but with the emulator's generic ARN-only not-found message instead of AWS's resource-labelled not-found template.
AWS: recorded behavior only for the message text — the Smithy model does not bind ResourceNotFoundException to GetDistributionConfiguration (modeled errors for that operation are CallRateLimitExceededException 429, ClientException 400, ForbiddenException 403, InvalidRequestException 400, ServiceException 500, ServiceUnavailableException 503); the model does define com.amazonaws.imagebuilder#ResourceNotFoundException, errorKind client, httpError 404, member `message`, doc "At least one of the resources referenced by your request does not exist." The recording shows the refusal as HTTP 404, error code ResourceNotFoundException, message "The following required resource 'DistributionConfiguration' cannot be found: '<distributionConfigurationArn>'." The fixed text of that message is "The following required resource 'DistributionConfiguration' cannot be found: '" ... "'."; its only variable part is the distribution-configuration ARN taken verbatim from the request (the recording's concrete ARN value is an example, not part of the requirement). The same resource-labelled template is used for other Image Builder resource types with their own label (e.g. 'ImageRecipe'), so the resource type name is part of the template.
Emulator: returns the correct error code ResourceNotFoundException, but the message is "The resource ARN '<distributionConfigurationArn>' was not found." — a generic ARN-only template with no resource-type label.
Why: the error message is a requirement. A client, SDK retry/branching layer, deployment tool or operator that reads AWS's text to learn WHICH referenced resource is missing gets no resource type from the emulator; code that matches or parses AWS's fixed text sees a mismatch on an otherwise correct 404, and messages produced against the emulator are not portable back to real AWS.
Fix: the not-found branch of the GetDistributionConfiguration handler in the imagebuilder service crate (its lookup-miss path and the shared not-found message formatter it calls) must emit the resource-labelled template carrying the 'DistributionConfiguration' label rather than the generic ARN-only template.
Traces: imagebuilder.update-distribution-configuration-persists-new-region — the rest of that check passed: CreateDistributionConfiguration, GetDistributionConfiguration right after create (single region), UpdateDistributionConfiguration adding a second region, GetDistributionConfiguration reflecting both regions, DeleteDistributionConfiguration, and the ResourceNotFoundException *code* on the post-delete read all behaved correctly; only the refusal message diverged.

## Evidence
Replay of the simparity-behavioral-aws-v1 behavioral suite against the emulator built from this checkout.

[mismatch] imagebuilder.update-distribution-configuration-persists-new-region, at check line 66:
  assert message == f"The following required resource 'DistributionConfiguration' cannot be found: '{resp['distributionConfigurationArn']}'."
  AssertionError: expected the exact ResourceNotFoundException message AWS returns, got "The resource ARN 'arn:aws:imagebuilder:<region>:<account>:distribution-configuration/<name>' was not found."

Expected (AWS): HTTP 404, code ResourceNotFoundException, message fixed text "The following required resource 'DistributionConfiguration' cannot be found: '" + <distributionConfigurationArn from the request> + "'."
Actual (emulator): code ResourceNotFoundException, message fixed text "The resource ARN '" + <distributionConfigurationArn from the request> + "' was not found."

The variable part in both is the distribution-configuration ARN echoed from the request; the account id, region, generated configuration name and request id in the recorded line are per-run values and not part of the requirement.

## Steps to Reproduce
1. Build the emulator from this checkout and boot it.
2. Replay the simparity-behavioral-aws-v1 behavioral suite.
3. Run check imagebuilder.update-distribution-configuration-persists-new-region: CreateDistributionConfiguration(name=<unique>, distributions=[{region: <ctx.region>, amiDistributionConfiguration:{name: ...}}], clientToken=<uuid>); GetDistributionConfiguration on the returned ARN; UpdateDistributionConfiguration with a second region; GetDistributionConfiguration again; DeleteDistributionConfiguration; then GetDistributionConfiguration on the deleted ARN.
4. Inspect the final refusal: the code is ResourceNotFoundException (correct) but the message is "The resource ARN '<arn>' was not found." instead of "The following required resource 'DistributionConfiguration' cannot be found: '<arn>'."
5. Re-replay imagebuilder.update-distribution-configuration-persists-new-region to confirm the fix.
