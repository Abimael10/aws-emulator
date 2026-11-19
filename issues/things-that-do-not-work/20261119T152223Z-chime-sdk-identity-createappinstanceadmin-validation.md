Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: e5b74111f4def785a5d4295c90400d18c8715b4a

# CreateAppInstanceAdmin promotes any ARN under the app instance, including ARNs that name neither an AppInstanceUser nor an AppInstanceBot

_Filed: 2026-11-19T15:22:23Z_
_Operation: CreateAppInstanceAdmin | Class: validation_

> _Note: this issue was identified against the code base at commit `e5b74111f4def785a5d4295c90400d18c8715b4a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The model states that only the two identity kinds may be promoted. The handler enforces something weaker: it only checks that `AppInstanceAdminArn` starts with `<AppInstanceArn>/`, and only requires the target to EXIST when the ARN is in the `/user/` space. Any other child segment — including Chime SDK Messaging's `<AppInstanceArn>/channel/<id>`, which this crate's own state module names as a SIBLING's resource — passes the check, is recorded in the shared app-instance admin directory, and is afterwards reported by `DescribeAppInstanceAdmin` (200) and `ListAppInstanceAdmins` as an administrator of the app instance. The crate already owns the predicate that answers the question correctly (`crate::state::names_identity_resource`, which admits only `app-instance/<id>/user/<id>` and `app-instance/<id>/bot/<id>`), and uses it in `taggable`, but this handler does not call it. This is about ARNs whose child segment is neither `user` nor `bot`; the `"{instance}/bot/helper"` row that tests/all/app_instance_admin.rs:76-83 pins satisfies `names_identity_resource` and is untouched by this finding.

## Evidence

Smithy (query=doc, shape=CreateAppInstanceAdmin): "<p>Only an <code>AppInstanceUser</code> and <code>AppInstanceBot</code> can be promoted to an <code>AppInstanceAdmin</code>\n         role.</p>"

The handler's whole shape check — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/create_app_instance_admin.rs:179-202:
```
        // An administrator is an identity OF the app instance it administers, so the
        // promoted ARN has to be one of its children (`<AppInstanceArn>/user/<id>` for an
        // `AppInstanceUser`, `/bot/<id>` for an `AppInstanceBot`).
        let child_prefix = format!("{}/", input.app_instance_arn);
...
            if !input.app_instance_admin_arn.starts_with(&child_prefix) {
                return Err(CreateAppInstanceAdminErrorModeled::BadRequestException(
```
The existence check that follows applies ONLY to the `/user/` space, so every other child path falls through to the `None` arm and is promoted — create_app_instance_admin.rs:203-232:
```
            match store.user(&input.app_instance_admin_arn) {
                Some(user) => Some(user.name.clone()),
                None if input.app_instance_admin_arn.starts_with(&format!(
                    "{}/user/",
                    input.app_instance_arn
                )) =>
                {
                    return Err(unresolved(
...
                None => store.identity_name(&input.app_instance_admin_arn),
            }
...
        directory.lock().promote(
            &input.app_instance_arn,
            &input.app_instance_admin_arn,
            crate::state::now_millis(),
        );
```

The helper this crate already has for exactly this question, which the handler skips — src/state.rs:752-773:
```
/// Whether `arn` names a resource THIS service mints, so that this service's store is
/// what says whether it exists.
...
/// `…/user/<id>` ([`app_instance_user_arn`]) and `…/bot/<id>`. A sibling's resource hangs
/// off the same app instance under a segment of its own (Chime SDK Messaging's
/// `…/channel/<id>`), and every other family member's resources carry a different resource
/// type altogether, so neither reads as this service's.
pub fn names_identity_resource(arn: &str) -> bool {
...
        ["app-instance", id, child, child_id] => {
            !id.is_empty() && !child_id.is_empty() && matches!(*child, "user" | "bot")
        }
```
and it IS used by a sibling path in the same crate — src/state.rs:584-589:
```
    pub fn taggable(&self, arn: &str) -> bool {
        ::service_common::chime_tags::addresses_taggable_resource(
            arn,
            names_identity_resource(arn).then(|| self.resource_exists(arn)),
        )
```

Test coverage: tests/all/app_instance_admin.rs:67-112 tables five rows (a stored user, `"{instance}/bot/helper"`, `"{instance}/user/nobody"`, an identity of another app instance, an absent app instance). No row uses a child segment that is neither `user` nor `bot`, so nothing pins the current acceptance.

## Steps to Reproduce

1. POST /app-instances {Name, ClientRequestToken} -> AppInstanceArn A
2. POST /app-instances/{A}/admins with body {"AppInstanceAdminArn": "<A>/channel/abc"} (a Chime SDK Messaging channel ARN under the same app instance, which names no AppInstanceUser and no AppInstanceBot)
3. Observe 201 with AppInstanceAdmin.Arn = "<A>/channel/abc" instead of a refusal
4. GET /app-instances/{A}/admins/{<A>/channel/abc} -> 200: the non-identity ARN is now readable as an administrator
5. GET /app-instances/{A}/admins -> the same ARN is listed among the app instance's administrators
