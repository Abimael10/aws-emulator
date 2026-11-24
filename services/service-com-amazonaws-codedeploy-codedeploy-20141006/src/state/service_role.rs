// SPDX-License-Identifier: CC0-1.0

//! The SERVICE ROLE a deployment group runs under, resolved against the account's
//! own IAM records.
//!
//! `serviceRoleArn` is "a service role Amazon Resource Name (ARN) that allows
//! CodeDeploy to act on the user's behalf when interacting with Amazon Web
//! Services services"
//! (`smithy_query(query="doc",shape="CreateDeploymentGroupInput",member="serviceRoleArn")`),
//! so whether a deployment group can be created under it at all is a question
//! about a role in IAM: the store those roles live in ([`iam_state`]), whether the
//! account holds the role the ARN names, the principal that role has to trust
//! ([`SERVICE_PRINCIPAL`]) and the reading of its trust policy
//! ([`unassumable_role_message`]).
//!
//! Every path below is absolute, so this module imports nothing from its parent.

/// The IAM store a deployment group resolves its `serviceRoleArn` against.
///
/// It is the IAM service's OWN state struct, reached through the SAME shared
/// engine this service's store lives in, so CodeDeploy reads the very records
/// `CreateRole` wrote rather than a second copy of them.
pub type IamState =
    ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ComAmazonawsIamAwsidentitymanagementv20100508State;

/// The service principal CodeDeploy assumes a deployment group's service role as.
///
/// A role CodeDeploy is to act as has to trust this principal for
/// `sts:AssumeRole`; the CodeDeploy User Guide's service-role step ("Create a
/// service role for CodeDeploy") gives exactly that trust relationship.
pub const SERVICE_PRINCIPAL: &str = "codedeploy.amazonaws.com";

/// The IAM store in the SAME shared engine, which is where the account's roles
/// live.
///
/// This is the IAM service's OWN accessor, called rather than copied, so
/// CodeDeploy reads the very store that service's handlers write. `None` has the
/// same meaning as for [`crate::state::service_state`]: the engine carries no
/// such store.
pub fn iam_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<IamState>> {
    ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state::service_state(engine)
}

/// `None` when this emulator has no reason to think CodeDeploy could not assume
/// `role_arn`; otherwise the `InvalidRoleException` message that refuses the call.
///
/// A deployment group acts as the role it names, so the role is resolved up front
/// — against the account's IAM records, under the NAME and the PATH the ARN
/// carries, the same records `CreateRole` wrote — and the call is refused when this
/// account holds no such role AT ALL, or holds one whose trust policy does not let
/// [`SERVICE_PRINCIPAL`] assume it: neither can be assumed, so neither could ever
/// deploy. An ARN naming ANOTHER account's role is no evidence either way (that
/// account's IAM is not this emulator's to ask), and so is a trust document this
/// emulator cannot read as a policy, so those go ahead.
///
/// Which of the two readings of "IAM holds no such role" a service takes belongs to
/// that service's recorded behaviour, and the shared module states both:
/// `service_common::iam_role::role_is_unassumable` refuses an absent role and
/// `…::held_role_is_unassumable` serves it. CodeDeploy is recorded refusing a
/// deployment group whose `serviceRoleArn` names a role the account does not hold,
/// so the question here is the FIRST of them.
///
/// This is the STORE-READING form of it, the one
/// `service-com-amazonaws-cloudformation-cloudformation`'s
/// `state::service_role_trust` takes: the records are reached through the shared
/// engine and the verdict is answered WITHOUT an await, because it is also needed
/// from the synchronous pre-dispatch seam (`crate::service::role_precedence`),
/// which has no await to spend. Which role of this account the ARN names, and the
/// evaluation of the trust document, are the shared readings either form ends in.
///
/// Call it with the handler's `&*self.engine` and no store lock held.
pub fn unassumable_role_message<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arn: &str,
) -> Option<String> {
    // WHICH role of this account the value names, read through the crate's one
    // reading of an IAM entity ARN: a value naming another account's role, another
    // vendor's resource or no role at all resolves to nothing, and the name and
    // the path are the two halves IAM holds a role under.
    let role_name = ::service_common::iam_role::role_name(role_arn)?;
    let role_path = ::service_common::iam_role::role_path(role_arn)?;
    let iam = iam_state(engine)?;
    let store = iam.lock();
    // IAM holds its roles under the case-folded name, through its own key
    // function, so the lookup here is the one that service's reads make — and the
    // record it finds has to be the one the ARN names, path included: a role IAM
    // holds at another path is not the role this value named.
    let held = store
        .roles
        .get(
            &::service_com_amazonaws_iam_awsidentitymanagementv20100508::state::name_key(role_name),
        )
        .filter(|role| role.path == role_path);
    let Some(role) = held else {
        return Some(cannot_assume_role_message(role_arn));
    };
    let document = role.assume_role_policy_document.as_deref()?;
    // The rule itself is IAM's, not this service's, so it is read through the ONE
    // evaluator every service that resolves a caller-supplied service role shares:
    // the statement must name the principal and `sts:AssumeRole`, an explicit
    // `Deny` wins over any `Allow`, and the document is percent-decoded before it
    // is parsed. A document this emulator cannot read as a policy at all is no
    // evidence either way.
    match ::service_common::iam_trust_policy::admits_service_principal(document, SERVICE_PRINCIPAL) {
        None | Some(true) => None,
        Some(false) => Some(cannot_assume_role_message(role_arn)),
    }
}

/// `InvalidRoleException` (HTTP 400) as CodeDeploy words a role it cannot assume:
/// the sentence names the role the REQUEST asked for, which is the one thing the
/// caller has to change.
///
/// One sentence for the one refusal: a role this account does not hold and a role
/// it holds that withholds the assume are the same answer to the same question —
/// CodeDeploy cannot take the role on — and the caller's fix is the same.
///
/// The ARN is stated whole rather than bounded the way a malformed value is
/// ([`crate::state::invalid_role_message`]): this refusal is only ever reached for
/// a value that already read as an IAM role ARN of this account, whose role name
/// IAM itself bounds.
pub fn cannot_assume_role_message(role_arn: &str) -> String {
    format!(
        "AWS CodeDeploy does not have the permissions required to assume the role {role_arn}."
    )
}
