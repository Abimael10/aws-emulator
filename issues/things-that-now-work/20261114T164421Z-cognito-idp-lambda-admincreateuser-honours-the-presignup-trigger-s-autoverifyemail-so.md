Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp__lambda.pre-sign-up-auto-confirm-applies-to-sign-up-and-is-ignored-for-admin-create-user
Issue-Key: cognito-idp__lambda.pre-sign-up-auto-confirm-applies-to-sign-up-and-is-ignored-for-admin-create-user|run#31|AssertionError|assert invited_attrs.get("email_verified") != "true", (
Issue-Commit: d55a3288bccfe55e7c52387cacb77214d56a079f

# AdminCreateUser honours the PreSignUp trigger's autoVerifyEmail, so an administratively created user is stored with email_verified=true

> _Note: this issue was identified against the code base at commit `d55a3288bccfe55e7c52387cacb77214d56a079f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AdminCreateUser

## Description
1 net-new cause.

Cause 1 of 1: `AdminCreateUser` applies the PreSignUp Lambda trigger's auto-verification response fields to the user it stores. AWS runs the PreSignUp trigger on the administrative-create path (it can still block the create), but it IGNORES the trigger's `response.autoConfirmUser`, `response.autoVerifyEmail` and `response.autoVerifyPhone` there; those fields are honoured only on the self-service `SignUp` path. Failing check: cognito-idp__lambda.pre-sign-up-auto-confirm-applies-to-sign-up-and-is-ignored-for-admin-create-user

AWS: the Smithy model describes the trigger only generically — `LambdaConfigType$PreSignUp` "evaluates new users and can bypass confirmation, link a federated user profile, or block sign-up requests", and `AdminCreateUser.ValidationData` is "temporary user attributes that contribute to the outcomes of your pre sign-up Lambda trigger" — it says nothing about which response fields each entry point honours. The asymmetry is documented in the Amazon Cognito Developer Guide's pre sign-up Lambda trigger page (https://docs.aws.amazon.com/cognito/latest/developerguide/user-pool-lambda-pre-sign-up.html) and is what the recording shows: with one and the same PreSignUp function returning `autoConfirmUser=true` and `autoVerifyEmail=true` for every username, a `SignUp` of a user carrying an `email` attribute comes back `UserConfirmed=true`, `AdminGetUser` reports `UserStatus=CONFIRMED` and the stored `email_verified` is `"true"`; an `AdminCreateUser` of an equivalent user in the SAME pool (which also has `AutoVerifiedAttributes=["email"]`) is left at `UserStatus=FORCE_CHANGE_PASSWORD` and its `email_verified` attribute is NOT `"true"` — it is either absent or `"false"`, because nothing has verified that address. Neither the pool's `AutoVerifiedAttributes` nor the trigger's response verifies an attribute on the administrative-create path; only an explicit `email_verified` value in the request's `UserAttributes`, or a later verification, does. The trigger still runs on that path: a function that throws makes `AdminCreateUser` fail with `UserLambdaValidationException` (400), so "ignored" means its auto-confirm/auto-verify response fields are dropped, not that it is not invoked.

Emulator: `AdminCreateUser` applies the whole PreSignUp decision to the account it stores, so the subsequent `AdminGetUser` of the administratively created user reports `email_verified` = `"true"`. The check fails on `AssertionError: AdminCreateUser must IGNORE the trigger's autoVerifyEmail, but email_verified is 'true'`. The `UserStatus` leg of the same check passes (the stored user is still `FORCE_CHANGE_PASSWORD`), so the divergence is confined to the auto-verified attribute flags, not the confirmation status.

Why: a client that reads `email_verified` as proof an address was proven — gating password recovery, email-based sign-in aliases, notification delivery or any "verified contact" rule on it — gets a user whose address was never verified reported as verified. Against real Cognito the invited user's `email_verified` stays unverified until the address is confirmed, so a flow that works against the emulator silently grants verified-contact privileges in production terms, and a flow that correctly re-verifies is never exercised. It also makes the two creation paths indistinguishable to a test suite that relies on the documented asymmetry.

