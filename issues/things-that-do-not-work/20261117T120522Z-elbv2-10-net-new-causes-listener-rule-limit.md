Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2.create-listener-forward-action-target-group-limit-rejected elbv2.create-rule-forward-action-target-group-limit-rejected elbv2.create-rule-forward-target-groups-exceeding-limit-rejected elbv2.create-listener-network-load-balancer-listener-limit-rejected elbv2.create-load-balancer-same-az-subnets-rejected elbv2.create-rule-condition-wildcards-exceeding-limit-rejected elbv2.create-rule-malformed-regex-value-rejected elbv2.create-rule-two-routing-actions-rejected elbv2.describe-rules-without-listener-or-rule-arn-rejected elbv2.set-rule-priorities-mixed-batch-with-deleted-rule-rejected elbv2.xamzn-trace-id-generated-and-preserved-toward-the-target elbv2.regex-condition-matches-and-falls-through-on-near-miss elbv2.regex-conditions-route-http-and-header-matches
Issue-Shape-Id: com.amazonaws.elasticloadbalancingv2#ElasticLoadBalancing_v10
Issue-Key: elbv2.create-listener-forward-action-target-group-limit-rejected|_error#7|AssertionError|raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Key: elbv2.create-rule-forward-action-target-group-limit-rejected|_error#9|AssertionError|raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Key: elbv2.create-rule-forward-target-groups-exceeding-limit-rejected|_refused#14|AssertionError|raise AssertionError(f"expected the rule to be refused, but it was created: {result!r}")
Issue-Key: elbv2.create-listener-network-load-balancer-listener-limit-rejected|_refused#16|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: elbv2.create-load-balancer-same-az-subnets-rejected|run#9|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {unexpected!r}")
Issue-Key: elbv2.create-rule-condition-wildcards-exceeding-limit-rejected|_refused#11|AssertionError|raise AssertionError(f"expected the rule to be refused, but it was created: {result!r}")
Issue-Key: elbv2.create-rule-malformed-regex-value-rejected|_error#11|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: elbv2.create-rule-two-routing-actions-rejected|run#11|AssertionError|assert message == "Action types 'redirect,forward' cannot be specified together", f"MESSAGE={message!r}"
Issue-Key: elbv2.describe-rules-without-listener-or-rule-arn-rejected|run#11|AssertionError|assert message == "You must specify either listener rule ARNs or a listener ARN", f"MESSAGE={message!r}"
Issue-Key: elastic-load-balancing-v2|SetRulePriorities|RuleNotFound
Issue-Key: elbv2.xamzn-trace-id-generated-and-preserved-toward-the-target|run#16|AssertionError|assert generated is not None, "the target received no X-Amzn-Trace-Id header at all"
Issue-Key: elbv2.regex-condition-matches-and-falls-through-on-near-miss|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Key: elbv2.regex-conditions-route-http-and-header-matches|_poll_until#25|Timeout|time.sleep(interval_s)
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# elbv2: 10 net-new causes — listener/rule limit and condition validation is missing, four refusal messages are emulator-authored, and the ALB adds no X-Amzn-Trace-Id

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRule

## Description
Ten net-new root causes in the ELBv2 control plane and ALB data plane.

Cause 1 of 10 — a forward action naming more than 5 target groups is accepted (CreateListener and CreateRule).
AWS: Smithy — `CreateListener.DefaultActions` / `CreateRule.Actions` (Actions -> Action.ForwardConfig.TargetGroups); a forward action may name at most 5 target groups, and AWS refuses a sixth with error code `ValidationError` and message `You cannot specify more than 5 target groups per action` (entirely fixed text; the limit 5 is the documented service limit, not a per-run value). Exactly 5 is accepted.
Emulator: both calls SUCCEED with six target groups — CreateListener returns a listener on the extra port and CreateRule returns a rule with all six TargetGroups echoed, each Weight 1.
Why: a client's weighted blue/green or canary fan-out silently exceeds the service limit; the configuration has no AWS equivalent, and the listener/rule that AWS refuses is left in place and routed to.
Fix: forward-action validation shared by the CreateListener / CreateRule (and Modify*) handlers.
Traces: elbv2.create-listener-forward-action-target-group-limit-rejected, elbv2.create-rule-forward-action-target-group-limit-rejected, elbv2.create-rule-forward-target-groups-exceeding-limit-rejected.

