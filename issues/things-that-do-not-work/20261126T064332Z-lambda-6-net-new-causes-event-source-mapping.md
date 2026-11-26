Issue-Source: trace-sweep-divergence
Issue-Trace: lambda-esm-filter-criteria-invalid-pattern-rejected
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lambda-esm-filter-criteria-invalid-pattern-rejected lambda-esm-scaling-config-roundtrip lambda-esm-sqs-fifo-batch-size-above-ten-rejected lambda-list-event-source-mappings-params-and-pagination lambda-concurrency-qualified-and-missing-function-errors lambda-lvau-traj-env-cleared-with-empty-map
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda|CreateEventSourceMapping|2xx->2xx|$.State
Issue-Key: lambda|GetEventSourceMapping|2xx->2xx|$.State
Issue-Key: lambda|ListEventSourceMappings|2xx->2xx|$.EventSourceMappings[*]
Issue-Key: lambda|GetFunctionConcurrency|4xx->4xx|$.Message
Issue-Key: lambda|UpdateFunctionConfiguration|2xx->2xx|$.Environment
Issue-Key: lambda|GetFunctionConfiguration|2xx->2xx|$.Environment
Issue-Commit: 08fcd12aac9b5a9e1aa801393cd42766d9a05ade

# lambda: 6 net-new causes — event-source-mapping state never settles, list rows diverge, empty Environment echoed back, and a $LATEST-qualified not-found ARN

> _Note: this issue was identified against the code base at commit `08fcd12aac9b5a9e1aa801393cd42766d9a05ade`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
Six net-new wire divergences on the lambda service, all payload-fidelity faults in the event-source-mapping state machine, the event-source-mapping list projection, the UpdateFunctionConfiguration environment handling, and one ResourceNotFoundException message. Every status line matches; what diverges is the body. Six causes, all of which must be closed before this issue is submitted.

Cause 1 of 6: CreateEventSourceMapping reports State 'Creating' for a mapping created with Enabled=false.
AWS: Smithy: CreateEventSourceMapping input member Enabled (boolean); output shape EventSourceMappingConfiguration, member State documented as 'The state of the event source mapping. It can be one of the following: Creating, Enabling, Enabled, Disabling, Disabled, Updating, or Deleting.' The recording shows that a create carrying Enabled=false answers HTTP 202 with State already 'Disabled' — a mapping that is not to be polled has no transient creation phase to pass through, so the terminal state is published in the create response itself.
Emulator: answers 202 with $.State = 'Creating' instead of 'Disabled'.
Why: a client that creates a disabled mapping and branches on State (or polls until State leaves a transient value) sees a transient state that, per Cause 2, never advances. Deliberately-parked mappings read as still being set up, so provisioning code either loops to its timeout or mis-classifies a correctly disabled mapping as not ready.
Fix: the CreateEventSourceMapping handler's state derivation — the created mapping's State must be computed from the requested Enabled flag (false -> 'Disabled') rather than initialised to a fixed 'Creating'.
Traces: lambda-esm-filter-criteria-invalid-pattern-rejected#task#9 (cluster also covers lambda-esm-scaling-config-roundtrip#task#7 and lambda-esm-sqs-fifo-batch-size-above-ten-rejected#task#10).

Cause 2 of 6: GetEventSourceMapping read-back of a disabled mapping is still 'Creating' — the state never settles.
AWS: Smithy: GetEventSourceMapping output EventSourceMappingConfiguration, member State (same documented value set as above). The recording shows the read-back of a mapping created with Enabled=false answering HTTP 200 with State 'Disabled'.
Emulator: answers 200 with $.State = 'Creating' on the subsequent read, so the stored state is 'Creating' and nothing ever transitions it.
Why: State is the only observable a client has for whether a mapping is settled. Because the stored value never leaves the transient state, a wait-until-ready or wait-until-disabled loop never terminates, and code that refuses to proceed while a mapping is 'Creating' blocks forever.
Fix: the event-source-mapping state in the service's stored state plus its read path — a create must persist the settled state (or the read path must resolve the transition), not leave 'Creating' permanently stored.
Traces: lambda-esm-scaling-config-roundtrip#task#8.

