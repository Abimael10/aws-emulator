Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__wafv2.deploy-webacl-ratebasedstatement-ref-number-parameter-as-string-accepted cloudformation__wafv2.deploy-webacl-ratebasedstatement-limit-below-minimum-number-parameter-rejected
Issue-Key: cloudformation__wafv2.deploy-webacl-ratebasedstatement-ref-number-parameter-as-string-accepted|run#9|AssertionError|assert status == "CREATE_COMPLETE", (
Issue-Key: cloudformation__wafv2.deploy-webacl-ratebasedstatement-limit-below-minimum-number-parameter-rejected|run#11|AssertionError|assert got == expected, f"handler reason {got!r}, expected {expected!r}"
Issue-Commit: 16dd3a69d4b4af3373dd95173ca2a60641ca57cc

# A Ref to a Number parameter used for RateBasedStatement.Limit is passed to WAFv2 as a string, so every such AWS::WAFv2::WebACL fails with "expected number, got string"

> _Note: this issue was identified against the code base at commit `16dd3a69d4b4af3373dd95173ca2a60641ca57cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateWebACL

## Description
Cause 1 of 1: CloudFormation resolves `{"Ref": "<NumberParameter>"}` to a JSON string when it feeds AWS::WAFv2::WebACL, and the WebACL handler then refuses the request outright.
AWS: Smithy: wafv2#RateBasedStatement.Limit targets RateLimit, a required long with @range min 10, max 2000000000. A template parameter of Type Number referenced by Ref is substituted as a number, so the WebACL is created and GetWebACL reports Rules[0].Statement.RateBasedStatement.Limit as the parameter's value. When that value is below the modeled minimum, WAFv2 — not a type checker — refuses it, and the resource's CREATE_FAILED ResourceStatusReason is `Resource handler returned message: "1 validation error detected: Value '<value>' at 'rules.<n>.member.statement.rateBasedStatement.limit' failed to satisfy constraint: Member must have value greater than or equal to 10 (Service: Wafv2, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: GeneralServiceException)`. Fixed text: the `1 validation error detected: Value ... at ... failed to satisfy constraint: Member must have value greater than or equal to 10` sentence, the property path shape and the SDK suffix fragments; variable parts: the offending value and the rule index (from the template) and the per-run ids.
Emulator: in both the in-range and the below-minimum case the handler fails with the message `expected number, got string` and HandlerErrorCode `InvalidRequest`. The in-range stack therefore settles at ROLLBACK_COMPLETE (WebACL CREATE_FAILED, then DELETE_COMPLETE) instead of CREATE_COMPLETE, and the below-minimum stack reports a type error instead of WAFv2's range refusal — the range constraint is never reached or reported.
Why: any template that parameterises a rate limit (the normal way a rate-based rule is tuned per environment) cannot be deployed at all, and when the limit really is invalid the client is told the wrong thing: a type complaint with no property path, no service attribution and the wrong handler error code, so it cannot tell which rule or which bound was violated.
Fix: the template Ref/parameter resolution that must preserve a Number parameter's numeric type when building the resource's properties, and the AWS::WAFv2::WebACL resource handler's property deserialization (accept the resolved value and let WAFv2 enforce the @range, surfacing its message).
Traces: cloudformation__wafv2.deploy-webacl-ratebasedstatement-ref-number-parameter-as-string-accepted, cloudformation__wafv2.deploy-webacl-ratebasedstatement-limit-below-minimum-number-parameter-rejected

## Evidence
Below-minimum case: AssertionError: handler reason ('expected number, got string', 'InvalidRequest'), expected ("1 validation error detected: Value '9' at 'rules.1.member.statement.rateBasedStatement.limit' failed to satisfy constraint: Member must have value greater than or equal to 10 (Service: Wafv2, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)", 'GeneralServiceException').
In-range case: AssertionError: stack ended 'ROLLBACK_COMPLETE': [('WebACL', 'CREATE_FAILED'), ('WebACL', 'DELETE_COMPLETE'), ('<stack>', 'ROLLBACK_COMPLETE')], expected 'CREATE_COMPLETE' with GetWebACL reporting the Ref'd limit.

## Steps to Reproduce
Replay the behavioral suite and re-run:
1. cloudformation__wafv2.deploy-webacl-ratebasedstatement-ref-number-parameter-as-string-accepted
2. cloudformation__wafv2.deploy-webacl-ratebasedstatement-limit-below-minimum-number-parameter-rejected
Each deploys an AWS::WAFv2::WebACL whose RateBasedStatement.Limit is `{"Ref": "<Number parameter>"}`.
