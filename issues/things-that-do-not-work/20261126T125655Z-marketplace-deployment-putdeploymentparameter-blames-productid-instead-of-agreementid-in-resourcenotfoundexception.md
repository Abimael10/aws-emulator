Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected
Issue-Shape-Id: com.amazonaws.marketplacedeployment#AWSMPDeploymentParametersService
Issue-Key: marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected|run#5|AssertionError|assert control_message == (
Issue-Commit: 2d00e84d1b9a81afde435c81a22263e60ab43a33

# PutDeploymentParameter blames ProductId instead of AgreementId in ResourceNotFoundException when neither target resolves

> _Note: this issue was identified against the code base at commit `2d00e84d1b9a81afde435c81a22263e60ab43a33`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutDeploymentParameter

## Description
One cause.

Cause 1 of 1: the ResourceNotFoundException message for an unresolved write target names the wrong input member when a request names neither a held product nor a held agreement. Failing check: marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected.

AWS: PutDeploymentParameter is `POST /catalogs/{catalog}/products/{productId}/deployment-parameters` with `agreementId` in the body; `ResourceNotFoundException` (httpError 404) is the modeled failure for a target that does not resolve (Smithy: PutDeploymentParameter errors include ResourceNotFoundException @httpError 404; input members catalog/productId are @httpLabel, agreementId is a required body member). The model does not state WHICH member a 404 blames, so this is recorded behavior only: for a request carrying a freshly generated, non-existent productId AND a freshly generated, non-existent agreementId in catalog `AWSMarketplace`, AWS answers HTTP 404, code `ResourceNotFoundException`, message `AgreementId: Request references a resource which does not exist.` In that message the fixed text is `: Request references a resource which does not exist.` and the variable part is the blamed input member name, rendered upper-camel (`AgreementId`); AWS resolves/blames the agreement the body names, not the product the URI path addresses, when both are unknown. Hypothesis: AWS authorizes and resolves the agreement first because the agreement is what grants the write, and products are not independently resolvable resources of this API.

Emulator: answers the same code and status but the message `ProductId: Request references a resource which does not exist.` The emulator deliberately inverts the precedence: when the (catalog, productId, agreementId) triple is not held it reports `ProductId` unless the product alone resolves, and only then reports `AgreementId`.

Why: a client (or a human reading the error) is told the product id is the bad input when AWS points at the agreement id, so retry/repair logic that keys off the member name in the message — regenerating or re-looking-up the product instead of the agreement — fixes the wrong field and keeps failing. It also makes the error non-interchangeable with AWS's for any test or tooling that matches on the message, and here it masks the rest of the check: the tag-count rejection (51 tags; expected HTTP 400 `BadRequestException` with message `The resultant tag set must not have more than 50 user tags.`, Smithy: PutDeploymentParameterRequest$tags is TagsMap @length max 50, and BadRequestException is not in the operation's modeled error list — it comes from the tagging front end) is never exercised, so a fixer must re-replay the check and confirm that assertion too.

Fix: the target-resolution step of this operation's handler state, i.e. the member-precedence decision in the emulator's `unresolved_target` helper that `plan_write` consults before anything is written — the member it returns should be the agreement when the request's agreement does not resolve, including the case where the product does not resolve either.

Traces: behavioral check marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected (control call: catalog=AWSMarketplace, random productId, random agreementId, one tag). Code citations: services/service-com-amazonaws-marketplacedeployment-awsmpdeploymentparametersservice/src/state.rs lines 764-778 (`unresolved_target`, returns "ProductId" when the product is not held) and lines 1256-1260 (`target_not_found`, the message template); services/service-com-amazonaws-marketplacedeployment-awsmpdeploymentparametersservice/src/ops/put_deployment_parameter.rs lines 514-521 (`plan_write` raising the exception) and its module doc lines 74-78, which states the inverted order as intended.

## Evidence
Check marketplace-deployment.put-deployment-parameter-tags-exceed-max-rejected, control call:
  md.put_deployment_parameter(catalog="AWSMarketplace", productId=<unique>, agreementId=<unique>, deploymentParameter={name:<unique>, secretString:"s3cr3t-value-1"}, tags={"one-tag":"under-the-limit"})

Expected (AWS, recorded): Error.Code == "ResourceNotFoundException" (HTTP 404), Error.Message == "AgreementId: Request references a resource which does not exist."
Actual (emulator):        Error.Code == "ResourceNotFoundException", Error.Message == "ProductId: Request references a resource which does not exist."

Suite output:
  AssertionError: ProductId: Request references a resource which does not exist.
  (assert control_message == "AgreementId: Request references a resource which does not exist.")

Not reached because of the above (still required by the check): 51-tag request must fail HTTP 400 code "BadRequestException" with message "The resultant tag set must not have more than 50 user tags."

## Steps to Reproduce
1. Build and boot the emulator; create a botocore client for "marketplace-deployment" pinned to region us-east-1.
2. Call PutDeploymentParameter with catalog="AWSMarketplace", a productId that does not exist, an agreementId that does not exist, deploymentParameter={"name": <unique>, "secretString": "s3cr3t-value-1"} and tags={"one-tag": "under-the-limit"}.
3. Observe HTTP 404 ResourceNotFoundException with message "ProductId: Request references a resource which does not exist."; AWS answers "AgreementId: Request references a resource which does not exist."
4. Repeat the same call with a 51-entry tags map and confirm HTTP 400 BadRequestException with message "The resultant tag set must not have more than 50 user tags." (second half of the check, currently unreached).
