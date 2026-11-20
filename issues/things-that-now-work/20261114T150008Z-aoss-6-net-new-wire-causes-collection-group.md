Issue-Source: trace-sweep-divergence
Issue-Trace: opensearchserverless-collection-group-lifecycle
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: opensearchserverless-collection-group-lifecycle opensearchserverless-create-access-policy-idempotent-clienttoken opensearchserverless-encryption-security-policy-lifecycle opensearchserverless-vpc-endpoint-errors
Issue-Shape-Id: com.amazonaws.opensearchserverless#OpenSearchServerless
Issue-Key: aoss|ListCollectionGroups|2xx->2xx|$.collectionGroupSummaries[arn="arn:aws:aoss:us-west-2:000000000000:collection-group/e0o7knckwjwgc9qzpdl3"].storageConfiguration
Issue-Key: aoss|GetAccessPolicy|2xx->4xx|$.accessPolicyDetail
Issue-Key: aoss|GetAccessPolicy|2xx->4xx|status
Issue-Key: aoss|DeleteAccessPolicy|2xx->4xx|status
Issue-Key: aoss|UpdateSecurityPolicy|2xx->2xx|$.securityPolicyDetail.policyVersion
Issue-Key: aoss|GetSecurityPolicy|2xx->2xx|$.securityPolicyDetail.policyVersion
Issue-Key: aoss|CreateVpcEndpoint|4xx->4xx|$.message
Issue-Commit: c17029b5143eaf540f8fe3ae3fd6caada5435149

# aoss: 6 net-new wire causes — collection-group listing drops storageConfiguration, an idempotent-clientToken access policy cannot be read or deleted, UpdateSecurityPolicy mints a version AWS does not move, and CreateVpcEndpoint's VPC-absent message is invented

> _Note: this issue was identified against the code base at commit `c17029b5143eaf540f8fe3ae3fd6caada5435149`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListCollectionGroups

## Description
SIX net-new root causes from the wire-fixture replay of the `aoss` (OpenSearch Serverless) surface, all response-fidelity divergences on the policy / collection-group / VPC-endpoint control plane. Each cause is independent and must be closed on its own; all six fixture steps must be re-replayed before this issue is submitted.

=== Cause 1 of 6: ListCollectionGroups omits storageConfiguration from every collectionGroupSummaries row ===
AWS: recorded behavior only — the shape is not in the model as shipped. This repo's own model carries it as a local addition: smithy_query(shape="CollectionGroupStorageConfiguration") documents "the shape and the storageConfiguration member on CreateCollectionGroupDetail and CollectionGroupDetail are not in the model as shipped, and are carried here because the live OpenSearch Serverless API returns the block on every collection-group detail (recorded behaviour)". The recording of this fixture shows the LISTING row carrying it too: each collectionGroupSummaries entry has storageConfiguration {"v2Enabled": <bool>} — false for a CLASSIC-generation group, which is the only half a recording backs.
Emulator: the row has no storageConfiguration member at all (replay reports it `missing`, expected {'v2Enabled': False}, got None). The local addition was applied to CollectionGroupDetail and CreateCollectionGroupDetail only, so CollectionGroupSummary models no such member and the row builder cannot set one.
Why: a client that reads a group's storage mode off a listing — the cheap, paginated way to inspect every group — cannot tell a v2-storage group from a classic one, and code that dereferences summary.storageConfiguration.v2Enabled (present on every real AWS row) faults on a null.
Fix: the model plus the ListCollectionGroups row builder — add the same local-addition `storageConfiguration` member to CollectionGroupSummary that the two detail shapes already carry, and report it from the stored record the way the detail renderers do. Traces/citations: services/service-com-amazonaws-opensearchserverless-opensearchserverless/src/state.rs:1186-1195 (`CollectionGroupRecord::summary`, whose doc explicitly excludes storageConfiguration) vs :1152 and :1172 (the detail renderers) and :1210-1214 (`storage_configuration()`); handler src/ops/list_collection_groups.rs:178-185.
Traces: opensearchserverless-collection-group-lifecycle#task#2.

