Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: neptunedata.create-ml-endpoint-nonexistent-training-job-rejected neptunedata.gremlin-query-malformed-queryid-rejected-by-status-and-cancel neptunedata.opencypher-query-malformed-queryid-rejected-by-status-and-cancel neptunedata.gremlin-malformed-query-rejected neptunedata.opencypher-malformed-query-rejected neptunedata.start-loader-job-invalid-iam-role-arn-rejected
Issue-Shape-Id: com.amazonaws.neptunedata#AmazonNeptuneDataplane
Issue-Key: neptunedata.create-ml-endpoint-nonexistent-training-job-rejected|run#7|AssertionError|assert code == "MLResourceNotFoundException", code
Issue-Key: neptunedata.gremlin-query-malformed-queryid-rejected-by-status-and-cancel|run#8|AssertionError|assert status_message == expected, f"expected message {expected!r}, got {status_message!r}"
Issue-Key: neptunedata.opencypher-query-malformed-queryid-rejected-by-status-and-cancel|run#7|AssertionError|assert status_code == "InvalidParameterException", (
Issue-Key: neptunedata.gremlin-malformed-query-rejected|run#9|AssertionError|assert code == "MalformedQueryException", (
Issue-Key: neptunedata.opencypher-malformed-query-rejected|run#11|AssertionError|assert error.get("Message") == expected, f"expected message {expected!r}, got {error.get('Message')!r}"
Issue-Key: neptunedata.start-loader-job-invalid-iam-role-arn-rejected|run#7|AssertionError|assert code == "InvalidParameterException", code
Issue-Commit: f55279b3dc5678191fb0e8465c0432e63e772ce0

# neptunedata refusal paths diverge: ML, openCypher status/cancel, StartLoaderJob and malformed-query refusals answer 501 NotImplemented, and the implemented queryId refusal carries an empty client message

