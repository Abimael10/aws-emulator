Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.get-origin-access-control-config-nonexistent-rejected
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront.get-origin-access-control-config-nonexistent-rejected|run#9|AssertionError|assert message == "The specified origin access control does not exist.", (
Issue-Commit: c208dffe0dd114dceb7c782194c5df9ad164c45a

# cloudfront: GetOriginAccessControlConfig reports a well-formed absent id as a malformed id — 404 NoSuchOriginAccessControl carries 'Id format is invalid' instead of AWS's not-found sentence

> _Note: this issue was identified against the code base at commit `c208dffe0dd114dceb7c782194c5df9ad164c45a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetOriginAccessControlConfig

## Description
One net-new root cause (Cause 1 of 1). Failing check: cloudfront.get-origin-access-control-config-nonexistent-rejected.

Cause 1 of 1: the GET /2020-05-31/origin-access-control/{Id}/config route runs an identifier-format predicate on the path label and, for an id that simply names no origin access control, emits a malformed-identifier message instead of the fixed not-found message AWS words for this error class. The HTTP status and the error code already agree with AWS; only Error.Message diverges.

AWS:      404 with error code NoSuchOriginAccessControl and Message 'The specified origin access control does not exist.' Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31): GetOriginAccessControlConfig input {Id: string, @httpLabel, required} with NO @pattern and NO @length constraint; http {method GET, uri /2020-05-31/origin-access-control/{Id}/config, code 200}; errors AccessDenied (403) and NoSuchOriginAccessControl (404), the latter documented 'The origin access control does not exist.' Because the model places no format constraint on Id, AWS carries any supplied id through to the state lookup and reports the resource ABSENT; it has no modelled basis for a malformed-identifier refusal on this route. The message text for this class is wholly FIXED — it contains no id, name, number, limit or timestamp taken from the request or from state — so reproducing it verbatim is the entire requirement (wording per recorded behavior; the model supplies the error shape, not the prose).
Emulator: answers the same 404 / NoSuchOriginAccessControl but with Message 'Id format is invalid' — fixed text belonging to a malformed-argument class AWS does not use here, produced by a pre-lookup id-shape check on the config route.
Why:      'Id format is invalid' tells a caller its own id handling is broken (a non-retryable programming error), whereas 'The specified origin access control does not exist.' tells it the OAC is gone and the correct response is to create or re-resolve it. Create-if-absent logic, cleanup paths that swallow not-found by message, and tests that branch on the wording all take the wrong branch. Because the emulator's format predicate is narrower than AWS's (which has none per the model), ids AWS would resolve may never be looked up against state at all.
Fix:      GetOriginAccessControlConfig's request validation / handler for GET /2020-05-31/origin-access-control/{Id}/config — drop the pre-lookup malformed-id refusal for a form the model does not constrain, and emit NoSuchOriginAccessControl 404 carrying its fixed not-found text when the id names no origin access control.
Related, NOT covering: open issue 20261116T020304Z-cloudfront-2-net-new-causes-createinvalidation-and-getoriginaccesscontrol.md records this same inversion on CreateInvalidation and on GetOriginAccessControl (the /{Id} read route). It does not carry this check's replay key and does not touch the /{Id}/config route, so fixing it would leave this route still answering 'Id format is invalid'. Same family, different handler: open issues 20261115T235211Z-cloudfront-getdistributionconfig-and-getoriginaccesscontrol-report-absent-ids-as.md and 20261115T095648Z-cloudfront-accept-refuse-decisions-diverge-anycast-quota-if.md.
Traces:   cloudfront.get-origin-access-control-config-nonexistent-rejected.

## Evidence
Check cloudfront.get-origin-access-control-config-nonexistent-rejected, verdict mismatch, from the replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (531/702 passed, 75.6%; bootstrap_signal false).

  cf.get_origin_access_control_config(Id=<a unique id that was never created>)
  assertion: assert message == "The specified origin access control does not exist."
  AssertionError: expected AWS's exact not-found message, got 'Id format is invalid'

The preceding assertion `assert code == "NoSuchOriginAccessControl"` PASSED, so the 404 status and the error code already match AWS; ONLY Error.Message diverges.

Message anatomy: the expected sentence 'The specified origin access control does not exist.' is entirely fixed text for the NoSuchOriginAccessControl class on this route — no part is templated from the request or from emulator state. The emulator's 'Id format is invalid' is likewise fixed text, but from a malformed-argument class AWS does not select here.

The check's positive control in the same file (create an OAC, then GetOriginAccessControlConfig on its real id and read back Name, SigningProtocol='sigv4' and OriginAccessControlOriginType='s3') is what AWS does on the happy path; it was not reached because the negative assertion failed first.

Smithy evidence (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json): GetOriginAccessControlConfig — Id @httpLabel required string, no pattern/length; errors AccessDenied httpError 403, NoSuchOriginAccessControl httpError 404 (doc: 'The origin access control does not exist.'). No modelled constraint makes a malformed-identifier refusal available on this route.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from that checkout; replay the behavioral suite simparity-behavioral-aws-v1.
2. CreateOriginAccessControl with OriginAccessControlConfig {Name: <unique>, Description: <any>, SigningProtocol: 'sigv4', SigningBehavior: 'always', OriginAccessControlOriginType: 's3'} so the positive control exists.
3. Call GetOriginAccessControlConfig with an Id that names no origin access control (any well-formed string the account never created).
4. Observe HTTP 404 with Code 'NoSuchOriginAccessControl' and Error.Message 'Id format is invalid'. Expected: HTTP 404, Code 'NoSuchOriginAccessControl', Error.Message 'The specified origin access control does not exist.' (compare ONLY Error.Message — status and code already match).
5. Confirm the positive control still passes: GetOriginAccessControlConfig on the created id returns its OriginAccessControlConfig with the Name, SigningProtocol and OriginAccessControlOriginType supplied at create.
6. Re-replay cloudfront.get-origin-access-control-config-nonexistent-rejected to confirm the fix.
