Issue-Source: trace-sweep-divergence
Issue-Trace: glue-gjwr-traj-hand-written-job-definition-refusals
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: glue-gjwr-traj-hand-written-job-definition-refusals glue-search-assets-no-matches glue-usage-profile-update-lifecycle
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|CreateJob|4xx->4xx|$.message
Issue-Key: glue|UpdateJob|4xx->4xx|$.Message
Issue-Key: glue|SearchAssets|2xx->2xx|$.TotalCount
Issue-Key: glue|SearchAssets|2xx->2xx|$headers.content-type
Issue-Key: glue|UpdateUsageProfile|4xx->2xx|$.Message
Issue-Key: glue|UpdateUsageProfile|4xx->2xx|$.__type
Issue-Key: glue|UpdateUsageProfile|4xx->2xx|status
Issue-Commit: e36f29dfe14d7969c6e4663f6c56187970cbb973

# Glue validation refusals and response envelopes diverge from AWS on four operations

> _Note: this issue was identified against the code base at commit `e36f29dfe14d7969c6e4663f6c56187970cbb973`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateJob

## Description
This issue bundles 4 net-new causes found replaying the wire corpus against glue. All four are the same family of defect: the emulator's request validation and response envelope for a Glue operation do not match what AWS serves — a refusal is worded/parameterised differently, an invalid request is not refused at all, or a success envelope is incomplete. Each cause is independent and must be fixed and re-replayed separately.

