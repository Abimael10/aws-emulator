Issue-Source: trace-sweep-divergence
Issue-Trace: sagemaker-train-host-and-invoke-xgboost-model
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sagemaker-train-host-and-invoke-xgboost-model sagemaker-training-job-hyperparameter-numeric-precision-preserved
Issue-Shape-Id: com.amazonaws.sagemaker#SageMaker
Issue-Key: sagemaker|DescribeTrainingJob|2xx->2xx|$.AlgorithmSpecification.MetricDefinitions
Issue-Key: sagemaker|DescribeTrainingJob|2xx->2xx|$.ImageMetadata
Issue-Key: sagemaker|DescribeTrainingJob|2xx->2xx|$.InputDataConfig
Issue-Related: 20261109T123745Z-sagemaker-describetrainingjob-and-describeendpoint-omit-response-members-the.md
Issue-Commit: 9eefc33eaece22bd4a7a53f7a36483a2f6e00b07

# sagemaker DescribeTrainingJob projection drops the algorithm metric definitions and the echoed job detail members

> _Note: this issue was identified against the code base at commit `9eefc33eaece22bd4a7a53f7a36483a2f6e00b07`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTrainingJob

## Description
2 causes. DescribeTrainingJob answers 200 for a training job that exists and whose scalar members match, but its response projection omits members AWS always populates for that job: the algorithm's MetricDefinitions set, the echoed input channel list, and the per-job image/output detail members. Both causes are the SAME projection/state gap seen on two different jobs (a built-in xgboost job and a hyperparameter-precision job), which is why they are bundled; a fixer must close BOTH clusters.

Cause 1 of 2: DescribeTrainingJob for a job launched from a built-in algorithm image omits AlgorithmSpecification.MetricDefinitions, ImageMetadata and TrainingJobOutput.
AWS: Smithy (services/service-com-amazonaws-sagemaker-sagemaker): DescribeTrainingJobResponse member AlgorithmSpecification -> AlgorithmSpecification, whose member MetricDefinitions -> MetricDefinitionList (@length 0..40) is documented as "A list of metric definition objects. Each object specifies the metric name and regular expressions used to parse algorithm logs. SageMaker publishes each metric to Amazon CloudWatch"; each MetricDefinition is a structure with required Name (MetricName, @length 1..255) and required Regex (MetricRegex, @length 1..500). For a job created from a SageMaker built-in/first-party algorithm image, AWS resolves that algorithm's OWN fixed metric set onto the described job even when CreateTrainingJob supplied no MetricDefinitions, so the response carries one {Name, Regex} entry per metric the algorithm publishes (for the xgboost image: train:/validation: pairs such as train:mae, train:merror, train:mlogloss, validation:logloss, validation:auc, validation:precision, validation:recall and the rest, each with that algorithm's log-scraping regex template of the form '.*\\[[0-9]+\\].*#011<metric-name>:([-+]?[0-9]*\\.?[0-9]+(?:[eE][-+]?[0-9]+)?).*'). The requirement is the metric NAMES and their regex templates, which follow from the algorithm image, not from this run; the count of entries likewise follows from the algorithm. ImageMetadata and TrainingJobOutput are not declared in this checkout's on-disk DescribeTrainingJobResponse, so for those two it is recorded behavior only: the recorded 200 response carries both present on every DescribeTrainingJob of a completed job.
Emulator: returns 200 with $.AlgorithmSpecification.MetricDefinitions absent (null), and $.ImageMetadata and $.TrainingJobOutput absent. The status is 200 either way, so nothing signals the omission.
Why: a client that reads AlgorithmSpecification.MetricDefinitions to learn which metric names a job emits and which regexes parse them - the standard SageMaker analytics/tuning path, and the only place the built-in algorithm's metric set is exposed - gets nothing back and concludes the job publishes no metrics, so metric collection and any tuning objective keyed on one of those names silently does nothing.
Fix: the DescribeTrainingJob handler's response projection and the training-job state it reads from - the algorithm image's metric set must be resolved onto the stored job at CreateTrainingJob time and echoed back, and the image/output detail members must be stored and echoed.
Traces: sagemaker-train-host-and-invoke-xgboost-model#task#6.

Cause 2 of 2: DescribeTrainingJob omits the echoed InputDataConfig channel list (plus MetricDefinitions and ImageMetadata) for a job whose create request supplied channels.
AWS: Smithy: DescribeTrainingJobResponse member InputDataConfig -> InputDataConfig, a list of Channel with @length min 1 max 20; Channel models required ChannelName (@length 1..64, @pattern ^[A-Za-z0-9\\.\\-_]+$), required DataSource, ContentType, CompressionType, RecordWrapperType and InputMode. InputDataConfig is the authoritative echo of what CreateTrainingJob was given, so AWS returns the submitted channel list on every describe of that job; MetricDefinitions is resolved as in Cause 1. ImageMetadata is not in the on-disk model - recorded behavior only, present on the recorded 200 response.
Emulator: returns 200 with $.InputDataConfig absent (null) together with $.AlgorithmSpecification.MetricDefinitions and $.ImageMetadata.
Why: re-reading a job to recover its channels is the normal clone/retry path; with InputDataConfig absent a client cannot reconstruct the job's inputs and either recreates it with no channels or raises on a successful describe.
Fix: same component - the DescribeTrainingJob output projection plus the stored training-job record, which must retain the create request's InputDataConfig channel list verbatim and echo it.
Traces: sagemaker-training-job-hyperparameter-numeric-precision-preserved#task#4.

