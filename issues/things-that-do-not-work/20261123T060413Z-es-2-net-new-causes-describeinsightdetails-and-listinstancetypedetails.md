Issue-Source: trace-sweep-divergence
Issue-Trace: opensearch-describe-insight-details-not-found
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: opensearch-describe-insight-details-not-found opensearch-list-instance-type-details-instance-filter-without-azs-rejected
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: es|DescribeInsightDetails|4xx->2xx|$.message
Issue-Key: es|DescribeInsightDetails|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: es|DescribeInsightDetails|4xx->2xx|status
Issue-Key: es|ListInstanceTypeDetails|4xx->2xx|$.message
Issue-Key: es|ListInstanceTypeDetails|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: es|ListInstanceTypeDetails|4xx->2xx|status
Issue-Commit: 176f9573b750fec83509ce8fc4c12c088941c937

# es: 2 net-new causes — DescribeInsightDetails and ListInstanceTypeDetails answer 200 instead of the modeled refusal

> _Note: this issue was identified against the code base at commit `176f9573b750fec83509ce8fc4c12c088941c937`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInsightDetails

## Description
Shared fault: two OpenSearch Service (`es`) read paths have NO refusal branch at all. Both requests are ones AWS refuses, and the emulator answers each with a plain HTTP 200 success envelope — no status, no `x-amzn-errortype` header, no `message` member. The defect is a missing validation/lookup-failure branch in each operation's handler, not a wording difference: there is nothing to re-word because no error is produced.

Cause 1 of 2: DescribeInsightDetails does not refuse an insight that does not exist.
AWS: ResourceNotFoundException is a modeled error of DescribeInsightDetails (smithy_query operation=DescribeInsightDetails on services/service-com-amazonaws-opensearch-amazonopensearchservice/smithy-model/com-amazonaws-opensearch-amazonopensearchservice.json: errors include ResourceNotFoundException, ValidationException, DisabledOperationException, LimitExceededException, InternalException, BaseException; input members Entity (InsightEntity, required) and InsightId (GUID, required, @length 36..36, @pattern ^\p{XDigit}{8}-\p{XDigit}{4}-\p{XDigit}{4}-\p{XDigit}{4}-\p{XDigit}{12}$); output member Fields is required; HTTP binding POST /2021-01-01/opensearch/insight-details). ResourceNotFoundException is documented as "An exception for accessing or deleting a resource that doesn't exist" and carries a single `message` member. For the status and error code the recording is authoritative: a well-formed InsightId/Entity pair naming an insight that is not present is answered HTTP 404 with header `x-amzn-errortype: ResourceNotFoundException` and body `{"message": "Resource not found"}`. Note the model's own httpError for ResourceNotFoundException is 409; the recorded wire status for this path is 404, so 404 is the requirement here. The message `Resource not found` is entirely FIXED text — it interpolates no part of the request and no state value, so there is no variable part to template.
Emulator: answers HTTP 200. The replay reports `status: expected 404, got 200`, `$.message: missing (expected 'Resource not found', got None)` and `$headers.x-amzn-errortype: missing (expected 'ResourceNotFoundException', got None)` — i.e. a success response is serialized for an insight that was never created.
Why: a client cannot distinguish "this insight does not exist" from "this insight exists and has no fields". Every AWS SDK decides error-vs-success from the HTTP status and the `x-amzn-errortype` header, so no SDK raises ResourceNotFoundException here; the call returns a DescribeInsightDetailsResponse whose required `Fields` member is absent or empty. Retry/`catch ResourceNotFoundException` logic, provisioning waiters that poll an insight until it appears, and any code that treats a 200 as proof of existence all take the wrong branch, silently.
Fix: the DescribeInsightDetails handler — its insight lookup must fail closed, raising ResourceNotFoundException (404, errortype `ResourceNotFoundException`, message `Resource not found`) when the Entity/InsightId pair resolves to no stored insight, instead of falling through to the success serializer.
Traces: opensearch-describe-insight-details-not-found#task#1.

