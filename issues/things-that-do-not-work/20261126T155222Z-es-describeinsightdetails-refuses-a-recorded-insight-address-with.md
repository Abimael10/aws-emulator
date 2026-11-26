Issue-Source: trace-sweep-divergence
Issue-Trace: opensearch-domain-insights-operations
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: opensearch-domain-insights-operations
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: es|DescribeInsightDetails|2xx->4xx|$.Fields
Issue-Key: es|DescribeInsightDetails|2xx->4xx|status
Issue-Commit: 80a7c7f5468625a96d1dbbdd9db26cb93639edde

# es DescribeInsightDetails refuses a recorded insight address with 404 instead of answering 200 with the console-guidance field

> _Note: this issue was identified against the code base at commit `80a7c7f5468625a96d1dbbdd9db26cb93639edde`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInsightDetails

## Description
One root cause.

Cause 1 of 1: DescribeInsightDetails answers a not-found refusal for an insight address that AWS answers successfully, so the whole read path fails rather than returning the insight's detail view.
AWS: Smithy com.amazonaws.opensearch#DescribeInsightDetails - POST /2021-01-01/opensearch/insight-details, success status 200; input Entity -> InsightEntity @required, InsightId -> GUID @required (length 36..36, pattern ^\p{XDigit}{8}-\p{XDigit}{4}-\p{XDigit}{4}-\p{XDigit}{4}-\p{XDigit}{12}$), ShowHtmlContent -> Boolean; output Fields -> InsightFieldList @required, whose member InsightField is {Name String @required, Type InsightFieldType @required (enum: text, metric), Value String @required}; modeled errors BaseException(400, defaulted), ValidationException(400), DisabledOperationException(409), LimitExceededException(409), ResourceNotFoundException(409), InternalException(500) - 404 is NOT a status this operation is modeled to answer on at all. What AWS does for the recorded request is recorded behavior only: the recorded reply is HTTP 200 with exactly one field, Fields = [{Name: 'Message', Type: 'text', Value: 'Please use the Amazon OpenSearch Console to view the Insight Details for a better user experience.'}]. That is the complete fixed text of the field's Value - it carries no part of the request, no insight id and no entity, so the same sentence is the answer for any well-formed address; the recorded step shows AWS answering it WITHOUT making the insight address resolve to a stored insight first.
Emulator: answers HTTP 404 with no body members. The replay reports 'status: expected 200, got 404' and '$.Fields: missing (expected [{Name: Message, Type: text, Value: Please use the Amazon OpenSearch Console to view the Insight Details for a better user experience.}], got None)'. The operation is implemented (stub_path is empty) and the handler already holds the correct guidance text - services/service-com-amazonaws-opensearch-amazonopensearchservice/src/ops/describe_insight_details.rs defines GUIDANCE as exactly that sentence and guidance_details() builds exactly the one Name='Message'/Type='text' field - but it is reached only AFTER store.insight_of_entity(&input.entity, &input.insight_id) resolves both halves of the address; when the store holds no insight under that entity and id the handler maps the refusal to ResourceNotFoundException and the request is refused. So the emulator imposes an address-resolution precondition AWS does not impose here, and it answers that refusal on 404, which is not among the operation's modeled error statuses (ResourceNotFoundException is httpError 409).
Why: a client reading an insight's detail view - the console-equivalent flow, and any tool that lists insights then describes one - gets a hard failure instead of the service's guidance payload. Against AWS the call always succeeds with the one text field, so client code has no not-found branch for it and the error surfaces as an unhandled exception; and because the refusal arrives on 404 rather than the modeled 409, an SDK's error-shape matching cannot even classify it as ResourceNotFoundException, so retry/classification logic mis-handles it.
Fix: the DescribeInsightDetails handler in the opensearch service crate (services/service-com-amazonaws-opensearch-amazonopensearchservice/src/ops/describe_insight_details.rs, the store.insight_of_entity branch) must answer the detail view for a well-formed request without requiring the address to resolve to a stored insight - i.e. details with guidance_details() (Fields = the single Name='Message', Type='text', Value='Please use the Amazon OpenSearch Console to view the Insight Details for a better user experience.' field) as the recording shows, keeping ValidationException(400) for a malformed InsightId/Entity. If any not-found refusal is retained for this shape it must be answered on the modeled ResourceNotFoundException status 409, never 404.
Related: no open issue carries this cluster's replay key - the replay reports it unowned (owned_by empty) and unblocked (stub_path empty).
Traces: opensearch-domain-insights-operations#task#3.

