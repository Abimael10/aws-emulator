Issue-Source: trace-sweep-divergence
Issue-Trace: autoscalingplans-forecast-time-window-validation-envelopes
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscalingplans-forecast-time-window-validation-envelopes
Issue-Shape-Id: com.amazonaws.autoscalingplans#AnyScaleScalingPlannerFrontendService
Issue-Key: autoscaling-plans|GetScalingPlanResourceForecastData|2xx->4xx|$.Datapoints
Issue-Key: autoscaling-plans|GetScalingPlanResourceForecastData|2xx->4xx|status
Issue-Commit: 9335e6a6f0eaf1e6ae0c62968f13439343c093f5

# GetScalingPlanResourceForecastData refuses forecast windows AWS serves: 400 ValidationException instead of 200 with an empty Datapoints list

> _Note: this issue was identified against the code base at commit `9335e6a6f0eaf1e6ae0c62968f13439343c093f5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetScalingPlanResourceForecastData

## Description
Cause 1 of 1: the emulator's request-level forecast-window validation on GetScalingPlanResourceForecastData is drawn tighter than AWS's, so window envelopes AWS answers successfully are refused with HTTP 400 ValidationException. The fixture `autoscalingplans-forecast-time-window-validation-envelopes` walks a family of StartTime/EndTime envelopes against one plan; the envelopes AWS refuses are refused correctly (those steps pass), but four envelopes AWS ACCEPTS are refused too (task#1, task#5, task#6, task#7 — one cluster, one cause).

AWS: Smithy (model com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice) — GetScalingPlanResourceForecastData binds exactly two errors, InternalServiceException (HTTP 500, server) and ValidationException (HTTP 400, client); there is no not-found error on this operation. Its output shape GetScalingPlanResourceForecastDataResponse has a single @required member `Datapoints` (list Datapoints), so a successful answer ALWAYS carries the member, and an answer with no forecast carries it as the empty list — never as an absent member. The only window text the model states is on the two input members: StartTime — "The inclusive start time of the time range for the forecast data to get. The date and time can be at most 56 days before the current date and time."; EndTime — "The exclusive end time of the time range for the forecast data to get. The maximum time duration between the start and end time is seven days. Although this parameter can accept a date and time that is more than two days in the future, the availability of forecast data has limits. AWS Auto Scaling only issues forecasts for periods of two days in advance." For the four envelopes at issue the recorded behavior is: HTTP 200 with body `{"Datapoints": []}` — i.e. AWS treats a window it holds no forecast data over as an EMPTY forecast, not as a malformed request. Recorded behavior only for the precise placement of each boundary; the recording is the authority for which envelopes are accepted.

Emulator: each of the four steps answers HTTP 400 (ValidationException) and the response body therefore carries no `Datapoints` member at all — the replay reports `status: expected 200, got 400` and `$.Datapoints: missing (expected [], got None)`. The refusal comes from the operation's pre-state validation, which rejects a window on three self-authored rules before the plan is ever looked up, with these messages: "Start time must be before end time" (any EndTime at or before StartTime), "The maximum allowed time range is 7 days" (width measured in whole 3600 s forecast periods), and "Start time must be at most 56 days before the current date and time" (history measured the same way). In each of those messages the sentence text is fixed emulator-authored prose and the only variable parts are the two day counts (7 and 56) interpolated from the crate's own constants — no name, id or value from the request appears, so the message cannot be matched to AWS's text for an accepted-window case because AWS issues no refusal there at all. The requirement these steps assert is the success shape: status 200 and a present, empty `Datapoints` list.

Why: a client reading a forecast for a window at or just outside the documented bounds — a zero-width or inverted range, a range at the seven-day width, or a start at the 56-day history edge — gets a hard client error where AWS gives it a well-formed empty forecast. SDK callers see a modelled ValidationException and abort, and any caller that iterates windows (paging a 56-day history in fixed slices, or probing whether a plan has predictive data yet) fails on the edge slice instead of reading through it. Because the member is @required in the output, a generated client deserializing the 400 also loses the guarantee that `Datapoints` is always present.

Fix: the operation's validation, not its forecast computation. `validate_forecast_window` in services/service-com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice/src/ops/get_scaling_plan_resource_forecast_data.rs must only refuse the windows AWS refuses, and every other window must fall through to the handler's normal path, which already answers an empty `Datapoints` list when the stored plan holds no instruction for the requested resource. The call site is the `validate_forecast_window(...).map_err(validation)?` line in `handle`, ahead of the plan lookup; the three refusal strings and the whole-period measurement helpers (`whole_periods`, `days_in_periods`, `MAX_FORECAST_WINDOW_DAYS`, `MAX_FORECAST_HISTORY_DAYS`) are in the same file.

Traces: autoscalingplans-forecast-time-window-validation-envelopes#task#1 (representative), plus task#5, task#6 and task#7 of the same fixture — all four in one cluster with divergence key `service=autoscaling-plans|GetScalingPlanResourceForecastData|status=200->400|keys=$.Datapoints,status`. The same fixture's remaining steps pass, so the defect is where the window bounds are drawn, not the existence of window validation.

No existing open issue covers this: the autoscaling-plans issues already filed are `20261114T175837Z-autoscaling-plans-reopens-createscalingplan-refuses-mincapacity-0-...` (CreateScalingPlan MinCapacity 0) and `20261115T121354Z-autoscaling-plans-updatescalingplan-discards-the-request-s-new-targetvalue-for` (UpdateScalingPlan TargetValue), neither of which touches GetScalingPlanResourceForecastData; `20261111T072954Z-autoscaling-getpredictivescalingforecast-stub.md` is a different service (autoscaling) and a different operation.

## Evidence
Replay cluster (trace set simparity-wire-aws-v1, emulator up, /__admin/health 200):
  fixture_id: autoscalingplans-forecast-time-window-validation-envelopes
  step_key:   task#1
  service:    autoscaling-plans
  operation:  GetScalingPlanResourceForecastData
  verdict:    mismatch
  expected_status: 200   actual_status: 400
  detail: "status: expected 200, got 400" / "$.Datapoints: missing (expected [], got None)"
  affected (4): task#1, task#5, task#6, task#7
  divergence_key: service=autoscaling-plans|GetScalingPlanResourceForecastData|status=200->400|keys=$.Datapoints,status

Expected (recorded AWS): HTTP 200, body carries the @required output member as an empty list — Datapoints: [].
Actual (emulator): HTTP 400 ValidationException, no Datapoints member in the body. The refusal text is one of the three emulator-authored sentences in validate_forecast_window — "Start time must be before end time", "The maximum allowed time range is 7 days", "Start time must be at most 56 days before the current date and time" — whose only variable parts are the interpolated day counts 7 and 56; AWS emits no refusal for these envelopes, so any of the three is a divergence here.

Model evidence (smithy_query on services/service-com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice/smithy-model/com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice.json):
  errors: InternalServiceException (client=false, httpError 500), ValidationException (client, httpError 400) — no not-found error
  output: Datapoints -> target Datapoints, required: true (list)
  input: StartTime/EndTime TimestampType, both required

45 of 49 corpus steps pass; these 4 are the whole divergence set (score 91.8%).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus: replay_wire_corpus() on trace set simparity-wire-aws-v1.
2. Replay fixture `autoscalingplans-forecast-time-window-validation-envelopes`, step `task#1` (and `task#5`, `task#6`, `task#7`): each POSTs GetScalingPlanResourceForecastData with the required ScalingPlanName, ScalingPlanVersion, ServiceNamespace, ResourceId, ScalableDimension, ForecastDataType and a StartTime/EndTime pair at one of the fixture's window envelopes.
3. Observe HTTP 400 with a ValidationException naming the window constraint, and no Datapoints member in the body.
4. Expected per the recording: HTTP 200 with `Datapoints: []` (the member is @required, so it is present and empty when there is no forecast to report).
