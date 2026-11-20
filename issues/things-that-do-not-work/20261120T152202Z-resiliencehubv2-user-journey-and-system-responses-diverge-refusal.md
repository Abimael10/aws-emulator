Issue-Source: trace-sweep-divergence
Issue-Trace: resiliencehubv2-user-journey-lifecycle
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: resiliencehubv2-user-journey-lifecycle resiliencehubv2-service-function-lifecycle resiliencehubv2-system-lifecycle resiliencehubv2-system-validation-and-conflict-errors
Issue-Shape-Id: com.amazonaws.resiliencehubv2#NGRHServiceCore
Issue-Key: resiliencehubv2|CreateUserJourney|4xx->4xx|$.message
Issue-Key: resiliencehubv2|CreateUserJourney|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: resiliencehubv2|CreateUserJourney|4xx->4xx|status
Issue-Key: resiliencehubv2|GetUserJourney|4xx->2xx|$.message
Issue-Key: resiliencehubv2|GetUserJourney|4xx->2xx|$.resourceId
Issue-Key: resiliencehubv2|GetUserJourney|4xx->2xx|$.resourceType
Issue-Key: resiliencehubv2|GetUserJourney|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: resiliencehubv2|GetUserJourney|4xx->2xx|status
Issue-Key: resiliencehubv2|CreateServiceFunctionResources|4xx->4xx|$.message
Issue-Key: resiliencehubv2|CreateSystem|2xx->2xx|$.system.systemId
Issue-Commit: 53763dd8e71a257e04fb6c212b98aad1027c99f7

# resiliencehubv2 user-journey and system responses diverge: refusal class, deleted-journey reads, refusal wording and minted id shape

