// SPDX-License-Identifier: CC0-1.0

//! The PERMISSIONS a stack operation runs under: the identity policies of the stack's
//! service role, and the refusal a call those policies deny is answered with.
//!
//! ## The rule this module implements
//!
//! `CreateStackInput$RoleARN`: "The Amazon Resource Name (ARN) of an IAM role that
//! CloudFormation assumes to create the stack. CloudFormation uses the role's credentials
//! to make calls on your behalf." So the calls a deployment makes in the services that own
//! a stack's resources ([`super::provisioning`]) are not made with the caller's own
//! permissions but with that role's: a change whose API call the role is denied cannot be
//! carried out, and CloudFormation reports the resource it could not act on as failed with
//! that refusal — which is what parks a stack in `UPDATE_ROLLBACK_FAILED` when a rollback's
//! own calls are the denied ones, and what `ContinueUpdateRollback`'s `ResourcesToSkip` then
//! steps over.
//!
//! [`super::role_trust_refusal`] is the other half of the same reading, answered when the
//! request arrives: a role whose TRUST policy does not let CloudFormation assume it at all
//! refuses the operation outright. This module is about a role that CAN be assumed and what
//! its PERMISSIONS policies say about one call.
//!
//! ## What is refused, and what is not
//!
//! The decision is [`::service_common::iam_authorization`]'s — the tree's ONE evaluation of
//! IAM policy documents, so two services handed the same documents cannot answer
//! differently. For the calls a DEPLOYMENT makes in the services that own a stack's resources
//! ([`refusal`]) two answers refuse the call, each of them evidence that the role cannot make
//! it:
//!
//! * an EXPLICIT `Deny` of the call, which refuses whatever else is in force ("an explicit
//!   deny in any policy overrides any allows"); and
//! * IAM's DEFAULT DENY, where no readable document of the role allows the call — asked of the
//!   SERVICE the call is made to rather than of the one action: a role whose documents allow
//!   it no action of that service at all can make none of the calls that create, change or
//!   delete a resource there, so the resource fails with that service's own authorization
//!   refusal.
//!
//! Why the service and not the action: the calls a deployment makes are this crate's
//! ([`super::provisioner`]), not the resource provider AWS ships for the type, so the set of
//! actions a deployment of one resource touches is not the set the real provider needs a grant
//! for — refusing the call whose exact action no statement happens to name would refuse
//! deployments the real service performs. Which SERVICE the provider has to call is not an
//! approximation: a resource of a type lives in the service that owns it.
//!
//! Nothing else is judged: a stack with no `RoleARN` runs with the caller's own permissions
//! ("CloudFormation uses a temporary session that's generated from your user credentials"), an
//! ARN the IAM store holds no role for may belong to another account, a role carrying an
//! attachment whose document this emulator does not hold carries a grant that cannot be read,
//! a role carrying no readable document at all is one whose permissions this emulator cannot
//! establish, and a document it cannot read as a policy is evidence of nothing.
//!
//! A call CLOUDFORMATION ITSELF makes on a resource the TEMPLATE names is held to the default
//! deny over the ONE ACTION ([`refusal_without_a_grant`]): such a call is not this
//! crate's approximation of a provider's API calls but the documented call of the service
//! being emulated, taken on an ARN the template itself supplies, so the action a grant is
//! needed for is exactly the action AWS asks for — and a role that allows that action is the
//! only role the call can be made under. The one call of that kind is the invocation of the provider a
//! custom resource's `ServiceToken` names
//! ([`super::provisioning::custom_resource`]). The same unreadability rule holds there as
//! everywhere else in this module: a role carrying an attached policy whose document this
//! emulator does not hold carries a grant that cannot be read, so the call is not refused for
//! want of one.
//!
//! ## How a call reaches the decision
//!
//! The role is resolved by the OPERATION that drives a deployment, from the IAM store and
//! with no store of this service locked, and put in scope for the whole of that deployment
//! ([`acting_as`]); the provisioning walk puts the RESOURCE each call is about in scope
//! around that resource's own calls ([`on_resource`]). The in-process call helpers then ask
//! [`refusal`] before they send anything, so the one place a call is authorized is the one
//! place it is made, and a call made outside a deployment (a readiness read a client's poll
//! drives, say) is judged by nothing here.
//!
//! Both scopes are TASK-local: a deployment's calls run in the task serving that request, so
//! two requests deploying two stacks under two roles never see each other's.
//!
//! ## Bounds
//!
//! One role's documents per deployment, at most [`MAX_ROLE_DOCUMENTS`] of them — a role
//! carrying more is one this gate cannot read in full, so it decides nothing — cloned out of
//! the IAM store once per deployment and shared by reference for every call of it.

