Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue__kinesis__kinesisanalyticsv2.stop-and-force-stop-of-a-never-started-studio-notebook-rejected
Issue-Key: glue__kinesis__kinesisanalyticsv2.stop-and-force-stop-of-a-never-started-studio-notebook-rejected|run#22|AssertionError|assert got == want, f"StopApplication on an unknown name gave {got!r}, expected {want!r}"
Issue-Commit: dda73b1c89ff90e3a0e7799df332912882e239a1

# StopApplication's ResourceNotFoundException message for an unknown application name carries a trailing period AWS does not emit

> _Note: this issue was identified against the code base at commit `dda73b1c89ff90e3a0e7799df332912882e239a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopApplication

## Description
REOPENS 20261102T114851Z-glue-kinesis-kinesisanalyticsv2-stopapplication-s-resourceinuseexception-message-for.md: its fix left the call refused. That issue carried this exact check as its replay key and fixed StopApplication's ResourceInUseException wording for a READY Studio notebook (the check now gets past those two arms), but the same check's final StopApplication call is still refused with a message AWS does not emit, so the check does not pass.

ONE cause. The divergence is in kinesisanalyticsv2, a bootstrapped service that answers; the Glue database and IAM role in the check are only the Studio notebook's catalog configuration and execution identity and are not involved in the refusal.

Cause 1 of 1: StopApplication on an application name that was never created is refused with the right code and status but with a message that appends a sentence-final period.
AWS: Smithy (services/service-com-amazonaws-kinesisanalyticsv2-kinesisanalytics-20180523): StopApplication input is ApplicationName (@required, length 1..128, pattern ^[a-zA-Z0-9_.-]+$) plus Force, and the operation declares ResourceNotFoundException (errorKind client, httpError 400, doc "Specified application can't be found."). A name that resolves to no application is refused — not reported as un-stoppable — with HTTP 400, Error.Code ResourceNotFoundException and the message "Application '<ApplicationName>' does not exist": the fixed text is "Application '" and "' does not exist" with NO terminating period, and the single variable part is the ApplicationName echoed from the request, in single quotes. Reference: API_StopApplication. Note that this service does not use one not-found sentence everywhere — the recorded DescribeApplication not-found message DOES end in a period (see open issue 20261103T054447Z, Cause 5) — so the fix must be made on the StopApplication path and must not be generalised into a single shared sentence for both operations.
Emulator: HTTP 400 with Error.Code ResourceNotFoundException (correct) and Error.Message "Application '<ApplicationName>' does not exist." — identical apart from one extra trailing '.' character.
Why: the refusal text is part of the contract clients assert on and operators match against. A client (or test suite, or log-scraping automation) that compares the not-found message for an exact match, or that strips the known AWS sentence to recover the application name, sees a string AWS never emits and takes its unknown-error branch even though the operation, status and error code are all right.
Fix: the not-found refusal in the StopApplication handler of the kinesisanalyticsv2 service crate — emit AWS's sentence for this operation without the terminating period, keeping the request's ApplicationName as the only interpolated value, and leave the DescribeApplication not-found sentence (which does end in a period) alone.
Traces: glue__kinesis__kinesisanalyticsv2.stop-and-force-stop-of-a-never-started-studio-notebook-rejected (the final StopApplication assertion of the check, after the READY/Force arms it also exercises).

Relation to open issues: 20261103T022644Z-kinesisanalyticsv2-studio-flink-runtime-rules-unenforced-createapplication-fails-open.md and 20261103T054447Z-kinesisanalyticsv2-refusal-responses-diverge-wrong-error-class-wrong.md describe other kinesisanalyticsv2 refusal-message defects (CreateApplication/DeleteApplication/DescribeApplication/UpdateApplication); neither carries this check's replay key nor this StopApplication not-found message, so neither covers this cause.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout.

glue__kinesis__kinesisanalyticsv2.stop-and-force-stop-of-a-never-started-studio-notebook-rejected [mismatch]:
AssertionError: StopApplication on an unknown name gave ('ResourceNotFoundException', "Application 'sim2real-flk-stop-missing-c4d4d776d205' does not exist."), expected ('ResourceNotFoundException', "Application 'sim2real-flk-stop-missing-c4d4d776d205' does not exist")

Expected (AWS): HTTP 400, Error.Code='ResourceNotFoundException', Error.Message="Application '<name>' does not exist" — fixed text "Application '" + "' does not exist", with <name> taken verbatim from the request's ApplicationName. The quoted name above is this run's generated value filling that template, not part of the requirement.
Actual (emulator): same status and code, Error.Message="Application '<name>' does not exist." — one extra trailing '.'.

The earlier arms of the same check now agree (ResourceInUseException "Application cannot be Stopped in 'READY' state" and the Force=true variant, the untouched READY/version state, and the control UpdateApplication to version 2), so this trailing period is the only remaining divergence in the check.

Smithy citations: StopApplication input ApplicationName @required (length 1..128, pattern ^[a-zA-Z0-9_.-]+$), Force -> BooleanObject; errors ConcurrentModificationException (409), InvalidApplicationConfigurationException (400), InvalidArgumentException (400), InvalidRequestException (400), ResourceInUseException (400), ResourceNotFoundException (400, "Specified application can't be found.").

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. glue:CreateDatabase and create an IAM role trusted by kinesisanalytics.amazonaws.com (only needed because the check creates a Studio notebook first; the refusal below needs no resources at all).
3. Call kinesisanalyticsv2:StopApplication with an ApplicationName that was never created (any name satisfying ^[a-zA-Z0-9_.-]+$, e.g. a freshly minted unique name).
4. Read Error.Code and Error.Message from the refusal: the code is ResourceNotFoundException with HTTP 400 as AWS answers, but the message ends in '.' where AWS's message ends at "does not exist".
5. Equivalently, replay behavioral check glue__kinesis__kinesisanalyticsv2.stop-and-force-stop-of-a-never-started-studio-notebook-rejected and observe the final assertion fail on that one character.
