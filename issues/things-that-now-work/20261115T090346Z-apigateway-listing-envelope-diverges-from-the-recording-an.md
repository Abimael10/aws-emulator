Issue-Source: trace-sweep-divergence
Issue-Trace: apigateway-create-base-path-mapping-nonexistent-restapi-fails
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigateway-create-base-path-mapping-nonexistent-restapi-fails apigw-envelope-api-keys-position-walk
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway|GetBasePathMappings|2xx->2xx|$
Issue-Key: apigateway|GetApiKeys|2xx->2xx|$.position
Issue-Commit: 99f9186547f1799cdeb9842b49f33a85d7bdb1e3

# apigateway listing envelope diverges from the recording: an empty GetBasePathMappings page emits item:[] and GetApiKeys mints its position cursor from an item id

> _Note: this issue was identified against the code base at commit `99f9186547f1799cdeb9842b49f33a85d7bdb1e3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBasePathMappings

## Description
2 net-new root causes, both of the apigateway control plane's LISTING/PAGING envelope (the shared `crate::state::Page` helper and the two operations' response paths). Both are wire-replay mismatches against recorded real-AWS responses; neither is owned by an open issue and neither is blocked by an unimplemented operation.

Cause 1 of 2: GetBasePathMappings serves `"item": []` for an EMPTY collection where the recording shows AWS serving an empty JSON object.
AWS: Smithy (services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json) models GetBasePathMappings as GET /domainnames/{domainName}/basepathmappings, success 200, output shape BasePathMappings with members `items` (ListOfBasePathMapping, jsonName "item") and `position`, plus paginated {inputToken: position, outputToken: position, items: items, pageSize: limit}. The model states nothing about member PRESENCE, so for the empty case this is recorded behavior only: the recorded 200 for a custom domain that carries no base path mapping has the body `{}` — the `item` key is absent entirely, and no `position` is reported either.
Emulator: the same request is answered 200 with the body `{"item": []}` — the collection member is always present, even when nothing is being reported.
Why: the two differ in exactly the member a client keys off to decide "is anything mapped?". Code written against AWS branches on the member's ABSENCE (`"items" not in response`, or an SDK's absent/None list) for the empty case; against this emulator that branch is never taken, so an emulator-passing client can be wrong on real AWS (and vice versa), and the two environments disagree on the wire bytes of the single most common listing answer.
Fix: GetBasePathMappings' response path together with the shared reporting helper it calls — the member must be reported as the recording shows for an empty page rather than unconditionally materialised. Note the DIRECT CONFLICT a fixer must reconcile rather than ping-pong on: open issue 20261114T023619Z-apigateway-nine-empty-collection-listings-omit-the-items.md asserts the OPPOSITE rule (that every empty listing must carry `items: []`) for nine operations including GetBasePathMappings, and it is grounded in authored behavioral checks, not in a recording; THIS cause is grounded in a recorded real-AWS response for GetBasePathMappings. Whatever the shared helper ends up doing, GetBasePathMappings' empty 200 has to match the recorded wire body.
Traces: apigateway-create-base-path-mapping-nonexistent-restapi-fails#task#4 (replay detail: `$: value_mismatch (expected {}, got {'item': []})`, status 200 on both sides).

Cause 2 of 2: GetApiKeys reports a pagination `position` cursor whose shape is not the one AWS's recorded cursor has — the emulator echoes the last listed api key's own id as the cursor.
AWS: Smithy models GetApiKeys as GET /apikeys, success 200, output `items` (ListOfApiKey, jsonName "item"), `warnings` and `position` (String), paginated {inputToken: position, outputToken: position, items: items, pageSize: limit}. The model states no FORM for the token, so recorded behavior only: on the recorded api-keys position walk the 200 carries a `position` whose shape the replay's shape-level comparison does not accept the emulator's value for (the recorded value itself is a per-run token and is NOT the requirement; its shape class is).
Emulator: the handler assigns `position: page.next_position.clone()` (services/service-com-amazonaws-apigateway-backplanecontrolservice/src/ops/get_api_keys.rs line 236, token minted by `crate::state::page_of`), i.e. the cursor IS the id of the last api key on the page — a bare 10-character lowercase resource id. Replay reports `$.position: value_mismatch (expected '<shaped-not-asserted>', got 'm54nh4kei0')` with status 200 on both sides.
Why: a client that only feeds the cursor straight back keeps working, but the cursor is part of the response contract: one that persists or validates a cursor, or that compares it against the ids it just read, sees a token API Gateway would not have issued, and the emulator's cursor additionally exposes a resource identifier that AWS's token does not. Hypothesis: the shape difference is the id-versus-opaque-token distinction just described; the recording asserts only the shape, not the value.
Fix: GetApiKeys' response path plus the shared cursor mint in the apigateway crate's state (`page_of`/`next_position`) — mint the page cursor in the form the recorded AWS token takes instead of echoing the last item's id.
Traces: apigw-envelope-api-keys-position-walk#task#3.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 1943 steps, 70 mismatched). The two clusters below carry no `stub_path` and no `owned_by`.

Cause 1 — GetBasePathMappings, fixture apigateway-create-base-path-mapping-nonexistent-restapi-fails, step task#4:
  expected_status 200 / actual_status 200
  detail: `$: value_mismatch (expected {}, got {'item': []})`
  Expected (recorded AWS): 200, body `{}` — no `item` member, no `position`.
  Actual (emulator): 200, body `{"item": []}`.
  Emulator source: services/service-com-amazonaws-apigateway-backplanecontrolservice/src/ops/get_base_path_mappings.rs lines 187-197, `items: page.reported_items(items)` with the comment "Always reported, and empty for a domain name that carries no mapping".

Cause 2 — GetApiKeys, fixture apigw-envelope-api-keys-position-walk, step task#3:
  expected_status 200 / actual_status 200
  detail: `$.position: value_mismatch (expected '<shaped-not-asserted>', got 'm54nh4kei0')`
  Expected (recorded AWS): a 200 whose `position` cursor has the recorded token's shape (value not asserted, shape asserted).
  Actual (emulator): `position` = `m54nh4kei0`, the id of the last api key on the page.
  Emulator source: services/service-com-amazonaws-apigateway-backplanecontrolservice/src/ops/get_api_keys.rs line 236, `position: page.next_position.clone()`.

Note on values: the domain name, api ids, api key ids and the cursor string in these recordings are per-run values and are not part of either requirement. Cause 1's requirement is structural (which members an empty 200 listing carries); cause 2's is the shape class of the paging cursor.

## Steps to Reproduce
Cause 1:
1. Point a boto3 `apigateway` client at the emulator.
2. Create a REGIONAL custom domain name, then (without creating any base path mapping, e.g. after a CreateBasePathMapping that was refused for a nonexistent restApiId) call `GET /domainnames/<domain>/basepathmappings`.
3. Observe the emulator's 200 body is `{"item": []}`; the recording's 200 body is `{}`.
4. Full scenario: replay fixture apigateway-create-base-path-mapping-nonexistent-restapi-fails, step task#4.

Cause 2:
1. Create several api keys, then walk `GET /apikeys?limit=<n>`, feeding the returned `position` back in.
2. Observe the returned `position` is the last listed key's own id (a 10-character lowercase id) rather than a cursor of the recorded AWS token's shape.
3. Full scenario: replay fixture apigw-envelope-api-keys-position-walk, step task#3.
