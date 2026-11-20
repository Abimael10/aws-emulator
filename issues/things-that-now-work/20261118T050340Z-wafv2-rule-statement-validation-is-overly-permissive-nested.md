Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: wafv2.rate-based-statement-nested-in-logical-statement-rejected wafv2.rate-based-statement-window-outside-60-120-300-600-rejected wafv2.check-capacity-pre-parse-text-transformation-on-uripath-rejected
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2.rate-based-statement-nested-in-logical-statement-rejected|_error#5|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: wafv2.rate-based-statement-window-outside-60-120-300-600-rejected|_error#3|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: wafv2.check-capacity-pre-parse-text-transformation-on-uripath-rejected|expect_error#2|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Commit: 87988956329e39898d1a96c56401098086adb3bb

# wafv2 rule-statement validation is overly permissive: nested RateBasedStatement, out-of-set EvaluationWindowSec, and PreParseTextTransformations on an unsupported FieldToMatch are all accepted

> _Note: this issue was identified against the code base at commit `87988956329e39898d1a96c56401098086adb3bb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateWebACL

## Description
Three independent rule-statement validation rules are missing from the wafv2 emulator: a rule set that AWS WAF refuses with HTTP 400 WAFInvalidParameterException is accepted with HTTP 200, and in two of the three cases a WebACL is actually created. This is ONE issue with THREE causes; every cause below must be closed before the fix is submitted.

Cause 1 of 3: a RateBasedStatement nested inside a logical statement is accepted by CreateWebACL.
AWS: Smithy: com.amazonaws.wafv2#NotStatement$Statement and OrStatement$Statements/AndStatement$Statements are documented "You can use any statement that can be nested"; com.amazonaws.wafv2#WAFInvalidParameterException (errorKind client, httpError 400) is documented for exactly this case — "Your nested statement isn't valid. You might have tried to nest a statement that can\u2019t be nested." A RateBasedStatement is a top-level-only statement, so WAF refuses the CreateWebACL with HTTP 400, __type WAFInvalidParameterException, Field = the fixed enum token RATE_BASED_STATEMENT (a member of ParameterExceptionField), Parameter = the fixed member name RateBasedStatement, Reason = the fixed sentence "A reference in your rule statement is not valid.", and message = "Error reason: A reference in your rule statement is not valid., field: RATE_BASED_STATEMENT, parameter: RateBasedStatement". Every part of that refusal is fixed text for this error class — nothing in it is taken from the request — and the refusal must create nothing (no WebACL is left behind for ListWebACLs to return).
Emulator: HTTP 200 and a CreateWebACL Summary (Name/Id/LockToken/ARN) for each of NotStatement, OrStatement and AndStatement wrapping the same RateBasedStatement; the invalid WebACL is persisted and listable. Replay detail: "AssertionError: expected the call to be refused, but it succeeded: {'Summary': {...'ARN': 'arn:aws:wafv2:<region>:<account>:regional/webacl/<name>/<id>'}, 'ResponseMetadata': {'HTTPStatusCode': 200 ...}}" (the generated name, id, lock token and ARN are per-run values, not requirements).
Why: a client that nests a rate-based rule under NOT/OR/AND gets a web ACL the emulator says exists and enforces; against real WAF the same template or SDK call fails at create time with a 400. The invalid rule is also evaluated by the emulator's data plane, so request-matching behaviour the client tests is behaviour AWS can never produce, and the config is unshippable.
Fix: the nested-statement validation in the CreateWebACL handler's rule validation (shared with UpdateWebACL, CreateRuleGroup/UpdateRuleGroup and CheckCapacity), which must reject RateBasedStatement anywhere other than as a rule's own top-level Statement, before anything is written to state.
Traces: wafv2.rate-based-statement-nested-in-logical-statement-rejected.

Cause 2 of 3: RateBasedStatement.EvaluationWindowSec outside {60, 120, 300, 600} is accepted by CreateWebACL.
AWS: Smithy: com.amazonaws.wafv2#RateBasedStatement$EvaluationWindowSec (long, default 300) is documented "Valid settings are 60, 120, 300, and 600." — the member has no @range narrowing it, so the allowed set is an enumerated service rule, not a range check. A value inside the numeric domain but outside that set (e.g. 90, 599) is refused with HTTP 400, __type WAFInvalidParameterException, Field = the fixed enum token RATE_BASED_STATEMENT, Parameter = the rejected EvaluationWindowSec value taken from the request (the only variable part), Reason = the fixed sentence "Your request in not valid." (WAF's own wording, including its grammatical error), and message = "Error reason: Your request in not valid., field: RATE_BASED_STATEMENT, parameter: " + that value. The refusal must create nothing. The four valid settings must be accepted and read back from GetWebACL unchanged.
Emulator: HTTP 200 and a CreateWebACL Summary for EvaluationWindowSec=90 (and 599); the WebACL is persisted. Replay detail: "AssertionError: expected the call to be refused, but it succeeded: {'Summary': {...}, 'ResponseMetadata': {'HTTPStatusCode': 200 ...}}".
Why: WAF checks the rate over a fixed set of look-back windows. A client that configures an unsupported window sees it accepted and assumes rate limiting over that window; AWS refuses the same call, so the emulator hides a config error that blocks deployment, and any rate behaviour the emulator shows for a 90 s window corresponds to no real WAF setting.
Fix: EvaluationWindowSec membership validation in the CreateWebACL handler's rate-based-statement validation (shared with UpdateWebACL, CreateRuleGroup/UpdateRuleGroup and CheckCapacity), before state is written. Keep the modeled default of 300 when the member is absent.
Traces: wafv2.rate-based-statement-window-outside-60-120-300-600-rejected.

Cause 3 of 3: CheckCapacity accepts PreParseTextTransformations on a FieldToMatch that does not support it.
AWS: Smithy: com.amazonaws.wafv2#ByteMatchStatement$PreParseTextTransformations targets PreParseTextTransformation, documented as supported only when FieldToMatch is SingleQueryArgument or AllQueryArguments; ParameterExceptionField contains the enum value PRE_PARSE_TEXT_TRANSFORMATION; CheckCapacity models WAFInvalidParameterException (client, 400). With FieldToMatch = UriPath and a PreParseTextTransformations entry present, WAF refuses with HTTP 400, __type WAFInvalidParameterException, Field PRE_PARSE_TEXT_TRANSFORMATION, Parameter PreParseTextTransformation, Reason "PreParseTextTransformations is only supported for SingleQueryArgument and AllQueryArguments field to match types." and message "Error reason: PreParseTextTransformations is only supported for SingleQueryArgument and AllQueryArguments field to match types., field: PRE_PARSE_TEXT_TRANSFORMATION, parameter: PreParseTextTransformation" — all fixed text, no request-derived values. The same statement on SingleQueryArgument is accepted and its Capacity includes the pre-parse transformations' WCU contribution (recorded behavior only for the arithmetic: the check expects 30 WCU for the supported-field control rule, strictly more than the base statement cost).
Emulator: HTTP 200 with a Capacity body. Replay detail: "AssertionError: expected the call to be rejected, but it succeeded: {'Capacity': 10, 'ResponseMetadata': {'HTTPStatusCode': 200 ...}}" — the refusal members (__type, Field, Parameter, Reason, message) are all absent, and the reported Capacity also omits the pre-parse contribution.
Why: the capacity pre-check is the step a client uses to decide whether a rule set fits its WCU budget. A 200 here tells the client the rule set is both valid and cheap; AWS refuses the statement outright, and for the supported field charges more WCU than the emulator quotes, so the client sizes the web ACL wrong and discovers the invalid statement only at CreateWebACL time.
Fix: the FieldToMatch/PreParseTextTransformations compatibility check and the pre-parse WCU contribution in the CheckCapacity handler's rule validation and capacity calculation, shared with CreateWebACL/CreateRuleGroup/Update* rule validation. NOTE: open issue 20261117T233522Z-wafv2-checkcapacity-ignores-preparsetexttransformations-wcu-cost-undercounted-and.md describes the same emulator behaviour from the wire corpus; it carries only wire replay keys (wafv2|CheckCapacity|...) and not this behavioral check, so this cause is recorded here with its behavioral check id. Fixing one should fix both; re-replay both.
Traces: wafv2.check-capacity-pre-parse-text-transformation-on-uripath-rejected.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (195/235 passed; the emulator was up and serving, so these are fidelity failures, not a bootstrap gap). Three failing checks under the `### wafv2` heading are neither "ALREADY FILED" nor "OWNED by" any open issue:

