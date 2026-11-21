Issue-Source: trace-sweep-divergence
Issue-Trace: apigateway-api-key-value-length-bounds
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigateway-api-key-value-length-bounds apigateway-authorizer-lifecycle-with-test-invoke apigateway-documentation-parts-versions-and-export apigateway-documentation-version-crud apigateway-documentation-version-crud-lifecycle apigateway-documentation-version-lifecycle apigateway-documentation-version-lifecycle-basic apigateway-documentation-versions-crud-lifecycle apigateway-multiple-documentation-versions apigateway-usage-plan-key-association-and-usage-tracking apigateway-vpc-link-negative-paths apigw-envelope-documentation-parts-position-walk
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway|GetApiKeys|2xx->2xx|$
Issue-Key: apigateway|GetAuthorizers|2xx->2xx|$
Issue-Key: apigateway|GetDocumentationVersions|2xx->2xx|$
Issue-Key: apigateway|GetUsagePlanKeys|2xx->2xx|$
Issue-Key: apigateway|GetVpcLinks|2xx->2xx|$
Issue-Key: apigateway|GetDocumentationParts|2xx->2xx|$
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# apigateway empty listing 200s omit the item member entirely where the recordings carry item: []

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetApiKeys

## Description
6 net-new root causes, all of the apigateway control plane, all one shared fault: a 200 listing of an EMPTY collection is served as the bare JSON object `{}` instead of carrying its collection member as an empty array. Every cause below is its own operation/response path, and each has to report a present member before its own replay step passes. All six clusters carry no stub_path and no owned_by.

Common AWS statement: Smithy services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json models each of these operations as HTTP GET with success 200 and an output member `items` targeting a list with jsonName "item" (plus `position`, and for GetApiKeys/GetVpcLinks/GetUsagePlanKeys also @paginated {inputToken: position, outputToken: position, items: items, pageSize: limit}). The model does not state member PRESENCE, so for the empty case this is recorded behavior only: every recorded 200 below carries the wire key `item` with an empty array, and reports no `position`/`limit` alongside it.

Common emulator statement: the same request is answered 200 with an empty JSON object — the `item` key is absent altogether.

Common why-it-matters: the member is exactly what a client keys off to answer "is it listed / is it gone?". The idiomatic botocore read `response["items"]` raises KeyError: 'items' and a paginator's `items` result key resolves to nothing, precisely on the fresh API/account that owns nothing yet — so every delete-confirmation and no-phantom-state poll written against AWS breaks here, and the two environments disagree on the wire bytes of the single most common listing answer.

Common fix location: each operation's response path in services/service-com-amazonaws-apigateway-backplanecontrolservice/src/ops/<op>.rs together with the shared paging/report helper in that crate's src/state.rs that currently turns an empty sequence into an absent optional member.

IMPORTANT, and NOT covered by the open issues a fixer will find: open issue 20261114T023619Z-apigateway-nine-empty-collection-listings-omit-the-items.md asserts the same rule but is grounded in BEHAVIORAL checks only and names none of the fixture steps below (none of these clusters is marked owned by it), so a fix validated against it leaves these replay steps unrun. Open issue 20261115T090346Z-apigateway-listing-envelope-diverges-from-the-recording-an.md records the OPPOSITE requirement for GetBasePathMappings (its recorded empty 200 body is `{}`). The two are reconcilable only per operation: do NOT flip the shared helper blanket-wise in either direction — the six operations below must carry `item: []` on an empty page while GetBasePathMappings must stay as its own recording shows.

Cause 1 of 6: GetApiKeys (GET /apikeys) answers `{}` for a selection that matches nothing.
AWS: Smithy GetApiKeys -> output shape ApiKeys {items: ListOfApiKey jsonName "item", position, warnings}, 200. Recorded behavior only: the recorded 200 body is `{"item": []}`.
Emulator: 200 with body `{}` (replay detail `$: empty_body (expected {'item': []}, got {})`).
Why/Fix/Traces: as the common statements above; src/ops/get_api_keys.rs response path. Trace: apigateway-api-key-value-length-bounds#task#3.

Cause 2 of 6: GetAuthorizers (GET /restapis/{restApiId}/authorizers) answers `{}` for an API whose authorizers have all been deleted.
AWS: Smithy GetAuthorizers -> output Authorizers {items: ListOfAuthorizer jsonName "item", position}, 200. Recorded behavior only: recorded 200 body `{"item": []}`.
Emulator: 200 with body `{}` (`$: empty_body (expected {'item': []}, got {})`).
Fix: src/ops/get_authorizers.rs response path + the shared helper. Trace: apigateway-authorizer-lifecycle-with-test-invoke#task#9.

Cause 3 of 6: GetDocumentationVersions (GET /restapis/{restApiId}/documentation/versions) answers `{}` for an API with no documentation version. This operation is named by NO open issue — it is not one of the nine in 20261114T023619Z.
AWS: Smithy GetDocumentationVersions -> output DocumentationVersions {items: ListOfDocumentationVersion jsonName "item", position}, 200. Recorded behavior only: recorded 200 body `{"item": []}`.
Emulator: 200 with body `{}` (`$: empty_body (expected {'item': []}, got {})`). This cluster is the widest of the six: 8 recorded steps across 7 fixtures hit it.
Fix: src/ops/get_documentation_versions.rs response path + the shared helper. Trace: apigateway-documentation-parts-versions-and-export#task#14.

