Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: macie2.create-allow-list-mutually-exclusive-criteria-rejected macie2.get-findings-batch-size-limit-rejected
Issue-Shape-Id: com.amazonaws.macie2#Macie2
Issue-Key: macie2.create-allow-list-mutually-exclusive-criteria-rejected|run#10|AssertionError|assert error.get("Message") == (
Issue-Key: macie2.get-findings-batch-size-limit-rejected|run#9|AssertionError|assert error["Message"] == (
Issue-Commit: 029e84051a36c895c74f0bab3e13d17031d444b4

# macie2: 2 net-new behavioral root causes — CreateAllowList mutually-exclusive-criteria rejection uses the Smithy doc sentence as its error message, and GetFindings findingIds over-limit rejection does not use AWS's constraint-violation message

> _Note: this issue was identified against the code base at commit `029e84051a36c895c74f0bab3e13d17031d444b4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAllowList

## Description
Two distinct root causes in the macie2 emulator. Both are ValidationException rejections that fire at the right moment with the right error code and status, but carry a message that is not the one AWS words for that error class. The emulator mints its own prose instead of AWS's, so any client, SDK waiter, log assertion or support-flow that reads the message text sees something AWS never emits.

=== Cause 1 of 2: CreateAllowList rejects both-criteria input with the Smithy documentation sentence instead of AWS's error message ===
Failing check: macie2.create-allow-list-mutually-exclusive-criteria-rejected

AWS: Smithy — CreateAllowList (POST /allow-lists), input CreateAllowListRequest with required name, clientToken and criteria; criteria targets AllowListCriteria, whose documentation states "Specifies the criteria for an allow list. The criteria must specify a regular expression (regex) or an S3 object (s3WordsList). It can't specify both." ValidationException is a modeled error of the operation (com.amazonaws.macie2#ValidationException, errorKind client, httpError 400, single `message` member). The mutual exclusion itself is modeled only as prose, so the rejection's wording is recorded behavior only: AWS refuses a CreateAllowList whose criteria sets BOTH `regex` and `s3WordsList` with HTTP 400 / `ValidationException` and the message `Allow list criteria can specify a regex or an S3 object, but not both.` That message is entirely fixed text for this error class — it interpolates no name, id, value or member from the request — so there is no per-request variable part in it and nothing in it may be taken from a recording. Nothing is persisted on the rejected path: the rejected `name` must not appear in ListAllowLists. A single-criterion (regex-only) CreateAllowList is accepted and GetAllowList returns the minted id.

Emulator: the call IS refused with code `ValidationException` (so the validation exists and fires in the right place), but the message is `The criteria must specify a regular expression (regex) or an S3 object (s3WordsList). It can't specify both.` — i.e. the handler has copied the AllowListCriteria shape's `documentation` trait verbatim into the error body rather than emitting the message AWS returns. The replayed check asserted the message and failed with: AssertionError: unexpected message: "The criteria must specify a regular expression (regex) or an S3 object (s3WordsList). It can't specify both."

Why: a client that branches on the rejection message (error-message matching is the only discriminator AWS gives here, because every input problem on this operation shares the single code `ValidationException`) cannot tell the mutual-exclusion violation apart from any other validation failure, and a test or runbook written against real AWS text fails against the emulator. Reusing the model's documentation prose as runtime text is also a general-purpose hazard: it will drift with the model instead of with AWS's error catalogue.

Fix: the CreateAllowList input validation inside the CreateAllowList handler — the branch that detects criteria with both `regex` and `s3WordsList` set must emit the modeled ValidationException with AWS's fixed message text, not the AllowListCriteria documentation string. Note the related fixed issue 20261102T000237Z-macie2-createallowlist-never-validates-an-s3wordslist-criteria-or.md, which covered the s3WordsList object-existence rejection on the same operation; this is a separate message, on the separate both-criteria branch, and that issue's fix did not set it. (The call is NOT refused where AWS succeeds here, so this is not a regression of that fix — the positive control, regex-only creation plus GetAllowList, passed.)

Traces: macie2.create-allow-list-mutually-exclusive-criteria-rejected

=== Cause 2 of 2: GetFindings over-limit findingIds rejection does not use AWS's standard constraint-violation message ===
Failing check: macie2.get-findings-batch-size-limit-rejected

AWS: Smithy — GetFindings (POST /findings/describe), input GetFindingsRequest.findingIds, a required list targeting __listOf__string; the 50-element cap is not a modeled @length trait, it is stated in the member's documentation ("An array of strings that lists the unique identifiers for the findings to retrieve. You can specify as many as 50 unique identifiers in this array."). ValidationException is a modeled error of the operation (client, httpError 400). Recorded behavior: a findingIds array longer than 50 is refused at the request-validation layer with HTTP 400 / `ValidationException` and AWS's standard constraint-violation message, whose template is: `1 validation error detected: Value '[<the submitted list elements, joined by ", ">]' at 'findingIds' failed to satisfy constraint: Member must have length less than or equal to 50`. Fixed text: `1 validation error detected: Value '[`, then `]' at '`, then `' failed to satisfy constraint: Member must have length less than or equal to 50`. Variable parts taken from the request and the constraint: the echoed element list (every submitted findingIds value, in request order, comma-space separated), the member name `findingIds`, and the limit `50`. The specific 51 ids in any one recording fill that template and are not part of it. A within-limit call using a real finding id succeeds and returns exactly that finding.

