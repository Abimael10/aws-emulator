Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: e5b74111f4def785a5d4295c90400d18c8715b4a

# PutAppInstanceRetentionSettings writes the AppInstance but does not move its LastUpdatedTimestamp, unlike every other write in the crate

_Filed: 2026-11-19T15:22:23Z_
_Operation: PutAppInstanceRetentionSettings | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `e5b74111f4def785a5d4295c90400d18c8715b4a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The handler writes two fields onto the stored `AppInstanceRecord` (`retention_settings` and `retention_deletion_initiated_at`) and leaves `last_updated_timestamp` untouched, so after a served retention put `DescribeAppInstance` still reports NO `LastUpdatedTimestamp` for an instance that has been written to. Its direct counterpart for the other resource kind — `PutAppInstanceUserExpirationSettings` — states the rule explicitly and re-stamps the record for exactly this reason, and `UpdateAppInstance` stamps the instance on its own write. The crate's own update test states the rule as general for every resource kind: a resource that was written to carries the stamp of its last write.

## Evidence

The write that skips the stamp — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/put_app_instance_retention_settings.rs:184-187:
```
        let settings = input.app_instance_retention_settings;
        record.retention_deletion_initiated_at = crate::state::retention_days_in_effect(&settings)
            .map(|_days| ::std::time::SystemTime::now());
        record.retention_settings = Some(settings.clone());
```
(no `record.last_updated_timestamp = ...` anywhere in the file)

The counterpart settings-put states the rule and applies it — src/ops/put_app_instance_user_expiration_settings.rs:212-215:
```
        record.expiration_settings = input.expiration_settings;
        // Writing the settings writes the user, so its last-updated instant moves, the
        // way every other operation that changes a user stamps it.
        record.last_updated_timestamp = ::std::time::SystemTime::now();
```
and the instance's other writer does the same — src/ops/update_app_instance.rs:192-197:
```
        // `Name` and `Metadata` are both `required`, so an update REPLACES the stored
        // pair with what the caller sent. That change is what gives this instance a
        // last-updated instant to report.
        record.name = input.name;
        record.metadata = Some(input.metadata);
        record.last_updated_timestamp = Some(::std::time::SystemTime::now());
```
The reader that exposes the difference — src/ops/describe_app_instance.rs:187-191:
```
                // Reported only once something has updated the instance: an instance
                // that still carries exactly what created it has no update to report.
                last_updated_timestamp: record
                    .last_updated_timestamp
                    .map(crate::state::wire_timestamp),
```
The crate's own test states the rule generally — tests/all/resource_update.rs:239-248:
```
            // A resource nobody wrote to still reports a last-updated instant only
            // where its kind is stamped at creation; one that was written to carries the
            // stamp of its last write.
```
Test coverage: no test in tests/all/app_instance_retention_settings.rs or tests/all/retention_settings_effect_instant.rs reads `LastUpdatedTimestamp` (grep for `LastUpdatedTimestamp` over tests/ matches only user_expiration_settings.rs, app_instance_bot_description.rs, user_endpoint_registration.rs, resource_update.rs and app_instance_lifecycle.rs:353, the latter on a never-written instance), so nothing pins the current absence.

## Steps to Reproduce

1. POST /app-instances -> AppInstanceArn A
2. GET /app-instances/{A} -> 200, AppInstance.LastUpdatedTimestamp absent (correct: nothing has written it)
3. PUT /app-instances/{A}/retention-settings with {"AppInstanceRetentionSettings": {"ChannelRetentionSettings": {"RetentionDays": 30}}} -> 200
4. GET /app-instances/{A}/retention-settings -> 200 with the settings, proving the put wrote the instance
5. GET /app-instances/{A} -> 200 but AppInstance.LastUpdatedTimestamp is STILL absent
6. Compare: PUT /app-instance-users/{U}/expiration-settings then GET /app-instance-users/{U} -> the user's LastUpdatedTimestamp has moved
