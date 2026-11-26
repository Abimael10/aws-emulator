Issue-Source: trace-sweep-divergence
Issue-Trace: marketplace-entitlement-service-get-entitlements-unentitled-product-and-invalid-token
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: marketplace-entitlement-service-get-entitlements-unentitled-product-and-invalid-token
Issue-Shape-Id: com.amazonaws.marketplaceentitlementservice#AWSMPEntitlementService
Issue-Key: marketplace-entitlement|GetEntitlements|4xx->4xx|$.message
Issue-Commit: f438550964d9638a1a39e41f0ff9e83de9e635fd

# GetEntitlements invalid-NextToken InvalidParameterException message is reworded instead of AWS's "Invalid NextToken: <token>"

> _Note: this issue was identified against the code base at commit `f438550964d9638a1a39e41f0ff9e83de9e635fd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetEntitlements

## Description
Single cause (Cause 1 of 1): the error message AWS Marketplace Entitlement Service returns when GetEntitlements is called with a NextToken the service did not issue is replaced by an emulator-authored sentence.

AWS: Smithy (services/service-com-amazonaws-marketplaceentitlementservice-awsmpentitlementservice) models GetEntitlements with input members ProductCode (required, @length 1..255), Filter, NextToken (target NonEmptyString, @pattern ^\\S+$) and MaxResults (@range 1..25), @paginated {inputToken: NextToken, outputToken: NextToken, pageSize: MaxResults}, and errors InternalServiceErrorException (500), InvalidParameterException (client, httpError 400, doc "One or more parameters in your request was invalid.") and ThrottlingException. The model does not state the message text, so for the wording this is recorded behavior only: the recording shows HTTP 400 with an InvalidParameterException whose message is the fixed text "Invalid NextToken: " followed immediately by the rejected token exactly as it arrived in the request's NextToken member. In that message the only variable part is that token value; everything up to and including "Invalid NextToken: " is fixed text, and there is no mention of ProductCode, Filter, or pagination provenance.

Emulator: the emulator gets the status and error class right (400, same error) but serves message "NextToken is not a pagination token this service issued for this ProductCode and Filter" — a sentence of its own composition that drops the "Invalid NextToken: " prefix entirely and never echoes the offending token.

Why: a client relying on AWS's behavior cannot recover the rejected token from the message, and any log scraping, user-facing surfacing or test assertion keyed on AWS's "Invalid NextToken: <token>" wording (the only place the service names which token it refused) fails to match. Because the error code and status are identical, the divergence is invisible to code-based handling and only shows up as wrong text at the point where it is read by a human or a matcher.

Fix: the GetEntitlements handler's NextToken validation path in the marketplace-entitlement service crate — the site that constructs the InvalidParameterException for an unissued/unparseable pagination token — must emit the fixed prefix "Invalid NextToken: " concatenated with the request's NextToken value verbatim, rather than a hand-written explanatory sentence. No status or error-shape change is needed.

Traces: marketplace-entitlement-service-get-entitlements-unentitled-product-and-invalid-token#task#2 (expected message "Invalid NextToken: <token-from-request>", got "NextToken is not a pagination token this service issued for this ProductCode and Filter"; $.message value_mismatch, expected_status 400 == actual_status 400).

## Evidence
Replay divergence key service=marketplace-entitlement|GetEntitlements|status=400->400|keys=$.message, verdict=mismatch.
Expected (recorded AWS response): HTTP 400, InvalidParameterException, $.message = "Invalid NextToken: sim2real-probe-marketplace-entitlement-unissued-token" — i.e. fixed text "Invalid NextToken: " + the NextToken value supplied in the request (that trailing token string is this recording's request value, not part of the template).
Actual (emulator): HTTP 400, same error class, $.message = "NextToken is not a pagination token this service issued for this ProductCode and Filter" — fully reworded, no token echoed.
Smithy citation: GetEntitlements input NextToken -> NonEmptyString @pattern ^\\S+$; error InvalidParameterException errorKind=client httpError=400 (defaulted), doc "One or more parameters in your request was invalid."; @paginated inputToken=NextToken.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture marketplace-entitlement-service-get-entitlements-unentitled-product-and-invalid-token.
2. At step task#2, GetEntitlements is called with a ProductCode and a NextToken value the emulator never issued.
3. Observe the response is HTTP 400 InvalidParameterException (correct) but $.message is "NextToken is not a pagination token this service issued for this ProductCode and Filter".
4. Expected: $.message is "Invalid NextToken: " immediately followed by the NextToken value from the request.
