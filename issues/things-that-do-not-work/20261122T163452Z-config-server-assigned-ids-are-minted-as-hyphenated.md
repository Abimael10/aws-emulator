Issue-Source: trace-sweep-divergence
Issue-Trace: config-list-evaluations-by-context-identifier
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: config-list-evaluations-by-context-identifier config-proactive-evaluation-lifecycle config-proactive-resource-evaluation-lifecycle config-resource-evaluation-context-grouping config-resource-evaluation-grouped-by-context config-start-resource-evaluation-smoke config-resource-tagging-and-compliance-summaries config-stored-query-expression-unicode-round-trip config-stored-query-unicode-expression-round-trip config-stored-query-unicode-and-emoji-round-trip
Issue-Shape-Id: com.amazonaws.configservice#StarlingDoveService
Issue-Key: config|StartResourceEvaluation|2xx->2xx|$.ResourceEvaluationId
Issue-Key: config|PutStoredQuery|2xx->2xx|$.QueryArn
Issue-Key: config|GetStoredQuery|2xx->2xx|$.StoredQuery.QueryId
Issue-Key: config|GetStoredQuery|2xx->2xx|$.StoredQuery.QueryArn
Issue-Commit: cd255d70c517b248e494c18d4f6f889fcbe65829

# config server-assigned ids are minted as hyphenated UUIDs, a form Config never returns (4 causes)

> _Note: this issue was identified against the code base at commit `cd255d70c517b248e494c18d4f6f889fcbe65829`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartResourceEvaluation

