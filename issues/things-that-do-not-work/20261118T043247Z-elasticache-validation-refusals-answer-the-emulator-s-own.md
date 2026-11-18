Issue-Source: trace-sweep-divergence
Issue-Trace: elasticache-cache-parameter-group-source-filter-and-event-validation
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticache-cache-parameter-group-source-filter-and-event-validation elasticache-cache-parameter-group-workflow elasticache-describe-serverless-cache-snapshots-not-found-and-validation elasticache-describe-snapshots-source-validation-and-filtered-reads elasticache-describe-update-actions-and-service-updates-validation-errors
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache|DescribeCacheParameters|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: elasticache|DescribeEvents|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: elasticache|DescribeEvents|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: elasticache|ResetCacheParameterGroup|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: elasticache|DeleteCacheParameterGroup|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: elasticache|DescribeServerlessCacheSnapshots|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: elasticache|DescribeSnapshots|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: elasticache|DescribeServiceUpdates|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: elasticache|DescribeServiceUpdates|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: elasticache|DescribeUpdateActions|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: elasticache|DescribeUpdateActions|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 5c103ae721112cfc59443e8c0b52708204a7cb08

# ElastiCache validation refusals answer the emulator's own prose and the wrong error code instead of AWS's text

> _Note: this issue was identified against the code base at commit `5c103ae721112cfc59443e8c0b52708204a7cb08`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCacheParameters

## Description
Eight net-new causes, one shared fault: these refusals reach the client with the right HTTP status but the emulator's own wording — and in three of them the wrong Error/Code — because the refusal is produced by a generic constraint/label-based validator rather than by the operation's AWS-specific refusal text. Each cause is a distinct operation and fixture step and must be closed separately.

Cause 1 of 8: DescribeCacheParameters rejects an unknown Source with the wrong message.
AWS: Smithy DescribeCacheParameters.Source is a plain String (not an enum); the modeled refusal is InvalidParameterValueException (awsQueryError code InvalidParameterValue, HTTP 400). AWS words it 'Invalid Source: <Source>. Allowed values are: engine-default, system, user' — fixed text 'Invalid Source: ' + the Source value taken from the request + '. Allowed values are: engine-default, system, user'.
Emulator: HTTP 400 with code InvalidParameterValue (correct) but message 'Invalid value <Source> for Source: must be one of user | system | engine-default.' — its own template and its own ordering of the allowed values.
Why: a client that matches on the refusal text, or that parses the allowed-value list out of the message, does not recognise the emulator's wording.
Fix: the DescribeCacheParameters handler's Source validation — emit AWS's message template.
Traces: elasticache-cache-parameter-group-source-filter-and-event-validation#task#3.

Cause 2 of 8: DescribeEvents rejects an unknown SourceType with the wrong error code AND message.
AWS: DescribeEvents.SourceType targets enum SourceType, and the operation models InvalidParameterValueException (awsQueryError code InvalidParameterValue, HTTP 400). On the wire AWS answers Error.Code 'InvalidParameterValue' with the wholly fixed text 'Invalid event source type. Valid types are cache-cluster,cache-parameter-group,cache-security-group,cache-subnet-group,replication-group,user,user-group,serverless-cache,serverless-cache-snapshot' (no request value is interpolated).
Emulator: HTTP 400 with Error.Code 'ValidationError' and the generic smithy-constraint message "1 validation error detected: Value '<value>' at 'sourceType' failed to satisfy constraint: Member must satisfy enum value set: [...]".
Why: ValidationError is not in DescribeEvents' modeled error set, so a client that catches InvalidParameterValueException never catches this refusal and surfaces it as an unmodelled failure.
Fix: the DescribeEvents handler's SourceType validation — raise the modeled InvalidParameterValueException with AWS's fixed text instead of letting the generic enum validator answer.
Traces: elasticache-cache-parameter-group-source-filter-and-event-validation#task#6.

Cause 3 of 8: ResetCacheParameterGroup's invalid-identifier refusal is the emulator's own label prose.
AWS: HTTP 400 InvalidParameterValue with 'The parameter CacheParameterGroupName is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.' — the variable part is the member name ('CacheParameterGroupName'), the rest is fixed text.
Emulator: same status/code but 'Cache Parameter Group Name specified is not a valid identifier. Identifiers must begin with a letter; ...' — it substitutes a human label for the modeled member name and uses a '<Label> specified is not a valid identifier' opening AWS never emits.
Why: a client that matches AWS's 'The parameter <Member> is not a valid identifier' form, or that extracts the offending member name from the message, gets neither.
Fix: the shared identifier-validation helper used by the cache-parameter-group handlers — interpolate the modeled member name into AWS's 'The parameter <Member> is not a valid identifier. ...' template.
Traces: elasticache-cache-parameter-group-workflow#task#12.