Cause 3 of 6: ListEventSourceMappings rows are not the page AWS serves — different row selected first, transient State, and whole-second LastModified.
AWS: Smithy: ListEventSourceMappings (GET /2015-03-31/event-source-mappings) takes EventSourceArn, FunctionName, Marker and MaxItems as httpQuery members and is paginated (inputToken Marker / outputToken NextMarker / pageSize MaxItems / items EventSourceMappings); each row is an EventSourceMappingConfiguration whose LastModified is a Date timestamp and whose State carries the documented value set. The recording's page carries, as its first row, the mapping for a different event-source queue than the emulator's first row, each row with State 'Disabled' and LastModified as fractional epoch seconds (millisecond precision).
Emulator: answers 200 with a page whose rows do not correspond to the recorded rows: the first row is a different mapping, every row carries State 'Creating' (Cause 1/2 again, visible through the list projection), and LastModified is serialized as whole epoch seconds with no fractional part. Hypothesis: the handler returns mappings in raw insertion order and does not apply the request's EventSourceArn/FunctionName/MaxItems selection to choose and order the page.
Why: a client paging event source mappings, or filtering them by event source or function, is handed rows for the wrong sources in the wrong order, so it operates on a mapping it did not ask about; and the truncated LastModified precision breaks change-detection that compares the modification instant across reads.
Fix: the ListEventSourceMappings handler's selection/ordering of the page and its row serializer (LastModified precision), plus the shared mapping state that supplies State.
Traces: lambda-list-event-source-mappings-params-and-pagination#task#13 (cluster also covers that fixture's task#14 and task#16). Note: the row UUIDs, ARNs and absolute LastModified values in the recording are per-run and are NOT the requirement — the requirement is which mapping each row is, the ordering, the State value, and the timestamp precision.

Cause 4 of 6: GetFunctionConcurrency's not-found message appends ':$LATEST' to a function ARN the caller never qualified.
AWS: Smithy: GetFunctionConcurrency (GET /2019-09-30/functions/{FunctionName}/concurrency) models ResourceNotFoundException (httpError 404). The recorded 404 body carries Message 'Function not found: <function-arn>' — fixed text 'Function not found: ' followed by one variable part, the unqualified ARN of the function the request named (arn:aws:lambda:<region>:<account>:function:<name>). No qualifier is appended when the request did not carry one.
Emulator: answers the correct 404 but with Message 'Function not found: <function-arn>:$LATEST' — it appends a ':$LATEST' qualifier the request never supplied.
Why: the message is part of the error contract. A client that parses the ARN out of the message, or asserts on it in tests and error-classification logic, gets a qualified ARN for an unqualified request and resolves or reports the wrong resource identity.
Fix: the GetFunctionConcurrency handler's not-found path (the ARN it formats into the ResourceNotFoundException message) — format the resolved function ARN without a qualifier unless the request carried one.
Traces: lambda-concurrency-qualified-and-missing-function-errors#task#6.

Cause 5 of 6: UpdateFunctionConfiguration with an empty Environment.Variables map echoes Environment back instead of reporting it cleared.
AWS: Smithy: UpdateFunctionConfiguration input member Environment (shape Environment, sole member Variables, a map), output FunctionConfiguration member Environment (shape EnvironmentResponse, members Variables and Error; documented as 'The results of an operation to update or read environment variables'). The recording shows that an update sending Environment={Variables:{}} — i.e. clearing the variables — answers HTTP 200 with the Environment member ABSENT from the response (recorded behavior only: the model does not state the empty-map rule, the recording does). An empty variable set is reported as no environment, not as an empty environment.
Emulator: answers 200 with $.Environment = {'Variables': {}} — it round-trips the empty map as a present-but-empty structure.
Why: 'cleared' and 'empty object' are different observations. A client that tests for the presence of Environment to decide whether a function has environment variables concludes the clear did not happen, and tooling that diffs configuration (CloudFormation-style drift, idempotent redeploys) sees a persistent difference against AWS and re-applies the update forever.
Fix: the UpdateFunctionConfiguration serializer / its environment-update logic — an environment whose variable map is empty must be stored and serialized as absent, not as an empty EnvironmentResponse.
Traces: lambda-lvau-traj-env-cleared-with-empty-map#task#4.

Cause 6 of 6: GetFunctionConfiguration read-back after the empty-map clear also reports Environment={Variables:{}}.
AWS: Smithy: GetFunctionConfiguration output FunctionConfiguration member Environment (EnvironmentResponse). The recorded read-back of a function whose environment was cleared with an empty map answers 200 with the Environment member ABSENT (recorded behavior only).
Emulator: answers 200 with $.Environment = {'Variables': {}}, so the empty structure is what was persisted by Cause 5 and is served on every later read, not just in the update's own response.
Why: the wrong shape is durable rather than transient — every subsequent describe of the function reports an empty environment object where AWS reports none, so the client-side divergence above persists for the life of the function.
Fix: the function-configuration state (how a cleared environment is stored) and the GetFunctionConfiguration serializer, which must omit Environment when no variables are set.
Traces: lambda-lvau-traj-env-cleared-with-empty-map#task#5.