Fix: the PreSignUp decision application on the administrative-create path — `AdminCreateUser`'s handler, where the trigger's decision is applied to the user before it is stored, and/or the shared pre-sign-up decision type that applies it. The decision already knows which entry point dispatched it (the registration path is passed as `Registration::AdminCreateUser` vs the sign-up path), so the auto-confirm and auto-verify response fields must be honoured only for the self-service sign-up registration and dropped for the administrative create, while a reported trigger failure keeps blocking the create exactly as it does now.

Traces: cognito-idp__lambda.pre-sign-up-auto-confirm-applies-to-sign-up-and-is-ignored-for-admin-create-user — file/line citations for the emulator side: services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice/src/ops/admin_create_user.rs:658 (`decision.apply(&mut user);` in `store`) and :618-628 (where `Registration::AdminCreateUser` is already recorded on the dispatch).

## Evidence
Failing check cognito-idp__lambda.pre-sign-up-auto-confirm-applies-to-sign-up-and-is-ignored-for-admin-create-user, [mismatch]:

  File "check_cognito_idp__lambda_pre_sign_up_auto_confirm_applies_to_sign_up_and_is_ignored_for_admin_create_user.py", line 146, in run
    assert invited_attrs.get("email_verified") != "true", (
  AssertionError: AdminCreateUser must IGNORE the trigger's autoVerifyEmail, but email_verified is 'true'

Expected (AWS): after AdminCreateUser(UserPoolId=<pool>, Username=<invited>, MessageAction="SUPPRESS", TemporaryPassword=<pw>, UserAttributes=[{email: <addr>}]) against a pool whose LambdaConfig.PreSignUp returns response.autoConfirmUser=true and response.autoVerifyEmail=true, AdminGetUser(<invited>) returns UserStatus="FORCE_CHANGE_PASSWORD" and its UserAttributes do NOT contain email_verified="true".

Actual (emulator): AdminGetUser(<invited>) returns UserStatus="FORCE_CHANGE_PASSWORD" (correct) but UserAttributes contains email_verified="true" (wrong) — the trigger's autoVerifyEmail was applied.

Positive control in the same check, same pool, same trigger, which the emulator answers correctly: SignUp(...) returns UserConfirmed=true, AdminGetUser reports UserStatus="CONFIRMED" and email_verified="true". Leg 3 of the check also confirms the trigger IS invoked on the AdminCreateUser path (a throwing function refuses the create), so only the auto-verify/auto-confirm response fields are mis-applied.

No HTTP status or error code diverges here: both calls return 200; the divergence is in the stored and reported attribute value.

## Steps to Reproduce
1. Create a Lambda function whose handler sets event["response"]["autoConfirmUser"]=True and, when the request carries an email attribute, event["response"]["autoVerifyEmail"]=True, and raises for one sentinel username.
2. cognito-idp CreateUserPool with AutoVerifiedAttributes=["email"].
3. lambda AddPermission(Principal="cognito-idp.amazonaws.com", SourceArn=<pool ARN>, Action="lambda:InvokeFunction").
4. cognito-idp UpdateUserPool(UserPoolId=<pool>, LambdaConfig={"PreSignUp": <fn arn>}, AutoVerifiedAttributes=["email"]).
5. CreateUserPoolClient with ExplicitAuthFlows=["ALLOW_USER_PASSWORD_AUTH","ALLOW_REFRESH_TOKEN_AUTH"], GenerateSecret=False.
6. SignUp a user with an email attribute -> observe UserConfirmed=true and AdminGetUser email_verified="true" (both correct).
7. AdminCreateUser a DIFFERENT user in the same pool with MessageAction="SUPPRESS", a known TemporaryPassword and UserAttributes=[{"Name":"email","Value":...}].
8. AdminGetUser that invited user and read its UserAttributes: the emulator reports email_verified="true"; AWS reports no email_verified="true" for it.
