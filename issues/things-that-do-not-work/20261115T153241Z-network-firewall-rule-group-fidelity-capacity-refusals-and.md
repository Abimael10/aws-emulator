Issue-Source: trace-sweep-divergence
Issue-Trace: network-firewall-nfwr-traj-rule-group-capacity-and-type-mistakes
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: network-firewall-nfwr-traj-rule-group-capacity-and-type-mistakes network-firewall-nfwr-traj-suricata-rule-syntax-refusals network-firewall-nfwr-traj-stateless-rule-priority-mistakes
Issue-Shape-Id: com.amazonaws.networkfirewall#NetworkFirewall_20201112
Issue-Key: network-firewall|CreateRuleGroup|4xx->4xx|$.Message
Issue-Key: network-firewall|CreateRuleGroup|4xx->4xx|$.__type
Issue-Key: network-firewall|DescribeRuleGroup|2xx->2xx|$.RuleGroup.RulesSource.StatelessRulesAndCustomActions.StatelessRules[*].Priority
Issue-Key: network-firewall|DescribeRuleGroup|2xx->2xx|$.RuleGroup.RulesSource.StatelessRulesAndCustomActions.StatelessRules[*].RuleDefinition.MatchAttributes.Sources[*].AddressDefinition
Issue-Commit: 793cbc9bbe6ac7027e5bacd4220c2c28231dad7a

# network-firewall rule group fidelity: capacity refusals and stateless rule order diverge (3 causes)