use std::sync::Arc;

use ::service_common::iam_authorization::{self as iam, Decision};

/// The error code the refusal of a denied provisioning call carries.
///
/// The refusal is CloudFormation's own — the call was never made, so no owning service
/// answered a code of its own, and the spellings those services use differ
/// (`AccessDeniedException`, `AuthorizationError`). What a client acts on is the CLASS the
/// refusal falls in, and this is the code that class is read off
/// ([`super::provisioning`]'s `HANDLER_ERROR_CLASSES`), which is what says whether the
/// failed resource is worth retrying.
pub const ACCESS_DENIED: &str = "AccessDenied";

/// The role session name CloudFormation's own calls are made under, which is what an
/// authorization refusal names the caller by.
///
/// `RoleARN`: "CloudFormation uses the role's credentials to make calls on your behalf" —
/// those calls are made as a SESSION of that role, so the principal a refusal is about is
/// `arn:<partition>:sts::<account>:assumed-role/<role name>/<this name>`.
const SESSION_NAME: &str = "AWSCloudFormation";

/// The most identity-policy documents this gate reads for one role.
///
/// A role carrying more is one whose permissions this gate cannot establish, so it decides
/// nothing for it rather than deciding on part of them. The documented default quota is 10
/// inline policies and 10 attached managed policies per role, so this is that pair.
const MAX_ROLE_DOCUMENTS: usize = 20;

/// The permissions in force on one stack's service role: the principal the role's calls are
/// made as, and the identity-policy documents that decide them.
///
/// [`Self::unspecified`] is a deployment with no role to judge — a stack that named none, an
/// engine with no IAM store, an ARN that account holds no role for — and it refuses nothing.
#[derive(Debug, Clone, Default)]
pub struct ServiceRole {
    /// The session principal a refusal names ([`SESSION_NAME`]), or `None` where there is no
    /// role.
    principal: Option<String>,
    /// The identity-policy documents in force on the role: its inline policies and the
    /// documents of the policies attached to it, as IAM's own store collects them.
    documents: Vec<String>,
    /// Set when the role carries an attached managed policy whose document this emulator
    /// holds nothing for — an entry of AWS's own catalog it does not carry, which IAM reports
    /// as uncarried. The GRANT such a policy would give cannot be read, so a call for want of
    /// a grant is not refused on it ([`Self::refusal_without_a_grant`]); an explicit `Deny` in
    /// a document that CAN be read still refuses.
    unreadable_attachment: bool,
}