Not requirements, deliberately excluded from this issue: both clusters also list $.CreationTime, $.LastModifiedTime, $.TrainingStartTime, $.TrainingEndTime and $.FinalMetricDataList[0].Timestamp against '<shaped-not-asserted>'. Those are per-run clock values and are not part of either cause.

Related but NOT covering these clusters: open issue 20261109T123745Z-sagemaker-describetrainingjob-and-describeendpoint-omit-response-members-the.md and open issue 20261102T105242Z-sagemaker-describe-responses-omit-members-present-in-the.md both discuss DescribeTrainingJob omissions, but the replay reports both clusters here with NO owner (their replay keys do not carry $.AlgorithmSpecification.MetricDefinitions / $.ImageMetadata / $.TrainingJobOutput), so they are filed with their own replay keys.

## Evidence
Cause 1 - fixture sagemaker-train-host-and-invoke-xgboost-model, step task#6, DescribeTrainingJob, status 200 expected -> 200 observed. Replay detail: "$.AlgorithmSpecification.MetricDefinitions: missing (expected [{'Name': 'train:mae', 'Regex': '.*\\[[0-9]+\\].*#011train-mae:([-+]?[0-9]*\\.?[0-9]+(?:[eE][-+]?[0-9]+)?).*'}, {'Name': 'train:merror', ...}, {'Name': 'validation:logloss', ...}, {'Name': 'validation:auc', ...}, ...], got None)". divergence_key: service=sagemaker|DescribeTrainingJob|status=200->200|keys=$.AlgorithmSpecification.MetricDefinitions,$.CreationTime,$.FinalMetricDataList[0].Timestamp,$.ImageMetadata,$.LastModifiedTime,$.TrainingEndTime,$.TrainingJobOutput,$.TrainingStartTime. The metric names and their regex templates are the requirement; the timestamp keys in that same key set are per-run values and are not.

Cause 2 - fixture sagemaker-training-job-hyperparameter-numeric-precision-preserved, step task#4, DescribeTrainingJob, status 200 expected -> 200 observed. Replay detail: "$.AlgorithmSpecification.MetricDefinitions: missing (expected [{'Name': 'validation:binary_log_loss', 'Regex': '#quality_metric: host=\\S+, validation binary_logloss <None>=(\\S+)'}, {'Name': 'test:dcg', 'Regex': '#quality_metric: host=\\S+, test dcg <score>=(\\S+)'}, {'Name': 'train:progress', 'Regex': '#progress_metric: host=\\S+, completed (\\S+) %'}, {'Name': 'validation:mae', ...}, ...], got None)". divergence_key: service=sagemaker|DescribeTrainingJob|status=200->200|keys=$.AlgorithmSpecification.MetricDefinitions,$.ImageMetadata,$.InputDataConfig. Here the whole $.InputDataConfig channel list is absent, not merely members inside it.

Smithy consulted: DescribeTrainingJob (input TrainingJobName required; output members AlgorithmSpecification, InputDataConfig @length 1..20, OutputDataConfig, ResourceConfig, FinalMetricDataList @length 0..40; error ResourceNotFound httpError 400); shape AlgorithmSpecification (member MetricDefinitions -> MetricDefinitionList @length 0..40); shape MetricDefinition (required Name @length 1..255, required Regex @length 1..500); shape Channel (required ChannelName, required DataSource, ContentType, CompressionType, RecordWrapperType, InputMode). ImageMetadata and TrainingJobOutput are absent from the on-disk model (recorded behavior only).

## Steps to Reproduce
1. Replay the wire corpus against the emulator.
2. Cause 1: replay fixture sagemaker-train-host-and-invoke-xgboost-model and stop at step task#6. It creates a training job from the built-in xgboost image and then calls DescribeTrainingJob. Read $.AlgorithmSpecification.MetricDefinitions - absent, where AWS returns the xgboost algorithm's full {Name, Regex} metric set. Read $.ImageMetadata and $.TrainingJobOutput - also absent.
3. Cause 2: replay fixture sagemaker-training-job-hyperparameter-numeric-precision-preserved and stop at step task#4. It calls CreateTrainingJob with an InputDataConfig channel list, then DescribeTrainingJob. Read $.InputDataConfig - absent, where AWS echoes the submitted channel list; read $.AlgorithmSpecification.MetricDefinitions and $.ImageMetadata - also absent.
4. In both cases the response status is 200 and every other asserted member matches, so only the missing members diverge. Ignore $.CreationTime / $.LastModifiedTime / $.TrainingStartTime / $.TrainingEndTime / $.FinalMetricDataList[0].Timestamp: those are per-run clock values.
