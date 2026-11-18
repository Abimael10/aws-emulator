Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.sign-up-custom-number-bounds-rejected cognito-idp.sign-up-custom-string-length-bounds-rejected cognito-idp.invalid-lambda-response-error
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp.sign-up-custom-number-bounds-rejected|run#9|AssertionError|assert (err["Code"], err["Message"]) == ("InvalidParameterException", message), (
Issue-Key: cognito-idp.sign-up-custom-string-length-bounds-rejected|run#8|AssertionError|assert (err["Code"], err["Message"]) == ("InvalidParameterException", message), (
Issue-Key: cognito-idp.invalid-lambda-response-error|_error#41|AssertionError|raise AssertionError("real AWS rejects this call, but it SUCCEEDED")
Issue-Commit: 0ec2fb94436bf840a0bf63ae99f3da1459d90f3e

# cognito-idp: 2 net-new causes — SignUp's schema-constraint refusal message is emulator-worded, and a malformed PostConfirmation trigger response does not fail AdminConfirmSignUp

> _Note: this issue was identified against the code base at commit `0ec2fb94436bf840a0bf63ae99f3da1459d90f3e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SignUp

## Description
Two net-new root causes in the cognito-idp user-pool data plane. Both were found by replaying the behavioral suite against the emulator; neither is covered by an open issue (the nearest, 20261023T114649Z-cognito-idp-cognito-user-pool-lambda-triggers-are-not-invoked.md, is about triggers never firing at all, which is not what happens here: the PreSignUp leg of the same check passes, so triggers do fire).

Cause 1 of 2 — SignUp refuses a custom attribute that violates its schema constraint with the emulator's own sentence instead of AWS's schema-validation message.
AWS: Smithy SignUp declares InvalidParameterException (httpError 400) and the custom-attribute constraints live in the pool Schema (NumberAttributeConstraints.MinValue/MaxValue, StringAttributeConstraints.MinLength/MaxLength). Recorded behavior: the refusal is InvalidParameterException whose message is the fixed text "Attributes did not conform to the schema: Custom.User.<attribute>: This attribute does not satisfy the <VALIDATION> validation defined in the schema." — fixed parts are "Attributes did not conform to the schema: ", the "Custom.User." path prefix, ": This attribute does not satisfy the " and " validation defined in the schema."; the variable parts are the custom attribute's bare name taken from the request (the name WITHOUT the "custom:" prefix) and the name of the violated bound, one of MAX_VALUE_VALIDATION, MIN_VALUE_VALIDATION, MAX_LENGTH_VALIDATION, MIN_LENGTH_VALIDATION. A value that is not a number at all for a Number attribute is refused with the same prefix followed by AWS's constraint envelope: "Attributes did not conform to the schema: 1 validation error detected: Value at 'Custom.User.<attribute>' failed to satisfy constraint: Member must satisfy regular expression pattern: $|^[+-]?((\p{Nd}+(\.\p{Nd}*)?)|(\.\p{Nd}+))([eE][+-]?\p{Nd}+)?". No message carries a trailing newline, and the attribute is always named by its schema path, never by the wire name the request used.
Emulator: the error code and 400 status are right, but the message is the emulator's own one-liner naming the attribute as "user.custom:<attribute>" and ending in a newline — for a Number attribute above MaxValue: 'user.custom:score: Number must be less than 10\n'; for a String attribute above MaxLength: 'user.custom:tier: String must be no longer than 5 characters\n'. The "Attributes did not conform to the schema: " prefix, the Custom.User.<name> path and the <BOUND>_VALIDATION token are all absent.
Why: a client that reads the message to learn WHICH schema validation failed (the only place that information is carried — the error code is the same for every attribute defect) gets a sentence it cannot parse, and a client matching the documented wording treats a correct refusal as an unknown failure. The emulator's path spelling also leaks the request's wire name instead of the pool's schema path.
Fix: the schema-constraint validation messages produced by the SignUp handler's attribute validation (the same validator serves AdminCreateUser / UpdateUserAttributes, so the wording belongs in that shared component, not at the call site).
Traces: cognito-idp.sign-up-custom-number-bounds-rejected, cognito-idp.sign-up-custom-string-length-bounds-rejected. Both checks fail on the FIRST refusal they assert, so their later legs (the refusal leaves no user behind; values exactly at MinValue/MaxValue and MinLength/MaxLength sign up and are stored verbatim) were never reached and are not claimed here — re-replay both to confirm.

Cause 2 of 2 — a PostConfirmation trigger whose response is malformed does not fail AdminConfirmSignUp.
AWS: Smithy AdminConfirmSignUp declares InvalidLambdaResponseException (httpError 400). Recorded behavior: with LambdaConfig.PostConfirmation pointing at a function that returns a body Cognito cannot read, AdminConfirmSignUp is refused with InvalidLambdaResponseException and the message "Invalid user name. User name in Lambda mismatches the actual username." (fixed text, no variable parts), and because PostConfirmation runs after confirmation commits the user is left UserStatus CONFIRMED — the error does not roll the confirmation back. Clearing LambdaConfig makes the identical call succeed again.
Emulator: the call SUCCEEDS — no error is raised at all (replay: "real AWS rejects this call, but it SUCCEEDED"). The same check's PreSignUp leg does produce InvalidLambdaResponseException "Unrecognizable lambda output" for AdminCreateUser, so trigger invocation itself works; what is missing is the PostConfirmation path either invoking the trigger or validating what it returned.
Why: a pool whose PostConfirmation trigger is broken looks healthy on the emulator — the confirming call returns 200 — so code that handles InvalidLambdaResponseException (alerting, retry, surfacing the failure to the end user) is never exercised and a deployment defect that AWS reports loudly is silently swallowed.
Fix: the PostConfirmation trigger invocation and response validation in the AdminConfirmSignUp handler (and the ConfirmSignUp path that shares it).
Traces: cognito-idp.invalid-lambda-response-error. The check stops at this leg, so its later PreAuthentication (AdminInitiateAuth), CustomMessage (AdminResetUserPassword) and PreTokenGeneration (GetTokensFromRefreshToken) legs are unverified rather than passing.

## Evidence
Verbatim from the replay (service heading: cognito-idp):

[mismatch] cognito-idp.sign-up-custom-number-bounds-rejected
  AssertionError: SignUp with custom:score '11' answered {'Message': 'user.custom:score: Number must be less than 10\n', 'Code': 'InvalidParameterException'}
  expected: ('InvalidParameterException', "Attributes did not conform to the schema: Custom.User.score: This attribute does not satisfy the MAX_VALUE_VALIDATION validation defined in the schema.")

[mismatch] cognito-idp.sign-up-custom-string-length-bounds-rejected
  AssertionError: SignUp with custom:tier of length 6 answered {'Message': 'user.custom:tier: String must be no longer than 5 characters\n', 'Code': 'InvalidParameterException'}
  expected: ('InvalidParameterException', "Attributes did not conform to the schema: Custom.User.tier: This attribute does not satisfy the MAX_LENGTH_VALIDATION validation defined in the schema.")

[mismatch] cognito-idp.invalid-lambda-response-error
  check_cognito_idp_invalid_lambda_response_error.py line 347: code2, message2 = _error(lambda: c.admin_confirm_sign_up(UserPoolId=pool_id, Username=confirm_username))
  AssertionError: real AWS rejects this call, but it SUCCEEDED
  expected: InvalidLambdaResponseException / "Invalid user name. User name in Lambda mismatches the actual username.", with the user left UserStatus CONFIRMED

Smithy (com.amazonaws.cognitoidentityprovider): SignUp errors include InvalidParameterException (400) and InvalidLambdaResponseException (400); AdminConfirmSignUp errors include InvalidLambdaResponseException (400).

## Steps to Reproduce
Cause 1:
1. CreateUserPool with Schema=[{Name: score, AttributeDataType: Number, Mutable: true, NumberAttributeConstraints: {MinValue: "1", MaxValue: "10"}}] and a second pool with Schema=[{Name: tier, AttributeDataType: String, Mutable: true, StringAttributeConstraints: {MinLength: "2", MaxLength: "5"}}]; CreateUserPoolClient on each.
2. SignUp with UserAttributes=[{Name: "custom:score", Value: "11"}] (then "0", then "abc") and with [{Name: "custom:tier", Value: "abcdef"}] (then "a").
3. Compare each InvalidParameterException message with AWS's "Attributes did not conform to the schema: Custom.User.<attr>: This attribute does not satisfy the <BOUND>_VALIDATION validation defined in the schema." (and the regex-constraint envelope for the non-numeric value).
4. Then confirm the refused SignUp left no user (AdminGetUser -> UserNotFoundException "User does not exist.") and that values exactly at each bound sign up and are stored as sent.

Cause 2:
1. Create a Lambda function that returns a body Cognito cannot parse, and grant cognito-idp.amazonaws.com invoke on it for the pool ARN.
2. SignUp a user, leaving it UNCONFIRMED.
3. UpdateUserPool with LambdaConfig={"PostConfirmation": <fn arn>}.
4. AdminConfirmSignUp for that user -> expect InvalidLambdaResponseException "Invalid user name. User name in Lambda mismatches the actual username.", not a 200; then AdminGetUser -> UserStatus CONFIRMED.
5. UpdateUserPool with LambdaConfig={} and AdminConfirmSignUp a fresh unconfirmed user -> succeeds.
