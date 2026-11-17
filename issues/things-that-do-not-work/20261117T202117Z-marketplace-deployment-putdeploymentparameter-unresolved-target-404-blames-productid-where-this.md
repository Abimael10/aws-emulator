Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected
Issue-Shape-Id: com.amazonaws.marketplacedeployment#AWSMPDeploymentParametersService
Issue-Key: marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected|run#5|AssertionError|assert control_message == (
Issue-Commit: 4f7db5b1105780994fb2a54279ab21096be5892d

# PutDeploymentParameter unresolved-target 404 blames ProductId where this recording blames AgreementId, so the >50-tag refusal is never reached

> _Note: this issue was identified against the code base at commit `4f7db5b1105780994fb2a54279ab21096be5892d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutDeploymentParameter

## Description
Cause 1 of 1 — the member named in the PutDeploymentParameter ResourceNotFoundException message for a write whose (catalog, productId, agreementId) target is not provisioned. Failing check: marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected.

RELATED OPEN ISSUE (does NOT cover this check): 20261118T000000Z-marketplace-deployment-putdeploymentparameter-unresolved-target-blames-opposite-members-in-two-recordings.md discusses this same sentence and even names put-deployment-parameter-tags-exceed-max-rejected in prose as "the contradicting recording", but it carries only the four opposite-polarity checks in its Issue-Trace-Ids/Issue-Key lines (put-deployment-parameter-nonexistent-agreement-not-found, -catalog-validation-rejected, -name-pattern-violation-rejected, -secret-string-too-long-rejected) and the suite did not mark this check as owned by it. It is filed here so this check's replay key is carried by an open work item rather than only mentioned in another issue's prose. Regression history, for the fixer's benefit: 20261115T000840Z-marketplace-deployment-putdeploymentparameter-blames-productid-where-aws-blames-agreementid-in.md is marked FIXED and claimed exactly the member this check expects; the resolution order in state.rs has since been inverted back (the four checks above now pass and this one fails), which is the third turn of that oscillation (see also the FIXED 20261029T201601Z and 20261106T210945Z).

AWS: Smithy (services/service-com-amazonaws-marketplacedeployment-awsmpdeploymentparametersservice): PutDeploymentParameter, POST /catalogs/{catalog}/products/{productId}/deployment-parameters, input catalog (@httpLabel @required, Catalog length 1..64 pattern ^[a-zA-Z_-]+$), productId (@httpLabel @required, ResourceId), agreementId (@required, body, ResourceId), deploymentParameter (@required), tags -> TagsMap @length max 50; modelled errors include ResourceNotFoundException (errorKind client, httpError 404) and ValidationException (400). The model fixes the 404 and the code and says NOTHING about which segment of the target is blamed, so the member choice is recorded behavior only. The message anatomy agreed by every recording of this service is the fixed text "<Member>: Request references a resource which does not exist.", whose only variable part is <Member>, the PascalCase name of the target segment reported unresolved; no request value (product id, agreement id, ARN) and no per-run value appears in it. This recording's control call — catalog="AWSMarketplace", a never-provisioned productId, a never-provisioned agreementId, deploymentParameter={name, secretString}, plus a one-entry tags map — is recorded as HTTP 404, code ResourceNotFoundException, message "AgreementId: Request references a resource which does not exist." Note also that the refusal this check exists to assert, code BadRequestException with the fixed text "The resultant tag set must not have more than 50 user tags." for a 51-entry tags map, is recorded behavior only: BadRequestException is not among this operation's modelled errors, and that assertion is currently unreachable, hence unverified.

Emulator: state.rs unresolved_target applies ONE order to every request — holds_product(catalog, productId) is consulted first and the body's agreementId second — so a request holding NEITHER segment is reported on the path's product: `Some(if self.holds_product(catalog, product_id) { "AgreementId" } else { "ProductId" })`. target_not_found formats the sentence from that. The replayed control call therefore answers HTTP 404, code ResourceNotFoundException (correct), message "ProductId: Request references a resource which does not exist." where this recording has "AgreementId: …". The status and code match; only the blamed member diverges.

Why: a client that reads, logs or branches on the blamed member out of the not-found sentence is sent to re-resolve the wrong identifier of the two, and any test or runbook asserting AWS's exact wording fails even though status and code match. Because the refusal sits on this operation's only create path, it also aborts the check before its real subject is exercised, so the over-50-tag refusal (identity and wording) stays unverified on this emulator.

Fix: the single place that picks the reported segment — state.rs unresolved_target (with holds_target/holds_product/holds_agreement) — and its unresolved_target_precedence tests, consumed by ops/put_deployment_parameter.rs before anything is minted. Do NOT change the message template, the 404 or the ResourceNotFoundException code: those are already right. Because a plain inversion of the order makes the four checks of the related open issue fail instead (that has now happened three times), a fix must establish the discriminator the two halves of the corpus actually differ in (the state each recording ran against, or the full recorded requests) rather than flipping the order again; a branch keyed on whether the request carries a tags map is not a documented behavior of this service. If the corpus cannot supply a discriminator, that conclusion belongs on the related open issue — but this check's key must not be left uncarried.

Traces: marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected (control call of the check, before its tag-count assertion). Code citations: services/service-com-amazonaws-marketplacedeployment-awsmpdeploymentparametersservice/src/state.rs lines 745-778 (unresolved_target) and 1256-1260 (target_not_found); services/service-com-amazonaws-marketplacedeployment-awsmpdeploymentparametersservice/src/ops/put_deployment_parameter.rs (the target precondition).

## Evidence
Check marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected, assertion at check_marketplace_deployment_put_deployment_parameter_tags_exceed_max_rejected.py line 35:

  AssertionError: ProductId: Request references a resource which does not exist.

Expected (recorded): HTTP 404, Error.Code "ResourceNotFoundException", Error.Message "AgreementId: Request references a resource which does not exist."
Actual (emulator):   HTTP 404, Error.Code "ResourceNotFoundException" (matches), Error.Message "ProductId: Request references a resource which does not exist."

Message anatomy: fixed text "<Member>: Request references a resource which does not exist."; the single variable part is <Member>, the PascalCase target segment reported unresolved — here recorded as AgreementId, emitted as ProductId. Nothing else in the sentence comes from the request or from state.

Blocked, therefore unverified, assertion of the same check: for tags={key0..key50} (51 entries, documented maximum 50) it expects Error.Code "BadRequestException" and the fixed text "The resultant tag set must not have more than 50 user tags." The run never reaches it.

Source of the single order, src/state.rs unresolved_target:
    if self.holds_target(catalog, product_id, agreement_id) { return None; }
    Some(if self.holds_product(catalog, product_id) { "AgreementId" } else { "ProductId" })

The other half of the corpus (the four checks of open issue 20261118T000000Z…) currently PASSES with this order, which is why a plain inversion is not the fix.

## Steps to Reproduce
1. Boot the emulator and build a botocore "marketplace-deployment" client pinned to region us-east-1 (the service serves only us-east-1).
2. Call put_deployment_parameter(catalog="AWSMarketplace", productId="prod-<random, never provisioned>", agreementId="agr-<random, never provisioned>", deploymentParameter={"name": "sim2real-dp-<random>", "secretString": "s3cr3t-value-1"}, tags={"one-tag": "under-the-limit"}).
3. Observe HTTP 404 with code ResourceNotFoundException and message "ProductId: Request references a resource which does not exist." where the recording has "AgreementId: Request references a resource which does not exist."
4. Repeat the same call with tags = {f"key{i}": f"value{i}" for i in range(51)}: the same ProductId 404 is returned, so the recorded BadRequestException "The resultant tag set must not have more than 50 user tags." is never exercised.
5. Replay marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected together with the four checks listed in open issue 20261118T000000Z…: exactly one half of the five passes under either resolution order, so confirm any fix by replaying ALL FIVE in one run.
