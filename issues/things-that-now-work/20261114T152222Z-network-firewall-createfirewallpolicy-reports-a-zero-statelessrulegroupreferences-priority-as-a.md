Issue-Source: trace-sweep-divergence
Issue-Trace: network-firewall-nfwr-traj-stateless-rule-priority-mistakes
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: network-firewall-nfwr-traj-stateless-rule-priority-mistakes
Issue-Shape-Id: com.amazonaws.networkfirewall#NetworkFirewall_20201112
Issue-Key: network-firewall|CreateFirewallPolicy|4xx->4xx|$.message
Issue-Commit: 4c77ade3d5e6aed90c9f3ce2b3b42ec7a5a8e0f0

# CreateFirewallPolicy reports a zero StatelessRuleGroupReferences Priority as a null member instead of a range violation

> _Note: this issue was identified against the code base at commit `4c77ade3d5e6aed90c9f3ce2b3b42ec7a5a8e0f0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFirewallPolicy

## Description
One root cause (Cause 1 of 1).

Cause 1 of 1: CreateFirewallPolicy mis-classifies an out-of-range FirewallPolicy.StatelessRuleGroupReferences[].Priority as a missing member, so the constraint-violation message names the wrong constraint.

AWS: Smithy model com.amazonaws.networkfirewall#StatelessRuleGroupReference member Priority targets an integer and is `required: true` with `@range min 1, max 65535` (ResourceArn is required with @length 1..256 and pattern ^arn:aws). A value that is PRESENT but below the minimum is a range violation, not an absence, so the AWS front-end constraint validator rejects the request with HTTP 400 and the standard Smithy client-side constraint report in the lowercase `message` field: "<N> validation error detected: Value at '<memberPath>' failed to satisfy constraint: Member must have value greater than or equal to 1". In that template the fixed text is "validation error detected: Value at '", "' failed to satisfy constraint: Member must have value greater than or equal to "; the variable parts are the violation count N, the member path (derived from the request's own shape path — here the serialized path of the offending list element's Priority member, e.g. firewallPolicy.statelessRuleGroupReferences.<index>.member.priority) and the bound 1 taken from the model's @range min. "Member must not be null" is the distinct message AWS emits only when a required member is genuinely ABSENT from the request.

Emulator: for the same request the emulator answers 400 with `$.message` = "1 validation error detected: Value at 'firewallPolicy.statelessRuleGroupReferences.1.member.priority' failed to satisfy constraint: Member must not be null" — the correct status, error class and member path, but the required/null branch of the validator instead of the range branch. Hypothesis: the deserialized Priority is held in an optional integer whose zero value is collapsed to "absent" (or a falsy/`unwrap_or_default` style check runs before the range check), so a present-but-too-small value can never reach the @range comparison.

Why: a client that submits Priority 0 (or any value below 1) is told the member is missing. SDK and IaC callers branch on that message to decide what to repair — they will add a field that is already present rather than raise it into 1..65535, and retry loops keyed on "must not be null" never converge. It also masks the real limit, so the emulator cannot be used to test a caller's handling of the documented 1..65535 range, and any value of 0 is indistinguishable from an omitted Priority.

Fix: CreateFirewallPolicy's input validation for FirewallPolicy.StatelessRuleGroupReferences — the required-member presence check must be decided on whether the member was present in the request payload (not on whether its value is zero/default), and a present value outside the modeled @range must emit the greater-than-or-equal-to / less-than-or-equal-to constraint message with the model's own bound. The same ordering applies to every @range-constrained required integer in the operation's input closure.

Traces: fixture network-firewall-nfwr-traj-stateless-rule-priority-mistakes step task#5 (expected "...Member must have value greater than or equal to 1", got "...Member must not be null").

## Evidence
Replay of fixture `network-firewall-nfwr-traj-stateless-rule-priority-mistakes`, step `task#5`, operation CreateFirewallPolicy.

Status: expected 400, got 400 (status and error class match).
Body key `$.message`:
  Expected: "1 validation error detected: Value at 'firewallPolicy.statelessRuleGroupReferences.1.member.priority' failed to satisfy constraint: Member must have value greater than or equal to 1"
  Actual:   "1 validation error detected: Value at 'firewallPolicy.statelessRuleGroupReferences.1.member.priority' failed to satisfy constraint: Member must not be null"

Fixed text of the message template: "<N> validation error detected: Value at '<path>' failed to satisfy constraint: Member must have value greater than or equal to <min>". Variable parts taken from the request/model: N (violation count), <path> (the offending member's serialized path), <min> = 1 from the Smithy @range on com.amazonaws.networkfirewall#StatelessRuleGroupReference$Priority. The index `1` inside the path in this recording is request data, not part of the requirement.

Model evidence (smithy_query shape StatelessRuleGroupReference): Priority — target integer, required: true, range {min: 1, max: 65535}.

No open issue covers this step: the replay reports `owned_by: []` for this cluster. The nearby open issue "CreateRuleGroup misses rule-group type, capacity and priority validation and then blocks the valid create" (20261102T120026Z-network-firewall-createrulegroup-misses-rule-group-type-capacity-and-priority.md) owns the other clusters of this fixture, but it is scoped to CreateRuleGroup, not to CreateFirewallPolicy's StatelessRuleGroupReferences constraint reporting.

## Steps to Reproduce
1. Create a stateless rule group so a valid ResourceArn is available (as the fixture does in its earlier steps).
2. Call CreateFirewallPolicy with a FirewallPolicy whose StatelessRuleGroupReferences contains an entry with a valid ResourceArn and Priority explicitly set to 0 (any value below the modeled minimum of 1).
3. Observe the 400 response body's lowercase `message` member.
4. Expected (AWS): "...failed to satisfy constraint: Member must have value greater than or equal to 1". Actual (emulator): "...failed to satisfy constraint: Member must not be null".
5. Equivalently: replay fixture network-firewall-nfwr-traj-stateless-rule-priority-mistakes and inspect step task#5.