=== Cause 2 of 6: after an idempotent-clientToken CreateAccessPolicy probe, GetAccessPolicy for that policy is refused 400 instead of answering 200 with the policy ===
AWS: Smithy: GetAccessPolicy(type: AccessPolicyType @required, name: PolicyName @required, @length 3..32, @pattern ^[a-z][a-z0-9-]+$) -> 200 with `accessPolicyDetail` (AccessPolicyDetail: type, name, policyVersion, description, policy, createdDate, lastModifiedDate). Its modeled errors are InternalServerException (500), ResourceNotFoundException (404 — "Thrown when accessing or deleting a resource that does not exist") and ValidationException (400 — "Thrown when the HTTP request contains invalid input or is missing required input"). CreateAccessPolicy's `clientToken` is @idempotencyToken, so repeating the create with the same token answers the policy already created and leaves exactly one policy in place; the recording shows the read-back succeeding with the created policy's description, policy document and a policyVersion.
Emulator: status 400 and no `accessPolicyDetail` at all, for a well-formed read of the data policy the same fixture just created (type `data`, a 30-character name matching the modeled pattern). A 400 is wrong on both readings: if the policy exists the call must be 200, and if it genuinely does not exist the modeled refusal is ResourceNotFoundException with HTTP 404 — never ValidationException 400.
Why: a client cannot read back a policy it just created idempotently; worse, the status class says "your request was malformed" for a request AWS accepts, so retry/not-found handling takes a branch that neither re-reads nor recreates.
Fix: the access-policy create/replay path and the read handler in this crate — src/ops/create_access_policy.rs:243-322 (the clientToken replay branch and the `(type, name)` insert) with crate::state's token index (state.rs:3852-3863 `replayed_access_policy`, :3874+ `remember_token`, :4067-4078 the key/value encoding), and src/ops/get_access_policy.rs:161-197 (the only two 400-producing paths are the type-enum guard and the boundary validator; the not-found path is already 404). Hypothesis: after the token-replayed create the policy is not reachable under the `(type, name)` key the read addresses, or the read's own input guard refuses the well-formed probe — the emulator's 400 error code and message for this step identify which, and the fix must make the read answer 200 and keep 404 for a truly absent policy.
Traces: opensearchserverless-create-access-policy-idempotent-clienttoken#task#4.

=== Cause 3 of 6: DeleteAccessPolicy for that same policy is refused 400 instead of 200 ===
AWS: Smithy: DeleteAccessPolicy(type @required, name @required, clientToken @idempotencyToken) models NO output members, so a successful delete is HTTP 200 with an empty body; its modeled errors are ConflictException (409), InternalServerException (500), ResourceNotFoundException (404) and ValidationException (400). The recording shows the teardown delete of the policy the fixture created answering 200.
Emulator: status 400. As with Cause 2, 400 is reachable only through an input refusal — the handler's own missing-record path is ResourceNotFoundException (404) — so the emulator both fails to delete an existing policy and would misclassify an absent one.
Why: the fixture's resource is never cleaned up, and a client's teardown path sees a malformed-input error for a well-formed delete, so it neither retries nor treats the policy as gone — the account is left holding a policy the client believes it removed.
Fix: same component as Cause 2 — src/ops/delete_access_policy.rs:165-207 plus the access-policy state keyed by `(type, name)` (state.rs `remove_access_policy`). The delete must answer 200 with an empty body for the policy the create stored, and reserve 400 for invalid input only.
Traces: opensearchserverless-create-access-policy-idempotent-clienttoken#teardown#5.

=== Cause 4 of 6: UpdateSecurityPolicy mints a fresh policyVersion where the recording keeps the version the policy already carries ===
AWS: Smithy: UpdateSecurityPolicy requires `policyVersion` (PolicyVersion, @length 20..36, @pattern ^([0-9a-zA-Z+/]{4})*(([0-9a-zA-Z+/]{2}==)|([0-9a-zA-Z+/]{3}=))?$) and answers `securityPolicyDetail` (SecurityPolicyDetail.policyVersion = "The version of the policy"); `clientToken` is @idempotencyToken. Recorded behavior: at this step the update's response reports the SAME policyVersion that the fixture captured from an earlier step of the same encryption-policy lifecycle — the version does not move across this call. (The replay asserts a concrete 20-character token here, which is the value the emulator itself minted earlier in this run and the harness correlated; a value the recording could not correlate is reported as `<shaped-not-asserted>`, as the createdDate/lastModifiedDate members in this corpus are.)
Emulator: the update mints a brand-new policyVersion and stores it, so the response reports a token the client has never seen (replay: expected the correlated version, got a freshly minted one).
Why: policyVersion is the optimistic-concurrency precondition of every Update*Policy call. A client that holds the version AWS reported — or that retries the same update under its clientToken — finds the token it holds is no longer the stored one, so its next update is refused as stale and it must re-read; and one logical write is recorded as two versions.
Fix: the UpdateSecurityPolicy handler's version/idempotency path — src/ops/update_security_policy.rs:238-327 (the clientToken replay branch, the precondition check, and the `crate::state::mint_policy_version()` call at :299) together with state.rs:3837-3848 (`replayed_security_policy`) and :3874+ (`remember_token`). Hypothesis: the step is a retry under a clientToken already spent (or an update that changes nothing), which must answer the stored record untouched instead of performing a second write; the replay branch is not taking effect for it.
Traces: opensearchserverless-encryption-security-policy-lifecycle#task#4.

