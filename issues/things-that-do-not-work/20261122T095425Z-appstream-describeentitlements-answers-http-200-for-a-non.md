Issue-Source: trace-sweep-divergence
Issue-Trace: appstream-entitlement-lifecycle
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appstream-entitlement-lifecycle
Issue-Shape-Id: com.amazonaws.appstream#PhotonAdminProxyService
Issue-Key: appstream|DescribeEntitlements|4xx->2xx|$.Message
Issue-Key: appstream|DescribeEntitlements|4xx->2xx|$.__type
Issue-Key: appstream|DescribeEntitlements|4xx->2xx|status
Issue-Commit: 68c71f6585a425304635c9d8c1cc1d9408beb2ea

# appstream DescribeEntitlements answers HTTP 200 for a non-existent entitlement instead of EntitlementNotFoundException

> _Note: this issue was identified against the code base at commit `68c71f6585a425304635c9d8c1cc1d9408beb2ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEntitlements

## Description
Cause 1 of 1: DescribeEntitlements does not raise EntitlementNotFoundException when the request names an entitlement that does not exist on the stack (here, after the entitlement has been deleted earlier in the same lifecycle).

AWS: Smithy model services/service-com-amazonaws-appstream-photonadminproxyservice/smithy-model/com-amazonaws-appstream-photonadminproxyservice.json — operation DescribeEntitlements takes input DescribeEntitlementsRequest {StackName (required, @pattern ^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,100}$), Name (optional, same pattern), NextToken, MaxResults} and declares errors EntitlementNotFoundException (client, httpError 400), OperationNotPermittedException (400) and ResourceNotFoundException (404). EntitlementNotFoundException is documented "The entitlement can't be found." and carries a single Message member. So when the optional Name member is supplied and names no entitlement on the stack, the lookup is a single-entitlement lookup and AWS fails it rather than returning an empty page. The recorded AWS response is HTTP 400 with an AWS JSON 1.1 error body carrying __type = "EntitlementNotFoundException" and Message = "The requested entitlement does not exist." — that message is entirely fixed text for this error class (it interpolates neither the entitlement name nor the stack name), so it must be reproduced verbatim; the only variable parts of the response are the per-run request id in the headers.

Emulator: the step is answered HTTP 200 with a success body. No __type and no Message member are present at all ($.__type: missing, $.Message: missing), i.e. the handler treats the unknown Name as a successful empty/absent-filter describe instead of a failed lookup. Nothing in the replay suggests a wrong error code is emitted — the error path is simply never taken.

Why: a client that deletes an entitlement and then polls DescribeEntitlements to confirm the deletion, or that probes for an entitlement's existence before creating it, can never observe absence. Against AWS the call raises EntitlementNotFoundException, which SDKs surface as a typed, catchable exception; against the emulator the SDK returns a 200 model object, so existence checks come back "found" (or silently empty), delete-then-verify loops report the entitlement as still present, and error-handling branches guarded on EntitlementNotFoundException are dead code that is never exercised. Idempotent create/teardown flows therefore diverge and may loop or double-create.

Fix: the DescribeEntitlements handler for the appstream service — specifically its lookup/validation step. When the request carries a Name, the handler must resolve that entitlement within the named stack in service state and, on a miss, return EntitlementNotFoundException (HTTP 400, __type "EntitlementNotFoundException", Message "The requested entitlement does not exist.") rather than falling through to the list-and-serialize success path. The stack-level miss must keep returning ResourceNotFoundException (404) as modeled; only the entitlement-level miss is affected here.

Traces: appstream-entitlement-lifecycle#task#8 (expected 400 EntitlementNotFoundException, got 200). No open appstream issue covers this operation — the nearest open items (20261121T085721Z-appstream-describeimages-answers-http-400-with-no-images.md, 20261122T035842Z-appstream-associateapplicationtoentitlement-stub.md) are a different operation and an unimplemented stub respectively and do not carry this replay key.

## Evidence
Fixture appstream-entitlement-lifecycle, step task#8, operation DescribeEntitlements.

Expected (recorded AWS): HTTP 400, body $.__type = "EntitlementNotFoundException", $.Message = "The requested entitlement does not exist."

Actual (emulator): HTTP 200; $.__type missing (got None), $.Message missing (got None).

Replay divergence_key: service=appstream|DescribeEntitlements|status=400->200|keys=$.Message,$.__type,status — verdict mismatch, affected_count 1.

Model evidence: DescribeEntitlements errors = [EntitlementNotFoundException (httpError 400), OperationNotPermittedException (400), ResourceNotFoundException (404)]; EntitlementNotFoundException doc "The entitlement can't be found."; input member Name is optional with pattern ^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,100}$ and StackName is required.

## Steps to Reproduce
1. Replay fixture appstream-entitlement-lifecycle against the emulator (replay_wire_corpus).
2. The lifecycle creates a stack and an entitlement, then removes the entitlement.
3. At step task#8 it calls appstream DescribeEntitlements with the required StackName and the optional Name set to the entitlement that no longer exists.
4. Observe the emulator answers HTTP 200 with a success body and no error members.
5. Expected: HTTP 400 with AWS JSON 1.1 error body {"__type":"EntitlementNotFoundException","Message":"The requested entitlement does not exist."}.