Cause 2 of 2: ListInstanceTypeDetails does not refuse an InstanceType filter supplied without RetrieveAZs.
AWS: smithy_query operation=ListInstanceTypeDetails on the same model shows EngineVersion as a required @httpLabel (@length 14..18, @pattern ^Elasticsearch_[0-9]{1}\.[0-9]{1,2}$|^OpenSearch_[0-9]{1,2}\.[0-9]{1,2}$) and InstanceType (@httpQuery instanceType, @length 10..40, @pattern ^.*\..*\.search$), RetrieveAZs (@httpQuery retrieveAZs, boolean) and DomainName/MaxResults/NextToken as optional query members, with ValidationException (HTTP 400) and BaseException (HTTP 400, defaulted) modeled; HTTP binding GET /2021-01-01/opensearch/instanceTypeDetails/{EngineVersion}. The model does not state the interaction between InstanceType and RetrieveAZs, so for that rule this is recorded behavior only: the recording shows that filtering by InstanceType is only meaningful once RetrieveAZs asks the service to resolve the type against Availability Zones, so an InstanceType filter sent WITHOUT RetrieveAZs is refused with HTTP 400 and the message `Retrieve AZ flag need to be enabled to get Instance Type AZ information.` The recording does not assert the error code/errortype for this path (the replay records the expected `x-amzn-errortype` as `<shaped-not-asserted>`), so the requirement is the 400 status and that message; the message is wholly FIXED text, interpolating nothing from the request or state.
Emulator: answers HTTP 200 with the full instance-type catalog. The replay reports `status: expected 400, got 200`, `$.message: missing (expected 'Retrieve AZ flag need to be enabled to get Instance Type AZ information.', got None)` and `$headers.x-amzn-errortype: missing`.
Why: a client that filters by InstanceType and forgets RetrieveAZs believes its filter was honored, because it gets a 200 and a populated InstanceTypeDetails list. AWS makes that programming error loud; the emulator makes it silent, and the client then reads AZ information that was never resolved (AvailabilityZones is null on every row) as if it were authoritative.
Fix: the ListInstanceTypeDetails handler's input validation — add the precondition that an InstanceType filter requires RetrieveAZs, refusing with 400 and the message above before the catalog is assembled. The open issue 20261117T190848Z-opensearch-listinstancetypedetails-re-words-the-az-unsupported-instancetype.md concerns the WORDING of a different refusal on this operation (the AZ-unsupported instance type, `The AZ doesn't support instance type :<type>`) and explicitly argues that a request without RetrieveAZs should NOT be graded; it therefore does not cover this cause and its reasoning must be reconciled with this recording, which shows AWS refusing precisely the no-RetrieveAZs case when an InstanceType filter is present.
Traces: opensearch-list-instance-type-details-instance-filter-without-azs-rejected#task#1.

## Evidence
Cause 1 — fixture opensearch-describe-insight-details-not-found, step task#1, operation DescribeInsightDetails, verdict mismatch.
  Expected (AWS, recorded): HTTP 404; header x-amzn-errortype: ResourceNotFoundException; body $.message == "Resource not found".
  Actual (emulator): HTTP 200; $.message absent; x-amzn-errortype absent.
  Replay detail verbatim: "status: expected 404, got 200 / $.message: missing (expected 'Resource not found', got None) / $headers.x-amzn-errortype: missing (expected 'ResourceNotFoundException', got None)".
  divergence_key: service=es|DescribeInsightDetails|status=404->200|keys=$.message,$headers.x-amzn-errortype,status
  Fixed vs variable text: "Resource not found" is entirely fixed; no request or state value is interpolated.

Cause 2 — fixture opensearch-list-instance-type-details-instance-filter-without-azs-rejected, step task#1, operation ListInstanceTypeDetails, verdict mismatch.
  Expected (AWS, recorded): HTTP 400; body $.message == "Retrieve AZ flag need to be enabled to get Instance Type AZ information."; error shape not asserted by the recording (<shaped-not-asserted>).
  Actual (emulator): HTTP 200 with a populated InstanceTypeDetails list; $.message absent; x-amzn-errortype absent.
  Replay detail verbatim: "status: expected 400, got 200 / $.message: missing (expected 'Retrieve AZ flag need to be enabled to get Instance Type AZ information.', got None) / $headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)".
  divergence_key: service=es|ListInstanceTypeDetails|status=400->200|keys=$.message,$headers.x-amzn-errortype,status
  Fixed vs variable text: the whole sentence is fixed; nothing is interpolated.

Model evidence (both causes): services/service-com-amazonaws-opensearch-amazonopensearchservice/smithy-model/com-amazonaws-opensearch-amazonopensearchservice.json — DescribeInsightDetails errors = [BaseException(400, defaulted), DisabledOperationException(409), InternalException(500), LimitExceededException(409), ResourceNotFoundException(409), ValidationException(400)]; ListInstanceTypeDetails errors = [BaseException(400, defaulted), InternalException(500), ResourceNotFoundException(409), ValidationException(400)]. Both error shapes carry exactly one `message` member (ErrorMessage).

## Steps to Reproduce
Cause 1 (DescribeInsightDetails):
1. Boot the emulator and point an OpenSearch Service (`es`) client at its data listener.
2. Positive control: a DescribeInsightDetails call for an insight that DOES exist must answer 200 with a Fields list, so the 200 below is not just "the operation is unimplemented".
3. Call DescribeInsightDetails with a well-formed Entity and an InsightId that satisfies the GUID pattern (36 chars, 8-4-4-4-12 hex) but names no stored insight.
4. Expect HTTP 404, header x-amzn-errortype: ResourceNotFoundException and body {"message": "Resource not found"}. Observe HTTP 200 with no message and no errortype header.
5. Re-replay to confirm a fix: fixture opensearch-describe-insight-details-not-found, step task#1.

Cause 2 (ListInstanceTypeDetails):
1. Against the same emulator, positive control: ListInstanceTypeDetails(EngineVersion='OpenSearch_2.11') answers 200 with a non-empty InstanceTypeDetails list.
2. Call ListInstanceTypeDetails with a valid EngineVersion and an InstanceType query filter matching ^.*\..*\.search$, and WITHOUT the retrieveAZs query parameter.
3. Expect HTTP 400 whose body message is exactly "Retrieve AZ flag need to be enabled to get Instance Type AZ information.". Observe HTTP 200 carrying the instance-type catalog.
4. Contrast: the same request WITH retrieveAZs=true must not take this refusal branch.
5. Re-replay to confirm a fix: fixture opensearch-list-instance-type-details-instance-filter-without-azs-rejected, step task#1.
