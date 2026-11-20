Issue-Source: trace-sweep-divergence
Issue-Trace: wafv2-create-web-acl-monetize-without-monetization-config
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wafv2-create-web-acl-monetize-without-monetization-config wafv2-create-rule-group-monetize-without-monetization-config wafv2-create-web-acl-monetize-rate-based-rule wafv2-create-web-acl-monetize-regional-scope wafv2-webacl-monetization-connected-account-configs-rejected
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2|CreateWebACL|4xx->2xx|$.__type
Issue-Key: wafv2|CreateWebACL|4xx->2xx|$.message
Issue-Key: wafv2|CreateWebACL|4xx->2xx|status
Issue-Key: wafv2|CreateRuleGroup|4xx->2xx|$.__type
Issue-Key: wafv2|CreateRuleGroup|4xx->2xx|$.message
Issue-Key: wafv2|CreateRuleGroup|4xx->2xx|status
Issue-Key: wafv2|CreateWebACL|4xx->2xx|$.Field
Issue-Key: wafv2|CreateWebACL|4xx->2xx|$.Parameter
Issue-Key: wafv2|CreateWebACL|4xx->2xx|$.Reason
Issue-Key: wafv2|CreateWebACL|4xx->2xx|$.Message
Issue-Key: wafv2|CreateWebACL|2xx->4xx|$.Summary
Issue-Key: wafv2|CreateWebACL|2xx->4xx|status
Issue-Key: wafv2|CreateRuleGroup|2xx->4xx|$.Summary
Issue-Key: wafv2|CreateRuleGroup|2xx->4xx|status
Issue-Commit: 5f9152346f0a34f19c016441be64bcf6cb300f64

# wafv2 monetization create-path is inverted: invalid Monetize requests accepted, valid ones refused

> _Note: this issue was identified against the code base at commit `5f9152346f0a34f19c016441be64bcf6cb300f64`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateWebACL

## Description
The Monetize rule action and the MonetizationConfig resource setting are not modeled on the wafv2 create path, and the emulator diverges in BOTH directions: it accepts with HTTP 200 every monetization request AWS WAF refuses, and it refuses with HTTP 400 the well-formed monetization resources AWS WAF creates. This is ONE issue with SIX causes; every cause below must be closed before the fix is submitted.

Cause 1 of 6: CreateWebACL with a rule whose Action is Monetize but no MonetizationConfig on the web ACL.
AWS: Smithy: CreateWebACL.MonetizationConfig targets MonetizationConfig, documented "You must provide this configuration when any rule in the web ACL or rule group uses the Monetize action"; RuleAction.Monetize is documented as requiring "a MonetizationConfig on the web ACL"; the operation models WAFInvalidParameterException (client, httpError 400). WAF refuses with HTTP 400, __type `WAFInvalidParameterException` and the message `MonetizeAction requires MonetizationConfig to be configured on the WebACL. Provide a MonetizationConfig with payment network details.` — entirely fixed text; the only resource-dependent part is the resource kind being created (`WebACL`).
Emulator: HTTP 200 with a normal CreateWebACL Summary; no __type and no message.
Why: the client gets a web ACL whose Monetize rule has no payment network to settle against, so the HTTP 402 payment flow silently never charges and the configuration error surfaces only in production traffic.
Fix: the rule-action/monetization cross-check in the CreateWebACL handler's input validation.
Traces: wafv2-create-web-acl-monetize-without-monetization-config#task#1.

Cause 2 of 6: CreateRuleGroup with a Monetize rule action but no MonetizationConfig on the rule group.
AWS: Smithy: CreateRuleGroup.MonetizationConfig targets MonetizationConfig (same doc as Cause 1, "web ACL or rule group"); the operation models WAFInvalidParameterException (400). WAF refuses with HTTP 400, __type `WAFInvalidParameterException` and the message `MonetizeAction requires MonetizationConfig to be configured on the RuleGroup. Provide a MonetizationConfig with payment network details.` — fixed text with the resource kind `RuleGroup`.
Emulator: HTTP 200 with a CreateRuleGroup Summary; no error members.
Why: same as Cause 1, one layer down — the unusable rule group is then referenced by web ACLs that believe monetization is configured.
Fix: the same cross-check in the CreateRuleGroup handler's input validation.
Traces: wafv2-create-rule-group-monetize-without-monetization-config#task#1.