Cause 4 of 8: DeleteCacheParameterGroup's invalid-identifier refusal has the same wrong text.
AWS: identical to cause 3 — 'The parameter CacheParameterGroupName is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.'
Emulator: 'Cache Parameter Group Name specified is not a valid identifier. ...'.
Why: same as cause 3.
Fix: the DeleteCacheParameterGroup handler's identifier validation (same helper as cause 3, but it is a separate step and must be re-replayed).
Traces: elasticache-cache-parameter-group-workflow#task#13.

Cause 5 of 8: DescribeServerlessCacheSnapshots' not-found message is the emulator's own sentence.
AWS: the modeled fault is ServerlessCacheSnapshotNotFoundFault (HTTP 404) and AWS words it 'Serverless Cache Snapshot with name <ServerlessCacheSnapshotName> not found' — fixed text 'Serverless Cache Snapshot with name ' + the requested snapshot name + ' not found'.
Emulator: HTTP 404 with the right code but 'Serverless cache snapshot with specified name does not exists.', which drops the requested name entirely.
Why: a client (or an operator reading a log) cannot tell WHICH snapshot was not found, and text matching fails.
Fix: the DescribeServerlessCacheSnapshots handler's not-found path — build the message from the requested name.
Traces: elasticache-describe-serverless-cache-snapshots-not-found-and-validation#task#2.

Cause 6 of 8: DescribeSnapshots rejects an unknown SnapshotSource with the wrong message.
AWS: DescribeSnapshots.SnapshotSource is a plain String; the refusal is InvalidParameterValueException (code InvalidParameterValue, HTTP 400) with the wholly fixed text "Invalid snapshot source specified. If specified, should be either 'automated' or 'manual'".
Emulator: HTTP 400 InvalidParameterValue but 'Invalid value <value> for SnapshotSource: must be one of system | automated | user | manual.' — its own template, and it advertises two values ('system', 'user') AWS does not accept here.
Why: the message tells a client that 'system'/'user' are valid sources when AWS accepts only 'automated' or 'manual', so a client that retries on the advertised values is refused again.
Fix: the DescribeSnapshots handler's SnapshotSource validation.
Traces: elasticache-describe-snapshots-source-validation-and-filtered-reads#task#3.

Cause 7 of 8: DescribeServiceUpdates rejects an unknown ServiceUpdateStatus with the wrong code and message.
AWS: ServiceUpdateStatus targets ServiceUpdateStatusList (enum members, length 0..3) and the operation models InvalidParameterValueException (code InvalidParameterValue, HTTP 400); AWS answers 'The input service update status <value> is not valid.' — fixed text around the rejected value taken from the request.
Emulator: HTTP 400 with Error.Code 'ValidationError' and "1 validation error detected: Value '<value>' at 'serviceUpdateStatus.1.member' failed to satisfy constraint: Member must satisfy enum value set: [available, cancelled, expired]".
Why: ValidationError is not a modeled error of DescribeServiceUpdates, so the client's InvalidParameterValueException handler is bypassed.
Fix: the DescribeServiceUpdates handler's ServiceUpdateStatus validation — raise the modeled fault with AWS's text instead of the generic enum validator.
Traces: elasticache-describe-update-actions-and-service-updates-validation-errors#task#1.

Cause 8 of 8: DescribeUpdateActions answers an enum ValidationError where AWS refuses the parameter COMBINATION.
AWS: DescribeUpdateActions models InvalidParameterCombinationException (awsQueryError code InvalidParameterCombination, HTTP 400). When UpdateActionStatus is supplied without ServiceUpdateName, AWS refuses on the combination first, with the wholly fixed text 'Update action status needs to be provided together with service update name.'
Emulator: HTTP 400 with Error.Code 'ValidationError' and "1 validation error detected: Value '<value>' at 'updateActionStatus.1.member' failed to satisfy constraint: Member must satisfy enum value set: [...]" — it validates the member value and never applies the combination rule.
Why: the client is told the status value is malformed when the real requirement is that ServiceUpdateName must accompany it, and the code it branches on (InvalidParameterCombination) never arrives.
Fix: the DescribeUpdateActions handler's validation ordering — apply the UpdateActionStatus/ServiceUpdateName combination gate, raising InvalidParameterCombinationException, before any per-member enum check.
Traces: elasticache-describe-update-actions-and-service-updates-validation-errors#task#2.