Cause 2 of 10 — the per-load-balancer listener limit is not enforced.
AWS: Smithy — `CreateListener` error `TooManyListenersException` (awsQueryError `TooManyListeners`, HTTP 400). The 51st listener on one load balancer is refused with code `TooManyListeners` and message `The maximum number of listeners on load balancer '<LoadBalancerArn>' has been reached` — fixed text around the load balancer ARN taken from the request.
Emulator: the 51st CreateListener SUCCEEDS (200, a new listener ARN on port 51).
Why: a client that expects the quota to bound its listener fan-out gets unbounded growth here and a hard refusal in AWS; quota-handling and back-off code paths are never exercised.
Fix: the CreateListener handler's listener-count check against the load balancer's state.
Traces: elbv2.create-listener-network-load-balancer-listener-limit-rejected.

Cause 3 of 10 — two subnets in the same Availability Zone are accepted for one load balancer.
AWS: Smithy — `CreateLoadBalancer.Subnets` and error `InvalidConfigurationRequestException` (awsQueryError `InvalidConfigurationRequest`, HTTP 400). AWS refuses a load balancer attached to two subnets in one AZ with code `InvalidConfigurationRequest` and message `A load balancer cannot be attached to multiple subnets in the same Availability Zone` (entirely fixed text).
Emulator: the call SUCCEEDS and DescribeLoadBalancers reports two AvailabilityZones entries with the SAME ZoneName and different SubnetIds.
Why: a client building subnets programmatically never learns its AZ layout is invalid, and the described load balancer carries a zone list AWS can never produce, so anything deriving one-subnet-per-AZ from it misbehaves.
Fix: CreateLoadBalancer subnet validation (and SetSubnets) in the ELBv2 handler.
Traces: elbv2.create-load-balancer-same-az-subnets-rejected.

Cause 4 of 10 — a condition value with more than 6 wildcards is accepted.
AWS: `CreateRule.Conditions` -> PathPatternConfig.Values; a rule may hold at most 6 condition wildcards, and a seventh is refused with code `ValidationError` and message `A rule can only have '6' condition wildcards` — fixed text, the quoted 6 being the documented limit. A six-wildcard value is accepted and echoed back unchanged.
Emulator: the seven-wildcard rule is CREATED (priority 1, value echoed in PathPatternConfig.Values).
Why: the rule exceeds the matching engine's documented capacity, so a client's routing table is accepted here and rejected in AWS; the negative control passes.
Fix: condition-wildcard counting in CreateRule/ModifyRule validation.
Traces: elbv2.create-rule-condition-wildcards-exceeding-limit-rejected.

Cause 5 of 10 — a malformed RegexValues condition is accepted.
AWS: `CreateRule.Conditions` -> PathPatternConfig.RegexValues; AWS compiles the regex and refuses an invalid one with code `ValidationError` and message `One or more provided condition regex values are not valid` (entirely fixed text — the offending pattern is not interpolated). No rule is created.
Emulator: a rule with RegexValues ['^/bad/(unclosed'] — an unclosed group — is CREATED and echoed back.
Why: the client learns at request time (or never) that its routing regex is broken instead of at configuration time; the rule either matches nothing or the data plane fails later, and the configuration cannot be reproduced in AWS.
Fix: regex compilation/validation in the CreateRule/ModifyRule condition validator.
Traces: elbv2.create-rule-malformed-regex-value-rejected.

Cause 6 of 10 — the two-routing-actions refusal message is emulator-authored.
AWS: `CreateRule.Actions`; a rule naming two routing actions is refused with message `Action types 'redirect,forward' cannot be specified together` — fixed text around the comma-joined action types taken from the request's Actions.
Emulator: refuses, but with `A rule must include exactly one of the following actions: forward, fixed-response, redirect` — a different message that names no offending types.
Why: the refusal is correct but its text is not AWS's, so a client cannot tell WHICH actions clashed from the message, and anything matching on the message diverges.
Fix: the CreateRule action-validation message in the ELBv2 handler.
Traces: elbv2.create-rule-two-routing-actions-rejected.

