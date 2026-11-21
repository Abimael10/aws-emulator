Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: d7cb5d802aaf50f8ef4cbec9c9028bf6415c27df

# ComponentsInfo accepts any ComponentType although the member documentation states WD is the only accepted value

_Filed: 2026-11-21T03:43:20Z_
_Operation: RegisterApplication | Class: validation_

> _Note: this issue was identified against the code base at commit `d7cb5d802aaf50f8ef4cbec9c9028bf6415c27df`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`RegisterApplication$ComponentsInfo` carries `ComponentInfo` structures, and the model's documentation for `ComponentInfo$ComponentType` states that the only accepted value is `WD`. The `ComponentType` enum shape itself admits eight values because it is shared with the read shapes (`Component$ComponentType`, `ComponentSummary$ComponentType`), so the boundary validator cannot narrow it — only the handler can, and it does not: the handler validates only the list's length (`components_within_bounds`) and then stores whatever type the declaration carried, so a registration declaring `ASCS`, `ERS`, `HANA`, `HANA_NODE`, `ABAP`, `DIALOG` or `WEBDISP` is accepted, a component is minted for it, and it is reported by GetComponent/ListComponents and tagged under its own ARN.

This is the one handler-level value check the operation skips, while it re-states exactly this kind of narrowing for its other members: the same handler re-checks `ApplicationType` against `APPLICATION_TYPE_VALUES` before storing, and the crate re-checks `StopConnectedEntity` and `BackintMode` the same way. A declaration of `HANA` is moreover the one case the component builder has to special-case, because such an entry collides with the SID-derived HANA component — code that only exists because the handler admits a type the member documentation excludes.

## Evidence

Model (smithy_query(query="shape", shape="ComponentInfo")), member `ComponentType`:
```
"ComponentType": {"target": "ComponentType", "required": true, "type": "enum",
  "doc": "<p>This string is the type of the component.</p> <p>Accepted value is <code>WD</code>.</p>"}
```
and smithy_query(query="shape", shape="ComponentType") lists all eight values the shape admits, which is what the boundary validator enforces: `["ABAP","ASCS","DIALOG","ERS","HANA","HANA_NODE","WD","WEBDISP"]` (mirrored at services/service-com-amazonaws-ssmsap-ssmsap/src/ops/types/shared_types.rs:145).

The handler's only check on `ComponentsInfo` is its length — services/service-com-amazonaws-ssmsap-ssmsap/src/ops/register_application.rs:241-242:
```
crate::state::components_within_bounds(input.components_info.as_deref().unwrap_or_default())
    .map_err(RegisterApplicationErrorModeled::ValidationException)?;
```
and the declared type is stored verbatim — services/service-com-amazonaws-ssmsap-ssmsap/src/state.rs:2914-2924:
```
for component in components_info {
    let id = component_id(&component.sid, &component.component_type);
    ...
    components.push(StoredComponent {
        id,
        component_type: component.component_type,
```

The same handler DOES re-state a narrowed value set for another member — register_application.rs:211-220:
```
if !APPLICATION_TYPE_VALUES.contains(&input.application_type.as_str()) {
    return Err(RegisterApplicationErrorModeled::ValidationException(
        crate::state::validation(format!(
            "ApplicationType must be one of {}, but was {}",
```

Test coverage: the behaviour is exercised across the suite with the stated reason that the rule should generalise over the model's whole enum — tests/all/component_discovery.rs:428 ("RULE: the rule holds over the model's whole `ComponentType` catalogue, not the types ...") and tests/all/component_reads.rs:546 — e.g. tests/all/component_reads.rs:218-219 registers `("ASCS","PRD",...)`, tests/all/app_registry_arn.rs:118 registers `{"ComponentType": "HANA", ...}`. That stated reason is about the model's enum SHAPE; it is the member's own documentation sentence quoted above that narrows what this input accepts.

## Steps to Reproduce

1. POST /register-application with ApplicationId `wd-only`, ApplicationType `SAP_ABAP`, Instances `["i-0123456789abcdef0"]`, Sid `ABC`, DatabaseArn set, and `ComponentsInfo: [{"ComponentType": "ASCS", "Sid": "ABC", "Ec2InstanceId": "i-0123456789abcdef0"}]`.
2. Observe 200: the registration is accepted although `ComponentInfo$ComponentType` documents `WD` as the accepted value.
3. Wait past TRANSITION_SPAN, then POST /list-components with that ApplicationId: observe a ComponentSummary `ABC-ASCS` with ComponentType `ASCS`.
4. Repeat with `"ComponentType": "HANA"` and the application's own Sid: observe that the declaration takes over the SID-derived HANA component (state.rs:2908-2936), so the application's databases are reported under a component the registration placed on an arbitrary instance.