1. [mismatch] wafv2.rate-based-statement-nested-in-logical-statement-rejected
   Expected (all three of NotStatement / OrStatement / AndStatement wrapping {"RateBasedStatement": {"Limit": 100, "AggregateKeyType": "IP"}}):
     code    = WAFInvalidParameterException (HTTP 400)
     message = "Error reason: A reference in your rule statement is not valid., field: RATE_BASED_STATEMENT, parameter: RateBasedStatement"
     and ListWebACLs shows no WebACL left behind.
   Actual: HTTP 200 — "expected the call to be refused, but it succeeded: {'Summary': {'Name': 'sim2real-<rand>-not', 'Id': '<uuid>', 'Description': '', 'LockToken': '<uuid>', 'ARN': 'arn:aws:wafv2:us-west-2:000000000000:regional/webacl/sim2real-<rand>-not/<uuid>'}, 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}". Names/ids/ARNs/dates in that body are per-run values; the divergence is 200-with-a-created-WebACL instead of 400.

2. [mismatch] wafv2.rate-based-statement-window-outside-60-120-300-600-rejected
   Expected, for EvaluationWindowSec in (90, 599):
     code    = WAFInvalidParameterException (HTTP 400)
     message = "Error reason: Your request in not valid., field: RATE_BASED_STATEMENT, parameter: <the rejected window value>"
     (fixed text "Error reason: Your request in not valid., field: RATE_BASED_STATEMENT, parameter: "; the trailing value is echoed from the request)
     and ListWebACLs shows nothing created.
   Actual: HTTP 200 — "expected the call to be refused, but it succeeded: {'Summary': {'Name': 'sim2real-<rand>-bad90', 'Id': '<uuid>', ... 'ARN': 'arn:aws:wafv2:us-west-2:000000000000:regional/webacl/...'}, 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}".

