Issue-Source: trace-sweep-divergence
Issue-Trace: eserr-describe-instance-type-limits-t2-micro-invalid-type
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eserr-describe-instance-type-limits-t2-micro-invalid-type
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: es|DescribeInstanceTypeLimits|4xx->2xx|$.message
Issue-Key: es|DescribeInstanceTypeLimits|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: es|DescribeInstanceTypeLimits|4xx->2xx|status
Issue-Commit: ac00ac04e037c7b5605f6e580e441fd566e0efa2

# opensearch DescribeInstanceTypeLimits serves limits for an instance type the engine release does not offer instead of refusing it

> _Note: this issue was identified against the code base at commit `ac00ac04e037c7b5605f6e580e441fd566e0efa2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceTypeLimits

## Description
One root cause.

Cause 1 of 1: DescribeInstanceTypeLimits answers HTTP 200 with a full LimitsByRole map for a legacy burstable instance type that the engine release named in the request does not offer, where AWS refuses the pairing. The engine-scoping backstop in the handler does not classify this type as unoffered, so a negative path is served as a success.

AWS: Smithy com.amazonaws.opensearch#DescribeInstanceTypeLimits — GET /2021-01-01/opensearch/instanceTypeLimits/{EngineVersion}/{InstanceType}, with required @httpLabel members EngineVersion (VersionString, length 14..18, pattern ^Elasticsearch_[0-9]{1}\.[0-9]{1,2}$|^OpenSearch_[0-9]{1,2}\.[0-9]{1,2}$) and InstanceType (OpenSearchPartitionInstanceType enum); modeled errors include InvalidTypeException (errorKind client, httpError 409, "An exception for trying to create or access a sub-resource that's either invalid or not supported"), ValidationException 400, ResourceNotFoundException 409, LimitExceededException 409, BaseException 400, InternalException 500. Which instance types a given engine release offers is recorded behavior only; the recording shows that when the requested type is a member of the enum but is not offered for the requested engine release, AWS answers HTTP 409 with the error type carried in the x-amzn-errortype response header and body message 'Invalid instance type: <InstanceType>'. The fixed text of that message is 'Invalid instance type: '; its single variable part is the InstanceType taken verbatim from the request URI path — the engine release is NOT named in the message. The recorded probe exercises this with the legacy burstable type t2.micro.search, which the enum still contains but which the requested engine release does not offer.

Emulator: answers HTTP 200 with LimitsByRole populated. The replay reports 'status: expected 409, got 200', '$.message: missing (expected 'Invalid instance type: t2.micro.search', got None)' and '$headers.x-amzn-errortype: missing (expected <shaped-not-asserted>, got None)' — i.e. no error at all, so neither the status, nor the error type header, nor the message is produced. The handler's preceding checks do pass through: the engine version resolves and the type is in the published catalog, but the "does this release offer this type" predicate returns no refusal for it, so the handler falls through to publishing limits.

Why: a client that uses DescribeInstanceTypeLimits to decide whether a type can be used for a domain on a given engine release — capacity planners, instance-type pickers, engine-upgrade tooling, and the CreateDomain pre-flight that is specified to carry the identical refusal text — is told a legacy type IS available on that release. Against AWS the same call fails 409 InvalidTypeException, so the subsequent CreateDomain is refused; against the emulator the client proceeds on a type the service would never accept, and a negative-path test asserting the refusal passes vacuously.

Fix: the engine-release/instance-type offering predicate behind the DescribeInstanceTypeLimits handler's InvalidTypeException branch in the opensearch service crate (crate::state::unoffered_instance_type_refusal, consumed by ops/describe_instance_type_limits.rs) must treat a type the requested release does not offer — including the legacy t2/burstable generation on OpenSearch releases — as unoffered and raise InvalidTypeException with status 409, the x-amzn-errortype header for that shape, and the message 'Invalid instance type: ' + the request's InstanceType. The same predicate must govern the CreateDomain/UpdateDomainConfig refusal so one rule covers all three paths.

Related issues, which do NOT cover this cluster: 20261117T201942Z-es-instance-type-and-service-software-catalogs-are.md (open) concerns the ENUM VALUE SET rendered inside the 400 constraint message of DescribeInstanceTypeLimits and the stale 200 catalogs, not a missing 409 refusal; 20261025T185727Z-cloudformation-opensearch-sns-opensearch-describeinstancetypelimits-the-invalidtypeexception-message-for-an-instance.md is RESOLVED and concerned only the WORDING of a refusal that was already being raised for a newer-than-engine type — here no refusal is raised at all. The replay reported this cluster as unowned.

Traces: eserr-describe-instance-type-limits-t2-micro-invalid-type#task#2 (service es, operation DescribeInstanceTypeLimits, verdict mismatch, expected 409 / actual 200). Handler and branch to inspect: services/service-com-amazonaws-opensearch-amazonopensearchservice/src/ops/describe_instance_type_limits.rs lines 199-212 (the unoffered-type InvalidTypeException branch) and the predicate it calls in that crate's state module.

## Evidence
Replay cluster (verdict mismatch, service es, operation DescribeInstanceTypeLimits, fixture eserr-describe-instance-type-limits-t2-micro-invalid-type, step task#2, affected_count 1, stub_path empty, owned_by empty):
  expected_status 409, actual_status 200
  detail:
    status: expected 409, got 200
    $.message: missing (expected 'Invalid instance type: t2.micro.search', got None)
    $headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)

Expected (AWS): HTTP 409, error shape InvalidTypeException (announced in x-amzn-errortype), body message 'Invalid instance type: t2.micro.search' — fixed text 'Invalid instance type: ' plus the InstanceType value taken from the request URI; the engine release does not appear in the message.

Actual (emulator): HTTP 200 with a LimitsByRole map and no error members at all.

Smithy grounding (model_path services/service-com-amazonaws-opensearch-amazonopensearchservice/smithy-model/com-amazonaws-opensearch-amazonopensearchservice.json): DescribeInstanceTypeLimits errors = BaseException(400, defaulted), InternalException(500), InvalidTypeException(409), LimitExceededException(409), ResourceNotFoundException(409), ValidationException(400); input InstanceType -> OpenSearchPartitionInstanceType @required @httpLabel, EngineVersion -> VersionString @required @httpLabel; output LimitsByRole -> LimitsByRole map. InvalidTypeException: structure, errorKind client, httpError 409, member message -> ErrorMessage, doc 'An exception for trying to create or access a sub-resource that's either invalid or not supported.'

Corroborating that the emulator does have the refusal machinery and only misses this type: ops/describe_instance_type_limits.rs already raises InvalidTypeException via crate::state::unoffered_instance_type_refusal for types a release does not offer (lines 199-212), and the 409 path for a newer-than-engine type is exercised elsewhere; for this legacy type the predicate yields no refusal.

## Steps to Reproduce
1. Build and boot the emulator from this checkout; point an opensearch/es client at the data listener.
2. Issue the recorded read: GET /2021-01-01/opensearch/instanceTypeLimits/<EngineVersion>/t2.micro.search (boto3: opensearch.describe_instance_type_limits(EngineVersion=<the engine release in the fixture step>, InstanceType='t2.micro.search')).
3. Observed: HTTP 200 with LimitsByRole populated and no x-amzn-errortype header.
   Expected: HTTP 409, x-amzn-errortype naming InvalidTypeException, body message 'Invalid instance type: t2.micro.search'.
4. Confirm the sibling rule: CreateDomain with the same engine release and ClusterConfig.InstanceType='t2.micro.search' must be refused with the identical code and message text.
5. Re-replay eserr-describe-instance-type-limits-t2-micro-invalid-type#task#2 to confirm the cause is closed.
