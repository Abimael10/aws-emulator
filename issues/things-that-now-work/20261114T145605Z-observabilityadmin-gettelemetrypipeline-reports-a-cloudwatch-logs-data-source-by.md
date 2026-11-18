Issue-Source: trace-sweep-divergence
Issue-Trace: observabilityadmin-telemetry-pipeline-workflow
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: observabilityadmin-telemetry-pipeline-workflow
Issue-Shape-Id: com.amazonaws.observabilityadmin#ObservabilityAdmin
Issue-Key: observabilityadmin|GetTelemetryPipeline|2xx->2xx|$.Pipeline.ConfigurationSummary.DataSources[*]
Issue-Commit: c17029b5143eaf540f8fe3ae3fd6caada5435149

# GetTelemetryPipeline reports a CloudWatch Logs data source by its configured data_source_name instead of the pipeline's log-event-metadata identifier

> _Note: this issue was identified against the code base at commit `c17029b5143eaf540f8fe3ae3fd6caada5435149`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTelemetryPipeline

## Description
Cause 1 of 1: Pipeline.ConfigurationSummary.DataSources[].Name is taken from the configuration document for a vended (CloudWatch Logs) source, where AWS takes it from the log event metadata.

AWS: Smithy: GetTelemetryPipeline -> output Pipeline (TelemetryPipeline); the model snapshot does not declare ConfigurationSummary on TelemetryPipeline, so the member itself is recorded behavior only, but the model DOES document DataSource$Name: "The name of the data source. For CloudWatch Logs sources, this corresponds to the data_source_name from the log event metadata. For third-party sources, this is either the configured data_source_name or defaults to the plugin name if not specified." So the rule is split by source kind: a CloudWatch Logs (AWS-vended) source reports the name the metadata of the events vended FOR THAT PIPELINE carries - i.e. the pipeline's own identifier, the pipeline resource name in identifier form, with '-' replaced by '_' (TelemetryPipelineName's pattern [a-z][a-z0-9\-]+ admits no '_', so the identifier form is service-derived and never caller-sent) - and the configured data_source_name governs only a third-party source. The recorded 200 response for this step confirms it: its DataSources[0] is {"Name": "<pipeline-name-in-identifier-form>", "Type": "default"}, the pipeline identifier, NOT the data_source_name the submitted configuration document declares for that source.

Emulator: the replayed GetTelemetryPipeline answers 200 with the same shape but a different DataSources[0].Name - the token the configuration document declared - so the recorded element is reported missing and the emulator's element extra at $.Pipeline.ConfigurationSummary.DataSources[0] (recorded Name suffix ...3cf46f5c vs served ...c7fdf512; only the suffix token differs, Type is "default" in both). Hypothesis: the derivation gives the document-declared name unconditional precedence - a declared name is used verbatim whatever the source kind, and the pipeline-identifier form is used only when the document declares no name at all - so the vended-source branch of DataSource$Name is never reached when the document happens to declare a data_source_name.

Why: a client that renders a pipeline's data sources, or correlates a telemetry pipeline's summary with the data source names that appear in vended log event metadata (dashboards, per-data-source filters, anything joining ConfigurationSummary to ingested events), joins on the wrong key against the emulator: it sees the author-chosen document label where AWS reports the pipeline's metadata identifier, so the join silently finds nothing and the reported data source identity is not the one real traffic carries. Code written against the emulator also cannot be relied on to notice when the two names legitimately differ.

Fix: the ConfigurationSummary derivation in this service's state - the data-source naming helper that turns a parsed configuration document plus the stored pipeline record into DataSource entries. Choose the name by source kind, as DataSource$Name splits it: for a source that reads AWS-vended log events, the Name is the pipeline's own identifier (pipeline name with '-' -> '_') regardless of any data_source_name the document declares; only for a third-party source does the configured data_source_name win, falling back to the plugin name. The same derivation feeds TelemetryPipelineSummary.ConfigurationSummary in the listing path, so the listing must change with it, and no state write is involved (the read is derived).

