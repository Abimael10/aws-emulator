Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.invalid-lambda-response-error
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp.invalid-lambda-response-error|run#22|AssertionError|assert code1 == "InvalidLambdaResponseException", (
Issue-Commit: b4d40a65307383d4ed870c0d75e08e4b4e336f55

# cognito-idp: a user-pool Lambda trigger that returns a malformed response is refused UnexpectedLambdaException instead of InvalidLambdaResponseException — the trigger dispatch models no invalid-response failure class at all

> _Note: this issue was identified against the code base at commit `b4d40a65307383d4ed870c0d75e08e4b4e336f55`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AdminCreateUser

## Description
Cause 1 of 1 — the user-pool Lambda-trigger dispatch has no "the function answered, but the answer is not usable" failure class, so a malformed trigger response is reported as an unexpected Lambda failure (or silently ignored) rather than as an invalid Lambda response.

AWS: Amazon Cognito validates the response envelope a user-pool trigger function returns, and a function whose answer it cannot use fails the TRIGGERING operation with `InvalidLambdaResponseException` — "This exception is thrown when Amazon Cognito encounters an invalid Lambda response" (Smithy: `com.amazonaws.cognitoidentityprovider#InvalidLambdaResponseException`, errorKind client, httpError 400). It is modeled on every operation this check exercises, DISTINCT from `UnexpectedLambdaException` ("an unexpected exception with Lambda") and from `UserLambdaValidationException`, all three of which are modeled side by side on `AdminCreateUser`, `AdminConfirmSignUp`, `AdminInitiateAuth`, `AdminResetUserPassword` and `GetTokensFromRefreshToken`. The two message forms the recording establishes are fixed text with NO variable parts: a corrupted RESPONSE member (`response.autoConfirmUser` set to a non-boolean on a `PreSignUp_*` event, `response.claimsOverrideDetails` set to a non-object on a `TokenGeneration_*` event) yields exactly `Unrecognizable lambda output`; a corrupted ENVELOPE identity (`event.userName` returned as a number rather than the username Cognito delivered) yields exactly `Invalid user name. User name in Lambda mismatches the actual username.` The status is HTTP 400 in both cases. Also recorded behavior: the classification does not change the stage's commit semantics — a gating trigger (PreSignUp) leaves nothing stored, so a follow-up `AdminGetUser` must answer 404 `UserNotFoundException` with message `User does not exist.`, while a post-commit trigger (PostConfirmation) raises AFTER confirmation commits, so the user is still `CONFIRMED`; and clearing `LambdaConfig` must make the identical call succeed again.

Emulator: with a `PreSignUp` trigger configured whose function returns the delivered event with `response.autoConfirmUser` set to the string `not-a-boolean`, `AdminCreateUser` is refused with error code `UnexpectedLambdaException` instead of `InvalidLambdaResponseException`. The dispatch in `crate::state::lambda_triggers` enumerates only two failure classes — `Failure::Reported(..)` (the function ran and reported an error → `UserLambdaValidationException`) and `Failure::NotInvokable(..)` (the function could not be invoked, or missed the trigger budget → `UnexpectedLambdaException`). There is no third class for a function that RETURNED an unusable document, and no validation step that could produce one. Correspondingly, `pre_sign_up::Decision::read` is written to treat a member "stated as anything other than a boolean" as "asks for nothing" and a response half that is not an object as deciding nothing, i.e. a malformed answer is defined as acceptable rather than as an invalid response. The identical absence applies to the `PostConfirmation`, `PreAuthentication`, `CustomMessage` and `PreTokenGeneration` legs of the same check, none of which the replay reached because it stopped at the first leg.

Why: a client cannot tell a broken trigger from a broken Lambda control plane. `UnexpectedLambdaException` reads as a transient/infrastructure fault that SDK and application retry logic will re-attempt, whereas `InvalidLambdaResponseException` is a deterministic contract violation in the customer's own trigger code that must be surfaced to the developer and never retried. Code that branches on the error code therefore retries a permanently failing sign-up, sign-in, confirmation, password reset or token refresh instead of reporting the trigger bug; conversely, where the emulator accepts the malformed answer outright (the `Decision::read` tolerance above), a trigger that AWS would reject appears to succeed and the pool silently stores or mints the wrong outcome, which no test against the emulator can catch.