Cause 3 of 6: CreateWebACL with a Monetize action on a rate-based rule.
AWS: Smithy: MonetizeAction is documented "You cannot use the Monetize action for rate-based rules", and ParameterExceptionField contains the enum value `RULE_ACTION`; CreateWebACL models WAFInvalidParameterException (400). WAF refuses with HTTP 400, __type `WAFInvalidParameterException`, Field `RULE_ACTION`, Reason the fixed sentence `The parameter value isn't supported.`, Parameter the rendered action `MonetizeAction(priceMultiplier=<PriceMultiplier>)` where the variable part is the rule's MonetizeAction.PriceMultiplier taken from the request and is the literal token `null` when the member is absent, and message `Error reason: ` + Reason + `, field: RULE_ACTION, parameter: ` + Parameter.
Emulator: HTTP 200 and the web ACL is created; __type, Field, Parameter, Reason and message all absent.
Why: a rate-based rule carrying a terminating Monetize action is a combination WAF cannot evaluate, so the client ships a rule set whose behavior under load is undefined rather than being told at create time.
Fix: the per-rule action compatibility check (rate-based statement vs Monetize action) in the CreateWebACL handler's validation.
Traces: wafv2-create-web-acl-monetize-rate-based-rule#task#1.

Cause 4 of 6: CreateWebACL configuring monetization under REGIONAL scope.
AWS: Smithy: MonetizeAction is documented "This action is available only for web ACLs associated with Amazon CloudFront distributions"; CreateWebACL models WAFInvalidOperationException (client, 400, documented "The operation isn't valid"). WAF refuses a regional-scope monetization web ACL with HTTP 400, __type `WAFInvalidOperationException` and Message `Monetization is not available for regional resources. Use CloudFront scope to configure monetization.` — fixed text, no variable parts.
Emulator: HTTP 200 with a Summary; neither __type nor Message present.
Whot/Why: the client believes monetization is active on a regional resource where WAF can never serve the 402 manifest, and a scope mistake that AWS reports at create time becomes a silent no-op.
Fix: the scope precondition for monetization in the CreateWebACL handler's validation (shared with any other monetization-taking operation).
Traces: wafv2-create-web-acl-monetize-regional-scope#task#1 (same cluster: wafv2-webacl-monetization-connected-account-configs-rejected#task#2).

Cause 5 of 6: CreateWebACL REFUSES a well-formed CLOUDFRONT-scoped monetization web ACL that AWS creates.
AWS: Smithy: CreateWebACL returns output member Summary (WebACLSummary: Name, Id, ARN, Description, LockToken) with HTTP 200 when the request is valid — i.e. a CLOUDFRONT-scoped web ACL whose Monetize rules are accompanied by a MonetizationConfig and are not rate-based is created normally. The recorded success carries Name = the requested Name, Id and LockToken freshly generated identifiers, ARN of the form `arn:aws:wafv2:<region>:<account-id>:global/webacl/<Name>/<Id>` for CLOUDFRONT scope, and Description the empty string when the request supplied none. The identifiers, account id and region in the recording are per-run values, not requirements; the requirement is the 200 and the populated Summary shape.
Emulator: HTTP 400 and no Summary at all — the valid monetization web ACL cannot be created.
Why: this is the complement of Causes 1-4 and the more damaging half: the emulator refuses the only monetization configuration AWS accepts, so no monetization flow can be set up at all and every dependent step in the client's workflow fails.
Fix: accept MonetizationConfig and the Monetize rule action in the CreateWebACL handler (request model/validation) and persist them in the web ACL state so the Summary is returned.
Traces: wafv2-create-web-acl-monetize-rate-based-rule#task#2 (same cluster: wafv2-create-web-acl-monetize-without-monetization-config#task#2).

Cause 6 of 6: CreateRuleGroup REFUSES a well-formed monetization rule group that AWS creates.
AWS: Smithy: CreateRuleGroup returns Summary (RuleGroupSummary: Name, Id, ARN, Description, LockToken) with HTTP 200 for a valid request. The recorded success has ARN of the form `arn:aws:wafv2:<region>:<account-id>:global/rulegroup/<Name>/<Id>` and Description the empty string when none was supplied; Id/LockToken are per-run values.
Emulator: HTTP 400 and no Summary.
Why: a rule group that declares its MonetizationConfig — the exact remediation AWS asks for in Cause 2 — is rejected, so there is no valid path to a monetizing rule group.
Fix: same as Cause 5, in the CreateRuleGroup handler and rule-group state.
Traces: wafv2-create-rule-group-monetize-without-monetization-config#task#2.

## Evidence
Replay of simparity-wire-aws-v1 (emulator up; /__admin/health 200, data listener accepting). Six verdict=mismatch clusters, none owned by an open issue:

