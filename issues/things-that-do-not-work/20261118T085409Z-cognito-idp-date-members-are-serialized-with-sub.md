Issue-Source: trace-sweep-divergence
Issue-Trace: cognito-idp-user-pool-tag-change-and-remove-last
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cognito-idp-user-pool-tag-change-and-remove-last cip-alias-force-creation-migrates-a-verified-email cognito-idp-email-alias-pool-refuses-email-format-usernames
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp|CreateUserPool|2xx->2xx|$.UserPool.CreationDate
Issue-Key: cognito-idp|CreateUserPool|2xx->2xx|$.UserPool.LastModifiedDate
Issue-Key: cognito-idp|DescribeUserPool|2xx->2xx|$.UserPool.CreationDate
Issue-Key: cognito-idp|DescribeUserPool|2xx->2xx|$.UserPool.LastModifiedDate
Issue-Key: cognito-idp|AdminCreateUser|2xx->2xx|$.User.UserCreateDate
Issue-Key: cognito-idp|AdminCreateUser|2xx->2xx|$.User.UserLastModifiedDate
Issue-Key: cognito-idp|AdminGetUser|2xx->2xx|$.UserCreateDate
Issue-Key: cognito-idp|AdminGetUser|2xx->2xx|$.UserLastModifiedDate
Issue-Commit: a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50

# cognito-idp date members are serialized with sub-millisecond precision

> _Note: this issue was identified against the code base at commit `a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateUserPool

## Description
Every cognito-idp response member whose target is the model's DateType is serialized with a fractional second carrying more than millisecond precision, so the recorded shape of the member is not reproduced. 4 causes, one per operation whose read path the corpus exercises; each must be closed before submitting.

Cause 1 of 4: CreateUserPool's UserPool.CreationDate / UserPool.LastModifiedDate.
AWS: Smithy: CreateUserPool output UserPool (UserPoolType); UserPoolType.CreationDate / LastModifiedDate target com.amazonaws.cognitoidentityprovider#DateType (type timestamp, no @timestampFormat override), and the service's protocol is awsJson1_1, whose default timestamp serialization is epoch-seconds as a JSON number. The member's documentation states "Amazon Cognito returns this timestamp in UNIX epoch time format". Recorded behavior only for the precision: the recorded responses carry an epoch-second number whose shape the corpus accepts, and the emulator's value does not match that shape. Hypothesis: AWS emits at most millisecond resolution in the fraction (3 fractional digits), so a value with 7 fractional digits is a different shape.
Emulator: emits the raw float clock reading, e.g. 1794991550.211196 for both CreationDate and LastModifiedDate.
Why: a client that round-trips, hashes, caches or textually compares the serialized timestamp (or that asserts on the precision AWS guarantees) sees a value AWS never produces; sub-millisecond digits also make two reads of an unmodified pool distinguishable from AWS's.
Fix: the timestamp serializer used for DateType members in the cognito-idp crate's response serialization (services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice) — it must truncate the epoch-seconds fraction to the resolution AWS emits, for all DateType members rather than per operation.
Traces: cognito-idp-user-pool-tag-change-and-remove-last#task#1.

Cause 2 of 4: DescribeUserPool's UserPool.CreationDate / UserPool.LastModifiedDate — the same DateType members on the read path, same emitted value as the create that produced them.
AWS: as Cause 1 (UserPoolType.CreationDate/LastModifiedDate -> DateType).
Emulator: 1794991550.211196 for both.
Why: as Cause 1; the read path is the one clients poll.
Fix: as Cause 1 (shared serializer).
Traces: cognito-idp-user-pool-tag-change-and-remove-last#task#3 (and #task#5, #task#7 in the same cluster).

Cause 3 of 4: AdminCreateUser's User.UserCreateDate / User.UserLastModifiedDate.
AWS: Smithy: AdminCreateUser output User (UserType); UserType.UserCreateDate / UserLastModifiedDate target DateType, documented as UNIX epoch time.
Emulator: 1794991625.7440178 for both.
Why: as Cause 1.
Fix: as Cause 1.
Traces: cip-alias-force-creation-migrates-a-verified-email#task#3.

Cause 4 of 4: AdminGetUser's UserCreateDate / UserLastModifiedDate (top-level DateType members of the response).
AWS: Smithy: AdminGetUser output members UserCreateDate / UserLastModifiedDate -> DateType.
Emulator: 1794991625.7440178 for both; also 1794991573.6076317 / 1794991573.7265584 in the standard-attribute fixture.
Why: as Cause 1.
Fix: as Cause 1.
Traces: cip-alias-force-creation-migrates-a-verified-email#task#4.

The same wrong-precision value also appears inside the UserPoolClient clusters filed under "cognito-idp UserPoolClient responses omit ClientType and emit empty list members" and in the AdminGetUser cluster filed under "cognito-idp user-attribute update paths perform no validation"; closing this serializer closes the date keys there too.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200):

cognito-idp-user-pool-tag-change-and-remove-last#task#1 CreateUserPool, status 200 -> 200:
  $.UserPool.CreationDate: value_mismatch (expected '<shaped-not-asserted>', got 1794991550.211196)
  $.UserPool.LastModifiedDate: value_mismatch (expected '<shaped-not-asserted>', got 1794991550.211196)
cognito-idp-user-pool-tag-change-and-remove-last#task#3 DescribeUserPool (cluster also covers #task#5, #task#7): identical two keys, same values.
cip-alias-force-creation-migrates-a-verified-email#task#3 AdminCreateUser:
  $.User.UserCreateDate / $.User.UserLastModifiedDate: value_mismatch (expected '<shaped-not-asserted>', got 1794991625.7440178)
cip-alias-force-creation-migrates-a-verified-email#task#4 AdminGetUser:
  $.UserCreateDate / $.UserLastModifiedDate: value_mismatch (expected '<shaped-not-asserted>', got 1794991625.7440178)

'<shaped-not-asserted>' means the corpus asserts the member's SHAPE and not its value, so the per-run clock reading is not the complaint — the extra fractional digits are. Smithy: UserPoolType.CreationDate/LastModifiedDate, UserType.UserCreateDate/UserLastModifiedDate, AdminGetUser.UserCreateDate/UserLastModifiedDate all target com.amazonaws.cognitoidentityprovider#DateType (timestamp, no @timestampFormat); protocol awsJson1_1 -> epoch-seconds number.

## Steps to Reproduce
1. Replay fixture cognito-idp-user-pool-tag-change-and-remove-last, step task#1: CreateUserPool and read UserPool.CreationDate / UserPool.LastModifiedDate -> the emulator answers an epoch-seconds number with 6-7 fractional digits.
2. Replay the same fixture step task#3 (DescribeUserPool) -> same two members, same precision.
3. Replay cip-alias-force-creation-migrates-a-verified-email step task#3 (AdminCreateUser) and task#4 (AdminGetUser) -> User.UserCreateDate / User.UserLastModifiedDate and the top-level UserCreateDate / UserLastModifiedDate show the same precision.
4. Compare each against the recorded shape for a cognito-idp DateType member.