> _Note: this issue was identified against the code base at commit `53763dd8e71a257e04fb6c212b98aad1027c99f7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateUserJourney

## Description
FOUR distinct net-new root causes in resiliencehubv2, found by replaying the wire fixture corpus simparity-wire-aws-v1 against the emulator built from this checkout. A fixer must close ALL FOUR (Cause 1 of 4 .. Cause 4 of 4) before submitting, and must re-replay every fixture#step named under Traces, not only the representative step in the front matter.

Related issues, none of which covers these wire steps: 20261119T233720Z-resiliencehubv2-createservicefunctionresources-undiscovered-resource-validationexception-message-is-reworded.md describes the same fault as Cause 3 but is a BEHAVIORAL issue (Issue-Corpus: simparity-behavioral-aws-v1) carrying NO wire replay key, so the fixture step in Cause 3 stays un-gated by it; 20261116T044615Z-resiliencehubv2-4-net-new-causes-system-lifecycle-existence.md is RESOLVED and its cause 4 attempted the systemId shape of Cause 4 below (the UUID version nibble) without making the shaped assertion pass; the DeleteUserJourney scaffold's own stub issue (auto-filed for the blocked 501 step) covers that operation being unwritten, but not the post-delete READ contract of Cause 2.

Cause 1 of 4: CreateUserJourney refuses a systemArn that is pattern-valid but does not name a system of this service with the WRONG status and error class — 404 ResourceNotFoundException instead of AWS's 400 ARN-validation refusal.
AWS: Smithy CreateUserJourney (POST /v2/create-user-journey, success 200) takes @required systemArn -> Arn (@length min 31, pattern ^arn:(aws|aws-cn|aws-iso|aws-iso-[a-z]{1}|aws-us-gov):[A-Za-z0-9][A-Za-z0-9_/.-]{0,62}:([a-z]{2}-((iso[a-z]{0,1}-)|(gov-)){0,1}[a-z]+-[0-9]):[0-9]{12}:[A-Za-z0-9/][A-Za-z0-9:_/+.-]{0,1023}$) and models ValidationException (@error client, httpError 400, @required message) beside ResourceNotFoundException (404). The generic Arn pattern admits any resource segment, so an ARN whose resource segment is not this service's system/<id> form satisfies the pattern yet names no system; recorded behavior for such a value is HTTP 400 with an x-amzn-errortype header for the 400 class and body message 'ARN validation error' — the whole message is FIXED text, with no variable part interpolated from the request. The ARN-shape verdict is reached BEFORE any store lookup, so the caller is told the ARN itself is wrong rather than that the resource is absent.
Emulator: the handler skips the ARN-shape check and goes straight to the store, so the call answers HTTP 404 with x-amzn-errortype: ResourceNotFoundException and message 'System not found: <resource segment of the supplied ARN>' (the replayed step shows 'System not found: service/fake:ffffff' for a systemArn whose resource segment is service/fake:ffffff; that segment is the per-run value filling the template, not part of the requirement).
Why: a client cannot distinguish 'you sent me a malformed/wrong-type ARN' (a 400 the caller must fix, never retried) from 'the system is gone' (a 404 that reconciliation and teardown treat as success). SDK retry/404-tolerant code paths swallow a programming error, and a typed ARN mistake surfaces as a missing resource instead of a validation failure.
Fix: CreateUserJourney's input validation (the branch in the operation handler that resolves systemArn) — verify the ARN names a system of this service before resolving existence, and return the modeled ValidationException (400, message 'ARN validation error') for one that does not; only an ARN that IS a system ARN may fall through to the ResourceNotFoundException path.
Traces: resiliencehubv2-user-journey-lifecycle#task#2. Component: services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/create_user_journey.rs lines 176-178 (store.systems.get(&system_arn) -> system_not_found) and the refusal helper at lines 269-274, which builds it through crate::state::resource_not_found (src/state/not_found.rs lines 53-60, message template at lines 69-71).

Cause 2 of 4: a deleted user journey is still readable — GetUserJourney answers 200 with the journey body where AWS answers 404, because DeleteUserJourney is an unimplemented scaffold so the record is never removed.
AWS: Smithy GetUserJourney (GET /v2/get-user-journey, @readonly, success 200) takes @required systemArn (Arn) and userJourneyId (UserJourneyId, @length 1..255, pattern ^\S{1,255}$) as httpQuery members and models ResourceNotFoundException (httpError 404), whose shape carries @required message plus resourceId ('The identifier of the resource that was not found') and resourceType ('The type of the resource that was not found'). Recorded behavior after the journey has been deleted: HTTP 404 with an x-amzn-errortype header, body message 'User journey not found: <userJourneyId>' (fixed text 'User journey not found: ' plus the id the request supplied), resourceId = that same userJourneyId and resourceType = 'UserJourney'.
Emulator: the read answers HTTP 200 with the full userJourney body; the replay reports 'status: expected 404, got 200' and $.message, $.resourceId, $.resourceType and $headers.x-amzn-errortype all missing. GetUserJourney's own 404 path is already correct; the journey is still in its system's record because DeleteUserJourney answers 501 NotImplemented and removes nothing.
Why: the post-delete contract is unobservable. A client's teardown or reconciliation loop reads a journey it just deleted as live, journeys accumulate for the life of the emulator, and 404-handling code (the normal 'already gone' branch) never runs.
Fix: implement DeleteUserJourney in its handler file over the system record's user_journeys map — resolve systemArn (404 for a system that is not stored) and the userJourneyId (404 'User journey not found: <id>'), remove the record together with the clientToken index entries that named it, and delete the SCAFFOLDED marker — so GetUserJourney's existing ResourceNotFoundException path becomes reachable.
Traces: resiliencehubv2-user-journey-lifecycle#task#8. Component: services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/delete_user_journey.rs line 162 (Scaffolded::not_implemented("DeleteUserJourney")); the correct read-side refusal is already built at src/ops/get_user_journey.rs lines 168-173 via src/state/user_journeys.rs lines 125-131.

Cause 3 of 4: CreateServiceFunctionResources refuses a resource the service never discovered with the WRONG ValidationException message.
AWS: Smithy CreateServiceFunctionResources (POST /v2/create-service-function-resources, success 200) takes @required serviceArn (Arn), serviceFunctionId (EntityId, @length 1..255, pattern ^\S{1,255}$) and resources (ResourceList, @length 1..100), and models ValidationException 400 beside ConflictException 409 and ResourceNotFoundException 404. Status 400 and the error code ValidationException are modeled; the refusal TEXT is recorded behavior only: AWS answers message 'Resource not found: <resource>. Resource must be discovered before it can be added to a function.' — the FIXED text is 'Resource not found: ' and '. Resource must be discovered before it can be added to a function.', and the single VARIABLE part is the unresolvable identifier taken verbatim from the request's resources entry (the replayed S3 ARN arn:aws:s3:::sim2real-probe-rhv2-undiscovered-6ff502b5 is a per-run value filling that slot, not part of the template).
Emulator: status 400 and code ValidationException are right, but the body's message is the emulator's own sentence 'Resource not discovered for service: <resource>' — entirely different fixed text, dropping AWS's 'Resource not found' lead-in and the sentence that tells the caller discovery must run first. The identifier is interpolated correctly.
Why: a client, IaC provider or test suite that keys on AWS's refusal text to tell 'this resource has not been discovered yet — run/await discovery and retry' apart from any other ValidationException does not recognise the refusal and reports an unknown validation failure; retry-on-undiscovered logic never fires.
Fix: the CreateServiceFunctionResources handler's undiscovered-resource refusal branch — word the message exactly as AWS does, keeping the request's identifier as the only interpolated value; status, code and reason are already correct.
Traces: resiliencehubv2-service-function-lifecycle#task#6. Component: services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/create_service_function_resources.rs (the undiscovered_resource helper, message built around line 276, reached from the first_undiscovered branch around lines 194-196).

Cause 4 of 4: CreateSystem mints a system.systemId whose SHAPE the recorded response does not have, and the previous version-nibble fix did not close it.
AWS: Smithy System$systemId targets SystemId (@length 1..255, pattern ^\S{1,255}$, doc 'System ID for cross-account use without exposing account structure'), so the model does not pin a format — recorded behavior only. The fixture asserts the SHAPE of $.system.systemId and not a literal value (the replay prints the expectation as '<shaped-not-asserted>'), so no per-run id is being required here; what is required is that the minted id belong to the same value class as the recorded systemId.
Emulator: the id the emulator mints fails that shaped assertion on CreateSystem in two fixtures — replay detail '$.system.systemId: value_mismatch (expected <shaped-not-asserted>, got a50da811-de31-4c62-8e17-110ee641023c)'. Note for the fixer: the resolved issue 20261116T044615Z read the recorded shape as a lowercase RFC-9562 version-4 UUID and the generator was changed accordingly (src/state.rs UUID_VERSION = 0x40, mint_uuid sets bytes[6] = (bytes[6] & 0x0f) | 0x40 and bytes[8] = (bytes[8] & 0x3f) | 0x80). The value replayed here IS a well-formed lowercase v4 UUID (third group '4c62' starts with 4, fourth group '8e17' with 8) and the assertion STILL fails, so the recorded systemId's shape differs from an 8-4-4-4-12 hyphenated UUID in some other respect — the version nibble is not the requirement. Hypothesis: the recorded id is a differently punctuated/sized opaque identifier (the member exists precisely so a system can be named cross-account without its ARN), so the fix must be read off the recorded value class in the fixture rather than guessed again.
Why: a client, IaC template or tool that validates or parses a system identifier against the form AWS hands out rejects or mis-handles the ids the emulator mints, even though their length and charset look plausible, so ids do not round-trip through id-aware consumers the way AWS's do; the same minting path feeds the resource segment of the system ARN.
Fix: the shared minted-id path in the service's state module (src/state.rs mint_uuid, used by src/state/systems.rs mint_system_arn, which returns the id and the ARN that carries it) — mint systemId in the value class the recorded CreateSystem response uses, keeping the id and the ARN's resource segment identical as they are today.
Traces: resiliencehubv2-system-lifecycle#task#1 (cluster also covers resiliencehubv2-system-validation-and-conflict-errors#task#2). Component: services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/state.rs lines 657-694 (UUID_VERSION, mint_uuid) and src/state/systems.rs lines 258-266 (mint_system_arn).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true (/__admin/health 200, data listener accepting), 13 fixtures replayed, 124 steps — 90 passed, 27 mismatched, 7 blocked, score 72.6%, bootstrap_signal=false, 5 divergence clusters. The fifth cluster (TagResource Content-Length, resiliencehubv2-policy-tagging#task#3) is OWNED by 20261113T163326Z and is not part of this issue. The four clusters below are marked neither ALREADY FILED nor OWNED.

Cause 1 — fixture resiliencehubv2-user-journey-lifecycle, step task#2, operation CreateUserJourney, verdict mismatch:
  Expected (AWS recording): HTTP 400, x-amzn-errortype for the 400 class, $.message = 'ARN validation error'.
  Actual (emulator): HTTP 404; replay detail: "status: expected 400, got 404 / $.message: value_mismatch (expected 'ARN validation error', got 'System not found: service/fake:ffffff') / $headers.x-amzn-errortype: shape_mismatch (expected '<shaped-not-asserted>', got 'ResourceNotFoundException')".
  divergence_key: service=resiliencehubv2|CreateUserJourney|status=400->404|keys=$.message,$headers.x-amzn-errortype,status
  Source: src/ops/create_user_journey.rs:176-178 resolves the ARN in the store with no ARN-shape gate; the 404 body comes from src/state/not_found.rs:53-71.

Cause 2 — fixture resiliencehubv2-user-journey-lifecycle, step task#8, operation GetUserJourney, verdict mismatch:
  Expected (AWS recording): HTTP 404, x-amzn-errortype present, $.message = 'User journey not found: <userJourneyId>', $.resourceId = <userJourneyId>, $.resourceType = 'UserJourney'.
  Actual (emulator): HTTP 200 with the journey body; replay detail: "status: expected 404, got 200 / $.message: missing (expected 'User journey not found: ba228e44-ba43-4ca8-a970-02c8f0ba12e8', got None) / $.resourceId: missing / $.resourceType: missing / $headers.x-amzn-errortype: missing". The id ba228e44-... is the per-run journey id filling the template.
  divergence_key: service=resiliencehubv2|GetUserJourney|status=404->200|keys=$.message,$.resourceId,$.resourceType,$headers.x-amzn-errortype,status
  Source: src/ops/delete_user_journey.rs:162 returns Scaffolded::not_implemented("DeleteUserJourney") (501), so the preceding delete removes nothing; src/ops/get_user_journey.rs:168-173 would have produced the exact 404 body had the record been gone.

Cause 3 — fixture resiliencehubv2-service-function-lifecycle, step task#6, operation CreateServiceFunctionResources, verdict mismatch:
  Expected (AWS recording): HTTP 400, $.message = 'Resource not found: <resource>. Resource must be discovered before it can be added to a function.'
  Actual (emulator): HTTP 400 with $.message = 'Resource not discovered for service: <resource>'; replay detail: "$.message: value_mismatch (expected 'Resource not found: arn:aws:s3:::sim2real-probe-rhv2-undiscovered-6ff502b5. Resource must be discovered before it can be added to a function.', got 'Resource not discovered for service: arn:aws:s3:::sim2real-probe-rhv2-undiscovered-6ff502b5')".
  divergence_key: service=resiliencehubv2|CreateServiceFunctionResources|status=400->400|keys=$.message

Cause 4 — fixture resiliencehubv2-system-lifecycle, step task#1 (cluster also covers resiliencehubv2-system-validation-and-conflict-errors#task#2), operation CreateSystem, verdict mismatch, expected_status 200 / actual_status 200:
  Expected (AWS recording): $.system.systemId in the recorded value class (shape asserted, value not).
  Actual (emulator): replay detail: "$.system.systemId: value_mismatch (expected '<shaped-not-asserted>', got 'a50da811-de31-4c62-8e17-110ee641023c')" — a well-formed lowercase v4 UUID, so the earlier version-nibble change (resolved issue 20261116T044615Z, cause 4) did not satisfy the assertion.
  divergence_key: service=resiliencehubv2|CreateSystem|status=200->200|keys=$.system.systemId
  Source: src/state.rs:657-694 (UUID_VERSION = 0x40; mint_uuid) feeding src/state/systems.rs:258-266 (mint_system_arn).

Model evidence (services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/smithy-model/com-amazonaws-resiliencehubv2-ngrhservicecore.json, service NGRHServiceCore, sdkId resiliencehubv2, protocol restJson1): CreateUserJourney POST /v2/create-user-journey (errors ValidationException 400, ResourceNotFoundException 404, ConflictException 409, AccessDeniedException 403, ServiceQuotaExceededException 402, InternalServerException 500); GetUserJourney GET /v2/get-user-journey (ResourceNotFoundException 404 with message/resourceId/resourceType); CreateServiceFunctionResources POST /v2/create-service-function-resources (ValidationException 400); CreateSystem POST /v2/create-system returning @required system (System$systemId -> SystemId, @length 1..255, pattern ^\S{1,255}$).

## Steps to Reproduce
Prerequisites: clone the repo, build and boot the emulator from this checkout, point an AWS SDK client for resiliencehubv2 at it.

Cause 1 (resiliencehubv2-user-journey-lifecycle#task#2):
1. CreateSystem(name=<unique>) -> note systemArn.
2. Call CreateUserJourney(systemArn='arn:aws:resiliencehub:<region>:<12-digit account>:service/fake:ffffff', name=<valid EntityLabel>) — an ARN that satisfies the modeled Arn pattern but whose resource segment is not this service's system/<id>.
   Expected: HTTP 400, x-amzn-errortype for the 400 class, message 'ARN validation error'. Observed: HTTP 404, x-amzn-errortype: ResourceNotFoundException, message 'System not found: service/fake:ffffff'.

Cause 2 (resiliencehubv2-user-journey-lifecycle#task#8):
1. CreateSystem(name=<unique>) -> systemArn; CreateUserJourney(systemArn, name=<unique>) -> note userJourneyId.
2. DeleteUserJourney(systemArn, userJourneyId) (today: HTTP 501 NotImplemented).
3. GetUserJourney(systemArn, userJourneyId).
   Expected: HTTP 404 with message 'User journey not found: <userJourneyId>', resourceId=<userJourneyId>, resourceType='UserJourney'. Observed: HTTP 200 with the journey body.

Cause 3 (resiliencehubv2-service-function-lifecycle#task#6):
1. Create an IAM role resiliencehub.amazonaws.com may assume, then CreateService(name=<unique>, regions=[<region>], permissionModel={invokerRoleName:<role>}) -> serviceArn.
2. CreateServiceFunction(serviceArn, name=<unique>, criticality='PRIMARY') -> serviceFunctionId.
3. CreateServiceFunctionResources(serviceArn, serviceFunctionId, resources=['arn:aws:s3:::<a bucket the service never discovered>']).
   Expected: HTTP 400 ValidationException with message 'Resource not found: <resource>. Resource must be discovered before it can be added to a function.' Observed: 'Resource not discovered for service: <resource>'.

Cause 4 (resiliencehubv2-system-lifecycle#task#1, also resiliencehubv2-system-validation-and-conflict-errors#task#2):
1. CreateSystem(name=<unique>).
2. Inspect system.systemId (and the resource segment of system.systemArn): today a lowercase v4 UUID such as a50da811-de31-4c62-8e17-110ee641023c, which still fails the fixture's shaped assertion on $.system.systemId. Read the recorded systemId's value class from fixture resiliencehubv2-system-lifecycle step task#1 and mint ids in that class.

Re-replay all four fixture steps named above (resiliencehubv2-user-journey-lifecycle#task#2, resiliencehubv2-user-journey-lifecycle#task#8, resiliencehubv2-service-function-lifecycle#task#6, resiliencehubv2-system-lifecycle#task#1) to confirm the fix.
