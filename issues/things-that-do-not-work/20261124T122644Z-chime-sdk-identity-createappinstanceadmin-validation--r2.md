Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: df478bb1f06c590288aa633b226b8e001caeea1c

# CreateAppInstanceAdmin promotes a /bot/<id> ARN that names no AppInstanceBot this service holds

_Filed: 2026-11-24T12:26:44Z_
_Operation: CreateAppInstanceAdmin | Class: validation_

> _Note: this issue was identified against the code base at commit `df478bb1f06c590288aa633b226b8e001caeea1c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The handler requires the promoted ARN to be shaped like an identity of the named AppInstance, and it requires an `.../user/<id>` ARN to resolve to a stored user — but an `.../bot/<id>` ARN is recorded as an administrator without any existence check, so promoting a bot ARN that was never created (or that has since been deleted) answers 201 and records a grant. The model says only an AppInstanceUser or AppInstanceBot may be promoted; an ARN this service holds no bot for names neither. The branch's premise is stale: it was written for a service that kept no bot records, while this crate stores bots (`insert_bot`, `bots_by_arn`, `bot()`) and serves CreateAppInstanceBot / DescribeAppInstanceBot, so the same one-hop lookup the user branch uses is available for bots.

## Evidence

Model, CreateAppInstanceAdmin documentation (smithy_query query="doc" shape="CreateAppInstanceAdmin"): "Only an <code>AppInstanceUser</code> and <code>AppInstanceBot</code> can be promoted to an <code>AppInstanceAdmin</code> role."

The handler's asymmetric branch — services/.../src/ops/create_app_instance_admin.rs:210-229:
```
            match store.user(&input.app_instance_admin_arn) {
                Some(user) => Some(user.name.clone()),
                // An `AppInstanceUser` has to exist to be promoted; an `AppInstanceBot`
                // this service holds a record of reports the name on its own record, and one it does
                // not hold is recorded without a name rather than refused.
                None if input.app_instance_admin_arn.starts_with(&format!(
                    "{}/user/",
                    input.app_instance_arn
                )) =>
                {
                    return Err(unresolved(...));
                }
                None => store.identity_name(&input.app_instance_admin_arn),
            }
```
(the `None =>` arm records the promotion for any bot-shaped ARN).

The crate DOES hold bot records, so "a record this service does not keep" is no longer true — services/.../src/state.rs:331: `pub bots_by_arn: BTreeMap<String, String>,` and state.rs:431-433:
```
    pub fn bot(&self, bot_arn: &str) -> Option<&AppInstanceBotRecord> {
        let instance_arn = self.bots_by_arn.get(bot_arn)?;
```
with DescribeAppInstanceBot resolving through it (describe_app_instance_bot.rs:168: `let Some(record) = state.bot(&input.app_instance_bot_arn) else {`).

Test coverage: tests/all/app_instance_admin.rs:81-88 pins the current 201 for "an AppInstanceBot of the app instance, whose record this service does not keep", and tests/all/app_instance_admin_demotion.rs:130-137 repeats that premise — the stated reason is the one the crate's own bot store contradicts, while the neighbouring row "an AppInstanceUser the service does not hold" (app_instance_admin.rs:89-96) expects 400 `NotFound`.

## Steps to Reproduce

1. CreateAppInstance -> AppInstanceArn I
2. CreateAppInstanceAdmin(AppInstanceArn=I, AppInstanceAdminArn="<I>/bot/never-created")
3. Observed: 201, and DescribeAppInstanceAdmin(I, "<I>/bot/never-created") answers 200 for a bot that was never created
4. Contrast: CreateAppInstanceAdmin(AppInstanceArn=I, AppInstanceAdminArn="<I>/user/nobody") answers 400 with Code NotFound
5. Contrast: CreateAppInstanceBot under I, DeleteAppInstanceBot, then promote the dead bot ARN — still 201
