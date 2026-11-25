Issue-Source: trace-sweep-divergence
Issue-Trace: sesv2-multi-region-endpoint-lifecycle
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sesv2-multi-region-endpoint-lifecycle
Issue-Shape-Id: com.amazonaws.sesv2#SimpleEmailService_v2
Issue-Key: sesv2|CreateMultiRegionEndpoint|2xx->2xx|$.EndpointId
Issue-Commit: b9452e21b4cf2c09e703f5517f289577e00d07ce

# CreateMultiRegionEndpoint still mints an EndpointId whose form AWS does not emit

> _Note: this issue was identified against the code base at commit `b9452e21b4cf2c09e703f5517f289577e00d07ce`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMultiRegionEndpoint

## Description
One net-new cause on service token sesv2: the EndpointId CreateMultiRegionEndpoint hands back is still not of the form AWS emits, so the fixture's shape assertion on that member rejects it.

Cause 1 of 1: CreateMultiRegionEndpoint's EndpointId has the wrong identifier form.
AWS: Smithy (services/service-com-amazonaws-sesv2-simpleemailservice-v2/smithy-model/com-amazonaws-sesv2-simpleemailservice-v2.json) — CreateMultiRegionEndpoint is POST /v2/email/multi-region-endpoints, success 200, output members Status (enum Status: CREATING|DELETING|FAILED|READY) and EndpointId (shape com.amazonaws.sesv2#EndpointId, type string, documented only as "The ID of the multi-region endpoint (global-endpoint)"); the shape declares NO pattern and NO length, so the model states no format. Recorded behavior only: the fixture does not assert the recorded id literally (the value itself is per-run and is not a requirement) — it asserts the member BY SHAPE, and AWS's recorded id satisfies that shape. The requirement is therefore the FORM of the identifier AWS emits for a multi-region endpoint, which is the same id that is later accepted in SendEmail/SendBulkEmail's EndpointId input and that appears in the endpoint's ARN.
Emulator: returns a value that fails that shape assertion. Replay reports $.EndpointId: value_mismatch (expected '<shaped-not-asserted>', got 'c207a28a-637f-4968-81cf-fc377237e461') on a 200/200 response — i.e. status, Status and every other member match and only the id's form diverges. The mint is crate::state's next_endpoint_id (src/state.rs ~line 2066), which derives bytes from a monotonic sequence number, the instant, the endpoint name and the region and formats them with minted_uuid as a dashed 8-4-4-4-12 hexadecimal string; its own doc comment records the deliberate choice to use "the one this store writes its other opaque identifiers in — a UUID" because the model states no form. The value now emitted IS a well-formed RFC 4122 version-4 UUID (version nibble 4, variant 8) and the assertion still fails, so AWS's recorded EndpointId is NOT a dashed 36-character hex UUID at all; the form to emit must be read off the recorded 200 response for this step rather than guessed.
Why: EndpointId is an identifier clients parse, validate, store in typed fields and feed back to the send operations as the EndpointId input. A client that validates the id against the form AWS emits (or round-trips it through a typed identifier) rejects or mangles the emulator's id, and any ARN or configuration the emulator derives from it carries the wrong form onward, so code that works against AWS fails against the emulator at the point the endpoint is first created.
Fix: the identifier mint behind CreateMultiRegionEndpoint — next_endpoint_id / minted_uuid in services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/state.rs, consumed by src/ops/create_multi_region_endpoint.rs — must emit the identifier form AWS uses for a multi-region endpoint (uniqueness per endpoint and stability for the endpoint's lifetime, which the current mint already provides, are not at issue). Every read path reports the id off the record, so fixing the mint fixes Get/List as well.
Traces: sesv2-multi-region-endpoint-lifecycle#task#1.

This cluster is unowned by any open issue (the replay report lists owned_by empty and no stub_path). The RESOLVED issue 20261029T171854Z-sesv2-multi-region-endpoint-plane-wrong-route-count-refusal.md filed the same divergence key (sesv2|CreateMultiRegionEndpoint|2xx->2xx|$.EndpointId) as its Cause 2, against the earlier value '6dfa78b9-89f6-cee2-ba26-f281536eb3f5'; that issue is closed and does not cover this failure — the edit it received only made the value a syntactically valid v4 UUID, which the fixture still rejects, so the identifier form is still wrong. The route-count refusal and content-type causes of that issue no longer diverge in this replay.

## Evidence
Replay of wire trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up true, 384 steps, 330 passed).

Fixture sesv2-multi-region-endpoint-lifecycle, step task#1, verdict mismatch, divergence_key "service=sesv2|CreateMultiRegionEndpoint|status=200->200|keys=$.EndpointId", expected_status 200, actual_status 200:
  $.EndpointId: value_mismatch (expected '<shaped-not-asserted>', got 'c207a28a-637f-4968-81cf-fc377237e461')

The recorded id value itself is per-run and is NOT the requirement; what diverges is the member's form, which the fixture asserts by shape. The emulator's value is a well-formed RFC 4122 v4 UUID and is still rejected, so the required form is not a dashed 8-4-4-4-12 hex UUID. Every other member of the same 200 response (including Status) matched, and the fixture's later GetMultiRegionEndpoint / ListMultiRegionEndpoints / DeleteMultiRegionEndpoint steps passed.

Model evidence (smithy_query, services/service-com-amazonaws-sesv2-simpleemailservice-v2/smithy-model/com-amazonaws-sesv2-simpleemailservice-v2.json):
  CreateMultiRegionEndpoint — POST /v2/email/multi-region-endpoints, code 200; input EndpointName (required, len 1..64, pattern ^[\w\-_]+$) and Details (required), Tags optional; output Status (Status enum) and EndpointId (EndpointId); errors AlreadyExistsException/BadRequestException/LimitExceededException @400, TooManyRequestsException @429.
  EndpointId — com.amazonaws.sesv2#EndpointId, type string, no pattern, no length, doc "The ID of the multi-region endpoint (global-endpoint)."

Code evidence: services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/create_multi_region_endpoint.rs calls store.next_endpoint_id(&name, &primary_region, now) and returns it as the response's endpoint_id; services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/state.rs line 2066 implements that mint as minted_uuid(b"Endpoint", sequence, now, &[name, region]), i.e. UUID text form chosen because the model states no format.

## Steps to Reproduce
1. git_clone the repository and boot the emulator built from that checkout.
2. Replay the wire fixture corpus (replay_wire_corpus) and look at fixture sesv2-multi-region-endpoint-lifecycle, step task#1.
3. Call CreateMultiRegionEndpoint with a valid EndpointName and Details.RoutesDetails holding exactly one route, and inspect the returned EndpointId: the emulator returns a dashed 36-character UUID (e.g. 'c207a28a-637f-4968-81cf-fc377237e461'), which fails the fixture's shape assertion for that member.
4. Read the recorded 200 response for sesv2-multi-region-endpoint-lifecycle#task#1 to take the identifier form AWS emits for EndpointId, and change the mint (next_endpoint_id / minted_uuid in src/state.rs) to emit that form while keeping the id unique per endpoint and stable for its lifetime.
5. Re-replay sesv2-multi-region-endpoint-lifecycle and confirm task#1 passes and that the fixture's later Get/List/Delete steps, which report the id off the stored record, still pass.
