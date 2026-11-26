Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache.create-cache-cluster-invalid-arguments elasticache.delete-default-parameter-group-forbidden
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache.create-cache-cluster-invalid-arguments|_assert_error#9|AssertionError|assert message == expected[1], f"expected message {expected[1]!r}, got {message!r}"
Issue-Key: elasticache.delete-default-parameter-group-forbidden|_assert_error#18|AssertionError|assert got == expected[1], f"{expected[0]}: expected message {expected[1]!r}, got {got!r}"
Issue-Commit: 719a7d873c2ead99db4a3b531125e6f7ff90e604

# ElastiCache: 2 net-new causes — the invalid-identifier refusal message for CacheClusterId and CacheParameterGroupName is the emulator's own "<Label> specified is not a valid identifier." prose instead of AWS's "The parameter <Member> is not a valid identifier."

> _Note: this issue was identified against the code base at commit `719a7d873c2ead99db4a3b531125e6f7ff90e604`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCacheCluster

## Description
Two independent net-new causes in the ElastiCache control plane's identifier-format validation. In both the emulator refuses correctly (HTTP 400, Error.Code InvalidParameterValue) and leaves nothing provisioned, but the lead-in sentence of the message is the emulator's own prose rather than the sentence AWS returns for that error class. Both must be closed before this issue is submitted.

