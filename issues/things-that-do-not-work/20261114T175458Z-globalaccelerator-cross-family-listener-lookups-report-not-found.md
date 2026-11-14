Issue-Source: trace-sweep-divergence
Issue-Trace: globalaccelerator-custom-routing-separation
Issue-Step: task#9
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: globalaccelerator-custom-routing-separation globalaccelerator-listener-lifecycle
Issue-Shape-Id: com.amazonaws.globalaccelerator#GlobalAccelerator_V20180706
Issue-Key: globalaccelerator|DescribeListener|4xx->4xx|$.Message
Issue-Key: globalaccelerator|DescribeCustomRoutingListener|4xx->4xx|$.Message
Issue-Commit: 161256edb21d00f789595a0968fdc81e8b5da465

# globalaccelerator cross-family listener lookups report not-found instead of the API-separation error

> _Note: this issue was identified against the code base at commit `161256edb21d00f789595a0968fdc81e8b5da465`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeListener

## Description
Two net-new wire divergences in the globalaccelerator emulator, one root cause in each direction: the emulator's LISTENER resolution path is scoped to a single resource family, so a listener ARN that names a real listener of the OTHER family (standard vs custom routing) is reported as simply absent instead of being reported as a resource that exists but belongs to the other API family. In both causes the HTTP status (400) and the error code agree with the recording; the sole diverging member is the error body's $.Message. Global Accelerator is an awsJson1_1 service whose errors carry a human-readable Message that SDKs, the CLI and test assertions surface, so a wrong message is a wire defect even when status and code are right.

Cause 1 of 2: DescribeListener (the standard-listener API) addressed at a CUSTOM ROUTING listener ARN.
AWS: Smithy (services/service-com-amazonaws-globalaccelerator-globalaccelerator-v20180706/smithy-model/com-amazonaws-globalaccelerator-globalaccelerator-v20180706.json) models DescribeListener with a single required input member ListenerArn (GenericString, @length 0..255), output {Listener: Listener}, errors ListenerNotFoundException (httpError 404), InvalidArgumentException (httpError 400), InternalServiceErrorException (500). DescribeCustomRoutingListener is a SEPARATE operation with the same input member but output {Listener: CustomRoutingListener}, so the two listener families are modeled as distinct resource types served by distinct APIs — a custom routing listener ARN names a real resource, not an unknown one. The model does not state the wording, so the message is recorded behavior only: the recording shows HTTP 400 with the error code the emulator also returns and a Message of the form 'Custom Routing resources must use the Custom Routing APIs; got custom routing resource: <listenerArn>'. Fixed text: 'Custom Routing resources must use the Custom Routing APIs; got custom routing resource: '. The single variable part is the ListenerArn taken verbatim from the request. The recorded run's ARN (arn:aws:globalaccelerator::000000000000:accelerator/2115cdcc-ff8a-4cd5-85ed-b462634f162f/listener/6cf3d05a) is a per-run generated identifier filling that slot and is not part of the requirement.
Emulator: returns HTTP 400 with Message of the form 'Listener <listenerArn> not found.' — its generic listener-lookup-miss message — because the standard-listener lookup never sees custom routing listeners in state and treats the ARN as unknown. It never distinguishes 'this ARN names no listener' from 'this ARN names a custom routing listener, which this API does not serve'.
Why: a client gets the wrong diagnosis for a recoverable mistake. The real failure is that the caller used the wrong API family for a listener that genuinely exists; AWS says so and names the offending resource, while the emulator says the listener does not exist. Callers that branch on not-found will go on to (re)create a listener that already exists, or report a phantom deletion, and any handling keyed on the custom-routing separation sentence never fires.
Fix: DescribeListener's handler / the listener resolution path in the globalaccelerator state layer — standard-listener resolution must also consult custom routing listeners and, on a hit, raise the API-separation error with the service's fixed sentence interpolating only the request's ListenerArn, instead of falling through to the generic listener-not-found message.
Traces: globalaccelerator-custom-routing-separation#task#9.

Cause 2 of 2: DescribeCustomRoutingListener (the custom routing API) addressed at a STANDARD listener ARN — the same defect in the opposite direction.
AWS: Smithy models DescribeCustomRoutingListener with required ListenerArn (GenericString, @length 0..255), output {Listener: CustomRoutingListener}, errors ListenerNotFoundException (404), InvalidArgumentException (400), InternalServiceErrorException (500). Wording is recorded behavior only: the recording shows HTTP 400 with the error code the emulator also returns and a Message of the form 'Only Custom Routing resources are supported by this custom routing API; got standard accelerator: <listenerArn>'. Fixed text: 'Only Custom Routing resources are supported by this custom routing API; got standard accelerator: '. The single variable part is the ListenerArn taken verbatim from the request. The recorded ARN (arn:aws:globalaccelerator::000000000000:accelerator/6cbdf920-54ae-40bd-87a1-c0b51f72174d/listener/ef540f3c) is a per-run generated value, not a requirement.
Emulator: returns HTTP 400 with Message of the form 'Listener <listenerArn> not found.', the same generic lookup-miss message, because the custom routing listener lookup is likewise scoped to its own family and does not recognise an existing standard listener.
Why: same client-visible consequence in reverse — a caller that mistakenly used the custom routing API on a standard listener is told the resource is missing rather than that it belongs to the standard API family, so it cannot correct the call and may recreate or re-provision a listener that exists.
Fix: DescribeCustomRoutingListener's handler / the same listener resolution path — custom routing resolution must consult standard listeners and, on a hit, raise the API-separation error with that fixed sentence interpolating only the request's ListenerArn.
Traces: globalaccelerator-listener-lifecycle#task#10.

