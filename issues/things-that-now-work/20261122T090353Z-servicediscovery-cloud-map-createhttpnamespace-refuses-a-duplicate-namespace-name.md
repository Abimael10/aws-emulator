Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: servicediscovery.namespace-already-exists-error
Issue-Shape-Id: com.amazonaws.servicediscovery#Route53AutoNaming_v20170314
Issue-Key: servicediscovery.namespace-already-exists-error|run#14|AssertionError|assert code == "DuplicateRequest", f"duplicate namespace name must be rejected as DuplicateRequest, got {code}"
Issue-Commit: 58e8e75a7b1ef1cb8dd0bc31b7ab38eaf68cbf98

# Cloud Map CreateHttpNamespace refuses a duplicate namespace Name with NamespaceAlreadyExists instead of DuplicateRequest naming the completed create operation

> _Note: this issue was identified against the code base at commit `58e8e75a7b1ef1cb8dd0bc31b7ab38eaf68cbf98`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateHttpNamespace

## Description
REOPENS 20261022T013520Z-servicediscovery-cloud-map-control-plane-diverges-createservice-accepts-dnsconfig.md: its fix left the call refused. That issue's Cause 3 of 8 is this exact check (servicediscovery.namespace-already-exists-error) and recorded the same divergence; the issue is marked fixed, but the check still fails on the same refusal, now very likely because the later fix of 20261104T035823Z-servicediscovery-cloud-map-5-net-new-behavioral-causes-same.md (its Cause 1, which correctly re-classified the *private DNS* same-name race as NamespaceAlreadyExists) was applied to the shared namespace-create path instead of per-operation, so CreateHttpNamespace now answers the private-DNS classification. This is ONE root cause.

Cause 1 of 1: a second CreateHttpNamespace for a Name a stored namespace already answers to is refused NamespaceAlreadyExists (HTTP 400) where AWS refuses DuplicateRequest (HTTP 409) naming the already-completed create operation.

