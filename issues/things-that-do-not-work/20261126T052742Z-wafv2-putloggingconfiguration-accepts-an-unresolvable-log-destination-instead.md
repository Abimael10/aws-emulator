Issue-Source: trace-sweep-divergence
Issue-Trace: wafv2-cloudfront-webacl-logging-log-group-region
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wafv2-cloudfront-webacl-logging-log-group-region
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2|PutLoggingConfiguration|4xx->2xx|$.__type
Issue-Key: wafv2|PutLoggingConfiguration|4xx->2xx|status
Issue-Related: 20261117T233522Z-wafv2-web-acl-subordinate-state-diverges-logging-configuration.md
Issue-Commit: a4526bacfb21144d2b4adc80fbe556180514c444

# wafv2 PutLoggingConfiguration accepts an unresolvable log destination instead of refusing WAFNonexistentItemException

> _Note: this issue was identified against the code base at commit `a4526bacfb21144d2b4adc80fbe556180514c444`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutLoggingConfiguration

## Description
PutLoggingConfiguration performs no existence check on the logging destination it is handed: a destination that WAF cannot resolve for the target web ACL's scope is stored and acknowledged with HTTP 200 instead of being refused. This is ONE issue with ONE cause; it must be closed before the fix is submitted.

Cause 1 of 1: PutLoggingConfiguration accepts a logging configuration whose destination does not exist for the web ACL's scope.
AWS: Smithy: PutLoggingConfiguration (services/service-com-amazonaws-wafv2-awswaf-20190729 model) takes a required LoggingConfiguration member and models WAFNonexistentItemException (errorKind client, httpError 400), documented "WAF couldn’t perform the operation because your resource doesn't exist. If you've just created a resource that you're using in this operation, you might just need to wait a few minutes...". For a CLOUDFRONT-scoped web ACL WAF resolves the log destination in the scope's region, so a CloudWatch Logs log group that lives in a different region does not exist as far as WAF is concerned and the call is refused. Recorded response: HTTP 400 with $.__type 'WAFNonexistentItemException' and $.Message 'AWS WAF couldn’t perform the operation because your resource doesn’t exist.' — that message is entirely FIXED text for this error class (note the typographic apostrophes U+2019 in couldn’t/doesn’t); it carries no variable parts taken from the request or from state, so the requirement is the exact string, not a value from this recording. Hypothesis: the refusal is keyed on the destination log group not existing in the region implied by the web ACL's scope — the recording shows only the refusal, not WAF's internal reason.
Emulator: HTTP 200 with a LoggingConfiguration echo body; $.__type and $.Message are absent entirely, and the configuration is written to state.
Why: a client that mis-targets a log destination (wrong region for the scope) is told logging is configured when AWS would have rejected the request, so WAF log delivery silently never happens and the mistake is never surfaced; "logging is enabled" assertions pass against a configuration AWS never stored, and the subsequent read serves a configuration that does not exist.
Fix: destination existence/resolution validation in the PutLoggingConfiguration handler, before the configuration is persisted to the web ACL's state, raising WAFNonexistentItemException (HTTP 400, __type 'WAFNonexistentItemException') with the fixed message above.
Traces: wafv2-cloudfront-webacl-logging-log-group-region#task#4.

Related open issues (neither owns this replay key as reported by the replay): 20261117T233522Z-wafv2-web-acl-subordinate-state-diverges-logging-configuration.md describes the same fixture family but its recorded replay keys for PutLoggingConfiguration cover only the $.Message difference, not the status 400->200 divergence or the missing $.__type reported here; 20261029T192556Z-wafv2-request-validation-never-refuses-ten-operations-answer.md covers a DIFFERENT PutLoggingConfiguration refusal (malformed destination ARN -> WAFInvalidParameterException), a different error class. The replay reported this cluster with an empty owned_by, so it is filed here with its exact fixture/step binding.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, /__admin/health 200, score_pct 94.2). One verdict=mismatch cluster, owned_by empty, stub_path empty:

- service wafv2, operation PutLoggingConfiguration, fixture wafv2-cloudfront-webacl-logging-log-group-region, step task#4, divergence_key "service=wafv2|PutLoggingConfiguration|status=400->200|keys=$.Message,$.__type,status", affected_count 1.
  detail: "status: expected 400, got 200\n$.__type: missing (expected 'WAFNonexistentItemException', got None)\n$.Message: missing (expected 'AWS WAF couldn’t perform the operation because your resource doesn’t exist.', got None)"

Expected (AWS, from the recording): HTTP 400, $.__type = 'WAFNonexistentItemException', $.Message = 'AWS WAF couldn’t perform the operation because your resource doesn’t exist.' (fixed text for the error class; no request- or state-derived substitutions).
Actual (emulator): HTTP 200, no $.__type, no $.Message, logging configuration accepted and stored.

Model references used: PutLoggingConfiguration (input LoggingConfiguration required; errors include WAFNonexistentItemException client/httpError 400, WAFInvalidParameterException client/400, WAFLogDestinationPermissionIssueException client/400); com.amazonaws.wafv2#WAFNonexistentItemException documentation "WAF couldn’t perform the operation because your resource doesn't exist...".

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from it.
2. Call replay_wire_corpus() and look at fixture wafv2-cloudfront-webacl-logging-log-group-region, step task#4.
3. Manually: create a CLOUDFRONT-scoped web ACL, then call PutLoggingConfiguration for that web ACL's ARN with a LogDestinationConfigs entry naming a CloudWatch Logs log group that does not exist in the region implied by the CLOUDFRONT scope (us-east-1).
4. Expected: HTTP 400 with $.__type 'WAFNonexistentItemException' and $.Message 'AWS WAF couldn’t perform the operation because your resource doesn’t exist.'
5. Observed: HTTP 200 with the LoggingConfiguration echoed back and the configuration persisted (a following GetLoggingConfiguration then serves it).
