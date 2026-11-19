Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.update-provisioned-limit-out-of-range-value-rejected
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-identity-provider|UpdateProvisionedLimit|InvalidParameterException
Issue-Commit: 0ec2fb94436bf840a0bf63ae99f3da1459d90f3e

# cognito-idp: UpdateProvisionedLimit now refuses re-applying the account's current limit value with InvalidParameterException, so the limit cannot be written at all

> _Note: this issue was identified against the code base at commit `0ec2fb94436bf840a0bf63ae99f3da1459d90f3e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateProvisionedLimit

## Description
REOPENS 20261029T182116Z-cognito-idp-provisioned-limit-api-diverges-no-default.md: its fix left the call refused.

That issue's Cause 3 made UpdateProvisionedLimit reject a request whose RequestedLimitValue equals the current value. The emulator now refuses such a request, and the refusal fires on a request AWS accepts, so the operation can no longer be driven at all from its own reported state.

AWS: Smithy UpdateProvisionedLimit takes LimitDefinition (required) and RequestedLimitValue (required integer) and returns Limit (required); its modeled errors are InvalidParameterException (400), NotAuthorizedException (403), ResourceNotFoundException (404), ServiceQuotaExceededException (400), TooManyRequestsException (429), InternalErrorException (500). Recorded behavior: reading the current limit with GetProvisionedLimit for LimitClass API_CATEGORY / Category UserAuthentication and then calling UpdateProvisionedLimit with RequestedLimitValue set to exactly that value SUCCEEDS and returns Limit.ProvisionedLimitValue equal to the requested value — where the account has no explicitly provisioned value yet, "the current value" is the Limit's FreeLimitValue, and provisioning that same number is an ordinary accepted write, not a rejected no-op. The refusal AWS does make on this operation is ServiceQuotaExceededException, for a RequestedLimitValue beyond the account's Service Quotas ceiling, and it leaves the provisioned limit unchanged.
Emulator: the in-range re-apply is refused with HTTP 400 InvalidParameterException and the message "Requested value is the same as the current provisioned limit" (emulator-authored: the whole sentence is fixed text and names no request value). The call never reaches the out-of-range leg, so the ServiceQuotaExceededException path for an absurd RequestedLimitValue (and the assertion that the stored limit is unchanged afterwards) is left unverified.
Why: a client reads the current limit and writes it back — the normal idempotent apply that configuration tooling, retries and reconciliation loops perform — and gets a hard 400 instead of the limit. Combined with the FreeLimitValue fallback, there is no RequestedLimitValue derived from the service's own GetProvisionedLimit answer that the emulator will accept for an already-provisioned category, so the operation is unusable and an over-quota request can never be distinguished from this one.
Fix: the no-op/same-value gate in the UpdateProvisionedLimit handler — it must not refuse a request whose value is in range (and in particular must treat provisioning a value equal to FreeLimitValue, where no explicit ProvisionedLimitValue exists, as a normal write); the out-of-range case belongs to the ServiceQuotaExceededException check.
Traces: cognito-idp.update-provisioned-limit-out-of-range-value-rejected.

## Evidence
Verbatim from the replay (service heading: cognito-idp):

[error] cognito-idp.update-provisioned-limit-out-of-range-value-rejected
  botocore.errorfactory.InvalidParameterException: An error occurred (InvalidParameterException) when calling the UpdateProvisionedLimit operation: Requested value is the same as the current provisioned limit

The failing call is the check's positive control, which re-applies the value GetProvisionedLimit just reported:
  baseline = get_provisioned_limit(LimitDefinition={LimitClass: API_CATEGORY, Attributes: {Category: UserAuthentication}})["Limit"]
  current_value = baseline.get("ProvisionedLimitValue", baseline["FreeLimitValue"])
  control = update_provisioned_limit(LimitDefinition=..., RequestedLimitValue=current_value)["Limit"]
  assert control["ProvisionedLimitValue"] == current_value
so AWS accepts it and returns the requested value; the emulator raises 400 InvalidParameterException.

Smithy (com.amazonaws.cognitoidentityprovider#UpdateProvisionedLimit): errors InvalidParameterException 400, NotAuthorizedException 403, ResourceNotFoundException 404, ServiceQuotaExceededException 400, TooManyRequestsException 429, InternalErrorException 500; output Limit is required.

## Steps to Reproduce
1. GetProvisionedLimit with LimitDefinition={"LimitClass": "API_CATEGORY", "Attributes": {"Category": "UserAuthentication"}} and take ProvisionedLimitValue, falling back to FreeLimitValue when it is absent.
2. UpdateProvisionedLimit with that same LimitDefinition and RequestedLimitValue = the value from step 1 -> expect success with Limit.ProvisionedLimitValue equal to the requested value; the emulator answers 400 InvalidParameterException "Requested value is the same as the current provisioned limit".
3. UpdateProvisionedLimit with RequestedLimitValue=2000000000 -> expect ServiceQuotaExceededException.
4. GetProvisionedLimit again -> the provisioned limit must be unchanged from step 1.