Cause 4 of 6: GetUsagePlanKeys (GET /usageplans/{usagePlanId}/keys) answers `{}` for a usage plan with no keys left.
AWS: Smithy GetUsagePlanKeys -> output UsagePlanKeys {items: ListOfUsagePlanKey jsonName "item", position}, 200, @paginated. Recorded behavior only: recorded 200 body `{"item": []}`.
Emulator: 200 with body `{}` (`$: empty_body (expected {'item': []}, got {})`).
Fix: src/ops/get_usage_plan_keys.rs response path + the shared helper. Trace: apigateway-usage-plan-key-association-and-usage-tracking#task#12.

Cause 5 of 6: GetVpcLinks (GET /vpclinks) answers `{}` when the account holds no VPC link.
AWS: Smithy GetVpcLinks -> output VpcLinks {items: ListOfVpcLink jsonName "item", position}, 200, @paginated. Recorded behavior only: recorded 200 body `{"item": []}`.
Emulator: 200 with body `{}` (`$: empty_body (expected {'item': []}, got {})`).
Fix: src/ops/get_vpc_links.rs response path + the shared helper. Trace: apigateway-vpc-link-negative-paths#task#1.

Cause 6 of 6: GetDocumentationParts (GET /restapis/{restApiId}/documentation/parts) answers `{}` at the end of a position walk, where the recorded page carries `item: []` and reports NO `position` and NO `limit`.
AWS: Smithy GetDocumentationParts -> output DocumentationParts {items: ListOfDocumentationPart jsonName "item", position}, 200. Recorded behavior only: the recorded terminal page is `{"item": []}` with position and limit absent — i.e. the member is present-and-empty while the cursor members are omitted, not the other way round.
Emulator: 200 with body `{}` (`$: empty_body (expected {'item': [], 'position': None, 'limit': None}, got {})`).
Fix: src/ops/get_documentation_parts.rs response path + the shared helper. Trace: apigw-envelope-documentation-parts-position-walk#task#11.

Note on values: the api ids, api key ids, usage plan ids, part ids and cursor strings in these recordings are per-run values and are not part of the requirement. The requirement is structural: an empty 200 listing for these six operations carries `item: []`.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, 2023 steps, 1938 passed, 85 mismatched, 50 divergence clusters. The six clusters below all report verdict=mismatch, stub_path="" and owned_by=[].

Cause 1 — GetApiKeys, fixture apigateway-api-key-value-length-bounds, step task#3:
  expected_status 200 / actual_status 200
  detail: $: empty_body (expected {'item': []}, got {})
  Expected (recorded AWS): 200, body {"item": []}. Actual (emulator): 200, body {}.

Cause 2 — GetAuthorizers, fixture apigateway-authorizer-lifecycle-with-test-invoke, step task#9:
  expected_status 200 / actual_status 200
  detail: $: empty_body (expected {'item': []}, got {})

Cause 3 — GetDocumentationVersions, fixture apigateway-documentation-parts-versions-and-export, step task#14:
  expected_status 200 / actual_status 200
  detail: $: empty_body (expected {'item': []}, got {})
  Cluster members (8): apigateway-documentation-parts-versions-and-export#task#14, apigateway-documentation-version-crud#task#3, apigateway-documentation-version-crud-lifecycle#task#3, apigateway-documentation-version-lifecycle#task#4, apigateway-documentation-version-lifecycle-basic#task#3, apigateway-documentation-versions-crud-lifecycle#task#3, apigateway-multiple-documentation-versions#setup#3, apigateway-multiple-documentation-versions#task#11.

Cause 4 — GetUsagePlanKeys, fixture apigateway-usage-plan-key-association-and-usage-tracking, step task#12:
  expected_status 200 / actual_status 200
  detail: $: empty_body (expected {'item': []}, got {})

Cause 5 — GetVpcLinks, fixture apigateway-vpc-link-negative-paths, step task#1:
  expected_status 200 / actual_status 200
  detail: $: empty_body (expected {'item': []}, got {})

Cause 6 — GetDocumentationParts, fixture apigw-envelope-documentation-parts-position-walk, step task#11:
  expected_status 200 / actual_status 200
  detail: $: empty_body (expected {'item': [], 'position': None, 'limit': None}, got {})

Model evidence (same for all six): smithy_query query=operation on services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json reports, for each of GetApiKeys/GetAuthorizers/GetDocumentationVersions/GetUsagePlanKeys/GetVpcLinks/GetDocumentationParts, output member items -> list with jsonName "item", http GET with code 200.

## Steps to Reproduce
1. Point a boto3 `apigateway` client at the emulator.
2. Cause 1: call `get_api_keys()` (or with a `nameQuery` matching nothing) on an account with no api key -> observe 200 with body `{}` and `KeyError: 'items'`; expected body `{"item": []}`.
3. Cause 2: `create_rest_api(...)`, create then delete its authorizer, call `get_authorizers(restApiId=api_id)` -> 200 `{}`.
4. Cause 3: `create_rest_api(...)`, call `get_documentation_versions(restApiId=api_id)` before any version exists -> 200 `{}`.
5. Cause 4: `create_usage_plan(name=...)`, call `get_usage_plan_keys(usagePlanId=plan_id)` with no keys attached -> 200 `{}`.
6. Cause 5: with no VPC link in the account, call `get_vpc_links()` -> 200 `{}`.
7. Cause 6: walk `get_documentation_parts(restApiId=api_id, limit=n)` feeding `position` back until exhausted; the terminal page is 200 `{}` instead of `{"item": []}` (with no position/limit members).
8. Full scenarios: replay fixtures apigateway-api-key-value-length-bounds#task#3, apigateway-authorizer-lifecycle-with-test-invoke#task#9, apigateway-documentation-parts-versions-and-export#task#14, apigateway-usage-plan-key-association-and-usage-tracking#task#12, apigateway-vpc-link-negative-paths#task#1 and apigw-envelope-documentation-parts-position-walk#task#11.
