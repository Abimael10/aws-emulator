Issue-Source: trace-sweep-divergence
Issue-Trace: globalaccelerator-custom-routing-separation
Issue-Step: task#14
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: globalaccelerator-custom-routing-separation
Issue-Shape-Id: com.amazonaws.globalaccelerator#GlobalAccelerator_V20180706
Issue-Key: globalaccelerator|DescribeCustomRoutingEndpointGroup|4xx->2xx|$.Message
Issue-Key: globalaccelerator|DescribeCustomRoutingEndpointGroup|4xx->2xx|$.__type
Issue-Key: globalaccelerator|DescribeCustomRoutingEndpointGroup|4xx->2xx|status
Issue-Commit: 814edd6f7b3ad9f8ffb89a4a014f6c44b204f5ee

# globalaccelerator: DescribeCustomRoutingEndpointGroup returns 200 for an endpoint group that does not exist

> _Note: this issue was identified against the code base at commit `814edd6f7b3ad9f8ffb89a4a014f6c44b204f5ee`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCustomRoutingEndpointGroup

## Description
Cause 1 of 1: DescribeCustomRoutingEndpointGroup answers success for an EndpointGroupArn that names no custom-routing endpoint group, instead of raising the modeled not-found error.

AWS: Smithy: DescribeCustomRoutingEndpointGroup takes a single required input member EndpointGroupArn (GenericString, @length 0..255) and is modeled with the error EndpointGroupNotFoundException (client error, documented as "The endpoint group that you specified doesn't exist."; the model carries httpError 404). The recorded wire response for this operation when the ARN resolves to no custom-routing endpoint group is HTTP 400 with an awsJson1_1 error body whose "__type" is "EndpointGroupNotFoundException" and whose "Message" is "EndpointGroup <EndpointGroupArn> does not exist." In that message the fixed text is "EndpointGroup " and " does not exist."; the only variable part is the endpoint-group ARN echoed from the request's EndpointGroupArn member (in the recording it happened to be arn:aws:globalaccelerator::000000000000:accelerator/db1ba2a2-b8f9-42c3-aebb-492f1560c198/listener/50356dbd/endpoint-group/64d48eec7505 — that value is an example, not part of the requirement). The recorded status for this error class on this operation is 400, so 400 is the requirement here even though the model annotates the shape with httpError 404.

An ARN that belongs to a standard (non custom-routing) accelerator's listener, or to no stored endpoint group at all, is exactly the "doesn't exist" case for this operation: the custom-routing describe surface must only resolve endpoint groups created under a custom-routing accelerator, and anything else is not found.

Emulator: the replayed step got HTTP 200 with a success body. Both error fields are absent from the response: $.__type is missing (expected 'EndpointGroupNotFoundException') and $.Message is missing (expected 'EndpointGroup <arn> does not exist.'). Hypothesis: the handler either does not look the ARN up in state at all before serializing a CustomRoutingEndpointGroup, or it resolves it against the shared/standard endpoint-group store so that a non-custom-routing (or absent) endpoint group still produces a 200 — the fixture this diverges on is specifically about keeping the custom-routing resource tree separate from the standard one.

Why: a client relying on AWS's behavior treats a 200 from DescribeCustomRoutingEndpointGroup as proof that the endpoint group exists and is a custom-routing one. Against the emulator, existence probes and "create if absent" flows silently succeed, SDK error handling for EndpointGroupNotFoundException is never exercised, and code that reads EndpointGroup fields off the response gets a fabricated or wrong-type resource rather than an error. Negative-path tests that assert the not-found error pass incorrectly or fail to raise at all.

Fix: the DescribeCustomRoutingEndpointGroup handler (its state lookup / validation step) must resolve EndpointGroupArn only against custom-routing endpoint groups held in the globalaccelerator state and, when there is no such entry, return the modeled EndpointGroupNotFoundException with HTTP status 400, "__type": "EndpointGroupNotFoundException" and the Message template above filled from the request's EndpointGroupArn, rather than serializing a success response. Existing open globalaccelerator issues (UpdateEndpointGroup ClientIPPreservationEnabled, UpdateAcceleratorAttributes FlowLogsEnabled message, DescribeCustomRoutingAcceleratorAttributes stub) cover other operations and do not cover this one.

Traces: fixture globalaccelerator-custom-routing-separation, step task#14 (cluster of 1 affected step).

## Evidence
Replay of fixture 'globalaccelerator-custom-routing-separation' step 'task#14' (operation DescribeCustomRoutingEndpointGroup, service globalaccelerator), verdict mismatch:
- status: expected 400, got 200
- $.__type: expected 'EndpointGroupNotFoundException', got None (field missing)
- $.Message: expected 'EndpointGroup arn:aws:globalaccelerator::000000000000:accelerator/db1ba2a2-b8f9-42c3-aebb-492f1560c198/listener/50356dbd/endpoint-group/64d48eec7505 does not exist.', got None (field missing)
Affected steps in cluster: globalaccelerator-custom-routing-separation#task#14 (1).
Model evidence: DescribeCustomRoutingEndpointGroup errors = [EndpointGroupNotFoundException (client), InternalServiceErrorException, InvalidArgumentException]; EndpointGroupNotFoundException doc "The endpoint group that you specified doesn't exist." with a single Message member.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire fixture corpus (trace set simparity-wire-aws-v1).
2. Replay fixture 'globalaccelerator-custom-routing-separation' up to step 'task#14', which calls globalaccelerator DescribeCustomRoutingEndpointGroup with an EndpointGroupArn that is not a stored custom-routing endpoint group.
3. Observe the emulator answers HTTP 200 with a success body.
4. Expected: HTTP 400, awsJson1_1 error body with "__type": "EndpointGroupNotFoundException" and "Message": "EndpointGroup <the EndpointGroupArn sent in the request> does not exist."