## Description
SHARED FAULT (4 causes, all config): the emulator mints the server-assigned opaque identifiers of stored queries and resource evaluations as a canonical hyphenated 36-character UUID (`crate::state::uuid_id()`, reached through `mint_stored_query_id` and `mint_resource_evaluation_id`), and embeds that UUID in the stored-query ARN's resource-id segment. The recorded Config responses carry a compact opaque token in those members, so the wire FORM of every one of these identifiers diverges on every call. The corpus does not assert these per-run values (they are marked `<shaped-not-asserted>`); it asserts their SHAPE, and the UUID form is rejected on all 13 affected steps. This is not 'a generated id differs per run': the SAME replay accepts this service's other server-assigned ids, which are minted by `crate::state::opaque_id` in base-36 form (e.g. `ConformancePackId` = `conformance-pack-` + 8 base-36 characters, asserted and passing in config-conformance-pack-create-inspect-delete#task#3), so what is rejected is specifically the hyphenated-UUID shape. Hypothesis: Config mints these ids as one compact opaque base-36-style token with no hyphen-delimited groups, the same way the already-passing ids in this crate are minted; the model bounds but does not spell the alphabet.

Cause 1 of 4 — StartResourceEvaluation.ResourceEvaluationId (fixture config-list-evaluations-by-context-identifier, step task#1).
AWS: recorded behavior only. Smithy: StartResourceEvaluationResponse$ResourceEvaluationId -> ResourceEvaluationId @length min 1 max 128, NO @pattern; doc 'A unique ResourceEvaluationId that is associated with a single execution.' The recorded response's value is redacted and only its shape is asserted, and that shape is not a hyphenated UUID; the 128-character ceiling is far wider than a UUID's 36. The recorded status is 200 and no error is involved, so there is no error code or message requirement here.
Emulator: answers 200 with `ResourceEvaluationId` = a canonical hyphenated UUID (replay actual: 'dd98efc0-3af9-46e7-be63-0af841178984'), per `mint_resource_evaluation_id`, whose own comment states it reuses the stored-query UUID form because the model gives no pattern.
Why: a client (or a later recorded step) that round-trips this id through ListResourceEvaluations / GetResourceEvaluationSummary / the context-grouping paths carries an identifier whose form no real Config account issues, so any client-side validation, log scraping, fixture or snapshot keyed on Config's id form fails against the emulator while passing against AWS.
Fix: the identifier minting in the service state (`mint_resource_evaluation_id`), not the StartResourceEvaluation handler's logic — every read path already reports the persisted id, so changing the mint fixes all of them.
Traces: config-list-evaluations-by-context-identifier#task#1, plus 8 further affected steps in the same cluster (config-list-evaluations-by-context-identifier#task#2, config-proactive-evaluation-lifecycle#task#1, config-proactive-resource-evaluation-lifecycle#task#1, config-resource-evaluation-context-grouping#task#1 and #task#2, config-resource-evaluation-grouped-by-context#task#1 and #task#3, config-start-resource-evaluation-smoke#task#1). Code: services/service-com-amazonaws-configservice-starlingdoveservice/src/state.rs around line 4244.

Cause 2 of 4 — PutStoredQuery.QueryArn (fixture config-resource-tagging-and-compliance-summaries, step setup#1).
AWS: Smithy: PutStoredQueryResponse$QueryArn -> QueryArn @length min 1 max 500, @pattern '^arn:aws[a-z\-]*:config:[a-z\-\d]+:\d+:stored-query/[a-zA-Z0-9-_]+/query-[a-zA-Z\d-_/]+$'; doc 'Amazon Resource Name (ARN) of the query. For example, arn:partition:service:region:account-id:resource-type/resource-name/resource-id.' So the ARN's resource-id segment is `query-` followed by the query's assigned id, and the recorded fixture binds that whole ARN as the variable `{{tagged_query_arn}}` the later tagging/compliance steps key on. Recorded status 200, no error involved.
Emulator: answers 200 with QueryArn 'arn:aws:config:us-west-2:000000000000:stored-query/<name>/query-<hyphenated UUID>' (replay actual: '...stored-query/sim2real-probe-config-tagged-query-4569b86a/query-548a10c8-847a-4bb8-9284-495005ff0552'), i.e. `stored_query_arn()` composed over the UUID from `mint_stored_query_id`, which does not satisfy the fixture's bound ARN for that step. Partition, service, region and account segments all agree; only the id segment's form diverges.
Why: the stored-query ARN is the resource identity the tagging surface (TagResource / UntagResource / ListTagsForResource) and the compliance-summary paths address, so a client that captured the ARN from AWS and replays it, or that validates the ARN's resource-id form, addresses an ARN shape the service never issues.
Fix: same state-level minting (`mint_stored_query_id`); `stored_query_arn` itself is correct — it already spells the `stored-query/<name>/query-<id>` form the pattern requires and should keep deriving the ARN from the persisted id.
Traces: config-resource-tagging-and-compliance-summaries#setup#1. Code: services/service-com-amazonaws-configservice-starlingdoveservice/src/ops/put_stored_query.rs lines 240-241; state.rs lines 2921-2938.

Cause 3 of 4 — GetStoredQuery.StoredQuery.QueryId (fixture config-stored-query-expression-unicode-round-trip, step task#2).
AWS: Smithy: StoredQuery$QueryId -> QueryId @length min 1 max 36, @pattern '^\S+$'; doc 'The ID of the query.' Together with QueryArn's doc ('resource-type/resource-name/resource-id') the id is the ARN's resource-id segment. Recorded behavior: the value is redacted and only its shape asserted, and that shape is not a hyphenated UUID. Recorded status 200, no error involved.
Emulator: answers 200 with StoredQuery.QueryId = a bare hyphenated UUID (replay actual: '768b6ac1-945a-4b0e-bf9c-a2d579582826'), which also exactly fills the 36-character ceiling, leaving no room for the `query-` prefix the ARN's resource-id segment carries — so the emulator's QueryId is additionally NOT the ARN's resource-id segment (the ARN segment is 'query-' + that UUID).
Why: a client that reconstructs the ARN from QueryName + QueryId, or that matches a QueryId read back from GetStoredQuery/ListStoredQueries against the ARN it holds, gets a value that does not correspond, and any id-form validation fails.
Fix: same state-level minting; keep QueryId and the ARN's resource-id segment one and the same string.
Traces: config-stored-query-expression-unicode-round-trip#task#2, plus the cluster's other affected step config-stored-query-unicode-expression-round-trip#task#2. Code: state.rs lines 2929-2938.

Cause 4 of 4 — GetStoredQuery.StoredQuery.QueryArn AND .QueryId together (fixture config-stored-query-unicode-and-emoji-round-trip, step task#2).
AWS: as in causes 2 and 3 (Smithy StoredQuery$QueryArn pattern '^arn:aws[a-z\-]*:config:[a-z\-\d]+:\d+:stored-query/[a-zA-Z0-9-_]+/query-[a-zA-Z\d-_/]+$'; StoredQuery$QueryId @length 1..36 @pattern '^\S+$'). This fixture shape-asserts BOTH members of the read-back stored query, so it shows that the divergence is in the id-bearing segments of the read path, not only in the create response. Recorded status 200.
Emulator: answers 200 with QueryId '8dfe5033-529d-436c-b447-d2d2f2f90552' and QueryArn 'arn:aws:config:us-west-2:000000000000:stored-query/<name>/query-8dfe5033-529d-436c-b447-d2d2f2f90552' — both rejected by the shape assertion.
Why: the whole stored-query identity a caller reads back (both the id and the ARN) is in a form AWS does not issue, so nothing downstream that depends on Config's id form round-trips.
Fix: same state-level minting; the GetStoredQuery handler and `stored_query_arn` only report what the mint produced.
Traces: config-stored-query-unicode-and-emoji-round-trip#task#2. Code: state.rs lines 2913-2938.

Not covered elsewhere: no open issue carries any of these replay keys. The only related open issue is '20261109T154050Z-cloudformation-config-2-net-new-causes-aws-config.md' (AWS::Config::StoredQuery is not a registered CloudFormation resource type), which is a different surface and does not cover the identifier form. The 5th divergence this replay reported (DescribeConformancePackStatus reporting CREATE_COMPLETE) is already owned by '20261112T185622Z-config-describeconformancepackstatus-always-reports-create-complete-never-the.md' and is NOT part of this issue.

## Evidence
Replay of trace set simparity-wire-aws-v1 (56 fixtures, 282 steps, 265 passed, 17 mismatched, score 94.0%, emulator_up=true). Four net-new mismatch clusters, all service=config, all expected_status=200 / actual_status=200 — the divergence is purely the FORM of server-assigned identifier members:

1) divergence_key 'service=config|StartResourceEvaluation|status=200->200|keys=$.ResourceEvaluationId' (affected_count 9), fixture config-list-evaluations-by-context-identifier step task#1:
   detail: '$.ResourceEvaluationId: value_mismatch (expected \'<shaped-not-asserted>\', got \'dd98efc0-3af9-46e7-be63-0af841178984\')'
   Expected: an opaque ResourceEvaluationId of the recorded shape (value not asserted, shape asserted; Smithy @length 1..128, no pattern).
   Actual: a canonical hyphenated 36-char UUID.

2) divergence_key 'service=config|PutStoredQuery|status=200->200|keys=$.QueryArn' (affected_count 1), fixture config-resource-tagging-and-compliance-summaries step setup#1:
   detail: '$.QueryArn: value_mismatch (expected \'{{tagged_query_arn}}\', got \'arn:aws:config:us-west-2:000000000000:stored-query/sim2real-probe-config-tagged-query-4569b86a/query-548a10c8-847a-4bb8-9284-495005ff0552\')'
   Expected: the ARN the fixture binds for that stored query, of form arn:aws:config:<region>:<account>:stored-query/<QueryName>/query-<assigned id>.
   Actual: the same ARN with a hyphenated UUID in the resource-id segment. Only the id segment diverges — partition/service/region/account/name all agree.

3) divergence_key 'service=config|GetStoredQuery|status=200->200|keys=$.StoredQuery.QueryId' (affected_count 2), fixture config-stored-query-expression-unicode-round-trip step task#2 (also config-stored-query-unicode-expression-round-trip#task#2):
   detail: '$.StoredQuery.QueryId: value_mismatch (expected \'<shaped-not-asserted>\', got \'768b6ac1-945a-4b0e-bf9c-a2d579582826\')'

4) divergence_key 'service=config|GetStoredQuery|status=200->200|keys=$.StoredQuery.QueryArn,$.StoredQuery.QueryId' (affected_count 1), fixture config-stored-query-unicode-and-emoji-round-trip step task#2:
   detail: '$.StoredQuery.QueryArn: value_mismatch (expected \'<shaped-not-asserted>\', got \'arn:aws:config:us-west-2:000000000000:stored-query/sim2real-conv-config-sque-bb940ee9/query-8dfe5033-529d-436c-b447-d2d2f2f90552\')' and '$.StoredQuery.QueryId: value_mismatch (expected \'<shaped-not-asserted>\', got \'8dfe5033-529d-436c-b447-d2d2f2f90552\')'

Control observation in the same replay run: the config ids minted by `crate::state::opaque_id` are shape-asserted and PASS — config-conformance-pack-create-inspect-delete#task#3 accepts ConformancePackId 'conformance-pack-0dyztfeb' and its ARN (that step's only reported divergence is ConformancePackState, owned by an existing issue). So the shape assertion is not rejecting 'a generated id'; it rejects the hyphenated-UUID form.

Source of the form, in the checkout:
  state.rs:2936 `pub fn mint_stored_query_id() -> String { uuid_id() }` (comment: 'a uuid in the canonical hyphenated form ... exactly the 36 characters QueryId's 1..36 length allows')
  state.rs:4244 `pub fn mint_resource_evaluation_id() -> String { uuid_id() }` (comment: 'the model gives ResourceEvaluationId no pattern (only a 1..128 length) ... so the id takes the same canonical hyphenated uuid form a stored query's does')
  state.rs:2921 `stored_query_arn()` composes 'stored-query/{name}/query-{id}' over that UUID
  state.rs:2910/3008/3034/3491 the same crate mints recorder, conformance-pack, snapshot and remediation ids with `opaque_id(...)` base-36 tokens instead.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it, then replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture 'config-list-evaluations-by-context-identifier' step 'task#1' — call StartResourceEvaluation (EvaluationMode + ResourceDetails) and inspect $.ResourceEvaluationId in the 200 response: it is a canonical hyphenated UUID, which the fixture's shape assertion for that member rejects. Same on the other 8 steps of the cluster (config-proactive-evaluation-lifecycle#task#1, config-start-resource-evaluation-smoke#task#1, ...).
3. Cause 2: replay fixture 'config-resource-tagging-and-compliance-summaries' step 'setup#1' — call PutStoredQuery with a QueryName and a valid Config SELECT expression and inspect $.QueryArn: the resource-id segment is 'query-' + a hyphenated UUID, so it does not match the ARN the fixture binds as {{tagged_query_arn}}.
4. Causes 3 and 4: replay fixtures 'config-stored-query-expression-unicode-round-trip' step 'task#2' and 'config-stored-query-unicode-and-emoji-round-trip' step 'task#2' — PutStoredQuery, then GetStoredQuery(QueryName) and inspect $.StoredQuery.QueryId (and $.StoredQuery.QueryArn): the id is a bare hyphenated UUID and the ARN embeds it, both rejected by the shape assertions. Note also that QueryId is NOT equal to the ARN's resource-id segment ('query-' + QueryId), although QueryArn's doc defines that segment as the resource id.
5. Control: replay 'config-conformance-pack-create-inspect-delete' step 'task#3' and observe that the opaque base-36 ConformancePackId/ARN pass the same kind of shape assertion, confirming the UUID form is what diverges.
6. After the fix, re-replay all four representative steps listed above (the issue's Issue-Trace/Issue-Step plus the three trace_ids bindings) and confirm every affected step in each cluster passes.