Traces: observabilityadmin-telemetry-pipeline-workflow#task#5 (GetTelemetryPipeline, expected 200 / got 200, $.Pipeline.ConfigurationSummary.DataSources[0] missing+extra). Note: the other two divergences this replay reported in this service (DeleteTelemetryPipeline task#3 of observabilityadmin-telemetry-pipeline-error-paths and UpdateTelemetryPipeline task#8 of this fixture, both 401 Unauthorized) are already owned by the open issue 20261105T122721Z-observabilityadmin-telemetry-pipeline-writes-admit-unauthorized-requests-instead-of.md and are NOT part of this issue.

## Evidence
Replay step observabilityadmin-telemetry-pipeline-workflow#task#5, operation GetTelemetryPipeline, verdict mismatch, expected_status 200, actual_status 200.

Divergence detail as replayed:
  $.Pipeline.ConfigurationSummary.DataSources[0]: missing (expected {'Name': 'sim2real_probe_pipe_3cf46f5c', 'Type': 'default'}, got {'Name': 'sim2real_probe_pipe_c7fdf512', 'Type': 'default'})
  $.Pipeline.ConfigurationSummary.DataSources[0]: extra  (expected {'Name': 'sim2real_probe_pipe_3cf46f5c', 'Type': 'default'}, got {'Name': 'sim2real_probe_pipe_c7fdf512', 'Type': 'default'})

Expected (AWS, recorded): DataSources[0].Name is the requested pipeline's own identifier - the pipeline resource name with '-' replaced by '_' (the '...3cf46f5c' token above is that run's pipeline name; the requirement is the derivation, not the literal value). Type "default", as a vended source with no declared data_source_type reports.
Actual (emulator): DataSources[0].Name is the data_source_name the submitted configuration document declares for that source ('...c7fdf512' in this run) - a different token from the pipeline identifier, so no element of the recorded list matches.
Everything else in the response (status 200, Arn, Name, Configuration.Body, Status, Sources, Processors, ProcessorCount, Sinks, timestamps, tags) matched; the single diverging value is the vended data source's Name.

Model evidence cited: smithy_query(query="doc", shape="DataSource", member="Name") - "For CloudWatch Logs sources, this corresponds to the data_source_name from the log event metadata. For third-party sources, this is either the configured data_source_name or defaults to the plugin name if not specified."; smithy_query(query="shape", shape="TelemetryPipelineName") - pattern [a-z][a-z0-9\-]+, length 3..28 (no '_', so identifier form is service-derived).
Emulator evidence: services/service-com-amazonaws-observabilityadmin-observabilityadmin/src/state.rs - configuration_summary() (~line 724) -> data_sources_declared() (~line 840) -> data_source() (~line 923), whose Name arm prefers any declared name unconditionally (`Some(declared) => declared.to_string()`) and only falls back to data_source_identifier(pipeline) (~line 950) when the document declares none; GetTelemetryPipeline's handler returns record.as_pipeline(now) (src/ops/get_telemetry_pipeline.rs ~line 192), which carries that summary.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay wire fixture observabilityadmin-telemetry-pipeline-workflow, step task#5 (trace set simparity-wire-aws-v1). The fixture creates a telemetry pipeline whose configuration document declares a CloudWatch Logs / AWS-vended source carrying an explicit data_source_name that differs from the pipeline's resource name, then calls GetTelemetryPipeline on it.
3. Compare $.Pipeline.ConfigurationSummary.DataSources in the 200 response against the recording: AWS reports Name = the pipeline's resource name in identifier form ('-' -> '_'), the emulator reports Name = the document's declared data_source_name.
4. Equivalent by hand: CreateTelemetryPipeline with Name "my-pipe-one" and a configuration whose vended-log source block declares data_source_name: "some_other_label", then GetTelemetryPipeline PipelineIdentifier="my-pipe-one". AWS answers DataSources[0] = {"Name": "my_pipe_one", "Type": "default"}; the emulator answers {"Name": "some_other_label", "Type": "default"}. The same divergence appears in ListTelemetryPipelines' TelemetryPipelineSummary.ConfigurationSummary, which is derived by the same code.
