Issue-Source: trace-sweep-divergence
Issue-Trace: sesv2-multi-region-endpoint-lifecycle
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sesv2-multi-region-endpoint-lifecycle
Issue-Shape-Id: com.amazonaws.sesv2#SimpleEmailService_v2
Issue-Key: sesv2|CreateMultiRegionEndpoint|2xx->2xx|$.EndpointId
Issue-Commit: 52dd5548c9dac981d1505a1c67d2263892a7273a

# CreateMultiRegionEndpoint mints an EndpointId in the wrong identifier form

> _Note: this issue was identified against the code base at commit `52dd5548c9dac981d1505a1c67d2263892a7273a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMultiRegionEndpoint

## Description
One cause.

Cause 1 of 1: CreateMultiRegionEndpoint returns an EndpointId whose identifier FORM is not the one SES hands back.

AWS: recorded behavior only. Smithy says nothing about the format — CreateMultiRegionEndpointResponse$EndpointId targets com.amazonaws.sesv2#EndpointId, a plain `string` with no @pattern and no @length, documented only as "The ID of the multi-region endpoint (global-endpoint)" (smithy_query shape=EndpointId / doc CreateMultiRegionEndpointResponse$EndpointId on services/service-com-amazonaws-sesv2-simpleemailservice-v2/smithy-model/com-amazonaws-sesv2-simpleemailservice-v2.json). The requirement therefore comes from the recording, which asserts this member by SHAPE and not by value: the fixture carries the placeholder '<shaped-not-asserted>' because the concrete id is per-run, while the FORM of the id is the thing being asserted. The recorded SES response's EndpointId does not have the shape of a dashed 36-character RFC 4122 UUID. The op succeeds with HTTP 200 and Status=CREATING; neither the status nor Status diverges, so the whole requirement here is the identifier form of the one member.

Hypothesis: SES mints an opaque, prefixed identifier for a multi-region (global) endpoint, in the same style the SAME service's TenantId is recorded in — `tn-` followed by 30 lowercase hex characters (visible in this corpus as the recorded TenantId of email-tenant-creation-and-resource-association#task#2). That is a hypothesis about the exact alphabet/prefix; what the recording establishes without hypothesis is only that a bare dashed UUID is not it.

Emulator: returns a well-formed RFC 4122 version-4 UUID, i.e. 8-4-4-4-12 lowercase hex groups with the version nibble 4 and the variant bits 10xx. The id is minted by ComAmazonawsSesv2SimpleemailserviceV2State::next_endpoint_id, which delegates to minted_uuid and thence to service_common::uuid_from_random_bytes. The module doc of state/minted_ids.rs asserts as a premise that SES hands EndpointId back "as a UUID"; the recording contradicts that premise, and that premise is the defect. Note the shape assertion fails even though the value IS now a valid v4 UUID: an earlier pass over this plane recorded an EndpointId whose third group began 'cee2' (invalid version nibble) and the repair made the value a legal v4 UUID, which did not satisfy the assertion. So the gap is the choice of identifier form, not UUID well-formedness — re-deriving the bytes or fixing version/variant bits again will not close it.

Why: a client that validates, stores or round-trips the endpoint id against the form SES actually emits rejects the emulator's value (an id parsed into a typed/constrained field, a regex guard, a fixed-width column), and the id is also the component the emulator embeds in the identifiers it hands out for that endpoint, so the wrong form propagates past this one response. Any client keyed on the SES id form sees an endpoint it cannot address.

Fix: the EndpointId generator behind this operation — state/minted_ids.rs (minted_uuid, lines 35-52) as reached from ComAmazonawsSesv2SimpleemailserviceV2State::next_endpoint_id in services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/state.rs (around lines 1815-1827), which ops/create_multi_region_endpoint.rs calls at line 233 and stores on the record so Get/List read the same value back. The emission form belongs in the minting function, not in the handler, so the id stays stable for the life of the endpoint.

Related already-filed issues, none of which cover this: 20261029T171854Z-sesv2-multi-region-endpoint-plane-wrong-route-count-refusal.md carried this same cause as its Cause 2 but is RESOLVED (its repair changed the UUID's version/variant bits and the shape assertion still fails), and a resolved issue does not cover a live failure; the open 20261029T171854Z-sesv2-tenant-and-reputation-entity-reads-expose-unset-status.md owns the TenantId form cited above as evidence, not EndpointId.

Traces: sesv2-multi-region-endpoint-lifecycle#task#1.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 269 steps, score 77.3%).

Divergence cluster (verdict=mismatch, affected_count=1, stub_path empty, owned_by empty — net-new):
  service=sesv2 | CreateMultiRegionEndpoint | status 200 -> 200 | keys $.EndpointId
  fixture sesv2-multi-region-endpoint-lifecycle, step task#1
  detail: $.EndpointId: value_mismatch (expected '<shaped-not-asserted>', got '3db51568-82a0-46c8-bbc0-642e5bdad799')

Expected: a value matching the shape the recorded SES response's EndpointId has. The fixture deliberately does not assert the literal id ('<shaped-not-asserted>'), so the literal UUID quoted above is one run's value and is not itself the requirement; the requirement is the form.
Actual: a dashed 36-character RFC 4122 v4 UUID, which does not satisfy that shape.

Supporting model lookups: EndpointId is `string` with no pattern/length; CreateMultiRegionEndpoint http POST /v2/email/multi-region-endpoints, success 200, output members {Status (enum CREATING|READY|FAILED|DELETING), EndpointId}. Only $.EndpointId diverges.

Supporting code: src/state/minted_ids.rs documents the (contradicted) premise "handed back by SES as UUIDs" and renders the id through service_common::uuid_from_random_bytes; src/state.rs next_endpoint_id calls minted_uuid(b"Endpoint", endpoint_sequence, now, &[name, region]); src/ops/create_multi_region_endpoint.rs line 233 stores it on MultiRegionEndpointRecord and returns it.

## Steps to Reproduce
1. Replay fixture sesv2-multi-region-endpoint-lifecycle, step task#1, from trace set simparity-wire-aws-v1.
2. Equivalently: POST /v2/email/multi-region-endpoints with a valid EndpointName (1-64 chars, ^[\w\-_]+$) and Details.RoutesDetails containing exactly one route whose Region is an AWS Region.
3. The call answers 200 with Status=CREATING and an EndpointId.
4. Inspect the FORM of the returned EndpointId: the emulator returns a dashed 36-character v4 UUID (e.g. 3db51568-82a0-46c8-bbc0-642e5bdad799), which fails the fixture's shape-only assertion on that member. Compare against the recorded SES EndpointId form; a bare UUID is not it.
5. GET /v2/email/multi-region-endpoints/{EndpointName} and GET /v2/email/multi-region-endpoints read the same stored id back, so the wrong form is observable on every read of the endpoint, not just on create.
