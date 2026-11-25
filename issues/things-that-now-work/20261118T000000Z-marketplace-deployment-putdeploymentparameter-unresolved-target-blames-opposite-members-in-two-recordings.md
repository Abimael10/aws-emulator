Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: marketplace-deployment.put-deployment-parameter-nonexistent-agreement-not-found marketplace-deployment.put-deployment-parameter-catalog-validation-rejected marketplace-deployment.put-deployment-parameter-name-pattern-violation-rejected marketplace-deployment.put-deployment-parameter-secret-string-too-long-rejected
Issue-Shape-Id: com.amazonaws.marketplacedeployment#AWSMPDeploymentParametersService
Issue-Key: marketplace-deployment.put-deployment-parameter-nonexistent-agreement-not-found|run#11|AssertionError|assert message == "ProductId: Request references a resource which does not exist.", message
Issue-Key: marketplace-deployment.put-deployment-parameter-catalog-validation-rejected|run#8|AssertionError|assert message == "ProductId: Request references a resource which does not exist.", message
Issue-Key: marketplace-deployment.put-deployment-parameter-name-pattern-violation-rejected|run#7|AssertionError|assert control_message == (
Issue-Key: marketplace-deployment.put-deployment-parameter-secret-string-too-long-rejected|run#7|AssertionError|assert control_message == (

# PutDeploymentParameter: two recordings of the unresolved-target 404 name opposite members for what looks like the same state, and the emulator can satisfy only one of them

## Operation
PutDeploymentParameter

## Description
Cause 1 of 1: the ResourceNotFoundException message for a PutDeploymentParameter whose (catalog, productId, agreementId) target is not provisioned names the wrong request member for the four checks listed above — and naming the one THEY expect makes a fifth check (put-deployment-parameter-tags-exceed-max-rejected) fail instead. This issue exists to carry the half of the corpus the emulator's one documented resolution order cannot reproduce; it is filed deliberately rather than left as a silent regression, because the previous three fixes of this message traded one half for the other and each was re-filed within days.

AWS: Smithy: PutDeploymentParameter, POST /catalogs/{catalog}/products/{productId}/deployment-parameters, input members catalog (@httpLabel, @required), productId (@httpLabel, @required), agreementId (@required, body), deploymentParameter (@required), tags -> TagsMap @length max 50; modelled error ResourceNotFoundException, errorKind client, httpError 404. The model fixes the status and the code and says NOTHING about which segment of the target is blamed, so the member choice is recorded behaviour only. The message anatomy is agreed by every recording: the fixed text is "<Member>: Request references a resource which does not exist.", and the only variable part is <Member>, the PascalCase name of the target segment reported unresolved. No value from the request (product id, agreement id, ARN) and no per-run value appears in it.

The corpus records BOTH members for this one sentence:
* put-deployment-parameter-nonexistent-agreement-not-found, -catalog-validation-rejected, -name-pattern-violation-rejected and -secret-string-too-long-rejected expect "ProductId: Request references a resource which does not exist."; their calls are PutDeploymentParameter(catalog="AWSMarketplace", productId=<unique, never provisioned>, agreementId=<unique, never provisioned>, deploymentParameter={name, secretString}) with no tags member.
* put-deployment-parameter-tags-exceed-max-rejected expects "AgreementId: Request references a resource which does not exist."; its control call is the same request plus tags={"one-tag": "under-the-limit"}.

Emulator: `state.rs` `unresolved_target` applies ONE order to every request — the resource the operation is targeted by (agreementId) is resolved first and the product it has to cover second, so a request holding neither segment is reported on AgreementId — and `target_not_found` formats the sentence from it. That order reproduces the tags-exceed-max recording and contradicts the four above, which therefore fail on their positive-control assertion. The 404 status and the ResourceNotFoundException code are correct in every case; only the blamed member diverges.

Why: a client that reads, logs or branches on the blamed member out of the not-found message is sent to re-resolve the wrong identifier, and any test or runbook asserting AWS's exact wording fails even though status and code match. The refusal also sits on this operation's primary create path, so for three of the four checks it masks their own assertion (the catalog allowed-value message, the deploymentParameter.name pattern message and the oversized-secretString ForbiddenException), which stay unverified until the control passes.

