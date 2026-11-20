Issue-Source: trace-sweep-divergence
Issue-Trace: application-signals-slo-error-paths
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: application-signals-slo-error-paths
Issue-Shape-Id: com.amazonaws.applicationsignals#ApplicationSignals
Issue-Key: application-signals|CreateServiceLevelObjective|4xx->4xx|$.Message
Issue-Commit: 075f5affd9be323818b79ff1d792bbcf981ada1a

# CreateServiceLevelObjective duplicate-SLO ConflictException Message does not name the existing SLO ARN

> _Note: this issue was identified against the code base at commit `075f5affd9be323818b79ff1d792bbcf981ada1a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateServiceLevelObjective

## Description
Replay of the application-signals wire corpus (simparity-wire-aws-v1) leaves ONE net-new divergence in this service after the clusters owned by 20261113T135822Z-application-signals-responses-diverge-fabricated-dropped-and-mis.md are excluded: the duplicate-SLO ConflictException's Message text. Status and error classification are already correct; only the message body diverges.

Cause 1 of 1: CreateServiceLevelObjective's already-exists refusal answers a generic identifier-free sentence instead of the sentence AWS words for a duplicate SLO.

AWS: Smithy (services/service-com-amazonaws-applicationsignals-applicationsignals/smithy-model/com-amazonaws-applicationsignals-applicationsignals.json): CreateServiceLevelObjective -> POST /slo, success code 200, required input member Name (target ServiceLevelObjectiveName, @pattern ^[0-9A-Za-z][-._0-9A-Za-z ]{0,126}[0-9A-Za-z]$); modeled errors AccessDeniedException(403), ConflictException(errorKind client, httpError 409), ServiceQuotaExceededException(402), ThrottlingException(429), ValidationException(400). ConflictException carries exactly one required member, Message (target String), doc "This operation attempted to create a resource that already exists." The model does not state the message text, so: recorded behavior only for the wording. The recording shows that a CreateServiceLevelObjective for an SLO that already exists is refused HTTP 409 ConflictException with Message of the form:

    Slo with arn '<ARN of the existing SLO>' already exists.

Its FIXED text is "Slo with arn '" + "' already exists." — including the closing single quote and the trailing period. Its single VARIABLE part is the ARN of the already-existing SLO, taken from the request's Name and the emulator's own account/region/state and shaped by ServiceLevelObjective$Arn's @pattern ^arn:(aws|aws-us-gov):application-signals:[^:]*:[^:]*:slo/<Name>$. The region, 12-digit account id and the sim2real-probe-... SLO name that appear inside the ARN in this one recording are per-run values that FILL that variable part; they are not part of the requirement.

Emulator: answers the correct HTTP 409 with the correct error code ConflictException, but Message is the crate's generic cross-service sentence "A resource with the same identifier already exists" — no mention of the resource kind (Slo), no ARN, and no trailing period. The whole body of the message is wrong text, not a value substitution.

Why: a client that must tell "this SLO name is already taken" apart from other 409 conflicts — an idempotent provisioner, a create-or-adopt wrapper, a CDK/Terraform-style converger, or any log scraper — matches on this Message and, in the AWS case, recovers the existing SLO's ARN straight out of it to adopt the resource instead of recreating it. Against the emulator the match never fires and the ARN is absent, so the duplicate-name outcome is misclassified as a generic conflict and the adopt path is never taken; conversely code written against the emulator's generic sentence breaks on a real account.

Fix: the duplicate/already-exists branch of the CreateServiceLevelObjective handler (the SLO store's uniqueness check) and the error message it formats. That branch must word this resource's own already-exists sentence, interpolating the existing SLO's ARN from state, rather than delegating to the shared generic "A resource with the same identifier already exists" helper.

Contradicting open issues, named deliberately: 20261117T223334Z-application-signals-reopen-application-signals-createservicelevelobjective-duplicate-name-conflictexception-still.md and 20261102T091730Z-application-signals-createservicelevelobjective-duplicate-name-conflictexception-under-concurrency.md both argue the OPPOSITE direction (they demand the generic identifier-free sentence and treat the ARN-echoing one as the defect), and both are grounded in behavioral checks from simparity-behavioral-aws-v1, not in a wire recording. Neither carries this fixture step's replay key (the replay reports this cluster as unowned), so neither covers it. The resolved issue 20261105T081430Z-application-signals-createservicelevelobjective-conflictexception-message-is-generic-instead-of-naming.md asserted the same requirement as THIS issue and is marked fixed, yet the recorded step still diverges, so that fix did not land (or was reverted by the behavioral issues above). The wire recording of AWS is the evidence here: the Message names the existing SLO's ARN. Whoever fixes this must reconcile the two corpora rather than flipping the wording back and forth.

Traces: application-signals-slo-error-paths#task#5.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, 10 fixtures replayed, 62 steps, 55 passed, 7 mismatched, score 88.7%.

Fixture application-signals-slo-error-paths, step task#5, operation CreateServiceLevelObjective:
  expected_status: 409   actual_status: 409  (correct)
  error code: ConflictException (correct)
  $.Message value_mismatch:
    expected: "Slo with arn 'arn:aws:application-signals:us-west-2:000000000000:slo/sim2real-probe-application-signals-slo-errors-a7ce4fa4' already exists."
    actual:   "A resource with the same identifier already exists"
  divergence_key: service=application-signals|CreateServiceLevelObjective|status=409->409|keys=$.Message
  affected: application-signals-slo-error-paths#task#5 (affected_count 1)
  stub_path: (empty)   owned_by: (empty — not covered by any open issue's replay key)

Message decomposition for the requirement:
  fixed text  -> "Slo with arn '" ... "' already exists."
  variable part -> the existing SLO's ARN, derived from the request's Name plus the emulator's account id and region (ServiceLevelObjective$Arn @pattern ^arn:(aws|aws-us-gov):application-signals:[^:]*:[^:]*:slo/[0-9A-Za-z][-._0-9A-Za-z ]{0,126}[0-9A-Za-z]$)
  per-run values in the recording, NOT requirements -> us-west-2, 000000000000, sim2real-probe-application-signals-slo-errors-a7ce4fa4

Model grounding: ConflictException {Message: String, required}, errorKind client, httpError 409, doc "This operation attempted to create a resource that already exists."; CreateServiceLevelObjective POST /slo code 200 with ConflictException among its modeled errors.

## Steps to Reproduce
Boot the emulator from this checkout and replay the application-signals wire fixtures (replay_wire_corpus on trace set simparity-wire-aws-v1), or reproduce by hand:

1. POST /slo (CreateServiceLevelObjective) with a fresh Name N matching ^[0-9A-Za-z][-._0-9A-Za-z ]{0,126}[0-9A-Za-z]$ and a valid SliConfig/RequestBasedSliConfig plus Goal. It returns 200; keep Slo.Arn.
2. POST /slo again with the SAME Name N and any valid SLI config.
3. Observe HTTP 409 with Error.Code == "ConflictException" (correct) and Error.Message == "A resource with the same identifier already exists".
4. Expected per the recording: Message == "Slo with arn '<the ARN returned in step 1>' already exists." — fixed text "Slo with arn '" ... "' already exists." with the existing SLO's ARN interpolated.
5. GetServiceLevelObjective(Id=<Arn from step 1>) still returns the original SLO unchanged, confirming only the message wording diverges.

Cleanup: DeleteServiceLevelObjective(Id=<Arn>).