Cause 7 of 10 — the DescribeRules missing-selector refusal message is emulator-authored.
AWS: Smithy — `DescribeRules` input (ListenerArn, RuleArns both optional in the model); with neither supplied AWS refuses with code `ValidationError` and message `You must specify either listener rule ARNs or a listener ARN` (entirely fixed text).
Emulator: refuses with `You must specify either ListenerArn or RuleArns` — AWS's member names rather than AWS's wording.
Why: the code is right and the message is not; clients and operators matching the message see a string AWS never emits.
Fix: the DescribeRules input-validation message.
Traces: elbv2.describe-rules-without-listener-or-rule-arn-rejected.

Cause 8 of 10 — the SetRulePriorities RuleNotFound message is emulator-authored.
AWS: Smithy — `SetRulePriorities` error `RuleNotFoundException` (awsQueryError `RuleNotFound`, HTTP 400). A batch naming a deleted rule's ARN is refused with code `RuleNotFound` and message `One or more rules not found` (entirely fixed text), whether that entry is alone or paired with a live rule's move, and the live rule's priority is left untouched.
Emulator: the code is right but the message is `One or more priorities not found` — it blames priorities, not rules.
Why: the message misdescribes the failure: a client reading it looks for a bad priority rather than a stale rule ARN, and message-level handling diverges from AWS.
Fix: the SetRulePriorities not-found message in the ELBv2 handler.
Traces: elbv2.set-rule-priorities-mixed-batch-with-deleted-rule-rejected.

Cause 9 of 10 — the ALB adds no X-Amzn-Trace-Id header toward the target.
AWS: recorded behavior only (the header is not modeled): an Application Load Balancer generates an `X-Amzn-Trace-Id` header (`Root=1-<hex>-<hex>`) for a request that arrives without one and forwards it to the target, and preserves a client-supplied value instead of replacing it.
Emulator: the target receives NO X-Amzn-Trace-Id header at all.
Why: request tracing across an ALB is impossible — a target or downstream service that correlates logs and traces on X-Amzn-Trace-Id has nothing to correlate on, and code that reads the header to continue a trace sees it missing on every request.
Fix: the ALB data-plane request-forwarding path — generate X-Amzn-Trace-Id when absent and pass a supplied one through unchanged.
Traces: elbv2.xamzn-trace-id-generated-and-preserved-toward-the-target.

