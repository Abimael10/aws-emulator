Issue-Source: trace-sweep-divergence
Issue-Trace: observabilityadmin-telemetry-pipeline-workflow
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: observabilityadmin-telemetry-pipeline-workflow
Issue-Shape-Id: com.amazonaws.observabilityadmin#ObservabilityAdmin
Issue-Key: observabilityadmin|GetTelemetryPipeline|2xx->2xx|$.Pipeline.ConfigurationSummary.DataSources[*]
Issue-Commit: f4f970394571c9251d35aa06b5eed53392e5a050

# GetTelemetryPipeline still names a vended data source by the document's declared data_source_name

> _Note: this issue was identified against the code base at commit `f4f970394571c9251d35aa06b5eed53392e5a050`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTelemetryPipeline

## Description
1 cause, on observabilityadmin.

Cause 1 of 1: GetTelemetryPipeline reports Pipeline.ConfigurationSummary.DataSources[].Name for an AWS-vended (CloudWatch Logs) source from the name the submitted configuration document declares, where AWS reports the identifier the pipeline's own vended log-event metadata carries. Fixture observabilityadmin-telemetry-pipeline-workflow, step task#5.

AWS: Smithy: GetTelemetryPipeline -> output member Pipeline (TelemetryPipeline); the model snapshot does not declare ConfigurationSummary on TelemetryPipeline, so that member itself is recorded behavior only, but the model DOES document DataSource$Name (smithy_query query="doc", shape="DataSource", member="Name", model_path services/service-com-amazonaws-observabilityadmin-observabilityadmin/smithy-model/com-amazonaws-observabilityadmin-observabilityadmin.json): "The name of the data source. For CloudWatch Logs sources, this corresponds to the data_source_name from the log event metadata. For third-party sources, this is either the configured data_source_name or defaults to the plugin name if not specified." So the rule is split by source KIND: for a source reading AWS-vended CloudWatch Logs events, Name is the identifier the metadata of the events vended FOR THAT PIPELINE carries - the pipeline's resource name in identifier form, '-' replaced by '_' (TelemetryPipelineName's pattern [a-z][a-z0-9\-]+ admits no '_', so this form is service-derived and never caller-sent) - and a document-declared data_source_name governs only a third-party source. The recorded 200 for this step confirms it: DataSources[0] is {"Name": "<the requested pipeline's name in identifier form>", "Type": "default"}, not the data_source_name the configuration document declares for that source. The requirement is that derivation; the suffix token in any one recording is that run's generated pipeline name and is not part of it.

Emulator: the replayed GetTelemetryPipeline answers 200 with the same shape, but DataSources[0].Name is the document-declared data_source_name, so no element of the recorded list matches and the replay reports $.Pipeline.ConfigurationSummary.DataSources[0] both missing (recorded element) and extra (served element); Type is "default" in both and every other member of the response matches. Reading the current code, the kind split is documented but not implemented: services/service-com-amazonaws-observabilityadmin-observabilityadmin/src/state.rs data_source() (~line 930) builds Name as `match name { Some(declared) => declared.to_string(), None if vended => data_source_identifier(pipeline), None => plugin.to_string() }` - a declared name wins unconditionally, so the vended-source arm is reached only when the document declares no name at all. Only the TYPE half of DataSource$Name's split is keyed on `vended` (line ~943).

Why: a client that renders a pipeline's data sources, or correlates ConfigurationSummary with the data_source_name appearing in vended log-event metadata (dashboards, per-data-source filters, any join from the pipeline summary to ingested events), joins on the wrong key against the emulator: it sees the author-chosen document label where AWS reports the pipeline's metadata identifier, so the join finds nothing and the reported data-source identity is not the one real traffic carries. Code written against the emulator also never notices that the two names legitimately differ.

Fix: the ConfigurationSummary derivation in this service's state - the data-source naming helper that turns a parsed configuration document plus the stored pipeline record into DataSource entries. Choose the NAME by source kind exactly as DataSource$Name splits it (as the helper's own doc comment already claims it does): for a source reading AWS-vended log events the Name is the pipeline's identifier form regardless of any declared data_source_name; only for a third-party source does the declared data_source_name win, falling back to the plugin name. The same derivation feeds TelemetryPipelineSummary.ConfigurationSummary on the listing path, so the listing changes with it. No state write is involved - the summary is derived on read.

