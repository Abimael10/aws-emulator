Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudtrail__cognito-idp__events__sqs.xsvc-cognito-idp-1-admin-create-user-routes-only-to-its-own-rule-without-password-material
Issue-Key: cloudtrail__cognito-idp__events__sqs.xsvc-cognito-idp-1-admin-create-user-routes-only-to-its-own-rule-without-password-material|run#74|AssertionError|assert (del_detail.get("additionalEventData") or {}).get("sub") == user_sub and (
Issue-Commit: 027692653b37f8059828c0218f5349555bde1430

# cloudtrail__cognito-idp__events__sqs: the AdminDeleteUser record delivered via CloudTrail→EventBridge redacts the username but carries no additionalEventData.sub, so the record names no user at all

> _Note: this issue was identified against the code base at commit `027692653b37f8059828c0218f5349555bde1430`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AdminDeleteUser

## Description
Cause 1 of 1: the CloudTrail record the emulator publishes for cognito-idp:AdminDeleteUser (delivered onto the EventBridge default bus as an "AWS API Call via CloudTrail" event) replaces the @sensitive `username` request member with the literal HIDDEN_DUE_TO_SECURITY_REASONS — correctly — but emits no `detail.additionalEventData` at all, so the delivered record identifies no user. Failing check: cloudtrail__cognito-idp__events__sqs.xsvc-cognito-idp-1-admin-create-user-routes-only-to-its-own-rule-without-password-material.