Note on existing issues: the two checks below are each listed in the open issue 20261109T151249Z-elasticache-refusal-messages-are-the-emulator-s-own--r2.md, but for DIFFERENT causes (its Cause 3 = CreateCacheCluster's unknown-Engine text, its Cause 2 = the CacheParameterGroupNotFound text). Those earlier assertions now pass and each check has advanced to the identifier-format assertion described here, which no open issue carries — the suite marked neither check OWNED or ALREADY FILED. Related: 20261117T114712Z-elasticache-3-net-new-refusal-causes-createuser-pattern.md records the MIRROR-IMAGE defect for ReplicationGroupId (there AWS says "Replication Group ID specified is not a valid identifier." and the emulator says "The parameter ReplicationGroupIdentifier is not a valid identifier."); it covers ReplicationGroupId only. Taken together, the emulator appears to apply the two lead-in templates to the wrong resources — fixing this issue must not regress that one, so the lead-in has to be per-identifier, not one global template.

Cause 1 of 2: the CacheClusterId format refusal names the wrong subject.
AWS: Smithy (com-amazonaws-elasticache-amazonelasticachev9) models CreateCacheCluster.CacheClusterId as @required String and DescribeCacheClusters.CacheClusterId as String; both operations model InvalidParameterValueException with awsQueryError code InvalidParameterValue, httpResponseCode 400. Recorded behavior: an id that is not a legal identifier (uppercase letters and underscores, e.g. an id of the form "<Mixed_Case_Underscored>") is refused HTTP 400, Error.Code InvalidParameterValue, Error.Message "The parameter CacheClusterIdentifier is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens." The message is entirely FIXED text for this error class — it interpolates no value from the request; the only name in it is AWS's own parameter label CacheClusterIdentifier. The identical refusal and identical message are required from DescribeCacheClusters(CacheClusterId=<malformed id>), which validates the id's format BEFORE it looks anything up (it does not answer CacheClusterNotFound).
Emulator: the status and code are right, but the message's lead-in is "Cache Cluster ID specified is not a valid identifier." followed by the correct trailing rule sentence. Because the check compares the whole message and stops at the first mismatch, the DescribeCacheClusters leg of the same rule was not graded and very likely carries the same lead-in.
Why: the message is the only thing that distinguishes a malformed-identifier refusal from the operation's other InvalidParameterValue cases (unknown node type, unknown engine), so a client or parity suite that logs on, displays, or matches AWS's sentence sees text AWS never emits, and the emulator attributes the fault to a subject ("Cache Cluster ID") AWS does not name here.
Fix: the identifier-format validation shared by CreateCacheCluster and DescribeCacheClusters in the elasticache operation handlers — the refusal's message template for CacheClusterId must be AWS's fixed text above.
Traces: elasticache.create-cache-cluster-invalid-arguments.

Cause 2 of 2: the CacheParameterGroupName format refusal names the wrong subject.
AWS: Smithy models DeleteCacheParameterGroup.CacheParameterGroupName as @required String, with modeled errors CacheParameterGroupNotFoundFault (CacheParameterGroupNotFound, 404), InvalidCacheParameterGroupStateFault (400) and InvalidParameterValueException (InvalidParameterValue, 400). Recorded behavior: AWS refuses the delete of a family default group — whose name carries a dot, e.g. a name of the form "default.<family>" — at identifier validation, not at a protected-resource check: HTTP 400, Error.Code InvalidParameterValue, Error.Message "The parameter CacheParameterGroupName is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens." Entirely fixed text for this error class; the only name in it is the input member name CacheParameterGroupName. The refused delete must leave the group listed and unchanged.
Emulator: status and code are right, but the lead-in is "Cache Parameter Group Name specified is not a valid identifier." with the correct trailing rule sentence. Because the check stops at the first mismatch, the subsequent assertions (the group still listed, and ModifyCacheParameterGroup on a default group refused with the fixed sentence "Cannot modify a default parameter group.") were not graded in this run and may carry their own divergences.
Why: a client that keys default-group protection on AWS's documented sentence does not match, and a dotted name is reported as if the subject were a different parameter than the one the request carries.
Fix: the identifier-format validation in DeleteCacheParameterGroup's handler (and the shared parameter-group name validator it calls) — the message template for CacheParameterGroupName must be AWS's fixed text above.
Traces: elasticache.delete-default-parameter-group-forbidden.

## Evidence
Cause 1 — elasticache.create-cache-cluster-invalid-arguments (CreateCacheCluster with a malformed CacheClusterId, after a positive-control cluster on the same subnet group reached available):
  expected: HTTP 400, Error.Code InvalidParameterValue, Error.Message 'The parameter CacheClusterIdentifier is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.'
  actual:   HTTP 400, Error.Code InvalidParameterValue, Error.Message 'Cache Cluster ID specified is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.'
  (AssertionError at check_elasticache_create_cache_cluster_invalid_arguments.py line 32: assert message == expected[1])
  Fixed vs variable: the ENTIRE expected message is fixed text for this error class; nothing in it comes from the request. The divergence is confined to the lead-in sentence ('The parameter CacheClusterIdentifier is not a valid identifier.' vs 'Cache Cluster ID specified is not a valid identifier.'); the trailing rule sentence already matches.

Cause 2 — elasticache.delete-default-parameter-group-forbidden (DeleteCacheParameterGroup on the family default group, after the same create/modify/delete sequence succeeded on a custom group of the same family):
  expected: HTTP 400, Error.Code InvalidParameterValue, Error.Message 'The parameter CacheParameterGroupName is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.'
  actual:   HTTP 400, Error.Code InvalidParameterValue, Error.Message 'Cache Parameter Group Name specified is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.'
  (AssertionError at check_elasticache_delete_default_parameter_group_forbidden.py line 29: assert got == expected[1])
  Fixed vs variable: the ENTIRE expected message is fixed text; the divergence is the lead-in sentence only.

## Steps to Reproduce
Cause 1:
1. CreateCacheSubnetGroup over the run's subnets; CreateCacheCluster(CacheClusterId=<well-formed id>, Engine='redis', CacheNodeType='cache.t3.micro', NumCacheNodes=1, CacheSubnetGroupName=<that group>) and wait for 'available' (positive control).
2. CreateCacheCluster(CacheClusterId='Sim2Real_Behav_Bad_Id', Engine='redis', CacheNodeType='cache.t3.micro', NumCacheNodes=1, CacheSubnetGroupName=<that group>). Compare Error.Message to 'The parameter CacheClusterIdentifier is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.'
3. DescribeCacheClusters(CacheClusterId='Sim2Real_Behav_Bad_Id') — the same code and the same message are required (not CacheClusterNotFound).

Cause 2:
1. DescribeCacheParameterGroups(CacheParameterGroupName='default.redis7') — it must be listed with CacheParameterGroupFamily 'redis7'.
2. Positive control: CreateCacheParameterGroup(<custom name>, Family='redis7'), ModifyCacheParameterGroup(maxmemory-policy), DeleteCacheParameterGroup(<custom name>) — all succeed.
3. DeleteCacheParameterGroup(CacheParameterGroupName='default.redis7'). Compare Error.Message to 'The parameter CacheParameterGroupName is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.'
4. Re-read DescribeCacheParameterGroups(CacheParameterGroupName='default.redis7') — still listed, unchanged — and ModifyCacheParameterGroup on it must be refused InvalidParameterValue with 'Cannot modify a default parameter group.'
