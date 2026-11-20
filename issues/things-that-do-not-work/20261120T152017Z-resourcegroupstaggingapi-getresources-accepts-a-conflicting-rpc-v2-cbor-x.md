Issue-Source: trace-sweep-divergence
Issue-Trace: resource-groups-tagging-api-get-resources-rpc-v2-cbor-target-header-conflict
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: resource-groups-tagging-api-get-resources-rpc-v2-cbor-target-header-conflict
Issue-Shape-Id: com.amazonaws.resourcegroupstaggingapi#ResourceGroupsTaggingAPI_20170126
Issue-Key: resourcegroupstaggingapi|GetResources|4xx->2xx|$.MalformedHttpRequestException
Issue-Key: resourcegroupstaggingapi|GetResources|4xx->2xx|status
Issue-Commit: 53763dd8e71a257e04fb6c212b98aad1027c99f7

# GetResources accepts a conflicting rpc-v2-cbor + X-Amz-Target request with HTTP 200 instead of refusing it as MalformedHttpRequestException

> _Note: this issue was identified against the code base at commit `53763dd8e71a257e04fb6c212b98aad1027c99f7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResources

## Description
Cause 1 of 1: a GetResources request framed as Smithy RPC v2 CBOR (smithy-protocol: rpc-v2-cbor, dispatch by URI path /service/ResourceGroupsTaggingAPI_20170126/operation/GetResources) that ALSO carries an X-Amz-Target header is self-contradictory framing — rpc-v2-cbor carries the operation in its URI path, while X-Amz-Target is the awsJson1_1 dispatch header and is prohibited under rpc-v2-cbor. The real endpoint refuses such a request; the emulator now SERVES it, answering HTTP 200 with a normal GetResources result body and no error member at all.

AWS: recorded behavior only for this error class. The on-disk Smithy model (smithy_query query='service_info' and query='operation' operation='GetResources' on services/service-com-amazonaws-resourcegroupstaggingapi-resourcegroupstaggingapi-20170126/smithy-model/com-amazonaws-resourcegroupstaggingapi-resourcegroupstaggingapi-20170126.json) declares service com.amazonaws.resourcegroupstaggingapi#ResourceGroupsTaggingAPI_20170126, version 2017-01-26, protocols ['awsJson1_1'], sigv4Name/endpointPrefix 'tagging', GetResources errors InternalServiceException/InvalidParameterException/PaginationTokenExpiredException/ThrottledException with httpBindingsInert (RPC dispatch, payload in the body). The model therefore states neither the CBOR framing nor this error shape, so the recording is the authority: the recorded response is HTTP 400 whose body carries a top-level MalformedHttpRequestException member (recorded value the empty string, i.e. the error is reported by NAME with no message content). The requirement is the error class and the status: a request whose framing contradicts itself (rpc-v2-cbor dispatch path plus a prohibited X-Amz-Target header) is a malformed HTTP request and is rejected with HTTP 400 carrying MalformedHttpRequestException. 'MalformedHttpRequestException' is the fixed text; this error class echoes no variable parts (no request ids, resource names, ARNs or limits), so nothing in the recorded body is per-run data.

Emulator: replay of fixture resource-groups-tagging-api-get-resources-rpc-v2-cbor-target-header-conflict step task#4 reports 'status: expected 400, got 200' and '$.MalformedHttpRequestException: missing (expected '', got None)'. The front door neither refuses the request nor classifies it: the conflicting X-Amz-Target header is ignored (or silently preferred over the rpc-v2-cbor path dispatch), the operation is executed, and a success response is returned.

Why: a client that frames calls as rpc-v2-cbor and also sets X-Amz-Target (e.g. through a stale interceptor or a mixed-protocol middleware) gets a 200 from the emulator where AWS returns a 400 MalformedHttpRequestException. The misconfiguration is invisible in testing: code paths that must handle a malformed-request rejection are never exercised, and a caller concludes its protocol framing is valid when against the real endpoint every such call fails. Worse, the emulator's success body is produced from a request AWS never evaluated, so results observed under the broken framing are fabricated.

Fix: the service front door's protocol negotiation plus its error serializer — when the dispatch path says rpc-v2-cbor and the headers also carry an X-Amz-Target/X-Amzn-Target target, stop dispatching the operation and reject with HTTP 400 whose body carries the MalformedHttpRequestException error member, instead of falling through to the GetResources handler and returning 200.

Traces: resource-groups-tagging-api-get-resources-rpc-v2-cbor-target-header-conflict#task#4. Related but NOT covering this: open issue 20261117T203248Z-resourcegroupstaggingapi-getresources-rejects-an-rpc-v2-cbor-request-carrying.md was filed against the same fixture step when the emulator still answered 400 with the wrong (missing) error shape — its Issue-Key records 4xx->4xx. The emulator's behavior has since drifted to 200, so the divergence is now a status defect as well as an error-shape defect, and the replay reports this cluster (divergence_key service=resourcegroupstaggingapi|GetResources|status=400->200|keys=$.MalformedHttpRequestException,status) with owned_by EMPTY — i.e. no open issue carries this replay key. Open issue 20261117T150031Z-resourcegroupstaggingapi-a-smithy-rpc-v2-cbor-getresources-request.md covers the inverse case (a CBOR request with NO target header being refused instead of served) and likewise does not cover this one.

## Evidence
replay_wire_corpus against the emulator built from this checkout (trace set simparity-wire-aws-v1): emulator_up=true, bootstrap_signal=false, 12 fixtures replayed, 101 steps, 92 passed, 9 mismatched, 0 errored, 0 blocked, 3 divergence clusters, score 91.1%.

Cluster (verdict mismatch), stub_path empty, owned_by empty, affected_count 1:
  divergence_key: service=resourcegroupstaggingapi|GetResources|status=400->200|keys=$.MalformedHttpRequestException,status
  fixture_id: resource-groups-tagging-api-get-resources-rpc-v2-cbor-target-header-conflict
  step_key:   task#4
  expected_status: 400, actual_status: 200
  detail: "status: expected 400, got 200\n$.MalformedHttpRequestException: missing (expected '', got None)"
  affected: [resource-groups-tagging-api-get-resources-rpc-v2-cbor-target-header-conflict#task#4]

Expected vs actual:
  Expected: HTTP 400 whose body carries the top-level MalformedHttpRequestException error member (reported by name; recorded value the empty string, no variable parts).
  Actual:   HTTP 200 with a normal GetResources success body and no MalformedHttpRequestException member — the contradictory framing is accepted and the operation is executed.

Model evidence: smithy_query(query='service_info') -> protocols ['awsJson1_1'], sigv4Name 'tagging', common_errors [InternalServiceException, InvalidParameterException, ThrottledException]; smithy_query(query='operation', operation='GetResources') -> errors [InternalServiceException(500), InvalidParameterException(400), PaginationTokenExpiredException(400), ThrottledException(400)], http {POST /GetResources, inert: true}. MalformedHttpRequestException is not modeled, hence 'recorded behavior only' — the recording of the real endpoint is the requirement.

## Steps to Reproduce
Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1, then re-replay fixture resource-groups-tagging-api-get-resources-rpc-v2-cbor-target-header-conflict step task#4.

1. Send a sigv4-signed (service 'tagging') POST to the Smithy RPC v2 CBOR dispatch path /service/ResourceGroupsTaggingAPI_20170126/operation/GetResources with headers smithy-protocol: rpc-v2-cbor, Content-Type: application/cbor, Accept: application/cbor AND, in conflict with that framing, an X-Amz-Target header, carrying a CBOR-encoded GetResources input.
   Expect: HTTP 400 whose body carries the MalformedHttpRequestException error member (reported by name, no per-request values).
   Observe: HTTP 200 with a GetResources success body and no MalformedHttpRequestException member.
2. Confirm by replaying the fixture step: the divergence reports "status: expected 400, got 200" and "$.MalformedHttpRequestException: missing (expected '', got None)".
