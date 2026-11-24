Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: neptune-graph.neptune-graph-execute-query-during-graph-creation-conflict
Issue-Shape-Id: com.amazonaws.neptunegraph#AmazonNeptuneGraph
Issue-Key: neptune-graph.neptune-graph-execute-query-during-graph-creation-conflict|_error_awaiting_endpoint#7|AssertionError|raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Commit: 4a2d5a44947eccb6a6faf31afd46091a4f3aec95

# neptune-graph ExecuteQuery serves a graph still in CREATING with HTTP 200 instead of refusing it (ResourceNotFoundException "No graph found for customer [...] with graph id [...]")

> _Note: this issue was identified against the code base at commit `4a2d5a44947eccb6a6faf31afd46091a4f3aec95`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteQuery

## Description
One net-new root cause in neptune-graph's data plane.

Cause 1 of 1: ExecuteQuery accepts and answers a query against a graph whose control-plane status is still CREATING, where AWS refuses it because the graph's per-graph data-plane endpoint is not registered yet. Failing check: neptune-graph.neptune-graph-execute-query-during-graph-creation-conflict.

AWS: recorded behavior only for the refusal itself — ExecuteQuery's modeled error set (Smithy: ExecuteQuery errors = AccessDeniedException 403, ConflictException 409, InternalServerException 500, ThrottlingException 429, UnprocessableException 422, ValidationException 400; @endpoint hostPrefix "{graphIdentifier}.", http POST /queries, success code 200) does not carry a not-found error, and the recording shows the refusal arriving from the host-prefixed front door rather than from the operation. The recording for a graph that CreateGraph has just returned and whose GetGraph status is CREATING is an error with code ResourceNotFoundException and message "No graph found for customer [<account>] with graph id [<graph-id>]". Fixed text of that message: "No graph found for customer [", "] with graph id [", "]"; the variable parts are the caller's account id and the graphIdentifier from the request — never the account or id of any one run. Once the graph settles to AVAILABLE the identical query succeeds with 200 and the result set (the recording's query "RETURN 1 AS one" yields [{"one": 1}]), so the refusal is a function of the graph's status, not of the query.

Emulator: the same call returns HTTP 200 with an application/octet-stream payload (ResponseMetadata HTTPStatusCode 200, content-length 23) while GetGraph on the very same graph, immediately before, reports status CREATING. The handler does gate on serving status (services/service-com-amazonaws-neptunegraph-amazonneptunegraph/src/ops/execute_query.rs looks up `store.settled_graph(..).filter(|graph| graph.status.is_serving())` and otherwise answers `Scaffolded::graph_unavailable()`), so either CREATING is being counted as serving or the clock settlement used by the data-plane lookup promotes CREATING to AVAILABLE ahead of the status the control plane reports. The emulator therefore contradicts itself across its two planes for one graph at one instant, and never produces the recorded ResourceNotFoundException for this state.

Why: a client that waits for a graph to become usable by probing the data plane — issuing a query and treating the not-found refusal as "not up yet" before polling GetGraph — sees an immediate success against a graph AWS has not finished creating. Its readiness loop exits early, and code paths written for AWS's refusal (retry/backoff, waiters, error-class handling that distinguishes a not-yet-resolvable endpoint from a real query error) are never exercised against the emulator; conversely, any client asserting the refusal fails. The cross-plane inconsistency also makes CREATING unobservable from the data plane at all.

Fix: the data-plane admission decision for host-prefixed graph operations — the status/serving gate and the clock settlement it reads for ExecuteQuery (the operation's handler and the shared graph lookup it calls), plus the error channel that renders the refusal, which must emit the recorded code ResourceNotFoundException and the message template above rather than a 200 or the generic graph-unavailable error. A graph in CREATING must not be serving, and the status the data plane uses must be the same status GetGraph reports at that instant. Note the open issue 20261023T175427Z-neptune-graph-executequery-stub.md (ExecuteQuery scaffold stub) and 20261029T201135Z-neptune-graph-3-net-new-wire-causes-host.md (host-prefix refusal code for a graph that does not exist) are related but neither covers this state gate: ExecuteQuery now answers rather than returning 501, and here the graph does exist.

Traces: neptune-graph.neptune-graph-execute-query-during-graph-creation-conflict (GROUP error-semantics; CreateGraph -> GetGraph reports CREATING -> ExecuteQuery expected refused, got 200).

## Evidence
Check neptune-graph.neptune-graph-execute-query-during-graph-creation-conflict [mismatch]:

  File "check_neptune_graph_neptune_graph_execute_query_during_graph_creation_conflict.py", line 88, in _error_awaiting_endpoint
    raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED and returned {result!r}")
  AssertionError: expected the call to be rejected, but it SUCCEEDED and returned {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/octet-stream', 'content-length': '23', ...}, 'RetryAttempts': 1}, 'payload': <botocore.response.StreamingBody object>}

Expected (recorded AWS): ClientError with Error.Code == "ResourceNotFoundException" and Error.Message == "No graph found for customer [<account>] with graph id [<graph-id>]" (HTTP 404), with <account> the caller's account and <graph-id> the graphIdentifier sent; the check then polls GetGraph to AVAILABLE and the same query returns results == [{"one": 1}].

Actual (emulator): HTTP 200 with a streaming payload, even though GetGraph on that graph returned status "CREATING" on the immediately preceding call (that assertion passed).

Handler evidence: services/service-com-amazonaws-neptunegraph-amazonneptunegraph/src/ops/execute_query.rs lines 270-278 gate on `settled_graph(..).filter(|graph| graph.status.is_serving())` and otherwise return `crate::scaffolded::Scaffolded::graph_unavailable()`, so a CREATING graph is passing that gate (or being settled to AVAILABLE by the data-plane lookup) rather than being refused.

Model evidence: smithy_query(query="operation", operation="ExecuteQuery") on services/service-com-amazonaws-neptunegraph-amazonneptunegraph/smithy-model/com-amazonaws-neptunegraph-amazonneptunegraph.json — errors: AccessDenied 403, Conflict 409, InternalServer 500, Throttling 429, Unprocessable 422, Validation 400; http POST /queries code 200; endpoint hostPrefix "{graphIdentifier}."; graphIdentifier pattern ^g-[a-z0-9]{10}$ bound as hostLabel + header.

## Steps to Reproduce
1. Boot the emulator and point a neptune-graph client at it.
2. CreateGraph(graphName=<unique>, provisionedMemory=16, deletionProtection=false, publicConnectivity=true) and keep the returned id.
3. Immediately GetGraph(graphIdentifier=<id>) and confirm status == "CREATING".
4. Call ExecuteQuery(graphIdentifier=<id>, queryString="RETURN 1 AS one", language="OPEN_CYPHER").
5. Observe HTTP 200 with a payload. AWS instead refuses with ResourceNotFoundException / "No graph found for customer [<account>] with graph id [<id>]" until the graph leaves CREATING.
6. Poll GetGraph until AVAILABLE and repeat the query: it must then return results == [{"one": 1}].
