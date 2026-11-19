Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__dynamodb__lambda.deep-appsync-1-appsync-conflict-lambda-handler-reject-surfaces-conflictunhandled-and-item-unchanged
Issue-Key: appsync__dynamodb__lambda.deep-appsync-1-appsync-conflict-lambda-handler-reject-surfaces-conflictunhandled-and-item-unchanged|run#18|AssertionError|assert seeded["_version"] == 1, f"a versioned create must stamp _version 1: {seeded!r}"
Issue-Commit: 15eaad9fa74cff8f1dc14d7f5abd0b99014b7462

# appsync__dynamodb__lambda: a VERSIONED AMAZON_DYNAMODB data source never stamps _version/_lastChangedAt, so conflict detection can never engage

> _Note: this issue was identified against the code base at commit `15eaad9fa74cff8f1dc14d7f5abd0b99014b7462`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GraphQL data plane (versioned AMAZON_DYNAMODB data source)

## Description
One net-new root cause in the AppSync GraphQL data plane. Cause 1 of 1: a write through a data source created with dynamodbConfig.versioned = true does not stamp the system version attributes, so every versioned row comes back with _version and _lastChangedAt null.

AWS: when a resolver's data source is an AMAZON_DYNAMODB data source with dynamodbConfig.versioned = true (and a deltaSyncConfig), AppSync owns two system attributes on every item it writes: _version, a monotonically increasing integer starting at 1 on the item's first write, and _lastChangedAt, the write's epoch-milliseconds timestamp. A PutItem through such a resolver returns the stored item with _version = 1; each subsequent accepted UpdateItem advances it by one. The resolver's request template supplies the version the client believes is current ("_version": <expected>) and AppSync compares it with the stored _version: equal means apply, different means conflict, which is where conflictDetection VERSION and the syncConfig conflictHandler (here LAMBDA) are invoked. Recorded behavior only: the Smithy model carries the configuration (SyncConfig.conflictDetection = VERSION, DynamodbDataSourceConfig.versioned, deltaSyncConfig) but not the stamping rule, which is AppSync's documented conflict-detection/sync behavior. The variable parts are the integer (1 for a create, +1 per accepted write) and the timestamp; nothing else.

Emulator: the versioned createPost mutation answers 200 with no GraphQL errors, but the returned item is {'id': ..., 'title': ..., 'resolvedBy': None, '_version': None, '_lastChangedAt': None} — both system attributes are null, i.e. the versioned data source behaves like an unversioned one. (The id/title values quoted are this run's; the requirement is _version = 1 and a non-null _lastChangedAt.)

Why: with no stored _version there is no version to compare against, so conflict detection can never fire: a stale write is applied instead of being refused, the LAMBDA conflict handler is never invoked, ConflictUnhandled / ConflictConditionalCheck are never surfaced, and an AppSync/Amplify DataStore client that reads _version to drive optimistic concurrency, delta sync and offline reconciliation gets null and cannot build its next request. The entire conflict-resolution feature is unobservable, and a client whose correctness depends on losing a stale write silently wins instead.

Fix: the GraphQL data plane's versioned-DynamoDB write path (the request-mapping execution for PutItem/UpdateItem/DeleteItem against a data source whose dynamodbConfig.versioned is true) must maintain the two system attributes — set _version to 1 and _lastChangedAt to the write time on create, require the request's "_version" to match the stored one, and increment _version on each accepted write — and must route a mismatch into the configured conflictHandler before answering. The stamped attributes must be in the item persisted to DynamoDB as well as in the resolver's result, since both are read back.

Traces: appsync__dynamodb__lambda.deep-appsync-1-appsync-conflict-lambda-handler-reject-surfaces-conflictunhandled-and-item-unchanged. NOTE: open issue 20261114T003845Z-appsync-dynamodb-lambda-appsync-conflict-detection-and-per.md covers conflict detection not settling at all on an earlier form of this path and open issue 20261117T120125Z-appsync-dynamodb-the-vtl-engine-has-no-util.md covers the missing $util.dynamodb helpers; neither carries this check's replay key — the VTL template now executes and the mutation now returns a row, and what remains is the missing version stamping.

## Evidence
Behavioral suite replay against the emulator built from this checkout.

[mismatch] appsync__dynamodb__lambda.deep-appsync-1-appsync-conflict-lambda-handler-reject-surfaces-conflictunhandled-and-item-unchanged
  File "check_appsync__dynamodb__lambda_deep_appsync_1_...py", line 429, in run
    assert seeded["_version"] == 1, f"a versioned create must stamp _version 1: {seeded!r}"
  AssertionError: a versioned create must stamp _version 1: {'id': 'post-debb97db9f50', 'title': 'seed-c45cc61f66c8', 'resolvedBy': None, '_version': None, '_lastChangedAt': None}

Setup that produced it: CreateDataSource(type='AMAZON_DYNAMODB', dynamodbConfig={'tableName': <base>, 'awsRegion': <region>, 'versioned': True, 'deltaSyncConfig': {'baseTableTTL': 43200, 'deltaSyncTableName': <delta>, 'deltaSyncTableTTL': 60}}), and CreateResolver for Mutation.createPost/Mutation.setTitle with syncConfig {'conflictHandler': 'LAMBDA', 'conflictDetection': 'VERSION', 'lambdaConflictHandlerConfig': {'lambdaConflictHandlerArn': <fn>}}; request template operation PutItem with "_version": 1. The mutation is served 200 with no errors, so the resolver runs — only the version attributes are absent. Everything the check asserts after this point (v2 advance, ConflictUnhandled 'Conflict resolver rejects mutation.', the handler's existingItem/newItem, the RESOLVE advancing to v3) is unreachable while _version is null.

## Steps to Reproduce
1. Clone the repo, build and boot the emulator from this branch, and replay the simparity-behavioral-aws-v1 suite filtered to appsync__dynamodb__lambda.
2. Create a base DynamoDB table and a delta table, a service role for appsync.amazonaws.com with the DynamoDB write/read actions on both, and a GraphQL API with a schema exposing createPost(id, title) / setTitle(id, value, expectedVersion).
3. CreateDataSource(type='AMAZON_DYNAMODB', dynamodbConfig.versioned=true with a deltaSyncConfig naming the delta table).
4. CreateResolver for Mutation.createPost (PutItem, "_version": 1) and Mutation.setTitle (UpdateItem, "_version": $expectedVersion) with syncConfig conflictDetection=VERSION, conflictHandler=LAMBDA.
5. POST the createPost mutation to the API's GRAPHQL uri with an API key, selecting { id title _version _lastChangedAt }. Observe _version and _lastChangedAt are null; AWS returns _version 1 and a millisecond timestamp.
6. Replay appsync__dynamodb__lambda.deep-appsync-1-appsync-conflict-lambda-handler-reject-surfaces-conflictunhandled-and-item-unchanged to confirm.