impl ServiceRole {
    /// A deployment whose calls no service role governs.
    pub fn unspecified() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The permissions the role `role_arn` names carries, read from the IAM store in the
    /// same shared engine the account's roles live in.
    ///
    /// Call it with the handler's `&*self.engine` and NO store of this service locked: it
    /// takes the IAM guard, clones what it reads and releases it, so the deployment that
    /// follows holds neither lock.
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng, role_arn: Option<&str>) -> Arc<Self> {
        let Some(role_arn) = role_arn else {
            return Self::unspecified();
        };
        let Some(iam) = super::iam_state(engine) else {
            return Self::unspecified();
        };
        let store = iam.lock();
        // An ARN this account holds no role for says nothing about any call: the role may
        // belong to another account, or to a client that never told this emulator about it.
        let Some(role) = store.roles.values().find(|role| role.arn == role_arn) else {
            return Self::unspecified();
        };
        let role_name = role.role_name.clone();
        // The documents are IAM's OWN collection of what an identity carries — its inline
        // policies plus the documents of the policies attached to it — called rather than
        // re-derived, so this gate reads exactly what IAM itself judges that role on. An
        // attachment that collection resolved no document for is a grant this gate cannot
        // read, which it has to know about rather than read as the absence of a grant.
        let carried = store.principal_policies(role_arn).unwrap_or_default();
        drop(store);
        if carried.documents.len() > MAX_ROLE_DOCUMENTS {
            return Self::unspecified();
        }
        Arc::new(Self {
            principal: Some(session_principal(role_arn, &role_name)),
            documents: carried.documents,
            unreadable_attachment: !carried.uncarried.is_empty(),
        })
    }

    /// The sentence refusing a DEPLOYMENT's call of `operation` on the service `signing_name`,
    /// taken on `resource`, or `None` where the role's documents show nothing against that
    /// call.
    ///
    /// The two answers that refuse it are the module docs': an explicit `Deny` of the call, and
    /// IAM's default deny where no readable document of the role allows the role ANY action of
    /// the called service. A role carrying no readable document at all, and one carrying an
    /// attachment whose document this emulator does not hold, establish neither: refusing for
    /// want of a grant that cannot be read would refuse a call AWS permits.
    ///
    /// The wording is the one AWS answers an unauthorized call with — the principal, the
    /// action, the resource and why — every part of it read off the call and the account's own
    /// records, worded by the workspace's one authorization refusal
    /// ([`iam::not_authorized_by_identity_policy`]) so two services that reach the same
    /// decision cannot word it differently, and naming the action the way the service that
    /// refuses it words its own refusals ([`reported_action`]).
    fn refusal(&self, signing_name: &str, operation: &str, resource: &str) -> Option<String> {
        let principal = self.principal.as_deref()?;
        if self.documents.is_empty() {
            return None;
        }
        let action = reported_action(signing_name, operation);
        // Read once, because both questions below are asked of the same documents.
        let documents = iam::prepare_documents(&self.documents);
        let request = iam::Request::new(&action, resource, &[]);
        let reason = match iam::identity_decision_prepared(&documents, &request) {
            Decision::Allowed => return None,
            Decision::Denied => iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string(),
            Decision::NotStated => {
                if self.unreadable_attachment
                    || iam::identity_may_allow_an_action_of(&documents, signing_name)
                {
                    return None;
                }
                iam::no_identity_policy_allows(&action)
            }
        };
        Some(iam::not_authorized_by_identity_policy(
            principal, &action, resource, &reason,
        ))
    }

    /// The sentence refusing `action` on `resource` for want of a GRANT: the decision IAM
    /// itself makes, where a call no document allows is refused and an explicit `Deny`
    /// refuses it whatever else is in force.
    ///
    /// `None` where there is no role to judge — a deployment that named none, an ARN this
    /// account holds no role for — where the role's documents allow the call, and where the
    /// role carries a grant this emulator cannot read ([`Self::unreadable_attachment`]):
    /// refusing for want of a grant whose document is not held would refuse a call AWS
    /// permits, which is the reading [`super::calling_principal`] gives the same case. The
    /// reason clause names which of IAM's rules withheld the grant, as the refusal AWS words
    /// does.
    fn refusal_without_a_grant(&self, action: &str, resource: &str) -> Option<String> {
        let principal = self.principal.as_deref()?;
        let request = iam::Request::new(action, resource, &[]);
        let reason = match iam::identity_decision(&self.documents, &request) {
            Decision::Allowed => return None,
            Decision::Denied => iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string(),
            Decision::NotStated if self.unreadable_attachment => return None,
            Decision::NotStated => iam::no_identity_policy_allows(action),
        };
        Some(iam::not_authorized_by_identity_policy(
            principal, action, resource, &reason,
        ))
    }
}

/// The IAM action a call of `operation` on `signing_name` asks for, spelled as the service that
/// would refuse it names an action in its own refusal.
///
/// An action is `<service prefix>:<operation name>` ([`iam::iam_action`]), and the prefix is the
/// name the service signs under; the case is that service's own wording, which the workspace
/// states once ([`iam::reported_action`]) so this report of a call the service would have
/// refused and that service's own refusal of the same call cannot spell one action two ways.
fn reported_action(signing_name: &str, operation: &str) -> String {
    iam::reported_action(&iam::iam_action(signing_name, operation))
}

