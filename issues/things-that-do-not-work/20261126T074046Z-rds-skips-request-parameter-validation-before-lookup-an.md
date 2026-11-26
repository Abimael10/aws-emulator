Issue-Source: trace-sweep-divergence
Issue-Trace: rds-describe-global-clusters-db-cluster-id-filter
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-describe-global-clusters-db-cluster-id-filter rds-reserved-instances-catalog
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeGlobalClusters|4xx->2xx|$.ErrorResponse
Issue-Key: rds|DescribeGlobalClusters|4xx->2xx|status
Issue-Key: rds|PurchaseReservedDBInstancesOffering|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: rds|PurchaseReservedDBInstancesOffering|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|PurchaseReservedDBInstancesOffering|4xx->4xx|status
Issue-Commit: 0a2817577611e152839e8496ef0accae0bb6c9d7

# rds skips request-parameter validation before lookup: an unknown filter name is accepted and a malformed offering id is answered not-found

> _Note: this issue was identified against the code base at commit `0a2817577611e152839e8496ef0accae0bb6c9d7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeGlobalClusters

## Description
Two reads accept or mis-classify a malformed request parameter instead of refusing it with 400 InvalidParameterValue: an unrecognized Filters name is silently ignored, and a syntactically invalid reserved-offering id is resolved against state and reported not-found. 2 causes in this bundle.

Cause 1 of 2: DescribeGlobalClusters accepts an unrecognized filter name.
AWS: Smithy: DescribeGlobalClusters input Filters -> FilterList; the only modeled error is GlobalClusterNotFoundFault (404). The set of supported filter names is not in the model, so for this refusal: recorded behavior only — the recording shows AWS refusing a filter name it does not support for this operation with HTTP 400 and ErrorResponse.Error {Type 'Sender', Code 'InvalidParameterValue', Message 'Unrecognized filter name: db-cluster-identifier'}. The message's fixed text is 'Unrecognized filter name: '; the variable part is the Filter.Name taken verbatim from the request (here 'db-cluster-identifier'), so for any unsupported name the message is 'Unrecognized filter name: <Filter.Name>'.
Emulator: answers HTTP 200 with a normal GlobalClustersMessage and no ErrorResponse at all — the unknown filter is dropped and the request is served as if unfiltered.
Why: a client that filters global clusters by a name this operation does not support gets a 200 whose list is the UNFILTERED set, and reads it as the filtered answer — the filter silently does nothing, so the client acts on global clusters that do not match its predicate instead of being told the filter is wrong.
Fix: the DescribeGlobalClusters handler's input validation — reject a Filter.Name outside the supported set with 400 InvalidParameterValue before any state lookup.
Traces: rds-describe-global-clusters-db-cluster-id-filter#task#3.

Cause 2 of 2: PurchaseReservedDBInstancesOffering resolves a malformed offering id instead of validating its format.
AWS: Smithy: PurchaseReservedDBInstancesOffering requires ReservedDBInstancesOfferingId and models ReservedDBInstancesOfferingNotFoundFault (404), ReservedDBInstanceAlreadyExistsFault (404) and ReservedDBInstanceQuotaExceededFault (400). The id's format rule is not in the model, so: recorded behavior only — the recording shows AWS validating the id's syntax FIRST and refusing a non-UUID id with HTTP 400 and Error.Code 'InvalidParameterValue', Message 'ReservedDBInstanceOfferingId must be in valid UUID format'. That message is entirely fixed text — it names the member, not the submitted value — and the 404 ReservedDBInstancesOfferingNotFound class is reserved for a well-formed id that no offering carries.
Emulator: answers HTTP 404 with Error.Code 'ReservedDBInstancesOfferingNotFound' and an emulator-authored message that interpolates the submitted id ('ReservedDBInstanceOffering <id> not found.'), i.e. it skips the format check and goes straight to the catalog lookup.
Why: the two error classes mean different things to a client — 400 InvalidParameterValue is a non-retryable caller bug in the id it constructed, while 404 not-found tells the client the id was well formed but the offering is gone, which legitimately prompts a catalog re-read and retry. The emulator therefore sends clients into a re-discover/retry loop for a request that can never succeed, and SDK error-class handling (ReservedDBInstancesOfferingNotFoundFault vs the generic InvalidParameterValue) branches the wrong way.
Fix: the PurchaseReservedDBInstancesOffering handler's input validation — validate ReservedDBInstancesOfferingId's UUID format and refuse 400 InvalidParameterValue with AWS's fixed sentence before resolving it against the offering catalog.
Traces: rds-reserved-instances-catalog#task#3.

Open issues 20261122T205159Z-rds-modifytenantdatabase-validates-newtenantdbname-before-resolving-the-db.md and 20261125T233840Z-rds-stopdbinstanceautomatedbackupsreplication-never-resolves-the-source-instance-so.md record the same validate-vs-resolve ordering fault on other operations but carry neither of these replay keys, so they do not cover this finding.

## Evidence
DescribeGlobalClusters, rds-describe-global-clusters-db-cluster-id-filter#task#3: status expected 400, got 200; $.ErrorResponse: missing — expected {'Error': {'Type': 'Sender', 'Code': 'InvalidParameterValue', 'Message': 'Unrecognized filter name: db-cluster-identifier'}, 'RequestId': '<shaped-not-asserted>'}, got None. (Fixed text 'Unrecognized filter name: ' + the request's Filter.Name; RequestId is per-run and not asserted.)
PurchaseReservedDBInstancesOffering, rds-reserved-instances-catalog#task#3: status expected 400, got 404; $.ErrorResponse.Error.Code expected 'InvalidParameterValue', got 'ReservedDBInstancesOfferingNotFound'; $.ErrorResponse.Error.Message expected 'ReservedDBInstanceOfferingId must be in valid UUID format' (all fixed text), got 'ReservedDBInstanceOffering sim2real-nonexistent-offering-id not found.' (emulator wording interpolating the submitted id).

## Steps to Reproduce
1. Replay rds-describe-global-clusters-db-cluster-id-filter step task#3: DescribeGlobalClusters with Filters[0].Name = 'db-cluster-identifier'. Expect HTTP 400 with ErrorResponse.Error.Code 'InvalidParameterValue' and Message 'Unrecognized filter name: db-cluster-identifier'; observe HTTP 200 and an unfiltered GlobalClusters list.
2. Replay rds-reserved-instances-catalog step task#3: PurchaseReservedDBInstancesOffering with a non-UUID ReservedDBInstancesOfferingId. Expect HTTP 400, Code 'InvalidParameterValue', Message 'ReservedDBInstanceOfferingId must be in valid UUID format'; observe HTTP 404 with Code 'ReservedDBInstancesOfferingNotFound'.
3. Confirm a well-formed but unknown UUID still yields 404 ReservedDBInstancesOfferingNotFound.
