Issue-Source: trace-sweep-divergence
Issue-Trace: apigateway-create-base-path-mapping-nonexistent-restapi-fails
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigateway-create-base-path-mapping-nonexistent-restapi-fails
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway|GetBasePathMappings|2xx->2xx|$
Issue-Commit: 1a552fe8708f5d6fbecf5907a9407a2fce831c57

# apigateway GetBasePathMappings answers an empty collection with item:[] instead of the bare {} object

> _Note: this issue was identified against the code base at commit `1a552fe8708f5d6fbecf5907a9407a2fce831c57`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBasePathMappings

## Description
ONE (1) net-new root cause in the API Gateway REST control plane (service token `apigateway`), reported by the simparity-wire-aws-v1 replay with verdict=mismatch, empty `stub_path` and empty `owned_by`.

=== Cause 1 of 1: GetBasePathMappings serves `{"item": []}` for an EMPTY collection where the recorded service serves the bare object `{}` ===

AWS: Smithy (services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json) models GetBasePathMappings as GET /domainnames/{domainName}/basepathmappings with http success code 200, input { domainName (httpLabel, required), domainNameId (httpQuery), position (httpQuery), limit (httpQuery, NullableInteger) }, output shape BasePathMappings { items: ListOfBasePathMapping with jsonName "item", position: String }, @paginated {inputToken: position, outputToken: position, items: items, pageSize: limit}; modeled errors BadRequestException(400), NotFoundException(404), TooManyRequestsException(429), UnauthorizedException(401). The model states nothing about member PRESENCE, so for the EMPTY case this is recorded behavior only: the recorded 200 for an existing custom domain name that carries no base path mapping has the body `{}` — the `item` key is absent altogether, and no `position` is reported either. The rule is structural: for an empty page of this listing, neither the collection member nor the cursor is serialized.

Emulator: the same GET is answered 200 with the body `{"item": []}` — the collection member is materialized unconditionally, whatever the page holds (replay detail: `$: value_mismatch (expected {}, got {'item': []})`, status 200 on both sides).

Why: the presence of that member is exactly what a client keys off to decide "is anything mapped to this domain?". Code written against AWS branches on the member's ABSENCE (`"items" not in response`, or an SDK's absent/None list) for the empty case, and that branch is never taken against the emulator; conversely a client that assumes the member is always there passes here and raises on real AWS. The two environments disagree on the wire bytes of the most common answer this listing gives — the one for a freshly created domain name, which every create-then-verify and delete-then-confirm base-path-mapping sequence passes through.

Fix: the GetBasePathMappings response path in the apigateway crate, together with the shared paging/report helper it calls (`crate::state::Page::reported_items` / `next_position`), which currently turns an empty sequence into a PRESENT empty list rather than an absent optional member. Scope the change to GetBasePathMappings and reconcile it rather than flipping the shared helper blanket-wise: a sibling listing has the OPPOSITE recorded requirement (open issue 20261121T033135Z-apigateway-3-net-new-wire-causes-getdeployments-empty.md records that GetDeployments' recorded empty 200 carries `{"item": []}`), so the helper needs a per-operation decision, not one global rule.

Not covered by an open issue — this is a REGRESSION of a resolved one, and the resolved issue covers nothing: 20261115T090346Z-apigateway-listing-envelope-diverges-from-the-recording-an.md asserted this exact rule for this exact fixture step and is RESOLVED (status fixed), and 20261114T023619Z-apigateway-nine-empty-collection-listings-omit-the-items.md — which asserted the OPPOSITE rule (every empty apigateway listing must carry `items: []`) for nine operations including GetBasePathMappings, grounded in authored behavioral checks rather than a recording — is also RESOLVED, and the emulator source now implements that opposite rule here. The replay reports this cluster with owned_by=[], i.e. no open issue carries its replay key.

Traces: apigateway-create-base-path-mapping-nonexistent-restapi-fails#task#4 (this cluster's only member and its representative). Emulator source: services/service-com-amazonaws-apigateway-backplanecontrolservice/src/ops/get_base_path_mappings.rs lines 197-208 — `items: page.reported_items(items)` with the comment "The page's elements, PRESENT whatever the page holds", and `position: page.next_position.clone()`.

Note on values: the domain name, the rest api id and the ids in the recording are per-run values and are no part of the requirement; the requirement is structural (which members an empty 200 page of this listing carries).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true (/__admin/health answered 200 and the :38125 data listener accepts), 206 fixtures replayed, 2023 steps total, 2000 passed, 23 mismatched, 0 errored, 0 blocked, 12 divergence clusters, score 98.9%. This cluster reports verdict=mismatch, stub_path="" and owned_by=[] — neither blocked by an unimplemented operation nor carried by an open issue's replay key.

Cause 1 — GetBasePathMappings, fixture apigateway-create-base-path-mapping-nonexistent-restapi-fails, step task#4:
  divergence_key: service=apigateway|GetBasePathMappings|status=200->200|keys=$
  expected_status 200 / actual_status 200
  detail: $: value_mismatch (expected {}, got {'item': []})
  Expected (recorded AWS): HTTP 200, body `{}` — no `item` member and no `position` member.
  Actual (emulator): HTTP 200, body `{"item": []}`.
  affected_count 1: apigateway-create-base-path-mapping-nonexistent-restapi-fails#task#4.

Model evidence (smithy_query query=operation, model_path services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json):
  GetBasePathMappings -> http {method: GET, uri: /domainnames/{domainName}/basepathmappings, code: 200}; output_shape BasePathMappings with items -> ListOfBasePathMapping, jsonName "item", and position -> String; paginated {inputToken: position, outputToken: position, items: items, pageSize: limit}. No trait constrains member presence, so the empty-page body is recorded behavior only.

Emulator evidence (read of services/service-com-amazonaws-apigateway-backplanecontrolservice/src/ops/get_base_path_mappings.rs):
  lines 197-208 build the output as `items: page.reported_items(items)` / `position: page.next_position.clone()`, with an in-code comment stating the member is "PRESENT whatever the page holds" so that "a domain name that carries no mapping ... is answered the empty list rather than an answer that withholds the member" — the emulator deliberately implements the behavior the recording contradicts.

Regression context: the one issue that ever carried this fixture step, 20261115T090346Z-apigateway-listing-envelope-diverges-from-the-recording-an.md (Issue-Trace: apigateway-create-base-path-mapping-nonexistent-restapi-fails, Issue-Step: task#4), is RESOLVED, and the conflicting 20261114T023619Z-apigateway-nine-empty-collection-listings-omit-the-items.md is RESOLVED too — the behavior the latter installed is the one diverging here.

## Steps to Reproduce
1. Build and boot the emulator from this checkout; point a boto3 `apigateway` client at its data listener.
2. Create a REGIONAL custom domain name: `create_domain_name(domainName='<some-domain>', regionalCertificateArn=...)` (or reach the same state as the fixture does: issue a `create_base_path_mapping` for that domain naming a nonexistent restApiId, which is refused and leaves the domain with no mapping).
3. With NO base path mapping on that domain, call `get_base_path_mappings(domainName='<some-domain>')`, i.e. GET /domainnames/<some-domain>/basepathmappings.
4. Observe the emulator answers HTTP 200 with the body `{"item": []}`, so `response['items'] == []`. Expected (recorded AWS): HTTP 200 with the body `{}` — the `item` key absent entirely and no `position`, so an AWS-written client's `'items' not in response` branch is the one taken.
5. Full scenario: replay fixture apigateway-create-base-path-mapping-nonexistent-restapi-fails, step task#4.
