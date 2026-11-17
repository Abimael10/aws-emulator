Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-signals.slo-duplicate-name-conflict-rejected
Issue-Shape-Id: com.amazonaws.applicationsignals#ApplicationSignals
Issue-Key: application-signals.slo-duplicate-name-conflict-rejected|run#11|AssertionError|assert message == "A resource with the same identifier already exists", (
Issue-Commit: de462b36c91b69e24f585ed31866d00badd926bd

# REOPEN: application-signals CreateServiceLevelObjective duplicate-Name ConflictException still answers an ARN-echoing message instead of AWS's generic conflict sentence

> _Note: this issue was identified against the code base at commit `de462b36c91b69e24f585ed31866d00badd926bd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateServiceLevelObjective

## Description
REOPENS 20261029T192739Z-application-signals-words-seven-refusal-messages-differently-from.md: its fix left the call refused. That issue is marked fixed and its Cause 4 named exactly this check (application-signals.slo-duplicate-name-conflict-rejected) and exactly this wording, but the sequential duplicate-Name path still refuses CreateServiceLevelObjective with the emulator's own ARN-echoing sentence. The adjacent OPEN issue 20261102T091730Z-application-signals-createservicelevelobjective-duplicate-name-conflictexception-under-concurrency.md carries only the CONCURRENT racer check's replay key (application-signals.concurrent-create-slo-same-name-exactly-one-wins), so it does not cover this sequential check; both share the one root cause below and must land the identical fixed sentence.

Cause 1 of 1: CreateServiceLevelObjective duplicate-Name ConflictException message, on the sequential (non-racing) path.

AWS: Smithy (services/service-com-amazonaws-applicationsignals-applicationsignals/smithy-model/com-amazonaws-applicationsignals-applicationsignals.json): CreateServiceLevelObjective POST /slo, success code 200; required input member Name targets ServiceLevelObjectiveName with pattern ^[0-9A-Za-z][-._0-9A-Za-z ]{0,126}[0-9A-Za-z]$; modeled errors AccessDeniedException(403), ConflictException(errorKind client, httpError 409), ServiceQuotaExceededException(402), ThrottlingException(429), ValidationException(400). ConflictException carries a single required Message member and is documented "This operation attempted to create a resource that already exists." The model does not state the message text, so: recorded behavior only for the wording — creating a second SLO whose Name already exists in the account is refused 409 ConflictException with Message `A resource with the same identifier already exists`. That entire sentence is FIXED text: NO trailing period and NO variable parts at all — it names neither the SLO's Name, nor its Id, nor its ARN.

Emulator: the status and the error code are correct (409 ConflictException), and the original SLO is left untouched. What diverges is Message: the emulator answers `Slo with arn '<computed-arn-of-the-existing-slo>' already exists.` — the crate's shared already-exists template, whose fixed text is `Slo with arn '` ... `' already exists.` with the existing SLO's computed ARN interpolated as the single variable part, plus a trailing period AWS does not send. (The region, account id and SLO name appearing inside that ARN in the replay output are one run's per-run values; the defect is the template, not those values.)

Why: a client that must distinguish "this name is already taken" from other 409 conflicts — an idempotent provisioner, a CDK/Terraform-style converger, a create-or-adopt wrapper — compares or pattern-matches this Message and logs it verbatim; AWS's "same identifier" phrasing never matches, so the duplicate-name outcome is misclassified. The emulator additionally discloses the ARN of a resource the caller never named, in a field AWS deliberately keeps generic and identifier-free.

Fix: the duplicate-Name branch of the CreateServiceLevelObjective handler / the SLO store's Name-uniqueness check. That branch must word this operation's own conflict sentence instead of delegating to the crate's generic ARN-echoing already-exists message helper; the same helper feeds the concurrent-race entry into the guard, so the sequential and concurrent losers must come out with the identical fixed sentence.

Traces: application-signals.slo-duplicate-name-conflict-rejected (and, for the concurrent entry into the same guard, application-signals.concurrent-create-slo-same-name-exactly-one-wins, which is owned by the open issue named above).

## Evidence
Check application-signals.slo-duplicate-name-conflict-rejected, verdict [mismatch], assertion at check_application_signals_slo_duplicate_name_conflict_rejected.py line 43:

  AssertionError: expected the duplicate-name conflict message, got "Slo with arn 'arn:aws:application-signals:us-west-2:000000000000:slo/sim2real-slo-e3a15e80d3c1' already exists."

Assertions that PASSED immediately before, bounding the defect to the Message alone:
  - the first CreateServiceLevelObjective(Name=N, RequestBasedSliConfig=...) succeeded and returned Slo.Arn
  - line 42: the duplicate attempt's Error.Code == "ConflictException" (code and 409 classification are CORRECT and not part of this defect)

Divergence quoted exactly:
  status/code: 409 ConflictException — correct
  Message expected: `A resource with the same identifier already exists`  (entirely fixed text; no trailing period; no variable parts)
  Message actual:   `Slo with arn 'arn:aws:application-signals:<region>:<account>:slo/<Name>' already exists.`  (fixed text `Slo with arn '` ... `' already exists.`; the single variable part is the existing SLO's computed ARN, taken from emulator state; plus a trailing period AWS does not send)

The region `us-west-2`, account id `000000000000` and the `sim2real-slo-<suffix>` name inside the actual message are per-run values of this replay, not part of the requirement — the requirement is that no ARN or identifier appears in the Message at all.

Prior-art linkage: 20261029T192739Z-application-signals-words-seven-refusal-messages-differently-from.md is status=fixed and lists Issue-Trace-Ids including `application-signals.slo-duplicate-name-conflict-rejected` with Issue-Key `...|run#11|AssertionError|assert message == "A resource with the same identifier already exists", (` — the same assertion that fails again here, so that fix did not land for this cause.

Model citation: CreateServiceLevelObjective POST /slo (code 200); ConflictException errorKind client, httpError 409, required member Message, doc "This operation attempted to create a resource that already exists."

## Steps to Reproduce
Boot the emulator and point a botocore `application-signals` client at it.

1. Pick a fresh SLO Name N matching ^[0-9A-Za-z][-._0-9A-Za-z ]{0,126}[0-9A-Za-z]$.
2. CreateServiceLevelObjective(Name=N, Description="original", RequestBasedSliConfig=<valid RequestBasedSliMetricConfig: a TotalRequestCountMetric and a MonitoredRequestCountMetric.BadCountMetric, Period 300, Stat Sum>, Tags=[...]). It succeeds; keep Slo.Arn.
3. Call CreateServiceLevelObjective again with the SAME Name=N, Description="duplicate attempt" and any valid RequestBasedSliConfig.
4. Inspect the ClientError: response["Error"]["Code"] == "ConflictException" (correct), but response["Error"]["Message"] is "Slo with arn '<the existing SLO's ARN>' already exists." instead of the expected "A resource with the same identifier already exists" (no trailing period, no ARN).
5. GetServiceLevelObjective(Id=<Arn from step 2>) still shows Description=='original' and the same Arn — the original is correctly untouched, confirming only the wording diverges.
6. Same root cause, concurrent entry: submit 8 concurrent CreateServiceLevelObjective calls on one fresh Name; exactly one wins and the 7 losers each carry the same wrong ARN-echoing Message.

Cleanup: DeleteServiceLevelObjective(Id=<Arn>) for every SLO created.
