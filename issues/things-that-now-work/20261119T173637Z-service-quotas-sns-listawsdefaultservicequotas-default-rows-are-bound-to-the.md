Issue-Source: trace-sweep-divergence
Issue-Trace: service-quotas-catalog-list-aws-default-service-quotas-sns
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: service-quotas-catalog-list-aws-default-service-quotas-sns
Issue-Shape-Id: com.amazonaws.servicequotas#ServiceQuotasV20190624
Issue-Key: service-quotas|ListAWSDefaultServiceQuotas|2xx->2xx|$.Quotas[QuotaCode="L-6635B4C9"]
Issue-Key: service-quotas|ListAWSDefaultServiceQuotas|2xx->2xx|$.Quotas[QuotaCode="L-A4340BCD"]
Issue-Key: service-quotas|ListAWSDefaultServiceQuotas|2xx->2xx|$.Quotas[QuotaCode="L-BF824DE9"]
Issue-Related: 20261118T044910Z-service-quotas-default-quota-catalog-rows-misbound-across-sns-sqs.md
Issue-Commit: dd0d5f7a8a9b6c8eb2d3edf27a45a29ce399d527

# sns ListAWSDefaultServiceQuotas default rows are bound to the wrong QuotaCode and six AWS rows are absent

> _Note: this issue was identified against the code base at commit `dd0d5f7a8a9b6c8eb2d3edf27a45a29ce399d527`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListAWSDefaultServiceQuotas

## Description
1 cause (one replay cluster). ServiceCode 'sns' is served by the emulator's default-quota catalog, but the catalog rows for that ServiceCode are mis-keyed: the payload of a row (QuotaName, Description, Value, Adjustable, Unit, Period, UsageMetric) belongs to a different quota than the QuotaCode that identifies it, and six AWS QuotaCodes have no row at all.

