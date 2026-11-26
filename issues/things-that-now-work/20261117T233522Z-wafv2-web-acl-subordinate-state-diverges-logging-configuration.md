Issue-Source: trace-sweep-divergence
Issue-Trace: wafv2-cloudfront-webacl-logging-log-group-region
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wafv2-cloudfront-webacl-logging-log-group-region wafv2-challenge-javascript-sdk-injection-accepted-and-read-back
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2|PutLoggingConfiguration|4xx->2xx|$.Message
Issue-Key: wafv2|GetLoggingConfiguration|4xx->2xx|$.Message
Issue-Key: wafv2|GetLoggingConfiguration|4xx->2xx|$.__type
Issue-Key: wafv2|GetLoggingConfiguration|4xx->2xx|status
Issue-Key: wafv2|GetWebACL|2xx->2xx|$.ApplicationIntegrationURL
Issue-Key: wafv2|GetWebACL|2xx->2xx|$.WebACL.AssociationConfig.ChallengeJavascriptSdkInjection
Issue-Related: 20261029T192556Z-wafv2-request-validation-never-refuses-ten-operations-answer.md
Issue-Commit: 5f9152346f0a34f19c016441be64bcf6cb300f64

# wafv2 web ACL subordinate state diverges: logging configuration accepted for an unresolvable destination and read back, SDK-injection config dropped

> _Note: this issue was identified against the code base at commit `5f9152346f0a34f19c016441be64bcf6cb300f64`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutLoggingConfiguration

## Description
State that hangs off a web ACL — its logging configuration and its AssociationConfig — is not modeled the way WAF models it: a logging configuration whose destination WAF cannot resolve is stored and served instead of refused, and a web ACL's challenge-SDK-injection association setting is silently lost between create and read. This is ONE issue with THREE causes; all three must be closed before the fix is submitted.