1. CreateWebACL wafv2-create-web-acl-monetize-without-monetization-config#task#1 — "status: expected 400, got 200; $.__type: missing (expected 'WAFInvalidParameterException', got None); $.message: missing (expected 'MonetizeAction requires MonetizationConfig to be configured on the WebACL. Provide a MonetizationConfig with payment network details.', got None)".
2. CreateRuleGroup wafv2-create-rule-group-monetize-without-monetization-config#task#1 — same shape with '... configured on the RuleGroup. ...'.
3. CreateWebACL wafv2-create-web-acl-monetize-rate-based-rule#task#1 — "status: expected 400, got 200; $.__type missing (expected 'WAFInvalidParameterException'); $.Field missing (expected 'RULE_ACTION'); $.Parameter missing (expected 'MonetizeAction(priceMultiplier=null)'); $.Reason missing (expected \"The parameter value isn't supported.\"); $.message missing (expected \"Error reason: The parameter value isn't supported., field: RULE_ACTION, parameter: MonetizeAction(priceMultiplier=null)\")".
4. CreateWebACL wafv2-create-web-acl-monetize-regional-scope#task#1 — "status: expected 400, got 200; $.__type missing (expected 'WAFInvalidOperationException'); $.Message missing (expected 'Monetization is not available for regional resources. Use CloudFront scope to configure monetization.')"; affected_count 2 (also wafv2-webacl-monetization-connected-account-configs-rejected#task#2).
5. CreateWebACL wafv2-create-web-acl-monetize-rate-based-rule#task#2 — "status: expected 200, got 400; $.Summary: missing (expected {'ARN': 'arn:aws:wafv2:us-east-1:000000000000:global/webacl/<name>/{{web_acl_id}}', 'Description': '', 'Id': '{{web_acl_id}}', 'LockToken': '{{web_acl_lock}}', 'Name': '<name>'}, got None)"; affected_count 2 (also wafv2-create-web-acl-monetize-without-monetization-config#task#2).
6. CreateRuleGroup wafv2-create-rule-group-monetize-without-monetization-config#task#2 — "status: expected 200, got 400; $.Summary: missing (expected {'ARN': 'arn:aws:wafv2:us-east-1:000000000000:global/rulegroup/<name>/{{rule_group_id}}', ...}, got None)".

The names, account id, region and {{...}} identifiers above are this corpus's per-run values; the requirements are the statuses, the error classes, the fixed message text, and the populated Summary.

Model references used: com.amazonaws.wafv2#MonetizationConfig, com.amazonaws.wafv2#MonetizeAction, com.amazonaws.wafv2#RuleAction$Monetize, com.amazonaws.wafv2#ParameterExceptionField (contains RULE_ACTION and MONETIZATION_CONFIG), CreateWebACL/CreateRuleGroup error lists (WAFInvalidParameterException and WAFInvalidOperationException, both client/400).

The open issue 20261029T192556Z-wafv2-request-validation-never-refuses-ten-operations-answer covers ten OTHER never-refused operations (ListWebACLs, ListAPIKeys, CreateAPIKey, GetSampledRequests, ListSettlementRecords, PutLoggingConfiguration malformed ARN, DeletePermissionPolicy, AssociateWebACL, GetWebACLForResource, DisassociateWebACL); it carries none of the six replay keys above and does not mention monetization.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: CreateWebACL, Scope=CLOUDFRONT, with one rule whose Action is {"Monetize": {}} and NO MonetizationConfig -> expect HTTP 400 WAFInvalidParameterException with the 'MonetizeAction requires MonetizationConfig to be configured on the WebACL...' message; emulator answers 200 with a Summary.
3. Cause 2: CreateRuleGroup with a Monetize rule action and no MonetizationConfig -> expect the same refusal naming RuleGroup; emulator answers 200.
4. Cause 3: CreateWebACL, Scope=CLOUDFRONT, with a RateBasedStatement rule whose Action is Monetize (plus a MonetizationConfig) -> expect 400 WAFInvalidParameterException, Field RULE_ACTION, Parameter MonetizeAction(priceMultiplier=<value|null>); emulator answers 200.
5. Cause 4: CreateWebACL with Scope=REGIONAL and a MonetizationConfig/Monetize rule -> expect 400 WAFInvalidOperationException with the 'Monetization is not available for regional resources...' message; emulator answers 200.
6. Causes 5-6: re-issue the Cause 1 CreateWebACL and the Cause 2 CreateRuleGroup WITH a valid MonetizationConfig (CryptoConfig + CurrencyMode) and non-rate-based Monetize rules -> expect HTTP 200 and a populated Summary (Name/Id/ARN/Description/LockToken); emulator answers 400 with no Summary.
