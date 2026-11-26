Issue-Source: trace-sweep-divergence
Issue-Trace: glue-crawler-update-description-and-unknown-crawler
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: glue-crawler-update-description-and-unknown-crawler glue-data-catalog-export-configuration-validation-errors glue-put-data-catalog-encryption-settings-password-round-trip glue-job-etl-unset-version-defaults-tags-provider-update-max-retries-and-not-found glue-get-dataflow-graph glue-get-plan-script-generation glue-get-unfiltered-partitions-metadata-not-found glue-unfiltered-partition-metadata-unknown-table
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.LakeFormationConfiguration
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.LineageConfiguration
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.RecrawlPolicy
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.Role
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.SchemaChangePolicy
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.TableProfilingConfiguration
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.Targets.CatalogTargets
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.Targets.DeltaTargets
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.Targets.DynamoDBTargets
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.Targets.HudiTargets
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.Targets.IcebergTargets
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.Targets.JdbcTargets
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.Targets.MongoDBTargets
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.Targets.RestCatalogTargets
Issue-Key: glue|GetCrawler|2xx->2xx|$.Crawler.Targets.S3Targets[*].Exclusions
Issue-Key: glue|GetDataCatalogExportConfiguration|2xx->2xx|$.DataQualityExportSetting
Issue-Key: glue|GetDataCatalogExportConfiguration|2xx->2xx|$.DataQualityExportStatus
Issue-Key: glue|GetDataCatalogExportConfiguration|2xx->2xx|$.LineageExportSetting
Issue-Key: glue|GetDataCatalogExportConfiguration|2xx->2xx|$.LineageExportStatus
Issue-Key: glue|GetDataCatalogEncryptionSettings|2xx->2xx|$.DataCatalogEncryptionSettings.ConnectionPasswordEncryption.AwsKmsKeyId
Issue-Key: glue|GetJob|2xx->2xx|$.Job.CreatedOn
Issue-Key: glue|GetJob|2xx->2xx|$.Job.LastModifiedOn
Issue-Key: glue|GetDataflowGraph|2xx->2xx|$.DagNodes[Id="datasink0"].LineNumber
Issue-Key: glue|GetDataflowGraph|2xx->2xx|$.DagNodes[Id="datasource0"].LineNumber
Issue-Key: glue|GetPlan|2xx->2xx|$.PythonScript
Issue-Key: glue|GetUnfilteredPartitionsMetadata|4xx->4xx|$.Message
Issue-Key: glue|GetUnfilteredPartitionMetadata|4xx->4xx|$.Message
Issue-Related: 20261102T132146Z-glue-job-and-job-run-reads-omit-modeled.md
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# Glue read paths omit or misreport members AWS returns

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCrawler

## Description
Eight Glue responses are structurally wrong: a read drops the default sub-structures AWS always materialises, two reads drop members AWS returns, two generated artefacts differ from AWS's codegen, a stored value is not round-tripped, and two error bodies carry an empty-string Message where AWS serialises none.

Cause 1 of 8: GetCrawler omits every defaulted sub-structure and echoes Role as an ARN.
AWS: Smithy: GetCrawler output Crawler; the Crawler shape declares Role, Targets (CrawlerTargets with CatalogTargets/DeltaTargets/DynamoDBTargets/HudiTargets/IcebergTargets/JdbcTargets/MongoDBTargets/RestCatalogTargets/S3Targets), RecrawlPolicy, SchemaChangePolicy, LineageConfiguration, LakeFormationConfiguration. The recording shows AWS materialising every one of them with its service default even when the create request omitted it: LakeFormationConfiguration {AccountId:'', UseLakeFormationCredentials:false}, LineageConfiguration {CrawlerLineageSettings:'DISABLE'}, RecrawlPolicy {RecrawlBehavior:'CRAWL_EVERYTHING'}, SchemaChangePolicy {DeleteBehavior:'DEPRECATE_IN_DATABASE', UpdateBehavior:'UPDATE_IN_DATABASE'}, each unused target list as [], S3Targets[].Exclusions as [], plus TableProfilingConfiguration {CollectTableStatistics:false} (recorded behavior only — not in the checked-in model). It also echoes Crawler.Role as the role NAME the crawler was created with, not an ARN.
Emulator: all of those members are absent from the Crawler object, and Role is returned as 'arn:aws:iam::<account>:role/<name>'.
Why: clients and IaC diff engines read these defaults; absent members read as 'unset' and produce a spurious drift/update every reconcile, and Role must round-trip in the form it was supplied.
Fix: the GetCrawler serializer / the crawler state defaults.
Traces: glue-crawler-update-description-and-unknown-crawler#task#6. (The Role half overlaps open issue 20261111T102615Z-cloudformation-glue-logs-s3-2-net-new-causes.md, which does not carry this replay key.)

