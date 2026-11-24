Issue-Source: trace-sweep-divergence
Issue-Trace: athena-capacity-reservation-validation-and-lookup
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: athena-capacity-reservation-validation-and-lookup athena-notebook-reference-and-workgroup-type-errors
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena|ListCapacityReservations|4xx->4xx|$.Message
Issue-Key: athena|CreateNotebook|4xx->4xx|$.Message
Issue-Commit: 93ff44361a038ac1da9d65dc0eee101932df58d5

# Athena ListCapacityReservations token and CreateNotebook non-Spark refusals reword AWS's message (2 causes)

> _Note: this issue was identified against the code base at commit `93ff44361a038ac1da9d65dc0eee101932df58d5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListCapacityReservations

## Description
TWO net-new causes, both the same fault: an Athena InvalidRequestException refusal returns the correct HTTP 400 and the correct exception type, but its Message is emulator-authored prose instead of the fixed text AWS words for that error class — and in one case it additionally interpolates a request value AWS's message does not contain.

Cause 1 of 2 - ListCapacityReservations with a malformed NextToken is refused with the emulator's own token wording (fixture athena-capacity-reservation-validation-and-lookup, step task#1).
AWS: Smithy com.amazonaws.athena#ListCapacityReservations takes NextToken (target Token, @length min 1 max 1024) and MaxResults (range 1-50) and is modeled with errors InvalidRequestException (client, httpError 400) and InternalServerException; it is @paginated with inputToken/outputToken NextToken. A NextToken that is not a token this service issued is refused with HTTP 400, __type InvalidRequestException, and the message 'Invalid next token provided'. That message is entirely fixed text for this error class: it has NO variable parts — AWS does not echo the offending token value back. (Message text is recorded behavior; the shape/constraints and the modeled error are from the Smithy model.)
Emulator: HTTP 400 and the exception type are right, but the message is the emulator-authored 'Invalid NextToken is provided' — different fixed wording for the same refusal.
Why: the error message is a requirement, not cosmetics. Athena's InvalidRequestException covers many distinct causes on the same status, so a client (or a test fixture, or a log-scraping operator runbook) that distinguishes a stale/garbled pagination token from other 400s by matching AWS's message text never matches here, and the pagination-recovery branch that should fire on an expired token silently does not.
Fix: the ListCapacityReservations handler's NextToken validation / refusal construction in the athena service crate — emit AWS's fixed message for the invalid-pagination-token refusal rather than a locally phrased one.
Traces: athena-capacity-reservation-validation-and-lookup#task#1.

Cause 2 of 2 - CreateNotebook against a workgroup that is not Spark-enabled rewords the refusal and interpolates the workgroup name (fixture athena-notebook-reference-and-workgroup-type-errors, step task#3).
AWS: Smithy com.amazonaws.athena#CreateNotebook requires WorkGroup (WorkGroupName, pattern ^[a-zA-Z0-9._-]{1,128}$) and Name (NotebookName, length 1-255), and is modeled with errors InvalidRequestException (client, httpError 400), TooManyRequestsException and InternalServerException. Creating a notebook in a workgroup whose engine is not Spark is refused with HTTP 400, __type InvalidRequestException, and the message 'Workgroup EngineVersion is not Spark.' — all fixed text, with NO variable parts: AWS does not name the workgroup in this message. (Message text is recorded behavior; the error and input shape are modeled.)
Emulator: HTTP 400 and the exception type are right, but the message is the emulator-authored 'WorkGroup <workGroupName> is not a Spark enabled workgroup.' — different fixed wording, plus a workgroup name taken from the request that AWS's message for this class does not contain.
Why: a client branching on this refusal to tell 'wrong workgroup type' from the other InvalidRequestException causes on CreateNotebook has only the message to match on, and the string it matches does not exist on AWS; the interpolated name also makes the emulator's text vary per request where AWS's does not, so even an exact-match assertion written against the emulator is unstable.
Fix: the CreateNotebook handler's workgroup-type validation / refusal construction in the athena service crate — emit AWS's fixed non-Spark-workgroup message with no request interpolation.
Traces: athena-notebook-reference-and-workgroup-type-errors#task#3.

Related open issues name the same family of defect on neighbouring operations but carry neither of these clusters' replay keys, so both causes are filed here and those issues are named for the fixer: 20261108T041854Z-athena-notebook-operations-refuse-without-athenaerrorcode-errorcode-and.md (same non-Spark message on ImportNotebook step task#4 and ExportNotebook step task#8, NOT CreateNotebook task#3), 20261111T064644Z-athena-listworkgroups-summaries-drop-description-and-engineversion-category.md (a re-worded malformed-token refusal on ListWorkGroups, not on ListCapacityReservations), and 20261111T123309Z-athena-capacity-reservation-read-and-lifecycle-paths-diverge.md (capacity-reservation not-found wording, not the pagination-token refusal). The open stub issue 20261104T163600Z-athena-createnotebook-stub.md no longer matches observed behavior: CreateNotebook now answers a real 400, it just answers it with the wrong message.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 37 fixtures, 394 steps, 308 passed, 86 mismatched, score 78.2%, emulator_up true, bootstrap_signal false. Both clusters below report stub_path empty and owned_by empty (no filed issue carries their replay key).

Cause 1 - athena-capacity-reservation-validation-and-lookup#task#1, ListCapacityReservations, status 400 expected vs 400 actual:
  $.Message: value_mismatch (expected 'Invalid next token provided', got 'Invalid NextToken is provided')
  Required text: the whole string 'Invalid next token provided' is fixed; no part of it is derived from the request or from emulator state.
  divergence_key: service=athena|ListCapacityReservations|status=400->400|keys=$.Message

Cause 2 - athena-notebook-reference-and-workgroup-type-errors#task#3, CreateNotebook, status 400 expected vs 400 actual:
  $.Message: value_mismatch (expected 'Workgroup EngineVersion is not Spark.', got 'WorkGroup sim2real-probe-athena-notebook-ref-wg-e7343abd is not a Spark enabled workgroup.')
  Required text: the whole string 'Workgroup EngineVersion is not Spark.' is fixed; the workgroup name in the emulator's text is a value taken from the request's WorkGroup member and is not part of AWS's template at all.
  divergence_key: service=athena|CreateNotebook|status=400->400|keys=$.Message

Smithy cited: com.amazonaws.athena#ListCapacityReservations — input NextToken (Token, @length 1..1024), MaxResults (1..50); errors InvalidRequestException (client, httpError 400), InternalServerException; @paginated inputToken=NextToken/outputToken=NextToken. com.amazonaws.athena#CreateNotebook — input requires WorkGroup (pattern ^[a-zA-Z0-9._-]{1,128}$) and Name (length 1..255); errors InvalidRequestException (client, httpError 400), TooManyRequestsException, InternalServerException. The exact message strings are recorded behavior only.

## Steps to Reproduce
1. git clone the project repo and boot the emulator built from that checkout.
2. Replay the wire corpus simparity-wire-aws-v1 against it.
3. Cause 1: fixture athena-capacity-reservation-validation-and-lookup, step task#1 — call ListCapacityReservations with a NextToken value the service never issued (a non-empty, syntactically unremarkable string). Inspect the 400 body's Message: it must be exactly 'Invalid next token provided'.
4. Cause 2: fixture athena-notebook-reference-and-workgroup-type-errors, step task#3 — create a workgroup whose EngineVersion is a SQL/Presto engine (not Spark), then call CreateNotebook with that WorkGroup and any valid Name. Inspect the 400 body's Message: it must be exactly 'Workgroup EngineVersion is not Spark.' with no workgroup name interpolated.
