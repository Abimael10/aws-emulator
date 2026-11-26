Issue-Source: trace-sweep-divergence
Issue-Trace: wafv2-webacl-monetization-connected-account-configs-rejected
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wafv2-webacl-monetization-connected-account-configs-rejected
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2|CreateWebACL|4xx->2xx|$.Message
Issue-Key: wafv2|CreateWebACL|4xx->2xx|$.__type
Issue-Key: wafv2|CreateWebACL|4xx->2xx|status
Issue-Commit: c0140e598b65532f73885844c755a79e994abde7

# wafv2 CreateWebACL accepts a MonetizationConfig naming a nonexistent connected account

> _Note: this issue was identified against the code base at commit `c0140e598b65532f73885844c755a79e994abde7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateWebACL

## Description
CreateWebACL creates a web ACL whose MonetizationConfig references a connected (Payments) account that does not exist, answering HTTP 200 with a normal WebACLSummary, where AWS WAF refuses the request with a modeled client error. This is ONE issue with ONE cause.

Cause 1 of 1: CreateWebACL with a monetization/connected-account configuration whose connected account cannot be resolved.
AWS: recorded behavior only for the connected-account member itself — the on-disk Smithy model (com.amazonaws.wafv2#AWSWAF_20190729) exposes MonetizationConfig with only CryptoConfig and CurrencyMode and has no connected-account member, so the model states nothing about this precondition. What the model does state is that CreateWebACL binds WAFNonexistentItemException (errorKind client, httpError 400, single member Message of type ErrorMessage), which is the class the recording uses. The recorded AWS response is HTTP 400 with __type 'WAFNonexistentItemException' and Message 'The specified connected account does not exist. Verify the ConnectedAccountArn. If the account was deleted and recreated, it has a new ARN - use the current one from the Payments console.' That whole sentence is fixed text for this error class — it carries no variable parts at all: it names the member ('ConnectedAccountArn') generically rather than echoing the rejected ARN, so it must be reproduced verbatim, including the hyphen in 'new ARN - use the current one'. The condition that triggers it is general: the connected account named by the request's monetization configuration is absent from the account's Payments state (never created, or deleted and recreated so that the old ARN no longer resolves).
Emulator: HTTP 200 with a successful CreateWebACL body; __type and Message are both absent, and the web ACL is written to state with the unresolvable monetization configuration attached.
Why: a client that relies on WAF validating the connected account at create time gets a web ACL that looks monetization-enabled but is bound to an account that cannot receive settlement. The failure moves from a create-time 400 the client can handle (re-read the current ARN from the Payments console) to a silent runtime condition: Monetize-action rules reference payment state that does not exist, no error is ever surfaced, and the client's error-handling path for a stale or deleted connected account is never exercised. Teardown and reconciliation also cannot distinguish 'account configured' from 'account never existed'.
Fix: the connected-account resolution precondition in the CreateWebACL handler's validation — the monetization configuration must be resolved against the stored Payments/connected-account state before the web ACL is written, and an unresolvable reference must raise WAFNonexistentItemException (400) with WAF's fixed sentence rather than falling through to the success serializer. The same precondition belongs on the update path for consistency.
Note: the open issue 20261029T192556Z-wafv2-request-validation-never-refuses-ten-operations-answer.md covers ten other wafv2 operations that answer 200 where WAF returns a modeled 400, but it does not carry this CreateWebACL replay key and does not cover the monetization/connected-account precondition; 20261117T233522Z-wafv2-web-acl-subordinate-state-diverges-logging-configuration.md covers other web-ACL subordinate state (logging, SDK injection), not this one.
Traces: wafv2-webacl-monetization-connected-account-configs-rejected#task#2.

## Evidence
Replay of simparity-wire-aws-v1 against the booted emulator (emulator_up=true, /__admin/health answered 200). Cluster verdict=mismatch, service=wafv2, operation=CreateWebACL, divergence_key 'service=wafv2|CreateWebACL|status=400->200|keys=$.Message,$.__type,status', affected_count=1.

Fixture wafv2-webacl-monetization-connected-account-configs-rejected, step task#2:
  expected_status 400, actual_status 200
  'status: expected 400, got 200'
  '$.__type: missing (expected \'WAFNonexistentItemException\', got None)'
  '$.Message: missing (expected \'The specified connected account does not exist. Verify the ConnectedAccountArn. If the account was deleted and recreated, it has a new ARN - use the current one from the Payments console.\', got None)'

Expected (AWS, as recorded): HTTP 400, body {"__type": "WAFNonexistentItemException", "Message": "The specified connected account does not exist. Verify the ConnectedAccountArn. If the account was deleted and recreated, it has a new ARN - use the current one from the Payments console."}.
Actual (emulator): HTTP 200 with a CreateWebACL success body; neither __type nor Message present.

Model support: smithy_query on services/service-com-amazonaws-wafv2-awswaf-20190729/smithy-model/com-amazonaws-wafv2-awswaf-20190729.json shows CreateWebACL binds WAFNonexistentItemException (client, httpError 400, member Message: ErrorMessage) among its errors, and that MonetizationConfig in this model revision has only CryptoConfig and CurrencyMode (no connected-account member) — hence the 'recorded behavior only' basis for the precondition itself. The error message above is entirely fixed text for this class; no part of it is taken from the request, so no value from this recording is a requirement.

## Steps to Reproduce
1. Clone the repo and boot the emulator from mainline; replay the wire corpus (replay_wire_corpus) and look at fixture wafv2-webacl-monetization-connected-account-configs-rejected, step task#2.
2. Equivalently, by hand: against the emulator's wafv2 endpoint, call CreateWebACL with a valid Name/Scope/DefaultAction/VisibilityConfig and a MonetizationConfig whose connected-account reference (ConnectedAccountArn) names an account that was never created in the emulator's Payments/connected-account state (or one that was deleted and recreated, so the supplied ARN is stale).
3. Observe HTTP 200 with a WebACLSummary body, and the web ACL readable afterwards via GetWebACL/ListWebACLs with the monetization configuration attached.
4. AWS WAF instead answers HTTP 400 with __type 'WAFNonexistentItemException' and Message 'The specified connected account does not exist. Verify the ConnectedAccountArn. If the account was deleted and recreated, it has a new ARN - use the current one from the Payments console.', and no web ACL is created.
5. After the fix, re-replay wafv2-webacl-monetization-connected-account-configs-rejected#task#2 and confirm the step passes.
