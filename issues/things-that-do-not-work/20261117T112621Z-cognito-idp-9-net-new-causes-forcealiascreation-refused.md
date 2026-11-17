Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.admin-create-user-force-alias-creation-alias-sign-in-authenticates-new-owner cognito-idp.admin-create-user-force-alias-creation-moves-verified-email-alias cognito-idp.admin-create-user-force-alias-creation-does-not-bypass-phone-number-format-validation cognito-idp.sign-up-custom-number-bounds-rejected cognito-idp.sign-up-custom-string-length-bounds-rejected cognito-idp.update-user-attributes-custom-attribute-bounds-rejected cognito-idp.admin-update-custom-attribute-lower-bounds-rejected cognito-idp.admin-update-preferred-username-taken-alias-rejected cognito-idp.update-user-attributes-preferred-username-taken-alias-rejected cognito-idp.sign-up-preferred-username-alias-rejected-until-account-exists cognito-idp.groups-per-user-quota-rejected cognito-idp.list-users-attributes-to-get-requires-a-value-on-every-user cognito-idp.tag-resource-rejects-aws-prefixed-tag-key cognito-idp.tag-untag-resource-reject-aws-reserved-key-prefix cognito-idp.list-tags-for-resource-malformed-arn-rejected cognito-idp.sub-uuid-identical-in-admin-get-user-and-list-users-refused-as-username
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-identity-provider|AdminCreateUser|UsernameExistsException
Issue-Key: cognito-idp.sign-up-custom-number-bounds-rejected|_error#9|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: cognito-idp.sign-up-custom-string-length-bounds-rejected|_error#8|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: cognito-idp.update-user-attributes-custom-attribute-bounds-rejected|_error#12|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: cognito-idp.admin-update-custom-attribute-lower-bounds-rejected|_error#9|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: cognito-idp.admin-update-preferred-username-taken-alias-rejected|run#11|AssertionError|assert (err["Code"], err["Message"]) == TAKEN, (
Issue-Key: cognito-idp.update-user-attributes-preferred-username-taken-alias-rejected|run#14|AssertionError|assert (err["Code"], err["Message"]) == TAKEN, (
Issue-Key: cognito-idp.sign-up-preferred-username-alias-rejected-until-account-exists|_error#7|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: cognito-idp.groups-per-user-quota-rejected|expect_error#14|AssertionError|raise AssertionError(f"expected {code} but the call succeeded: {result!r}")
Issue-Key: cognito-idp.list-users-attributes-to-get-requires-a-value-on-every-user|_error#16|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: cognito-idp.tag-resource-rejects-aws-prefixed-tag-key|_error#5|AssertionError|raise AssertionError(f"expected a rejection, but the call SUCCEEDED and returned {result!r}")
Issue-Key: cognito-idp.tag-untag-resource-reject-aws-reserved-key-prefix|_expect_error#8|AssertionError|raise AssertionError(f"expected the call to be refused with {(code, message)!r}, got {result!r}")
Issue-Key: cognito-idp.list-tags-for-resource-malformed-arn-rejected|run#11|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: cognito-idp.sub-uuid-identical-in-admin-get-user-and-list-users-refused-as-username|expect_error#23|AssertionError|raise AssertionError(f"expected {code} but the call succeeded: keys {sorted(result)!r}")
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cognito-idp: 9 net-new causes — ForceAliasCreation refused, custom-attribute constraints unenforced, alias/tag/ARN refusal wording and sub-as-Username resolution

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AdminCreateUser

## Description
Nine distinct root causes in the cognito-idp user/attribute/alias/tagging surface, all grounded in failing behavioral checks. Each cause below must be closed before submitting.

