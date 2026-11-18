Issue-Source: trace-sweep-divergence
Issue-Trace: elasticache-describe-snapshots-source-validation-and-filtered-reads
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticache-describe-snapshots-source-validation-and-filtered-reads elasticache-describe-serverless-cache-snapshots-not-found-and-validation elasticache-cache-parameter-group-source-filter-and-event-validation
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache|DescribeSnapshots|2xx->4xx|$.DescribeSnapshotsResponse
Issue-Key: elasticache|DescribeSnapshots|2xx->4xx|status
Issue-Key: elasticache|DescribeSnapshots|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: elasticache|DescribeSnapshots|4xx->4xx|status
Issue-Key: elasticache|DescribeServerlessCacheSnapshots|4xx->2xx|$.ErrorResponse
Issue-Key: elasticache|DescribeServerlessCacheSnapshots|4xx->2xx|status
Issue-Key: elasticache|DescribeEvents|2xx->2xx|$.DescribeEventsResponse.DescribeEventsResult.Events
Issue-Related: 20261118T043247Z-elasticache-validation-refusals-answer-the-emulator-s-own.md
Issue-Commit: 5c103ae721112cfc59443e8c0b52708204a7cb08

# ElastiCache filtered reads answer the wrong status: an empty match 404s, an invalid filter value passes, and a parameter-combination gate is skipped

> _Note: this issue was identified against the code base at commit `5c103ae721112cfc59443e8c0b52708204a7cb08`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeSnapshots

## Description
Four net-new causes, one shared fault: ElastiCache's filtered read paths do not apply AWS's list-vs-fault and validate-before-lookup rules, so a read that matches nothing becomes a fault, an invalid filter value is accepted, a combination gate is never evaluated, and an event read returns a record AWS does not report.

Cause 1 of 4: a DescribeSnapshots read that matches nothing answers 404 instead of 200 with an empty list.
AWS: Smithy DescribeSnapshots returns DescribeSnapshotsListMessage {Marker, Snapshots}. A list read whose filters match no snapshot is a successful empty read — HTTP 200 with an empty Snapshots element. The modeled faults (SnapshotNotFoundFault, CacheClusterNotFoundFault, both HTTP 404) are for a named snapshot or a named cluster that does not exist, not for an empty result set.
Emulator: HTTP 404 with no DescribeSnapshotsResponse body at all.
Why: a client that lists snapshots to decide whether any exist gets an exception instead of an empty page, so the ordinary 'nothing yet' case is indistinguishable from a real not-found and paginating loops break.
Fix: the DescribeSnapshots handler's read path — return 200 with an empty Snapshots list when the filters select nothing, and reserve the 404 faults for an explicitly named, nonexistent snapshot or cluster.
Traces: elasticache-describe-snapshots-source-validation-and-filtered-reads#task#1.

Cause 2 of 4: DescribeSnapshots does not refuse ReplicationGroupId and CacheClusterId together.
AWS: DescribeSnapshots models InvalidParameterCombinationException (awsQueryError code InvalidParameterCombination, HTTP 400); supplying both ReplicationGroupId and CacheClusterId is refused with the wholly fixed text 'Cannot specify both ReplicationGroupId and CacheClusterId', and the combination check runs BEFORE any resource lookup.
Emulator: HTTP 404 with Error.Code 'CacheClusterNotFound' and 'CacheCluster not found: <CacheClusterId>' — it skips the combination gate and goes straight to resolving the cluster id.
Why: the client is told a resource is missing when its request was malformed; it retries against a 'missing' cluster forever instead of fixing the mutually exclusive filters, and the status it sees (404) is not the modeled 400.
Fix: the DescribeSnapshots handler's input validation ordering — evaluate the ReplicationGroupId/CacheClusterId exclusivity gate and raise InvalidParameterCombinationException before the state lookup.
Traces: elasticache-describe-snapshots-source-validation-and-filtered-reads#task#4.

Cause 3 of 4: DescribeServerlessCacheSnapshots accepts an invalid SnapshotType instead of refusing it.
AWS: DescribeServerlessCacheSnapshots.SnapshotType is a plain String and the operation models InvalidParameterValueException (code InvalidParameterValue, HTTP 400). An unknown SnapshotType is refused with "Invalid SnapshotType Specified. If specified, should be either 'automated' or 'manual'" (Error.Type 'Sender').
Emulator: HTTP 200 — the filter value is accepted and a normal (empty) result is returned; no ErrorResponse is produced.
Why: a client with a typo or a stale filter value silently gets results it never asked for, so a misfiltered read looks like a correct empty read and the bug is invisible.
Fix: the DescribeServerlessCacheSnapshots handler's validation — reject SnapshotType values outside automated|manual with the modeled InvalidParameterValueException and AWS's text.
Traces: elasticache-describe-serverless-cache-snapshots-not-found-and-validation#task#3.

