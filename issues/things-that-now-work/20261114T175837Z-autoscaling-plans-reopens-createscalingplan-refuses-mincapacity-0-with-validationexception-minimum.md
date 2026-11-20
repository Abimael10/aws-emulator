Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling-plans.asp-predictive-forecast-without-history-has-no-datapoints
Issue-Shape-Id: com.amazonaws.autoscalingplans#AnyScaleScalingPlannerFrontendService
Issue-Key: auto-scaling-plans|CreateScalingPlan|ValidationException
Issue-Commit: 925fe565299dec6eb0f742dd224c6e41f11f8969

# REOPENS: CreateScalingPlan refuses MinCapacity 0 with ValidationException "Minimum capacity cannot be less than 1", so no scale-to-zero or ForecastOnly predictive plan can be created

> _Note: this issue was identified against the code base at commit `925fe565299dec6eb0f742dd224c6e41f11f8969`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateScalingPlan

## Description
REOPENS 20261022T112048Z-autoscaling-plans-wrong-status-createscalingplan-skips-instruction-validation.md: its fix left the call refused.

That issue asked CreateScalingPlan to start validating ScalingInstructions (its recorded probe was MaxCapacity < MinCapacity). The validation that landed also invented a floor of 1 on MinCapacity, and that floor now refuses a request AWS accepts.

Cause 1 of 1: a scaling instruction whose MinCapacity is 0 is refused 400 ValidationException instead of being accepted.
AWS: ScalingInstruction$MinCapacity targets ResourceCapacity, an integer with NO @range in the model, documented only as "The minimum capacity of the resource." (Smithy: shape ScalingInstruction member MinCapacity -> ResourceCapacity (integer, no range); the only refusal the recorded validation probes state about capacity is the cross-member one, "Maximum capacity cannot be less than minimum capacity"). Recorded behavior: CreateScalingPlan with one instruction for ServiceNamespace 'autoscaling', ResourceId 'autoScalingGroup/<group>', ScalableDimension 'autoscaling:autoScalingGroup:DesiredCapacity', MinCapacity 0, MaxCapacity 1, one predefined target-tracking configuration, a PredefinedLoadMetricSpecification, PredictiveScalingMode 'ForecastOnly' and DisableDynamicScaling true answers HTTP 200 with {"ScalingPlanVersion": 1}, and the plan then settles Active. A zero minimum is the normal configuration for an Auto Scaling group whose MinSize is 0 (and for a ForecastOnly plan, which scales nothing at all), so the floor of 1 is not an AWS rule.
Emulator: HTTP 400, Error.Code 'ValidationException', Error.Message 'Minimum capacity cannot be less than 1' - entirely fixed text, with no part taken from the request or from state; the emulator invents both the rule and the sentence. The floor is a crate constant, not a modeled or recorded bound.
Why: the whole predictive-scaling surface of this service is unreachable for a scale-to-zero resource. A client that creates a ForecastOnly plan over an empty Auto Scaling group - the documented way to evaluate a forecast before enabling scaling - gets a terminal client error at CreateScalingPlan, so the plan never exists, DescribeScalingPlans never reports it, and GetScalingPlanResourceForecastData can never be exercised. The refusal is also indistinguishable, to an SDK, from a genuinely invalid request, so repair logic rewrites a request AWS would have accepted.
Fix: the scaling-instruction validation shared by the CreateScalingPlan and UpdateScalingPlan handlers (validate_scaling_instructions in the service's state module) must drop the invented MinCapacity >= 1 floor and keep only the relations the model and the recordings state (MaxCapacity >= MinCapacity, the documented PredictiveScalingMaxCapacityBuffer 1-100 range, the ScheduledActionBufferTime bounds). A MinCapacity of 0 must reach the create path and be stored and reported as given.
Traces: autoscaling-plans.asp-predictive-forecast-without-history-has-no-datapoints. Code sites: services/service-com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice/src/state.rs:953-958 (MIN_RESOURCE_CAPACITY = 1) and src/state.rs:1308-1312 (the refusal), reached from src/ops/create_scaling_plan.rs:185-186.

## Evidence
Check autoscaling-plans.asp-predictive-forecast-without-history-has-no-datapoints [error]:
  botocore.errorfactory.ValidationException: An error occurred (ValidationException) when calling the CreateScalingPlan operation: Minimum capacity cannot be less than 1

Request that was refused (one instruction): ServiceNamespace='autoscaling', ResourceId='autoScalingGroup/<group>', ScalableDimension='autoscaling:autoScalingGroup:DesiredCapacity', MinCapacity=0, MaxCapacity=1, TargetTrackingConfigurations=[{PredefinedScalingMetricSpecification={PredefinedScalingMetricType='ASGAverageCPUUtilization'}, TargetValue=50.0}], PredefinedLoadMetricSpecification={PredefinedLoadMetricType='ASGTotalCPUUtilization'}, PredictiveScalingMode='ForecastOnly', DisableDynamicScaling=True.

Expected (recorded): HTTP 200, body {"ScalingPlanVersion": 1}; the plan then settles StatusCode='Active' and reads back PredictiveScalingMode='ForecastOnly', DisableDynamicScaling=true and its PredefinedLoadMetricSpecification; every ForecastDataType read answers 200 with an empty Datapoints list.
Actual: HTTP 400 ValidationException 'Minimum capacity cannot be less than 1'; the plan is never created, so none of the forecast assertions are reachable.

Model evidence (com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice): ScalingInstruction$MinCapacity -> ResourceCapacity, declared `{"type": "integer"}` with no @range trait; ValidationException is a client error, httpError 400.

Emulator source: src/state.rs:956 comments the floor as "the service refuses a minimum capacity below one unit" - a rule neither the model nor any recording states.

## Steps to Reproduce
1. Point the behavioral harness at the emulator and run check autoscaling-plans.asp-predictive-forecast-without-history-has-no-datapoints.
2. Or directly: create a launch template and an Auto Scaling group with MinSize 0 / MaxSize 1 / DesiredCapacity 0, then call autoscaling-plans CreateScalingPlan with ApplicationSource={TagFilters:[{Key:'sim2real-asp',Values:['<tag>']}]} and one ScalingInstruction as quoted in the evidence (MinCapacity 0, MaxCapacity 1, PredictiveScalingMode 'ForecastOnly', DisableDynamicScaling true).
3. Observe HTTP 400 ValidationException 'Minimum capacity cannot be less than 1' instead of HTTP 200 with ScalingPlanVersion 1.
4. Re-run the same request with MinCapacity 1 to confirm the only thing refused is the zero minimum.
