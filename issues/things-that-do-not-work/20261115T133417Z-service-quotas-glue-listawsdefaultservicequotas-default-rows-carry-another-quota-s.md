Issue-Source: trace-sweep-divergence
Issue-Trace: service-quotas-catalog-list-aws-default-service-quotas-glue
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: service-quotas-catalog-list-aws-default-service-quotas-glue
Issue-Shape-Id: com.amazonaws.servicequotas#ServiceQuotasV20190624
Issue-Key: service-quotas|ListAWSDefaultServiceQuotas|2xx->2xx|$.Quotas[QuotaCode="L-2E9FB93F"]
Issue-Related: 20261113T025107Z-service-quotas-listawsdefaultservicequotas-default-rows-misbound-and-re-worded-for.md
Issue-Commit: 2f36732a5b78a3efee6be6751024e2ca62406c36

# glue ListAWSDefaultServiceQuotas default rows carry another quota's QuotaName, Description and Value and omit two QuotaCodes

> _Note: this issue was identified against the code base at commit `2f36732a5b78a3efee6be6751024e2ca62406c36`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListAWSDefaultServiceQuotas

## Description
ONE cause (Cause 1 of 1), ServiceCode `glue`, operation ListAWSDefaultServiceQuotas, fixture service-quotas-catalog-list-aws-default-service-quotas-glue step task#1. The emulator answers HTTP 200 with a `glue` default-quota catalog whose rows are bound to the wrong QuotaCodes: a row published under a code carries the QuotaName, Description and Value of a DIFFERENT glue quota, two recorded codes are published under no row at all, and one row's Description is omitted entirely.

AWS: Smithy fixes the row SHAPE only — ListAWSDefaultServiceQuotas.ServiceCode is @required (@length 1..63, @pattern ^[a-zA-Z][a-zA-Z0-9-]{1,63}$), output Quotas is a list of ServiceQuota, and ServiceQuota members are ServiceCode, ServiceName, QuotaArn, QuotaCode (@length 1..128), QuotaName, Value (QuotaValue, double, @range 0..10000000000), Unit, Adjustable (default false), GlobalQuota (default false), UsageMetric, Period, QuotaContext and Description (@length 0..350). The per-code CONTENT is recorded behavior only, so the recorded response of this fixture is the reference. The rule it states: a row published under a QuotaCode carries the QuotaName, Value, Unit, Adjustable, GlobalQuota, Period, UsageMetric and Description of the quota AWS publishes UNDER THAT CODE — the service's own wording, never a paraphrase and never a neighbouring quota's — and every recorded QuotaCode is present exactly once. Recorded rows this replay quotes: L-3890A802 Description 'The maximum number of column statistics tasks running concurrently in your account.'; L-F1653A6D QuotaName 'Max triggers per account' with Value 1000.0; L-E15CE20A QuotaName 'Concurrent machine learning task runs per transform', Description 'The maximum number of concurrent task runs per machine learning transform for this account.', Value 3.0; L-4256D6D2 QuotaName 'Max connection per account', Description 'The maximum number of connections in your account.', Value 1000.0; L-04CEE988 QuotaName 'Number of machine learning transforms', Description 'The maximum number of machine learning transforms for this account.', Value 100.0; L-2C3F5401 QuotaName 'Max partitions per table', Description 'The maximum number of partitions per table.'; and rows under L-2E9FB93F and L-337244C9 present in the listing. In all of these the Description/QuotaName text is FIXED text published by the service for that code, while the Value is the numeric limit the service publishes for that code — none of it is a per-run value, so none of it varies between environments.

