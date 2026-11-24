Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appstream.concurrent-user-creation-duplicate-name
Issue-Shape-Id: com.amazonaws.appstream#PhotonAdminProxyService
Issue-Key: appstream.concurrent-user-creation-duplicate-name|run#9|AssertionError|assert len(successes) == racers, f"expected all {racers} racers to succeed, got: {results}"
Issue-Commit: fc41187e4d936c85570d866c4bbccc081bf40c6e

# REOPENS appstream CreateUser idempotency: concurrent identical creates are still refused ResourceAlreadyExistsException (1 cause)

> _Note: this issue was identified against the code base at commit `fc41187e4d936c85570d866c4bbccc081bf40c6e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateUser

## Description
REOPENS 20261102T120557Z-appstream-createuser-is-not-idempotent-a-repeat-create.md: its fix left the call refused.

One root cause in the appstream CreateUser handler. That issue was closed as fixed, but the very check it was grounded in (appstream.concurrent-user-creation-duplicate-name) still fails with the identical refusal, so the idempotent path is not reached.

Cause 1 of 1: CreateUser refuses a concurrent/repeat create of an identity that already exists with exactly the same UserName and AuthenticationType, instead of answering the empty success result. Failing check: appstream.concurrent-user-creation-duplicate-name.

AWS: recorded behavior only for the re-create outcome. Smithy (services/service-com-amazonaws-appstream-photonadminproxyservice/smithy-model/com-amazonaws-appstream-photonadminproxyservice.json) models CreateUser with input {UserName (required, @length 1..128, pattern ^[\p{L}\p{M}\p{S}\p{N}\p{P}]+$, sensitive), AuthenticationType (required enum), MessageAction (enum), FirstName, LastName}, an EMPTY output (CreateUserResult has no members), and errors {InvalidAccountStatusException, InvalidParameterCombinationException, LimitExceededException, OperationNotPermittedException, ResourceAlreadyExistsException}, each httpError 400. There is no @idempotent trait and no prose on re-create, so the rule comes from the recording: N concurrent CreateUser calls on one (UserName, AuthenticationType) pair ALL return HTTP 200 with an empty body, and exactly one user exists for that UserName afterwards — DescribeUsers(AuthenticationType) returns one entry for it with Enabled=true. ResourceAlreadyExistsException is reserved for a conflicting create, not for an identical one arriving concurrently.

Emulator: of six concurrent identical create_user(UserName=<same>, AuthenticationType='USERPOOL', MessageAction='SUPPRESS') calls, exactly one succeeds and the other five are refused with error code 'ResourceAlreadyExistsException' (HTTP 400). Replay detail: "expected all 6 racers to succeed, got: [('ok', None), ('error', 'ResourceAlreadyExistsException'), ('error', 'ResourceAlreadyExistsException'), ('error', 'ResourceAlreadyExistsException'), ('error', 'ResourceAlreadyExistsException'), ('error', 'ResourceAlreadyExistsException')]". The handler still treats any existing record for the name as a conflict instead of comparing the requested identity against the stored one, and the existence-check/insert is not atomic-and-idempotent across concurrent requests.

Note the tension with the open issue 20261113T104042Z-appstream-createuser-accepts-a-duplicate-username-authenticationtype-with.md, which requires the SEQUENTIAL duplicate create to be refused HTTP 400 ResourceAlreadyExistsException with the fixed message 'A user with that username already exists.' Whatever reconciliation is chosen must leave that sequential refusal in place while making the concurrent identical-create burst all succeed as recorded here; the previous fix evidently satisfied the sequential side only and left this one refused.

Why: clients that provision AppStream users declaratively (a reconcile loop, a retried onboarding job, or several workers each ensuring the same user) are written against the idempotent contract and do not catch ResourceAlreadyExistsException for their own identical request. On the emulator every such concurrent duplicate fails the caller even though the desired state is already in place, so provisioning that succeeds on AWS aborts here; a client that does swallow the error can no longer distinguish 'already as requested' from a genuine identity conflict.

Fix: the CreateUser handler and its user-pool state in services/service-com-amazonaws-appstream-photonadminproxyservice — make the existence lookup and insert one atomic step, and when the stored (UserName, AuthenticationType) record matches the request, return the empty success result rather than raising ResourceAlreadyExistsException.

Traces: appstream.concurrent-user-creation-duplicate-name.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the booted emulator; overall 35/43 (81.4%), per-service appstream 20/24 (83%), 4 failed (the other three appstream failures are ALREADY FILED / OWNED by existing issues).

Check appstream.concurrent-user-creation-duplicate-name [mismatch]:
  6 concurrent appstream.create_user(UserName='<unique>@example.com', AuthenticationType='USERPOOL', MessageAction='SUPPRESS') from a 6-worker pool
  expected: all 6 calls return HTTP 200 with an empty CreateUserResult, then DescribeUsers(AuthenticationType='USERPOOL') reports exactly one entry for that UserName with Enabled=true
  got:      1 success and 5 refusals —
            AssertionError: expected all 6 racers to succeed, got: [('ok', None), ('error', 'ResourceAlreadyExistsException'), ('error', 'ResourceAlreadyExistsException'), ('error', 'ResourceAlreadyExistsException'), ('error', 'ResourceAlreadyExistsException'), ('error', 'ResourceAlreadyExistsException')]

The diverging response is HTTP 400 with Error.Code='ResourceAlreadyExistsException' where AWS answers HTTP 200 with an empty body. The UserName in the recording is a per-run unique value and is not part of the requirement; the requirement is that any concurrent repeat of an identical (UserName, AuthenticationType) succeeds and the identity is allocated exactly once.

This is the same check id and the same assertion as the issue marked fixed (20261102T120557Z-appstream-createuser-is-not-idempotent-a-repeat-create.md, Issue-Trace-Ids: appstream.concurrent-user-creation-duplicate-name), whose Issue-Key is appstream.concurrent-user-creation-duplicate-name|run#9|AssertionError|assert len(successes) == racers — so the fix did not take effect on this path.

Smithy evidence: CreateUser input UserName (required) + AuthenticationType (required) + MessageAction; output CreateUserResult = {} (no members); modeled errors include ResourceAlreadyExistsException (errorKind client, httpError 400). No @idempotent trait and no documented re-create outcome, so the idempotent outcome is grounded in the recording.

## Steps to Reproduce
1. git clone the project repo and boot the emulator built from it.
2. Run the behavioral suite (simparity-behavioral-aws-v1).
3. Replay check appstream.concurrent-user-creation-duplicate-name.
4. Equivalently by hand: fire 6 concurrent appstream CreateUser calls with one identical UserName (e.g. 'someone@example.com'), AuthenticationType='USERPOOL', MessageAction='SUPPRESS'. Observe one HTTP 200 and five HTTP 400 ResourceAlreadyExistsException responses instead of six empty HTTP 200s, then DescribeUsers(AuthenticationType='USERPOOL') showing exactly one entry for that UserName with Enabled=true.