/// The principal a session of `role_arn` is: the partition and the account are the role's
/// own, read off its ARN through the ONE ARN parse this workspace has, so a role of another
/// account is named as that role's session rather than as this emulator's.
fn session_principal(role_arn: &str, role_name: &str) -> String {
    let parsed = ::service_common::arn::parse(role_arn);
    let partition = parsed.map_or("aws", |arn| arn.partition);
    let account = parsed.map_or("", |arn| arn.account);
    format!("arn:{partition}:sts::{account}:assumed-role/{role_name}/{SESSION_NAME}")
}

tokio::task_local! {
    /// The service role the deployment running in this task makes its calls under, set by
    /// [`acting_as`] around the whole of that deployment.
    static ROLE: Arc<ServiceRole>;
    /// The resource the calls running inside this scope are about, set by [`on_resource`]
    /// around one resource's own calls: a policy statement's `Resource` element is what it
    /// is matched against.
    static TARGET: String;
}

/// Run `deployment` with `role`'s permissions in force for every call it makes.
pub async fn acting_as<F: ::std::future::Future>(role: Arc<ServiceRole>, deployment: F) -> F::Output {
    ROLE.scope(role, deployment).await
}

/// Run `calls` — the calls that create, change or delete ONE resource — with that resource
/// as what they are authorized against.
pub async fn on_resource<F: ::std::future::Future>(resource: String, calls: F) -> F::Output {
    TARGET.scope(resource, calls).await
}

/// The refusal the service role in force answers a call of `operation` on `signing_name`
/// with, or `None` where nothing refuses it.
///
/// `signing_name` is the service the call is signed for and `operation` the API operation it
/// invokes, which together are the IAM action it asks for: an action is
/// `<service prefix>:<operation name>`, and the prefix is the name the service signs under.
/// A call made outside a deployment, or about no resource, is judged by nothing: there is no
/// role in scope to judge it on.
pub fn refusal(signing_name: &str, operation: &str) -> Option<String> {
    let role = ROLE.try_with(Arc::clone).ok()?;
    let resource = TARGET.try_with(String::clone).ok()?;
    role.refusal(signing_name, operation, &resource)
}

/// The refusal the service role in force answers a call of `action` on `resource` with when
/// its documents do not ALLOW it, or `None` where they do — or where there is no role to judge
/// the call on.
///
/// For a call CloudFormation itself makes on a resource the template names, where the action
/// asked for is the documented one of the service being emulated and the ARN is the
/// template's own: there IAM's default deny holds, so the role has to allow the call (see the
/// module docs). `action` is the fully qualified IAM action and `resource` the ARN the call
/// addresses; a call made outside a deployment is judged by nothing, as it is for [`refusal`].
pub fn refusal_without_a_grant(action: &str, resource: &str) -> Option<String> {
    let role = ROLE.try_with(Arc::clone).ok()?;
    role.refusal_without_a_grant(action, resource)
}