Cause 4 of 4: DescribeEvents reports a fabricated cache-parameter-group event where AWS reports none.
AWS: DescribeEvents returns EventsMessage {Marker, Events}; for the filtered window this fixture reads, the recorded AWS response carries an EMPTY Events element — recorded behavior only (the model does not enumerate which events exist). AWS does not surface a parameter-group creation record for this read.
Emulator: returns one Event (SourceType 'cache-parameter-group', Message 'Cache parameter group created', SourceIdentifier the group just created) that AWS does not report. Hypothesis: the emulator synthesizes an event row on every control-plane mutation and ignores the request's source/time filtering.
Why: a client that polls DescribeEvents to detect real cluster events sees emulator-only noise, and any assertion on an empty event window fails.
Fix: the DescribeEvents handler and the event store that feeds it — only publish the event kinds AWS publishes, and honour the request's SourceType/SourceIdentifier/time-window filters.
Traces: elasticache-cache-parameter-group-source-filter-and-event-validation#task#5.

No open issue owns these steps (replay reported owned_by empty and stub_path empty for all four clusters). 20261109T054811Z / 20261112T022229Z cover ElastiCache events NOT reaching an SNS topic, which is a different path from DescribeEvents' contents.

## Evidence
Replay of trace set simparity-wire-aws-v1, service=elasticache, all verdict=mismatch, owned_by empty, stub_path empty.

1) elasticache-describe-snapshots-source-validation-and-filtered-reads#task#1 DescribeSnapshots: expected_status 200, actual_status 404. detail: 'status: expected 200, got 404' and '$.DescribeSnapshotsResponse: missing (expected {DescribeSnapshotsResult: {Snapshots: ""}, ResponseMetadata: {RequestId: <shaped-not-asserted>}}, got None)'.
2) ...#task#4 DescribeSnapshots: expected_status 400, actual_status 404. Error.Code expected 'InvalidParameterCombination', got 'CacheClusterNotFound'; Error.Message expected 'Cannot specify both ReplicationGroupId and CacheClusterId', got 'CacheCluster not found: <requested CacheClusterId>'.
3) elasticache-describe-serverless-cache-snapshots-not-found-and-validation#task#3 DescribeServerlessCacheSnapshots: expected_status 400, actual_status 200. detail: 'status: expected 400, got 200' and '$.ErrorResponse: missing (expected {Error: {Type: "Sender", Code: "InvalidParameterValue", Message: "Invalid SnapshotType Specified. If specified, should be either \'automated\' or \'manual\'"}, RequestId: <shaped-not-asserted>}, got None)'.
4) elasticache-cache-parameter-group-source-filter-and-event-validation#task#5 DescribeEvents 200->200: '$.DescribeEventsResponse.DescribeEventsResult.Events: type_mismatch (expected "", got {Event: {SourceIdentifier: <the probe parameter group>, SourceType: "cache-parameter-group", Message: "Cache parameter group created", Date: <timestamp>}})'.

(The cluster id, group name, Date and RequestId above are per-run values; the requirements are the statuses, the error codes, the refusal texts and the empty Events/Snapshots elements.)

## Steps to Reproduce
1. Clone the repo, boot the emulator, replay the bound wire corpus.
2. Fixture elasticache-describe-snapshots-source-validation-and-filtered-reads, task#1: issue the fixture's filtered DescribeSnapshots read that matches no snapshot — expect HTTP 200 with an empty Snapshots element; the emulator answers 404 with no body.
3. Same fixture, task#4: DescribeSnapshots with BOTH ReplicationGroupId and CacheClusterId set — expect 400 InvalidParameterCombination 'Cannot specify both ReplicationGroupId and CacheClusterId'; the emulator answers 404 CacheClusterNotFound.
4. Fixture elasticache-describe-serverless-cache-snapshots-not-found-and-validation, task#3: DescribeServerlessCacheSnapshots with SnapshotType set to a value outside automated|manual — expect 400 InvalidParameterValue; the emulator answers 200.
5. Fixture elasticache-cache-parameter-group-source-filter-and-event-validation, task#5: create a cache parameter group then issue the fixture's DescribeEvents read — expect an empty Events element; the emulator returns a synthesized 'Cache parameter group created' event.
