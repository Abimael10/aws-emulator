Issue-Source: trace-sweep-divergence
Issue-Trace: cognito-idp-email-alias-pool-refuses-email-format-usernames
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cognito-idp-email-alias-pool-refuses-email-format-usernames cip-chal-mfa-setup-required
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp|SignUp|4xx->4xx|$.message
Issue-Key: cognito-idp|RespondToAuthChallenge|4xx->4xx|$.__type
Issue-Key: cognito-idp|RespondToAuthChallenge|4xx->4xx|$.message
Issue-Key: cognito-idp|RespondToAuthChallenge|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: cognito-idp|AdminSetUserMFAPreference|4xx->4xx|$.message
Issue-Commit: a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50

# cognito-idp refusals diverge in error code, message wording and validation order

> _Note: this issue was identified against the code base at commit `a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SignUp

## Description
Three refusals that AWS does make are made by the emulator too, but with the wrong text, the wrong error class, or in the wrong order relative to another check. 3 causes; each must be closed before submitting.

Cause 1 of 3: SignUp's input-constraint report echoes the offending value and orders the violations the other way round.
AWS: Smithy: SignUp.Username targets UsernameType with @length min 1 max 128 and @pattern ^[\p{L}\p{M}\p{S}\p{N}\p{P}]+$; the service reports input-constraint violations as "<n> validation errors detected: " followed by one clause per violation, joined by '; '. Recorded refusal for an empty Username: "2 validation errors detected: Value at 'username' failed to satisfy constraint: Member must have length greater than or equal to 1; Value at 'username' failed to satisfy constraint: Member must satisfy regular expression pattern: [\p{L}\p{M}\p{S}\p{N}\p{P}]+". Two things are requirements here: for an empty value the clause is 'Value at '<member>' ...' with no value echoed, and the length violation is reported BEFORE the pattern violation. The fixed text is the clause template; the variable parts are the member name, the limit and the pattern, all taken from the request and the model.
Emulator: "2 validation errors detected: Value '' at 'username' failed to satisfy constraint: Member must satisfy regular expression pattern: [\p{L}\p{M}\p{S}\p{N}\p{P}]+; Value '' at 'username' failed to satisfy constraint: Member must have length greater than or equal to 1" — it inserts "''" for the empty value and emits the pattern clause first.
Why: the message is the only machine-readable detail in this refusal class; a client that parses or matches it (or diffs it in a test) does not recognise AWS's form, and the ordering difference makes the first reported violation the wrong one.
Fix: the input-constraint reporter shared by the cognito-idp operation handlers (services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice) — omit the value segment when the value is empty, and emit length violations before pattern violations.
Traces: cognito-idp-email-alias-pool-refuses-email-format-usernames#task#5.

Cause 2 of 3: RespondToAuthChallenge checks MFA registration before it checks for a missing required challenge parameter.
AWS: Smithy: RespondToAuthChallenge models InvalidParameterException (400) and SoftwareTokenMFANotFoundException. Recorded refusal when the challenge response omits USERNAME: HTTP 400, __type 'InvalidParameterException', message 'Missing required parameter USERNAME' — fixed text plus the missing parameter's name. Parameter presence is validated before any user- or MFA-state lookup, which is why the MFA error is never reached.
Emulator: HTTP 400 with __type 'SoftwareTokenMFANotFoundException', x-amzn-errortype 'SoftwareTokenMFANotFoundException:' and message 'Software token MFA is not registered for the user.'
Why: the client is told to enrol a TOTP factor when the actual fault is a malformed request, so the caller fixes the wrong thing; the leaked error class also reports MFA state for a request that never identified a user.
Fix: ordering in the RespondToAuthChallenge handler's validation in the cognito-idp crate — required ChallengeResponses members must be validated before the MFA/software-token lookup.
Traces: cip-chal-mfa-setup-required#task#8 (cluster also covers #task#9).

Cause 3 of 3: AdminSetUserMFAPreference's not-enabled refusal is emulator-worded.
AWS: Smithy: AdminSetUserMFAPreference models InvalidParameterException (400). Recorded refusal: HTTP 400 with the message 'User does not have delivery config set to turn on SOFTWARE_TOKEN_MFA' — fixed text whose only variable part is the MFA type named in the request (SOFTWARE_TOKEN_MFA here).
Emulator: same status, message 'User has not verified software token mfa'.
Why: the wording is what clients and operators match on, and the emulator's sentence describes a different condition (verification) from AWS's (delivery configuration), so the caller is pointed at the wrong remedy.
Fix: the refusal message emitted by the AdminSetUserMFAPreference handler in the cognito-idp crate. Related open issue 20261115T175613Z-cognito-idp-4-net-new-causes-a-spent.md reports an emulator-worded MFA-preference refusal from the behavioral corpus; it carries no wire replay key for this fixture step.
Traces: cip-chal-mfa-setup-required#task#6.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up):

cognito-idp-email-alias-pool-refuses-email-format-usernames#task#5 SignUp, 400 -> 400:
  $.message: expected "2 validation errors detected: Value at 'username' failed to satisfy constraint: Member must have length greater than or equal to 1; Value at 'username' failed to satisfy constraint: Member must satisfy regular expression pattern: [\p{L}\p{M}\p{S}\p{N}\p{P}]+"
            got      "2 validation errors detected: Value '' at 'username' failed to satisfy constraint: Member must satisfy regular expression pattern: [\p{L}\p{M}\p{S}\p{N}\p{P}]+; Value '' at 'username' failed to satisfy constraint: Member must have length greater than or equal to 1"
cip-chal-mfa-setup-required#task#8 RespondToAuthChallenge, 400 -> 400 (cluster also #task#9):
  $.__type: expected 'InvalidParameterException', got 'SoftwareTokenMFANotFoundException'
  $.message: expected 'Missing required parameter USERNAME', got 'Software token MFA is not registered for the user.'
  $headers.x-amzn-errortype: got 'SoftwareTokenMFANotFoundException:'
cip-chal-mfa-setup-required#task#6 AdminSetUserMFAPreference, 400 -> 400:
  $.message: expected 'User does not have delivery config set to turn on SOFTWARE_TOKEN_MFA', got 'User has not verified software token mfa'

Smithy: SignUp.Username -> UsernameType @length 1..128 @pattern ^[\p{L}\p{M}\p{S}\p{N}\p{P}]+$; RespondToAuthChallenge errors include InvalidParameterException (400) and SoftwareTokenMFANotFoundException; AdminSetUserMFAPreference errors include InvalidParameterException (400).

## Steps to Reproduce
1. Replay fixture cognito-idp-email-alias-pool-refuses-email-format-usernames, step task#5: SignUp with Username='' -> compare the message with AWS's two-clause form (no value echoed, length clause first).
2. Replay fixture cip-chal-mfa-setup-required, step task#8 (and task#9): RespondToAuthChallenge for the MFA challenge with no USERNAME in ChallengeResponses -> expect 400 InvalidParameterException 'Missing required parameter USERNAME'; the emulator answers SoftwareTokenMFANotFoundException.
3. Step task#6: AdminSetUserMFAPreference enabling SOFTWARE_TOKEN_MFA for a user without the delivery configuration -> expect 'User does not have delivery config set to turn on SOFTWARE_TOKEN_MFA'.
