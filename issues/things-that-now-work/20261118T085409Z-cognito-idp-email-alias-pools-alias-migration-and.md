Issue-Source: trace-sweep-divergence
Issue-Trace: cip-alias-force-creation-migrates-a-verified-email
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cip-alias-force-creation-migrates-a-verified-email cognito-idp-email-alias-pool-refuses-email-format-usernames
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp|AdminCreateUser|2xx->4xx|$.User
Issue-Key: cognito-idp|AdminCreateUser|2xx->4xx|status
Issue-Key: cognito-idp|AdminGetUser|2xx->2xx|$.UserAttributes[*].Value
Issue-Key: cognito-idp|AdminGetUser|2xx->2xx|$.Username
Issue-Key: cognito-idp|SignUp|4xx->2xx|$.__type
Issue-Key: cognito-idp|SignUp|4xx->2xx|$.message
Issue-Key: cognito-idp|SignUp|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: cognito-idp|SignUp|4xx->2xx|status
Issue-Key: cognito-idp|AdminCreateUser|4xx->4xx|$.__type
Issue-Key: cognito-idp|AdminCreateUser|4xx->4xx|$.message
Issue-Key: cognito-idp|AdminCreateUser|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: cognito-idp|AdminGetUser|4xx->2xx|$.__type
Issue-Key: cognito-idp|AdminGetUser|4xx->2xx|$.message
Issue-Key: cognito-idp|AdminGetUser|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: cognito-idp|AdminGetUser|4xx->2xx|status
Issue-Related: 20261118T085409Z-cognito-idp-date-members-are-serialized-with-sub.md
Issue-Commit: a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50

# cognito-idp email-alias pools: alias migration and email-format username rules unenforced

