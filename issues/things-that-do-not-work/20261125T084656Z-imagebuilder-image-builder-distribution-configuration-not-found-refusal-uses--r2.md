Issue-Source: trace-sweep-divergence
Issue-Trace: imagebuilder-create-distribution-configuration-rejects-a-region-entry-with-no-outputs
Issue-Step: teardown#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: imagebuilder-create-distribution-configuration-rejects-a-region-entry-with-no-outputs imagebuilder-create-distribution-configuration-rejects-an-unknown-region imagebuilder-distribution-configuration-accepts-an-empty-distribution-list imagebuilder-ibp-traj-cleanup-in-dependency-order imagebuilder-ibp-traj-distribution-config-mistakes
Issue-Shape-Id: com.amazonaws.imagebuilder#imagebuilder
Issue-Key: imagebuilder|GetDistributionConfiguration|4xx->4xx|$.message
Issue-Commit: cffe8f41acc6e00f5d2821a02f5b08be509d3433

# Image Builder distribution-configuration not-found refusal uses the resource-labelled template instead of AWS's bare-ARN wording

> _Note: this issue was identified against the code base at commit `cffe8f41acc6e00f5d2821a02f5b08be509d3433`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDistributionConfiguration

## Description
This issue carries ONE net-new root cause (Cause 1 of 1) in the Image Builder emulator's not-found refusal message on the distribution-configuration read path, observed on the wire corpus (simparity-wire-aws-v1). The replay marked this divergence as owned by nothing and blocked by no stub.

Cause 1 of 1: GetDistributionConfiguration on a distribution-configuration ARN that does not exist (a teardown read after the configuration was deleted, and the equivalent cleanup read in a dependency-ordered teardown) is refused with the correct HTTP status and the correct error code, but the message text is the resource-labelled template rather than the bare-ARN template the recording requires.

