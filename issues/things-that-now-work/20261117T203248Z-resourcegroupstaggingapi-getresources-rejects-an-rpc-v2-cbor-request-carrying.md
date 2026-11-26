Issue-Source: trace-sweep-divergence
Issue-Trace: resource-groups-tagging-api-get-resources-rpc-v2-cbor-target-header-conflict
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: resource-groups-tagging-api-get-resources-rpc-v2-cbor-target-header-conflict
Issue-Shape-Id: com.amazonaws.resourcegroupstaggingapi#ResourceGroupsTaggingAPI_20170126
Issue-Key: resourcegroupstaggingapi|GetResources|4xx->4xx|$.MalformedHttpRequestException
Issue-Commit: cf34de63cf7b24b4b8301888bdc4a22954455ddb

# GetResources rejects an rpc-v2-cbor request carrying X-Amz-Target without the modeled MalformedHttpRequestException error shape

> _Note: this issue was identified against the code base at commit `cf34de63cf7b24b4b8301888bdc4a22954455ddb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResources

## Description
Cause 1 of 1: when a Smithy RPC v2 CBOR framed GetResources request ALSO carries an X-Amz-Target header — a prohibited-header conflict, because rpc-v2-cbor carries the operation in its URI path and the X-Amz-Target header belongs to the awsJson framing — the tagging front door answers HTTP 400 but does NOT serialize the error as the MalformedHttpRequestException the real endpoint returns. Fixture resource-groups-tagging-api-get-resources-rpc-v2-cbor-target-header-conflict, step task#4.

AWS: recorded behavior only for the error shape. The on-disk Smithy model declares protocol awsJson1_1 for com.amazonaws.resourcegroupstaggingapi#ResourceGroupsTaggingAPI_20170126 (smithy_query query='service_info', model_path=services/service-com-amazonaws-resourcegroupstaggingapi-resourcegroupstaggingapi-20170126/smithy-model/com-amazonaws-resourcegroupstaggingapi-resourcegroupstaggingapi-20170126.json -> protocols ['awsJson1_1'], sigv4Name 'tagging', endpointPrefix 'tagging', common_errors InternalServiceException/InvalidParameterException/ThrottledException), so the model states neither the CBOR framing nor this error; the recording is the authority. The recorded response is HTTP 400 whose body carries a top-level MalformedHttpRequestException member (the recorded value is the empty string, i.e. the error is reported by NAME with no message content). The requirement is the error class: a request whose framing is self-contradictory — an rpc-v2-cbor dispatch path plus a prohibited X-Amz-Target target header — is a malformed HTTP request, reported as MalformedHttpRequestException. 'MalformedHttpRequestException' is the fixed text; the body has no variable parts in this error class (no per-request ids, names or values are echoed), so nothing in it is per-run data.

Emulator: replay of task#4 reports status 400 (matching) but '$.MalformedHttpRequestException: missing (expected '', got None)' — the emulator's 400 body contains no MalformedHttpRequestException member at all, so a client cannot classify the failure. Hypothesis: the front door is emitting its generic header-dispatch rejection body for this request (the open issue 20261117T150031Z-resourcegroupstaggingapi-a-smithy-rpc-v2-cbor-getresources-request.md records that same front door answering {"__type":"SerializationException","message":"missing required header `x-amz-target`"} for the no-target CBOR case), i.e. the conflict is funnelled into a serialization error rather than being classified as a malformed HTTP request.

Why: a client that frames calls as rpc-v2-cbor and inadvertently (or via a stale interceptor) also sets X-Amz-Target gets an unclassifiable 400 from the emulator. SDK error handling keys on the error shape, not the status, so the failure surfaces as an unknown/unmodeled 400 rather than the MalformedHttpRequestException AWS returns; retry and error-mapping logic that distinguishes a malformed request from a serialization or throttling error takes the wrong branch, and the real cause (contradictory protocol framing) is hidden from the caller.

Fix: the service front door's protocol negotiation and its error serializer — when the dispatch path and the headers disagree about framing (rpc-v2-cbor URI dispatch plus an X-Amz-Target/X-Amzn-Target header), reject with the MalformedHttpRequestException error shape in the response body instead of the generic serialization/missing-header body, so the 400 carries a classifiable error type.

Traces: resource-groups-tagging-api-get-resources-rpc-v2-cbor-target-header-conflict#task#4. Related but NOT covering this: open issue 20261117T150031Z-resourcegroupstaggingapi-a-smithy-rpc-v2-cbor-getresources-request.md covers a CBOR request with NO X-Amz-Target being refused instead of served (a 400-vs-200 status defect); this divergence is the opposite case — the request is correctly refused with 400, but the refusal's error shape is wrong — and the replay reports it with no owning issue.

## Evidence
replay_wire_corpus against the emulator built from this checkout (trace set simparity-wire-aws-v1): 101 steps, 92 passed, 9 mismatched, 3 divergence clusters, score 91.1%, emulator_up=true, bootstrap_signal=false.

Cluster (verdict mismatch, divergence_key service=resourcegroupstaggingapi|GetResources|status=400->400|keys=$.MalformedHttpRequestException), stub_path empty, owned_by empty, affected_count 1:
  fixture_id: resource-groups-tagging-api-get-resources-rpc-v2-cbor-target-header-conflict
  step_key:   task#4
  expected_status: 400, actual_status: 400
  detail: "$.MalformedHttpRequestException: missing (expected '', got None)"
  affected: [resource-groups-tagging-api-get-resources-rpc-v2-cbor-target-header-conflict#task#4]

Expected vs actual:
  Expected: HTTP 400 with a body carrying the top-level MalformedHttpRequestException error member (recorded value: empty string).
  Actual:   HTTP 400 with no MalformedHttpRequestException member in the body — the error type is absent, so the response is an unclassifiable 400.

Model evidence: smithy_query(query='service_info') on the resourcegroupstaggingapi model returns protocols ['awsJson1_1'] and common_errors [InternalServiceException, InvalidParameterException, ThrottledException]; MalformedHttpRequestException is not modeled, hence 'recorded behavior only' for the shape — the recording of the real endpoint is the requirement.

## Steps to Reproduce
Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1, then re-replay fixture resource-groups-tagging-api-get-resources-rpc-v2-cbor-target-header-conflict step task#4.

1. Send a sigv4-signed (service 'tagging') POST to the Smithy RPC v2 CBOR dispatch path /service/ResourceGroupsTaggingAPI_20170126/operation/GetResources with headers smithy-protocol: rpc-v2-cbor, Content-Type: application/cbor, Accept: application/cbor AND, in conflict with that framing, an X-Amz-Target header, with a CBOR-encoded GetResources input.
   Expect: HTTP 400 whose body carries the MalformedHttpRequestException error member (reported by name; no per-request values in the body).
   Observe: HTTP 400 whose body contains no MalformedHttpRequestException member — the error type is missing entirely.
2. Confirm by replaying the fixture step: the divergence reports "$.MalformedHttpRequestException: missing (expected '', got None)" with expected and actual status both 400.