Cause 1 of 9: AdminCreateUser with ForceAliasCreation=true is refused instead of moving the alias.
AWS: Smithy: AdminCreateUser.ForceAliasCreation (ForceAliasCreation, default false); errors include UsernameExistsException (400) and InvalidParameterException (400). AWS documents that when an email/phone alias value supplied in UserAttributes is already a verified alias of another user, ForceAliasCreation=true migrates that alias to the newly created user and the create SUCCEEDS (only ForceAliasCreation=false yields a refusal); the previous owner loses the alias. Attribute-format validation (e.g. phone_number must be E.164) still applies, so a malformed phone_number is refused InvalidParameterException even with ForceAliasCreation=true.
Emulator: every AdminCreateUser that supplies an alias value already held by another user is refused 400 UsernameExistsException, message 'User account already exists', regardless of ForceAliasCreation. The alias is never moved and the format check is never reached.
Why: a client that uses the documented alias-migration path (re-homing an email/phone to a new account) cannot create the user at all, and the refusal it gets names the wrong condition.
Fix: the AdminCreateUser handler and its alias/username resolution in the cognito-idp crate (services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice, ops/admin_create_user.rs plus the alias index in the pool state).
Traces: cognito-idp.admin-create-user-force-alias-creation-alias-sign-in-authenticates-new-owner, cognito-idp.admin-create-user-force-alias-creation-moves-verified-email-alias, cognito-idp.admin-create-user-force-alias-creation-does-not-bypass-phone-number-format-validation.

Cause 2 of 9: declared custom-attribute constraints are not enforced on any write path.
AWS: a pool's Schema entry for custom:<name> carries NumberAttributeConstraints {MinValue, MaxValue} or StringAttributeConstraints {MinLength, MaxLength}; SignUp, UpdateUserAttributes and AdminUpdateUserAttributes all refuse a value outside those bounds with InvalidParameterException (400) and leave the user unchanged (SignUp/UpdateUserAttributes/AdminUpdateUserAttributes all model InvalidParameterException). The message names the offending attribute and the violated bound.
Emulator: all four writes answer HTTP 200 — SignUp returns a UserSub and creates the user, AdminUpdateUserAttributes/UpdateUserAttributes return an empty 200 — for values below MinValue/MinLength and above MaxValue/MaxLength.
Why: schema constraints a client relies on as a server-side guard are silently dropped, so out-of-range values become stored user state.
Fix: schema-constraint validation shared by the SignUp, UpdateUserAttributes and AdminUpdateUserAttributes handlers in the cognito-idp crate. Related open issue 20261026T235252Z-cognito-idp-five-causes-declared-custom-attribute-constraints.md describes the same family but does not carry these checks.
Traces: cognito-idp.sign-up-custom-number-bounds-rejected, cognito-idp.sign-up-custom-string-length-bounds-rejected, cognito-idp.update-user-attributes-custom-attribute-bounds-rejected, cognito-idp.admin-update-custom-attribute-lower-bounds-rejected.

Cause 3 of 9: the AliasExistsException message for a taken preferred_username is emulator-authored.
AWS: setting preferred_username to a value already held by another user (as username or alias) in a preferred_username-alias pool is refused AliasExistsException with the fixed text 'User account already exists for the alias.' — the message has no variable part.
Emulator: answers AliasExistsException with 'An account with the given preferred_username already exists.'
Why: clients and SDK error mapping that match on AWS's message text do not recognise the refusal.
Fix: the message emitted by the AdminUpdateUserAttributes / UpdateUserAttributes alias-conflict path in the cognito-idp crate.
Traces: cognito-idp.admin-update-preferred-username-taken-alias-rejected, cognito-idp.update-user-attributes-preferred-username-taken-alias-rejected.

Cause 4 of 9: SignUp accepts preferred_username in a preferred_username-alias pool.
AWS: in a pool whose AliasAttributes include preferred_username, SignUp may not carry a preferred_username attribute: the call is refused InvalidParameterException (modeled on SignUp, 400) with the fixed text 'Preferred username cannot be provided for unconfirmed account, since user pool is configured for preferred username alias.' and no user is created (a subsequent AdminGetUser answers UserNotFoundException 'User does not exist.').
Emulator: SignUp answers 200 with UserConfirmed=false and a UserSub, creating the user with the alias set while unconfirmed.
Why: an alias is bound to an unconfirmed account, which AWS forbids precisely because an unconfirmed holder would block the real owner.
Fix: the SignUp handler's alias-attribute validation in the cognito-idp crate.
Traces: cognito-idp.sign-up-preferred-username-alias-rejected-until-account-exists.