AWS: Smithy CreateHttpNamespace{Name (required, NamespaceNameHttp, @length 0..1024, @pattern ^(?!arn:)[!-~]{1,1024}$), CreatorRequestId (@idempotencyToken, @length 0..64), Description, Tags} -> {OperationId}; modeled errors DuplicateRequest (HTTP 409, doc 'The operation is already in progress', member DuplicateOperationId doc 'The ID of the operation that's already in progress'), InvalidInput (400), NamespaceAlreadyExists (400, members NamespaceId/CreatorRequestId), ResourceLimitExceeded (400), TooManyTagsException (400) — model path services/service-com-amazonaws-servicediscovery-route53autonaming-v20170314/smithy-model/com-amazonaws-servicediscovery-route53autonaming-v20170314.json. Recorded behavior: for an HTTP namespace whose create operation has already settled SUCCESS, a second CreateHttpNamespace with the same Name (sent with a DIFFERENT, SDK-generated CreatorRequestId — the check passes no CreatorRequestId, so botocore fills the idempotency token with a fresh value each call) is refused with Code 'DuplicateRequest' and Message 'Another operation of type CreateHttpNamespace and id <operation-id> has completed'. In that message the fixed text is 'Another operation of type ', ' and id ' and ' has completed'; the variable parts are the operation TYPE name of the colliding create (CreateHttpNamespace) and the OperationId of the create operation that already owns the name, taken from the emulator's own state (the NamespaceRecord already stores it as create_operation_id). The refusal must not create a second namespace: ListNamespaces filtered NAME EQ <name> still returns exactly the original namespace id. Once the original namespace is deleted (GetNamespace settles to NamespaceNotFound) the identical CreateHttpNamespace under that Name succeeds, so the name is rejected only while it is in use. Note this is NOT the same classification as the private-DNS same-name race, which the fixed issue 20261104T035823Z recorded as NamespaceAlreadyExists — the two creates classify the conflict differently and the emulator must keep both.

Emulator: Code 'NamespaceAlreadyExists' (HTTP 400, carrying the existing NamespaceId) for the duplicate CreateHttpNamespace.

Why: the code and its status are the whole contract for a client here. DuplicateRequest is the 409 'an operation for this is already/has been filed — go read that operation' shape, and its DuplicateOperationId is how a caller reaches the create that owns the name; NamespaceAlreadyExists is a 400 terminal validation failure. Retry loops, CloudFormation-style reconcilers and idempotent provisioners branch on exactly this distinction, so against the emulator the DuplicateRequest branch never runs, the colliding operation id is unreachable, and a caller that treats 409 as 'poll the existing operation' instead sees a hard 400.

Fix: the name-conflict classification in CreateHttpNamespace's handler (services/service-com-amazonaws-servicediscovery-route53autonaming-v20170314/src/ops/create_http_namespace.rs, the stored-name branch) and the shared namespace-conflict helper it calls in that crate's state — make the classification per-operation rather than one shared refusal: CreateHttpNamespace raises DuplicateRequest with DuplicateOperationId and the message template above, built from the existing namespace record's create operation id, while the private-DNS create keeps NamespaceAlreadyExists. The idempotent-replay path (same CreatorRequestId + same Name returns the original OperationId) and the 'delete frees the name' behavior are unchanged.

Traces: servicediscovery.namespace-already-exists-error.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: 96/101 passed; servicediscovery 76/77 (99%), 1 failed.

[mismatch] servicediscovery.namespace-already-exists-error, at check line 88:
  assert code == "DuplicateRequest", f"duplicate namespace name must be rejected as DuplicateRequest, got {code}"
  AssertionError: duplicate namespace name must be rejected as DuplicateRequest, got NamespaceAlreadyExists

Sequence replayed: create_http_namespace(Name=<unique>, Tags=[...]) -> GetOperation polled to SUCCESS, namespace id taken from Operation.Targets['NAMESPACE'] and the create's OperationId retained; then create_http_namespace(Name=<the same name>) with no CreatorRequestId (botocore fills a fresh idempotency token). Expected (recorded AWS): ClientError Code 'DuplicateRequest' (HTTP 409), Message 'Another operation of type CreateHttpNamespace and id <the first create's OperationId> has completed' — the operation id is this run's value filling the template, not part of it. Got: Code 'NamespaceAlreadyExists' (HTTP 400). The check stopped at the code assertion, so the message assertion, the ListNamespaces NAME-EQ read (must still show only the original namespace id) and the restore leg (delete the original, poll GetNamespace to NamespaceNotFound, then the identical create must succeed and GetNamespace report that Name) were not reached and must also hold after the fix.

Source of the divergence in the checkout: services/service-com-amazonaws-servicediscovery-route53autonaming-v20170314/src/ops/create_http_namespace.rs lines 204-219 return CreateHttpNamespaceErrorModeled::NamespaceAlreadyExists(crate::state::namespace_already_exists(existing)) for any Name found in state.namespace_ids_by_name, and the module doc at lines 7-8 states that rule; the namespace record already carries create_operation_id (line 256), which is the value the DuplicateRequest message and DuplicateOperationId need.

## Steps to Reproduce
Boot the emulator from this checkout and replay servicediscovery.namespace-already-exists-error, or drive the endpoint with botocore (sd = boto3.client('servicediscovery', endpoint_url=<emulator>)) and read e.response['Error']['Code'], ['Message'] and ResponseMetadata['HTTPStatusCode']:
1. r = sd.create_http_namespace(Name='<unique-name>'); poll sd.get_operation(OperationId=r['OperationId'])['Operation'] until Status == 'SUCCESS'; keep ns = Operation['Targets']['NAMESPACE'] and op1 = r['OperationId'].
2. sd.create_http_namespace(Name='<the same name>') again (no CreatorRequestId, so the SDK sends a different idempotency token) -> the emulator raises NamespaceAlreadyExists (HTTP 400). AWS raises DuplicateRequest (HTTP 409) with Message 'Another operation of type CreateHttpNamespace and id ' + op1 + ' has completed' and DuplicateOperationId = op1.
3. sd.list_namespaces(Filters=[{'Name':'NAME','Values':['<the name>'],'Condition':'EQ'}]) -> must list exactly [ns]; the refused duplicate must not have landed.
4. sd.delete_namespace(Id=ns), poll its operation to SUCCESS and sd.get_namespace(Id=ns) until it raises NamespaceNotFound; then repeat step 1's call with the same Name -> it must succeed and sd.get_namespace(Id=<new id>)['Namespace']['Name'] must be that name (the name is refused only while in use).
5. Confirm the private-DNS classification is untouched: two concurrent sd.create_private_dns_namespace(Name=..., Vpc=..., CreatorRequestId=<distinct>) calls must still refuse the loser with NamespaceAlreadyExists (fixed issue 20261104T035823Z, Cause 1).
