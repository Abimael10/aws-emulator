Issue-Source: trace-sweep-divergence
Issue-Trace: imagebuilder-distribution-config-multi-region
Issue-Step: teardown#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: imagebuilder-distribution-config-multi-region
Issue-Shape-Id: com.amazonaws.imagebuilder#imagebuilder
Issue-Key: imagebuilder|GetDistributionConfiguration|4xx->4xx|$.message
Issue-Commit: 2586875af58cfe37da2b21b068b5f114b257c74a

# Image Builder distribution-configuration not-found refusal uses the generic ARN-only message instead of AWS's resource-labelled template

> _Note: this issue was identified against the code base at commit `2586875af58cfe37da2b21b068b5f114b257c74a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDistributionConfiguration

## Description
This issue carries ONE net-new root cause (Cause 1 of 1) in the Image Builder emulator's not-found refusal-message formatting on the distribution-configuration read path, observed on the WIRE corpus (simparity-wire-aws-v1).

(Related but NOT covering: open issues 20261117T031907Z-imagebuilder-getdistributionconfiguration-on-a-deleted-distribution-configuration-returns.md and 20261114T230016Z-imagebuilder-2-net-new-refusal-message-divergences-distribution.md describe the same wording defect, but both are behavioral-corpus issues keyed on behavioral check ids; neither carries this wire fixture step's replay key — the replay marked this divergence as owned by nothing — so it is filed here with its own fixture/step linkage. Also related and likewise not covering: 20261106T010945Z-imagebuilder-2-net-new-distribution-configuration-refusal-divergences.md.)

Cause 1 of 1: GetDistributionConfiguration on a distribution-configuration ARN that does not exist (here, one just removed by DeleteDistributionConfiguration during teardown) is refused with the correct HTTP status and error code but with the emulator's own generic ARN-only not-found text instead of AWS's resource-labelled not-found template.

AWS: recorded behavior only for the message text. Smithy (services/service-com-amazonaws-imagebuilder-imagebuilder/smithy-model/com-amazonaws-imagebuilder-imagebuilder.json): GetDistributionConfiguration is GET /GetDistributionConfiguration, success 200, input member distributionConfigurationArn (required, httpQuery, pattern ^arn:aws[^:]*:imagebuilder:[^:]+:(?:[0-9]{12}|aws):distribution-configuration/[a-z0-9-_]+$); its modeled errors are CallRateLimitExceededException 429, ClientException 400, ForbiddenException 403, InvalidRequestException 400, ServiceException 500, ServiceUnavailableException 503 — ResourceNotFoundException is NOT bound to the operation, though the model does define com.amazonaws.imagebuilder#ResourceNotFoundException (errorKind client, httpError 404, member `message`, doc "At least one of the resources referenced by your request does not exist."). The recording shows the refusal as HTTP 404 with error code ResourceNotFoundException and message "The following required resource 'DistributionConfiguration' cannot be found: '<distributionConfigurationArn>'." The FIXED text of that message is "The following required resource 'DistributionConfiguration' cannot be found: '" ... "'." — including the resource-type label 'DistributionConfiguration'; its ONLY variable part is the distribution-configuration ARN echoed from the request. The concrete ARN in the recording (account id, region and generated configuration name) is a per-run value filling that template, not part of the requirement. The same resource-labelled template is used for other Image Builder resource types with their own label (e.g. 'ImageRecipe'), so the label is a per-resource part of the template.

Emulator: answers the right status (404) and the right error code (ResourceNotFoundException), but the message is "The resource ARN '<distributionConfigurationArn>' was not found." — fixed text "The resource ARN '" ... "' was not found." with the ARN as its only variable part, i.e. no resource-type label at all.

Why: the error message is a requirement. A client, SDK error-branching layer, deployment tool or operator that reads AWS's text to learn WHICH referenced resource is missing gets no resource type from the emulator; code that matches or parses AWS's fixed text sees a mismatch on an otherwise correct 404, and messages produced against the emulator are not portable back to real AWS.

Fix: the lookup-miss branch of the GetDistributionConfiguration handler in the imagebuilder service crate — and the shared not-found message formatter it calls — must emit the resource-labelled template carrying the 'DistributionConfiguration' label rather than the generic ARN-only template.

Traces: imagebuilder-distribution-config-multi-region#teardown#6 (the only wire step in this cluster; the rest of that fixture — CreateDistributionConfiguration with multiple regions, the GetDistributionConfiguration round-trip and DeleteDistributionConfiguration — passed, and the 404 status and ResourceNotFoundException code on the post-delete read were correct; only the message diverged).

## Evidence
Replay of the simparity-wire-aws-v1 wire fixture corpus against the emulator built from this checkout (29 fixtures, 351 steps, score 90.6%).

[mismatch] fixture imagebuilder-distribution-config-multi-region, step teardown#6, service imagebuilder, operation GetDistributionConfiguration — expected status 404, got 404 (status agrees); divergence on the response body:
  $.message: value_mismatch
    expected: "The following required resource 'DistributionConfiguration' cannot be found: 'arn:aws:imagebuilder:us-west-2:000000000000:distribution-configuration/sim2real-probe-imagebuilder-ib-dist-multi-ecca2561'."
    actual:   "The resource ARN 'arn:aws:imagebuilder:us-west-2:000000000000:distribution-configuration/sim2real-probe-imagebuilder-ib-dist-multi-ecca2561' was not found."

Requirement vs defect, split into fixed text and variable parts:
  Expected (AWS): HTTP 404, code ResourceNotFoundException, message = fixed text "The following required resource 'DistributionConfiguration' cannot be found: '" + <distributionConfigurationArn taken from the request> + "'."
  Actual (emulator): HTTP 404, code ResourceNotFoundException, message = fixed text "The resource ARN '" + <distributionConfigurationArn taken from the request> + "' was not found."

The ARN is the only variable part in either message; the region, 12-digit account id and the generated configuration name inside the recorded ARN are per-run values and are NOT part of the requirement. The divergence is entirely in the fixed text: AWS's template names the missing resource type ('DistributionConfiguration'), the emulator's does not.

## Steps to Reproduce
1. Build the emulator from this checkout and boot it.
2. Replay the simparity-wire-aws-v1 wire fixture corpus.
3. Replay fixture imagebuilder-distribution-config-multi-region: it creates a distribution configuration with distributions in multiple regions, reads it back, then in teardown deletes it with DeleteDistributionConfiguration and issues GET /GetDistributionConfiguration?distributionConfigurationArn=<the deleted ARN> (step teardown#6).
4. Inspect that final refusal: the status is 404 and the code is ResourceNotFoundException (both correct) but the message is "The resource ARN '<arn>' was not found." instead of AWS's "The following required resource 'DistributionConfiguration' cannot be found: '<arn>'."
5. Re-replay imagebuilder-distribution-config-multi-region#teardown#6 to confirm the fix.