Not covered by either open observabilityadmin issue: 20261105T122721Z-observabilityadmin-telemetry-pipeline-writes-admit-unauthorized-requests-instead-of.md owns the two 401 divergences this same replay reported (DeleteTelemetryPipeline error-paths#task#3 and UpdateTelemetryPipeline workflow#task#8) and 20261110T221251Z-observabilityadmin-listdatasetintegrations-stub.md is an unimplemented-operation stub. The resolved issue 20261114T145605Z-observabilityadmin-gettelemetrypipeline-reports-a-cloudwatch-logs-data-source-by.md described this same behavior and is marked fixed, but the step still diverges: only the helper's documentation was changed, the Name selection was not, so this is filed net-new against the current checkout.

Traces: observabilityadmin-telemetry-pipeline-workflow#task#5.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, 67 steps, 44 passed, 23 mismatched, score 65.7%, 3 divergence clusters).

Cause 1 - observabilityadmin-telemetry-pipeline-workflow#task#5, GetTelemetryPipeline, verdict mismatch, expected_status 200, actual_status 200:
  $.Pipeline.ConfigurationSummary.DataSources[0]: missing (expected {'Name': 'sim2real_probe_pipe_3cf46f5c', 'Type': 'default'}, got {'Name': 'sim2real_probe_pipe_c7fdf512', 'Type': 'default'})
  $.Pipeline.ConfigurationSummary.DataSources[0]: extra   (expected {'Name': 'sim2real_probe_pipe_3cf46f5c', 'Type': 'default'}, got {'Name': 'sim2real_probe_pipe_c7fdf512', 'Type': 'default'})
  divergence_key: service=observabilityadmin|GetTelemetryPipeline|status=200->200|keys=$.Pipeline.ConfigurationSummary.DataSources[0]
  owned_by: [] (no open issue carries this replay key), stub_path: ""

Expected (AWS, recorded): DataSources[0].Name = the requested pipeline's resource name in identifier form ('-' -> '_'); Type "default", as a vended source with no declared data_source_type reports. The '...3cf46f5c' token is that run's generated pipeline name - the requirement is the derivation, not the literal value.
Actual (emulator): DataSources[0].Name = the data_source_name the submitted configuration document declares for that source ('...c7fdf512' in this run), a different token from the pipeline identifier, so the recorded list element has no match. Status, Arn, Name, Configuration.Body, Status, Sources, Processors, ProcessorCount, Sinks, timestamps and tags all matched; the single diverging value is the vended data source's Name.

Model evidence: smithy_query(query="doc", shape="DataSource", member="Name") -> "For CloudWatch Logs sources, this corresponds to the data_source_name from the log event metadata. For third-party sources, this is either the configured data_source_name or defaults to the plugin name if not specified."

Code evidence: services/service-com-amazonaws-observabilityadmin-observabilityadmin/src/state.rs:930-947 - data_source() returns `Some(declared) => declared.to_string()` ahead of the `None if vended => data_source_identifier(pipeline)` arm, so a document-declared name is used whatever the source kind; data_source_identifier() at :957 implements the '-' -> '_' identifier form and is only reachable for a source the document names not. The type half at :943-945 does key on `is_vended_log_source_type(plugin)`, so only the name half is missing the split.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay wire fixture observabilityadmin-telemetry-pipeline-workflow (trace set simparity-wire-aws-v1) through step task#5. The fixture creates a telemetry pipeline whose configuration document declares an AWS-vended CloudWatch Logs source carrying an explicit data_source_name different from the pipeline's resource name, then calls GetTelemetryPipeline on it.
3. Compare $.Pipeline.ConfigurationSummary.DataSources in the 200 response against the recording: AWS reports Name = the pipeline's resource name with '-' replaced by '_', the emulator reports Name = the document's declared data_source_name.
4. Equivalent by hand: CreateTelemetryPipeline with Name "my-pipe-one" and a configuration whose vended-log source block declares data_source_name "some-other-label" (any token differing from the pipeline name), then GetTelemetryPipeline PipelineIdentifier="my-pipe-one". AWS answers DataSources[0] = {"Name": "my_pipe_one", "Type": "default"}; the emulator answers the document's label instead. ListTelemetryPipelines' TelemetryPipelineSummary.ConfigurationSummary shows the same divergence, being derived by the same code.
