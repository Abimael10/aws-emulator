Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.op-contract-modify-traffic-mirror-filter-rule
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.op-contract-modify-traffic-mirror-filter-rule|run#18|AssertionError|assert err == "InvalidParameterValue", f"expected InvalidParameterValue, got {err!r}"
Issue-Commit: e1a20d446b2578340370dda6b22d020378cd7235

# ec2 ModifyTrafficMirrorFilterRule: an IPv4/IPv6 CIDR-family mismatch is refused InvalidParameterCombination instead of AWS's InvalidParameterValue

> _Note: this issue was identified against the code base at commit `e1a20d446b2578340370dda6b22d020378cd7235`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyTrafficMirrorFilterRule

## Description
This issue carries ONE net-new root cause (Cause 1 of 1) in the EC2 emulator's refusal classification on the traffic-mirror filter-rule modify path, observed on the behavioral corpus (simparity-behavioral-aws-v1).

(Related but NOT covering: open issue 20261117T185234Z-ec2-10-more-net-new-causes-getimagefindings-is.md, whose Cause 3/Cause 4 are CreateTrafficMirrorFilterRule's out-of-range RuleNumber and ModifyTrafficMirrorFilterRule's invalid RemoveFields value. Neither is the CIDR-family rule below, and that issue does not carry this check's replay key — the replay marked this failure as owned by nothing — so it is filed here.)

Cause 1 of 1: ModifyTrafficMirrorFilterRule that would leave SourceCidrBlock and DestinationCidrBlock in different IP families (here: a rule created with DestinationCidrBlock 10.0.0.0/24 and SourceCidrBlock 10.0.1.0/24, then modified with DestinationCidrBlock set to an IPv6 prefix such as 2001:db8::/32) is refused with the wrong error code.

AWS: Smithy (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json) models ModifyTrafficMirrorFilterRule with input members TrafficMirrorFilterRuleId (required), DestinationCidrBlock: String and SourceCidrBlock: String (both plain strings, no pattern), and errors: [] — EC2 is ec2Query, so every refusal is a service-level Error/Code in the query error envelope and no error is bound per-operation. Recorded behavior for the refusal itself: HTTP 400, error Code `InvalidParameterValue`, message "SourceCidrBlock and DestinationCidrBlock must both be IPv4 or both be IPv6 CIDR blocks". That whole sentence is FIXED text — it names no value from the request, so there are no variable parts to fill. The recording further requires the refusal to be effect-free: a subsequent DescribeTrafficMirrorFilters must still show the rule's original DestinationCidrBlock (the IPv4 block the request carried at create time), i.e. the rejected update must not persist.

Emulator: answers error Code `InvalidParameterCombination` for the same request (replay: "expected InvalidParameterValue, got 'InvalidParameterCombination'"). Because the check asserts the code before the message, the message text and the effect-absent read-back were never reached, so the message the emulator serves and whether the rejected DestinationCidrBlock is kept out of state are both unverified and must be checked as part of the fix.

Why: the error code is a requirement. A client that branches on `InvalidParameterValue` — the class AWS uses for a single member whose value is unacceptable — takes the fallback/unknown-error path, and retry or remediation logic keyed on AWS's codes misreads a per-value rejection as a combination-of-parameters problem. `InvalidParameterCombination` is AWS's class for mutually exclusive or co-required members, not for an out-of-family CIDR value.

Fix: the ModifyTrafficMirrorFilterRule handler's validation in the EC2 service crate — the same-IP-family check between SourceCidrBlock and DestinationCidrBlock must raise the InvalidParameterValue class with AWS's sentence, and must reject before any state mutation so the persisted rule is untouched.

Traces: ec2.op-contract-modify-traffic-mirror-filter-rule (the rest of that check — CreateTrafficMirrorFilter, CreateTrafficMirrorFilterRule, the RuleAction accept->reject modify, and the DescribeTrafficMirrorFilters read-back of the modified rule — passed; only the CIDR-family refusal diverged).

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout.

[mismatch] ec2.op-contract-modify-traffic-mirror-filter-rule
  check_ec2_op_contract_modify_traffic_mirror_filter_rule.py line 57:
    assert err == "InvalidParameterValue", f"expected InvalidParameterValue, got {err!r}"
  AssertionError: expected InvalidParameterValue, got 'InvalidParameterCombination'

Requirement vs defect:
  Expected (AWS): HTTP 400, Code = InvalidParameterValue, Message = "SourceCidrBlock and DestinationCidrBlock must both be IPv4 or both be IPv6 CIDR blocks" (all fixed text, no variable parts), and the persisted rule still reports its original IPv4 DestinationCidrBlock.
  Actual (emulator): Code = InvalidParameterCombination; message and post-refusal state unverified because the assertion on the code fired first.

The filter id, rule id and generated Name tag in the run are per-run values and are not part of the requirement.

## Steps to Reproduce
1. Build the emulator from this checkout and boot it.
2. ec2 CreateTrafficMirrorFilter(Description=..., TagSpecifications=[{ResourceType: traffic-mirror-filter, ...}]).
3. ec2 CreateTrafficMirrorFilterRule(TrafficMirrorFilterId=<that filter>, TrafficDirection='ingress', RuleNumber=1, RuleAction='accept', DestinationCidrBlock='10.0.0.0/24', SourceCidrBlock='10.0.1.0/24', Protocol=6).
4. ec2 ModifyTrafficMirrorFilterRule(TrafficMirrorFilterRuleId=<that rule>, DestinationCidrBlock='2001:db8::/32') — an IPv6 destination against the rule's IPv4 source.
5. Observe the refusal: Code is InvalidParameterCombination; AWS answers InvalidParameterValue with "SourceCidrBlock and DestinationCidrBlock must both be IPv4 or both be IPv6 CIDR blocks".
6. Then DescribeTrafficMirrorFilters(TrafficMirrorFilterIds=[<filter>]) and confirm the rule's DestinationCidrBlock is still '10.0.0.0/24' (the rejected update must not persist).
7. Re-replay ec2.op-contract-modify-traffic-mirror-filter-rule to confirm the fix.