What is NOT the fix, with evidence: inverting the order. It has been done twice (20261029T201601Z set the agreement first, 20261106T210945Z set the product first, 20261115T000840Z set the agreement first again), and each inversion made the other half of the corpus fail; a tag-presence switch keyed on whether the request carries a tags map was rejected in review as a branch on the recorded input's form, since no documented behaviour of this service makes tagging change which resource a target is resolved against.

What a fix needs, therefore, is the DISCRIMINATOR these two recordings actually differ in, which the issue reports do not carry:
1. the state each recording ran against — whether the recording account held an agreement or a product that the generated identifiers collided with, or held the product under another agreement, which would make the two states different rather than both "nothing held";
2. the full recorded requests, member for member (the recorded productId/agreementId values' namespace and form, the catalog, and whether the control call of each check really carries no tags);
3. whether AWS answers one request shape non-deterministically — two recordings of the SAME request state answered on different members would make the member a per-run choice rather than a rule, which is a corpus/analysis conclusion and not something the emulator can implement.
Only (1) or (2) can be implemented as a rule; (3) would mean this cause should be closed as not a defect, with the message template (already correct) as the whole requirement.

Fix: once the discriminator is known, `state.rs` `unresolved_target` (the single place that picks the reported segment, consumed by `src/ops/put_deployment_parameter.rs` `plan_write` before anything is minted) and the `unresolved_target_precedence` test suite that pins the rule. Do not change the message template, the 404 or the ResourceNotFoundException identity: those are already right.

Traces: marketplace-deployment.put-deployment-parameter-nonexistent-agreement-not-found, marketplace-deployment.put-deployment-parameter-catalog-validation-rejected, marketplace-deployment.put-deployment-parameter-name-pattern-violation-rejected, marketplace-deployment.put-deployment-parameter-secret-string-too-long-rejected. Code citations: services/service-com-amazonaws-marketplacedeployment-awsmpdeploymentparametersservice/src/state.rs (`unresolved_target`, `holds_agreement`, `holds_target`, `target_not_found`) and services/service-com-amazonaws-marketplacedeployment-awsmpdeploymentparametersservice/src/ops/put_deployment_parameter.rs (`plan_write`, the target precondition). The contradicting recording is the one 20261115T000840Z was filed and fixed for.

## Evidence
Failing checks (expected vs actual against the current emulator):
* put-deployment-parameter-nonexistent-agreement-not-found, run#11: expected HTTP 404, code ResourceNotFoundException, message "ProductId: Request references a resource which does not exist."; actual: HTTP 404, code ResourceNotFoundException (correct), message "AgreementId: Request references a resource which does not exist."
* put-deployment-parameter-catalog-validation-rejected, run#8: same expectation, same actual.
* put-deployment-parameter-name-pattern-violation-rejected, run#7: same control assertion, so the check never reaches its own name-pattern assertion.
* put-deployment-parameter-secret-string-too-long-rejected, run#7: same control assertion, so the check never reaches its own oversized-secret assertion.

The contradicting recording, which the emulator currently satisfies: put-deployment-parameter-tags-exceed-max-rejected, run#5, expects "AgreementId: Request references a resource which does not exist." for the same (never-provisioned productId, never-provisioned agreementId) target, its control call carrying a one-entry tags map.

Source of the single order, src/state.rs `unresolved_target`:
    if self.holds_target(catalog, product_id, agreement_id) { return None; }
    Some(if self.holds_agreement(catalog, agreement_id) { "ProductId" } else { "AgreementId" })

## Steps to Reproduce
1. Boot the emulator and create a botocore "marketplace-deployment" client pinned to region us-east-1 (the service serves only us-east-1).
2. Call put_deployment_parameter(catalog="AWSMarketplace", productId="prod-<random>", agreementId="agr-<random>" (neither provisioned), deploymentParameter={"name": "sim2real-dp-<random>", "secretString": "s3cr3t-value-1"}) with NO tags member. Observe HTTP 404, code ResourceNotFoundException and message "AgreementId: Request references a resource which does not exist." where these four checks record "ProductId: …".
3. Repeat the same call with tags={"one-tag": "under-the-limit"}: the answer is identical, and that is the message put-deployment-parameter-tags-exceed-max-rejected records and asserts.
4. Replay all five checks in ONE run: four fail on their control and the fifth passes. Any change that makes the four pass by inverting the order makes the fifth fail — establish the discriminator (recorded state and full recorded requests for both sides) before touching the order.