Cause 2 of 8: GetDataCatalogExportConfiguration omits the data-quality and lineage export members.
AWS: recorded behavior only — the checked-in model's GetDataCatalogExportConfigurationOutput declares ExportSetting/Status/EncryptionConfiguration/S3TableBucketArn/CreatedAt/UpdatedAt, but the live recording additionally returns DataQualityExportSetting, DataQualityExportStatus, LineageExportSetting and LineageExportStatus on a 200 response.
Emulator: 200 with all four absent.
Why: a client reading the catalog's export configuration cannot see the data-quality/lineage export state at all.
Fix: the GetDataCatalogExportConfiguration output serializer and the export-configuration state.
Traces: glue-data-catalog-export-configuration-validation-errors#task#3.

Cause 3 of 8: PutDataCatalogEncryptionSettings does not round-trip ConnectionPasswordEncryption.AwsKmsKeyId.
AWS: Smithy: ConnectionPasswordEncryption has ReturnConnectionPasswordEncrypted (@required boolean) and AwsKmsKeyId (NameString) — 'An KMS key that is used to encrypt the connection password'. The recording shows GetDataCatalogEncryptionSettings echoing the key id that was stored by the preceding Put (in the recording the AWS-managed alias).
Emulator: 200, but $.DataCatalogEncryptionSettings.ConnectionPasswordEncryption.AwsKmsKeyId is absent — the member is accepted on write and dropped on read.
Why: the configured password-encryption key is invisible, so a client cannot verify or re-apply it.
Fix: the data-catalog encryption-settings state (persist AwsKmsKeyId) and the GetDataCatalogEncryptionSettings serializer.
Traces: glue-put-data-catalog-encryption-settings-password-round-trip#task#3.

Cause 4 of 8: GetJob reports Command.PythonVersion '3' for a job created without one.
AWS: Smithy: JobCommand.PythonVersion targets PythonVersionString, @pattern ^([2-3]|3[.]9)$, doc 'Allowed values are 2 or 3'; no @default in the model. Recorded behavior: when CreateJob omits Command.PythonVersion, GetJob echoes '2'.
Emulator: echoes '3'.
Why: a client that reads the job back and re-submits it (or asserts on the effective runtime) sees a different Python runtime than AWS reports.
Fix: the CreateJob/GetJob job-command default in state.
Traces: glue-job-etl-unset-version-defaults-tags-provider-update-max-retries-and-not-found#task#3.

Cause 5 of 8: GetDataflowGraph reports the wrong CodeGenNode.LineNumber.
AWS: Smithy: CodeGenNode.LineNumber (integer, default 0) — 'The line number of the node.' Recorded behavior: for the submitted annotated script AWS reports the line of each node's annotation block as 15 for the datasource node and 20 for the datasink node.
Emulator: reports 19 and 24 — every node's line number is shifted by a constant 4, i.e. the preamble lines are counted differently from AWS.
Why: the line number is what a console/IDE uses to locate a node in the script; a shifted value points at the wrong statement.
Fix: the GetDataflowGraph script parser's line accounting.
Traces: glue-get-dataflow-graph#task#1.

Cause 6 of 8: GetPlan's generated PythonScript does not match AWS's codegen output.
AWS: Smithy: GetPlan output PythonScript. Recorded behavior: AWS emits a fixed template — the awsglue/pyspark import preamble, '## @params: [JOB_NAME]' + getResolvedOptions, the SparkContext/GlueContext/Job init block, then one annotated block per node ('## @type:', '## @args:', '## @return:', '## @inputs:' followed by the statement) for DataSource, ApplyMapping and DataSink, and a closing job.commit(). The variable parts are the database/table names, the mapping tuples and the sink connection options taken from the request.
Emulator: returns a script that diverges from that template (the replay shows the two scripts differing from the preamble onward).
Why: the returned script is the artefact the caller runs; a divergent one is not the job AWS would have generated.
Fix: the GetPlan script generator.
Traces: glue-get-plan-script-generation#task#3 (the fixture carries the full expected script).

