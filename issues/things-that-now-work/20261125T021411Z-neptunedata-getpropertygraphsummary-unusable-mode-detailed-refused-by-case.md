Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: neptunedata.propertygraph-summary-reflects-added-vertices
Issue-Shape-Id: com.amazonaws.neptunedata#AmazonNeptuneDataplane
Issue-Key: neptunedata|GetPropertygraphSummary|ValidationException
Issue-Commit: 8499d642ca764a1a3b76811380985b459db64763

# neptunedata GetPropertygraphSummary unusable: mode=DETAILED refused by case-sensitive enum validation with an unmodelled ValidationException, and the operation itself is a 501 stub

> _Note: this issue was identified against the code base at commit `8499d642ca764a1a3b76811380985b459db64763`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetPropertygraphSummary

## Description
GetPropertygraphSummary cannot be used at all against the emulator: the request is rejected before the handler by the centralized enum check, and the handler behind it is still a scaffold stub. Two root causes, both in this one operation.

Cause 1 of 2: the `mode` query parameter is validated case-sensitively against the enum's lowercase wire values, so the spelling AWS documents and accepts (`DETAILED`) is refused — with an error class this service does not model.
  AWS:      Smithy: GetPropertygraphSummary.mode -> GraphSummaryType, bound @httpQuery("mode"), default "basic", enum values [basic, detailed]; the member's own doc says "Mode can take one of two values: BASIC (the default), and DETAILED", and PropertygraphSummary.nodeStructures/edgeStructures are documented as "only present when the requested mode is DETAILED". Neptune's data plane therefore accepts the documented upper-case spelling of the mode value and answers 200 with the payload; the recording shows `get_propertygraph_summary(mode="DETAILED")` succeeding and returning payload.graphSummary. Additionally, `ValidationException` is NOT a shape in this service's model at all, and it is not among this operation's modelled errors (AccessDeniedException 403, BadRequestException 400, ClientTimeoutException 408, ConstraintViolationException 400, IllegalArgumentException 400, InvalidArgumentException 400, InvalidParameterException 400, MissingParameterException 400, PreconditionsFailedException 400, ReadOnlyViolationException 400, StatisticsNotAvailableException 400, TooManyRequestsException 429, UnsupportedOperationException 400). Whatever a genuinely unacceptable mode value should produce, it is one of those neptunedata exception classes (an argument that is not supported / has an invalid value), never a generic ValidationException.
  Emulator: the request is refused with HTTP 400 and error code `ValidationException`, message "1 validation error detected: Value 'DETAILED' at 'mode' failed to satisfy constraint: Member must satisfy enum value set: [basic, detailed]". Fixed text of that message: "1 validation error detected: Value '", "' at '", "' failed to satisfy constraint: Member must satisfy enum value set: [", "]"; its variable parts are the rejected value (`DETAILED`, taken from the request), the member name (`mode`) and the enum value list — i.e. nothing here is specific to the recording, the refusal fires for any upper-case mode. The call never reaches the handler.
  Why:      every client that follows the documented API — including the AWS SDKs' own examples and any caller that asks for the DETAILED summary by its documented name — gets a 400 for a valid request, and gets an error code (`ValidationException`) that is not in the service's error set, so SDK error-shape dispatch and `except client.exceptions.*` handling cannot match it and the failure surfaces as a generic ClientError.
  Fix:      the `mode` input validation for this operation (the enum/constraint validation step that runs before `GetPropertygraphSummary::handle` in services/service-com-amazonaws-neptunedata-amazonneptunedataplane): accept the documented enum spellings for this httpQuery-bound enum, and when a mode value really is unacceptable refuse it with one of the operation's modelled neptunedata exception classes instead of an unmodelled ValidationException.
  Traces:   neptunedata.propertygraph-summary-reflects-added-vertices