## Evidence
Replay cluster (trace set simparity-wire-aws-v1, verdict mismatch, service es, operation DescribeInsightDetails, fixture opensearch-domain-insights-operations, step task#3, affected_count 1, stub_path empty, owned_by empty):
  expected_status 200, actual_status 404
  detail:
    status: expected 200, got 404
    $.Fields: missing (expected [{'Name': 'Message', 'Type': 'text', 'Value': 'Please use the Amazon OpenSearch Console to view the Insight Details for a better user experience.'}], got None)
  divergence_key: service=es|DescribeInsightDetails|status=200->404|keys=$.Fields,status

Expected (AWS): HTTP 200, body {"Fields": [{"Name": "Message", "Type": "text", "Value": "Please use the Amazon OpenSearch Console to view the Insight Details for a better user experience."}]}. The Value is entirely fixed text - it has no variable part taken from the request or from state.

Actual (emulator): HTTP 404 with no Fields member. 404 is not a modeled status for this operation; its modeled client errors are 400 (BaseException/ValidationException) and 409 (DisabledOperationException, LimitExceededException, ResourceNotFoundException).

Smithy grounding (smithy_query, model_path services/service-com-amazonaws-opensearch-amazonopensearchservice/smithy-model/com-amazonaws-opensearch-amazonopensearchservice.json):
  DescribeInsightDetails: http {method POST, uri /2021-01-01/opensearch/insight-details, code 200}; input {Entity -> InsightEntity @required, InsightId -> GUID @required length 36..36 pattern ^\p{XDigit}{8}-\p{XDigit}{4}-\p{XDigit}{4}-\p{XDigit}{4}-\p{XDigit}{12}$, ShowHtmlContent -> Boolean}; output {Fields -> InsightFieldList @required}; errors BaseException(client,400,defaulted), DisabledOperationException(client,409), InternalException(server,500), LimitExceededException(client,409), ResourceNotFoundException(client,409), ValidationException(client,400).
  InsightField: structure {Name String @required, Type InsightFieldType @required (enum values text, metric), Value String @required}, doc 'Represents a field in the detailed view of an insight, returned by the DescribeInsightDetails operation.'

Code corroboration that only the precondition is wrong, not the payload: services/service-com-amazonaws-opensearch-amazonopensearchservice/src/ops/describe_insight_details.rs lines 251-276 define GUIDANCE_FIELD = "Message" and GUIDANCE = "Please use the Amazon OpenSearch Console to view the Insight Details for a better user experience." and build exactly the recorded single field; lines 209-221 refuse the request with ResourceNotFoundException when store.insight_of_entity does not resolve the entity/id pair, which is the branch taken here, and lines 230-247 (the guidance fallback) are never reached.

## Steps to Reproduce
1. git clone the repo, build and boot the emulator from this checkout, and point an opensearch/es client at the data listener.
2. Replay opensearch-domain-insights-operations up to and including task#3 so the fixture's earlier steps establish the domain, then issue the recorded read: POST /2021-01-01/opensearch/insight-details with the fixture step's Entity and InsightId (boto3: opensearch.describe_insight_details(Entity=<the step's entity>, InsightId=<the step's GUID>)).
3. Observed: HTTP 404 with no Fields member.
   Expected: HTTP 200 with Fields = [{'Name': 'Message', 'Type': 'text', 'Value': 'Please use the Amazon OpenSearch Console to view the Insight Details for a better user experience.'}].
4. Confirm the shape of the rule rather than one value: repeat with a different well-formed GUID and a different Entity - AWS answers the same single guidance field, so the reply must not depend on the address resolving to stored state.
5. Confirm the negative path stays modeled: a malformed InsightId (right length, not 8-4-4-4-12 hex) must still be refused with ValidationException on 400, and any retained not-found refusal must be answered on 409 (ResourceNotFoundException), never 404.
6. Re-replay opensearch-domain-insights-operations#task#3 and confirm it passes before submitting.
