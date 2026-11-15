Issue-Source: trace-sweep-divergence
Issue-Trace: vpclattice-target-group-register-deregister-targets
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: vpclattice-target-group-register-deregister-targets
Issue-Shape-Id: com.amazonaws.vpclattice#MercuryControlPlane
Issue-Key: vpc-lattice|RegisterTargets|2xx->4xx|$.successful
Issue-Key: vpc-lattice|RegisterTargets|2xx->4xx|$.unsuccessful
Issue-Key: vpc-lattice|RegisterTargets|2xx->4xx|status
Issue-Key: vpc-lattice|ListTargets|2xx->2xx|$.items[*].reasonCode
Issue-Key: vpc-lattice|ListTargets|2xx->2xx|$.items[*].status
Issue-Key: vpc-lattice|ListTargets|2xx->2xx|$.items[*]
Issue-Commit: 8b39438bd5de658d17c79c68a24e7b40ac4424c8

# vpc-lattice target registration and target state diverge: 3 net-new causes in RegisterTargets/ListTargets

> _Note: this issue was identified against the code base at commit `8b39438bd5de658d17c79c68a24e7b40ac4424c8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterTargets

## Description
Three net-new causes, all in the vpc-lattice target-group target slice (register -> list -> deregister -> list). They share one fault: this emulator models target registration as an all-or-nothing control-plane write over a target-group record, so a per-target failure becomes a whole-call 400, a target's reported health state ignores whether its group's type supports health checks, and a deregistration never removes the record a listing reports. No open issue carries any of these: 20261109T141403Z-lambda-vpc-lattice-vpc-lattice-registertargets-answers-notimplemented.md describes RegisterTargets answering NotImplemented (the operation now answers, so that behavior has moved on and these are different defects), and the other open vpc-lattice issues cover id minting, BatchUpdateRule failure text and dnsEntry.domainName. Each cause must be closed separately; replay all three fixture steps named below before submitting.

Cause 1 of 3: ListTargets reports UNUSED with no reasonCode for a target in a target group whose type runs no health checks, instead of UNAVAILABLE/HealthCheckNotSupported.
AWS: Smithy (services/service-com-amazonaws-vpclattice-mercurycontrolplane/smithy-model/com-amazonaws-vpclattice-mercurycontrolplane.json) models ListTargets as POST /targetgroups/{targetGroupIdentifier}/listtargets -> HTTP 200 with required output member `items` (TargetSummaryList). TargetSummary carries `status` (TargetStatus) and `reasonCode` (String, "The code for why the target status is what it is"). TargetStatus documents UNAVAILABLE as "Health checks are unavailable for the target group" / "Health checks are disabled" and UNUSED as "Target group is not used in a service" / "The target group is not used in a listener rule". For a target group whose type does not support health checks (a LAMBDA group, as in this fixture), the recorded AWS response reports the registered target with status 'UNAVAILABLE' and reasonCode 'HealthCheckNotSupported' — i.e. the health-check-support condition is reported ahead of, and independently of, whether a service/listener rule currently routes to the group, and the machine-readable reason code is always present to say which UNAVAILABLE case it is. The reason-code token 'HealthCheckNotSupported' is fixed text (recorded behavior; the model types `reasonCode` as a free String and states no value list).
Emulator: HTTP 200, but `$.items[0].status` is 'UNUSED' and `$.items[0].reasonCode` is absent (None). crate::state::Store::target_status answers UNUSED first whenever no service uses the group and only then considers group.health_checks_apply(), and the ListTargets handler hard-codes `reason_code: None` with the comment "the model states no code, and this service has no reason to report beyond the status itself".
Why: a client polling ListTargets to decide whether a target is healthy cannot distinguish "health checks do not apply to this target group type" from "nothing routes to this group yet". Code keyed on UNAVAILABLE + reasonCode == 'HealthCheckNotSupported' (the normal way to stop waiting for a LAMBDA target to go HEALTHY) never fires and the caller waits for a health state that will never be reported, and any reasonCode read is a null dereference.
Fix: ListTargets' status derivation — crate::state::Store::target_status (the UNAVAILABLE-vs-UNUSED precedence) and the TargetSummary construction in the ListTargets handler, which must emit a reason code alongside the status.
Traces: vpclattice-target-group-register-deregister-targets#task#6.

Cause 2 of 3: RegisterTargets refuses the whole call with HTTP 400 for a target AWS reports as an individual per-target failure under `unsuccessful` with HTTP 200.
AWS: Smithy models RegisterTargets as POST /targetgroups/{targetGroupIdentifier}/registertargets -> HTTP 200 with output members `successful` (TargetList) and `unsuccessful` (TargetFailureList); TargetFailure carries `id`, `port`, `failureCode` ("The failure code") and `failureMessage` ("The failure message"). Its modeled errors are AccessDeniedException 403, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ServiceQuotaExceededException 402, ThrottlingException 429 and ValidationException 400. A target the service accepts the SHAPE of but cannot register is reported per target, not as a request-level error: the recording shows registering a Lambda function ARN belonging to another account answering HTTP 200 with `successful` the empty list and `unsuccessful` one entry whose `id` is the submitted target id, `failureCode` 'InvalidTarget' and `failureMessage` 'Lambda <lambda-function-arn> is not owned by account <account-id>.' — fixed text 'Lambda ' and ' is not owned by account ' plus the trailing period, with the function ARN taken from the request's target `id` and the account id being the caller's own account from the request credentials (recorded behavior for the message text; the model states no text).
Emulator: HTTP 400 for the same call, with `$.successful` and `$.unsuccessful` both absent from the body. The handler decides every refusal for the whole request before anything is stored — crate::state::check_target_id over the target-id form, and the LAMBDA one-target-per-group rule / target ceiling, each returned as a request-level ValidationException — and its own comment states the intent: "Every target this service refuses, it refuses for the whole call ... So no individual target fails, and the member reports the empty list", so `unsuccessful` is only ever the empty list and is never populated.
Why: a client that registers a batch of targets and inspects `unsuccessful` to learn which ones failed instead gets an exception, so a partially valid batch registers nothing and the per-target failure code and message a caller branches on ('InvalidTarget', ownership text) are unavailable. SDK retry/error handling keyed on ValidationException also treats a non-retryable per-target rejection as a malformed request.
Fix: the RegisterTargets handler (services/service-com-amazonaws-vpclattice-mercurycontrolplane/src/ops/register_targets.rs) — per-target admission must be evaluated per target and reported in the response's `unsuccessful` list with a failure code and message, with the accepted targets still returned under `successful`, reserving the 400 for input the whole request is malformed in.
Traces: vpclattice-target-group-register-deregister-targets#task#7.

Cause 3 of 3: a deregistered target is still reported by ListTargets, so the target group's membership never reflects DeregisterTargets.
AWS: Smithy models DeregisterTargets as POST /targetgroups/{targetGroupIdentifier}/deregistertargets (@idempotent) -> HTTP 200 with `successful`/`unsuccessful`, and TargetStatus documents DRAINING as "The target is being deregistered" with a default draining time of one minute. For a LAMBDA target group (no connections to drain), the recording shows ListTargets after a successful deregistration of the group's only target answering HTTP 200 with `items` EMPTY — the target is gone from the group, not merely restatused.
Emulator: HTTP 200 but `$.items[0]` is still present, carrying the deregistered target ({'id': '<lambda-function-arn>', 'status': 'UNUSED'}) — an extra item where AWS reports none. The registration is never removed from the target-group record because DeregisterTargets is still the scaffold stub (services/service-com-amazonaws-vpclattice-mercurycontrolplane/src/ops/deregister_targets.rs, whose handle body returns crate::scaffolded::Scaffolded::not_implemented("DeregisterTargets")), so nothing ever mutates the stored target list that ListTargets reads.
Why: target-group membership is write-once from a client's point of view — a target can be added but never removed, so re-registering a different Lambda in a one-target LAMBDA group is permanently blocked by the ceiling, cleanup/teardown loops that deregister and then poll ListTargets until it is empty never terminate, and a listing reports a target the caller has already removed.
Fix: implement the DeregisterTargets handler against target-group state so an accepted target is removed from the stored target list (and reported under `successful`), and so ListTargets' listing of that group no longer reports it.
Traces: vpclattice-target-group-register-deregister-targets#task#10.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (score 76.4%, 9 divergences, 6 of them already owned by open issues).

Cause 1 — fixture vpclattice-target-group-register-deregister-targets, step task#6, operation ListTargets, verdict mismatch, status 200 -> 200:
  $.items[0].reasonCode: missing (expected 'HealthCheckNotSupported', got None)
  $.items[0].status: value_mismatch (expected 'UNAVAILABLE', got 'UNUSED')
  Modeled contract: TargetSummary.status (TargetStatus: DRAINING|UNAVAILABLE|HEALTHY|UNHEALTHY|INITIAL|UNUSED) and TargetSummary.reasonCode.

Cause 2 — fixture vpclattice-target-group-register-deregister-targets, step task#7, operation RegisterTargets, verdict mismatch, status expected 200, got 400:
  $.successful: missing (expected [], got None)
  $.unsuccessful: missing (expected [{'failureCode': 'InvalidTarget', 'failureMessage': 'Lambda arn:aws:lambda:us-west-2:111111111111:function:sim2real-probe-lat-nofn-f93a1c07 is not owned by account 000000000000.', 'id': 'arn:aws:lambda:us-west-2:111111111111:function:sim2real-probe-lat-nofn-f93a1c07'}], got None)
  In that recorded example the ARN and the account id are values from the request and the caller's account; the requirement is the 200 status, the empty `successful` list, and an `unsuccessful` entry whose failureCode is 'InvalidTarget' and whose failureMessage is 'Lambda <submitted-target-arn> is not owned by account <caller-account-id>.'
  Modeled contract contradicted: RegisterTargets returns HTTP 200 with successful/unsuccessful; 400 ValidationException is for input the service refuses outright.

Cause 3 — fixture vpclattice-target-group-register-deregister-targets, step task#10, operation ListTargets, verdict mismatch, status 200 -> 200:
  $.items[0]: extra (expected None, got {'id': 'arn:aws:lambda:us-west-2:000000000000:function:sim2real-probe-lat-tgrd-fn-7511851f', 'status': 'UNUSED'})
  Expected: `items` empty after the target was deregistered.

## Steps to Reproduce
1. Point a boto3 vpc-lattice client at the emulator (region us-west-2).
2. tg = vl.create_target_group(name='tg1', type='LAMBDA', config={'lambdaEventStructureVersion': 'V1'}).
3. vl.register_targets(targetGroupIdentifier=tg['id'], targets=[{'id': '<lambda-arn-in-your-account>'}]) -> 200, ARN under `successful`.
4. vl.list_targets(targetGroupIdentifier=tg['id']) — Cause 1: items[0] comes back status 'UNUSED' with no reasonCode, instead of status 'UNAVAILABLE' with reasonCode 'HealthCheckNotSupported' for a target group type that runs no health checks.
5. vl.register_targets(targetGroupIdentifier=tg['id'], targets=[{'id': 'arn:aws:lambda:us-west-2:111111111111:function:not-mine'}]) — Cause 2: the call fails with HTTP 400 and no body members, instead of HTTP 200 with successful=[] and unsuccessful=[{'id': <that arn>, 'failureCode': 'InvalidTarget', 'failureMessage': 'Lambda <that arn> is not owned by account <caller account>.'}].
6. vl.deregister_targets(targetGroupIdentifier=tg['id'], targets=[{'id': '<lambda-arn-in-your-account>'}]), then vl.list_targets(targetGroupIdentifier=tg['id']) — Cause 3: items still carries the deregistered target instead of being empty (DeregisterTargets is an unimplemented scaffold stub, so nothing is removed).
7. Re-replay fixture vpclattice-target-group-register-deregister-targets steps task#6, task#7 and task#10 to confirm all three causes.