> _Note: this issue was identified against the code base at commit `793cbc9bbe6ac7027e5bacd4220c2c28231dad7a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRuleGroup

## Description
Three net-new causes in the emulator's rule-group surface: the two capacity refusals CreateRuleGroup raises carry the wrong member name / wrong error class, and a stateless rule group is read back with its StatelessRules entries in the order they were submitted rather than in the order AWS reports them. All three are independent and must each be closed. None is covered by an open issue; the open network-firewall issues (20261102T120026Z-network-firewall-createfirewall-skips-vpc-and-subnet-reachability-validation-so.md, 20261102T120026Z-network-firewall-createfirewallpolicy-does-not-validate-statefuldefaultactions-against-the-policy.md, 20261114T152222Z-network-firewall-createfirewallpolicy-reports-a-zero-statelessrulegroupreferences-priority-as-a.md) are about CreateFirewall placement validation and CreateFirewallPolicy validation and own their own replay keys.

Cause 1 of 3: CreateRuleGroup's capacity-exceeded refusal names RulesString instead of StatefulRules when the rules arrive in the top-level Rules string.
AWS: Smithy: CreateRuleGroup.Rules (RulesString, @length 0..2000000) is documented as "A string containing stateful rule group rules specifications in Suricata flat format, with one rule per line... The call response returns a RuleGroup object that Network Firewall has populated from your string" — i.e. AWS converts the flat string into RuleGroup.RulesSource.StatefulRules (RulesSource$StatefulRules, list of StatefulRule) and accounts capacity against that member. When the rules need more capacity than CreateRuleGroup.Capacity allows, AWS answers 400 InvalidRequestException whose message is built from the request: the fixed text is "<member> capacity exceeded, parameter: [<required-capacity>], context: RulesSource.<member>", where <member> is the RulesSource member the parsed rules live in (StatefulRules for a Rules string) and <required-capacity> is the capacity those rules actually consume, taken from the request. Recorded response: 400, message "StatefulRules capacity exceeded, parameter: [1], context: RulesSource.StatefulRules" (the "1" is that one recording's computed requirement, not part of the template).
Emulator: answers 400 but with message "RulesString capacity exceeded, parameter: [1], context: RulesSource.RulesString" — the correct status and error class, the wrong member in both the stem and the context path.
Why: a client that parses the context path to learn which part of its request to shrink (or a test asserting on the refusal) is pointed at RulesSource.RulesString, a member AWS never names for a Rules-string request, so the remediation path and any message assertion break even though the capacity arithmetic agreed.
Fix: CreateRuleGroup's handler must build the rule-group document from the Rules string the way AWS does — parse the Suricata flat format into RulesSource.StatefulRules instead of retaining it as RulesSource.RulesString — so the capacity refusal built in the service's state module (the rules-source-member helper feeding the capacity-exceeded message) names StatefulRules. The same message is built on the UpdateRuleGroup path and must change with it.
Traces: network-firewall-nfwr-traj-rule-group-capacity-and-type-mistakes#task#3 (representative), plus #task#4 and network-firewall-nfwr-traj-suricata-rule-syntax-refusals#task#4 in the same cluster.

Cause 2 of 3: CreateRuleGroup refuses an over-large Capacity with LimitExceededException and an emulator-authored message instead of InvalidRequestException with AWS's capacity-exceeded message.
AWS: Smithy: CreateRuleGroup.Capacity (RuleCapacity, required, no modeled range); the operation models both InvalidRequestException (HTTP 400, "The operation failed because of a problem with your request... You specified an unsupported parameter name or value") and LimitExceededException (HTTP 400). For a Capacity above the service maximum, the recording shows AWS using InvalidRequestException with the same built shape as Cause 1: fixed text "Capacity capacity exceeded, parameter: [<requested-capacity>], context: Capacity", where <requested-capacity> is the Capacity value from the request. Recorded response: 400, __type com.amazonaws.networkfirewall.customerapi.v20201112#InvalidRequestException, message "Capacity capacity exceeded, parameter: [30001], context: Capacity" (30001 is the request's own value filling the template).
Emulator: 400 with __type "LimitExceededException" and message "This emulator allows at most 30000 capacity units per rule group; this request asked for 30001." — the ceiling itself agrees, but the error code is the wrong one and the message is emulator-authored prose that names the emulator rather than AWS's parameter/context shape.
Why: a client branches on the error code — LimitExceededException reads as an account/quota condition to raise or retry against, while InvalidRequestException is a non-retryable request defect — so error-handling and message assertions diverge on a request AWS rejects as malformed input.
Fix: in CreateRuleGroup's handler, replace the MAX_RULE_GROUP_CAPACITY LimitExceededException branch with the modeled InvalidRequestException carrying the state module's parameter/context message built as "Capacity capacity exceeded" with the requested capacity in the brackets and context "Capacity"; apply the same on UpdateRuleGroup if it enforces the same ceiling.
Traces: network-firewall-nfwr-traj-rule-group-capacity-and-type-mistakes#task#5.

Cause 3 of 3: DescribeRuleGroup returns a stateless rule group's StatelessRules in submission order instead of the order AWS reports.
AWS: recorded behavior only — the model puts no ordering trait on RulesSource$StatelessRulesAndCustomActions / StatelessRules (StatelessRules is a plain list of StatelessRule), but StatelessRule$Priority is documented as "Indicates the order in which to run this rule relative to all of the rules that are defined for a stateless rule group. Network Firewall evaluates the rules in a rule group starting with the lowest priority setting" (@range 1..65535, unique within the group). The recording shows DescribeRuleGroup returning the entries lowest-Priority-first regardless of the order they were supplied in: StatelessRules[0].Priority 10 (its RuleDefinition.MatchAttributes.Sources[0].AddressDefinition 10.1.0.0/16) before StatelessRules[1].Priority 20 (10.3.0.0/16). Hypothesis: AWS normalizes the stored rule list by ascending Priority when the group is written.
Emulator: 200 with the list in the order the create/update request supplied it — StatelessRules[0].Priority 20 (source 10.3.0.0/16) and StatelessRules[1].Priority 10 (source 10.1.0.0/16), i.e. the two entries transposed relative to AWS. Each rule's own Priority and RuleDefinition stay consistent; only the list order differs.
Why: a client that reads the group's rules positionally — rendering them in evaluation order, diffing a describe against a desired configuration, or asserting StatelessRules[0] is the first rule to run — sees the rules in the wrong evaluation order, and an idempotent apply loop sees a spurious diff and rewrites the rule group forever.
Fix: order StatelessRules by ascending Priority where the rule group document is stored (CreateRuleGroup / UpdateRuleGroup handlers, or the rule-group record's projection in the service's state module), so DescribeRuleGroup — which returns the stored document verbatim — reports evaluation order.
Traces: network-firewall-nfwr-traj-stateless-rule-priority-mistakes#task#7.

## Evidence
Cause 1 — fixture network-firewall-nfwr-traj-rule-group-capacity-and-type-mistakes, step task#3, CreateRuleGroup: expected 400 / $.Message "StatefulRules capacity exceeded, parameter: [1], context: RulesSource.StatefulRules"; actual 400 / $.Message "RulesString capacity exceeded, parameter: [1], context: RulesSource.RulesString". Same cluster: #task#4, network-firewall-nfwr-traj-suricata-rule-syntax-refusals#task#4.

Cause 2 — fixture network-firewall-nfwr-traj-rule-group-capacity-and-type-mistakes, step task#5, CreateRuleGroup: expected 400 / $.__type "com.amazonaws.networkfirewall.customerapi.v20201112#InvalidRequestException" / $.Message "Capacity capacity exceeded, parameter: [30001], context: Capacity"; actual 400 / $.__type "LimitExceededException" / $.Message "This emulator allows at most 30000 capacity units per rule group; this request asked for 30001."

Cause 3 — fixture network-firewall-nfwr-traj-stateless-rule-priority-mistakes, step task#7, DescribeRuleGroup: expected 200 with $.RuleGroup.RulesSource.StatelessRulesAndCustomActions.StatelessRules[0].Priority 10 and [0]...Sources[0].AddressDefinition "10.1.0.0/16", [1].Priority 20 with "10.3.0.0/16"; actual 200 with [0].Priority 20 / "10.3.0.0/16" and [1].Priority 10 / "10.1.0.0/16".

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Cause 1: replay network-firewall-nfwr-traj-rule-group-capacity-and-type-mistakes step task#3 — CreateRuleGroup with Type STATEFUL, the rules supplied in the top-level Rules Suricata flat string, and a Capacity lower than the rules require. Inspect $.Message: it must name StatefulRules in both the stem and context (RulesSource.StatefulRules).
3. Cause 2: replay the same fixture's step task#5 — CreateRuleGroup with Capacity above the service maximum (recorded: 30001). Inspect $.__type (must be ...#InvalidRequestException) and $.Message (must be "Capacity capacity exceeded, parameter: [<requested capacity>], context: Capacity").
4. Cause 3: replay network-firewall-nfwr-traj-stateless-rule-priority-mistakes up to step task#7 — create a STATELESS rule group whose StatelessRules are supplied out of Priority order, then DescribeRuleGroup it and check that RuleGroup.RulesSource.StatelessRulesAndCustomActions.StatelessRules comes back ordered by ascending Priority.