=== Cause 5 of 6: GetSecurityPolicy then reports that same unexpected policyVersion, so the divergence is persisted in state ===
AWS: Smithy: GetSecurityPolicy(type @required, name @required) -> 200 with securityPolicyDetail.policyVersion. Recorded behavior: the read that follows the update reports the version correlated earlier in the fixture — the same value the update answered, unchanged.
Emulator: the read reports the version the update minted (the same wrong token as Cause 4), confirming the extra version was written to the store rather than only mis-serialized on the update response.
Why: the wrong version is now the policy's state, so every later reader and every optimistic-concurrency precondition in the account is keyed on a value AWS never issued.
Fix: no separate serializer defect — this closes with Cause 4's state write (src/ops/update_security_policy.rs:298-323, `record.apply(write)` with a freshly minted version). It is listed as its own cause so the read-back step is re-replayed after the fix. Citation: src/ops/get_security_policy.rs renders the stored record only.
Traces: opensearchserverless-encryption-security-policy-lifecycle#task#5.

=== Cause 6 of 6: CreateVpcEndpoint's refusal for a vpcId that names no VPC carries this emulator's invented message instead of AWS's ===
AWS: Smithy: CreateVpcEndpoint(name @required, vpcId @required (@pattern ^vpc-[0-9a-z]*$), subnetIds @required, securityGroupIds, clientToken) models ConflictException (409), InternalServerException (500), ServiceQuotaExceededException (402) and ValidationException (400 — "the HTTP request contains invalid input"); the error body's only member is `message`. Recorded behavior: the status class is right (400), but the recorded `message` for a vpcId that resolves to no VPC differs from what the emulator sends. The report elides the recorded value as `<shaped-not-asserted>`, so the fixer must read the fixture's recorded message and reproduce AWS's wording: its fixed text verbatim, with only the `vpcId` the REQUEST carried as the variable part (the id is taken from the request, not from a per-run value).
Emulator: 400 ValidationException with `The Vpc with Id '<vpcId>' does not exist.` — a sentence this crate invented by analogy with its own collection refusal, as its own doc admits ("The sentence is this service's own, in the form it words a resource an id addresses that is not there"). The variable part is correct (the request's vpcId, single-quoted); it is the fixed text that is not AWS's.
Why: the message is the only human- and matcher-readable carrier of this refusal (the code is the generic ValidationException), so a client or test keyed on AWS's text does not recognise an unplaceable VPC, and operators see a sentence real AWS never emits.
Fix: the VPC-absence refusal wording used by CreateVpcEndpoint (and UpdateVpcEndpoint, which shares it) — services/service-com-amazonaws-opensearchserverless-opensearchserverless/src/state/ec2_network.rs:95-97 (`vpc_absent_message`), reached from src/ops/create_vpc_endpoint.rs:214-228. The sibling `subnet_absent_message` (:101-103) is worded from the same template and should be re-grounded against its own recording at the same time.
Traces: opensearchserverless-vpc-endpoint-errors#task#1.

NOTE on existing issues: no open issue carries these replay keys (the replay reported `owned_by` empty for all six). The open opensearchserverless issues 20261029T191222Z-... (Cause 2: BatchGetVpcEndpoint's NOT_FOUND errorMessage) and 20261030T003418Z-... (Cause 1: UpdateCollectionGroup's dead-id refusal class) concern different operations and different messages and do not cover Cause 6 or any other cause here.

## Evidence
Replay: trace_set simparity-wire-aws-v1, emulator_up true, 150 steps, 142 passed, 6 mismatched, score 94.7%. All six clusters are service `aoss`, verdict `mismatch`, stub_path empty, owned_by empty.

Cause 1 — opensearchserverless-collection-group-lifecycle#task#2, ListCollectionGroups, status 200 expected / 200 actual: `$.collectionGroupSummaries[arn="arn:aws:aoss:<region>:<account>:collection-group/<id>"].storageConfiguration: missing (expected {'v2Enabled': False}, got None)`. The arn/id in the key are per-run values; the defect is the absent member.

Cause 2 — opensearchserverless-create-access-policy-idempotent-clienttoken#task#4, GetAccessPolicy: `status: expected 200, got 400` and `$.accessPolicyDetail: missing`. The expected detail was {createdDate: <not asserted>, description: 'idempotent clientToken probe', lastModifiedDate: <not asserted>, name: <the probe policy name>, policy: [{Rules: [{Resource: ['collection/*'], Permission: ['aoss:DescribeCollectionItems'], ResourceType: 'collection'}], Principal: ['arn:aws:iam::<account>:root']}], policyVersion: <correlated>, type: 'data'} — the name, account and policyVersion are per-run values; the defect is the 400 and the absent detail.

Cause 3 — opensearchserverless-create-access-policy-idempotent-clienttoken#teardown#5, DeleteAccessPolicy: `status: expected 200, got 400` (AWS: 200 with an empty body, since the operation models no output members).

Cause 4 — opensearchserverless-encryption-security-policy-lifecycle#task#4, UpdateSecurityPolicy, status 200/200: `$.securityPolicyDetail.policyVersion: value_mismatch (expected 'NcrEOy6zQU0ZmlaZfMxX', got '4UZSRI2RCueyotiK0k1k')`. Both tokens are per-run minted values; the requirement is the EQUALITY — the version correlated from the earlier step of the fixture must still be the one reported here.

Cause 5 — opensearchserverless-encryption-security-policy-lifecycle#task#5, GetSecurityPolicy, status 200/200: the same `$.securityPolicyDetail.policyVersion` mismatch with the same pair of values, i.e. the update's extra version is what the store now holds.

Cause 6 — opensearchserverless-vpc-endpoint-errors#task#1, CreateVpcEndpoint, status 400 expected / 400 actual: `$.message: value_mismatch (expected '<shaped-not-asserted>', got "The Vpc with Id 'vpc-00000000000000000' does not exist.")`. The vpc id is the value the request carried; the diverging part is the sentence around it.

## Steps to Reproduce
Replay the corpus against the emulator built from this checkout (replay_wire_corpus), or exercise the steps directly against the `aoss` endpoint:

Cause 1: CreateCollectionGroup(name=<g>) (no `generation`, i.e. CLASSIC), then ListCollectionGroups() and inspect the row for that group — expected storageConfiguration {"v2Enabled": false}; observed no storageConfiguration member. Fixture: opensearchserverless-collection-group-lifecycle#task#2.

Causes 2 and 3: CreateAccessPolicy(type='data', name=<p>, description='idempotent clientToken probe', policy=<a data policy granting aoss:DescribeCollectionItems on collection/* to arn:aws:iam::<account>:root>, clientToken=<T>); repeat the create with the same clientToken <T>; then GetAccessPolicy(type='data', name=<p>) -> expected 200 with accessPolicyDetail, observed 400; then DeleteAccessPolicy(type='data', name=<p>) -> expected 200 empty body, observed 400. Fixture: opensearchserverless-create-access-policy-idempotent-clienttoken#task#4 and #teardown#5.

Causes 4 and 5: run the encryption security-policy lifecycle — CreateSecurityPolicy(type='encryption', ...), read it, then the fixture's UpdateSecurityPolicy(type='encryption', name=<p>, policyVersion=<the version then current>, clientToken=<T>) step -> expected the response's securityPolicyDetail.policyVersion to equal the version correlated earlier, observed a freshly minted token; then GetSecurityPolicy(type='encryption', name=<p>) -> reports the same freshly minted token. Fixture: opensearchserverless-encryption-security-policy-lifecycle#task#4 and #task#5.

Cause 6: CreateVpcEndpoint(name=<n>, vpcId=<an id no VPC of the account carries>, subnetIds=[<id>]) -> 400 ValidationException; compare its `message` with the fixture's recorded message. Fixture: opensearchserverless-vpc-endpoint-errors#task#1.
