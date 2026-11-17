Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cognito-idp__lambda.lambda-apigateway-create-deployment-missing-permission-negative
Issue-Key: apigateway__cognito-idp__lambda.lambda-apigateway-create-deployment-missing-permission-negative|run#11|KeyError|no_deployments = apigw.get_deployments(restApiId=api_id)["items"]
Issue-Commit: 128becbbcef12ce214c9904301e752156a6e58e1

# apigateway__cognito-idp__lambda: GetDeployments omits the `items` member entirely for a REST API with no deployments, so the "the denied call created nothing" read raises KeyError

> _Note: this issue was identified against the code base at commit `128becbbcef12ce214c9904301e752156a6e58e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDeployments

## Description
Cause 1 of 1: a 200 `GetDeployments` listing of an EMPTY collection is served with the collection member ABSENT instead of present-and-empty.
Failing check: apigateway__cognito-idp__lambda.lambda-apigateway-create-deployment-missing-permission-negative.

AWS: Smithy (com.amazonaws.apigateway#BackplaneControlService) models `GetDeployments` as HTTP `GET /restapis/{restApiId}/deployments`, success 200, output shape `Deployments` with members `items` (`ListOfDeployment`, jsonName `item`) and `position`, plus `paginated {inputToken: position, outputToken: position, items: items, pageSize: limit}`. The model does not state member presence, so for the empty case this is recorded behavior only: an AWS 200 listing always carries its collection member on the wire under the JSON key `item`, surfaced by SDKs as `items`; a collection holding nothing answers an empty list, never an absent member.

Emulator: the 200 body omits `item` entirely, so the SDK-parsed response has no `items` key at all and the idiomatic read raises `KeyError: 'items'`. The handler assigns `items: page.reported_items(items)` and that helper returns `None` whenever the underlying sequence is empty, after which the serializer drops `items` as an absent optional.

Why: the canonical effect-absent assertion for an IAM-denied control-plane call — "no deployment was actually created server-side" — is unwritable. `apigw.get_deployments(restApiId=api_id)["items"]` raises `KeyError: 'items'` in botocore clients and yields a missing/None list in typed SDKs, exactly on the API that owns nothing yet. Here the KeyError lands immediately after the denial itself was observed correctly, so the scenario aborts before its positive control (grant `apigateway:POST` on the same role, re-invoke, and confirm the deployment lands) is ever run.

Fix: the `GetDeployments` response path plus the shared `Page::reported_items` helper in the apigateway service crate's state module — a 200 listing must report a present collection member (an empty list when there is nothing to report), not omit it.

RELATED, NOT COVERING: open issue 20261114T023619Z-apigateway-nine-empty-collection-listings-omit-the-items.md reports exactly this defect family for nine OTHER operations (GetAuthorizers, GetDomainNameAccessAssociations, GetUsagePlanKeys, GetDocumentationParts, GetVpcLinks, GetBasePathMappings, GetUsagePlans, GetApiKeys, GetRequestValidators) under service `apigateway`. GetDeployments is not named by it and this check is not marked owned by it. A fix to the shared helper may close all of them at once; a fix scoped to those nine op files will not.

Traces: the failing check id in trace_ids.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout.

[error] apigateway__cognito-idp__lambda.lambda-apigateway-create-deployment-missing-permission-negative
  check_apigateway__cognito_idp__lambda_lambda_apigateway_create_deployment_missing_permission_negative.py:47 `no_deployments = apigw.get_deployments(restApiId=api_id)["items"]` -> KeyError: 'items'

Expected (AWS): HTTP 200 whose body carries the wire key `item`, so the parsed response contains `items` and `response["items"] == []` for an API with no deployments.
Actual (emulator): HTTP 200 whose body omits `item` entirely, so the parsed response has no `items` key and the read raises `KeyError: 'items'`.

The assertions immediately before this read already passed — the in-Lambda `CreateDeployment` was correctly refused with error code `AccessDeniedException` for a role with no inline statements — so only the listing read diverges.

Emulator source responsible: services/service-com-amazonaws-apigateway-backplanecontrolservice/src/ops/get_deployments.rs line 173, `items: page.reported_items(items)` inside `Ok(Paged::new(GetDeploymentsOutput { .. }))`; and `Page::reported_items` in services/service-com-amazonaws-apigateway-backplanecontrolservice/src/state.rs, which returns `None` for an empty sequence so the serializer drops the member.

Note on values: the api id, role name, function name and the `prod` stage name in this recording are per-run values and are not part of the requirement. The requirement is structural: the 200 body of a listing always carries its collection member, including when the collection is empty.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and point a boto3 `apigateway` client at it.
2. `api_id = apigw.create_rest_api(...)["id"]` and add a MOCK method on its root resource. Create NO deployment.
3. Call `apigw.get_deployments(restApiId=api_id)["items"]`.
4. Observed: HTTP 200 whose body has no `item` key, the parsed response has no `items` member, and the read raises `KeyError: 'items'`. Expected: `items == []`.
5. Full scenario: replay apigateway__cognito-idp__lambda.lambda-apigateway-create-deployment-missing-permission-negative. It creates a Lambda role with no inline statements, invokes a function that calls `create_deployment(restApiId=api_id, stageName='prod')` and correctly sees `AccessDeniedException`, then reads `get_deployments(restApiId=api_id)["items"]` to prove the effect is absent — and dies there, before the positive control that adds `apigateway:POST` to the same role.