Cause 7 of 8: GetUnfilteredPartitionsMetadata's refusal body carries an empty Message.
AWS: Smithy: GetUnfilteredPartitionsMetadata errors include EntityNotFoundException (400). Recorded behavior: for this refusal AWS's body carries NO Message member — the member is omitted, not empty.
Emulator: 400 with Message serialised as the empty string ''.
Why: a client that tests for the presence of a message, or prints it, sees an empty reason where AWS sends none; 'present and empty' and 'absent' are different on the wire.
Fix: the error serializer for this operation (omit Message when there is none, rather than defaulting it to '').
Traces: glue-get-unfiltered-partitions-metadata-not-found#task#1.

Cause 8 of 8: GetUnfilteredPartitionMetadata (singular) has the same empty-Message defect.
AWS: same error class, same recorded shape — no Message member in the 400 body.
Emulator: 400 with Message ''.
Fix: the same error serializer path as cause 7, on the singular operation.
Traces: glue-unfiltered-partition-metadata-unknown-table#task#1.

## Evidence
1. glue-crawler-update-description-and-unknown-crawler#task#6 GetCrawler (200) — missing $.Crawler.LakeFormationConfiguration {'AccountId':'','UseLakeFormationCredentials':False}, .LineageConfiguration {'CrawlerLineageSettings':'DISABLE'}, .RecrawlPolicy {'RecrawlBehavior':'CRAWL_EVERYTHING'}, .SchemaChangePolicy {'DeleteBehavior':'DEPRECATE_IN_DATABASE','UpdateBehavior':'UPDATE_IN_DATABASE'}, .TableProfilingConfiguration {'CollectTableStatistics':False}, .Targets.{CatalogTargets,DeltaTargets,DynamoDBTargets,HudiTargets,IcebergTargets,JdbcTargets,MongoDBTargets,RestCatalogTargets} = [], .Targets.S3Targets[0].Exclusions = []; $.Crawler.Role expected the role name, got 'arn:aws:iam::000000000000:role/<name>'.
2. glue-data-catalog-export-configuration-validation-errors#task#3 GetDataCatalogExportConfiguration (200) — $.DataQualityExportSetting, $.DataQualityExportStatus, $.LineageExportSetting, $.LineageExportStatus all missing.
3. glue-put-data-catalog-encryption-settings-password-round-trip#task#3 GetDataCatalogEncryptionSettings (200) — $.DataCatalogEncryptionSettings.ConnectionPasswordEncryption.AwsKmsKeyId expected 'alias/aws/glue' (the value stored by the preceding Put), got absent.
4. glue-job-etl-unset-version-defaults-tags-provider-update-max-retries-and-not-found#task#3 GetJob (200) — $.Job.Command.PythonVersion expected '2', got '3'.
5. glue-get-dataflow-graph#task#1 GetDataflowGraph (200) — $.DagNodes[Id="datasource0"].LineNumber expected 15, got 19; $.DagNodes[Id="datasink0"].LineNumber expected 20, got 24.
6. glue-get-plan-script-generation#task#3 GetPlan (200) — $.PythonScript differs from the recorded AWS-generated script.
7. glue-get-unfiltered-partitions-metadata-not-found#task#1 GetUnfilteredPartitionsMetadata (400) — $.Message unexpected_present (expected absent, got '').
8. glue-unfiltered-partition-metadata-unknown-table#task#1 GetUnfilteredPartitionMetadata (400) — $.Message unexpected_present (expected absent, got '').

## Steps to Reproduce
1. Replay the wire corpus (replay_wire_corpus).
2. Create a crawler with a single S3 target and no policies, then GetCrawler — the defaulted sub-structures are absent and Role comes back as an ARN.
3. PutDataCatalogExportConfiguration, then GetDataCatalogExportConfiguration — the four DataQuality*/Lineage* members are absent.
4. PutDataCatalogEncryptionSettings with ConnectionPasswordEncryption.AwsKmsKeyId set, then GetDataCatalogEncryptionSettings — the key id is gone.
5. CreateJob with Command but no PythonVersion, then GetJob — PythonVersion is '3'.
6. GetDataflowGraph on the fixture's annotated script — node LineNumbers are 4 higher than recorded.
7. GetPlan with the fixture's Mapping/Source/Sinks — compare PythonScript with the recording.
8. GetUnfilteredPartitionsMetadata and GetUnfilteredPartitionMetadata for an unknown table — the 400 body contains Message: ''.