AWS: Smithy (cognito-identity-provider): AdminDeleteUser.Username is `@sensitive` (target UsernameType, required, @length 1-128, @pattern ^[\p{L}\p{M}\p{S}\p{N}\p{P}]+$) and AdminDeleteUser.UserPoolId is required (@length 1-55, @pattern ^[\w-]+_[0-9a-zA-Z]+$); the model classifies the member as sensitive but says nothing about the audit representation, so the record shape is recorded behavior only. Recorded behavior: the management record Cognito emits for an Admin* call that takes a Username keeps the member and substitutes the fixed literal `HIDDEN_DUE_TO_SECURITY_REASONS` for its value (detail.requestParameters.username), and names the subject user instead by `detail.additionalEventData.sub` — the value being the `sub` attribute of that user, i.e. exactly the sub the AdminCreateUser response returned for the same user. Both the create record and the delete record for one user therefore carry the SAME additionalEventData.sub, which is the only way a subscriber can correlate the two. Fixed text here is the member names (`additionalEventData`, `sub`) and the redaction literal; the sub value itself is state (the pool user's sub), never a literal from one run. Envelope/detail framing is per https://docs.aws.amazon.com/awscloudtrail/latest/userguide/cloudwatch-event-reference.html.

Emulator: the AdminCreateUser leg now passes in full (envelope source aws.cognito-idp, detail-type "AWS API Call via CloudTrail", userPoolId verbatim, username and temporaryPassword both HIDDEN_DUE_TO_SECURITY_REASONS, messageAction SUPPRESS kept verbatim, responseElements.user.userStatus FORCE_CHANGE_PASSWORD, and additionalEventData.sub == the created user's sub). The AdminDeleteUser record delivered to the near-miss rule diverges: `detail.requestParameters.username` is 'HIDDEN_DUE_TO_SECURITY_REASONS' (correct) but `(detail.additionalEventData or {}).get('sub')` is None. Replay detail: `AssertionError: the delete names the SAME user by sub, with the username hidden: expected sub '231ba426-...' and username 'HIDDEN_DUE_TO_SECURITY_REASONS', actual None / 'HIDDEN_DUE_TO_SECURITY_REASONS'`. Hypothesis: the additionalEventData.sub enrichment was implemented only on the AdminCreateUser record builder (where the created user's sub is in hand from the response) and not on the delete path, which must resolve the sub from pool state before the user is removed.

Why: with the username redacted and no sub, the delete record is anonymous — a subscriber reacting to AdminDeleteUser cannot tell WHICH user was deleted, cannot correlate the delete with the earlier create (the two records share no identifying field), and any audit/alarm automation keyed on additionalEventData.sub silently matches nothing or drops the event. On real AWS the sub is the designed replacement for the hidden username, so this turns a correctly-redacted record into an unusable one.

Fix: the cognito-idp CloudTrail record builder for the Admin* user operations — specifically the AdminDeleteUser handler's audit-record construction — must resolve the target user's `sub` from user-pool state (before the deletion commits) and emit it as detail.additionalEventData.sub, the same enrichment the AdminCreateUser record already performs, so every record whose username is redacted still names its subject.

Note: open issue 20261112T213312Z-cloudtrail-cognito-idp-events-sqs-no-cloudtrail-sourced.md (filed when NO CloudTrail-sourced event reached any EventBridge rule and the check timed out in the delivery wait) does not cover this: delivery and the create-side redaction now work, and that issue carries a Timeout replay key in _collect, not this assertion. The remaining divergence is filed here.

Traces: cloudtrail__cognito-idp__events__sqs.xsvc-cognito-idp-1-admin-create-user-routes-only-to-its-own-rule-without-password-material (verdict mismatch; assertion at the AdminDeleteUser record check).

## Evidence
Expected (real AWS): the "AWS API Call via CloudTrail" event delivered for cognito-idp:AdminDeleteUser carries detail.eventSource 'cognito-idp.amazonaws.com', detail.eventName 'AdminDeleteUser', detail.requestParameters.userPoolId = the pool id, detail.requestParameters.username = the fixed literal 'HIDDEN_DUE_TO_SECURITY_REASONS', and detail.additionalEventData.sub = the deleted user's sub — the same sub value the AdminCreateUser record for that user carried, taken from pool state, not a literal.

Actual (emulator): the delete record is delivered to the correct rule with the username redacted, but additionalEventData is absent, so additionalEventData.sub is None. Replay output:
  [mismatch] cloudtrail__cognito-idp__events__sqs.xsvc-cognito-idp-1-admin-create-user-routes-only-to-its-own-rule-without-password-material
  AssertionError: the delete names the SAME user by sub, with the username hidden: expected sub '231ba426-bcca-4907-9c95-40e99b752d6a' and username 'HIDDEN_DUE_TO_SECURITY_REASONS', actual None / 'HIDDEN_DUE_TO_SECURITY_REASONS'
(The quoted sub is one run's generated value; the requirement is that it equal the deleted user's sub, whatever it is.) The same run's AdminCreateUser record DID carry additionalEventData.sub equal to that user's sub, so the omission is specific to the delete path.

## Steps to Reproduce
1. Boot the emulator. sts.get_caller_identity() -> account id.
2. idp.create_user_pool(PoolName=<unique>, Policies={PasswordPolicy: {...}}) -> pool id.
3. s3.create_bucket(<unique>) (+ CreateBucketConfiguration.LocationConstraint outside us-east-1) and put_bucket_policy allowing cloudtrail.amazonaws.com s3:GetBucketAcl on the bucket and s3:PutObject on AWSLogs/<account>/* with s3:x-amz-acl=bucket-owner-full-control.
4. cloudtrail.create_trail(Name=<unique>, S3BucketName=<bucket>, IsMultiRegionTrail=False, IncludeGlobalServiceEvents=False); put_event_selectors(TrailName=<arn>, EventSelectors=[{ReadWriteType: 'WriteOnly', IncludeManagementEvents: true}]); start_logging(Name=<arn>); get_trail_status -> IsLogging true.
5. Create two SQS queues with policies allowing events.amazonaws.com sqs:SendMessage from the rule ARN, and two EventBridge rules targeting them: {source:[aws.cognito-idp], detail-type:['AWS API Call via CloudTrail'], detail:{eventSource:['cognito-idp.amazonaws.com'], eventName:['AdminCreateUser']}} and the same with eventName AdminDeleteUser.
6. created = idp.admin_create_user(UserPoolId=<pool>, Username=<unique>, TemporaryPassword='Tp1!<token>', MessageAction='SUPPRESS')['User']; record sub = {a['Name']: a['Value'] for a in created['Attributes']}['sub']. Long-poll the create queue: the delivered record has requestParameters.username == 'HIDDEN_DUE_TO_SECURITY_REASONS' and additionalEventData.sub == sub (passes today).
7. idp.admin_delete_user(UserPoolId=<pool>, Username=<same username>); long-poll the delete queue and json-load the body. Expected: detail.requestParameters.username == 'HIDDEN_DUE_TO_SECURITY_REASONS' AND detail.additionalEventData.sub == the sub from step 6. Actual: the username is redacted but detail.additionalEventData is missing, so .sub is None.
