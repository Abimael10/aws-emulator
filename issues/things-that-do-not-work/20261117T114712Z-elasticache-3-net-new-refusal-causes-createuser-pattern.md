Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache.create-user-invalid-userid-format-rejected elasticache.create-replication-group-invalid-replication-group-id-rejected elasticache.describe-max-records-bounds-differ-by-operation
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache.create-user-invalid-userid-format-rejected|run#6|AssertionError|assert code == "InvalidParameterValue", f"expected InvalidParameterValue, got {code!r}"
Issue-Key: elasticache.create-replication-group-invalid-replication-group-id-rejected|run#7|AssertionError|assert message == _FORMAT_MESSAGE, f"unexpected message for a digit-leading id: {message!r}"
Issue-Key: elasticache.describe-max-records-bounds-differ-by-operation|_error#10|AssertionError|raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED with {result!r}")
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# ElastiCache: 3 net-new refusal causes — CreateUser pattern violation answers ValidationError, CreateReplicationGroup's invalid-identifier text is the emulator's own, and DescribeServiceUpdates enforces no MaxRecords bound

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateUser

## Description
Three independent net-new causes in the ElastiCache control plane's input-rejection paths. Each must be closed before this issue is submitted.

Cause 1 of 3: CreateUser refuses a pattern-violating UserId with the wrong error code.
AWS: Smithy (com-amazonaws-elasticache-amazonelasticachev9) models CreateUser.UserId as target UserId, @required, @length min 1, @pattern "^[a-zA-Z][a-zA-Z0-9\\-]*$"; CreateUser's modeled errors include InvalidParameterValueException with awsQueryError code InvalidParameterValue, httpResponseCode 400. Recorded behavior: a UserId that begins with a digit, or that contains an underscore, is refused HTTP 400 with Error.Code "InvalidParameterValue" and Error.Message "User id must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens." — that message is entirely fixed text for this error class (it names no request value), and no user is created (a following DescribeUsers for the same id is refused UserNotFound, "User <UserId> not found.", where <UserId> is the id from the request).
Emulator: the call is refused, but with Error.Code "ValidationError" — the generic constraint-validation envelope — rather than the modeled InvalidParameterValue. The check asserts the code first, so the message and the absent-effect read were not reached.
Why: a client that branches on the refusal (botocore's ec.exceptions.InvalidParameterValueException, or an `except` on code == "InvalidParameterValue") never matches; the malformed-identifier case becomes an unhandled generic validation failure instead of the modeled InvalidParameterValue, so retry/repair logic keyed on the modeled error is dead code against the emulator.
Fix: CreateUser's input validation in the elasticache operation handler — the UserId @pattern check must raise the service's InvalidParameterValueException (awsQuery code InvalidParameterValue, 400) carrying AWS's fixed "User id must begin with a letter; ..." text, not the generic ValidationError produced by the shared constraint layer.
Traces: elasticache.create-user-invalid-userid-format-rejected.

Cause 2 of 3: CreateReplicationGroup's invalid-ReplicationGroupId message is the emulator's own prose.
AWS: CreateReplicationGroup.ReplicationGroupId is @required; InvalidParameterValueException (InvalidParameterValue, 400) is modeled. Recorded behavior: an id that begins with a digit (and likewise one that ends with a hyphen) is refused InvalidParameterValue with the message "Replication Group ID specified is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens." The whole message is fixed text for this error class — it interpolates no request value. The same refusal and the same message are required from DescribeReplicationGroups(ReplicationGroupId=<malformed id>), which validates the id's format before it looks anything up, and no replication group is created.
Emulator: the code is correct but the message is "The parameter ReplicationGroupIdentifier is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens." — the leading sentence names a parameter ("The parameter ReplicationGroupIdentifier") that is not AWS's wording; AWS's fixed lead-in is "Replication Group ID specified is not a valid identifier." The trailing rule sentence matches. Because the check stops at the first mismatch, the trailing-hyphen variant and the DescribeReplicationGroups path were not graded and may carry the same wording defect.
Why: the message is the only thing distinguishing this refusal class for a client that logs or asserts on it; CLI users and parity test suites keyed to AWS's text see a different sentence, and the emulator-authored lead-in also mis-names the parameter (ReplicationGroupIdentifier is not the input member's name, ReplicationGroupId is).
Fix: the identifier-format validation shared by CreateReplicationGroup and DescribeReplicationGroups in the elasticache handlers — the refusal message template must be AWS's fixed text.
Traces: elasticache.create-replication-group-invalid-replication-group-id-rejected.

