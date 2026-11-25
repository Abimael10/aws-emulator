Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-parameter-unlabel-partial-semantics
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-parameter-unlabel-partial-semantics ssm-vscan-traj-patch-and-compliance-read-mistakes
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|GetParameterHistory|2xx->2xx|$.Parameters[*].Name
Issue-Key: ssm|DescribeInstancePatches|4xx->4xx|$.message
Issue-Commit: f91a062ed7cd5cab4f5d0dcf8790852b82a6c908

# ssm parameter-history name echo and patch-filter refusal diverge from AWS

> _Note: this issue was identified against the code base at commit `f91a062ed7cd5cab4f5d0dcf8790852b82a6c908`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetParameterHistory

## Description
Two net-new causes on ssm read paths: GetParameterHistory rewrites the parameter name it echoes back, and DescribeInstancePatches refuses a bad filter with emulator-authored prose instead of AWS's validation sentence. THIS ISSUE HAS 2 CAUSES — both must be fixed and re-replayed before it is closed.

Cause 1 of 2: GetParameterHistory echoes every history entry's Name with a '/' prepended, so the name does not match the name the parameter was created with.
AWS: Smithy (com.amazonaws.ssm) GetParameterHistory.Name is required, target PSParameterName (@length 1..2048); the result is GetParameterHistoryResult.Parameters -> ParameterHistoryList of ParameterHistory, whose Name member (target PSParameterName, doc "The name of the parameter.") reports the parameter's name. Parameter Store distinguishes one-level names, which have NO leading slash, from hierarchical names, which begin with '/' and whose levels are separated by '/'. AWS echoes the name as stored: a parameter created with a one-level name is reported in every history entry under exactly that name, with no leading slash added. The requirement is the identity Name-in == Name-out for each entry; the recording's particular name is just the probe's generated value.
Emulator: HTTP 200 with the right number of history entries (status and shape agree), but every entry's Name is the requested name with a leading '/' prepended — the emulator normalizes one-level names into hierarchical ones on the way out.
Why: the name is the parameter's identity. A client that correlates history entries back to the parameter it asked about, keys a map by Name, or feeds the echoed Name into a follow-up GetParameter/PutParameter/LabelParameterVersion call either fails to match or addresses a different (hierarchical) name than the one it created. It also makes the emulator self-inconsistent: the name the caller passed in is accepted, but the name handed back is not the one it used.
Fix: the GetParameterHistory response projection in the ssm handler, and the parameter-name normalization in the parameter store's state that it reads — the stored/echoed name must preserve the caller's form instead of forcing a leading '/'.
Traces: ssm-parameter-unlabel-partial-semantics#task#6.

Cause 2 of 2: DescribeInstancePatches' refusal of a filter carrying the wrong number of values is emulator-authored prose instead of AWS's validation message.
AWS: Smithy DescribeInstancePatches.Filters -> PatchOrchestratorFilterList (@length 0..5) of PatchOrchestratorFilter{Key @length 1..128, Values -> PatchOrchestratorFilterValues}; the operation models InvalidFilter (client, HTTP 400, awsQueryError code InvalidFilter). For DescribeInstancePatches, a patch filter must carry exactly one value, and when a filter's value list does not satisfy that, AWS answers HTTP 400 with the message "Invalid filter value size. A filter can only contain one value." delivered in the lower-case `message` envelope member. That whole sentence is fixed text — it has no variable parts: AWS reports the rule, not the offending key or value.
Emulator: the same HTTP 400 and the same error class and envelope member (only $.message diverged), but the message is "Filter key 'State' carries no value to search for" — a sentence the emulator wrote, which interpolates the filter key and describes a different rule ("no value" rather than "only one value").
Why: the message is the requirement for this error class. Clients, CLI users and log-matching tooling that key on AWS's sentence never see it, and the emulator's text misstates the constraint — a caller told the key "carries no value" may add several values, which AWS also rejects. Quoting the filter key also makes the message vary per request where AWS's does not, so assertions on it are unstable.
Fix: DescribeInstancePatches' filter validation / error construction in the ssm handler — raise InvalidFilter with AWS's fixed sentence for a filter whose value list is not exactly one.
Traces: ssm-vscan-traj-patch-and-compliance-read-mistakes#task#3.

No open issue covers either cause. The nearest open ssm issue, 20261113T231833Z-ssm-inventory-and-patch-baseline-reads-getinventory-misclassifies.md, is about GetInventory validation and the DescribePatchBaselines projection — different operations — and carries neither of the replay keys above.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator booted from this checkout (emulator_up true, /__admin/health 200). Both clusters are reported with stub_path empty and owned_by empty, i.e. net-new and unowned.

Cause 1 — GetParameterHistory, fixture ssm-parameter-unlabel-partial-semantics step task#6. status 200 expected, 200 actual.
  $.Parameters[0].Name value_mismatch: expected 'sim2real-ssm-unlabel-d8587bc2', got '/sim2real-ssm-unlabel-d8587bc2'.
  $.Parameters[1].Name value_mismatch: expected 'sim2real-ssm-unlabel-d8587bc2', got '/sim2real-ssm-unlabel-d8587bc2'.
  Both history entries show the same defect: a leading '/' the request did not contain. The name itself is generated per run; the requirement is that the echoed Name equals the name the parameter was created and queried with.

Cause 2 — DescribeInstancePatches, fixture ssm-vscan-traj-patch-and-compliance-read-mistakes step task#3. status 400 expected, 400 actual (class and envelope member agree; only the message diverged).
  $.message value_mismatch: expected 'Invalid filter value size. A filter can only contain one value.', got "Filter key 'State' carries no value to search for".
  The expected message is entirely fixed text with no variable parts; the emulator's message interpolates the request's filter key ('State' is that run's value) and states a different rule.

Model evidence used (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json):
- GetParameterHistory: input Name required -> PSParameterName @length 1..2048; output Parameters -> ParameterHistoryList; @paginated; errors InternalServerError, InvalidKeyId, InvalidNextToken, ParameterNotFound.
- ParameterHistory.Name -> PSParameterName @length 1..2048, doc "The name of the parameter."
- DescribeInstancePatches: InstanceId required; Filters -> PatchOrchestratorFilterList @length 0..5 of PatchOrchestratorFilter{Key @length 1..128, Values -> PatchOrchestratorFilterValues}; errors InternalServerError, InvalidFilter (client, 400, awsQueryError InvalidFilter), InvalidInstanceId, InvalidNextToken.

## Steps to Reproduce
Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus against trace set simparity-wire-aws-v1), or drive the steps directly against the ssm endpoint:

Cause 1: PutParameter a String parameter using a ONE-LEVEL name (no leading '/'), write a second version so there is history, then POST AmazonSSM.GetParameterHistory with that same name. Expect every Parameters[].Name to equal the name exactly as passed; observe each one returned with a '/' prepended. Fixture ssm-parameter-unlabel-partial-semantics step task#6.

Cause 2: POST AmazonSSM.DescribeInstancePatches for a managed node with a Filters entry whose Values list does not carry exactly one value (recorded: key 'State'). Expect HTTP 400 / InvalidFilter with message 'Invalid filter value size. A filter can only contain one value.'; observe HTTP 400 with "Filter key '<Key>' carries no value to search for". Fixture ssm-vscan-traj-patch-and-compliance-read-mistakes step task#3.

Both steps are in the same replayed corpus, so one replay after the fix re-exercises both causes (this issue's trace_ids names the second one's fixture#step).
