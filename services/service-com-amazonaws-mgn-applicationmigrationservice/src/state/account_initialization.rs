// SPDX-License-Identifier: CC0-1.0

//! What initializing an account for this service ESTABLISHES, and how an attempt to
//! establish it concludes.
//!
//! Initializing an account is not a flag this service sets for itself: "Initialize
//! Application Migration Service" (the operation's own documentation) is the creation of
//! the IAM resources the service's own machinery runs under — the service-linked role it
//! calls other Amazon Web Services under, and the EC2 instance profiles its replication
//! and conversion servers run under. So `InitializeService` ATTEMPTS that creation and
//! concludes from what IAM answers: the account is recorded as initialized only once IAM
//! reports every one of those resources, and an attempt IAM refuses leaves the account
//! uninitialized and is answered with the modelled `ValidationException` (HTTP 400,
//! `reason` [`super::VALIDATION_OTHER`]) whose sentence names which of them could not be
//! created.
//!
//! IAM is the OWNER of those resources, so this module keeps none of them: it asks IAM
//! for them the way any emulated service reaches another, over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), and every decision below is
//! made on what IAM ANSWERED to a read of its own records — never on what a create here
//! reported, because a resource reported as created for another component exists only if
//! that component returns it from its own read. A create's own refusal is therefore only
//! logged: what decides is the read that follows it. Nothing is cached, so a role deleted
//! out from under the account stops being established at once and the next initialization
//! re-establishes it.
//!
//! A hop that could not be MADE at all — an emulator with no IAM mounted, an answer this
//! emulator could not read — is this service failing to ask rather than IAM reporting that
//! the account holds nothing, so it concludes no failure: the initialization proceeds, as
//! it did before anything was asked. That reading is the tree's
//! (`service-com-amazonaws-pcs-awsparallelcomputingservice/src/state/ec2_subnets.rs` states
//! it for the same situation), and it is what keeps a refusal grounded in an answer.

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;

use super::{ValidationException, VALIDATION_OTHER};

/// The service principal this service acts under, which its service-linked role is linked
/// to: the service's endpoint prefix (`smithy_query(query="service_info")` →
/// `awsService.endpointPrefix` / `arnNamespace`, `mgn`) under the service-principal domain
/// every AWS service principal is named in.
pub const SERVICE_PRINCIPAL: &str = "mgn.amazonaws.com";

/// The instance profiles an initialized account holds, by name.
///
/// Initialization brings into the account the EC2 instance profiles the service's own
/// servers run under: one for the REPLICATION servers it keeps in the staging area, one
/// for the CONVERSION servers it launches for a cutover. Source: the Application Migration
/// Service user guide's initialization and IAM pages, which name the roles the service
/// creates for those two server kinds; the account's instance profile for each carries the
/// role's name.
///
/// A name here is both the role's and the instance profile's: an instance profile "holds"
/// the role an instance assumes through it, and AWS gives the pair one name.
pub const SERVER_INSTANCE_PROFILES: &[&str] = &[
    "AWSApplicationMigrationReplicationServerRole",
    "AWSApplicationMigrationConversionServerRole",
];

/// The service principal that assumes a server role through its instance profile: an
/// instance profile is only usable by an EC2 instance, so the role it holds has to trust
/// the EC2 service principal, or nothing the account launches can assume it.
const SERVER_TRUST_PRINCIPAL: &str = "ec2.amazonaws.com";

/// How the service words an initialization that could not establish what it needs: the
/// fixed text of the sentence, whose variable part is WHICH resources failed.
const FAILED_TO_CREATE: &str = "Failed to create";

/// The service-linked role, as that sentence names it.
const SERVICE_LINKED_ROLE_LABEL: &str = "SLR";

/// The instance profiles, as that sentence names them.
const INSTANCE_PROFILES_LABEL: &str = "instance profiles";

/// What an attempt to establish one kind of resource concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Establishment {
    /// IAM reports the resource: the account holds it.
    Established,
    /// IAM answered that the account does not hold it, and still did not after the
    /// attempt to create it.
    Failed,
    /// The question could not be put to IAM at all, so nothing is known either way (see
    /// the module docs). Never a refusal.
    Unknown,
}

impl Establishment {
    /// The weaker of two conclusions about one KIND of resource: a failure anywhere in a
    /// kind fails the kind, and an unanswered question anywhere leaves it unknown rather
    /// than established.
    fn weaker(self, other: Self) -> Self {
        match (self, other) {
            (Self::Failed, _) | (_, Self::Failed) => Self::Failed,
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            _ => Self::Established,
        }
    }
}

/// Which of the resources an initialization attempt needed it could not establish.
///
/// [`Self::refusal`] is the modelled answer for one that failed; a default value (nothing
/// failed) refuses nothing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct InitializationFailure {
    /// The account's service-linked role could not be established.
    pub service_linked_role: bool,
    /// At least one of [`SERVER_INSTANCE_PROFILES`] could not be established.
    pub instance_profiles: bool,
}