Cause 5 of 9: the per-user group-membership quota is not enforced.
AWS: a user may belong to at most 100 groups; the 101st AdminAddUserToGroup is refused LimitExceededException with the message 'The limit for addUserToGroup is: 100.' (fixed text, with the quota value as its only variable part) and the membership count stays at 100.
Emulator: the 101st AdminAddUserToGroup answers an empty HTTP 200 and the membership is added. (AdminAddUserToGroup models no LimitExceededException in this model copy; the quota refusal is AWS's documented account limit — recorded/documented behavior.)
Why: code that treats the quota refusal as the boundary signal never sees it, and the emulator's state exceeds a limit real AWS enforces.
Fix: quota enforcement in the AdminAddUserToGroup handler / group-membership state in the cognito-idp crate.
Traces: cognito-idp.groups-per-user-quota-rejected.

Cause 6 of 9: ListUsers(AttributesToGet=[...]) does not refuse a projection no matched user can satisfy.
AWS: Smithy: ListUsers.AttributesToGet (SearchedAttributeNamesListType); errors include InvalidParameterException (400). When a matched user has no value for a named attribute the whole call is refused InvalidParameterException with the fixed text 'Input fails to satisfy the constraints.'; once every matched user has a value the identical call succeeds and projects only those attributes.
Emulator: answers 200 and returns the users, one with an empty Attributes list.
Why: a client reading the projected attribute gets a silently empty list instead of the refusal that tells it the projection is invalid.
Fix: AttributesToGet handling in the ListUsers handler in the cognito-idp crate.
Traces: cognito-idp.list-users-attributes-to-get-requires-a-value-on-every-user.

Cause 7 of 9: TagResource/UntagResource accept aws:-prefixed system tag keys.
AWS: Smithy: TagResource.Tags (UserPoolTagsType), errors include InvalidParameterException (400). Tag keys in the reserved 'aws:' namespace may not be written or removed by a caller: the call is refused InvalidParameterException with the fixed text 'Caller is an end user and not allowed to mutate system tags' and the resource's tags are unchanged.
Emulator: both TagResource with an aws:-prefixed key and UntagResource naming one answer an empty HTTP 200, and the tag is written/removed.
Why: reserved-namespace tags become writable, so tag-based policy and cost allocation can be forged against the emulator.
Fix: reserved-prefix validation in the TagResource/UntagResource handlers in the cognito-idp crate.
Traces: cognito-idp.tag-resource-rejects-aws-prefixed-tag-key, cognito-idp.tag-untag-resource-reject-aws-reserved-key-prefix.

Cause 8 of 9: ListTagsForResource reports a malformed ResourceArn with the generic constraint message instead of Cognito's own.
AWS: Smithy: ListTagsForResource.ResourceArn (ArnType, @length 20..2048, @pattern ^arn:...$); error InvalidParameterException (400). For a value that is not a user-pool ARN the service answers InvalidParameterException whose message is 'Invalid user pool ARN: <the supplied value>' — fixed text plus the request's ResourceArn value.
Emulator: answers the framework's constraint report instead: "1 validation error detected: Value '<value>' at 'resourceArn' failed to satisfy constraint: Member must satisfy regular expression pattern: arn:[\\w+=/,.@-]+:...".
Why: a client branching on AWS's message (or surfacing it to an operator) sees text AWS never produces for this operation.
Fix: ARN parsing/refusal wording in the ListTagsForResource handler in the cognito-idp crate.
Traces: cognito-idp.list-tags-for-resource-malformed-arn-rejected.

Cause 9 of 9: AdminGetUser resolves a user's sub UUID as a Username.
AWS: Smithy: AdminGetUser.Username (UsernameType), error UserNotFoundException (404). The sub attribute value is not a username: unless the pool aliases make it one, AdminGetUser(Username=<sub uuid>) is refused UserNotFoundException with the fixed text 'User does not exist.' even though AdminGetUser/ListUsers report that same sub for the user.
Emulator: answers 200 with the full user (keys Username, UserAttributes, UserStatus, Enabled, ...) for the sub UUID.
Why: the sub becomes an accepted lookup key, hiding from clients the distinction between the immutable sub and the pool's username namespace.
Fix: username resolution in the AdminGetUser handler / user index in the cognito-idp crate.
Traces: cognito-idp.sub-uuid-identical-in-admin-get-user-and-list-users-refused-as-username.

## Evidence
Cause 1: AdminCreateUser -> botocore.errorfactory.UsernameExistsException: An error occurred (UsernameExistsException) when calling the AdminCreateUser operation: User account already exists (3 force-alias checks); expected the create to succeed and migrate the alias.
Cause 2: 'expected the call to be refused, but it succeeded' with HTTPStatusCode 200 (SignUp returns UserSub '79513968-...', AdminUpdateUserAttributes/UpdateUserAttributes return content-length 2).
Cause 3: expected ('AliasExistsException', 'User account already exists for the alias.'); got {'Code': 'AliasExistsException', 'Message': 'An account with the given preferred_username already exists.'}.
Cause 4: expected ('InvalidParameterException', 'Preferred username cannot be provided for unconfirmed account, since user pool is configured for preferred username alias.'); got 200 {'UserConfirmed': False, 'UserSub': 'ed2b8665-...'}.
Cause 5: expected ('LimitExceededException', 'The limit for addUserToGroup is: 100.'); got HTTP 200 on the 101st AdminAddUserToGroup.
Cause 6: expected ('InvalidParameterException', 'Input fails to satisfy the constraints.'); got 200 with Users=[{... 'Attributes': [{'Name':'given_name','Value':'Jane'}]}, {... 'Attributes': []}].
Cause 7: expected ('InvalidParameterException', 'Caller is an end user and not allowed to mutate system tags'); got HTTP 200 (content-length 2) for both TagResource and UntagResource.
Cause 8: expected message 'Invalid user pool ARN: <value>'; got "1 validation error detected: Value '<value>' at 'resourceArn' failed to satisfy constraint: Member must satisfy regular expression pattern: arn:[\\w+=/,.@-]+:[\\w+=/,.@-]+:([\\w+=/,.@-]*)?:[0-9]+:[\\w+=/,.@-]+(:[\\w+=/,.@-]+)?(:[\\w+=/,.@-]+)?".
Cause 9: expected UserNotFoundException 'User does not exist.'; the call succeeded with keys ['Enabled','ResponseMetadata','UserAttributes','UserCreateDate','UserLastModifiedDate','UserStatus','Username'].

## Steps to Reproduce
Re-replay the behavioral checks listed in trace_ids against the emulator. In outline: (1) create a pool with AliasAttributes=['email'] / ['preferred_username'], create a user owning a verified alias, then AdminCreateUser another user with the same alias and ForceAliasCreation=True; (2) create a pool with a custom number/string attribute carrying Min/Max constraints and SignUp / UpdateUserAttributes / AdminUpdateUserAttributes an out-of-bounds value; (3) set preferred_username to another user's value; (4) SignUp carrying preferred_username in a preferred_username-alias pool; (5) add one user to 101 groups; (6) ListUsers(AttributesToGet=['given_name']) with one user lacking given_name; (7) TagResource/UntagResource a user-pool ARN with an 'aws:'-prefixed key; (8) ListTagsForResource(ResourceArn='this-is-not-an-arn-at-all'); (9) AdminGetUser(Username=<the user's sub UUID>).
