Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sagemaker.sagemaker-endpoint-prediction-is-deterministic-and-ordered-in-its-input
Issue-Key: sagemaker.sagemaker-endpoint-prediction-is-deterministic-and-ordered-in-its-input|run#54|KeyError|assert variant["CurrentInstanceCount"] == 0, (
Issue-Commit: 59b962a2c04ab39e04c4f062d745d1b4088bc940

# sagemaker DescribeEndpoint omits ProductionVariants[].CurrentInstanceCount, so a serverless variant's instance count reads as absent instead of 0

> _Note: this issue was identified against the code base at commit `59b962a2c04ab39e04c4f062d745d1b4088bc940`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEndpoint

## Description
1 cause: the DescribeEndpoint response projection drops a ProductionVariantSummary member that AWS always populates for a deployed endpoint, so a client reading it sees absent where AWS reports a number.

Cause 1 of 1: DescribeEndpoint's ProductionVariants entries carry no CurrentInstanceCount.

AWS: Smithy: DescribeEndpoint output member ProductionVariants -> ProductionVariantSummaryList of ProductionVariantSummary, which models CurrentInstanceCount -> TaskCount (integer, @range min 0), documented as "The number of instances associated with the variant", alongside DesiredInstanceCount, CurrentWeight, DesiredWeight, DeployedImages and CurrentServerlessConfig. AWS populates CurrentInstanceCount on every variant summary of an endpoint that has been created; for a variant configured with a ProductionVariantServerlessConfig (no instances are provisioned for a serverless variant) the reported value is 0, which is the general rule the check asserts — the number is derived from the variant's provisioned capacity, not from this run. The response status is 200 in both AWS and the emulator; nothing in the status or an error code signals the omission.

Emulator: returns 200 with each ProductionVariants entry lacking the CurrentInstanceCount key entirely (botocore therefore raises KeyError: 'CurrentInstanceCount' when the member is read). The sibling members on the same entry are served: CurrentWeight and DesiredWeight are both present and equal 1.0 on the same response, so the projection populates part of ProductionVariantSummary and silently skips the instance-count member.

Why: a client cannot distinguish "this variant has zero instances" from "this field is not reported". Deployment-verification and autoscaling logic keyed on CurrentInstanceCount — the standard way to decide whether a variant has capacity attributed to it, and the only place a serverless variant's zero-instance state is visible — either crashes on the absent key (as the replayed check does) or reads a default and concludes capacity is unknown. Any client that round-trips a describe to reconstruct an endpoint's current capacity loses the member.

Fix: the DescribeEndpoint handler's ProductionVariantSummary response projection, and the endpoint state it reads from — the variant's current instance count must be recorded when the endpoint is created from its endpoint config (0 for a variant that declares a ServerlessConfig) and echoed on every variant summary.

Traces: behavioral check sagemaker.sagemaker-endpoint-prediction-is-deterministic-and-ordered-in-its-input. The check's later assertions on the same variant entry (CurrentServerlessConfig, DeployedImages.SpecifiedImage / ResolvedImage / ResolutionTime) are never reached because the KeyError aborts first, so this issue claims only the CurrentInstanceCount omission observed here.

Related but NOT covering: 20261109T123745Z-sagemaker-describetrainingjob-and-describeendpoint-omit-response-members-the.md describes the same DescribeEndpoint omission from the WIRE corpus (fixture sagemaker-train-host-and-invoke-xgboost-model#task#11) and carries only that fixture's replay keys; the behavioral replay reports this check as unowned, so it is filed here with its own behavioral replay key. 20261102T105242Z-sagemaker-describe-responses-omit-members-present-in-the.md is likewise adjacent (other describe operations) and does not cover this check.

## Evidence
Failing check sagemaker.sagemaker-endpoint-prediction-is-deterministic-and-ordered-in-its-input, verdict [error]:

  File "check_sagemaker_sagemaker_endpoint_prediction_is_deterministic_and_ordered_in_its_input.py", line 357, in run
    assert variant["CurrentInstanceCount"] == 0, (
  KeyError: 'CurrentInstanceCount'

Expected (AWS): DescribeEndpoint 200; ProductionVariants[0] contains CurrentInstanceCount with value 0 for the serverless variant (the member is always present; the value follows from the variant's provisioned capacity).
Actual (emulator): DescribeEndpoint 200; ProductionVariants[0] has no CurrentInstanceCount key at all. The preceding assertion on the same dict — variant["CurrentWeight"] == 1.0 and variant["DesiredWeight"] == 1.0 — passed, proving the variant summary is otherwise populated and only the instance-count member is dropped.

Smithy (services/service-com-amazonaws-sagemaker-sagemaker/smithy-model/com-amazonaws-sagemaker-sagemaker.json): com.amazonaws.sagemaker#ProductionVariantSummary.CurrentInstanceCount -> TaskCount, integer, @range{min:0}, doc "The number of instances associated with the variant."

No status code or error code diverges here — the divergence is an absent response member on a 200.

## Steps to Reproduce
1. Run the behavioral suite against the emulator and select check sagemaker.sagemaker-endpoint-prediction-is-deterministic-and-ordered-in-its-input.
2. Train a model, create a Model and an EndpointConfig whose single ProductionVariant declares a ServerlessConfig (MemorySizeInMB 2048, MaxConcurrency 1), then CreateEndpoint and wait for EndpointStatus InService.
3. Call DescribeEndpoint on that endpoint and read ProductionVariants[0].
4. Observe CurrentWeight and DesiredWeight are present (1.0), but the key CurrentInstanceCount is absent, where AWS reports CurrentInstanceCount = 0 for a serverless variant. Reading it raises KeyError: 'CurrentInstanceCount'.
