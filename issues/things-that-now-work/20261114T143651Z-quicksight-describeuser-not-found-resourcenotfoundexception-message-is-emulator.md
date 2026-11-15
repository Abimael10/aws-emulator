Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: quicksight.register-user-quicksight-identity-lifecycle
Issue-Shape-Id: com.amazonaws.quicksight#QuickSight_20180401
Issue-Key: quicksight.register-user-quicksight-identity-lifecycle|run#25|AssertionError|assert native_described_err["Message"] == (
Issue-Commit: 91e97466cace8d6c8c0f96a82b66a2b2c54b3f99

# QuickSight DescribeUser not-found ResourceNotFoundException message is emulator-authored prose instead of AWS's "AWS account id: ..., Namespace: ..., Username: ... " template

> _Note: this issue was identified against the code base at commit `91e97466cace8d6c8c0f96a82b66a2b2c54b3f99`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeUser

## Description
One net-new root cause in the QuickSight emulator. DescribeUser correctly refuses a user it has no record of with the right error shape (ResourceNotFoundException) and the right HTTP status (404), but writes its own English sentence as the message, so the text a client reads is wording AWS never emits. For this error class the message is part of the contract.

This bundle has 1 cause; it must be closed before submitting.

Related already-open issues describe the SAME wording fault class on OTHER QuickSight resource paths and do NOT cover the user path: 20261111T073736Z-quicksight-resourcenotfoundexception-messages-for-namespaces-and-themes-are.md (namespace, theme), 20261103T040231Z-quicksight-data-set-resourcenotfoundexception-message-is-invented-wording.md (data set), and 20261109T121538Z-quicksight-not-found-and-already-exists-error-messages.md (CreateGroup / CreateFolder / DescribeDataSource). No user not-found message appears in any of them. The recently FIXED issue 20261114T050522Z-quicksight-registeruser-quicksight-identity-rejection-on-an-iam.md covered RegisterUser's AccessDeniedException wording in this same check and its fix now holds — the check advances past that assertion and fails on the DescribeUser message that follows, so this is NOT a reopen of it.

Cause 1 of 1: DescribeUser on a user name that was never registered.
AWS: DescribeUser (Smithy: GET /accounts/{AwsAccountId}/namespaces/{Namespace}/users/{UserName}, success code 200; modeled errors include ResourceNotFoundException, errorKind client, httpError 404 — services/service-com-amazonaws-quicksight-quicksight-20180401/smithy-model/com-amazonaws-quicksight-quicksight-20180401.json). Describing a user name that has no record in the namespace answers HTTP 404, Code ResourceNotFoundException, with the locator-only template: fixed text "AWS account id: ", ", Namespace: ", ", Username: ", ". " — note it carries NO leading sentence at all for a name that was never registered, and it ends with a period followed by a TRAILING SPACE. The variable parts are the request's AwsAccountId, Namespace and UserName path labels; nothing in it comes from emulator state. The model fixes the shape and the status but not the text, so the template itself is recorded behavior only.
The same check asserts a SECOND, different template on the post-DeleteUser path — fixed text "User ", " does not exist." immediately concatenated (no separating space) with the same locator trailer "AWS account id: ", ", Namespace: ", ", Username: ", ". " — i.e. AWS prefixes the existence sentence only once the user is known to have been deleted. The run never reaches that assertion because it fails on the never-registered path first, but both texts are produced by the one shared helper named below, so the fix must cover both: the locator-only form for a name with no record, and the "User <UserName> does not exist." + locator form after DeleteUser.
Emulator: HTTP 404 and Code ResourceNotFoundException (both correct) with message "User <UserName> not found in namespace <Namespace> in account <AwsAccountId>." — an emulator-authored sentence from the template "User {UserName} not found in namespace {Namespace} in account {AwsAccountId}." It has none of AWS's fixed fragments ("AWS account id: ", ", Namespace: ", ", Username: "), no trailing space, and prefixes an existence sentence on a user that was never registered, where AWS emits the locator only.
Why: a client that surfaces, logs or pattern-matches QuickSight's user-lookup failure text — or parses the "AWS account id: ... , Namespace: ... , Username: ..." trailer out of it to recover which account/namespace/user the miss refers to, or keys off the presence of the "User <name> does not exist." prefix to tell a deleted user apart from one that never existed — finds none of the fragments it expects and cannot make that distinction at all, since the emulator serves one sentence for both cases.
Fix: the not-found message construction shared by the QuickSight user handlers — the helper crate::state::user_not_found_message, called from DescribeUser's handler (services/service-com-amazonaws-quicksight-quicksight-20180401/src/ops/describe_user.rs, the ResourceNotFoundException arm of handle) and from the other user paths that report a missing user. Render AWS's templates from the request's path labels, and distinguish the never-registered form from the post-delete form.
Traces: quicksight.register-user-quicksight-identity-lifecycle.

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against the emulator. HTTP status (404) and error Code (ResourceNotFoundException) matched; only the message text diverges.

