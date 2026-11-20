Issue-Source: trace-sweep-divergence
Issue-Trace: kinesisanalyticsv2-kfs-studio-parallelism-mistakes
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: kinesisanalyticsv2-kfs-studio-parallelism-mistakes
Issue-Shape-Id: com.amazonaws.kinesisanalyticsv2#KinesisAnalytics_20180523
Issue-Key: kinesisanalyticsv2|CreateApplication|4xx->4xx|$.Message
Issue-Commit: d900638f084a78febfbd89b9c5b038213abf8791

# CreateApplication's ParallelismPerKPU refusal says "supported value of" where AWS says "supported limit"

> _Note: this issue was identified against the code base at commit `d900638f084a78febfbd89b9c5b038213abf8791`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApplication

## Description
Single cause (Cause 1 of 1): the InvalidArgumentException message the emulator emits when a CreateApplication request configures a FlinkApplicationConfiguration.ParallelismConfiguration.ParallelismPerKPU above the supported per-KPU parallelism is worded differently from AWS's, so an otherwise correct 400 carries the wrong message text.

AWS: Smithy model kinesisanalyticsv2 (com.amazonaws.kinesisanalyticsv2, kinesisanalytics-20180523): ParallelismConfiguration.ParallelismPerKPU targets ParallelismPerKPU, an integer carrying only @range min 1 — the upper bound is a service rule, not a modelled constraint — and CreateApplication models InvalidArgumentException ("The specified input parameter value is not valid.", client error, HTTP 400). Recorded behavior for the over-limit value: HTTP 400, error InvalidArgumentException, Message "FlinkApplicationParallelismPerKPU provided for the application <ApplicationName> should be a positive integer and should not be larger than the supported limit <limit>". The fixed text of that message is "FlinkApplicationParallelismPerKPU provided for the application " ... " should be a positive integer and should not be larger than the supported limit "; its variable parts are the application name, taken verbatim from the request's ApplicationName, and the limit, taken from the service's supported ParallelismPerKPU ceiling (8 in the recording). AWS names the bound a "limit" and appends the number directly after "limit", with no "of".

Emulator: same HTTP 400 and same error code InvalidArgumentException — only $.Message diverges. The emulator emits "... should be a positive integer and should not be larger than the supported value of <limit>", substituting the fixed words "supported value of" for AWS's "supported limit". The application-name and limit substitutions themselves are correct; the divergence is purely in the fixed text.

Why: a client that keys on the refusal text — SDK retry/classification wrappers, log scrapers, CLI assertions, or a test suite that matches the documented refusal for an over-sized per-KPU parallelism — does not match the emulator's message, so a validation failure that AWS reports in one phrasing is reported in another and the client either mis-classifies the refusal or reports it as an unknown argument error. Anything that round-trips the message (surfacing it to an operator, or diffing recorded against emulated responses) shows a mismatch on an operation that is otherwise wire-correct.

Fix: the message builder for this refusal in the kinesisanalyticsv2 service state — the shared helper that words the ParallelismPerKPU refusal for both the create and the update path — must emit AWS's fixed text ("... should not be larger than the supported limit <limit>") while keeping the application name from the request and the limit from the service's supported ceiling. Because the helper is shared, UpdateApplication's identical refusal is corrected by the same change; the predicate that decides the refusal (value < 1 or value above the ceiling) is already right and should not change.

Traces: kinesisanalyticsv2-kfs-studio-parallelism-mistakes#task#5 (CreateApplication, expected 400 / actual 400, $.Message value_mismatch). Code: services/service-com-amazonaws-kinesisanalyticsv2-kinesisanalytics-20180523/src/state.rs lines 2536-2542 (parallelism_per_kpu_message) reached from services/service-com-amazonaws-kinesisanalyticsv2-kinesisanalytics-20180523/src/ops/create_application.rs lines 255-272.

No already-open kinesisanalyticsv2 issue covers this step: the open "StopApplication's ResourceNotFoundException message ... trailing period" and "CreateApplication refuses the deprecated ZEPPELIN-FLINK-1_0 runtime ..." issues concern different messages and different refusal rules.

## Evidence
Fixture kinesisanalyticsv2-kfs-studio-parallelism-mistakes, step task#5, operation CreateApplication.

Expected (recorded AWS): HTTP 400, InvalidArgumentException, $.Message = "FlinkApplicationParallelismPerKPU provided for the application sim2real-probe-kfs-par-bad-app-bb23aaa2 should be a positive integer and should not be larger than the supported limit 8"

Actual (emulator): HTTP 400, InvalidArgumentException, $.Message = "FlinkApplicationParallelismPerKPU provided for the application sim2real-probe-kfs-par-bad-app-bb23aaa2 should be a positive integer and should not be larger than the supported value of 8"

Replay verdict: mismatch, divergence_key service=kinesisanalyticsv2|CreateApplication|status=400->400|keys=$.Message. Status and error code agree; only the fixed text of the message differs ("supported limit <n>" vs "supported value of <n>"). The application name shown is that run's generated name and the 8 is that environment's ceiling — neither is part of the requirement; the fixed wording is.

## Steps to Reproduce
1. Boot the emulator and replay fixture kinesisanalyticsv2-kfs-studio-parallelism-mistakes, step task#5.
2. Equivalently, call kinesisanalyticsv2 CreateApplication with a valid ApplicationName, a Flink RuntimeEnvironment, a valid ServiceExecutionRole, and ApplicationConfiguration.FlinkApplicationConfiguration.ParallelismConfiguration = { ConfigurationType: CUSTOM, ParallelismPerKPU: <a value above the supported ceiling, e.g. 9> }.
3. Observe HTTP 400 with InvalidArgumentException.
4. Compare $.Message against AWS: it ends "should not be larger than the supported value of 8" instead of AWS's "should not be larger than the supported limit 8".