Cause 1 of 3: PutLoggingConfiguration accepts a destination that does not exist for the web ACL's scope.
AWS: Smithy: PutLoggingConfiguration takes a required LoggingConfiguration and models WAFNonexistentItemException (client, httpError 400), documented "WAF couldn't perform the operation because your resource doesn't exist." For a CLOUDFRONT-scoped web ACL, WAF resolves the log destination in the scope's region (us-east-1); a CloudWatch Logs log group that lives in another region does not exist as far as WAF is concerned, so the call is refused with HTTP 400, __type `WAFNonexistentItemException` and Message `AWS WAF couldn’t perform the operation because your resource doesn’t exist.` — fixed text, no variable parts, with the typographic apostrophes AWS uses. Hypothesis: the refusal is keyed on the destination log group not existing in the region implied by the web ACL's scope (the recording only shows the refusal, not WAF's internal reason).
Emulator: HTTP 200 — the configuration is accepted and stored; no __type and no Message.
Why: logging looks configured while no destination exists, so WAF log delivery silently never happens and the client's region mistake is never reported; an operator's "logging is on" assertion passes against a configuration AWS would have rejected.
Fix: destination resolution (existence in the scope's region) in the PutLoggingConfiguration handler, before the configuration is written to state.
Traces: wafv2-cloudfront-webacl-logging-log-group-region#task#4. Note: the open issue 20261029T192556Z-wafv2-request-validation-never-refuses-ten-operations-answer covers a DIFFERENT PutLoggingConfiguration refusal (a malformed destination ARN -> WAFInvalidParameterException) and does not carry this replay key or this error class.

Cause 2 of 3: GetLoggingConfiguration serves a configuration for a web ACL that has none.
AWS: Smithy: GetLoggingConfiguration takes a required ResourceArn (@length 20..2048, @pattern \\S) and models WAFNonexistentItemException (client, 400). Because the preceding PutLoggingConfiguration was refused, no logging configuration exists for that ResourceArn, and the read is refused with HTTP 400, __type `WAFNonexistentItemException` and the same fixed Message `AWS WAF couldn’t perform the operation because your resource doesn’t exist.` The requirement is that a web ACL with no stored logging configuration produces this refusal rather than a body.
Emulator: HTTP 200 with a LoggingConfiguration body — the configuration it wrongly accepted in Cause 1; __type and Message absent.
Why: "no logging configured" is indistinguishable from "logging configured", so a client cannot detect the missing configuration and reconciliation/drift checks read a configuration that AWS never stored.
Fix: the existence check in the GetLoggingConfiguration handler against the web ACL's stored logging configuration (it follows from Cause 1 but must be verified independently, since the read must refuse for any web ACL that has no configuration).
Traces: wafv2-cloudfront-webacl-logging-log-group-region#task#5.

Cause 3 of 3: GetWebACL drops AssociationConfig.ChallengeJavascriptSdkInjection and returns no ApplicationIntegrationURL.
AWS: Smithy: GetWebACL's output carries ApplicationIntegrationURL (target OutputUrl) alongside WebACL and LockToken; it is populated for a web ACL that uses the CAPTCHA/Challenge JavaScript SDK integration, and is the URL the client embeds to load the SDK. The on-disk model's AssociationConfig declares only RequestBody, so for the ChallengeJavascriptSdkInjection member this is recorded behavior only: a CreateWebACL that sets AssociationConfig.ChallengeJavascriptSdkInjection is accepted, and the subsequent GetWebACL echoes that member back under $.WebACL.AssociationConfig.ChallengeJavascriptSdkInjection with the same resource-type keys the request supplied (CLOUDFRONT in the recording, each mapping to an empty configuration object), together with a non-empty ApplicationIntegrationURL. The host and integration id inside that URL are per-environment values, not requirements — the requirement is that the member is present and non-empty.
Emulator: HTTP 200, but $.ApplicationIntegrationURL is null/absent and $.WebACL.AssociationConfig.ChallengeJavascriptSdkInjection is null/absent — the setting accepted at create time is not in the readback.
Why: a client that enables challenge JS SDK injection cannot confirm the setting and has no integration URL to embed, so the SDK can never be wired into the protected application; worse, any read-modify-write cycle (GetWebACL then UpdateWebACL) silently strips the setting from the web ACL.
Fix: persist AssociationConfig.ChallengeJavascriptSdkInjection in the web ACL state and serialize it, plus ApplicationIntegrationURL, in the GetWebACL handler's response. (The open issue 20261109T140923Z-wafv2-listapikeys-applicationintegrationurl-carries-a-32-character-integration concerns the FORM of the integration id in ListAPIKeys' URL; here GetWebACL emits no URL at all and does not carry this replay key.)
Traces: wafv2-challenge-javascript-sdk-injection-accepted-and-read-back#task#2.

## Evidence
Replay of simparity-wire-aws-v1 (emulator up; /__admin/health 200). Three verdict=mismatch clusters, none owned by an open issue:

1. PutLoggingConfiguration wafv2-cloudfront-webacl-logging-log-group-region#task#4 — "status: expected 400, got 200; $.__type: missing (expected 'WAFNonexistentItemException', got None); $.Message: missing (expected 'AWS WAF couldn\u2019t perform the operation because your resource doesn\u2019t exist.', got None)".
2. GetLoggingConfiguration wafv2-cloudfront-webacl-logging-log-group-region#task#5 — identical detail set: expected 400 with __type 'WAFNonexistentItemException' and the same Message; got 200 with neither.
3. GetWebACL wafv2-challenge-javascript-sdk-injection-accepted-and-read-back#task#2 — "$.ApplicationIntegrationURL: missing (expected '<shaped-not-asserted>', got None); $.WebACL.AssociationConfig.ChallengeJavascriptSdkInjection: missing (expected {'CLOUDFRONT': {}}, got None)" at expected_status 200 / actual_status 200. The CLOUDFRONT key is the resource type the recorded request configured; the requirement is that whatever the request set is echoed back.

Model references used: PutLoggingConfiguration (input LoggingConfiguration required; errors include WAFNonexistentItemException client/400 and WAFInvalidParameterException client/400), GetLoggingConfiguration (input ResourceArn required @length 20..2048 @pattern \\S; errors include WAFNonexistentItemException client/400), GetWebACL (output WebACL, LockToken, ApplicationIntegrationURL -> OutputUrl; errors include WAFNonexistentItemException), com.amazonaws.wafv2#WAFNonexistentItemException (client, httpError 400, Message -> ErrorMessage), com.amazonaws.wafv2#AssociationConfig and com.amazonaws.wafv2#WebACL$AssociationConfig.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: create a CLOUDFRONT-scoped web ACL, then PutLoggingConfiguration for it naming a CloudWatch Logs log group that does not exist in the scope's region (us-east-1) -> expect HTTP 400 WAFNonexistentItemException with Message 'AWS WAF couldn\u2019t perform the operation because your resource doesn\u2019t exist.'; the emulator answers 200 and stores the configuration.
3. Cause 2: immediately GetLoggingConfiguration for the same web ACL ARN -> expect the same HTTP 400 WAFNonexistentItemException, because no configuration was ever stored; the emulator answers 200 with a LoggingConfiguration body.
4. Cause 3: CreateWebACL with AssociationConfig.ChallengeJavascriptSdkInjection set for a resource type (CLOUDFRONT), then GetWebACL for it -> expect 200 whose $.WebACL.AssociationConfig.ChallengeJavascriptSdkInjection echoes the configured resource-type keys and whose $.ApplicationIntegrationURL is non-empty; the emulator returns 200 with both absent.