Check quicksight.register-user-quicksight-identity-lifecycle — subscribe the account (ENTERPRISE / IAM_ONLY), create an IAM user and RegisterUser(IdentityType=IAM, UserRole=READER) as the positive control, then RegisterUser(IdentityType=QUICKSIGHT) which is correctly refused AccessDeniedException, then DescribeUser on that native user name, which was never registered.

  Expected (AWS): Error.Code = ResourceNotFoundException, Error.Message = "AWS account id: <AwsAccountId>, Namespace: <Namespace>, Username: <UserName>. " — fixed text "AWS account id: " / ", Namespace: " / ", Username: " / ". " (note the trailing space); variable parts = the request's AwsAccountId, Namespace and UserName.
  Actual (emulator): {'Code': 'ResourceNotFoundException', 'Message': 'User <UserName> not found in namespace <Namespace> in account <AwsAccountId>.'} — observed with the generated user name, namespace "default" and the emulator's 12-digit account id filling the template; those per-run values only fill it and are not part of the requirement.
  Check assertion: `assert native_described_err["Message"] == (f"AWS account id: {account_id}, Namespace: {namespace}, Username: {native_user_name}. ")` -> AssertionError.

Later in the same check (not reached, same helper): `assert gone_err["Message"] == (f"User {user_name} does not exist." f"AWS account id: {account_id}, Namespace: {namespace}, Username: {user_name}. ")` after DeleteUser — the post-delete template, which the emulator's single sentence also cannot produce.

Handler evidence: services/service-com-amazonaws-quicksight-quicksight-20180401/src/ops/describe_user.rs builds the refusal as ResourceNotFoundException { message: Some(crate::state::user_not_found_message(&input.user_name, &input.namespace, &input.aws_account_id)), .. } when store.user(..) finds no record — so the wording lives entirely in that one helper.

Smithy citation: DescribeUser input AwsAccountId (len 12..12, pattern ^[0-9]{12}$), Namespace (len 0..64), UserName (len 1.., pattern ^[\u0020-\u00FF]+$), all required httpLabels; errors include ResourceNotFoundException (client, httpError 404). The model does not fix the message text, so the templates above are recorded behavior only.

## Steps to Reproduce
1. Subscribe the account if it is not already ACCOUNT_CREATED: CreateAccountSubscription(AwsAccountId, AccountName=<any>, NotificationEmail=<any>, AuthenticationMethod=IAM_ONLY, Edition=ENTERPRISE), then poll DescribeAccountSubscription until AccountInfo.AccountSubscriptionStatus is ACCOUNT_CREATED.
2. Pick a user name that was never registered, e.g. <fresh-name>.
3. Call DescribeUser(AwsAccountId=<account>, Namespace="default", UserName=<fresh-name>) and read Error.Message: the emulator answers "User <fresh-name> not found in namespace default in account <account>." where AWS answers the locator-only "AWS account id: <account>, Namespace: default, Username: <fresh-name>. " (with the trailing space and no leading sentence). Status 404 and Code ResourceNotFoundException are already correct.
4. For the second template of the same helper: create an IAM user, RegisterUser(AwsAccountId, Namespace="default", IdentityType=IAM, IamArn=<that user's arn>, Email=<any>, UserRole=READER), then DeleteUser(AwsAccountId, Namespace="default", UserName=<the registered user name>), then DescribeUser on it and read Error.Message: AWS answers "User <UserName> does not exist.AWS account id: <account>, Namespace: default, Username: <UserName>. ".