Cause 2 of 2: GetPropertygraphSummary has no implementation, so even a mode value the validation accepts cannot produce a summary.
  AWS:      Smithy: GetPropertygraphSummary GET /propertygraph/statistics/summary -> 200 with payload PropertygraphSummaryValueMap {version, lastStatisticsComputationTime, graphSummary}, where graphSummary is PropertygraphSummary {numNodes, numEdges, numNodeLabels, numEdgeLabels, nodeLabels, edgeLabels, numNodeProperties, numEdgeProperties, nodeProperties, edgeProperties, totalNodePropertyValues, totalEdgePropertyValues, and — for DETAILED — nodeStructures/edgeStructures}. The summary reflects the graph's current statistics, so a vertex label written by Gremlin appears in nodeLabels and numNodes counts the vertices present after a statistics refresh.
  Emulator: services/service-com-amazonaws-neptunedata-amazonneptunedataplane/src/ops/get_propertygraph_summary.rs still carries the "SCAFFOLDED, NOT IMPLEMENTED" marker and its handle() returns Scaffolded::not_implemented("GetPropertygraphSummary"), i.e. HTTP 501 `NotImplemented` with "operation GetPropertygraphSummary is scaffolded but not implemented in this emulator". Cause 1 masks this today; it is reached as soon as the mode value is accepted.
  Why:      a client cannot read any graph statistics: no nodeLabels, no numNodes, no lastStatisticsComputationTime, and a 501 that no SDK models as a retryable or handleable service error.
  Fix:      implement the handler in services/service-com-amazonaws-neptunedata-amazonneptunedataplane/src/ops/get_propertygraph_summary.rs over the service's graph state, computing the summary from the vertices/edges held after ManagePropertygraphStatistics(mode=refresh), and emitting the DETAILED-only members when the requested mode is detailed.
  Traces:   neptunedata.propertygraph-summary-reflects-added-vertices

No open issue covers this operation; the only other Propertygraph issue (20261104T225928Z-neptunedata-managepropertygraphstatistics-stub.md) is for ManagePropertygraphStatistics and is already fixed (that call succeeds in this check).

## Evidence
Failing check: neptunedata.propertygraph-summary-reflects-added-vertices (verdict: error).

Expected (AWS / model): two Gremlin-created vertices with a fresh label, then ManagePropertygraphStatistics(mode="refresh") and GetPropertygraphSummary(mode="DETAILED") -> HTTP 200, payload.graphSummary with the new label present in nodeLabels and numNodes >= 2.

Actual (emulator): GetPropertygraphSummary(mode="DETAILED") -> HTTP 400, error code `ValidationException` (a shape this service's model does not define), message:
  "1 validation error detected: Value 'DETAILED' at 'mode' failed to satisfy constraint: Member must satisfy enum value set: [basic, detailed]"
raised by botocore as botocore.errorfactory.ValidationException on the GetPropertygraphSummary operation. The handler is never entered; behind the validation it would answer HTTP 501 `NotImplemented` ("operation GetPropertygraphSummary is scaffolded but not implemented in this emulator"), per the SCAFFOLDED marker and the not_implemented() return in services/service-com-amazonaws-neptunedata-amazonneptunedataplane/src/ops/get_propertygraph_summary.rs lines 4-12 and 190.

Model evidence: GetPropertygraphSummaryInput.mode doc — "Mode can take one of two values: BASIC (the default), and DETAILED"; PropertygraphSummary.nodeStructures doc — "This field is only present when the requested mode is DETAILED"; GraphSummaryType values [basic, detailed]; operation errors do not include ValidationException (no such shape in com.amazonaws.neptunedata).

## Steps to Reproduce
1. Create a Neptune cluster + instance (IAM database auth enabled) and reach its data-plane endpoint on port 8182 from inside the VPC, as the check's _own_neptune_cluster helper does.
2. client = boto3 neptunedata client against that cluster endpoint.
3. client.execute_gremlin_query(gremlinQuery="g.addV('<fresh_label>')") twice.
4. client.manage_propertygraph_statistics(mode="refresh")  -> succeeds.
5. client.get_propertygraph_summary(mode="DETAILED")
   Observed: 400 ValidationException "1 validation error detected: Value 'DETAILED' at 'mode' failed to satisfy constraint: Member must satisfy enum value set: [basic, detailed]".
   Expected: 200 with payload.graphSummary whose nodeLabels contains <fresh_label> and whose numNodes >= 2.
6. Repeat step 5 with mode="detailed" to reach the handler: observed 501 NotImplemented for GetPropertygraphSummary (cause 2).
