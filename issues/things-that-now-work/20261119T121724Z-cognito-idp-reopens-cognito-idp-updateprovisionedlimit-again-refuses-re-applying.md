Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.update-provisioned-limit-out-of-range-value-rejected
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-identity-provider|UpdateProvisionedLimit|InvalidParameterException
Issue-Commit: 93847454bd4acf14d3c65fb9240f255701f52b4f

# REOPENS: cognito-idp UpdateProvisionedLimit again refuses re-applying the value GetProvisionedLimit reports (400 InvalidParameterException), so the limit cannot be written from the service's own state

> _Note: this issue was identified against the code base at commit `93847454bd4acf14d3c65fb9240f255701f52b4f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateProvisionedLimit

## Description
REOPENS 20261118T042515Z-cognito-idp-updateprovisionedlimit-now-refuses-re-applying-the.md: its fix left the call refused.

That issue carried this very check id (Issue-Trace-Ids: cognito-idp.update-provisioned-limit-out-of-range-value-rejected) and is marked fixed, yet the same call is refused again with the same status, error code and message. 1 cause.

Cause 1 of 1: the same-value / no-op gate in UpdateProvisionedLimit fires on a request AWS accepts — re-applying, for a LimitDefinition the account has never explicitly provisioned, the value GetProvisionedLimit itself just reported.
AWS: Smithy (com.amazonaws.cognitoidentityprovider#UpdateProvisionedLimit): input LimitDefinition (LimitDefinitionType, required) + RequestedLimitValue (IntegerType, required); output Limit (LimitType) REQUIRED; modeled errors InvalidParameterException 400, NotAuthorizedException 403, ResourceNotFoundException 404, ServiceQuotaExceededException 400, TooManyRequestsException 429, InternalErrorException 500. GetProvisionedLimit has the same required Limit output. Recorded behavior: for LimitClass API_CATEGORY with Attributes {Category: <an adjustable category>} that the account has not explicitly provisioned — the state in which the Limit's ProvisionedLimitValue is simply the category's default, equal to FreeLimitValue — reading the limit and then calling UpdateProvisionedLimit with RequestedLimitValue set to exactly that value SUCCEEDS with HTTP 200 and returns Limit.ProvisionedLimitValue equal to the requested value. Provisioning the default rate is an ordinary accepted write, not a rejected no-op. The only refusal AWS makes for an out-of-range value on this operation is ServiceQuotaExceededException (400), and it leaves the provisioned limit unchanged.
Emulator: the in-range re-apply is answered HTTP 400 InvalidParameterException with the message "Requested value is the same as the current provisioned limit" — that whole sentence is fixed text and carries no value from the request or from state, and AWS produces no such refusal for this condition. Because the control call raises, the check never reaches the out-of-range leg, so the ServiceQuotaExceededException path for an absurdly large RequestedLimitValue and the "stored limit is unchanged afterwards" assertion are both left unverified by this run.
Why: a client that reads the current limit and writes it back — the idempotent apply that configuration tooling, retries and reconciliation loops all perform — gets a hard 400 instead of the required Limit structure. There is then no RequestedLimitValue derivable from the service's own GetProvisionedLimit answer that the emulator will accept, so the provisioned limit can never be written from observed state, and an over-quota request (ServiceQuotaExceededException) becomes indistinguishable from this benign one (both are 400).
Fix: the same-value / no-op gate in the UpdateProvisionedLimit handler and the account-level provisioned-limit state of the cognito-idp crate. The gate must be conditioned on there being an EXPLICITLY provisioned value to collide with: a request whose RequestedLimitValue equals the category's default (the value also reported as FreeLimitValue, i.e. nothing has been provisioned yet) is an in-range write that must be stored and echoed back in the required Limit output. Note the pendulum: open issue 20261119T040922Z-cognito-idp-updateprovisionedlimit-accepts-a-requestedlimitvalue-equal-to.md requires the refusal for a value equal to an ALREADY-provisioned ProvisionedLimitValue, and open wire issue 20261118T085409Z-cognito-idp-updateprovisionedlimit-refuses-a-valid-request-with.md records the same 400 on fixture cognito-idp-provisioned-limit-round-trip#task#2 — neither carries this check's replay key. Satisfying all three requires the conditional gate above, not flipping the gate on and off wholesale; only the over-ceiling case belongs to ServiceQuotaExceededException.
Traces: cognito-idp.update-provisioned-limit-out-of-range-value-rejected.

## Evidence
Verbatim from the replay (service heading: cognito-idp):

[error] cognito-idp.update-provisioned-limit-out-of-range-value-rejected
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
  botocore.errorfactory.InvalidParameterException: An error occurred (InvalidParameterException) when calling the UpdateProvisionedLimit operation: Requested value is the same as the current provisioned limit

The raising call is the check's POSITIVE CONTROL, which re-applies the value GetProvisionedLimit just reported (check source, lines 70-82):
  baseline = cognito.get_provisioned_limit(LimitDefinition={"LimitClass": "API_CATEGORY", "Attributes": {"Category": "UserAuthentication"}})["Limit"]
  current_value = baseline.get("ProvisionedLimitValue", baseline["FreeLimitValue"])
  control = cognito.update_provisioned_limit(LimitDefinition=..., RequestedLimitValue=current_value)["Limit"]
  assert control["ProvisionedLimitValue"] == current_value, "positive control failed: re-applying the current value ... was not accepted"
so AWS answers 200 with Limit.ProvisionedLimitValue == the requested value; the emulator answers 400 InvalidParameterException.

Message anatomy of the diverging refusal: status 400, code InvalidParameterException, message "Requested value is the same as the current provisioned limit" — the entire sentence is fixed text, no part of it is taken from the request or from state. AWS emits no refusal at all here.

Not reached because of the above (unverified this run, not separate findings): UpdateProvisionedLimit with RequestedLimitValue=2000000000 must answer ServiceQuotaExceededException, and GetProvisionedLimit afterwards must still report the step-1 value.

Smithy (com.amazonaws.cognitoidentityprovider#UpdateProvisionedLimit): input LimitDefinition (required) + RequestedLimitValue (required integer, default 0); output Limit required; errors InvalidParameterException 400, NotAuthorizedException 403, ResourceNotFoundException 404, ServiceQuotaExceededException 400, TooManyRequestsException 429, InternalErrorException 500.

Prior filing of this exact check: 20261118T042515Z-cognito-idp-updateprovisionedlimit-now-refuses-re-applying-the.md (status fixed) — the refusal is back, identical in status, code and message.

## Steps to Reproduce
1. GetProvisionedLimit with LimitDefinition={"LimitClass": "API_CATEGORY", "Attributes": {"Category": "UserAuthentication"}} on an account that has never explicitly provisioned that category; take ProvisionedLimitValue, falling back to FreeLimitValue when absent (both report the category's default rate).
2. UpdateProvisionedLimit with that same LimitDefinition and RequestedLimitValue = the value from step 1 -> expect HTTP 200 with Limit.ProvisionedLimitValue equal to the requested value. The emulator answers HTTP 400 InvalidParameterException "Requested value is the same as the current provisioned limit".
3. UpdateProvisionedLimit with the same LimitDefinition and RequestedLimitValue=2000000000 -> expect ServiceQuotaExceededException.
4. GetProvisionedLimit again -> the provisioned limit must be unchanged from step 1.
5. Re-replay cognito-idp.update-provisioned-limit-out-of-range-value-rejected to confirm. Also re-replay cognito-idp.provisioned-limit-numeric-precision-preserved (open issue 20261119T040922Z) so the conditional gate is confirmed in both directions.
