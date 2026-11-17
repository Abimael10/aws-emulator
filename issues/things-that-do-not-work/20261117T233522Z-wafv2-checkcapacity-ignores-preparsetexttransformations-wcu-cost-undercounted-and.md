Issue-Source: trace-sweep-divergence
Issue-Trace: wafv2-check-capacity-pre-parse-text-transformation-cost
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wafv2-check-capacity-pre-parse-text-transformation-cost
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2|CheckCapacity|2xx->2xx|$.Capacity
Issue-Key: wafv2|CheckCapacity|4xx->2xx|$.Field
Issue-Key: wafv2|CheckCapacity|4xx->2xx|$.Parameter
Issue-Key: wafv2|CheckCapacity|4xx->2xx|$.Reason
Issue-Key: wafv2|CheckCapacity|4xx->2xx|$.__type
Issue-Key: wafv2|CheckCapacity|4xx->2xx|$.message
Issue-Key: wafv2|CheckCapacity|4xx->2xx|status
Issue-Commit: 5f9152346f0a34f19c016441be64bcf6cb300f64

# wafv2 CheckCapacity ignores PreParseTextTransformations: WCU cost undercounted and the unsupported-field refusal missing

> _Note: this issue was identified against the code base at commit `5f9152346f0a34f19c016441be64bcf6cb300f64`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CheckCapacity

## Description
CheckCapacity treats a rule statement's PreParseTextTransformations as if it were not there: it neither charges the transformations' web ACL capacity units nor enforces the FieldToMatch types the member is restricted to. This is ONE issue with TWO causes; both must be closed before the fix is submitted.

Cause 1 of 2: the WCU cost of PreParseTextTransformations is not added to the reported Capacity.
AWS: Smithy: CheckCapacity returns Capacity (ConsumedCapacity, long, @range min 0); ByteMatchStatement.PreParseTextTransformations targets PreParseTextTransformations and is documented "Pre-parse text transformations normalize the raw query string before WAF parses it into individual query arguments. They are applied before the standard text transformations... You can specify up to 10 pre-parse text transformations per rule statement." The model does not state the per-transformation WCU arithmetic, so for the exact number this is recorded behavior only: for a rule set whose statements carry pre-parse text transformations, AWS reports a Capacity strictly greater than the same statements' cost without them — in the recording twice the emulator's figure. Hypothesis: each pre-parse text transformation is charged like a standard text transformation, so the statement's base cost plus one charged transformation yields the recorded total. The requirement is that PreParseTextTransformations contribute to Capacity, consistently across every CheckCapacity call for the same rules.
Emulator: reports the base statement cost only — HTTP 200 with a Capacity that omits the pre-parse contribution ($.Capacity: expected 20, got 10, on all three CheckCapacity steps of the fixture).
Why: WCU is the quota WAF charges a web ACL against; a client that sizes its rule set with the emulator's figure builds a rule set that AWS will refuse with WAFLimitsExceededException at CreateWebACL/UpdateWebACL time, and capacity planning based on CheckCapacity is off by the whole pre-parse contribution.
Fix: the capacity calculation for statements carrying PreParseTextTransformations in the CheckCapacity handler (and the same shared cost function used when a web ACL's/rule group's Capacity is computed).
Traces: wafv2-check-capacity-pre-parse-text-transformation-cost#task#1 (cluster also covers #task#2 and #task#3 of the same fixture).