impl InitializationFailure {
    /// The sentence this failure is worded with, or `None` when nothing failed.
    ///
    /// The fixed text is the whole sentence; the only variable part is which resources
    /// failed, which is read off this value rather than off any one run.
    #[must_use]
    pub fn message(&self) -> Option<String> {
        match (self.service_linked_role, self.instance_profiles) {
            (true, true) => Some(format!(
                "{FAILED_TO_CREATE} {SERVICE_LINKED_ROLE_LABEL} or {INSTANCE_PROFILES_LABEL}"
            )),
            (true, false) => Some(format!("{FAILED_TO_CREATE} {SERVICE_LINKED_ROLE_LABEL}")),
            (false, true) => Some(format!("{FAILED_TO_CREATE} {INSTANCE_PROFILES_LABEL}")),
            (false, false) => None,
        }
    }

    /// The modelled `ValidationException` an initialization that failed is answered with,
    /// or `None` when it did not fail.
    ///
    /// `reason` is [`VALIDATION_OTHER`]: the failure is about neither an unknown operation,
    /// nor a body that could not be parsed, nor a member of the request — the request
    /// carries none — so it is the remaining `ValidationExceptionReason` the model
    /// declares, and there is no `fieldList` to report for a failure no field caused.
    #[must_use]
    pub fn refusal(&self) -> Option<ValidationException> {
        Some(ValidationException {
            message: Some(self.message()?),
            code: None,
            reason: Some(VALIDATION_OTHER.to_string()),
            field_list: None,
        })
    }
}

/// Establish what an initialized account holds, and answer which of it could not be
/// established.
///
/// Call it with this service's store NOT held: it asks IAM, which serves that question on
/// this same emulator.
pub async fn establish_account_initialization(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> InitializationFailure {
    // The Region a request signed for is caller-supplied text, and the client below puts
    // it in an endpoint URL's AUTHORITY, so it is read as a Region LABEL (the tree's one
    // reading of that grammar) rather than interpolated as given: a value carrying a host
    // or path delimiter would name an authority the caller chose. IAM is partition-global,
    // so a value that is no label at all leaves the question to be put in this emulator's
    // default Region rather than left unasked — what an initialization establishes does
    // not depend on which Region it was asked for.
    let label = ::service_common::arn::region_label(region).unwrap_or(super::DEFAULT_REGION);
    let client = ::service_common::iam_client::iam_client(ctx, label);

    let service_linked_role = establish_service_linked_role(&client).await;
    let mut instance_profiles = Establishment::Established;
    for name in SERVER_INSTANCE_PROFILES {
        instance_profiles = instance_profiles.weaker(establish_instance_profile(&client, name).await);
    }

    InitializationFailure {
        service_linked_role: service_linked_role == Establishment::Failed,
        instance_profiles: instance_profiles == Establishment::Failed,
    }
}

/// Establish the account's service-linked role for this service: ask IAM for it, create it
/// where IAM reports none, and conclude from a second read.
async fn establish_service_linked_role(client: &::aws_sdk_iam::Client) -> Establishment {
    match reported_service_linked_role(client).await {
        Some(true) => return Establishment::Established,
        None => return Establishment::Unknown,
        Some(false) => {}
    }
    // The role's NAME is IAM's to pick (it publishes one per service principal), so the
    // create names only the principal and the read below names only the path.
    if let Err(error) = client
        .create_service_linked_role()
        .aws_service_name(SERVICE_PRINCIPAL)
        .send()
        .await
    {
        ::tracing::warn!(
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the service-linked role an account initialization needs was not created"
        );
    }
    match reported_service_linked_role(client).await {
        Some(true) => Establishment::Established,
        Some(false) => Establishment::Failed,
        None => Establishment::Unknown,
    }
}

/// Whether IAM reports a role of this account under the path it files THIS service's
/// service-linked role under.
///
/// `None` when the question could not be put at all. The path comes from IAM's own
/// spelling of a service-linked role's path, so the role this looks for is the role IAM
/// itself would file rather than a second spelling of it — and the question needs no role
/// name, which is IAM's to pick.
async fn reported_service_linked_role(client: &::aws_sdk_iam::Client) -> Option<bool> {
    let path = iam::service_linked_role_path(SERVICE_PRINCIPAL);
    match client.list_roles().path_prefix(path).send().await {
        Ok(reported) => Some(!reported.roles().is_empty()),
        Err(error) => {
            // At `warn`: a deployment where this hop fails is one where an initialization
            // establishes nothing, which is worth seeing.
            ::tracing::warn!(
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "IAM could not be asked for this account's service-linked role"
            );
            None
        }
    }
}

/// Establish one of [`SERVER_INSTANCE_PROFILES`]: ask IAM for it, create the role and the
/// profile that holds it where IAM reports none, and conclude from a second read.
async fn establish_instance_profile(
    client: &::aws_sdk_iam::Client,
    name: &str,
) -> Establishment {
    match reported_instance_profile(client, name).await {
        Some(true) => return Establishment::Established,
        None => return Establishment::Unknown,
        Some(false) => {}
    }
    // Three writes make one instance profile a server can run under: the role itself, the
    // profile, and the binding between them. Each refusal is only logged — a role the
    // account already holds under this name is one this initialization can bind, and what
    // decides is the read that follows.
    if let Err(error) = client
        .create_role()
        .role_name(name)
        .assume_role_policy_document(server_trust_policy())
        .send()
        .await
    {
        log_write_refusal(name, "role", &error);
    }
    if let Err(error) = client
        .create_instance_profile()
        .instance_profile_name(name)
        .send()
        .await
    {
        log_write_refusal(name, "instance profile", &error);
    }
    if let Err(error) = client
        .add_role_to_instance_profile()
        .instance_profile_name(name)
        .role_name(name)
        .send()
        .await
    {
        log_write_refusal(name, "role binding", &error);
    }
    match reported_instance_profile(client, name).await {
        Some(true) => Establishment::Established,
        Some(false) => Establishment::Failed,
        None => Establishment::Unknown,
    }
}

/// Whether IAM reports an instance profile `name` of this account that HOLDS a role.
///
/// A profile holding no role hands an instance nothing to assume, so it is not yet the
/// resource an initialized account needs. `Some(false)` only where IAM answered that it
/// holds no such entity; `None` for every other answer, which is a question this emulator
/// could not put (see the module docs).
async fn reported_instance_profile(client: &::aws_sdk_iam::Client, name: &str) -> Option<bool> {
    match client
        .get_instance_profile()
        .instance_profile_name(name)
        .send()
        .await
    {
        Ok(reported) => Some(
            reported
                .instance_profile()
                .is_some_and(|profile| !profile.roles().is_empty()),
        ),
        Err(error) => {
            let no_such_entity = error
                .as_service_error()
                .is_some_and(::aws_sdk_iam::operation::get_instance_profile::GetInstanceProfileError::is_no_such_entity_exception);
            if no_such_entity {
                return Some(false);
            }
            ::tracing::warn!(
                instance_profile = %name,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "IAM could not be asked for an instance profile an account initialization needs"
            );
            None
        }
    }
}

/// Log one refused write of an initialization, at `warn` for the same reason an unanswered
/// read is logged: it is why an initialization established nothing.
fn log_write_refusal<E: ::std::error::Error + 'static, R: ::std::fmt::Debug>(
    name: &str,
    wrote: &str,
    error: &::aws_smithy_runtime_api::client::result::SdkError<E, R>,
) {
    ::tracing::warn!(
        instance_profile = %name,
        wrote,
        error = %::aws_smithy_types::error::display::DisplayErrorContext(error),
        "a write an account initialization needed was refused"
    );
}