Emulator: HTTP 200, same status as recorded, but the body misbinds 36 codes and omits 2. L-3890A802 is served with NO Description (expected the column-statistics sentence above). L-F1653A6D is served as QuotaName 'Triggers per account' with Value 25000.0 instead of 'Max triggers per account' / 1000.0. L-E15CE20A is served as the GetTable request-rate quota ('Rate of GetTable requests', 'The maximum number of GetTable requests you can make per second in this account in the current Region.', 100.0) instead of the ML-transform concurrency quota. L-4256D6D2 is served as the workflow-count quota ('Workflows per account', 'The maximum number of workflows in your account.', 250.0) instead of the connection-count quota. L-04CEE988 is served as the database-count quota ('Databases per account', 'The maximum number of databases in your account.', 10000.0) instead of the ML-transform-count quota. L-2C3F5401 is served as the development-endpoint quota ('The maximum number of development endpoints in your account.') instead of the partitions-per-table quota. The full set of codes whose QuotaName/Description/Value diverge: L-04CEE988, L-071ABE08, L-096DBB95, L-2C3F5401, L-3890A802 (Description only), L-4071B0E3, L-4256D6D2, L-5E4153CA, L-64A88F0A, L-76EC689B, L-785C1F39, L-7DD7C33A, L-83192DBF, L-83A59AA6, L-87F5991C, L-94D025B7, L-A457CBAB, L-ACAC12B6, L-AD871090, L-B8497671, L-C3952F6C, L-CB69DFA0, L-D987EC31, L-DBA56E2F, L-E15CE20A, L-EAC30D24, L-EEC98450, L-F1653A6D (QuotaName/Value), L-F1AAB534, L-F545C262, L-F574AED9, L-F953935E, L-FB210BD9, L-FEBBFA7A. Published under no row at all: L-2E9FB93F and L-337244C9.

Why: a client reading the default-quota catalog to size a workload or to drive a quota-increase request resolves a glue limit by QuotaCode. With the rows misbound it reads the wrong limit for the right code — e.g. 25000 triggers per account where AWS publishes 1000, 10000 ML transforms where AWS publishes 100, 250 connections where AWS publishes 1000 — so capacity planning, pre-flight limit checks and any RequestServiceQuotaIncrease built from the listing target the wrong quota. The two absent codes make a GetAWSDefaultServiceQuota/listing lookup for them find nothing, and L-3890A802's missing Description removes the only text that identifies what the limit governs.

Fix: the `glue` row table in the default-quota catalog data that backs the ListAWSDefaultServiceQuotas handler's state — each row must be keyed by the QuotaCode AWS publishes it under, with that code's own recorded QuotaName, Description, Value, Unit, Adjustable, GlobalQuota, Period and UsageMetric, and the two missing codes added as their own rows.

Relation to open issues (neither covers this cluster): `20261113T025107Z-service-quotas-listawsdefaultservicequotas-default-rows-misbound-and-re-worded-for.md` and `20261111T000000Z-service-quotas-listawsdefaultservicequotas-residual-misbound-rows-events-firehose-fms-fsx-gamelift-glue-imagebuilder.md` both name ServiceCode `glue`, but their Issue-Trace-Ids list the fixture id WITHOUT a step half, so the replay reports this cluster as UNOWNED (owned_by is empty) and a fixer's re-replay never runs it. This filing supplies the fixture#step linkage and the expected-vs-actual pairs the current replay reports — which have moved since those filings (L-3890A802's Description is now absent rather than mis-set, and L-F1653A6D is now served as 'Triggers per account'/25000.0 rather than a StartCrawler rate), so the state below is the current one. All three should be closed together.

Traces: service-quotas-catalog-list-aws-default-service-quotas-glue#task#1.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false; 270 steps, 39 passed, 231 mismatched).

Cluster: service=service-quotas, operation=ListAWSDefaultServiceQuotas, verdict=mismatch, expected_status=200, actual_status=200, fixture_id=service-quotas-catalog-list-aws-default-service-quotas-glue, step_key=task#1, affected=[service-quotas-catalog-list-aws-default-service-quotas-glue#task#1], stub_path='' , owned_by=[] (unowned by any open issue).

Expected vs actual, verbatim from the replay detail:
- $.Quotas[QuotaCode="L-3890A802"].Description: missing (expected 'The maximum number of column statistics tasks running concurrently in your account.', got None)
- $.Quotas[QuotaCode="L-F1653A6D"].QuotaName: expected 'Max triggers per account', got 'Triggers per account'
- $.Quotas[QuotaCode="L-F1653A6D"].Value: expected 1000.0, got 25000.0
- $.Quotas[QuotaCode="L-E15CE20A"].Description: expected 'The maximum number of concurrent task runs per machine learning transform for this account.', got 'The maximum number of GetTable requests you can make per second in this account in the current Region.'
- $.Quotas[QuotaCode="L-E15CE20A"].QuotaName: expected 'Concurrent machine learning task runs per transform', got 'Rate of GetTable requests'
- $.Quotas[QuotaCode="L-E15CE20A"].Value: expected 3.0, got 100.0
- $.Quotas[QuotaCode="L-4256D6D2"].Description: expected 'The maximum number of connections in your account.', got 'The maximum number of workflows in your account.'
- $.Quotas[QuotaCode="L-4256D6D2"].QuotaName: expected 'Max connection per account', got 'Workflows per account'
- $.Quotas[QuotaCode="L-4256D6D2"].Value: expected 1000.0, got 250.0
- $.Quotas[QuotaCode="L-04CEE988"].Description: expected 'The maximum number of machine learning transforms for this account.', got 'The maximum number of databases in your account.'
- $.Quotas[QuotaCode="L-04CEE988"].QuotaName: expected 'Number of machine learning transforms', got 'Databases per account'
- $.Quotas[QuotaCode="L-04CEE988"].Value: expected 100.0, got 10000.0
- $.Quotas[QuotaCode="L-2C3F5401"].Description: expected 'The maximum number of partitions per table.', got 'The maximum number of development endpoints in your account.'
- $.Quotas[QuotaCode="L-2C3F5401"].QuotaName: expected 'Max partitions per table', got a development-endpoint QuotaName

