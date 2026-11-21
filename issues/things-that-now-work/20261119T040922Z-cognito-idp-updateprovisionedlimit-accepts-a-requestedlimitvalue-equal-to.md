Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.provisioned-limit-numeric-precision-preserved
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp.provisioned-limit-numeric-precision-preserved|run#8|AssertionError|raise AssertionError(
Issue-Commit: 91a8ed8ebce8c6b25be2c9f1b5253611a954e20f

# cognito-idp UpdateProvisionedLimit accepts a RequestedLimitValue equal to the already-provisioned value instead of refusing it as a no-op

> _Note: this issue was identified against the code base at commit `91a8ed8ebce8c6b25be2c9f1b5253611a954e20f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateProvisionedLimit

## Description
One net-new root cause on the account-level provisioned-limit plane.

Cause 1 of 1: UpdateProvisionedLimit has no same-value (no-op) guard.
AWS: Smithy: UpdateProvisionedLimit {LimitDefinition (required), RequestedLimitValue (required, integer)} -> Limit (required); modeled client errors include InvalidParameterException (400) and ServiceQuotaExceededException (400). When RequestedLimitValue equals the value already reported as Limit.ProvisionedLimitValue for that LimitDefinition, AWS refuses the request with InvalidParameterException, HTTP 400, and the message "Requested value is the same as the current provisioned limit" — that text is entirely fixed, it carries no value from the request or from state — and the persisted limit is unchanged. (Error class and wording are from the recorded behavior of this operation; the error shape and status are modeled.)
Emulator: the identical call is answered HTTP 200 and returns a Limit body, i.e. the no-op request is accepted. The replay records: "UpdateProvisionedLimit accepted RequestedLimitValue=50 even though it already equals the current ProvisionedLimitValue" (the 50 is this run's FreeLimitValue/ProvisionedLimitValue for LimitClass=API_CATEGORY, Attributes={Category: UserCreation} — the requirement is the equality, not the number).
Why: a client that uses the InvalidParameterException refusal to detect "nothing to change" (idempotency / capacity-reconciliation loops that must not count a no-op as an applied change) sees a success where AWS reports a refusal, and cannot tell an applied capacity change from one AWS would have rejected.
Fix: the UpdateProvisionedLimit handler and the account-level provisioned-limit state in the cognito-idp crate (compare the requested value against the currently provisioned value before applying, and refuse with the modeled InvalidParameterException). Open issue 20261117T112621Z-cognito-idp-8-net-new-causes-cors-preflight.md (Cause 7) describes the broader UpdateProvisionedLimit laxity but does not carry this check's replay key, so this check is not covered by it.
Traces: cognito-idp.provisioned-limit-numeric-precision-preserved.

## Evidence
Expected: UpdateProvisionedLimit(LimitDefinition={LimitClass: API_CATEGORY, Attributes: {Category: UserCreation}}, RequestedLimitValue=<current ProvisionedLimitValue>) -> HTTP 400, InvalidParameterException, Error.Message exactly "Requested value is the same as the current provisioned limit"; GetProvisionedLimit still reports the same ProvisionedLimitValue.
Actual: HTTP 200 with a Limit body; no exception raised. Replay assertion (check_cognito_idp_provisioned_limit_numeric_precision_preserved.py line 42): "AssertionError: UpdateProvisionedLimit accepted RequestedLimitValue=50 even though it already equals the current ProvisionedLimitValue -- real AWS rejects this as a no-op".

## Steps to Reproduce
1. GetProvisionedLimit(LimitDefinition={"LimitClass": "API_CATEGORY", "Attributes": {"Category": "UserCreation"}}) and read FreeLimitValue / ProvisionedLimitValue.
2. Call UpdateProvisionedLimit with the same LimitDefinition and RequestedLimitValue set to the value GetProvisionedLimit just reported as ProvisionedLimitValue.
3. Observe HTTP 200 instead of the 400 InvalidParameterException "Requested value is the same as the current provisioned limit".
Re-replay cognito-idp.provisioned-limit-numeric-precision-preserved to confirm.