Cause 3 of 3: DescribeServiceUpdates accepts a MaxRecords above its upper bound.
AWS: Smithy models DescribeServiceUpdates.MaxRecords only as IntegerOptional (no @range) and declares InvalidParameterValueException (InvalidParameterValue, 400) among its errors; the bound is recorded behavior only. Recorded behavior: MaxRecords=101 is refused HTTP 400 InvalidParameterValue with "Invalid value for max records. Must be between 1 and 100" — fixed text apart from the two bounds, which are this operation's own (1 and 100, unlike DescribeCacheEngineVersions, whose refusal names 20 and 100). Values inside the range page normally (MaxRecords=19 returns at most 19 ServiceUpdates).
Emulator: DescribeServiceUpdates(MaxRecords=101) SUCCEEDS with HTTP 200 and a ServiceUpdates list (the catalog's elasticache-memcached-security-update-001, -002, elasticache-redis-oss-v6-eol-cache-clusters, ...) — no bound is enforced at all. The per-operation MaxRecords bound for DescribeCacheEngineVersions (20..100) was graded earlier in the same check and passed, so the gap is specific to DescribeServiceUpdates.
Why: a client probing the accepted MaxRecords range, or one relying on the refusal to surface a bad page size, gets a silent 200; the divergence also hides that ElastiCache's MaxRecords bounds differ per operation, so code calibrated against the emulator sends page sizes real AWS rejects.
Fix: DescribeServiceUpdates' input validation in the elasticache operation handler — add the 1..100 MaxRecords gate raising InvalidParameterValueException with that operation's own message.
Traces: elasticache.describe-max-records-bounds-differ-by-operation.

## Evidence
Cause 1 — elasticache.create-user-invalid-userid-format-rejected:
  expected: Error.Code "InvalidParameterValue" (HTTP 400), Error.Message "User id must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens."
  actual:   AssertionError: expected InvalidParameterValue, got 'ValidationError'

Cause 2 — elasticache.create-replication-group-invalid-replication-group-id-rejected:
  expected message: 'Replication Group ID specified is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.'
  actual message:   'The parameter ReplicationGroupIdentifier is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.'
  (AssertionError: unexpected message for a digit-leading id)

Cause 3 — elasticache.describe-max-records-bounds-differ-by-operation:
  expected: ClientError InvalidParameterValue, 'Invalid value for max records. Must be between 1 and 100'
  actual:   AssertionError: expected the call to be rejected, but it SUCCEEDED with {'ServiceUpdates': [{'ServiceUpdateName': 'elasticache-memcached-security-update-001', ...}, ...]} (HTTP 200)
  Note: the DescribeCacheEngineVersions assertions (MaxRecords 19 and 101 refused with 'Invalid value for max records. Must be between 20 and 100', MaxRecords=20 paging exactly 20) passed in the same check.

## Steps to Reproduce
1. CreateUser(UserId="9abc", UserName="9abc", Engine="redis", AccessString="on ~* &* +@all", Passwords=["<pw>"]) — and again with UserId="u_abc". Observe Error.Code; AWS answers InvalidParameterValue, the emulator answers ValidationError.
2. CreateReplicationGroup(ReplicationGroupId="9<suffix>", ReplicationGroupDescription=..., Engine="redis", CacheNodeType="cache.t3.micro", NumCacheClusters=1, CacheSubnetGroupName=<a valid group>) and DescribeReplicationGroups(ReplicationGroupId="9<suffix>"). Compare Error.Message with AWS's 'Replication Group ID specified is not a valid identifier. ...'. Repeat with an id ending in a hyphen.
3. DescribeServiceUpdates(MaxRecords=101) — AWS refuses InvalidParameterValue 'Invalid value for max records. Must be between 1 and 100'; the emulator returns 200 with the ServiceUpdates page. For contrast, DescribeCacheEngineVersions(MaxRecords=101) is correctly refused naming 20 and 100.