Relation to already-open work: 20261104T101344Z-globalaccelerator-describeaccelerator-on-a-custom-routing-accelerator-returns.md covers the ACCELERATOR-level case of this defect (DescribeAccelerator on a custom routing accelerator) and carries only the replay key globalaccelerator-custom-routing-separation#task#8; it does not cover either listener-level cause. 20261029T171730Z-globalaccelerator-error-messages-diverge-from-aws-wording-across.md bundles unrelated validation/conflict message wording (CreateAccelerator, CreateEndpointGroup, UpdateEndpointGroup, DeleteListener, CreateListener) and carries no replay key for these two steps. Neither issue covers the two causes filed here.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true, bootstrap_signal false, 7 fixtures replayed, 96 steps total, 42 passed, 46 mismatched, 8 blocked, 12 divergence clusters, score_pct 43.8. Ten of the twelve clusters are owned_by an open issue; these two clusters carry empty owned_by and empty stub_path.

Cause 1 cluster: divergence_key 'service=globalaccelerator|DescribeListener|status=400->400|keys=$.Message', verdict mismatch, service globalaccelerator, operation DescribeListener, fixture_id globalaccelerator-custom-routing-separation, step_key task#9, affected_count 1.
  expected_status 400, actual_status 400 — status and error code agree; the only diverging key is $.Message.
  Expected $.Message: 'Custom Routing resources must use the Custom Routing APIs; got custom routing resource: arn:aws:globalaccelerator::000000000000:accelerator/2115cdcc-ff8a-4cd5-85ed-b462634f162f/listener/6cf3d05a'
  Actual   $.Message: 'Listener arn:aws:globalaccelerator::000000000000:accelerator/2115cdcc-ff8a-4cd5-85ed-b462634f162f/listener/6cf3d05a not found.'
  Fixed vs variable: fixed text is 'Custom Routing resources must use the Custom Routing APIs; got custom routing resource: '; the only variable part is the request's ListenerArn. The ARN/UUID shown is a per-run generated value.

Cause 2 cluster: divergence_key 'service=globalaccelerator|DescribeCustomRoutingListener|status=400->400|keys=$.Message', verdict mismatch, service globalaccelerator, operation DescribeCustomRoutingListener, fixture_id globalaccelerator-listener-lifecycle, step_key task#10, affected_count 1.
  expected_status 400, actual_status 400 — again only $.Message diverges.
  Expected $.Message: 'Only Custom Routing resources are supported by this custom routing API; got standard accelerator: arn:aws:globalaccelerator::000000000000:accelerator/6cbdf920-54ae-40bd-87a1-c0b51f72174d/listener/ef540f3c'
  Actual   $.Message: 'Listener arn:aws:globalaccelerator::000000000000:accelerator/6cbdf920-54ae-40bd-87a1-c0b51f72174d/listener/ef540f3c not found.'
  Fixed vs variable: fixed text is 'Only Custom Routing resources are supported by this custom routing API; got standard accelerator: '; the only variable part is the request's ListenerArn.

Model grounding (smithy_query on services/service-com-amazonaws-globalaccelerator-globalaccelerator-v20180706/smithy-model/com-amazonaws-globalaccelerator-globalaccelerator-v20180706.json): DescribeListener input {ListenerArn: GenericString, required, @length 0..255}, output {Listener: Listener}, errors ListenerNotFoundException(404), InvalidArgumentException(400), InternalServiceErrorException(500); DescribeCustomRoutingListener input {ListenerArn: GenericString, required, @length 0..255}, output {Listener: CustomRoutingListener}, same error set. The distinct output shapes (Listener vs CustomRoutingListener) confirm two separate resource families, so a cross-family ARN names an existing resource and 'not found' is the wrong diagnosis in both directions.

## Steps to Reproduce
1. Clone the repository and boot the emulator built from that checkout.
2. Replay wire trace set simparity-wire-aws-v1.
3. Cause 1: replay fixture globalaccelerator-custom-routing-separation through step task#9. The fixture creates a CUSTOM ROUTING accelerator and a custom routing listener via the Custom Routing APIs, then calls DescribeListener (the standard API) with that custom routing listener's ARN. Inspect the 400 body: expected Message 'Custom Routing resources must use the Custom Routing APIs; got custom routing resource: <the ListenerArn sent in the request>'; actual Message 'Listener <the ListenerArn sent> not found.'. Confirm the listener exists by calling DescribeCustomRoutingListener with the same ARN — it succeeds, so 'not found' is wrong.
4. Cause 2: replay fixture globalaccelerator-listener-lifecycle through step task#10. The fixture creates a STANDARD accelerator and a standard listener, then calls DescribeCustomRoutingListener with that standard listener's ARN. Inspect the 400 body: expected Message 'Only Custom Routing resources are supported by this custom routing API; got standard accelerator: <the ListenerArn sent in the request>'; actual Message 'Listener <the ListenerArn sent> not found.'. Confirm the listener exists by calling DescribeListener with the same ARN.
5. Both causes must be closed; re-replay globalaccelerator-custom-routing-separation#task#9 and globalaccelerator-listener-lifecycle#task#10 to verify.