Related open issues that do NOT cover these steps: 20261102T083323Z-elasticache-refusals-carry-the-wrong-error-code-and, 20261109T151249Z-elasticache-refusal-messages-are-the-emulator-s-own--r2, 20261117T114712Z-elasticache-3-net-new-refusal-causes-createuser-pattern and 20261118T004803Z-elasticache-2-net-new-causes-the-invalid-identifier. They cover other operations' refusals; replay reported owned_by empty for all eight clusters here, so none of these steps is carried by an existing replay key.

## Evidence
Replay of trace set simparity-wire-aws-v1, all verdict=mismatch, service=elasticache, stub_path empty, owned_by empty.

1) elasticache-cache-parameter-group-source-filter-and-event-validation#task#3 DescribeCacheParameters 400->400: $.ErrorResponse.Error.Message value_mismatch — expected 'Invalid Source: notarealsource. Allowed values are: engine-default, system, user', got 'Invalid value notarealsource for Source: must be one of user | system | engine-default.'
2) ...#task#6 DescribeEvents 400->400: Error.Code expected 'InvalidParameterValue', got 'ValidationError'; Error.Message expected 'Invalid event source type. Valid types are cache-cluster,cache-parameter-group,cache-security-group,cache-subnet-group,replication-group,user,user-group,serverless-cache,serverless-cache-snapshot', got "1 validation error detected: Value 'notarealtype' at 'sourceType' failed to satisfy constraint: Member must satisfy enum value set: [...]".
3) elasticache-cache-parameter-group-workflow#task#12 ResetCacheParameterGroup 400->400: expected 'The parameter CacheParameterGroupName is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.', got 'Cache Parameter Group Name specified is not a valid identifier. Identifiers must begin with a letter; ...'.
4) elasticache-cache-parameter-group-workflow#task#13 DeleteCacheParameterGroup 400->400: same expected/actual pair as (3).
5) elasticache-describe-serverless-cache-snapshots-not-found-and-validation#task#2 DescribeServerlessCacheSnapshots 404->404: expected 'Serverless Cache Snapshot with name <requested-name> not found', got 'Serverless cache snapshot with specified name does not exists.'
6) elasticache-describe-snapshots-source-validation-and-filtered-reads#task#3 DescribeSnapshots 400->400: expected "Invalid snapshot source specified. If specified, should be either 'automated' or 'manual'", got 'Invalid value notarealsource for SnapshotSource: must be one of system | automated | user | manual.'
7) elasticache-describe-update-actions-and-service-updates-validation-errors#task#1 DescribeServiceUpdates 400->400: Error.Code expected 'InvalidParameterValue', got 'ValidationError'; Message expected 'The input service update status notarealstatus is not valid.', got "1 validation error detected: Value 'notarealstatus' at 'serviceUpdateStatus.1.member' failed to satisfy constraint: Member must satisfy enum value set: [available, cancelled, expired]".
8) ...#task#2 DescribeUpdateActions 400->400: Error.Code expected 'InvalidParameterCombination', got 'ValidationError'; Message expected 'Update action status needs to be provided together with service update name.', got "1 validation error detected: Value 'notarealstatus' at 'updateActionStatus.1.member' failed to satisfy constraint: Member must satisfy enum value set: [...]".

The rejected values shown ('notarealsource', 'notarealtype', 'notarealstatus', the probe snapshot name) are the fixtures' request values; the requirement is the message TEMPLATE and the error code, not those literals.

## Steps to Reproduce
1. Clone the repo, boot the emulator, replay the bound wire corpus.
2. DescribeCacheParameters with Source set to an unknown value (fixture elasticache-cache-parameter-group-source-filter-and-event-validation, task#3) and compare Error.Message.
3. DescribeEvents with SourceType set to an unknown value (same fixture, task#6) and compare Error.Code and Error.Message.
4. ResetCacheParameterGroup / DeleteCacheParameterGroup with a malformed CacheParameterGroupName (fixture elasticache-cache-parameter-group-workflow, task#12 and task#13) and compare Error.Message.
5. DescribeServerlessCacheSnapshots for a snapshot name that does not exist (fixture elasticache-describe-serverless-cache-snapshots-not-found-and-validation, task#2).
6. DescribeSnapshots with an unknown SnapshotSource (fixture elasticache-describe-snapshots-source-validation-and-filtered-reads, task#3).
7. DescribeServiceUpdates with an unknown ServiceUpdateStatus, and DescribeUpdateActions with UpdateActionStatus supplied but no ServiceUpdateName (fixture elasticache-describe-update-actions-and-service-updates-validation-errors, task#1 and task#2).
