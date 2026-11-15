Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 3221c15a7a3dab3b4318847e66c4117cbe0dd5e4

# An explicitly empty volumeArns list selects no volumes, contradicting the handler's own doc and every sibling reader

_Filed: 2026-11-15T07:52:41Z_
_Operation: GetEBSVolumeRecommendations | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `3221c15a7a3dab3b4318847e66c4117cbe0dd5e4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`requested_volume_ids` is documented to mean "the volumes this request asks about, by volume id, or `None` when it names none and so asks about the account's whole inventory in this Region". But the code only returns `None` when the member is ABSENT: an explicitly empty list (`"volumeArns": []`) falls through the loop and yields `Ok(Some(vec![]))`, which the selection filter then matches against with `wanted.contains(&volume.volume_id)` — false for every volume. So a request that names no volume at all is answered with zero recommendations instead of the account's whole analysed inventory, even though the doc above the function says the opposite.

Every other recommendation reader in this crate resolves the same "names none" case to "the whole inventory", and `GetEC2InstanceRecommendations` does it by checking emptiness BEFORE the per-ARN loop — which is exactly what also keeps the two distinct cases apart there: an empty REQUEST list means "all", while a list whose every ARN names another Region means "nothing". This handler conflates the two.

The same crate also already treats an empty list as "names none" for `accountIds` (`crate::state::requested_account_id` returns the caller's own account for `Some(&[])`), with a test that calls that "a shape clients send".

## Evidence

Handler doc states the rule, code breaks it — services/service-com-amazonaws-computeoptimizer-computeoptimizerservice/src/ops/get_ebs_volume_recommendations.rs:374-375:
```
/// The volumes this request asks about, by volume id, or `None` when it names none and so
/// asks about the account's whole inventory in this Region.
```
same file, lines 388-403:
```
    let Some(volume_arns) = volume_arns else {
        return Ok(None);
    };
    let mut volume_ids = Vec::new();
    for arn in volume_arns {
        ...
    }
    Ok(Some(volume_ids))
```
(an empty `volume_arns` is `Some(&[])`, so it skips the `else` and returns `Ok(Some(vec![]))`)

and the selection that consumes it, same file lines 282-286:
```
            .filter(|volume| {
                requested_volumes
                    .as_ref()
                    .is_none_or(|wanted| wanted.contains(&volume.volume_id))
            })
```

Sibling in the SAME crate does it the other way — services/service-com-amazonaws-computeoptimizer-computeoptimizerservice/src/ops/get_ec2_instance_recommendations.rs:433-435:
```
    if instance_arns.is_empty() {
        return Ok(None);
    }
```
and the other readers likewise: get_auto_scaling_group_recommendations.rs:327 `.filter(|(_, arn)| requested_arns.is_empty() || requested_arns.contains(arn))`; get_idle_recommendations.rs:324 `resource_arns.is_empty()\n                    || recommendation...`; get_license_recommendations.rs:319-321 `requested_arns.is_empty() || requested_arns.contains(...)`; get_rds_database_recommendations.rs:325 `resource_arns.is_empty() || resource_arns.contains(&resource.arn_in(&region))`; get_ecs_service_recommendations.rs:381 `let explicit = !requested.is_empty();`.

The crate's own empty-list convention for `accountIds` — src/state.rs:992-994:
```
    let Some(account_id) = account_ids.first() else {
        return Ok(::engine::emulator_account_id().to_string());
    };
```
pinned by tests/all/cross_account_requests.rs:151-155:
```
        // The caller's own account, named explicitly or by an empty list, reads exactly
        ...
            ("empty list", json!({"accountIds": []})),
```

Test coverage: tests/all/ebs_volume_recommendations.rs:534 `volume_arns_select_the_volumes_asked_about_and_nothing_else` exercises one-ARN, unknown-ARN, duplicate-ARN and non-volume-ARN cases but never the empty list; tests/all/ebs_volume_inventory_analysis.rs:661 pins the distinct out-of-Region case (`json!({"volumeArns": [other]})` → nothing), which the fix leaves unchanged.

## Steps to Reproduce

1. Opt the account in with UpdateEnrollmentStatus(status=Active) and let the analysis become due so GetEBSVolumeRecommendations reports the account's analysed volumes.
2. Call GetEBSVolumeRecommendations with an empty body `{}` and note the volumes reported (N > 0).
3. Call GetEBSVolumeRecommendations with `{"volumeArns": []}`.
4. Observe `volumeRecommendations` is empty, although the handler doc says an input that "names none" asks about the account's whole inventory, and the sibling GetEC2InstanceRecommendations answers `{"instanceArns": []}` with its whole inventory.