Fix: the user-pool Lambda-trigger dispatch and its failure classification — `services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice/src/state/lambda_triggers.rs` (the `Failure` enum and `invoke_gating`/`invoke`, which must gain an invalid-response class carried into each operation's error channel) plus the per-trigger answer readers that must validate rather than tolerate (`.../src/state/lambda_triggers/pre_sign_up.rs` `Decision::read`, and the `post_confirmation`, `pre_authentication`, `custom_email` and `pre_token_generation`/`token_overrides` readers in the same module tree). Both the response-member check (non-boolean `autoConfirmUser`, non-object `claimsOverrideDetails` → `Unrecognizable lambda output`) and the envelope-identity check (returned `userName` not equal to the delivered one → `Invalid user name. User name in Lambda mismatches the actual username.`) belong there, so every operation that fires a trigger reports the same 400 `InvalidLambdaResponseException`.

Traces: cognito-idp.invalid-lambda-response-error — assertion `AdminCreateUser (PreSignUp): expected InvalidLambdaResponseException, got UnexpectedLambdaException` at check_cognito_idp_invalid_lambda_response_error.py:294. No existing open issue covers this: a search of filed issues for "InvalidLambdaResponse" returns nothing, and the nearest neighbours (20261114T164421Z-cognito-idp-lambda-admincreateuser-honours-the-presignup-trigger-s-autoverifyemail-so.md, 20261023T114649Z-cognito-idp-cognito-user-pool-lambda-triggers-are-not-invoked.md) are about which trigger effects are APPLIED, not about how an unusable trigger answer is classified.

## Evidence
Check: cognito-idp.invalid-lambda-response-error (verdict: mismatch).

Setup the check performs: a pool with a lenient password policy and an SNS caller ARN; a python3.12 Lambda whose handler corrupts exactly the field Cognito validates for the invoking trigger —
  if triggerSource startswith 'PreSignUp':        response['autoConfirmUser']       = 'not-a-boolean'
  elif triggerSource startswith 'TokenGeneration': response['claimsOverrideDetails'] = 'not-an-object'
  else:                                            event['userName']                 = 12345
and returns the event. `lambda:InvokeFunction` is granted to cognito-idp.amazonaws.com for the pool ARN.

Positive control (passes): with `LambdaConfig={}`, AdminCreateUser on a fresh username succeeds — so the emulator is not simply rejecting every AdminCreateUser that has a PreSignUp trigger configured.

Divergence, leg 1 (AdminCreateUser / PreSignUp):
  UpdateUserPool(LambdaConfig={"PreSignUp": <fn_arn>}) then AdminCreateUser(UserPoolId, Username, MessageAction=SUPPRESS, UserAttributes=[email]).
  Expected (AWS): HTTP 400, Error.Code = "InvalidLambdaResponseException", Error.Message = "Unrecognizable lambda output" (fixed text, no variable parts).
  Actual (emulator): Error.Code = "UnexpectedLambdaException".
  AssertionError: AdminCreateUser (PreSignUp): expected InvalidLambdaResponseException, got UnexpectedLambdaException

The check stopped at that assertion, so the four remaining legs were never reached. They assert the same code with these messages, all fixed text:
  leg 2 AdminConfirmSignUp / PostConfirmation  -> InvalidLambdaResponseException, "Invalid user name. User name in Lambda mismatches the actual username."; user must still read back UserStatus == "CONFIRMED" (post-commit trigger does not roll back).
  leg 3 AdminInitiateAuth / PreAuthentication  -> InvalidLambdaResponseException, "Invalid user name. User name in Lambda mismatches the actual username."
  leg 4 AdminResetUserPassword / CustomMessage -> InvalidLambdaResponseException, "Invalid user name. User name in Lambda mismatches the actual username."; user must still read back UserStatus == "CONFIRMED".
  leg 5 GetTokensFromRefreshToken / PreTokenGeneration -> InvalidLambdaResponseException, "Unrecognizable lambda output".
Each leg is bracketed by its own no-trigger positive control and, after the refusal, by a restore leg that clears LambdaConfig and asserts the identical call succeeds.

Leg 1 also asserts the effect is absent: AdminGetUser(UserPoolId, Username) must be refused HTTP 404 "UserNotFoundException" with message "User does not exist." (fixed text).

Model evidence (smithy_query on services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice/smithy-model/com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice.json):
  InvalidLambdaResponseException: errorKind client, httpError 400, member message:MessageType.
  UnexpectedLambdaException:      errorKind client, httpError 400, member message:MessageType.
  Both are modeled errors of AdminCreateUser, AdminConfirmSignUp, AdminInitiateAuth, AdminResetUserPassword and GetTokensFromRefreshToken — they are not interchangeable.

Emulator-source evidence (services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice/src/state/lambda_triggers.rs, lines 521-532):
  pub enum Failure { Reported(String), NotInvokable(String) }
  — with doc comments mapping Reported -> UserLambdaValidationException and NotInvokable -> UnexpectedLambdaException. No invalid-response variant exists, so InvalidLambdaResponseException has no producer anywhere in the trigger path.
And src/state/lambda_triggers/pre_sign_up.rs lines 208-251 plus its own unit tests ("a function that stated a member as text" / "as a number" / "an event whose response half is not an object" all expected to decide nothing) encode the malformed answer as ACCEPTABLE rather than as an invalid response.

## Steps to Reproduce
1. Create a user pool (lenient password policy, AutoVerifiedAttributes=["email"], SmsConfiguration with a valid SnsCallerArn).
2. Create a Lambda (python3.12) whose handler returns the event it was given after corrupting the field Cognito validates for the invoking trigger:
     def handler(event, context):
         ts = event.get('triggerSource', '')
         resp = event.get('response') or {}
         if ts.startswith('PreSignUp'):
             resp['autoConfirmUser'] = 'not-a-boolean'; event['response'] = resp
         elif ts.startswith('TokenGeneration'):
             resp['claimsOverrideDetails'] = 'not-an-object'; event['response'] = resp
         else:
             event['userName'] = 12345
         return event
3. lambda:AddPermission for Principal cognito-idp.amazonaws.com, SourceArn = the pool ARN.
4. Positive control: with LambdaConfig={}, call AdminCreateUser on a fresh username — it succeeds.
5. UpdateUserPool(UserPoolId, LambdaConfig={"PreSignUp": <function ARN>}).
6. Call AdminCreateUser(UserPoolId, Username=<fresh>, MessageAction="SUPPRESS", UserAttributes=[{"Name":"email","Value":"<fresh>@example.com"}]).
   Observed: HTTP 400 with Error.Code "UnexpectedLambdaException".
   Expected: HTTP 400 with Error.Code "InvalidLambdaResponseException" and Error.Message exactly "Unrecognizable lambda output".
7. Call AdminGetUser(UserPoolId, Username=<the same>) — expect HTTP 404 "UserNotFoundException", message exactly "User does not exist."
8. UpdateUserPool(LambdaConfig={}) and repeat step 6 — it must succeed.
9. Repeat the same break/restore shape for the other four stages, each of which must also answer 400 InvalidLambdaResponseException:
   - AdminConfirmSignUp with LambdaConfig={"PostConfirmation": arn} -> message "Invalid user name. User name in Lambda mismatches the actual username."; the user is nevertheless CONFIRMED afterwards.
   - AdminInitiateAuth (ADMIN_USER_PASSWORD_AUTH) with {"PreAuthentication": arn} -> same message.
   - AdminResetUserPassword with {"CustomMessage": arn} (recovery via verified_phone_number) -> same message; the user stays CONFIRMED.
   - GetTokensFromRefreshToken with {"PreTokenGeneration": arn} -> message "Unrecognizable lambda output".