/// The trust policy of a server role: the one statement that lets an EC2 instance assume
/// it through the instance profile that holds it.
///
/// The policy language version is IAM's own constant for it, so the document this writes
/// and the documents IAM writes declare one version.
fn server_trust_policy() -> String {
    ::serde_json::json!({
        "Version": iam::POLICY_LANGUAGE_VERSION,
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": SERVER_TRUST_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentence an initialization failure is worded with, over every combination of
    /// resources that can fail: the fixed text with its variable part naming which ones
    /// did. The recorded AWS answer for an initialization that established neither is the
    /// first row.
    #[test]
    fn the_sentence_names_which_resources_failed() {
        let cases: [(&str, bool, bool, Option<&str>); 4] = [
            (
                "neither the service-linked role nor the instance profiles",
                true,
                true,
                Some("Failed to create SLR or instance profiles"),
            ),
            (
                "the service-linked role alone",
                true,
                false,
                Some("Failed to create SLR"),
            ),
            (
                "the instance profiles alone",
                false,
                true,
                Some("Failed to create instance profiles"),
            ),
            ("nothing failed", false, false, None),
        ];
        for (case, service_linked_role, instance_profiles, expected) in cases {
            let failure = InitializationFailure {
                service_linked_role,
                instance_profiles,
            };
            assert_eq!(
                failure.message().as_deref(),
                expected,
                "case {case:?}: expected message {expected:?}, got {:?}",
                failure.message()
            );
            let refusal = failure.refusal();
            assert_eq!(
                refusal.as_ref().and_then(|refusal| refusal.message.as_deref()),
                expected,
                "case {case:?}: the refusal must carry that sentence, got {refusal:?}"
            );
            assert_eq!(
                refusal.as_ref().and_then(|refusal| refusal.reason.as_deref()),
                expected.map(|_| VALIDATION_OTHER),
                "case {case:?}: the refusal must carry the modelled reason, got {refusal:?}"
            );
        }
    }
}
