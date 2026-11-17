Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.update-provisioned-limit-out-of-range-value-rejected
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-identity-provider|UpdateProvisionedLimit|InvalidParameterException
Issue-Commit: f6d8f01946911e76d57d5db06ea3a96a4efb6135

# cognito-idp UpdateProvisionedLimit: re-applying the account's current (default) rate is refused 400 InvalidParameterException "Requested value is the same as the current provisioned limit" instead of being accepted with 200

> _Note: this issue was identified against the code base at commit `f6d8f01946911e76d57d5db06ea3a96a4efb6135`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateProvisionedLimit

## Description
REOPENS 20261029T182116Z-cognito-idp-provisioned-limit-api-diverges-no-default.md: its fix left the call refused.

That resolved issue's Cause 3 added a same-value gate to UpdateProvisionedLimit. The gate now refuses an in-range write that AWS accepts, so a legitimate UpdateProvisionedLimit call is answered with an error. This is not a new defect report — it is the same operation left refusing.

Cause 1 of 1 — the no-op gate refuses a RequestedLimitValue equal to the rate the account currently enforces, which AWS accepts.
AWS: Smithy: UpdateProvisionedLimit input {LimitDefinition (required), RequestedLimitValue (IntegerType, required)}, output {Limit (LimitType, required)} where LimitType carries ProvisionedLimitValue ("the rate that Amazon Cognito currently enforces for your account") and FreeLimitValue ("the default (free) limit value ... the rate included at no additional cost"); errors InvalidParameterException 400, ResourceNotFoundException 404, ServiceQuotaExceededException 400, NotAuthorizedException 403, TooManyRequestsException 429. The operation's own documentation states the accepted range: "Sets the provisioned limit for a specific API category. The value must be between the default limit and your account-level maximum limit in Service Quotas" — the default (free) limit is the BOTTOM of the accepted range, so a value equal to it is in range. Recorded behavior: for LimitDefinition {LimitClass: API_CATEGORY, Attributes: {Category: UserAuthentication}}, UpdateProvisionedLimit with RequestedLimitValue equal to the rate GetProvisionedLimit currently reports (ProvisionedLimitValue when the account carries one, otherwise FreeLimitValue) succeeds with HTTP 200 and Limit.ProvisionedLimitValue == RequestedLimitValue; the check uses exactly that call as its positive control before probing the over-ceiling refusal.
Emulator: the same call is refused with HTTP 400 InvalidParameterException and the message "Requested value is the same as the current provisioned limit" (emulator-authored text carrying no request or state values at all — AWS writes no message here because it answers 200 with the Limit). The refusal happens on the first write of the check, so the account's rate is never provisioned.
Hypothesis: the same-value comparison is made against the account's EFFECTIVE rate — falling back to FreeLimitValue when no explicit provisioned value is stored — so the first provisioning of a category at its default rate, and any idempotent re-apply of the current rate, both trip the gate. AWS only treats a value outside the [default limit, Service Quotas ceiling] range as an error.
Why: a client that re-applies the rate it just read (idempotent configuration, infrastructure-as-code convergence, retry after a timeout) gets a 400 from the emulator where AWS returns 200 with the Limit; the caller cannot establish a provisioned value equal to the default at all, and any provisioning code that treats InvalidParameterException as a malformed request aborts its run. It also hides every subsequent assertion in the flow: the suite's over-ceiling probe (RequestedLimitValue far above the ceiling, which must answer ServiceQuotaExceededException — already owned by open issue 20261112T090407Z-cognito-idp-2-net-new-causes-in-updateprovisionedlimit.md, Cause 1) is never reached because this control call fails first.
Fix: the same-value/no-op gate in the UpdateProvisionedLimit handler and the provisioned-limit state function it calls — an in-range RequestedLimitValue (default limit <= value <= the account's Service Quotas ceiling) must be stored and echoed with HTTP 200, including when it equals the rate currently enforced or the category's FreeLimitValue; only out-of-range values are refused.
Traces: cognito-idp.update-provisioned-limit-out-of-range-value-rejected.

## Evidence
Verbatim from the replay (cognito-idp.update-provisioned-limit-out-of-range-value-rejected, verdict error):

  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
  botocore.errorfactory.InvalidParameterException: An error occurred (InvalidParameterException) when calling the UpdateProvisionedLimit operation: Requested value is the same as the current provisioned limit

The call that raised it is the check's positive control, built from the value GetProvisionedLimit had just returned:
  baseline = get_provisioned_limit(LimitDefinition={'LimitClass': 'API_CATEGORY', 'Attributes': {'Category': 'UserAuthentication'}})['Limit']
  current_value = baseline.get('ProvisionedLimitValue', baseline['FreeLimitValue'])
  control = update_provisioned_limit(LimitDefinition=<same>, RequestedLimitValue=current_value)['Limit']
  assert control['ProvisionedLimitValue'] == current_value

Expected vs actual:
- expected: HTTP 200 with Limit.ProvisionedLimitValue == RequestedLimitValue (== the rate currently enforced), no error.
- actual: HTTP 400, Code InvalidParameterException, Message "Requested value is the same as the current provisioned limit" — the whole sentence is emulator-authored fixed text with no variable parts; AWS emits no error for this request.
The refusal aborts the check before its real subject (an absurd RequestedLimitValue of 2,000,000,000 must be refused ServiceQuotaExceededException while leaving the limit unchanged) is exercised.

## Steps to Reproduce
1. cognito-idp GetProvisionedLimit with LimitDefinition={'LimitClass': 'API_CATEGORY', 'Attributes': {'Category': 'UserAuthentication'}}; note V = Limit.ProvisionedLimitValue if present, else Limit.FreeLimitValue.
2. cognito-idp UpdateProvisionedLimit with the same LimitDefinition and RequestedLimitValue = V.
   Expect: HTTP 200 and Limit.ProvisionedLimitValue == V (the default limit is the bottom of the accepted range, so V is in range).
   Observed: HTTP 400 InvalidParameterException "Requested value is the same as the current provisioned limit".
3. GetProvisionedLimit again — the rate is unchanged and was never provisioned.
4. (Blocked by step 2, tracked separately by 20261112T090407Z-...) UpdateProvisionedLimit with RequestedLimitValue = 2000000000 -> expect ServiceQuotaExceededException with the limit left unchanged.
