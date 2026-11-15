Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected
Issue-Shape-Id: com.amazonaws.marketplacedeployment#AWSMPDeploymentParametersService
Issue-Key: marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected|run#5|AssertionError|assert control_message == (
Issue-Commit: a49cdab94fc41177458b6e53730040285f53d8d6

# PutDeploymentParameter blames ProductId where AWS blames AgreementId in the ResourceNotFoundException message for an unresolved target

> _Note: this issue was identified against the code base at commit `a49cdab94fc41177458b6e53730040285f53d8d6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutDeploymentParameter

## Description
REOPENS 20261029T201601Z-marketplace-deployment-putdeploymentparameter-blames-the-wrong-target-segment-on-an.md: its fix left the call refused. That issue carries this exact check's replay key and its Cause 1 is this very message; it is marked fixed, yet the check still fails on the same control assertion, now with the blamed member inverted again by the later fix for 20261106T210945Z-marketplace-deployment-putdeploymentparameter-blames-agreementid-instead-of-productid-in-the.md. This is not a new defect, it is the same refusal still wrong.

Cause 1 of 1: the ResourceNotFoundException message for a PutDeploymentParameter whose (catalog, productId, agreementId) target is not provisioned names the wrong request member.

AWS: Smithy: PutDeploymentParameter, POST /catalogs/{catalog}/products/{productId}/deployment-parameters, input members catalog (@httpLabel, @required), productId (@httpLabel, @required), agreementId (@required, body), deploymentParameter (@required), tags -> TagsMap @length max 50; modeled error ResourceNotFoundException, errorKind client, httpError 404. The model says nothing about WHICH segment is blamed, so for that choice this is recorded behavior only: the recording for a well-formed request that carries a one-entry tags map and names a productId and an agreementId the account was never provisioned with answers HTTP 404, Error.Code "ResourceNotFoundException", Error.Message "AgreementId: Request references a resource which does not exist." Message anatomy: the fixed text is "<Member>: Request references a resource which does not exist."; the only variable part is <Member>, the PascalCase name of the target segment that did not resolve. No value from the request (no product id, agreement id, ARN) and no per-run value (timestamp, request id) appears in it, so the whole divergence is the member name.

Emulator: for the same request it answers HTTP 404 with the right code but message "ProductId: Request references a resource which does not exist." The precedence is explicit in the code: state.rs `unresolved_target` returns Some("ProductId") whenever no agreement of the account in the catalog covers the product, and only returns Some("AgreementId") once the product itself resolves and the named agreement does not cover it. A request naming neither a held product nor a held agreement — the ordinary never-provisioned case this check exercises — is therefore blamed on ProductId where AWS blames AgreementId.

Why: a client that reads, logs or branches on the blamed member out of the not-found message is told its productId is wrong when AWS tells it its agreementId is wrong, so it re-resolves and retries against the wrong identifier; any test, runbook or assertion on AWS's exact wording fails even though status and code match. The refusal also sits on this operation's primary create path, so it masks the check's second assertion: the 51-entry tags map rejection (expected HTTP 400, code BadRequestException, message "The resultant tag set must not have more than 50 user tags.", where "50" is the member's documented maximum and not a value echoed from the request) is never reached in this replay and stays unverified until the control passes. That second behavior is NOT filed as a cause here because it was not observed on the wire.

Hypothesis: the two fixed issues above assert opposite members for what looks like the same request shape, and the only difference visible between their recordings and this one is that this request carries a tags map. One of the two orders must be wrong; this replay's recording is the ground truth for the request shape it exercises, so a fixer must re-replay BOTH this check and the checks listed in 20261106T210945Z (put-deployment-parameter-nonexistent-agreement-not-found, -catalog-validation-rejected, -name-pattern-violation-rejected, -secret-string-too-long-rejected) in one run and reconcile the blame rule so that no check is traded for another.

Fix: the target-resolution precedence in this service's state layer — the function that picks which segment of (catalog, productId, agreementId) is reported unresolved — and, through it, the PutDeploymentParameter handler's target precondition check, which consumes it before anything is minted or updated. The message template and the 404/ResourceNotFoundException identity are already correct; only the selected member must change.

Traces: marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected. Code citations: services/service-com-amazonaws-marketplacedeployment-awsmpdeploymentparametersservice/src/state.rs lines 683-711 (`unresolved_target`, product judged first) and `target_not_found` (formats the sentence); services/service-com-amazonaws-marketplacedeployment-awsmpdeploymentparametersservice/src/ops/put_deployment_parameter.rs lines 304-333 (handler call site). The open marketplace-deployment issue 20261024T161623Z-marketplace-deployment-putdeploymentparameter-event-propagation.md (secretString not materialised as a Secrets Manager secret) does not cover this refusal.

## Evidence
Failing check: marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected [mismatch]

Replay output:
  File "check_marketplace_deployment_put_deployment_parameter_tags_exceed_max_rejected.py", line 35, in run
    assert control_message == (
  AssertionError: ProductId: Request references a resource which does not exist.

Control call: put_deployment_parameter(catalog="AWSMarketplace", productId="prod-<unique, never provisioned>", agreementId="agr-<unique, never provisioned>", deploymentParameter={name: "sim2real-dp-<unique>", secretString: "s3cr3t-value-1"}, tags={"one-tag": "under-the-limit"}).

Expected (AWS, per the replayed recording): HTTP 404, Error.Code "ResourceNotFoundException", Error.Message "AgreementId: Request references a resource which does not exist."
Actual (emulator): HTTP 404, Error.Code "ResourceNotFoundException" (correct), Error.Message "ProductId: Request references a resource which does not exist."

Fixed text of the message: "<Member>: Request references a resource which does not exist." Variable part: <Member> only — the PascalCase name of the unresolved target segment. Nothing else in the message comes from the request or from emulator state.

Source of the divergence, src/state.rs (lines 698-711):
    if !self.holds_product(catalog, product_id) { return Some("ProductId"); }
    if !self.holds_target(catalog, product_id, agreement_id) { return Some("AgreementId"); }
    None
and the doc comment above it (lines 683-697) states the inverted rule: "the resource the URI PATH addresses is judged FIRST".

Blocked assertion (not observed this replay): the same check's second call with 51 well-formed tag pairs expects HTTP 400, code BadRequestException, message "The resultant tag set must not have more than 50 user tags."; it is never reached because the control assertion fires first.

## Steps to Reproduce
1. Boot the emulator and create a botocore "marketplace-deployment" client pinned to region us-east-1 (the service serves only us-east-1).
2. Call put_deployment_parameter(catalog="AWSMarketplace", productId="prod-<random>" (no held agreement covers it), agreementId="agr-<random>" (not held), deploymentParameter={"name": "sim2real-dp-<random>", "secretString": "s3cr3t-value-1"}, tags={"one-tag": "under-the-limit"}).
3. Observe the ClientError: HTTP 404, Error.Code "ResourceNotFoundException" (correct) and Error.Message "ProductId: Request references a resource which does not exist." where the recording gives "AgreementId: Request references a resource which does not exist."
4. Repeat the same call with tags = {f"key{i}": f"value{i}" for i in range(51)}; this path is only reachable on the wire once step 3 is corrected, and must answer HTTP 400 with code BadRequestException and the message "The resultant tag set must not have more than 50 user tags."
5. Re-replay marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected together with marketplace-deployment.put-deployment-parameter-nonexistent-agreement-not-found, -catalog-validation-rejected, -name-pattern-violation-rejected and -secret-string-too-long-rejected, and confirm all pass in ONE run — the blame rule must not be flipped back and forth between them.