3. [mismatch] wafv2.check-capacity-pre-parse-text-transformation-on-uripath-rejected
   Expected, for a ByteMatchStatement with FieldToMatch={"UriPath": {}} and PreParseTextTransformations=[{"Priority": 0, "Type": "URL_DECODE"}]:
     code    = WAFInvalidParameterException (HTTP 400)
     message = "Error reason: PreParseTextTransformations is only supported for SingleQueryArgument and AllQueryArguments field to match types., field: PRE_PARSE_TEXT_TRANSFORMATION, parameter: PreParseTextTransformation"
     and the same statement on SingleQueryArgument reports Capacity 30.
   Actual: HTTP 200 — "expected the call to be rejected, but it succeeded: {'Capacity': 10, 'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/x-amz-json-1.1', ...}}}".

Model references used (services/service-com-amazonaws-wafv2-awswaf-20190729/smithy-model/com-amazonaws-wafv2-awswaf-20190729.json):
  - com.amazonaws.wafv2#RateBasedStatement$EvaluationWindowSec — long, default 300, doc "Valid settings are 60, 120, 300, and 600." (no @range narrowing).
  - com.amazonaws.wafv2#NotStatement$Statement / OrStatement$Statements — doc "You can use any statement that can be nested."
  - com.amazonaws.wafv2#WAFInvalidParameterException — errorKind client, httpError 400, members message/Field (ParameterExceptionField)/Parameter/Reason; doc explicitly covers "Your nested statement isn't valid. You might have tried to nest a statement that can\u2019t be nested."
  - com.amazonaws.wafv2#ParameterExceptionField — contains RATE_BASED_STATEMENT and PRE_PARSE_TEXT_TRANSFORMATION.

