Issue-Source: trace-sweep-divergence
Issue-Trace: elb-catalog-describe-load-balancer-policies
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elb-catalog-describe-load-balancer-policies
Issue-Shape-Id: com.amazonaws.elasticloadbalancing#ElasticLoadBalancing_v7
Issue-Key: elb|DescribeLoadBalancerPolicies|2xx->2xx|$.DescribeLoadBalancerPoliciesResponse.DescribeLoadBalancerPoliciesResult.PolicyDescriptions.member[*]
Issue-Commit: 384f5f940fcbeb20a0d7a3c3cf2d9778f9da2b04

# elb: DescribeLoadBalancerPolicies describes none of the predefined sample policies when the request names no load balancer

> _Note: this issue was identified against the code base at commit `384f5f940fcbeb20a0d7a3c3cf2d9778f9da2b04`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeLoadBalancerPolicies

## Description
Cause 1 of 1: a DescribeLoadBalancerPolicies request that names no load balancer is answered 200 with a PolicyDescriptions list that contains none of the predefined sample-policy descriptions AWS returns — the replay reports every one of the recording's eleven members, member[0] through member[10], as 'missing' from the emulator's response.

AWS: Smithy (com.amazonaws.elasticloadbalancing#ElasticLoadBalancing_v7): DescribeLoadBalancerPolicies input DescribeLoadBalancerPoliciesInput {LoadBalancerName (AccessPointName), PolicyNames (list)}, output DescribeLoadBalancerPoliciesOutput {PolicyDescriptions: list<PolicyDescription>}; modeled errors AccessPointNotFoundException (awsQueryError LoadBalancerNotFound, 400) and PolicyNotFoundException (PolicyNotFound, 400). The operation's documentation in that model states the rule directly: "If you don't specify a load balancer name, the action returns descriptions of the specified sample policies, or descriptions of all sample policies. The names of the sample policies have the ELBSample- prefix." Each PolicyDescription carries PolicyName, PolicyTypeName and PolicyAttributeDescriptions (a list of {AttributeName, AttributeValue}), and the recording shows the whole attribute set of the SSL negotiation policy type reported per policy: the protocol attributes (Protocol-TLSv1, Protocol-SSLv3, Protocol-TLSv1.1, Protocol-TLSv1.2), Server-Defined-Cipher-Order, and one attribute per published cipher suite (ECDHE-*, AES*, DHE-*, CAMELLIA128-SHA, ...), each valued 'true' or 'false' according to whether that sample's configuration enables it — never omitted. Which protocols and suites each sample enables, and the order of the attribute rows, is recorded behavior (the published predefined-policy table), not something the model states.

Emulator: status matches (200) and the response envelope is well formed, but $.DescribeLoadBalancerPoliciesResponse.DescribeLoadBalancerPoliciesResult.PolicyDescriptions carries no member matching any of the eleven recorded descriptions: the replay grades member[0] .. member[10] 'missing', with member[0] expected to be a full SSLNegotiationPolicyType description (Protocol-TLSv1 true, Protocol-SSLv3 false, Protocol-TLSv1.1 true, Protocol-TLSv1.2 true, Server-Defined-Cipher-Order true, the ECDHE/AES suites true, DHE-RSA-AES128-SHA false, CAMELLIA128-SHA false, ...). So a caller asking for the account-wide sample catalogue is handed an answer that describes no policy at all. Hypothesis: the catalogue branch of the handler either projects nothing onto the wire for this request shape, or projects entries under names/keys that do not correspond to the ELBSample- catalogue the recording returns — the checkout does define a catalogue of eleven SSL negotiation samples (src/state/sample_policies.rs, SAMPLE_POLICIES lines 243-343) whose attribute values match member[0]'s recorded values, which is why the divergence reads as absent members rather than as differing attribute values.

Why: the sample policies are how a client discovers which SSL negotiation configurations it may reference before creating one — an SDK or console flow listing available security policies, or a deployment that validates a policy name against the catalogue, sees an empty catalogue and concludes the account has no selectable negotiation policy, so it either offers nothing to choose or fails its pre-flight check. A client that then names a documented ELBSample- policy gets PolicyNotFound from the same service that just reported no policies exist.

Fix: the no-LoadBalancerName (sample catalogue) branch of the DescribeLoadBalancerPolicies handler and the projection of its catalogue onto PolicyDescriptions — the handler must answer that request with one PolicyDescription per predefined sample policy, each named with the documented ELBSample- prefix, of the SSL negotiation policy type, and carrying the complete attribute set of that policy type with each attribute's enabled/disabled value, filtered only by any PolicyNames the request named.

Traces: elb-catalog-describe-load-balancer-policies#task#1. Code: services/service-com-amazonaws-elasticloadbalancing-elasticloadbalancing-v7/src/ops/describe_load_balancer_policies.rs (the sample branch, lines 168-197, and describe_sample lines 287-302), src/state/sample_policies.rs (SAMPLE_POLICIES lines 243-343, ssl_negotiation_attribute_names lines 95-101).

No filed issue covers this: the replay marks the cluster owned_by [] and stub_path empty. The open ELB/ELBv2 issues found by a keyword search are about the data plane, published DNS names and target health, not about the sample-policy catalogue.

## Evidence
Replay of simparity-wire-aws-v1 against this checkout: 166 steps, 154 passed, 12 mismatched, score 92.8%, emulator_up true.

Cluster representative elb-catalog-describe-load-balancer-policies#task#1, service elb, operation DescribeLoadBalancerPolicies, verdict mismatch, expected_status 200 / actual_status 200, affected_count 1, owned_by [], stub_path '':
  divergence_key: service=elb|DescribeLoadBalancerPolicies|status=200->200|keys=$.DescribeLoadBalancerPoliciesResponse.DescribeLoadBalancerPoliciesResult.PolicyDescriptions.member[0] .. member[10]  (eleven keys, member[0] through member[10])
  detail: $.DescribeLoadBalancerPoliciesResponse.DescribeLoadBalancerPoliciesResult.PolicyDescriptions.member[0]: missing (expected {'PolicyAttributeDescriptions': {'member': [{'AttributeValue': 'true', 'AttributeName': 'Protocol-TLSv1'}, {'AttributeValue': 'false', 'AttributeName': 'Protocol-SSLv3'}, {'AttributeValue': 'true', 'AttributeName': 'Protocol-TLSv1.1'}, {'AttributeValue': 'true', 'AttributeName': 'Protocol-TLSv1.2'}, {'AttributeValue': 'true', 'AttributeName': 'Server-Defined-Cipher-Order'}, {'AttributeValue': 'true', 'AttributeName': 'ECDHE-ECDSA-AES128-GCM-SHA256'}, ... {'AttributeValue': 'false', 'AttributeName': 'DHE-RSA-AES128-SHA'}, ... {'AttributeValue': 'false', 'AttributeName': 'DHE-DSS-AES128-SHA'}, {'AttributeValue': 'false', 'AttributeName': 'CAMELLIA128-SHA'}, ...]}})

Expected (AWS): 200 with eleven PolicyDescriptions members, one per predefined ELBSample- sample policy, each of the SSL negotiation policy type and each carrying the complete attribute set of that type with per-attribute 'true'/'false' values.
Actual (emulator): 200 with a PolicyDescriptions list in which none of those eleven members is present.
Model evidence: DescribeLoadBalancerPolicies -> DescribeLoadBalancerPoliciesOutput {PolicyDescriptions}; PolicyDescription {PolicyName, PolicyTypeName, PolicyAttributeDescriptions}; PolicyAttributeDescription {AttributeName, AttributeValue}; documentation 'If you don't specify a load balancer name, the action returns descriptions of the specified sample policies, or descriptions of all sample policies. The names of the sample policies have the ELBSample- prefix.'

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Replay fixture elb-catalog-describe-load-balancer-policies, step task#1: a DescribeLoadBalancerPolicies Query call that names no LoadBalancerName (the account-wide sample-policy catalogue request).
3. Read DescribeLoadBalancerPoliciesResult.PolicyDescriptions in the 200 response: none of the eleven recorded sample-policy members is present (the replay grades member[0] through member[10] 'missing').
4. Expected after a fix: the same request answers 200 with one PolicyDescription per predefined sample policy — PolicyName carrying the ELBSample- prefix, PolicyTypeName SSLNegotiationPolicyType, and PolicyAttributeDescriptions listing every attribute of that policy type with its 'true'/'false' value, in the published attribute order — and a request naming a subset in PolicyNames returns exactly those, while a name outside the catalogue is still refused PolicyNotFound (400).
