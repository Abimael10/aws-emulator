Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: resourcegroupstaggingapi.get-resources-rpc-v2-cbor-request-accepted
Issue-Shape-Id: com.amazonaws.resourcegroupstaggingapi#ResourceGroupsTaggingAPI_20170126
Issue-Key: resourcegroupstaggingapi.get-resources-rpc-v2-cbor-request-accepted|run#17|AssertionError|assert status == 200, f"RPC v2 CBOR GetResources must be served; got ({status}, {raw!r})"
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# resourcegroupstaggingapi: a Smithy RPC v2 CBOR GetResources request is refused 400 SerializationException 'missing required header `x-amz-target`' instead of being served

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResources

## Description
Cause 1 of 1: the tagging endpoint dispatches only on X-Amz-Target, so a request framed as Smithy RPC v2 CBOR — which carries the operation in its URI path and deliberately has no X-Amz-Target header — is rejected as a serialization error instead of being executed.
AWS: recorded behavior only. The on-disk Smithy model declares protocol awsJson1_1 for com.amazonaws.resourcegroupstaggingapi#ResourceGroupsTaggingAPI_20170126 (smithy_query query='service_info': protocols ['awsJson1_1'], sigv4Name 'tagging'), so the model does not state the CBOR framing; the recording shows the real endpoint serving it. A sigv4-signed POST to /service/ResourceGroupsTaggingAPI_20170126/operation/GetResources with headers smithy-protocol: rpc-v2-cbor, Content-Type: application/cbor, Accept: application/cbor and a CBOR-encoded GetResources input and NO X-Amz-Target is answered with HTTP 200, response headers smithy-protocol: rpc-v2-cbor and content-type: application/cbor, and a CBOR body that decodes to the same payload the awsJson1_1 call returns for that query ({'PaginationToken': '', 'ResourceTagMappingList': [{'ResourceARN': <arn>, 'Tags': [{'Key': <k>, 'Value': <v>}]}]} — the ARN and tag values are per-run data, the shape and the equality with the JSON answer are the requirement). The CBOR read is a read: it must not perturb the tag index.
Emulator: answers HTTP 400 with body {"__type":"SerializationException","message":"missing required header `x-amz-target`"} — fixed emulator-authored text naming a header the rpc-v2-cbor framing does not use. The operation is never dispatched, and the response is JSON, with neither smithy-protocol nor application/cbor content type.
Why: SDKs are migrating JSON-protocol AWS services onto rpc-v2-cbor; any client (newer AWS SDK release, or one configured for CBOR) that frames its calls this way gets a hard 400 on every operation against the emulator while the same code works against AWS, and a SerializationException misdirects the caller to its payload when the real problem is protocol routing.
Fix: request routing/protocol negotiation in the service's front door — recognise the /service/<serviceId>/operation/<OperationName> path plus smithy-protocol: rpc-v2-cbor as an operation dispatch (no X-Amz-Target required), decode the CBOR body into the operation input, and encode the response in CBOR with the smithy-protocol and content-type response headers; the X-Amz-Target requirement must apply only to the awsJson framing.
Traces: resourcegroupstaggingapi.get-resources-rpc-v2-cbor-request-accepted. No open issue covers the CBOR framing of this service.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 against the emulator built from this checkout: 47/66 passed; service resourcegroupstaggingapi 14/15 (93%), 1 failed, not marked ALREADY FILED or OWNED.

[mismatch] get-resources-rpc-v2-cbor-request-accepted, check line 62:
  assert status == 200, f"RPC v2 CBOR GetResources must be served; got ({status}, {raw!r})"
  AssertionError: RPC v2 CBOR GetResources must be served; got (400, b'{"__type":"SerializationException","message":"missing required header `x-amz-target`"}')

The check first proves the positive control: it creates an SQS queue, tags the queue ARN with rgta.tag_resources (FailedResourcesMap {}), and polls plain JSON-protocol GetResources(TagFilters=[{Key:<k>,Values:[<v>]}]) until it returns exactly {'PaginationToken': '', 'ResourceTagMappingList': [{'ResourceARN': <queue arn>, 'Tags': [{'Key': <k>, 'Value': <v>}]}]} — that succeeds. It then signs and POSTs the identical query to _PATH = /service/ResourceGroupsTaggingAPI_20170126/operation/GetResources with headers {'smithy-protocol': 'rpc-v2-cbor', 'Content-Type': 'application/cbor', 'Accept': 'application/cbor'} and no X-Amz-Target, and asserts 200 + the smithy-protocol / application/cbor response headers + a CBOR body decoding to the same mapping; the 400 above stops it there.

Model evidence: smithy_query(query='service_info', model_path=services/service-com-amazonaws-resourcegroupstaggingapi-resourcegroupstaggingapi-20170126/smithy-model/com-amazonaws-resourcegroupstaggingapi-resourcegroupstaggingapi-20170126.json) -> {'service': 'ResourceGroupsTaggingAPI_20170126', 'protocols': ['awsJson1_1'], 'sigv4Name': 'tagging'}; GetResources is an RPC operation (http POST /GetResources, bindings inert, 'dispatch by X-Amz-Target'), which is exactly the assumption the front door hard-codes.

## Steps to Reproduce
Boot the emulator from this checkout and replay the behavioral suite simparity-behavioral-aws-v1, then re-run resourcegroupstaggingapi.get-resources-rpc-v2-cbor-request-accepted.

1. Create an SQS queue, read its QueueArn, and TagResources(ResourceARNList=[<queue arn>], Tags={'<k>':'<v>'}).
2. Confirm the ordinary awsJson1_1 GetResources(TagFilters=[{Key:'<k>',Values:['<v>']}]) returns the queue mapping with PaginationToken ''.
3. Send a sigv4-signed (service 'tagging') POST to /service/ResourceGroupsTaggingAPI_20170126/operation/GetResources with headers smithy-protocol: rpc-v2-cbor, Content-Type: application/cbor, Accept: application/cbor, NO X-Amz-Target, and a CBOR encoding of {'TagFilters':[{'Key':'<k>','Values':['<v>']}]}.
   Expect: HTTP 200, response headers smithy-protocol: rpc-v2-cbor and content-type: application/cbor, CBOR body decoding to the same mapping step 2 returned.
   Observe: HTTP 400, JSON body {"__type":"SerializationException","message":"missing required header `x-amz-target`"}.
4. Re-issue the JSON query to confirm the CBOR request did not disturb the tag index.