Cause 1 of 1 - ServiceCode 'sns' (fixture service-quotas-catalog-list-aws-default-service-quotas-sns, step task#1).

AWS: Smithy ListAWSDefaultServiceQuotas(ServiceCode required, @length 1..63, @pattern ^[a-zA-Z][a-zA-Z0-9-]{1,63}$; NextToken/MaxResults @range 1..100) answers HTTP 200 with Quotas: list<ServiceQuota>. Per com.amazonaws.servicequotas#ServiceQuota, each row carries ServiceCode, ServiceName, QuotaArn, QuotaCode (@length 1..128, @pattern ^[a-zA-Z][a-zA-Z0-9-]{1,128}$), QuotaName, Value (double @range 0..10000000000), Unit, Adjustable (boolean, default false, 'Indicates whether the quota value can be increased'), GlobalQuota (default false), UsageMetric (MetricInfo: MetricNamespace/MetricName/MetricDimensions/MetricStatisticRecommendation @pattern ^(Sum|Maximum)$), Period (QuotaPeriod) and Description (@length 0..350, 'The quota description'). AWS returns one row per published default quota of the ServiceCode, and every member of a row describes the SAME quota the row's QuotaCode names. Which codes AWS publishes for 'sns' and their individual values are not stated by the model - recorded behavior only; the recording is the reference. The requirement asserted here is the BINDING (QuotaCode -> that quota's name, description, value, unit, adjustability, period and usage metric) and the PRESENCE of every published code, not any one recorded number.

Emulator: HTTP 200 (expected 200), body-only divergence over 44 QuotaCodes in one response. Three sub-shapes:
(a) Row absent entirely for 6 codes - e.g. L-154ECCA7, whose AWS row is QuotaName 'SetSMSAttributes Transactions per Second', Description 'The maximum number of times the SetSMSAttributes action can be called per second.', Value 1.0, Unit 'None', Adjustable false, GlobalQuota false, Period {PeriodUnit SECOND, PeriodValue 1}, UsageMetric {MetricNamespace 'AWS/Usage', MetricName 'CallCount', MetricStatisticRecommendation 'Sum', MetricDimensions {Class 'None', Resource 'SetSMSAttributes', Service 'SNS', Type 'API'}} - plus L-039289D5, L-6635B4C9, L-93CEC191, L-A4340BCD and L-BF824DE9 (reported as `missing ... got None` for the whole row).
(b) Row present but carrying ANOTHER quota's payload - L-AB928142 should be 'CreateTopic Transactions per Second' / 'The maximum number of times the CreateTopic action can be called per second.' / Value 900.0 / UsageMetric.MetricDimensions.Resource 'CreateTopic', and is served as 'Publish Transactions per Second' / 'The maximum number of times the Publish action can be called per second.' / Value 30000.0 / Resource 'Publish'; L-F8E2BA85 should be 'Messages Published per Second' / 'The maximum number of messages published per second.' and is served as 'UntagResource Transactions per Second' / 'The maximum number of times the UntagResource action can be called per second.'. The same shift affects ~38 codes in this one response, across the members Adjustable, Description, QuotaName, Unit, Value, Period and UsageMetric (MetricName, MetricStatisticRecommendation, MetricDimensions.Resource/.Type).
(c) Row present with the right identity but an understated or incomplete value - L-876E1222.Value is 30.0 where AWS publishes 900.0, and L-1A43D3DB.UsageMetric.MetricDimensions.Resource is omitted where AWS publishes 'ApproximateNumberOfPendingSubscriptions'.

Why: a client resolving an SNS limit by QuotaCode is the normal use of this API (QuotaCode is the join key between ListAWSDefaultServiceQuotas, GetServiceQuota and RequestServiceQuotaIncrease). With the rows shifted it reads a different quota's name and number under the code it asked for - so it throttles or sizes against the wrong limit (e.g. 30000/s under the CreateTopic code, 900 vs 30 for L-876E1222) while believing it read the right one, and six published codes simply cannot be found. A wrong Adjustable additionally decides whether a quota-increase request is attempted at all, and the dropped UsageMetric dimension breaks CloudWatch AWS/Usage lookups derived from the row.

Fix: the service-quotas crate's static default-quota catalog for ServiceCode 'sns' and the ListAWSDefaultServiceQuotas row projection - entries must be keyed by QuotaCode so that QuotaName/Description/Value/Unit/Adjustable/GlobalQuota/Period/UsageMetric all come from the SAME catalog entry, and the missing codes must be added to the catalog.

Not covered by an existing issue: open issue 20261118T044910Z-service-quotas-default-quota-catalog-rows-misbound-across-sns-sqs.md describes this defect class for a group of ServiceCodes including sns, but it carries no replay key for this fixture/step (the replay reports this cluster as unowned), so the sns cluster is filed here. Related open issues for other ServiceCodes: 20261113T222805Z-service-quotas-access-analyzer-acm-acm-pca-and-6-more.md and 20261030T104829Z-service-quotas-listawsdefaultservicequotas-default-catalog-misbinds-and-omits-aws-quota.md.

Traces: fixture service-quotas-catalog-list-aws-default-service-quotas-sns, step task#1 (the cluster's only affected step).

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200). Cluster: verdict mismatch, expected_status 200, actual_status 200 - body-only divergence; affected_count 1 (service-quotas-catalog-list-aws-default-service-quotas-sns#task#1).

divergence_key: service=service-quotas|ListAWSDefaultServiceQuotas|status=200->200|keys=$.Quotas[QuotaCode="L-01EACE54"].Adjustable,...,$.Quotas[QuotaCode="L-F8E2BA85"].Value (44 distinct QuotaCodes).

Reported divergences (expected = recorded AWS response, got = emulator):
  $.Quotas[QuotaCode="L-1A43D3DB"].UsageMetric.MetricDimensions.Resource: missing (expected 'ApproximateNumberOfPendingSubscriptions', got None)
  $.Quotas[QuotaCode="L-876E1222"].Value: value_mismatch (expected 900.0, got 30.0)
  $.Quotas[QuotaCode="L-154ECCA7"]: missing (expected {'Adjustable': False, 'Description': 'The maximum number of times the SetSMSAttributes action can be called per second.', 'GlobalQuota': False, 'Period': {'PeriodUnit': 'SECOND', 'PeriodValue': 1}, 'QuotaArn': 'arn:aws:servicequotas:<region>::sns/L-154ECCA7', 'QuotaCode': 'L-154ECCA7', 'QuotaName': 'SetSMSAttributes Transactions per Second', 'ServiceCode': 'sns', 'ServiceName': 'Amazon Simple Notification Service (Amazon SNS)', 'Unit': 'None', 'UsageMetric': {'MetricDimensions': {'Class': 'None', 'Resource': 'SetSMSAttributes', 'Service': 'SNS', 'Type': 'API'}, 'MetricName': 'CallCount', 'MetricNamespace': 'AWS/Usage', 'MetricStatisticRecommendation': 'Sum'}, 'Value': 1.0}, got None)
  $.Quotas[QuotaCode="L-AB928142"].Description: value_mismatch (expected 'The maximum number of times the CreateTopic action can be called per second.', got 'The maximum number of times the Publish action can be called per second.')
  $.Quotas[QuotaCode="L-AB928142"].QuotaName: value_mismatch (expected 'CreateTopic Transactions per Second', got 'Publish Transactions per Second')
  $.Quotas[QuotaCode="L-AB928142"].UsageMetric.MetricDimensions.Resource: value_mismatch (expected 'CreateTopic', got 'Publish')
  $.Quotas[QuotaCode="L-AB928142"].Value: value_mismatch (expected 900.0, got 30000.0)
  $.Quotas[QuotaCode="L-F8E2BA85"].Description: value_mismatch (expected 'The maximum number of messages published per second.', got 'The maximum number of times the UntagResource action can be called per second.')
  $.Quotas[QuotaCode="L-F8E2BA85"].QuotaName: value_mismatch (expected 'Messages Published per Second', got 'UntagResource Transactions per Second')

Wholly absent rows (key reported with no member suffix): L-039289D5, L-154ECCA7, L-6635B4C9, L-93CEC191, L-A4340BCD, L-BF824DE9.
Misbound/field-level rows: the remaining ~38 codes in the key list, e.g. L-01EACE54, L-19F66BE5, L-21C7FFA6, L-2EE2434F, L-4126E74A, L-41B81227, L-61103206, L-A05852B7, L-A228E0EB, L-E1E48E53, L-F514C636 (Adjustable/Description/QuotaName/Period/Unit/UsageMetric/Value).

Model citations: Smithy ListAWSDefaultServiceQuotas (input ServiceCode required @length 1..63 @pattern ^[a-zA-Z][a-zA-Z0-9-]{1,63}$, output Quotas: ServiceQuotaListDefinition, paginated by NextToken/MaxResults); com.amazonaws.servicequotas#ServiceQuota members QuotaCode/QuotaName/Value/Unit/Adjustable/GlobalQuota/Period/UsageMetric/Description (Description @length 0..350); com.amazonaws.servicequotas#MetricInfo members MetricNamespace/MetricName/MetricDimensions/MetricStatisticRecommendation.

## Steps to Reproduce
1. Boot the emulator from this checkout (/__admin/health answers 200, data listener accepting).
2. Replay fixture service-quotas-catalog-list-aws-default-service-quotas-sns, step task#1 (POST /, X-Amz-Target ServiceQuotasV20190624.ListAWSDefaultServiceQuotas, body {"ServiceCode":"sns"}), collecting every page via NextToken.
3. Look up QuotaCode L-154ECCA7 in the 200 response's Quotas list - there is no row for it (likewise L-039289D5, L-6635B4C9, L-93CEC191, L-A4340BCD, L-BF824DE9).
4. Look up QuotaCode L-AB928142 - the row is served with QuotaName 'Publish Transactions per Second', Description 'The maximum number of times the Publish action can be called per second.', Value 30000.0 and UsageMetric.MetricDimensions.Resource 'Publish', i.e. the Publish quota's payload under the CreateTopic code (AWS: 'CreateTopic Transactions per Second', Value 900.0, Resource 'CreateTopic').
5. Look up QuotaCode L-F8E2BA85 - served as 'UntagResource Transactions per Second' where AWS publishes 'Messages Published per Second'.
6. Read Quotas[QuotaCode="L-876E1222"].Value (30.0, AWS 900.0) and Quotas[QuotaCode="L-1A43D3DB"].UsageMetric.MetricDimensions.Resource (absent, AWS 'ApproximateNumberOfPendingSubscriptions').