Not covered by existing open issues: the replay report attributes no owning issue to any of these six clusters. Nearby open lambda issues ('20261103T165236Z-lambda-updatefunctionconfiguration-skips-vpc-and-environment-validation.md', '20261125T062539Z-iam-lambda-logs-sqs-updateeventsourcemapping-refuses-every-update.md', '20261106T225443Z-lambda-6-net-new-causes-layer-permission-preconditions.md') concern request validation, UpdateEventSourceMapping refusals and other causes, and none of them carries the replay keys listed above.

## Evidence
Replay of trace set simparity-wire-aws-v1 (174 fixtures, 1889 steps, 1850 passed, 39 mismatched, score 97.9%). Six net-new divergence clusters, all service=lambda, all verdict=mismatch with no stub_path and no owned_by:

Cause 1 — CreateEventSourceMapping, expected 202 / actual 202, key $.State: expected 'Disabled', got 'Creating'. Cluster: lambda-esm-filter-criteria-invalid-pattern-rejected#task#9, lambda-esm-scaling-config-roundtrip#task#7, lambda-esm-sqs-fifo-batch-size-above-ten-rejected#task#10.

Cause 2 — GetEventSourceMapping, expected 200 / actual 200, key $.State: expected 'Disabled', got 'Creating'. Cluster: lambda-esm-scaling-config-roundtrip#task#8.

Cause 3 — ListEventSourceMappings, expected 200 / actual 200, keys $.EventSourceMappings[0..2]: the recorded first row is the mapping whose EventSourceArn names one queue, with State 'Disabled' and LastModified 1793340109.656 (fractional epoch seconds); the emulator's first row is the mapping for a different queue, with State 'Creating' and LastModified 1795674717 (whole seconds). Cluster: lambda-list-event-source-mappings-params-and-pagination#task#13, #task#14, #task#16. (UUIDs/ARNs/absolute timestamps are per-run and not asserted as requirements.)

Cause 4 — GetFunctionConcurrency, expected 404 / actual 404, key $.Message: expected 'Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-probe-lambda-concurrency-missing-b38e6b1a', got the same string with ':$LATEST' appended. Fixed text: 'Function not found: '. Variable part: the unqualified function ARN from the request. Cluster: lambda-concurrency-qualified-and-missing-function-errors#task#6.

Cause 5 — UpdateFunctionConfiguration, expected 200 / actual 200, key $.Environment: type_mismatch, expected None (member absent), got {'Variables': {}}. Cluster: lambda-lvau-traj-env-cleared-with-empty-map#task#4.

Cause 6 — GetFunctionConfiguration, expected 200 / actual 200, key $.Environment: type_mismatch, expected None (member absent), got {'Variables': {}}. Cluster: lambda-lvau-traj-env-cleared-with-empty-map#task#5.

Model evidence cited: EventSourceMappingConfiguration.State documentation (Creating|Enabling|Enabled|Disabling|Disabled|Updating|Deleting); CreateEventSourceMapping input member Enabled; ListEventSourceMappings httpQuery members EventSourceArn/FunctionName/Marker/MaxItems and its paginated trait; EventSourceMappingConfiguration.LastModified target Date; GetFunctionConcurrency error ResourceNotFoundException httpError 404; UpdateFunctionConfiguration/GetFunctionConfiguration output member Environment target EnvironmentResponse.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it; replay trace set simparity-wire-aws-v1.
2. Cause 1/2: replay lambda-esm-filter-criteria-invalid-pattern-rejected#task#9 and lambda-esm-scaling-config-roundtrip#task#8 — create an SQS event source mapping with Enabled=false (POST /2015-03-31/event-source-mappings), then GET /2015-03-31/event-source-mappings/{UUID}. Observe State 'Creating' in both responses where the recording has 'Disabled'.
3. Cause 3: replay lambda-list-event-source-mappings-params-and-pagination#task#13 — GET /2015-03-31/event-source-mappings with the fixture's EventSourceArn/FunctionName/MaxItems query parameters and compare the returned rows, their order, each row's State, and the precision of LastModified against the recorded page.
4. Cause 4: replay lambda-concurrency-qualified-and-missing-function-errors#task#6 — GET /2019-09-30/functions/<name-that-does-not-exist>/concurrency with no qualifier; observe the 404 Message carries ':$LATEST' appended to the ARN.
5. Cause 5/6: replay lambda-lvau-traj-env-cleared-with-empty-map#task#4 and #task#5 — PUT /2015-03-31/functions/{FunctionName}/configuration with body {"Environment":{"Variables":{}}}, then GET /2015-03-31/functions/{FunctionName}/configuration; observe Environment serialized as {"Variables":{}} in both where the recording omits the member.