/// What a policy statement's `Resource` element is matched against for the calls about
/// `resource`: the ARN the resource carries, which is what a statement names it by, and
/// otherwise the identifier its owning service knows it by.
///
/// Every resource carries an ARN: either its physical id IS one ("Ref returns the Amazon
/// Resource Name (ARN)", for the types documented that way) or one of its `…Arn` return
/// values is, and a type that publishes neither still carries the conventional one
/// [`super::template::provision`] computes for it from the stack's identity and the
/// resource's name. The physical id is the fallback for a record carrying no ARN at all,
/// which is a resource no statement scoped to an ARN can be about either.
pub fn target_of(resource: &super::StoredResource) -> String {
    if resource.physical_resource_id.starts_with("arn:") {
        return resource.physical_resource_id.clone();
    }
    resource
        .attributes
        .iter()
        .find(|(attribute, value)| attribute.ends_with("Arn") && value.starts_with("arn:"))
        .map(|(_, arn)| arn.clone())
        .unwrap_or_else(|| resource.physical_resource_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ARN a provider-invocation decision is taken on, and the action it performs.
    const PROVIDER: &str = "arn:aws:lambda:us-east-1:000000000000:function:provider";
    const INVOKE: &str = "lambda:InvokeFunction";

    /// A role whose session is named and whose documents are `documents`.
    fn role(documents: &[String], unreadable_attachment: bool) -> ServiceRole {
        ServiceRole {
            principal: Some(session_principal(
                "arn:aws:iam::000000000000:role/deployer",
                "deployer",
            )),
            documents: documents.to_vec(),
            unreadable_attachment,
        }
    }

    /// One statement document, as IAM's own grammar writes it.
    fn document(effect: &str, action: &str, resource: &str) -> String {
        ::serde_json::json!({
            "Version": "2012-10-17",
            "Statement": [{ "Effect": effect, "Action": action, "Resource": resource }],
        })
        .to_string()
    }

    /// The same document with the action element written as the `NotAction` the grammar also
    /// admits: the statement is about every action EXCEPT the one its value names.
    fn excluding(effect: &str, not_action: &str, resource: &str) -> String {
        ::serde_json::json!({
            "Version": "2012-10-17",
            "Statement": [{ "Effect": effect, "NotAction": not_action, "Resource": resource }],
        })
        .to_string()
    }

    /// RULE: a call CloudFormation itself makes on a template-named resource is decided under
    /// IAM's default deny — allowed only where a readable document allows it, refused with the
    /// clause naming the rule that withheld it, and decided by nothing where there is no role
    /// or where a grant this emulator cannot read may be the one that allows it.
    ///
    /// The rows span what the decision turns on: whether a statement is about the call at all,
    /// how its action and resource are spelled (named in full, a wildcard, a pattern, another
    /// resource's ARN), the effect it states, and whether the role carries an attachment whose
    /// document is not held.
    #[test]
    fn a_call_needing_a_grant_is_refused_unless_a_readable_document_allows_it() {
        // (case, the role, the expected reason clause or None for a call that is not refused)
        let rows: Vec<(&str, ServiceRole, Option<String>)> = vec![
            (
                "an allow of the action on the provider's own ARN",
                role(&[document("Allow", INVOKE, PROVIDER)], false),
                None,
            ),
            (
                "an allow of every action on every resource",
                role(&[document("Allow", "*", "*")], false),
                None,
            ),
            (
                "an allow whose action and resource are patterns the call matches",
                role(
                    &[document(
                        "Allow",
                        "lambda:Invoke*",
                        "arn:aws:lambda:us-east-1:000000000000:function:*",
                    )],
                    false,
                ),
                None,
            ),
            (
                "no statement about the call at all",
                role(&[document("Allow", "sns:Publish", "*")], false),
                Some(iam::no_identity_policy_allows(INVOKE)),
            ),
            (
                "an allow of the action on ANOTHER resource",
                role(
                    &[document(
                        "Allow",
                        INVOKE,
                        "arn:aws:lambda:us-east-1:000000000000:function:someone-else",
                    )],
                    false,
                ),
                Some(iam::no_identity_policy_allows(INVOKE)),
            ),
            (
                "no documents at all",
                role(&[], false),
                Some(iam::no_identity_policy_allows(INVOKE)),
            ),
            (
                "an allow and an explicit deny of the same call",
                role(
                    &[
                        document("Allow", "*", "*"),
                        document("Deny", INVOKE, PROVIDER),
                    ],
                    false,
                ),
                Some(iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string()),
            ),
            // A grant this emulator cannot read may be the one that allows the call, so the
            // absence of a readable grant is no evidence against it.
            (
                "an attachment whose document is not held",
                role(&[], true),
                None,
            ),
            // ...but an explicit deny in a document that CAN be read still refuses.
            (
                "an explicit deny beside an attachment whose document is not held",
                role(&[document("Deny", INVOKE, "*")], true),
                Some(iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string()),
            ),
            (
                "no role to judge the call on",
                ServiceRole::default(),
                None,
            ),
        ];
        for (case, judged, clause) in rows {
            let expected = clause.map(|clause| {
                iam::not_authorized_by_identity_policy(
                    judged.principal.clone().unwrap_or_default().as_str(),
                    INVOKE,
                    PROVIDER,
                    &clause,
                )
            });
            let actual = judged.refusal_without_a_grant(INVOKE, PROVIDER);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The resource one DEPLOYMENT call of the rows below is about, and the call itself: the
    /// service it is made to and the operation it invokes.
    const QUEUE: &str = "arn:aws:sqs:us-east-1:000000000000:orders";
    const QUEUE_SERVICE: &str = "sqs";
    const CREATE_QUEUE: &str = "CreateQueue";

    /// RULE: a DEPLOYMENT's call is refused by an explicit `Deny` about it, and — under IAM's
    /// default deny — where no readable document allows the role any action of the service the
    /// call is made to; a role allowed some action of that service is refused nothing, whichever
    /// action and whichever resource that allow is scoped to, and so are a role carrying a grant
    /// this emulator cannot read, a role carrying no readable document at all and a deployment
    /// with no role.
    ///
    /// The rows span what the decision turns on: whether a statement is about the called service
    /// at all, how its action element is spelled (the call's own action, another action of that
    /// service, a wildcard over it, every action, a `NotAction` excluding part of it and one
    /// excluding all of it), what its resource element names, the effect it states, and what the
    /// role carries beside its readable documents. The refusal's own wording is held to as well,
    /// action included: the case is the refusing service's ([`reported_action`]).
    #[test]
    fn a_deployment_call_is_refused_unless_the_role_may_act_in_the_called_service() {
        let action = reported_action(QUEUE_SERVICE, CREATE_QUEUE);
        // (case, the role, the expected reason clause or None for a call that is not refused)
        let rows: Vec<(&str, ServiceRole, Option<String>)> = vec![
            (
                "an allow of the call's own action on the resource",
                role(&[document("Allow", "sqs:CreateQueue", QUEUE)], false),
                None,
            ),
            (
                "an allow of ANOTHER action of the called service",
                role(&[document("Allow", "sqs:DeleteQueue", "*")], false),
                None,
            ),
            (
                "an allow of the call's action on another resource",
                role(
                    &[document(
                        "Allow",
                        "sqs:CreateQueue",
                        "arn:aws:sqs:us-east-1:000000000000:someone-else",
                    )],
                    false,
                ),
                None,
            ),
            (
                "an allow under a wildcard over the called service",
                role(&[document("Allow", "sqs:*", "*")], false),
                None,
            ),
            (
                "an allow of every action on every resource",
                role(&[document("Allow", "*", "*")], false),
                None,
            ),
            (
                "every action of ANOTHER service and none of this one",
                role(&[document("Allow", "ssm:*", "*")], false),
                Some(iam::no_identity_policy_allows(&action)),
            ),
            (
                "an allow of one unrelated action",
                role(&[document("Allow", "s3:GetObject", "*")], false),
                Some(iam::no_identity_policy_allows(&action)),
            ),
            (
                "an allow of every action but ANOTHER action of the called service",
                role(&[excluding("Allow", "sqs:DeleteQueue", "*")], false),
                None,
            ),
            (
                "an allow of every action but the called service's own",
                role(&[excluding("Allow", "sqs:*", "*")], false),
                Some(iam::no_identity_policy_allows(&action)),
            ),
            (
                "an allow of everything beside an explicit deny of the call",
                role(
                    &[
                        document("Allow", "*", "*"),
                        document("Deny", "sqs:CreateQueue", QUEUE),
                    ],
                    false,
                ),
                Some(iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string()),
            ),
            // A grant this emulator cannot read may be the one that allows the call.
            (
                "an attachment whose document is not held",
                role(&[document("Allow", "ssm:*", "*")], true),
                None,
            ),
            // ...and a role whose permissions this emulator holds nothing of establishes
            // nothing about one call.
            ("no readable document at all", role(&[], false), None),
            ("no role to judge the call on", ServiceRole::default(), None),
        ];
        for (case, judged, clause) in rows {
            let expected = clause.map(|clause| {
                iam::not_authorized_by_identity_policy(
                    judged.principal.clone().unwrap_or_default().as_str(),
                    &action,
                    QUEUE,
                    &clause,
                )
            });
            let actual = judged.refusal(QUEUE_SERVICE, CREATE_QUEUE, QUEUE);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
