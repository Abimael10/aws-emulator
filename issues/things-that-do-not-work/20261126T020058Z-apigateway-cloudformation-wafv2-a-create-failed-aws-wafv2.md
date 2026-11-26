Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation__wafv2.deploy-webacl-duplicate-rule-priority-rejected-and-rolls-back-the-whole-stack
Issue-Key: apigateway__cloudformation__wafv2.deploy-webacl-duplicate-rule-priority-rejected-and-rolls-back-the-whole-stack|run#30|AssertionError|assert reason == expected_reason, (
Issue-Commit: d13ac0f0199fff89cf39280dd342767ab3065596

# apigateway__cloudformation__wafv2: a CREATE_FAILED AWS::WAFv2::WebACL's ResourceStatusReason omits the AWS SDK exception suffix, so a rolled-back WAF resource carries no originating service, HTTP status or request id

> _Note: this issue was identified against the code base at commit `d13ac0f0199fff89cf39280dd342767ab3065596`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
This is ONE issue with ONE cause.

Cause 1 of 1: when the AWS::WAFv2::WebACL resource provider's control-plane call is refused, the ResourceStatusReason CloudFormation records on the resource's CREATE_FAILED event stops at the WAF sentence and omits the AWS SDK exception suffix.

AWS: a stack whose AWS::WAFv2::WebACL declares two Rules at the same Priority is refused by WAFv2 (Smithy com.amazonaws.wafv2#AWSWAF_20190729: CreateWebACL models WAFInvalidParameterException — errorKind client, httpError 400 — carrying Field (ParameterExceptionField), Parameter and Reason, whose message is composed as "Error reason: <Reason>, field: <Field>, parameter: <Parameter>"). CloudFormation then fails the resource, rolls the stack back to ROLLBACK_COMPLETE and records the reason as the handler message wrapped in its own envelope PLUS the SDK's exception suffix:

  Resource handler returned message: "<WAF message> (Service: Wafv2, Status Code: <status>, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)

Fixed text of that reason, for this error class: `Resource handler returned message: "` + `Error reason: You have a duplicate priority. Priorities must be unique., field: RULE, parameter: ` + the duplicated Priority taken from the template + ` (Service: Wafv2, Status Code: ` + the HTTP status of the WAF refusal + `, Request ID: ` + the per-call request id + `) (SDK Attempt Count: ` + the attempt count + `)" (RequestToken: ` + the per-call request token + `, HandlerErrorCode: InvalidRequest)`. The request id, request token and attempt count are per-run values and are masked by the check before comparison — the requirement is the presence and fixed text of the two parenthesised SDK clauses, not their values. The WAF Reason/field/parameter clauses themselves come from the refusal; the recording shows the duplicate-priority reason above (recorded behavior only for the exact Reason sentence, which the model does not state).

Emulator: the stack correctly reaches ROLLBACK_COMPLETE, the WebAcl row is CREATE_FAILED, the HandlerErrorCode is InvalidRequest and the WAF sentence is right, but the reason ends at that sentence: 'Resource handler returned message: "Error reason: You have a duplicate priority. Priorities must be unique., field: RULE, parameter: 0" (RequestToken: <request-token>, HandlerErrorCode: InvalidRequest)'. Both ` (Service: Wafv2, Status Code: 400, Request ID: <id>)` and ` (SDK Attempt Count: <n>)` are absent.

Why: tooling and operators parse that suffix to recover which service refused a failed resource, with which HTTP status, under which request id — it is how a CloudFormation rollback is correlated back to the service call that caused it. A client scraping DescribeStackEvents reasons for `Service: ...` / `Status Code: ...` / `Request ID: ...` finds nothing, so it cannot classify the failure as a 4xx client fault versus a retryable 5xx, and cannot escalate with a request id.

Fix: the CloudFormation resource-provider error path that formats a downstream service refusal into ResourceStatusReason for the AWS::WAFv2::* providers (the envelope builder that already emits `Resource handler returned message: "..." (RequestToken: ..., HandlerErrorCode: ...)` must also append the SDK exception suffix), not the wafv2 refusal itself, which is already correct.

Related open issue which does NOT cover this check: 20261109T195723Z-cloudformation-wafv2-3-net-new-causes-aws-wafv2.md, whose Cause 2 is the same formatting family but is keyed on cloudformation__wafv2.deploy-rulegroup-cfn-capacity-too-low-rejected (an AWS::WAFv2::RuleGroup capacity refusal under the cloudformation__wafv2 heading). It carries neither this check id nor the WebACL duplicate-priority path, so this failure is unowned; a fixer closing both should expect one shared formatting seam.

Traces: apigateway__cloudformation__wafv2.deploy-webacl-duplicate-rule-priority-rejected-and-rolls-back-the-whole-stack.

## Evidence
Check apigateway__cloudformation__wafv2.deploy-webacl-duplicate-rule-priority-rejected-and-rolls-back-the-whole-stack, verdict [mismatch]:

  AssertionError: the WebAcl CREATE_FAILED reason, with its request id/token masked, must read 'Resource handler returned message: "Error reason: You have a duplicate priority. Priorities must be unique., field: RULE, parameter: 0 (Service: Wafv2, Status Code: 400, Request ID: <request-id>) (SDK Attempt Count: <n>)" (RequestToken: <request-token>, HandlerErrorCode: InvalidRequest)'; got 'Resource handler returned message: "Error reason: You have a duplicate priority. Priorities must be unique., field: RULE, parameter: 0" (RequestToken: <request-token>, HandlerErrorCode: InvalidRequest)'

Expected: the CREATE_FAILED row's ResourceStatusReason contains the WAF message followed by ` (Service: Wafv2, Status Code: <status>, Request ID: <id>) (SDK Attempt Count: <n>)` inside the handler-message quotes.
Actual: the quoted handler message ends at the WAF sentence; both parenthesised SDK clauses are missing.

Everything before that assertion passes: the positive-control stack (two rules at Priority 0 and 1) reaches CREATE_COMPLETE and GetWebACL reads the priorities back as [0, 1]; the duplicate-priority stack reaches ROLLBACK_COMPLETE; a CREATE_FAILED event for the WebAcl logical id exists; and HandlerErrorCode is InvalidRequest. `parameter: 0` is the duplicated Priority the template declared, not a constant, and the masked request id/token/attempt count are per-run values.

## Steps to Reproduce
1. CreateStack with a template holding an AWS::ApiGateway::RestApi and an AWS::WAFv2::WebACL (Scope REGIONAL, DefaultAction Allow, VisibilityConfig) whose Rules list two rules — e.g. "First" and "Second", each a ByteMatchStatement with Action Count — BOTH at Priority 0.
2. Poll DescribeStacks until the stack settles: it reaches ROLLBACK_COMPLETE (correct).
3. DescribeStackEvents and read ResourceStatusReason on the WebAcl row whose ResourceStatus is CREATE_FAILED.
4. Expect: 'Resource handler returned message: "Error reason: You have a duplicate priority. Priorities must be unique., field: RULE, parameter: 0 (Service: Wafv2, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)'.
5. Observed: the same string with ` (Service: Wafv2, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)` absent.
6. Or replay the check id in trace_ids.