Whole rows absent (the divergence key reports the row itself, with no member suffix): $.Quotas[QuotaCode="L-2E9FB93F"] and $.Quotas[QuotaCode="L-337244C9"].

Also diverging on QuotaName/Description/Value in the same response (divergence_key, verbatim codes): L-071ABE08, L-096DBB95, L-4071B0E3, L-5E4153CA, L-64A88F0A, L-76EC689B, L-785C1F39, L-7DD7C33A, L-83192DBF, L-83A59AA6, L-87F5991C, L-94D025B7, L-A457CBAB, L-ACAC12B6, L-AD871090, L-B8497671, L-C3952F6C, L-CB69DFA0, L-D987EC31, L-DBA56E2F, L-EAC30D24, L-EEC98450, L-F1AAB534, L-F545C262, L-F574AED9, L-F953935E, L-FB210BD9, L-FEBBFA7A.

Model consulted: services/service-com-amazonaws-servicequotas-servicequotasv20190624/smithy-model/com-amazonaws-servicequotas-servicequotasv20190624.json — ListAWSDefaultServiceQuotas(ServiceCode @required, NextToken, MaxResults @range 1..100) -> {Quotas: ServiceQuotaListDefinition, NextToken}; ServiceQuota members QuotaCode @length 1..128, QuotaName, Value @range 0..10000000000, Unit, Adjustable (default false), GlobalQuota (default false), UsageMetric, Period, Description @length 0..350. The model constrains shape only; per-code content is recorded behavior.

Emulator component carrying the data (per the existing glue filings): services/service-com-amazonaws-servicequotas-servicequotasv20190624/src/state/quota_rows.rs, with coverage pinned in tests/all/aws_quota_metadata_per_code.rs and tests/all/aws_minted_catalog_coverage.rs.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1 (fixture service-quotas-catalog-list-aws-default-service-quotas-glue, step task#1).
2. Equivalently, POST the service-quotas endpoint with target ServiceQuotasV20190624.ListAWSDefaultServiceQuotas and body {"ServiceCode": "glue"} — the same request that fixture's task#1 step issues.
3. Index the returned $.Quotas by QuotaCode and compare each row against the recorded response for the SAME QuotaCode.
4. Expected (recorded): L-3890A802 carries Description 'The maximum number of column statistics tasks running concurrently in your account.'; L-F1653A6D is 'Max triggers per account' with Value 1000.0; L-E15CE20A is 'Concurrent machine learning task runs per transform' with Value 3.0; L-4256D6D2 is 'Max connection per account' with Value 1000.0; L-04CEE988 is 'Number of machine learning transforms' with Value 100.0; L-2C3F5401 is 'Max partitions per table'; rows exist for L-2E9FB93F and L-337244C9.
5. Actual: status 200, but L-3890A802 has no Description; L-F1653A6D is 'Triggers per account'/25000.0; L-E15CE20A is 'Rate of GetTable requests'/100.0; L-4256D6D2 is 'Workflows per account'/250.0; L-04CEE988 is 'Databases per account'/10000.0; L-2C3F5401 carries the development-endpoint quota; L-2E9FB93F and L-337244C9 are absent; 36 codes in all diverge.
6. The ServiceCode is clean when every returned row's QuotaName, Description, Value, Unit, Adjustable, GlobalQuota, Period and UsageMetric match the recorded row for the same QuotaCode, and no recorded QuotaCode is missing.