Cause 10 of 10 — regex rule conditions never produce a routed response, so the regex data plane reaches no verdict.
AWS: `CreateRule.Conditions` with PathPatternConfig.RegexValues / HttpHeaderConfig; a request matching the regex is routed to that rule's action and a near-miss falls through to the listener default.
Emulator: both regex data-plane checks are cut off — `Check timed out after 60.0s` — so no response is ever served through the regex-conditioned listener and neither the match nor the fall-through can be observed.
Why: a client that routes on regex conditions gets no traffic served at all; the rules are accepted by the control plane but the data plane behind them never answers, so the routing contract is unverifiable and in practice broken.
Fix: the ALB data-plane rule-matching path for regex conditions (and the listener's answer at the served endpoint).
Traces: elbv2.regex-condition-matches-and-falls-through-on-near-miss, elbv2.regex-conditions-route-http-and-header-matches.
Related but not covering: 20261116T083232Z-elbv2-2-net-new-causes-the-alb-data.md and 20261112T140934Z-elbv2-7-net-new-causes-loadbalancer-dnsname-is.md record other ALB data-plane non-answers; neither carries these two regex checks.

Related but not covering the above: 20261104T094656Z-elbv2-ten-overly-permissive-control-plane-gaps-calls.md collects a different set of overly-permissive control-plane gaps (SSL policy, concurrent listener creation, IP pools, target-group attribute legality); none of the ten causes here is among them.

## Evidence
Cause 1: AssertionError: expected the call to be rejected, but it SUCCEEDED and returned {'Listeners': [{... 'ForwardConfig': {'TargetGroups': [6 entries, each Weight 1] ...}}]} / {'Rules': [{... 'ForwardConfig': {'TargetGroups': [6 entries]}}]}. Expected ('ValidationError', 'You cannot specify more than 5 target groups per action') and only the accepted 5-group listener/rule present.

Cause 2: AssertionError: expected the call to be refused, but it succeeded: {'Listeners': [{'ListenerArn': '…/net/…', 'Port': 51, 'Protocol': 'TCP', …}], 'ResponseMetadata': {'HTTPStatusCode': 200}}. Expected ('TooManyListeners', "The maximum number of listeners on load balancer '<lb arn>' has been reached").

Cause 3: AssertionError: expected the call to be refused, but it succeeded: {'LoadBalancerArn': …, 'AvailabilityZones': [{'ZoneName': 'us-west-2a', 'SubnetId': 'subnet-…'}, {'ZoneName': 'us-west-2a', 'SubnetId': 'subnet-…'}], …}. Expected ('InvalidConfigurationRequest', 'A load balancer cannot be attached to multiple subnets in the same Availability Zone').

Cause 4: AssertionError: expected the rule to be refused, but it was created: {'Rules': [{'Priority': '1', 'Conditions': [{'Field': 'path-pattern', 'PathPatternConfig': {'Values': ['/w0*w1*w2*w3*w4*w5*w6*w7']}}], …}]}. Expected ('ValidationError', "A rule can only have '6' condition wildcards").

Cause 5: AssertionError: expected the call to be rejected, but it succeeded: {'Rules': [{'Conditions': [{'PathPatternConfig': {'RegexValues': ['^/bad/(unclosed']}}], …}]}. Expected ('ValidationError', 'One or more provided condition regex values are not valid').

Cause 6: AssertionError: MESSAGE='A rule must include exactly one of the following actions: forward, fixed-response, redirect'. Expected "Action types 'redirect,forward' cannot be specified together".

Cause 7: AssertionError: MESSAGE='You must specify either ListenerArn or RuleArns'. Expected 'You must specify either listener rule ARNs or a listener ARN'.

Cause 8: AssertionError: expected ('RuleNotFound', 'One or more rules not found'), got ('RuleNotFound', 'One or more priorities not found').

Cause 9: AssertionError: the target received no X-Amzn-Trace-Id header at all. Expected a generated Root=1-<hex>-<hex> value when the client sends none, and the client's value preserved when it does.

Cause 10: Error: Check timed out after 60.0s (both regex checks). Expected the regex-matching request to be served by the matching rule's action and the near-miss to fall through to the listener default.

All ARNs, names, ports, request ids and timestamps quoted above are per-run values.

## Steps to Reproduce
Common setup: create a VPC with two subnets in different AZs and a security group.

Cause 1: create 6 target groups and an internal ALB; CreateListener (and separately CreateRule on an existing listener) with a forward action whose ForwardConfig.TargetGroups names all 6. Observed: accepted. Expected: ValidationError 'You cannot specify more than 5 target groups per action'; exactly 5 accepted.
Cause 2: create an NLB and 50 TCP listeners on ports 1..50; CreateListener on port 51. Observed: accepted. Expected: TooManyListeners.
Cause 3: create two subnets in the SAME AZ; CreateLoadBalancer Type='application', Scheme='internal' with both. Observed: accepted, two same-ZoneName AvailabilityZones. Expected: InvalidConfigurationRequest.
Cause 4: CreateRule with PathPatternConfig.Values=['/w0*w1*w2*w3*w4*w5*w6*w7'] (7 wildcards). Observed: created. Expected: ValidationError "A rule can only have '6' condition wildcards"; the six-wildcard value accepted and echoed.
Cause 5: CreateRule with PathPatternConfig.RegexValues=['^/bad/(unclosed']. Observed: created. Expected: ValidationError 'One or more provided condition regex values are not valid'.
Cause 6: CreateRule with Actions=[{Type:'redirect',…},{Type:'forward',…}]. Observed: refused with the wrong text. Expected "Action types 'redirect,forward' cannot be specified together".
Cause 7: call DescribeRules with neither ListenerArn nor RuleArns. Observed: wrong message. Expected 'You must specify either listener rule ARNs or a listener ARN'.
Cause 8: create two rules, delete one, then SetRulePriorities naming the deleted rule's ARN (alone, and paired with a live rule's move). Observed message 'One or more priorities not found'. Expected 'One or more rules not found', with the live rule's priority untouched.
Cause 9: create an ALB with a Lambda target that echoes its headers; GET through the listener with no X-Amzn-Trace-Id, then with one. Observed: no such header reaches the target. Expected: generated when absent, preserved when supplied.
Cause 10: create rules with regex path-pattern and http-header conditions plus fixed-response actions; GET a matching path and a near-miss. Observed: the check is killed at 60 s with no response served. Expected: the matching rule's action, and fall-through to the default for the near-miss.
