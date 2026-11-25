Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__iam__imagebuilder.ibp-e2e-ec2-image-builder-pipeline-lifecycle
Issue-Key: ec2__iam__imagebuilder.ibp-e2e-ec2-image-builder-pipeline-lifecycle|phase#12|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: e1a20d446b2578340370dda6b22d020378cd7235

# Image Builder GetDistributionConfiguration not-found message now uses the resource-labelled template where the behavioral recording requires AWS's bare-ARN wording

> _Note: this issue was identified against the code base at commit `e1a20d446b2578340370dda6b22d020378cd7235`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDistributionConfiguration

## Description
This issue carries ONE net-new root cause (Cause 1 of 1) in the Image Builder emulator's not-found refusal message on the distribution-configuration read path, observed on the behavioral corpus (simparity-behavioral-aws-v1). The replay marked this failure as owned by nothing.

CONFLICT TO RECONCILE, stated up front: open issue 20261118T174922Z-imagebuilder-image-builder-distribution-configuration-not-found-refusal-uses.md (and the resolved 20261117T031907Z-imagebuilder-getdistributionconfiguration-on-a-deleted-distribution-configuration-returns.md, 20261114T230016Z-imagebuilder-2-net-new-refusal-message-divergences-distribution.md, 20261115T021705Z-imagebuilder-image-builder-teardown-paths-diverge-legitimate-deletes-refused.md) assert the OPPOSITE requirement for this exact operation: that AWS answers the resource-labelled template and the emulator's bare-ARN text is the defect. The emulator now serves the resource-labelled template, and this behavioral recording fails on it. Those issues do not cover this check (they carry wire fixture keys / other check ids, not this one), and one of them is still open, so this is filed with its own grounding. A fixer must reconcile the two recordings before changing wording again — flipping the formatter back blind will re-break whatever those issues were graded on. Hypothesis (neither model nor either recording states it): "The following required resource '<Type>' cannot be found: '<arn>'." is AWS's wording when a resource is validated as a REQUIRED REFERENCE of another request (e.g. a pipeline/image create naming a missing distribution configuration), while a distribution configuration's or infrastructure configuration's OWN Get* answers the bare-ARN wording; if so both recordings are right and the formatter must be chosen per call site rather than per resource type.

Cause 1 of 1: GetDistributionConfiguration on a distribution-configuration ARN that does not exist (here, one seeded as residue and then removed by the check's scoped reset) is refused with the right status and error code but the wrong message text.

AWS: recorded behavior only for the message text. Smithy (services/service-com-amazonaws-imagebuilder-imagebuilder/smithy-model/com-amazonaws-imagebuilder-imagebuilder.json): GetDistributionConfiguration is GET /GetDistributionConfiguration, success 200, input member distributionConfigurationArn (required, httpQuery, pattern ^arn:aws[^:]*:imagebuilder:[^:]+:(?:[0-9]{12}|aws):distribution-configuration/[a-z0-9-_]+$); its bound errors are CallRateLimitExceededException 429, ClientException 400, ForbiddenException 403, InvalidRequestException 400, ServiceException 500, ServiceUnavailableException 503 — ResourceNotFoundException is NOT bound to the operation, so the model says nothing about this refusal. The behavioral recording requires error code ResourceNotFoundException with message "The resource ARN '<distributionConfigurationArn>' was not found." — fixed text "The resource ARN '" ... "' was not found.", whose ONLY variable part is the distribution-configuration ARN echoed from the request. The same recording states that this bare-ARN wording is shared by GetInfrastructureConfiguration, while Component, ImageRecipe, ImagePipeline and Image not-found refusals use the resource-labelled template with their own label. The region, 12-digit account id and generated configuration name inside the recorded ARN are per-run values filling that template, not part of the requirement.

Emulator: right error code (ResourceNotFoundException) but message "The following required resource 'DistributionConfiguration' cannot be found: '<distributionConfigurationArn>'." — fixed text carrying the 'DistributionConfiguration' label, with the ARN as its only variable part.

Why: the error message is a requirement. A client, deployment tool or operator that matches AWS's fixed not-found text (or distinguishes an own-resource read miss from a missing required reference by that wording) sees a mismatch on an otherwise correct refusal, and text produced against the emulator is not portable back to real AWS. In this check the mismatch also aborts a six-phase Image Builder lifecycle at phase 2 of 6 (pre_invoke), so everything the later phases grade — the scoped reset's convergence, the bystander-scoping assertion, the pipeline solve, the in-flight-build cancel and the IAM/stack teardown — is never exercised.

Fix: the not-found branch of the GetDistributionConfiguration handler in the imagebuilder service crate and the shared not-found message formatter it calls: the distribution-configuration (and infrastructure-configuration) own-read miss must emit the bare-ARN template, while the resource-labelled template stays for the resource kinds that use it.

Traces: ec2__iam__imagebuilder.ibp-e2e-ec2-image-builder-pipeline-lifecycle (phase 2/6 pre_invoke; phase 1/6 deploy of the build-network stack passed).

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout.

[mismatch] ec2__iam__imagebuilder.ibp-e2e-ec2-image-builder-pipeline-lifecycle
  AssertionError: [phase 2/6 pre_invoke] the reset left residue dist 'arn:aws:imagebuilder:<region>:<account>:distribution-configuration/<name>-residue' resolvable (got ('ResourceNotFoundException', "The following required resource 'DistributionConfiguration' cannot be found: '<same arn>'."))

The check's probe asserts the (code, message) pair the resource is absent WITH, via _absent()/_ib_not_found():
  Expected (AWS): code ResourceNotFoundException, message = "The resource ARN '" + <distributionConfigurationArn from the request> + "' was not found."
  Actual (emulator): code ResourceNotFoundException, message = "The following required resource 'DistributionConfiguration' cannot be found: '" + <same ARN> + "'."

The resource IS gone (the call is refused, not answered), and the code agrees; the divergence is entirely the fixed text of the message. The ARN is the only variable part of either template; its region, account id and generated configuration name are per-run values and are not part of the requirement.

Note the direct contradiction with open issue 20261118T174922Z (wire corpus, fixture imagebuilder-distribution-config-multi-region#teardown#6), which records the labelled template as AWS's answer for the same operation.

## Steps to Reproduce
1. Build the emulator from this checkout and boot it.
2. imagebuilder CreateDistributionConfiguration(name=<n>, distributions=[{region: <region>, amiDistributionConfiguration: {name: ...}}]).
3. DeleteDistributionConfiguration(distributionConfigurationArn=<that arn>).
4. GetDistributionConfiguration(distributionConfigurationArn=<that same arn>).
5. Observe the refusal: code ResourceNotFoundException with "The following required resource 'DistributionConfiguration' cannot be found: '<arn>'." where this recording requires "The resource ARN '<arn>' was not found." (same check: GetInfrastructureConfiguration on a dead infrastructure-configuration ARN must use the bare-ARN wording too, while Component/ImageRecipe/ImagePipeline/Image keep the labelled template).
6. Reconcile against open issue 20261118T174922Z before changing the formatter, then re-replay ec2__iam__imagebuilder.ibp-e2e-ec2-image-builder-pipeline-lifecycle, which must get past phase 2/6 pre_invoke.
