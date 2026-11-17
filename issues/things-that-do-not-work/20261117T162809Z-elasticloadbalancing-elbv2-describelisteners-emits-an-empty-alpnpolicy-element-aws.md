Issue-Source: trace-sweep-divergence
Issue-Trace: elbv2-listener-tag-change-and-remove-last-then-not-found
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elbv2-listener-tag-change-and-remove-last-then-not-found
Issue-Shape-Id: com.amazonaws.elasticloadbalancingv2#ElasticLoadBalancing_v10
Issue-Key: elasticloadbalancing|DescribeListeners|2xx->2xx|$.DescribeListenersResponse.DescribeListenersResult.Listeners.member.AlpnPolicy
Issue-Commit: 81e21ca007588719de802b4dd67d83498ccb7823

# ELBv2 DescribeListeners emits an empty AlpnPolicy element AWS omits

> _Note: this issue was identified against the code base at commit `81e21ca007588719de802b4dd67d83498ccb7823`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeListeners

## Description
1 net-new cause on service elasticloadbalancing.

Cause 1 of 1 — DescribeListeners serializes an unset AlpnPolicy as an empty element (fixture elbv2-listener-tag-change-and-remove-last-then-not-found, step task#7).
AWS: Smithy: Listener.AlpnPolicy targets AlpnPolicyName, a list of AlpnPolicyValue, documented '[TLS listener] The name of the Application-Layer Protocol Negotiation (ALPN) policy.' An unset optional member is not serialized at all in the awsQuery/XML response, so for a non-TLS listener the Listeners.member element contains NO AlpnPolicy child. There is no modeled default and no 'empty list' representation. Status is 200 on both sides; no error code or message is involved.
Emulator: emits an AlpnPolicy element whose content is empty, so the member is present with the value '' where AWS omits it.
Why: AlpnPolicy is a list, so a present-but-empty element makes a client see a populated-yet-meaningless member: SDKs deserialize it as a list containing one empty string (or as an explicitly-set empty list), and code that branches on 'is an ALPN policy configured on this listener?' takes the TLS branch for a listener that has no ALPN policy. Round-tripping a described listener back into ModifyListener then sends an invalid AlpnPolicy value.
Fix: the DescribeListeners response serializer for the Listener shape in the elasticloadbalancingv2 crate — omit optional members that hold no value instead of writing an empty element.
Traces: elbv2-listener-tag-change-and-remove-last-then-not-found#task#7.

This is the same serializer class as two already-open issues but neither covers DescribeListeners: 20261030T195554Z-elasticloadbalancing-elbv2-setrulepriorities-response-emits-an-empty-transforms-element.md (SetRulePriorities / Transforms) and 20261030T195633Z-elasticloadbalancing-elb-classic-describeinstancehealth-serializes-instancestates-as-an-empty.md (ELB Classic / InstanceStates). The replay reported this cluster with no owning issue; fixing it in the shared serializer would plausibly close all three.

## Evidence
Replay of trace set simparity-wire-aws-v1, DescribeListeners, fixture elbv2-listener-tag-change-and-remove-last-then-not-found step task#7, status 200 expected and 200 actual:
  $.DescribeListenersResponse.DescribeListenersResult.Listeners.member.AlpnPolicy: unexpected_present (expected None, got '')

Model evidence (services/service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10/smithy-model):
  Listener.AlpnPolicy -> AlpnPolicyName (type list, member AlpnPolicyValue), doc '[TLS listener] The name of the Application-Layer Protocol Negotiation (ALPN) policy.'
  DescribeListeners output: Listeners (list), NextMarker; errors ListenerNotFound, LoadBalancerNotFound, UnsupportedProtocol

## Steps to Reproduce
1. git_clone() and boot the emulator built from the checkout.
2. Replay fixture elbv2-listener-tag-change-and-remove-last-then-not-found up to step task#7: create a non-TLS (HTTP) listener, then call DescribeListeners for it.
3. Inspect the XML under DescribeListenersResponse.DescribeListenersResult.Listeners.member: the emulator writes an empty <AlpnPolicy/> child; AWS writes no AlpnPolicy child at all.
4. Re-run replay_wire_corpus() and confirm the cluster passes.