Emulator: refuses with the right code and status but an invented message — `The findingIds array can contain as many as 50 unique identifiers; the request named 51.` The replayed check asserted the message and failed with: AssertionError: unexpected message: 'The findingIds array can contain as many as 50 unique identifiers; the request named 51.' Note the emulator's text neither echoes the offending value nor uses the `N validation error(s) detected: Value ... at '<member>' failed to satisfy constraint: ...` shape that AWS's front-end validator produces for every such violation, so the divergence is systematic, not a typo: no over-limit/constraint rejection on this service will match AWS.

Why: clients and tools parse AWS's constraint-violation message to recover WHICH member failed and WHAT limit it violated (the member path and the limit are the only machine-readable parts, since the code is just `ValidationException`). Against the emulator that parse yields nothing, so a client cannot report the offending field, and a batching client that trims its request based on the reported limit cannot read one. Suites and assertions written against AWS text fail.

Fix: the GetFindings handler's input validation, and the shared request-validation/constraint-error formatting it should delegate to — the over-limit path must render AWS's standard constraint-violation message template (error count prefix, echoed value, member path, constraint clause) instead of bespoke prose, so every constrained member on the service reports uniformly.

Traces: macie2.get-findings-batch-size-limit-rejected

## Evidence
Suite: simparity-behavioral-aws-v1. macie2 11/22 passed; the two checks below are the only failures not already covered by a filed unimplemented-stub issue.

--- Cause 1: check macie2.create-allow-list-mutually-exclusive-criteria-rejected (verdict: mismatch) ---
Request: CreateAllowList(name=<unique>, clientToken=<unique>, criteria={"regex": ".+", "s3WordsList": {"bucketName": "some-bucket-name", "objectKey": "words.txt"}})
Expected (AWS): HTTP 400, Code `ValidationException`, Message exactly `Allow list criteria can specify a regex or an S3 object, but not both.` (all fixed text), and no allow list with the rejected name in ListAllowLists.
Actual (emulator): HTTP 400, Code `ValidationException` (correct), Message `The criteria must specify a regular expression (regex) or an S3 object (s3WordsList). It can't specify both.` — the AllowListCriteria shape's documentation sentence.
Replay output:
  File "check_macie2_create_allow_list_mutually_exclusive_criteria_rejected.py", line 48, in run
    assert error.get("Message") == (
  AssertionError: unexpected message: "The criteria must specify a regular expression (regex) or an S3 object (s3WordsList). It can't specify both."
Positive control in the same check (regex-only CreateAllowList + GetAllowList) passed, so only the message diverges.

--- Cause 2: check macie2.get-findings-batch-size-limit-rejected (verdict: mismatch) ---
Request: GetFindings(findingIds=[51 distinct 32-hex-char ids])
Expected (AWS): HTTP 400, Code `ValidationException`, Message rendered from AWS's constraint-violation template: `1 validation error detected: Value '[<all submitted findingIds, ", "-joined>]' at 'findingIds' failed to satisfy constraint: Member must have length less than or equal to 50`.
Actual (emulator): HTTP 400, Code `ValidationException` (correct), Message `The findingIds array can contain as many as 50 unique identifiers; the request named 51.`
Replay output:
  File "check_macie2_get_findings_batch_size_limit_rejected.py", line 33, in run
    assert error["Message"] == (
  AssertionError: unexpected message: 'The findingIds array can contain as many as 50 unique identifiers; the request named 51.'
The check's positive control (GetFindings with one real sample-finding id returning exactly that finding) was reached only after the assertion, so it is unverified by this run; the recorded behavior is that it succeeds.

## Steps to Reproduce
Cause 1 (CreateAllowList both-criteria message):
1. Boot the macie2 emulator; ensure a Macie session exists (GetMacieSession, else EnableMacie, tolerating ConflictException).
2. Positive control: CreateAllowList(name=<unique>, clientToken=<unique>, criteria={"regex": ".+"}) and GetAllowList on the returned id — both succeed today.
3. CreateAllowList(name=<unique-bad>, clientToken=<unique>, criteria={"regex": ".+", "s3WordsList": {"bucketName": "some-bucket-name", "objectKey": "words.txt"}}).
4. Observe Code `ValidationException` (correct) but Message `The criteria must specify a regular expression (regex) or an S3 object (s3WordsList). It can't specify both.` instead of AWS's `Allow list criteria can specify a regex or an S3 object, but not both.`
5. Confirm via ListAllowLists that the rejected name was not persisted (this part already matches).
6. Equivalent: replay check macie2.create-allow-list-mutually-exclusive-criteria-rejected.

Cause 2 (GetFindings over-limit message):
1. Boot the macie2 emulator; ensure a Macie session exists; call CreateSampleFindings.
2. Poll ListFindings(findingCriteria={"criterion": {"sample": {"eq": ["true"]}}}) until findingIds is non-empty.
3. Call GetFindings with 51 distinct findingIds (e.g. f"{i:032x}" for i in range(51)).
4. Observe Code `ValidationException` (correct) but Message `The findingIds array can contain as many as 50 unique identifiers; the request named 51.` instead of `1 validation error detected: Value '[<the 51 submitted ids, ", "-joined>]' at 'findingIds' failed to satisfy constraint: Member must have length less than or equal to 50`.
5. Then call GetFindings(findingIds=<first real id>) and confirm exactly one finding with that id is returned.
6. Equivalent: replay check macie2.get-findings-batch-size-limit-rejected.
