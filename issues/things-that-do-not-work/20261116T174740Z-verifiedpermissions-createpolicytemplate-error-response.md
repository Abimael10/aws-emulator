Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 7e081c0d0a956f09d8276e607b12ee6e057271df

# CreatePolicyTemplate's per-store quota refusal omits resourceId, so the 402 does not name the policy store its three sibling quota refusals name

_Filed: 2026-11-16T17:47:40Z_
_Operation: CreatePolicyTemplate | Class: error-response_

> _Note: this issue was identified against the code base at commit `7e081c0d0a956f09d8276e607b12ee6e057271df`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Four handlers in this crate refuse a write because a PER-POLICY-STORE quota is exhausted, all with the modelled `ServiceQuotaExceededException`, whose `resourceId` the model documents as "The unique ID of the resource referenced in the failed request". Three of them — `CreatePolicy`, `CreateIdentitySource`, `PutSchema` (and `UpdatePolicy` for the same store-wide Cedar-byte budget) — fill `resourceId` with the policy store id, because the policy store is the resource the request referenced and the resource that is at its limit. `CreatePolicyTemplate` alone leaves the member absent (`..Default::default()`), with a comment that justifies the omission by saying "there is no one template id to name here".

That reasoning is contradicted by the siblings inside this same crate: `CreatePolicy`'s quota is equally "on the store's policies as a whole" — its message says exactly that — and it still reports `resource_id: Some(store_id)`. So for the identical class of refusal a client that reads `resourceId` to learn WHICH policy store is full is told the store on a policy, identity-source or schema create and told nothing on a policy-template create.

The crate's own test suite states the convention explicitly: `tests/all/identity_sources.rs` asserts of the per-store quota 402 that it must "name the store". No test pins `CreatePolicyTemplate`'s quota body beyond its error code (`tests/all/name_conflict_precedes_capacity.rs` asserts only `error_code == "ServiceQuotaExceededException"`), so the divergence is unpinned rather than deliberate.

(The one defensible omission is `CreatePolicyStoreAlias`, whose quota is account-wide and references no single store; it is not part of this finding.)

## Evidence

The outlier — `services/service-com-amazonaws-verifiedpermissions-verifiedpermissions/src/ops/create_policy_template.rs:300-321`:
```
        if store.policy_templates.len() >= crate::state::MAX_POLICY_TEMPLATES_PER_STORE
            || store.cedar_bytes() + input.statement.len()
                > crate::state::MAX_POLICY_BYTES_PER_STORE
        {
            return Err(
                CreatePolicyTemplateErrorModeled::ServiceQuotaExceededException(
                    ServiceQuotaExceededException {
                        message: format!(
                            "policy store {store_id} is at its limit of {} policy templates / \
                             {} bytes of Cedar text",
                            crate::state::MAX_POLICY_TEMPLATES_PER_STORE,
                            crate::state::MAX_POLICY_BYTES_PER_STORE
                        ),
                        // The quota is on the store's policy templates as a whole,
                        // so there is no one template id to name here.
                        resource_type: "POLICY_TEMPLATE".to_string(),
                        ..Default::default()
                    },
                )
                .into(),
            );
        }
```

The sibling with the same store-wide quota, which DOES name the store — `src/ops/create_policy.rs:419-437`:
```
        if store.policies.len() >= crate::state::MAX_POLICIES_PER_STORE
            || store.cedar_bytes() + statement.as_ref().map_or(0, String::len)
                > crate::state::MAX_POLICY_BYTES_PER_STORE
        {
            return Err(CreatePolicyErrorModeled::ServiceQuotaExceededException(
                ServiceQuotaExceededException {
                    message: format!(
                        "policy store {store_id} is at its limit of {} policies / {} bytes \
                         of Cedar text",
                        ...
                    ),
                    resource_id: Some(store_id.clone()),
                    resource_type: "POLICY".to_string(),
                    ..Default::default()
                },
            )
            .into());
        }
```

`src/ops/create_identity_source.rs:299-314`:
```
        if store.identity_sources.len() >= crate::state::MAX_IDENTITY_SOURCES_PER_STORE {
            ... ServiceQuotaExceededException {
                        message: format!(
                            "policy store {store_id} already holds the maximum of {} identity sources",
                            ...),
                        resource_id: Some(store_id.clone()),
                        resource_type: crate::state::RESOURCE_TYPE_IDENTITY_SOURCE.to_string(),
```

`src/ops/put_schema.rs:161-177`:
```
        if definition.len() > crate::state::cedar::MAX_SCHEMA_BYTES {
            return Err(PutSchemaErrorModeled::ServiceQuotaExceededException(
                ServiceQuotaExceededException {
                    message: format!(...),
                    resource_id: Some(store_id.clone()),
                    resource_type: crate::state::RESOURCE_TYPE_SCHEMA.to_string(),
```

`src/ops/update_policy.rs:391-401` (same Cedar-byte budget, on an update):
```
                return Err(UpdatePolicyErrorModeled::ServiceQuotaExceededException(
                    ServiceQuotaExceededException {
                        message: format!(
                            "policy store {store_id} is at its limit of {} bytes of Cedar text",
                            ...),
                        resource_id: Some(store_id.clone()),
                        resource_type: crate::state::RESOURCE_TYPE_POLICY.to_string(),
```

The member the model gives the shape — `smithy_query(query="shape", shape="ServiceQuotaExceededException")`:
```
"resourceId": {"target": "String", "doc": "<p>The unique ID of the resource referenced in the failed request.</p>"},
"resourceType": {"target": "ResourceType", "required": true, ...}
```

The crate's own test stating the convention — `tests/all/identity_sources.rs:800-804`:
```
    assert_eq!(
        body["resourceId"], json!(store_id),
        "case \"one-past-the-bound\": expected the refusal to name the store, got {:?}",
        body["resourceId"]
    );
```

Test coverage of the outlier, which pins only the code and so would not change if `resourceId` were filled — `tests/all/name_conflict_precedes_capacity.rs:493-503`:
```
        let (status, body) = attempt_create(&router, *resource, &store_id, free).await;
        assert_eq!(status, 402, ...);
        assert_eq!(
            error_code(&body),
            "ServiceQuotaExceededException",
            "case {case:?}: expected the modelled quota error, got {body}"
        );
```

## Steps to Reproduce

1. CreatePolicyStore with `{"validationSettings": {"mode": "OFF"}}`.
2. Fill the store to the policy-template bound (`MAX_POLICY_TEMPLATES_PER_STORE`, or the store's Cedar-byte budget) with CreatePolicyTemplate calls.
3. Call CreatePolicyTemplate once more and read the 402 body: `resourceType` is `POLICY_TEMPLATE` and `resourceId` is ABSENT — the response does not say which policy store is at its limit.
4. Do the same on a fresh store with CreatePolicy (fill to the Cedar-byte budget, then create one more), and read the 402 body: `resourceId` is the `policyStoreId`.
5. Same again with CreateIdentitySource (`MAX_IDENTITY_SOURCES_PER_STORE`) and with PutSchema (a schema past `MAX_SCHEMA_BYTES`): both report `resourceId` = the store id.
6. Compare: four per-store quota refusals of one service, three naming the store and one naming nothing, for the same documented failure mode.