> _Note: this issue was identified against the code base at commit `a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AdminCreateUser

## Description
A user pool whose AliasAttributes include email behaves differently from AWS on every leg the corpus drives: the alias is never migrated, the email-format-username rule is never applied, and a user that must not exist is served. 6 causes; each must be closed before submitting.

Cause 1 of 6: AdminCreateUser with ForceAliasCreation=true is refused instead of migrating a verified email alias.
AWS: Smithy: AdminCreateUser.ForceAliasCreation (ForceAliasCreation, default false); modeled errors include UsernameExistsException (400) and InvalidParameterException (400); output User (UserType). AWS documents that when an email value supplied in UserAttributes is already a verified alias of another user, ForceAliasCreation=true moves the alias to the new user and the create SUCCEEDS; only ForceAliasCreation=false refuses. Recorded response: HTTP 200 with User carrying the new Username, Enabled true, UserStatus FORCE_CHANGE_PASSWORD and Attributes email / email_verified=true / sub.
Emulator: HTTP 400 and no User member at all.
Why: the documented alias re-homing path is unusable — a client cannot create the new owner of an email, so any account-migration or email-transfer flow stops at this call.
Fix: the AdminCreateUser handler's alias-conflict branch and the pool's alias index in the cognito-idp crate (services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice) — ForceAliasCreation=true must take the alias instead of refusing. Related open issue 20261117T112621Z-cognito-idp-9-net-new-causes-forcealiascreation-refused.md describes the same behavior from the behavioral corpus but carries no wire replay key for these fixture steps.
Traces: cip-alias-force-creation-migrates-a-verified-email#task#5.

Cause 2 of 6: after a forced alias creation, resolving the alias still returns the previous holder.
AWS: Smithy: AdminGetUser.Username (UsernameType) accepts an alias value in an alias pool; recorded behavior: once the alias has been migrated, AdminGetUser by that email answers with the NEW user's Username and the new user's sub.
Emulator: answers with the first holder's Username and that user's sub (the corpus expected the second holder's Username and the sub captured from the second create).
Why: a client signing in or looking up by email reaches the wrong account — the alias silently still points at the account that lost it, which is an identity-level error, not a cosmetic one.
Fix: the alias index update performed by the AdminCreateUser alias-migration path, and alias resolution in AdminGetUser, in the cognito-idp crate.
Traces: cip-alias-force-creation-migrates-a-verified-email#task#6.

Cause 3 of 6: the previous holder's email_verified is not cleared by the migration.
AWS: recorded behavior: after the alias moves, AdminGetUser on the former holder reports email_verified 'false' (the attribute value is the string 'false'); losing a verified alias unverifies it on the old account.
Emulator: the former holder still reports email_verified 'true'.
Why: two accounts are left claiming the same verified email, so verification state can no longer be trusted as a uniqueness signal and the old account retains a credential-grade claim it should have lost.
Fix: the alias-migration path in the AdminCreateUser handler / user state in the cognito-idp crate — it must reset the losing user's email_verified.
Traces: cip-alias-force-creation-migrates-a-verified-email#task#7.

Cause 4 of 6: SignUp accepts an email-format Username in an email-alias pool.
AWS: Smithy: SignUp models InvalidParameterException (400). Recorded refusal: HTTP 400, __type InvalidParameterException, x-amzn-errortype present, message 'Username cannot be of email format, since user pool is configured for email alias.' — the whole sentence is fixed text with no variable part.
Emulator: HTTP 200 with a normal sign-up body; no refusal at all.
Why: the username namespace and the alias namespace collide — the pool ends up with a user whose username is an email, which is exactly what AWS forbids because it would shadow another user's alias; a client relying on the server-side guard stores an unusable account.
Fix: alias-attribute validation in the SignUp handler in the cognito-idp crate.
Traces: cognito-idp-email-alias-pool-refuses-email-format-usernames#task#3.

Cause 5 of 6: AdminCreateUser reports the wrong error class for an email-format Username.
AWS: recorded refusal: HTTP 400, __type InvalidParameterException, message 'Username cannot be of email format, since user pool is configured for email alias.' (fixed text).
Emulator: HTTP 400 with __type UsernameExistsException, x-amzn-errortype 'UsernameExistsException:' and message 'User account already exists'.
Why: the client is told the name is taken when the real problem is that the name is not allowed in this pool, so retry-with-another-name logic loops on a condition that will never clear.
Fix: the username-format check in the AdminCreateUser handler in the cognito-idp crate — it must precede and not be reported as the existing-user check.
Traces: cognito-idp-email-alias-pool-refuses-email-format-usernames#task#4.

Cause 6 of 6: AdminGetUser returns a user for a username that was never created.
AWS: Smithy: AdminGetUser models UserNotFoundException. Recorded refusal for this step: HTTP 400, __type UserNotFoundException, x-amzn-errortype present, message 'User does not exist.' (fixed text, no variable part).
Emulator: HTTP 200 with the user body.
Why: the earlier refusals the emulator skipped left state AWS would not have, and the lookup then confirms a principal that does not exist on AWS — a client's existence check returns the opposite answer.
Fix: username/alias resolution in the AdminGetUser handler in the cognito-idp crate, together with Causes 4 and 5 (the user must not have been created in the first place).
Traces: cognito-idp-email-alias-pool-refuses-email-format-usernames#task#10.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up):

cip-alias-force-creation-migrates-a-verified-email#task#5 AdminCreateUser, expected 200 got 400:
  status: expected 200, got 400
  $.User: missing (expected {'Attributes': [{'Name':'email',...}, {'Name':'email_verified','Value':'true'}, {'Name':'sub','Value':'<shaped-not-asserted>'}], 'Enabled': True, 'UserStatus': 'FORCE_CHANGE_PASSWORD', 'Username': 'alias-second-holder', ...}, got None)
cip-alias-force-creation-migrates-a-verified-email#task#6 AdminGetUser, 200 -> 200:
  $.Username: value_mismatch (expected 'alias-second-holder', got 'alias-first-holder')
  $.UserAttributes[2].Value: value_mismatch (expected the sub captured from the second create, got '365f35f2-...')
cip-alias-force-creation-migrates-a-verified-email#task#7 AdminGetUser, 200 -> 200:
  $.UserAttributes[1].Value: value_mismatch (expected 'false', got 'true')   [email_verified on the former holder]
cognito-idp-email-alias-pool-refuses-email-format-usernames#task#3 SignUp, expected 400 got 200:
  $.__type: missing (expected 'InvalidParameterException'); $.message: missing (expected 'Username cannot be of email format, since user pool is configured for email alias.'); $headers.x-amzn-errortype: missing
cognito-idp-email-alias-pool-refuses-email-format-usernames#task#4 AdminCreateUser, 400 -> 400:
  $.__type: expected 'InvalidParameterException', got 'UsernameExistsException'
  $.message: expected 'Username cannot be of email format, since user pool is configured for email alias.', got 'User account already exists'
  $headers.x-amzn-errortype: got 'UsernameExistsException:'
cognito-idp-email-alias-pool-refuses-email-format-usernames#task#10 AdminGetUser, expected 400 got 200:
  $.__type: missing (expected 'UserNotFoundException'); $.message: missing (expected 'User does not exist.')

Smithy (com.amazonaws.cognitoidentityprovider): AdminCreateUser input ForceAliasCreation (default false), Username UsernameType @length 1..128 @pattern ^[\p{L}\p{M}\p{S}\p{N}\p{P}]+$, output User UserType; errors include InvalidParameterException (400), UsernameExistsException (400), UserNotFoundException. The recorded statuses above are the requirement where the model's httpError differs.

## Steps to Reproduce
1. Replay fixture cip-alias-force-creation-migrates-a-verified-email. Step task#5: AdminCreateUser for a second user carrying an email that is already another user's verified alias, with ForceAliasCreation=true -> expect 200 with User for the new username; the emulator answers 400 with no User.
2. Step task#6: AdminGetUser by that email -> expect the new username and its sub; the emulator returns the first holder.
3. Step task#7: AdminGetUser on the first holder -> expect email_verified 'false'; the emulator reports 'true'.
4. Replay fixture cognito-idp-email-alias-pool-refuses-email-format-usernames (pool with AliasAttributes including email). Step task#3: SignUp with an email-format Username -> expect 400 InvalidParameterException 'Username cannot be of email format, since user pool is configured for email alias.'; the emulator answers 200.
5. Step task#4: AdminCreateUser with an email-format Username -> expect the same InvalidParameterException; the emulator answers UsernameExistsException 'User account already exists'.
6. Step task#10: AdminGetUser for that username -> expect 400 UserNotFoundException 'User does not exist.'; the emulator answers 200 with the user.
