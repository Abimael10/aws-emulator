// SPDX-License-Identifier: CC0-1.0

//! Which values are identifiers of a notify code configuration, and how one that is
//! not is refused.
//!
//! `NotifyCodeConfigurationIdOrArn` accepts two forms — a bare, system-generated
//! identifier or the full ARN of a configuration — so a value reaching no stored
//! configuration does so for one of two reasons, and they are different failures to a
//! caller: a value that is neither form can name a configuration in NO state and is a
//! bad request, while an identifier of the documented form that this state holds no
//! record for is a missing resource. Classifying the value therefore happens BEFORE any
//! lookup, and it lives here rather than in one handler so every operation that takes
//! such an identifier — whichever member it arrives on — tells the two apart the same
//! way.

/// The resource prefix a system-generated `notifyCodeConfigurationId` carries, which is
/// what [`super::ComAmazonawsEndusermessagingEndusermessagingState::mint_notify_code_configuration_id`]
/// mints every identifier of this resource with.
pub const NOTIFY_CODE_CONFIGURATION_ID_PREFIX: &str = "ncc";

/// The longest a bare `notifyCodeConfigurationId` is, as `NotifyCodeConfigurationId`'s
/// `@length(1..64)` declares it
/// (`smithy_query(query="shape",shape="NotifyCodeConfigurationId")`).
const NOTIFY_CODE_CONFIGURATION_ID_MAX_LENGTH: usize = 64;

/// The member a `GetNotifyCodeConfiguration`, `UpdateNotifyCodeConfiguration` or
/// `DeleteNotifyCodeConfiguration` request carries the identifier on.
pub const NOTIFY_CODE_CONFIGURATION_ID_MEMBER: &str = "notifyCodeConfigurationId";

/// The member a `SendNotifyCodeVerification` request carries the identifier on.
pub const NOTIFY_CODE_CONFIGURATION_MEMBER: &str = "notifyCodeConfiguration";

/// Whether `candidate` is a bare `notifyCodeConfigurationId`: the system-generated
/// identifier this service mints, its resource prefix followed by a hyphen and a body of
/// `NotifyCodeConfigurationId`'s own character class, inside that shape's length.
///
/// `NotifyCodeConfigurationIdOrArn` documents the identifier as an opaque token whose
/// length and format a client must not assume, so no particular body length is required
/// of one: what makes a value an identifier of THIS resource is the resource prefix the
/// service mints it with, and a body the bare-identifier shape admits
/// (`^[A-Za-z0-9_-]+$`).
fn is_notify_code_configuration_id(candidate: &str) -> bool {
    candidate.len() <= NOTIFY_CODE_CONFIGURATION_ID_MAX_LENGTH
        && candidate
            .strip_prefix(NOTIFY_CODE_CONFIGURATION_ID_PREFIX)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|body| {
                !body.is_empty()
                    && body
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            })
}

/// The `notifyCodeConfigurationId` a well-formed `NotifyCodeConfigurationIdOrArn` names,
/// whichever of the two forms the shape documents it took, or `None` for a value that is
/// neither.
///
/// A bare identifier names itself; an ARN of this service's `notify-code-configuration`
/// resource names the identifier in its resource field. The ARN form carries the same
/// bare identifier, so a value is well formed only when the identifier it names is.
/// Whether that identifier reaches a stored configuration is a separate question, asked
/// after this one.
pub fn notify_code_configuration_id_of(id_or_arn: &str) -> Option<&str> {
    if is_notify_code_configuration_id(id_or_arn) {
        return Some(id_or_arn);
    }
    let id = super::resource_id_from_arn(id_or_arn, super::NOTIFY_CODE_CONFIGURATION_RESOURCE)?;
    is_notify_code_configuration_id(id).then_some(id)
}

/// `ValidationException` for a `NotifyCodeConfigurationIdOrArn` that is not an identifier
/// of a notify code configuration at all — neither a bare identifier nor the ARN of one.
///
/// Such a value can name no configuration in any state, so it is a refusal about the
/// input rather than about a missing resource. The fixed text is this refusal's; the only
/// variable part is the value the request gave, and `fieldList` names the member it
/// arrived on — the identifier reaches different operations on different members — and
/// states that one sentence about it: the gate has one thing to say about the member it
/// refused. The member is named the way this service's own refusals name a top-level
/// member at fault — by the member's own name, as the brand-profile identifier gate
/// beside it does ([`super::invalid_brand_profile_id`]).
pub fn invalid_notify_code_configuration_id(
    member: &str,
    id_or_arn: &str,
) -> crate::ops::types::ValidationException {
    let stated = format!("invalid notify code configuration ID format: {id_or_arn}");
    crate::ops::types::ValidationException {
        message: stated.clone(),
        field_list: Some(vec![crate::ops::types::ValidationExceptionField {
            path: member.to_string(),
            message: stated,
        }]),
    }
}