> _Note: this issue was identified against the code base at commit `f55279b3dc5678191fb0e8465c0432e63e772ce0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMLEndpoint, GetGremlinQueryStatus, GetOpenCypherQueryStatus, ExecuteGremlinQuery, ExecuteOpenCypherQuery, StartLoaderJob

## Description
SIX distinct root causes on the neptunedata data plane, every one of them a refusal path. All were reached against a RUNNING emulator: the cluster endpoint answered and served these calls (so this is not the open unreachability issue 20261109T113928Z-neptunedata-is-still-entirely-unreachable-the-neptune-db.md), it answered them wrongly. A client that branches on Neptune's modelled error classes cannot tell "bad request" from "feature absent", and in one case gets a refusal with no message text at all.

NOTE ON OVERLAP: Causes 1-5 below restate the still-open issue 20261104T225928Z-neptunedata-5-net-new-refusal-path-causes-ml.md, whose replay keys no longer match these checks (the suite did not mark them OWNED, so they resurfaced as live mismatches); the handlers it names are still unchanged in the checkout. Cause 6 is net-new. Causes 4, 5 and 6 sit on operations whose bare stubs are separately auto-filed ("Implement neptunedata ExecuteGremlinQuery", "...ExecuteOpenCypherQuery", "...StartLoaderJob"); what is stated here is the REFUSAL CONTRACT those implementations must satisfy — classification plus message template — which the stub items do not state.

Cause 1 of 6: CreateMLEndpoint / GetMLEndpoint answer 501 NotImplemented where Neptune answers 404 MLResourceNotFoundException.
AWS: Smithy (com.amazonaws.neptunedata) models CreateMLEndpoint POST /ml/endpoints with error MLResourceNotFoundException (httpError 404, "Raised when a specified machine-learning resource could not be found"), required members detailedMessage, requestId, code. A CreateMLEndpoint naming an mlModelTrainingJobId that does not exist is refused with that class, and GetMLEndpoint on the id that was therefore never created is refused with the same class. Recorded behavior: the message is wholly FIXED text with no variable part — "Invalid ID provided. Please check whether a resource with the given ID exists."
Emulator: both operations are scaffold stubs, so the call is answered HTTP 501 with error code "NotImplemented" and text of the form "operation CreateMLEndpoint is scaffolded but not implemented in this emulator".
Why: a client cannot distinguish a nonexistent training job from an unavailable service; retry/branch logic keyed on MLResourceNotFoundException never fires, and nothing proves the rejected create left no endpoint behind.
Fix: the CreateMLEndpoint and GetMLEndpoint handlers plus an ML-resource registry in this crate's state (the training-job/endpoint lookup the 404 is derived from).
Traces: neptunedata.create-ml-endpoint-nonexistent-training-job-rejected.

Cause 2 of 6: GetGremlinQueryStatus / CancelGremlinQuery refuse an unparseable queryId with the RIGHT class but an EMPTY client-visible message.
AWS: Smithy models GetGremlinQueryStatus GET /gremlin/status/{queryId} and CancelGremlinQuery DELETE /gremlin/status/{queryId} with InvalidParameterException (httpError 400). Recorded behavior: the client-visible message is "Supplied queryId <queryId> is invalid" — fixed text "Supplied queryId " ... " is invalid" around the queryId taken verbatim from the request path (the recording's 'totally-invalid-query-id-not-a-uuid' is one filling of that slot, not part of the template).
Emulator: the refusal is classified InvalidParameterException (400) and the text is written ONLY into detailedMessage; the field a client reads as the error message is absent from the body, so botocore surfaces Message == '' (empty string).
Why: every client-side diagnostic, log line and message-matching branch gets nothing; the refusal is unactionable even though the code is right.
Fix: the shared modelled-error payload builders and the error serializer of this crate — the InvalidParameterException payload builder used by the status/cancel handlers populates detailedMessage/requestId/code only, and the serialized body must also carry the message field AWS populates. The same builders back missing_parameter, invalid_fast_reset_token and invalid_parameter_range, so the fix belongs in that shared layer, not in one handler.
Traces: neptunedata.gremlin-query-malformed-queryid-rejected-by-status-and-cancel.

Cause 3 of 6: GetOpenCypherQueryStatus and CancelOpenCypherQuery answer 501 NotImplemented where Neptune answers 400 InvalidParameterException.
AWS: Smithy models GetOpenCypherQueryStatus GET /opencypher/status/{queryId} and CancelOpenCypherQuery DELETE /opencypher/status/{queryId}, both with InvalidParameterException (httpError 400). Recorded behavior: an unparseable queryId is refused with that class and the message "Supplied queryId <queryId> is invalid" (same template as Cause 2), while the sibling ListOpenCypherQueries still succeeds and returns a queries key.
Emulator: both handlers are scaffold stubs and answer HTTP 501 with error code "NotImplemented".
Why: the openCypher query-management surface diverges from the Gremlin one that IS implemented, so a client written against Neptune's uniform queryId validation breaks on exactly one of the two query languages.
Fix: the GetOpenCypherQueryStatus and CancelOpenCypherQuery handlers, reusing the query registry and the shared invalid-queryId payload already present in this crate's state (and gated on the message-field fix of Cause 2).
Traces: neptunedata.opencypher-query-malformed-queryid-rejected-by-status-and-cancel.

Cause 4 of 6: ExecuteGremlinQuery does not refuse a syntactically invalid Gremlin query with MalformedQueryException.
AWS: Smithy models ExecuteGremlinQuery POST /gremlin with MalformedQueryException (httpError 400, "Raised when a query is submitted that is syntactically incorrect or does not pass additional validation"). Recorded behavior: an unbalanced query is refused 400 MalformedQueryException with the parser's message, whose FIXED text is "Failed to interpret Gremlin query: Query parsing failed at line 1, character position at " + <0-based character offset at which the parser stopped> + ", error message : no viable alternative at input '" + <the submitted query up to and including that character> + "'". The offset and the quoted prefix derive from the request's own query string; no value from any one recording is part of the requirement.
Emulator: the operation is a scaffold stub, so the malformed query is answered 501 "NotImplemented" rather than refused as malformed — nothing in the emulator parses Gremlin.
Why: a client cannot distinguish its own syntax error from an emulator gap, and a suite asserting a bad query is rejected passes or fails for the wrong reason.
Fix: the ExecuteGremlinQuery handler and its query-parse/validation step (classification + message construction) in services/service-com-amazonaws-neptunedata-amazonneptunedataplane/src/ops/execute_gremlin_query.rs.
Traces: neptunedata.gremlin-malformed-query-rejected.

Cause 5 of 6: ExecuteOpenCypherQuery does not refuse a syntactically invalid openCypher query with MalformedQueryException.
AWS: Smithy models ExecuteOpenCypherQuery POST /opencypher with MalformedQueryException (httpError 400). Recorded behavior: a misspelled clause is refused 400 MalformedQueryException with the openCypher parser's message, template "Invalid input '<the offending character>': expected '<the alternatives the parser expected>' (line <line>, column <column> (offset: <0-based offset>))" — every variable part derives from the submitted query (one recording of CREAT (n:`<Label>`) SET n.x = yields "Invalid input ' ': expected 'e/E' (line 1, column 6 (offset: 5))").
Emulator: the operation is a scaffold stub and answers 501 "NotImplemented"; no node is created, but no malformed-query refusal is produced either, and the response body carries an empty Message.
Why: same as Cause 4 — the only signal a client gets for invalid Cypher is an emulator-gap status, so the rejection contract of the data plane is unobservable.
Fix: the ExecuteOpenCypherQuery handler and its parse/validation step in services/service-com-amazonaws-neptunedata-amazonneptunedataplane/src/ops/execute_open_cypher_query.rs.
Traces: neptunedata.opencypher-malformed-query-rejected.

Cause 6 of 6: StartLoaderJob answers 501 NotImplemented for an unusable iamRoleArn where Neptune answers 400 InvalidParameterException.
AWS: Smithy models StartLoaderJob POST /loader with required members source, format, region (wire name of s3BucketRegion) and iamRoleArn, and declares InvalidParameterException (httpError 400) among its errors. iamRoleArn is an unconstrained String, so the value is not refused by a modelled constraint — the loader refuses it when it cannot resolve credentials for it. Recorded behavior: a non-ARN iamRoleArn is refused 400 InvalidParameterException with the message "Failed to start new load from the source <the source value from the request>. Couldn't find the aws credential for iam_role_arn: <the iamRoleArn value from the request>" — the two sentences are fixed text and both variable parts are echoed from the request. The refusal also admits no job: ListLoaderJobs' loadIds set is unchanged across the call and ListLoaderJobs itself keeps answering.
Emulator: StartLoaderJob is a scaffold stub (services/service-com-amazonaws-neptunedata-amazonneptunedataplane/src/ops/start_loader_job.rs) and answers HTTP 501 with error code "NotImplemented", so no ARN/credential validation exists at all.
Why: a client that keys on InvalidParameterException to report a misconfigured load role instead sees a feature-absent status, and the "a rejected load adds no loadId" invariant is untested.
Fix: the StartLoaderJob handler, its iamRoleArn validation, and the loader-job registry in this crate's state (the registry ListLoaderJobs reads, which must stay unchanged on a refusal). The bare stub is covered by the auto-filed "Implement neptunedata StartLoaderJob" item; this cause states the refusal classification and message template that implementation must satisfy.
Traces: neptunedata.start-loader-job-invalid-iam-role-arn-rejected.

## Evidence
Cause 1 — expected: ClientError Code MLResourceNotFoundException (HTTP 404), Message "Invalid ID provided. Please check whether a resource with the given ID exists." from both create_ml_endpoint(id=<fresh id>, mlModelTrainingJobId=<nonexistent>) and the follow-up get_ml_endpoint(id=<that id>), with get_engine_status() still reporting healthy/recovery. Actual: "AssertionError: NotImplemented" at `assert code == "MLResourceNotFoundException"` — Code 'NotImplemented', HTTP 501.

Cause 2 — expected: get_gremlin_query_status(queryId=<unparseable id>) and cancel_gremlin_query(queryId=<same>) -> InvalidParameterException with Message "Supplied queryId <that id> is invalid". Actual: Code InvalidParameterException (correct), but "AssertionError: expected message 'Supplied queryId totally-invalid-query-id-not-a-uuid is invalid', got ''". Root cause visible in the checkout: services/service-com-amazonaws-neptunedata-amazonneptunedataplane/src/state.rs:693-699 (`invalid_query_id`) builds InvalidParameterException { detailed_message, request_id, code } with no message member, and src/ops/get_gremlin_query_status.rs:197-200 returns exactly that payload; the model's InvalidParameterException (state.rs peers invalid_parameter_range:707, missing_parameter:726, invalid_fast_reset_token:740 do the same) declares only detailedMessage/requestId/code, so the client-visible message must be added by the error serializer.

Cause 3 — expected: InvalidParameterException (HTTP 400), Message "Supplied queryId <that id> is invalid" from GetOpenCypherQueryStatus and CancelOpenCypherQuery, with list_open_cypher_queries() succeeding. Actual: "AssertionError: expected InvalidParameterException from GetOpenCypherQueryStatus on a malformed queryId, got 'NotImplemented'" — src/ops/get_open_cypher_query_status.rs:4 and :201 still read "SCAFFOLDED, NOT IMPLEMENTED ... answers HTTP 501" / `Scaffolded::not_implemented("GetOpenCypherQueryStatus")`; src/ops/cancel_open_cypher_query.rs is the same.

Cause 4 — expected: MalformedQueryException (HTTP 400), Message "Failed to interpret Gremlin query: Query parsing failed at line 1, character position at <offset>, error message : no viable alternative at input '<query prefix>'". Actual: "AssertionError: expected MalformedQueryException for a syntactically invalid Gremlin query, got 'NotImplemented'" (HTTP 501).

Cause 5 — expected: MalformedQueryException (HTTP 400), Message "Invalid input '<char>': expected '<alternatives>' (line <l>, column <c> (offset: <o>))". Actual: "AssertionError: expected message \"Invalid input ' ': expected 'e/E' (line 1, column 6 (offset: 5))\", got ''" after the Code assertion saw 'NotImplemented' (HTTP 501).

Cause 6 — expected: InvalidParameterException (HTTP 400), Message "Failed to start new load from the source <source>. Couldn't find the aws credential for iam_role_arn: <iamRoleArn>", and ListLoaderJobs' loadIds unchanged. Actual: "AssertionError: NotImplemented" at `assert code == "InvalidParameterException"` — Code 'NotImplemented', HTTP 501, from the start_loader_job.rs scaffold.

## Steps to Reproduce
1. Stand up a Neptune DB cluster plus one db.t4g.medium instance in the run's private network with IAM database authentication, and point the neptunedata client at the cluster Endpoint on port 8182 (what each check's _own_neptune_cluster helper does).
2. Cause 1: create_ml_endpoint(id=<fresh id>, mlModelTrainingJobId=<an id that was never created>), then get_ml_endpoint(id=<that fresh id>); inspect Error.Code/Error.Message of both, then get_engine_status().
3. Cause 2: get_gremlin_query_status(queryId='totally-invalid-query-id-not-a-uuid') and cancel_gremlin_query(queryId=<same>); inspect Error.Message (empty today) while list_gremlin_queries() still succeeds.
4. Cause 3: get_open_cypher_query_status(queryId=<same unparseable id>) and cancel_open_cypher_query(queryId=<same>); both answer 501 NotImplemented, with list_open_cypher_queries() as the positive control.
5. Cause 4: execute_gremlin_query(gremlinQuery="g.addV('<label>').property(((("); inspect Error.Code/Error.Message, then count vertices of that label.
6. Cause 5: execute_open_cypher_query(openCypherQuery="CREAT (n:`<Label>`) SET n.x = "); inspect Error.Code/Error.Message, then MATCH-count that label.
7. Cause 6: record list_loader_jobs()['payload']['loadIds'], call start_loader_job(source='s3://<bucket>/data/', format='csv', s3BucketRegion=<a loader region>, iamRoleArn='not-a-valid-arn'), inspect Error.Code/Error.Message, then re-read loadIds and assert it is unchanged.
8. Replay: run_behavioral_suite and re-check the six check ids listed in trace_ids.