Cause 1 of 4: CreateJob — the WorkerType enum-constraint refusal enumerates a stale value set.
AWS: Smithy: CreateJob.WorkerType targets the WorkerType enum; modeled error InvalidInputException (httpError 400). A WorkerType the service does not know is refused 400 with the standard Smithy constraint message, whose fixed text is "1 validation error detected: Value '<value>' at '<memberName>' failed to satisfy constraint: Member must satisfy enum value set: [<set>]". The variable parts are <value> (the rejected value taken verbatim from the request), <memberName> (the lowerCamel member path, here workerType) and <set> (the service's CURRENT full WorkerType value set — set membership is the requirement, the printed order is not). The recording shows AWS's live set contains the R.* and *.GRAVITON worker families as well as the Spark/Ray ones: R.4X, R.2X, R.1X, G.8X, G.12X.GRAVITON, G.1X.GRAVITON, G.2X.GRAVITON, G.4X, G.16X.GRAVITON, G.16X, G.025X, G.4X.GRAVITON, G.2X, G.1X, G.12X, G.8X.GRAVITON, R.1X.GRAVITON, R.2X.GRAVITON, R.4X.GRAVITON, Z.2X, R.8X.GRAVITON, R.8X, Standard. The checked-in model is behind the service here (its WorkerType lists only Standard, G.1X, G.2X, G.025X, G.4X, G.8X, Z.2X), so the recorded response is the authority for the set.
Emulator: returns 400 with the right shape and the right fixed text, but prints only the seven model values — "... Member must satisfy enum value set: [G.8X, G.4X, G.025X, G.2X, G.1X, Z.2X, Standard]".
Why: a client that parses the refusal to discover the admissible worker types (or a user reading it to correct a typo) is told that R.1X, G.16X and every GRAVITON worker type is invalid, and a job definition that AWS would accept looks unavailable. It also means the emulator will refuse CreateJob for those worker types outright.
Fix: the CreateJob handler's input validation — the WorkerType enum value set it validates against and renders into the constraint message (and the same set wherever UpdateJob/CreateSession validate a worker type).
Traces: glue-gjwr-traj-hand-written-job-definition-refusals#task#4.

Cause 2 of 4: UpdateJob — the missing-Role refusal drops AWS's "or empty" clause.
AWS: Smithy: UpdateJob takes a required JobUpdate whose Role member is documented "The name or Amazon Resource Name (ARN) of the IAM role associated with this job (required)"; modeled error InvalidInputException (httpError 400). When JobUpdate carries no usable Role, AWS refuses 400 InvalidInputException with the Message "Role should not be null or empty." — the whole string is fixed text, with no values taken from the request or state.
Emulator: refuses with the correct status and __type but the Message "Role should not be null.", i.e. the "or empty" clause is missing, so the one validation that covers both the absent and the blank Role reports only the absent case.
Why: a client (or test) asserting on the refusal text, and a user shown it, get a sentence AWS never emits, and it mis-states the rule: an empty-string Role is refused too.
Fix: the UpdateJob handler's validation — the message constant for the Role check.
Traces: glue-gjwr-traj-hand-written-job-definition-refusals#task#13.

Cause 3 of 4: SearchAssets — the empty-result envelope omits TotalCount and is served with the awsJson content type.
AWS: recorded behavior only for both details. The model's SearchAssetsOutput declares only Items and NextToken (no TotalCount), and SearchAssets carries an @http POST /search-assets binding that is inert under the service's awsJson1_1 protocol. The recording shows that AWS nevertheless answers SearchAssets 200 with a TotalCount member — 0 when nothing matches, i.e. the count of matching assets, always present — and with the response header Content-Type: application/json rather than the awsJson1.1 media type the rest of the service uses. Hypothesis: SearchAssets is served by a REST/JSON frontend inside Glue rather than the awsJson one, which is also why it carries a REST uri in the model.
Emulator: answers 200 but with no TotalCount member at all, and with Content-Type: application/x-amz-json-1.1.
Why: a client that reads TotalCount to show "0 results" or to size a result set gets null and cannot distinguish "no matches" from "field unsupported"; a client or proxy that dispatches on the response media type sees a type AWS does not send for this operation.
Fix: the SearchAssets handler's serializer/response envelope — emit TotalCount (the number of matching assets, 0 for an empty result) and serve this operation's response with the application/json content type.
Traces: glue-search-assets-no-matches#task#1.

Cause 4 of 4: UpdateUsageProfile — a half-specified job default pair is accepted instead of refused.
AWS: Smithy: UpdateUsageProfile takes a required Configuration of ProfileConfiguration (SessionConfiguration / JobConfiguration, each a map of configuration objects); modeled error InvalidInputException (httpError 400). Recorded behavior for the rule itself: when the JobConfiguration sets a default for exactly one of the worker-type / number-of-workers pair, AWS refuses the update 400 InvalidInputException with the Message "Please set default value for both Worker type and Number of workers in jobs configuration, if you intend to set default for either parameter." The whole sentence is fixed text — it names no value from the request.
Emulator: answers 200 and applies the update, emitting no __type and no Message, so the half-specified profile is persisted.
Why: a client relying on AWS's behaviour gets a usage profile AWS would never have created — a job default pair that is only half set — and discovers the inconsistency later (or never), instead of a 400 at the call that caused it. A negative test for this rule passes against the emulator and fails against AWS.
Fix: the UpdateUsageProfile handler's validation — add the cross-member check over JobConfiguration's worker-type / number-of-workers defaults before the profile is mutated (and keep CreateUsageProfile consistent with it).
Traces: glue-usage-profile-update-lifecycle#task#2.

Related open issues, none of which owns these steps (the replay reports no owner for any of the four): "Glue input-validation refusals carry the wrong parameter, error type and status" (20261119T092454Z-glue-input-validation-refusals-carry-the-wrong-parameter.md) and "Glue accepts requests AWS refuses and answers 200 where AWS returns 400" (20261117T133709Z-glue-accepts-requests-aws-refuses-and-answers-200.md) describe the same families on other operations.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator (emulator_up=true, 1613 steps, 1417 passed, score 87.8%). The four clusters below were reported with stub_path="" and owned_by=[] (net-new).

Cause 1 — fixture glue-gjwr-traj-hand-written-job-definition-refusals, step task#4, CreateJob, status 400 expected / 400 actual:
  $.message expected "1 validation error detected: Value 'G.1x' at 'workerType' failed to satisfy constraint: Member must satisfy enum value set: [R.4X, R.2X, R.1X, G.8X, G.12X.GRAVITON, G.1X.GRAVITON, G.2X.GRAVITON, G.4X, G.16X.GRAVITON, G.16X, G.025X, G.4X.GRAVITON, G.2X, G.1X, G.12X, G.8X.GRAVITON, R.1X.GRAVITON, R.2X.GRAVITON, R.4X.GRAVITON, Z.2X, R.8X.GRAVITON, R.8X, Standard]"
  $.message actual   "1 validation error detected: Value 'G.1x' at 'workerType' failed to satisfy constraint: Member must satisfy enum value set: [G.8X, G.4X, G.025X, G.2X, G.1X, Z.2X, Standard]"
  (The 'G.1x' in the message is the request's value, not a requirement; the enumerated set is.)

Cause 2 — fixture glue-gjwr-traj-hand-written-job-definition-refusals, step task#13, UpdateJob, status 400 expected / 400 actual:
  $.Message expected 'Role should not be null or empty.'
  $.Message actual   'Role should not be null.'

Cause 3 — fixture glue-search-assets-no-matches, step task#1, SearchAssets, status 200 expected / 200 actual:
  $.TotalCount expected 0, actual missing (None)
  $headers.content-type expected 'application/json', actual 'application/x-amz-json-1.1'

Cause 4 — fixture glue-usage-profile-update-lifecycle, step task#2, UpdateUsageProfile, status expected 400, actual 200:
  $.__type expected 'InvalidInputException', actual missing (None)
  $.Message expected 'Please set default value for both Worker type and Number of workers in jobs configuration, if you intend to set default for either parameter.', actual missing (None)

Model facts cited (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json, AWSGlue 2017-03-31, protocol awsJson1_1): CreateJob.WorkerType -> WorkerType enum values [Standard, G.1X, G.2X, G.025X, G.4X, G.8X, Z.2X], errors include InvalidInputException (400); UpdateJob input JobUpdate required, JobUpdate.Role documented "(required)", errors include InvalidInputException (400); SearchAssetsOutput members Items, NextToken only (no TotalCount), @http POST /search-assets flagged httpBindingsInert; UpdateUsageProfile.Configuration -> ProfileConfiguration {SessionConfiguration, JobConfiguration}, errors include InvalidInputException (400).

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the simparity-wire-aws-v1 wire corpus.
3. Cause 1: replay fixture glue-gjwr-traj-hand-written-job-definition-refusals step task#4 — CreateJob with WorkerType 'G.1x' (an unknown value); compare the $.message enum value set against the recorded AWS set.
4. Cause 2: replay the same fixture, step task#13 — UpdateJob with a JobUpdate that supplies no usable Role; compare $.Message against 'Role should not be null or empty.'.
5. Cause 3: replay fixture glue-search-assets-no-matches step task#1 — SearchAssets with a search that matches nothing; check for a TotalCount member (0) in the 200 body and a Content-Type of application/json.
6. Cause 4: replay fixture glue-usage-profile-update-lifecycle step task#2 — UpdateUsageProfile whose JobConfiguration sets a default for only one of the worker-type / number-of-workers pair; expect 400 InvalidInputException with the pairing message, not 200.
