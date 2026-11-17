Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: accessanalyzer.list-analyzers-account-type-tracks-exact-membership
Issue-Shape-Id: com.amazonaws.accessanalyzer#AccessAnalyzer
Issue-Key: accessanalyzer.list-analyzers-account-type-tracks-exact-membership|_expect_error#14|AssertionError|raise AssertionError(f"expected {code} ({message!r}), call returned {result!r} instead")
Issue-Commit: 077021ed17173fe4a565f7526f07b6650a74c582

# accessanalyzer: CreateAnalyzer accepts a second ACCOUNT-type analyzer instead of refusing it with ServiceQuotaExceededException "Analyzer limit exceeded"

> _Note: this issue was identified against the code base at commit `077021ed17173fe4a565f7526f07b6650a74c582`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAnalyzer

## Description
One net-new cause.

Cause 1 of 1: CreateAnalyzer does not enforce the one-analyzer-per-type-per-account-per-Region limit, so a second ACCOUNT-type create succeeds. Failing check: accessanalyzer.list-analyzers-account-type-tracks-exact-membership.

AWS: the Smithy model documents on CreateAnalyzerRequest$type — "The type of analyzer to create. You can create only one analyzer per account per Region. You can create up to 5 analyzers per organization per Region." CreateAnalyzer models ServiceQuotaExceededException with httpError 402 (Smithy: CreateAnalyzer errors = AccessDeniedException 403, ConflictException 409, InternalServerException 500, ServiceQuotaExceededException 402, ThrottlingException 429, ValidationException 400). The recorded behavior shows the limit is scoped PER ANALYZER TYPE CLASS, not as a single global count: with one ACCOUNT (external-access) analyzer already present, a CreateAnalyzer(type=ACCOUNT_UNUSED_ACCESS) is accepted and returns a distinct ARN, while a second CreateAnalyzer(type=ACCOUNT) is REFUSED with error code ServiceQuotaExceededException and message "Analyzer limit exceeded". That message is entirely FIXED text — it carries no request- or state-derived values (no analyzer name, no limit number, no account or Region), because the limit is a property of the account and Region rather than of the request. The refusal must also leave the listing untouched: ListAnalyzers(type=ACCOUNT) still names exactly the one pre-existing ACCOUNT analyzer.

Emulator: the second CreateAnalyzer(type=ACCOUNT) is ACCEPTED. The emulator answers HTTP 200 with a newly minted analyzer ARN (an arn:aws:access-analyzer:<region>:<account>:analyzer/<name> for the second requested name) and stores a second ACCOUNT-type analyzer. The only quota gate in the handler is a global store ceiling: services/.../src/ops/create_analyzer.rs refuses only when state.analyzers.len() >= crate::state::MAX_ANALYZERS, and MAX_ANALYZERS is 1_000 (documented in src/state.rs as the emulator's own memory ceiling and explicitly "NOT a fidelity claim about the published quota"). Nothing counts analyzers by type, so the 402 is never reached for the second ACCOUNT create. The replayed check asserted the refusal and instead got {'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'arn': 'arn:aws:access-analyzer:...:analyzer/<second-name>'}.

Why: a client that relies on AWS's singleton rule gets wrong state in two ways. First, an application or test that probes for the limit (create, expect ServiceQuotaExceededException 402, then reuse the existing analyzer) sees a success and proceeds as if it owns a brand-new ACCOUNT analyzer that real AWS would never have created; against real AWS the same code path takes the error branch, so the emulator exercises the opposite branch. Second, the account's observable topology diverges: ListAnalyzers(type=ACCOUNT) now names two analyzers where AWS names exactly one, so any code that treats the ACCOUNT analyzer as an account singleton (resolving "the" analyzer by listing and taking the sole entry, or asserting exact listing membership) breaks or picks an analyzer that cannot exist. Findings/archive-rule workflows hung off the "wrong" duplicate analyzer then silently diverge too.

Fix: belongs in the CreateAnalyzer handler's quota/precondition check (the branch that currently tests only the global store ceiling) and the quota-error construction it calls in the service state module. The check must count the account's EXISTING analyzers of the requested type class and refuse the create with the modeled ServiceQuotaExceededException (HTTP 402) and the fixed message "Analyzer limit exceeded" when one of that class is already present, before anything is stored, leaving the analyzer store and the ListAnalyzers projection unchanged. The ACCOUNT_UNUSED_ACCESS class must remain independently creatable alongside an ACCOUNT analyzer. The emulator's existing memory ceiling can stay as a separate, larger backstop, but it is not the AWS rule and must not be the only gate.

Traces: accessanalyzer.list-analyzers-account-type-tracks-exact-membership — creates an ACCOUNT analyzer, creates an ACCOUNT_UNUSED_ACCESS analyzer alongside it (expected to succeed), then expects the second ACCOUNT create to be refused; the handler file and the ceiling constant are services/service-com-amazonaws-accessanalyzer-accessanalyzer/src/ops/create_analyzer.rs (quota branch) and services/service-com-amazonaws-accessanalyzer-accessanalyzer/src/state.rs (MAX_ANALYZERS, analyzer_quota_exceeded).

## Evidence
Check accessanalyzer.list-analyzers-account-type-tracks-exact-membership (mismatch).

Expected (AWS / recorded): CreateAnalyzer(analyzerName=<second-name>, type="ACCOUNT") while an ACCOUNT analyzer already exists in the account/Region is refused — HTTP 402, Error.Code = "ServiceQuotaExceededException", Error.Message = "Analyzer limit exceeded" (wholly fixed text; no request/state values interpolated). ListAnalyzers(type=ACCOUNT) still names exactly the one pre-existing ACCOUNT analyzer.

Actual (emulator): the call SUCCEEDS. Assertion text from the replay:
  AssertionError: expected ServiceQuotaExceededException ('Analyzer limit exceeded'), call returned {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/json', ...}, 'RetryAttempts': 0}, 'arn': 'arn:aws:access-analyzer:us-west-2:000000000000:analyzer/sim2real-behav-aa-track-second-86b0c1c8f871'} instead
(the ARN, name suffix, region/account and timestamp above are per-run values, not requirements; the requirement is the 402 + code + fixed message, and that no analyzer is stored.)

Supporting evidence in the checkout: services/service-com-amazonaws-accessanalyzer-accessanalyzer/src/ops/create_analyzer.rs lines 217-225 gate only on `state.analyzers.len() >= crate::state::MAX_ANALYZERS`; services/service-com-amazonaws-accessanalyzer-accessanalyzer/src/state.rs line 355 sets `MAX_ANALYZERS: usize = 1_000` and its doc comment states the value is not a fidelity claim about the published quota. No per-type counting exists anywhere in the create path.

## Steps to Reproduce
1. CreateAnalyzer(analyzerName="aa-first", type="ACCOUNT") -> note the returned arn.
2. ListAnalyzers(type="ACCOUNT") -> names exactly {"aa-first": <arn>}.
3. CreateAnalyzer(analyzerName="aa-first-ua", type="ACCOUNT_UNUSED_ACCESS") -> succeeds with a distinct arn (correct; must keep succeeding).
4. CreateAnalyzer(analyzerName="aa-second", type="ACCOUNT").
   Expected: HTTP 402 ServiceQuotaExceededException, message "Analyzer limit exceeded".
   Observed: HTTP 200 with a new analyzer arn for "aa-second".
5. ListAnalyzers(type="ACCOUNT") -> emulator now names two ACCOUNT analyzers; AWS names only "aa-first".
