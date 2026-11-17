Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation__lambda.lambda-sdk-apigateway-create-deployment-missing-permission-negative
Issue-Key: apigateway__cloudformation__lambda.lambda-sdk-apigateway-create-deployment-missing-permission-negative|run#15|KeyError|not_deployed = apig.get_deployments(restApiId=api_without)["items"]
Issue-Commit: 128becbbcef12ce214c9904301e752156a6e58e1

# apigateway__cloudformation__lambda: GetDeployments omits the `items` member entirely for a REST API with no deployments, so the "the denied call created nothing" read raises KeyError

> _Note: this issue was identified against the code base at commit `128becbbcef12ce214c9904301e752156a6e58e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDeployments

## Description
Cause 1 of 1: a 200 `GetDeployments` listing of an EMPTY collection is served with the collection member ABSENT instead of present-and-empty.
Failing check: apigateway__cloudformation__lambda.lambda-sdk-apigateway-create-deployment-missing-permission-negative.

AWS: Smithy (com.amazonaws.apigateway#BackplaneControlService) models `GetDeployments` as HTTP `GET /restapis/{restApiId}/deployments`, success 200, output shape `Deployments` with members `items` (`ListOfDeployment`, jsonName `item`) and `position`, plus `paginated {inputToken: position, outputToken: position, items: items, pageSize: limit}`. The model does not state member presence, so for the empty case this is recorded behavior only: an AWS 200 listing always carries its collection member on the wire under the JSON key `item`, surfaced by SDKs as `items`; a collection holding nothing answers an empty list, never an absent member.

Emulator: the 200 body omits `item` entirely, so the SDK-parsed response has no `items` key at all and the idiomatic read raises `KeyError: 'items'`. The handler assigns `items: page.reported_items(items)` and that helper returns `None` whenever the underlying sequence is empty, after which the serializer drops `items` as an absent optional.

Why: the canonical effect-absent assertion for an IAM-denied control-plane call — "the API still holds zero deployments" — is unwritable. `apig.get_deployments(restApiId=api)["items"]` raises `KeyError: 'items'` in botocore clients and yields a missing/None list in typed SDKs, exactly on the API that owns nothing yet. Here it also aborts the scenario on the POSITIVE CONTROL leg, before the permission-boundary claim this check exists to verify (`AccessDeniedException` from the role without `apigateway:POST`, then success once the grant is added) is ever asserted.

Fix: the `GetDeployments` response path plus the shared `Page::reported_items` helper in the apigateway service crate's state module — a 200 listing must report a present collection member (an empty list when there is nothing to report), not omit it.

RELATED, NOT COVERING: open issue 20261114T023619Z-apigateway-nine-empty-collection-listings-omit-the-items.md reports exactly this defect family for nine OTHER operations (GetAuthorizers, GetDomainNameAccessAssociations, GetUsagePlanKeys, GetDocumentationParts, GetVpcLinks, GetBasePathMappings, GetUsagePlans, GetApiKeys, GetRequestValidators) under service `apigateway`. GetDeployments is not named by it and this check is not marked owned by it. A fix to the shared helper may close all of them at once; a fix scoped to those nine op files will not.

Traces: the failing check id in trace_ids.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout.

[error] apigateway__cloudformation__lambda.lambda-sdk-apigateway-create-deployment-missing-permission-negative
  check_apigateway__cloudformation__lambda_lambda_sdk_apigateway_create_deployment_missing_permission_negative.py:60 `not_deployed = apig.get_deployments(restApiId=api_without)["items"]` -> KeyError: 'items'

Expected (AWS): HTTP 200 whose body carries the wire key `item`, so the parsed response contains `items` and `response["items"] == []` for an API with no deployments; a `get_deployments` paginator's `items` result key resolves to an empty page.
Actual (emulator): HTTP 200 whose body omits `item` entirely, so the parsed response has no `items` key and the read raises `KeyError: 'items'`.

Emulator source responsible: services/service-com-amazonaws-apigateway-backplanecontrolservice/src/ops/get_deployments.rs line 173, `items: page.reported_items(items)` inside `Ok(Paged::new(GetDeploymentsOutput { .. }))`; and `Page::reported_items` in services/service-com-amazonaws-apigateway-backplanecontrolservice/src/state.rs, which returns `None` for an empty sequence so the serializer drops the member.

Note on values: the api ids, role names, function names and stage name in this recording are per-run values and are not part of the requirement. The requirement is structural: the 200 body of a listing always carries its collection member, including when the collection is empty.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and point a boto3 `apigateway` client at it.
2. `api_id = apigw.create_rest_api(name=..., endpointConfiguration={"types": ["REGIONAL"]})["id"]`; add a root MOCK GET method (PutMethod + PutIntegration type=MOCK + PutMethodResponse + PutIntegrationResponse). Create NO deployment on it.
3. Call `apigw.get_deployments(restApiId=api_id)["items"]`.
4. Observed: HTTP 200 whose body has no `item` key, the parsed response has no `items` member, and the read raises `KeyError: 'items'`. Expected: `items == []`.
5. Full scenario: replay apigateway__cloudformation__lambda.lambda-sdk-apigateway-create-deployment-missing-permission-negative. It creates two mock APIs and two Lambda roles (one with `apigateway:POST`, one with no statements), invokes the granted function to deploy the control API, then reads `get_deployments` on the ungranted API to prove the denied CreateDeployment left zero deployments — and dies at that listing before asserting the `AccessDeniedException` boundary.
