Issue-Severity: MEDIUM
Issue-Gap-Class: event-propagation
Issue-Commit: ee4b7fe7bd6b7d53702c6155929c40eba220b7c5

# A replicated offline-store object declares a column twice when a posted feature is named is_deleted, write_time or api_invocation_time

_Filed: 2026-11-17T22:45:12Z_
_Operation: PutRecord | Class: event-propagation_

> _Note: this issue was identified against the code base at commit `ee4b7fe7bd6b7d53702c6155929c40eba220b7c5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::offline_store::record_parquet_bytes` lays out one Parquet column per feature of the posted record and then UNCONDITIONALLY appends the three columns Feature Store adds to every offline row (`write_time`, `api_invocation_time`, `is_deleted`). Nothing in this crate keeps a posted feature from carrying one of those three names, so the schema of the replicated object can declare the same column name twice — and the `is_deleted` column that says whether a row is a delete marker is then indistinguishable from an ingested feature value of the same name.

The module's own doc asserts the collision is impossible because those are "the three names a feature group may therefore not declare". That rule belongs to the SageMaker CONTROL plane's `FeatureDefinitions`; this runtime data plane never checks a posted feature name against the group's declared features, and it cannot: the shared directory record it resolves a group through carries no schema at all (only the group's name, ARN, record-identifier feature, event-time feature, online flag, offline delivery and default TTL). The modelled `FeatureName` constraint admits all three names, so the request reaches the layout unchanged.

So the one quoted premise that makes the layout safe is not enforced anywhere in the crate, and the artifact delivered into Amazon S3 is malformed for input the model admits. The same function is reached by `BatchWriteRecord` and `DeleteRecord` through the same `replicate` call.

## Evidence

The doc that states the premise — services/.../src/state/offline_store.rs:24-28:
```
//! columns are the record's own features plus the three Feature Store adds to every offline
//! record: `write_time`, `api_invocation_time` and `is_deleted` — the three names a feature
//! group may therefore not declare ("FeatureNames cannot be any of the following:
//! is_deleted, write_time, api_invocation_time", `FeatureDefinitions`).
```

The code that breaks under it — services/.../src/state/offline_store.rs:282-289:
```
    let mut fields = Vec::with_capacity(record.features.len() + 3);
    for (name, _) in &record.features {
        fields.push(::std::sync::Arc::new(text_column(name)?));
    }
    for name in [WRITE_TIME_COLUMN, API_INVOCATION_TIME_COLUMN] {
        fields.push(::std::sync::Arc::new(timestamp_column(name)?));
    }
    fields.push(::std::sync::Arc::new(boolean_column(IS_DELETED_COLUMN)?));
```
with the three reserved names at services/.../src/state/offline_store.rs:47-54 (`WRITE_TIME_COLUMN = "write_time"`, `API_INVOCATION_TIME_COLUMN = "api_invocation_time"`, `IS_DELETED_COLUMN = "is_deleted"`). No branch excludes, renames or refuses a feature column whose name equals one of them.

The model admits those names — `smithy_query(query="shape", shape="FeatureName")`: `{"type": "string", "length": {"min": 1, "max": 64}, "pattern": "^[a-zA-Z0-9]([-_]*[a-zA-Z0-9]){0,63}$"}`. `is_deleted`, `write_time` and `api_invocation_time` all match that pattern (`[-_]*[a-zA-Z0-9]` admits the `_`), so the boundary validator passes them.

Nothing upstream filters them: `crate::state::prepare_record` reads the posted record only through the group's `record_identifier_feature_name` and `event_time_feature_name` and stores every other feature verbatim — services/.../src/state.rs:501-505:
```
        features: held
            .iter()
            .copied()
            .map(StoredFeatureValue::from)
            .collect(),
```
and the resolved definition carries no feature list to check against — services/common/src/feature_store_groups.rs:48-82 declares `FeatureGroupRecord` with exactly `name`, `arn`, `record_identifier_feature_name`, `event_time_feature_name`, `online_store_enabled`, `offline_store`, `default_ttl_seconds`.

The call site that delivers it — services/.../src/ops/put_record.rs:287-299 passes `OfflineRecord::of(&definition, &input.record)` to `crate::state::offline_store::replicate`, which lays the bytes out at services/.../src/state/offline_store.rs:241-244.

Test coverage: the existing layout tests only use ordinary feature names — services/.../src/state/offline_store.rs:425-436 posts `customer_id`, `event_time`, `tags`, and asserts the column list at :453-465 as `["customer_id", "event_time", "tags", WRITE_TIME_COLUMN, API_INVOCATION_TIME_COLUMN, IS_DELETED_COLUMN]`. No case posts a feature named after one of the three, so none pins the colliding behaviour.

## Steps to Reproduce

1. Through the SageMaker control plane, CreateFeatureGroup `customers` with RecordIdentifierFeatureName=customer_id, EventTimeFeatureName=event_time, OnlineStoreConfig.EnableOnlineStore=true and an OfflineStoreConfig pointing at an existing S3 bucket (so `definition.offline_store` is Some).
2. PUT /FeatureGroup/customers with Record=[{FeatureName:"customer_id",ValueAsString:"C1"},{FeatureName:"event_time",ValueAsString:"2026-06-01T00:00:00Z"},{FeatureName:"is_deleted",ValueAsString:"maybe"}] — every name satisfies the modelled FeatureName pattern, so the request is accepted and answered 200.
3. List the group's offline-store prefix in S3 and read the object that was appended.
4. Inspect its Parquet schema: it declares four feature columns plus the three added ones, with `is_deleted` appearing TWICE — once as a UTF8 column holding "maybe" and once as the BOOLEAN delete marker — so the delete-marker column of the row can no longer be identified by name.
5. Repeat with a feature named `write_time` or `api_invocation_time` for the same result on the two timestamp columns.