Related open issues that do NOT carry these behavioral check ids: 20261029T192556Z-wafv2-request-validation-never-refuses-ten-operations-answer.md (ten other never-refused wafv2 operations; CreateWebACL rule validation and CheckCapacity are not among them) and 20261117T233522Z-wafv2-checkcapacity-ignores-preparsetexttransformations-wcu-cost-undercounted-and.md (same CheckCapacity behaviour, wire keys only — see Cause 3).

## Steps to Reproduce
1. Clone this checkout, build and boot the emulator, then replay simparity-behavioral-aws-v1 (run_behavioral_suite). The three checks named in trace_ids fail.

2. Cause 1 — nested RateBasedStatement:
   For each wrapper in NotStatement / OrStatement / AndStatement, call CreateWebACL with Scope=REGIONAL, DefaultAction={"Allow":{}}, a VisibilityConfig, and Rules=[{Name:"r", Priority:0, Action:{"Block":{}}, VisibilityConfig:..., Statement: <wrapper around {"RateBasedStatement":{"Limit":100,"AggregateKeyType":"IP"}}>}]
   (OrStatement/AndStatement additionally carry a GeoMatchStatement sibling).
   Expect HTTP 400 WAFInvalidParameterException, Field RATE_BASED_STATEMENT, Parameter RateBasedStatement, Reason "A reference in your rule statement is not valid." and no WebACL created (ListWebACLs with that name prefix returns empty). The emulator returns 200 with a Summary and persists the WebACL.
   Control: the same RateBasedStatement used directly as the rule's own Statement must be accepted, and GetWebACL must read it back as {"Limit":100,"AggregateKeyType":"IP","EvaluationWindowSec":300} (the modeled default filled in).

3. Cause 2 — EvaluationWindowSec outside the allowed set:
   Call CreateWebACL (Scope=REGIONAL) with a rule whose Statement is {"RateBasedStatement":{"Limit":100,"AggregateKeyType":"IP","EvaluationWindowSec":90}}, then again with 599.
   Expect HTTP 400 WAFInvalidParameterException with message "Error reason: Your request in not valid., field: RATE_BASED_STATEMENT, parameter: 90" (resp. "... parameter: 599") and nothing created. The emulator returns 200 and creates the WebACL.
   Control: one WebACL carrying four rate-based rules with EvaluationWindowSec 60, 120, 300 and 600 must be accepted and each window must round-trip unchanged through GetWebACL.

4. Cause 3 — PreParseTextTransformations on an unsupported field:
   Call CheckCapacity with Scope=REGIONAL and Rules=[{Name:"probe", Priority:0, Action:{"Count":{}}, VisibilityConfig:..., Statement:{"ByteMatchStatement":{"SearchString": b"probe", "FieldToMatch": {"UriPath": {}}, "TextTransformations":[{"Priority":0,"Type":"NONE"}], "PreParseTextTransformations":[{"Priority":0,"Type":"URL_DECODE"}], "PositionalConstraint":"CONTAINS"}}}].
   Expect HTTP 400 WAFInvalidParameterException with Field PRE_PARSE_TEXT_TRANSFORMATION, Parameter PreParseTextTransformation and the fixed Reason/message quoted above. The emulator answers 200 with {"Capacity": 10}.
   Control: the identical statement with FieldToMatch={"SingleQueryArgument":{"Name":"q"}} must be accepted and report Capacity 30 (the emulator currently quotes the base cost with the pre-parse contribution dropped).