AWS: recorded behavior only for the message text. Smithy (services/service-com-amazonaws-imagebuilder-imagebuilder/smithy-model/com-amazonaws-imagebuilder-imagebuilder.json): GetDistributionConfiguration is GET /GetDistributionConfiguration, success 200, input member distributionConfigurationArn (required, httpQuery, pattern ^arn:aws[^:]*:imagebuilder:[^:]+:(?:[0-9]{12}|aws):distribution-configuration/[a-z0-9-_]+$); its bound errors are CallRateLimitExceededException 429, ClientException 400, ForbiddenException 403, InvalidRequestException 400, ServiceException 500 and ServiceUnavailableException 503 — ResourceNotFoundException (shape com.amazonaws.imagebuilder#ResourceNotFoundException, client error, httpError 404, member message, doc "At least one of the resources referenced by your request does not exist.") is NOT bound to this operation, so the model says nothing about this particular refusal and the recording is the only authority on its text. The recorded response is HTTP 404 with the message template "The resource ARN '<distributionConfigurationArn>' was not found." — the fixed text is "The resource ARN '" ... "' was not found.", and its ONLY variable part is the distribution-configuration ARN echoed from the request's distributionConfigurationArn. The region, the 12-digit account id and the generated configuration name inside the ARN of any one recording are per-run values that fill that single variable slot; they are not part of the requirement.

Emulator: same HTTP 404 and the same error code, but the message is the other template — fixed text "The following required resource 'DistributionConfiguration' cannot be found: '" ... "'.", i.e. a resource-kind label plus the ARN, where the ARN is again the only variable part. So status and code agree and the whole divergence is the message's fixed text.

Why: an error message is a requirement. A client, deployment tool, teardown script or operator that matches AWS's fixed not-found text — or that distinguishes "this resource I asked for is gone" from "a resource my request referenced is missing" by that wording, which is exactly what the two templates mean on real AWS — treats an otherwise-correct refusal as unrecognized, and logic or diagnostics developed against the emulator does not carry back to real AWS. Because this is the idempotent "is it really gone?" probe at the end of several fixtures, it also makes clean teardown look like an unexpected failure.

Fix: the not-found branch of the GetDistributionConfiguration handler in the imagebuilder service crate, and the shared not-found message formatter it calls: the distribution-configuration own-read miss must emit the bare-ARN template while the resource-labelled template remains for the call sites/resource kinds that genuinely use it (the labelled wording belongs to a missing REQUIRED REFERENCE of another request, not to a resource's own Get*). Hypothesis (stated by neither the model nor this recording): the formatter is currently selected per resource type rather than per call site, which is why the own-read path picked up the labelled wording.

Related already-filed work a fixer must reconcile before editing the formatter: open behavioral issue 20261125T024626Z-ec2-iam-imagebuilder-image-builder-getdistributionconfiguration-not-found-message-now-uses.md states the SAME requirement (bare-ARN wording) from the behavioral corpus but carries behavioral check ids, not this fixture step, so it does not cover this divergence. The issues that asserted the OPPOSITE requirement for this operation — 20261118T174922Z-imagebuilder-image-builder-distribution-configuration-not-found-refusal-uses.md, 20261117T031907Z-imagebuilder-getdistributionconfiguration-on-a-deleted-distribution-configuration-returns.md, 20261114T230016Z-imagebuilder-2-net-new-refusal-message-divergences-distribution.md and 20261115T021705Z-imagebuilder-image-builder-teardown-paths-diverge-legitimate-deletes-refused.md — are all RESOLVED and therefore cover nothing here; their fix is what moved this path onto the labelled template, and the wire corpus now records the bare-ARN wording for it.

Traces: fixture imagebuilder-create-distribution-configuration-rejects-a-region-entry-with-no-outputs step teardown#6 (representative), plus the five other members of the same cluster: that fixture's teardown#7, imagebuilder-create-distribution-configuration-rejects-an-unknown-region#teardown#5, imagebuilder-distribution-configuration-accepts-an-empty-distribution-list#teardown#4, imagebuilder-ibp-traj-cleanup-in-dependency-order#task#19 and imagebuilder-ibp-traj-distribution-config-mistakes#teardown#9.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 351 steps, 331 passed, score 94.3%).

[mismatch] service=imagebuilder | GetDistributionConfiguration | expected_status 404 -> actual_status 404
  divergence_key: service=imagebuilder|GetDistributionConfiguration|status=404->404|keys=$.message
  representative: imagebuilder-create-distribution-configuration-rejects-a-region-entry-with-no-outputs#teardown#6
  detail: $.message: value_mismatch
    expected: "The resource ARN 'arn:aws:imagebuilder:<region>:<account>:distribution-configuration/<name>' was not found."
    actual:   "The following required resource 'DistributionConfiguration' cannot be found: 'arn:aws:imagebuilder:<region>:<account>:distribution-configuration/<name>'."

The status (404) and the error code (ResourceNotFoundException) match; $.message is the only diverging key, and it is a difference of the template's FIXED text, not of a per-run value:
  Required fixed text: "The resource ARN '" + <distributionConfigurationArn from the request> + "' was not found."
  Emulator fixed text: "The following required resource 'DistributionConfiguration' cannot be found: '" + <the same ARN> + "'."
The ARN is the single variable part of both templates; the region, account id and generated configuration name within it are per-run values and are not part of the requirement.

Cluster membership (affected_count 6): imagebuilder-create-distribution-configuration-rejects-a-region-entry-with-no-outputs#teardown#6, #teardown#7, imagebuilder-create-distribution-configuration-rejects-an-unknown-region#teardown#5, imagebuilder-distribution-configuration-accepts-an-empty-distribution-list#teardown#4, imagebuilder-ibp-traj-cleanup-in-dependency-order#task#19, imagebuilder-ibp-traj-distribution-config-mistakes#teardown#9. owned_by: none. stub_path: none.

## Steps to Reproduce
1. Build the emulator from this checkout and boot it.
2. imagebuilder CreateDistributionConfiguration(name=<n>, distributions=[{region: <region>, amiDistributionConfiguration: {name: ...}}]) and note the returned distributionConfigurationArn.
3. imagebuilder DeleteDistributionConfiguration(distributionConfigurationArn=<that arn>).
4. imagebuilder GetDistributionConfiguration(distributionConfigurationArn=<that same arn>) — i.e. the teardown existence probe.
5. Observe HTTP 404 / ResourceNotFoundException with message "The following required resource 'DistributionConfiguration' cannot be found: '<arn>'." where this corpus requires "The resource ARN '<arn>' was not found."
6. Reconcile with open issue 20261125T024626Z (same requirement, behavioral grounding) and with the RESOLVED issues that asserted the labelled template for this operation before changing the formatter, then re-replay fixture imagebuilder-create-distribution-configuration-rejects-a-region-entry-with-no-outputs step teardown#6 and the five other cluster members listed in the evidence.
