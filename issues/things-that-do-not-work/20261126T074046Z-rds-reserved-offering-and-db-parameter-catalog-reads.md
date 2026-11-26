Issue-Source: trace-sweep-divergence
Issue-Trace: rds-catalog-describe-reserved-db-instances-offerings
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-catalog-describe-reserved-db-instances-offerings rds-db-parameters-pagination
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeReservedDBInstancesOfferings|2xx->2xx|$.DescribeReservedDBInstancesOfferingsResponse.DescribeReservedDBInstancesOfferingsResult.ReservedDBInstancesOfferings.ReservedDBInstancesOffering[*]
Issue-Key: rds|DescribeReservedDBInstancesOfferings|2xx->2xx|$.DescribeReservedDBInstancesOfferingsResponse.DescribeReservedDBInstancesOfferingsResult.ReservedDBInstancesOfferings
Issue-Key: rds|DescribeDBParameters|2xx->2xx|$.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter[*].Description
Issue-Key: rds|DescribeDBParameters|2xx->2xx|$.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter[*].MinimumEngineVersion
Issue-Commit: 0a2817577611e152839e8496ef0accae0bb6c9d7

# rds reserved-offering and DB-parameter catalog reads return synthetic rows instead of the published AWS catalog

> _Note: this issue was identified against the code base at commit `0a2817577611e152839e8496ef0accae0bb6c9d7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeReservedDBInstancesOfferings

## Description
The emulator's static RDS catalog reads do not serve AWS's published catalog: the reserved-DB-instance offering catalog is generated per request (random offering ids, one repeated price template, a re-worded ProductDescription, the wrong MultiAZ/Duration/OfferingType mix) and the DB-parameter catalog carries emulator-authored descriptions and minimum engine versions. 5 causes in this bundle.

Cause 1 of 5: DescribeReservedDBInstancesOfferings for aurora-mysql returns synthetic offerings.
AWS: Smithy: DescribeReservedDBInstancesOfferings -> ReservedDBInstancesOfferingMessage$ReservedDBInstancesOfferings; each ReservedDBInstancesOffering carries ReservedDBInstancesOfferingId, DBInstanceClass, Duration, FixedPrice, UsagePrice, CurrencyCode, ProductDescription, OfferingType, MultiAZ and RecurringCharges. The model states no values, so: recorded behavior only — the recording shows AWS serving a fixed, published catalog per (DBInstanceClass, ProductDescription) with stable UUID offering ids, the engine token spelled as in the engine catalog ('aurora-mysql'), several distinct Durations (31536000 and 94608000), all three OfferingTypes ('No Upfront', 'Partial Upfront', 'All Upfront') and the FixedPrice/RecurringChargeAmount pair that belongs to each one (e.g. an 'All Upfront' row has RecurringChargeAmount '0.0').
Emulator: answers 200 with rows whose ReservedDBInstancesOfferingId is freshly generated, whose ProductDescription is re-worded to 'aurora mysql' (space instead of hyphen), whose Duration is always 31536000, whose OfferingType is always 'Partial Upfront', and whose FixedPrice/RecurringChargeAmount repeat one template pair ('353.2'/'0.04032', '706.41'/'0.08064') regardless of the offering, so no recorded offering is present in the response.
Why: a client that selects an offering by OfferingType/Duration and then purchases it by id cannot find the offering AWS advertises; a cost-estimation client reads prices that bear no relation to the advertised ones; and because the ids are regenerated per request, an id read from one response is not purchasable later.
Fix: the DescribeReservedDBInstancesOfferings handler's catalog data (its static offering table), not the serializer — it must serve the published offering rows with stable ids and the engine token from the engine catalog.
Traces: rds-catalog-describe-reserved-db-instances-offerings#task#1 (also #task#2).

Cause 2 of 5: the same read filtered to mariadb.
AWS: recorded behavior only — the mariadb catalog contains offerings with distinct Durations (31536000 and 94608000) and distinct OfferingTypes, each with its own FixedPrice.
Emulator: answers 200 with the same single-template rows (Duration always 31536000, OfferingType always 'Partial Upfront', FixedPrice '353.2'/'706.41'), so no recorded mariadb offering appears.
Why: as cause 1 — a 3-year ('94608000') or 'All Upfront' mariadb reservation cannot be discovered at all.
Fix: same catalog table, mariadb rows.
Traces: rds-catalog-describe-reserved-db-instances-offerings#task#3.

Cause 3 of 5: the ReservedDBInstancesOfferings list is serialized with the wrong structure for the filtered/paginated reads.
AWS: ReservedDBInstancesOfferings is a list wrapped in <ReservedDBInstancesOffering> members; the recording shows the full member list for these requests.
Emulator: the replay reports type_mismatch on $.DescribeReservedDBInstancesOfferingsResponse.DescribeReservedDBInstancesOfferingsResult.ReservedDBInstancesOfferings — the node the emulator emits is not the list-of-members the recording has at that path.
Why: a client's XML/Query parser gets a differently shaped node where it expects the offering list, so the list either deserializes empty or fails outright for these filter/pagination combinations.
Fix: the DescribeReservedDBInstancesOfferings serializer (list/member wrapping), plus the catalog table of causes 1-2.
Traces: rds-catalog-describe-reserved-db-instances-offerings#task#4 (also #task#5, #task#6).

Cause 4 of 5: SQL Server / custom-engine offerings ignore the requested product and MultiAZ.
AWS: recorded behavior only — a request for 'sqlserver-se(li)' returns rows for that product plus the custom-engine products the catalog publishes (e.g. 'custom-sqlserver-se(li)'), each row carrying the MultiAZ flag and the FixedPrice that offering actually has (recorded rows include MultiAZ 'true' with FixedPrice '8042.0' and an 'All Upfront' row with FixedPrice '10163.387' and RecurringChargeAmount '0.0').
Emulator: answers 200 with the same generated template (FixedPrice '353.2'/'706.41', OfferingType 'Partial Upfront', RecurringChargeAmount '0.04032'/'0.08064'), and never emits the custom-engine product rows.
Why: a licence-included or custom-engine reservation workflow sees neither the correct price nor the product rows it must choose between.
Fix: same catalog table — the per-product rows and their MultiAZ/price pairs.
Traces: rds-catalog-describe-reserved-db-instances-offerings#task#7.

Cause 5 of 5: DescribeDBParameters serves emulator-authored parameter descriptions and minimum engine versions.
AWS: Smithy: DescribeDBParameters -> DBParameterGroupDetails$Parameters, each Parameter carrying Description and MinimumEngineVersion; the model states no values, so recorded behavior only — AWS serves the engine's published parameter metadata verbatim (e.g. Description 'Defines the global automatic password expiration policy.', 'Number of digits by which to increase the scale of the result of division operations.', and MinimumEngineVersion '8.0.35' for the Group Replication member-expel-timeout parameter).
Emulator: answers 200 with paraphrased descriptions for the same parameters ('This variable defines the global automatic password expiration policy.', 'The number of digits by which to increase the scale of the result of division operations performed with the / operator.', 'The time in seconds the server waits for a connect packet before responding with a bad handshake.') and MinimumEngineVersion '8.0.13' where the recording has '8.0.35'.
Why: the Description text is what a console or tuning client shows the operator, and MinimumEngineVersion is what a client uses to decide whether a parameter may be set on a given engine version — an understated minimum lets a client attempt a parameter the engine does not have.
Fix: the DescribeDBParameters parameter catalog (its static per-family parameter metadata), not the pagination or serializer.
Traces: rds-db-parameters-pagination#task#4.

Open issue 20261119T221450Z-rds-static-catalog-reads-serve-emulator-authored-rows.md covers the orderable-DB-instance-options and Aurora engine-default *cluster* parameter catalogs, and 20261123T220118Z-rds-3-net-new-causes-rebootdbinstance-publishes-in.md covers engine-default parameter text; neither carries these reserved-offering or DescribeDBParameters replay keys, so they do not cover this finding.

## Evidence
DescribeReservedDBInstancesOfferings, rds-catalog-describe-reserved-db-instances-offerings#task#1 (status 200 -> 200): $...ReservedDBInstancesOffering[0]: missing — expected {'ProductDescription': 'aurora-mysql', 'OfferingType': 'Partial Upfront', 'FixedPrice': '707.0', 'RecurringChargeAmount': '0.081', 'ReservedDBInstancesOfferingId': '059c43ef-...'}, got {'ProductDescription': 'aurora mysql', 'FixedPrice': '353.2', 'RecurringChargeAmount': '0.04032', 'ReservedDBInstancesOfferingId': '7a02d69b-...'}; offering[1] expected OfferingType 'All Upfront' / FixedPrice '1385.0' / RecurringChargeAmount '0.0', got 'Partial Upfront' / '706.41' / '0.08064'. All 24 offering indices diverge.
Same op, #task#3 (mariadb): expected rows include Duration '94608000' / 'All Upfront' / FixedPrice '2287.0'; every actual row is Duration '31536000' / 'Partial Upfront' / '353.2' or '706.41'.
Same op, #task#4: $...ReservedDBInstancesOfferings: type_mismatch — the expected node is the ReservedDBInstancesOffering member list.
Same op, #task#7: expected 'sqlserver-se(li)' MultiAZ 'true' FixedPrice '8042.0' and 'custom-sqlserver-se(li)' FixedPrice '10163.387'; got the '353.2'/'706.41' template and no custom-engine rows.
DescribeDBParameters, rds-db-parameters-pagination#task#4 (status 200 -> 200): Parameter[7].Description expected 'Defines the global automatic password expiration policy.', got 'This variable defines the global automatic password expiration policy.'; Parameter[12].MinimumEngineVersion expected '8.0.35', got '8.0.13'; Parameter[6], [10], [11] Descriptions likewise paraphrased.

## Steps to Reproduce
1. Replay rds-catalog-describe-reserved-db-instances-offerings steps task#1, task#3, task#4 and task#7 and diff the ReservedDBInstancesOfferings list against the recording: compare offering ids for stability across two identical requests, the ProductDescription spelling, and the (Duration, OfferingType, FixedPrice, RecurringChargeAmount, MultiAZ) tuples.
2. For task#4, check the shape of the ReservedDBInstancesOfferings node — it must be a list of ReservedDBInstancesOffering members.
3. Replay rds-db-parameters-pagination step task#4 and diff Parameter[*].Description and Parameter[*].MinimumEngineVersion against the recording.