Cause 2 of 2: a PreParseTextTransformations on an unsupported FieldToMatch is accepted instead of refused.
AWS: Smithy: com.amazonaws.wafv2#PreParseTextTransformation is documented "Pre-parse text transformations are only supported when FieldToMatch is SingleQueryArgument or AllQueryArguments"; ParameterExceptionField contains the enum value `PRE_PARSE_TEXT_TRANSFORMATION`; CheckCapacity models WAFInvalidParameterException (client, httpError 400). WAF refuses the rule set with HTTP 400, __type `WAFInvalidParameterException`, Field `PRE_PARSE_TEXT_TRANSFORMATION`, Parameter `PreParseTextTransformation`, Reason `PreParseTextTransformations is only supported for SingleQueryArgument and AllQueryArguments field to match types.` and message `Error reason: ` + Reason + `, field: PRE_PARSE_TEXT_TRANSFORMATION, parameter: PreParseTextTransformation`. Every part of that refusal is fixed text — the member name and the enum token are not taken from the request.
Emulator: HTTP 200 with a Capacity figure; __type, Field, Parameter, Reason and message all absent.
Why: the client gets a capacity quote for a rule set WAF will never accept, so the invalid statement is discovered only at CreateWebACL time (or, worse, is assumed valid because the capacity pre-check passed).
Fix: the FieldToMatch/PreParseTextTransformations compatibility check in the CheckCapacity handler's rule validation, shared with CreateWebACL/CreateRuleGroup/Update* rule validation.
Traces: wafv2-check-capacity-pre-parse-text-transformation-cost#task#4.

## Evidence
Replay of simparity-wire-aws-v1 (emulator up). Two verdict=mismatch clusters on CheckCapacity, neither owned by an open issue:

1. wafv2-check-capacity-pre-parse-text-transformation-cost#task#1 (affected_count 3: #task#1, #task#2, #task#3) — expected_status 200, actual_status 200, detail "$.Capacity: value_mismatch (expected 20, got 10)". The emulator's figure is exactly the cost with the pre-parse transformations dropped; 20 vs 10 are this corpus's rule set, the requirement is that the pre-parse contribution is charged.
2. wafv2-check-capacity-pre-parse-text-transformation-cost#task#4 — expected_status 400, actual_status 200, detail "status: expected 400, got 200; $.__type: missing (expected 'WAFInvalidParameterException', got None); $.Field: missing (expected 'PRE_PARSE_TEXT_TRANSFORMATION', got None); $.Parameter: missing (expected 'PreParseTextTransformation', got None); $.Reason: missing (expected 'PreParseTextTransformations is only supported for SingleQueryArgument and AllQueryArguments field to match types.', got None); $.message: missing (expected 'Error reason: PreParseTextTransformations is only supported for SingleQueryArgument and AllQueryArguments field to match types., field: PRE_PARSE_TEXT_TRANSFORMATION, parameter: PreParseTextTransformation', got None)".

Model references used: CheckCapacity (input Scope+Rules required; output Capacity -> ConsumedCapacity; errors include WAFInvalidParameterException client/400 and WAFLimitsExceededException client/400), com.amazonaws.wafv2#ByteMatchStatement$PreParseTextTransformations, com.amazonaws.wafv2#PreParseTextTransformation (doc: supported only for SingleQueryArgument/AllQueryArguments), com.amazonaws.wafv2#ParameterExceptionField (contains PRE_PARSE_TEXT_TRANSFORMATION).

The open issue 20261029T192556Z-wafv2-request-validation-never-refuses-ten-operations-answer lists ten never-refused operations but not CheckCapacity, and carries neither replay key above.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: CheckCapacity with a rule whose ByteMatchStatement has FieldToMatch = {"AllQueryArguments": {}} (or SingleQueryArgument) plus a PreParseTextTransformations entry -> AWS reports a Capacity that includes the pre-parse transformation's cost (20 in the recording); the emulator reports the base cost only (10). Repeat for the fixture's task#2 and task#3 rule sets.
3. Cause 2: CheckCapacity with the same statement but FieldToMatch set to a type other than SingleQueryArgument/AllQueryArguments (e.g. UriPath or QueryString) while keeping PreParseTextTransformations -> expect HTTP 400 WAFInvalidParameterException with Field PRE_PARSE_TEXT_TRANSFORMATION, Parameter PreParseTextTransformation and the fixed Reason/message; the emulator answers 200 with a Capacity.
