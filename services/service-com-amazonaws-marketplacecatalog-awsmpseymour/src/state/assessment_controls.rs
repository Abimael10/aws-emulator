// SPDX-License-Identifier: CC0-1.0

//! The controls each assessment framework of this catalog runs over a submitted
//! delivery option, and the `ControlAssessment` each one reports.

use super::*;

/// The controls of the AMI Security framework, named for the delivery-option
/// member each one reads.
///
/// `ControlAssessment$ControlId` is a `ResourceId` the model constrains in
/// syntax only (`^[\w\-]+$`) and whose values this service publishes nowhere,
/// so this catalogue is the emulator's OWN: each control is a check the
/// emulator can actually RUN over what a change submitted, and what each one
/// requires is what a change adding an AMI delivery option carries — the image
/// to evaluate, the role the catalog assumes to read it, the login user it
/// offers, the ingress its security groups open, and the instructions a buyer
/// launches it with. A control reports `PASS` only when its condition HELD on
/// the submitted value.
pub const CONTROL_AMI_SOURCE_IMAGE: &str = "AmiSourceImageDeclared";

/// A control of [`FRAMEWORK_AMI_SECURITY`] — see [`CONTROL_AMI_SOURCE_IMAGE`].
pub const CONTROL_AMI_ACCESS_ROLE: &str = "AmiSourceAccessRoleDeclared";

/// A control of [`FRAMEWORK_AMI_SECURITY`] — see [`CONTROL_AMI_SOURCE_IMAGE`].
pub const CONTROL_AMI_LOGIN_USER: &str = "AmiSourceLoginUserIsNotSuperuser";

/// A control of [`FRAMEWORK_AMI_SECURITY`] — see [`CONTROL_AMI_SOURCE_IMAGE`].
pub const CONTROL_AMI_INGRESS: &str = "AmiSecurityGroupIngressIsScoped";

/// A control of [`FRAMEWORK_AMI_SECURITY`] — see [`CONTROL_AMI_SOURCE_IMAGE`].
pub const CONTROL_AMI_USAGE_INSTRUCTIONS: &str = "AmiUsageInstructionsDeclared";

/// The controls of the Container Security framework, on the same footing as
/// [`CONTROL_AMI_SOURCE_IMAGE`]: what a change adding a container delivery
/// option carries is the images it delivers and the instructions a buyer runs
/// them with.
pub const CONTROL_CONTAINER_IMAGES: &str = "ContainerImagesDeclared";

/// A control of [`FRAMEWORK_CONTAINER_SECURITY`] — see
/// [`CONTROL_CONTAINER_IMAGES`].
pub const CONTROL_CONTAINER_IMAGES_PINNED: &str = "ContainerImagesArePinned";

/// A control of [`FRAMEWORK_CONTAINER_SECURITY`] — see
/// [`CONTROL_CONTAINER_IMAGES`].
pub const CONTROL_CONTAINER_USAGE_INSTRUCTIONS: &str = "ContainerUsageInstructionsDeclared";

/// The member of an AMI delivery option's details naming the image to evaluate.
pub(super) const AMI_IMAGE_PATH: &[&str] = &["AmiSource", "AmiId"];

/// The member naming the role the catalog assumes to read that image.
pub(super) const AMI_ACCESS_ROLE_PATH: &[&str] = &["AmiSource", "AccessRoleArn"];

/// The member naming the login user the image offers a buyer.
pub(super) const AMI_LOGIN_USER_PATH: &[&str] = &["AmiSource", "UserName"];

/// The member naming the security groups the image expects.
pub(super) const AMI_SECURITY_GROUPS_PATH: &[&str] = &["SecurityGroups"];

/// The member of one security group naming the addresses its rule admits.
pub(super) const SECURITY_GROUP_IP_RANGES_MEMBER: &str = "IpRanges";

/// The member naming the instructions a buyer uses the delivery option with,
/// which both delivery forms carry.
pub(super) const USAGE_INSTRUCTIONS_PATH: &[&str] = &["UsageInstructions"];

/// The member of a container delivery option's details naming the images it
/// delivers.
pub(super) const CONTAINER_IMAGES_PATH: &[&str] = &["ContainerImages"];

/// An `ErrorScope$Name`: "the name of the resource field the error applies to
/// (for example, `AMI_ID`, `FILE_PATH`, or `PACKAGE_NAME`)" — the field name a
/// control's error applies to, in the form that doc spells.
pub(super) const SCOPE_AMI_ID: &str = "AMI_ID";

/// An `ErrorScope$Name` — see [`SCOPE_AMI_ID`].
pub(super) const SCOPE_ACCESS_ROLE_ARN: &str = "ACCESS_ROLE_ARN";

/// An `ErrorScope$Name` — see [`SCOPE_AMI_ID`].
pub(super) const SCOPE_USER_NAME: &str = "USER_NAME";

/// An `ErrorScope$Name` — see [`SCOPE_AMI_ID`].
pub(super) const SCOPE_IP_RANGES: &str = "IP_RANGES";

/// An `ErrorScope$Name` — see [`SCOPE_AMI_ID`].
pub(super) const SCOPE_USAGE_INSTRUCTIONS: &str = "USAGE_INSTRUCTIONS";

/// An `ErrorScope$Name` — see [`SCOPE_AMI_ID`].
pub(super) const SCOPE_CONTAINER_IMAGES: &str = "CONTAINER_IMAGES";

/// The login user an image may not offer: the superuser account, which a buyer
/// logging in would hold every privilege on the instance as.
pub(super) const AMI_SUPERUSER: &str = "root";

/// The CIDR blocks that admit EVERY address, in IPv4 and in IPv6 — an ingress
/// rule naming one opens its port to the whole internet rather than to the
/// addresses the product needs.
pub(super) const UNRESTRICTED_IP_RANGES: &[&str] = &["0.0.0.0/0", "::/0"];

/// The container image tag that FLOATS: a reference carrying it names whatever
/// the registry holds when the image is pulled, so what a buyer runs is not the
/// image that was assessed.
pub(super) const FLOATING_IMAGE_TAG: &str = "latest";

/// What evaluating the delivery option `option` against `framework` found: one
/// `ControlAssessment` per control of that framework, in the order an
/// assessment reports them.
///
/// Empty for a framework this catalog does not assess against, which is what
/// keeps "nothing was evaluated" apart from "every control passed": no
/// assessment is recorded at all.
pub fn assess_delivery_option(framework: &str, option: &Value) -> Vec<ControlAssessment> {
    let details = delivery_option_details(option);
    match framework {
        FRAMEWORK_AMI_SECURITY => vec![
            text_declared_control(
                CONTROL_AMI_SOURCE_IMAGE,
                SCOPE_AMI_ID,
                AMI_IMAGE_PATH,
                &details,
            ),
            text_declared_control(
                CONTROL_AMI_ACCESS_ROLE,
                SCOPE_ACCESS_ROLE_ARN,
                AMI_ACCESS_ROLE_PATH,
                &details,
            ),
            ami_login_user_control(&details),
            ami_ingress_control(&details),
            text_declared_control(
                CONTROL_AMI_USAGE_INSTRUCTIONS,
                SCOPE_USAGE_INSTRUCTIONS,
                USAGE_INSTRUCTIONS_PATH,
                &details,
            ),
        ],
        FRAMEWORK_CONTAINER_SECURITY => vec![
            container_images_declared_control(&details),
            container_images_pinned_control(&details),
            text_declared_control(
                CONTROL_CONTAINER_USAGE_INSTRUCTIONS,
                SCOPE_USAGE_INSTRUCTIONS,
                USAGE_INSTRUCTIONS_PATH,
                &details,
            ),
        ],
        _ => Vec::new(),
    }
}

/// The number of delivery options the details of a change submit, or `None`
/// when they carry no delivery option list at all.
///
/// Reads the LENGTH of the submitted list without walking its entries, so a
/// change past [`MAX_DELIVERY_OPTIONS_PER_CHANGE`] is refused before anything
/// of it is copied.
pub fn delivery_option_count(details: &Value) -> Option<usize> {
    Some(
        details
            .get(DELIVERY_OPTIONS_DETAIL_MEMBER)?
            .as_array()?
            .len(),
    )
}

/// The members of a submitted delivery option that a framework's controls read.
///
/// A delivery option carries the delivery form's own members inside
/// [`DELIVERY_OPTION_DETAILS_MEMBER`], itself wrapped in ONE member naming that
/// form (`AmiDeliveryOptionDetails`, `EcrDeliveryOptionDetails`), so the
/// members a control reads are one level further down. An option that wraps
/// nothing is read as it stands, so the controls see the same members whichever
/// way the change spelled the option.
pub(super) fn delivery_option_details(option: &Value) -> Map<String, Value> {
    let Some(members) = option.as_object() else {
        return Map::new();
    };
    let Some(Value::Object(details)) = members.get(DELIVERY_OPTION_DETAILS_MEMBER) else {
        return members.clone();
    };
    if details.len() == 1 {
        if let Some(Value::Object(form)) = details.values().next() {
            return form.clone();
        }
    }
    details.clone()
}

/// The value `path` names: a chain of object members walked from `details`.
pub(super) fn detail_at<'a>(details: &'a Map<String, Value>, path: &[&str]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let mut value = details.get(*first)?;
    for member in rest {
        value = value.as_object()?.get(*member)?;
    }
    Some(value)
}

/// The assessment of a control whose condition held.
pub(super) fn control_passed(id: &str) -> ControlAssessment {
    ControlAssessment {
        control_id: Some(id.to_string()),
        control_assessment_result: Some(CONTROL_PASS.to_string()),
        // "An array of ControlError objects associated with the control
        // evaluation": a control that found nothing wrong has none.
        errors: None,
    }
}

/// The assessment of a control the delivery option carries nothing for, so the
/// catalog evaluated nothing — reported as such rather than as a pass.
pub(super) fn control_not_executed(id: &str) -> ControlAssessment {
    ControlAssessment {
        control_id: Some(id.to_string()),
        control_assessment_result: Some(CONTROL_NOT_EXECUTED.to_string()),
        errors: None,
    }
}

/// The assessment of a control whose condition did not hold, with the one error
/// that says why: its code, the sentence, and the `ErrorScope` naming the field
/// the error applies to and the value the control refused.
pub(super) fn control_failed(
    id: &str,
    code: &str,
    message: String,
    scope: &str,
    value: Option<String>,
) -> ControlAssessment {
    ControlAssessment {
        control_id: Some(id.to_string()),
        control_assessment_result: Some(CONTROL_FAIL.to_string()),
        errors: Some(vec![ControlError {
            code: Some(code.to_string()),
            message: Some(message),
            scope: Some(vec![ErrorScope {
                name: Some(scope.to_string()),
                // Absent when the option carried no value at all: the scope
                // names the field, and there is nothing to quote for it.
                value,
            }]),
        }]),
    }
}

/// `text` as an `ErrorScope$Value` carries it: cut at [`SCOPE_VALUE_LIMIT`]
/// characters so it stays inside `ScopeValue`'s modelled length, and rendered
/// as an empty JSON string when it is empty, which `@length(min: 1)` leaves no
/// room for.
pub(super) fn scope_text(text: &str) -> String {
    if text.is_empty() {
        return "\"\"".to_string();
    }
    cut_to(text, SCOPE_VALUE_LIMIT, "")
}

/// The same, for a submitted value that may be no text at all: its JSON
/// rendering.
pub(super) fn scope_value(value: &Value) -> String {
    match value.as_str() {
        Some(text) => scope_text(text),
        None => scope_text(&value.to_string()),
    }
}

/// The assessment of a control that requires the delivery option to declare a
/// non-empty text value at `path`.
///
/// The catalog has nothing to deliver or to evaluate without it, so an option
/// that omits it FAILS the control — unlike one that omits a member the control
/// merely reads about ([`control_not_executed`]).
pub(super) fn text_declared_control(
    id: &str,
    scope: &str,
    path: &[&str],
    details: &Map<String, Value>,
) -> ControlAssessment {
    let member = path.join(".");
    let Some(value) = detail_at(details, path) else {
        return control_failed(
            id,
            CONTROL_ERROR_MISSING_VALUE,
            format!("The assessed delivery option does not declare {member}."),
            scope,
            None,
        );
    };
    match value.as_str() {
        Some(text) if !text.trim().is_empty() => control_passed(id),
        _ => control_failed(
            id,
            CONTROL_ERROR_INVALID_VALUE,
            format!("{member} of the assessed delivery option names no value."),
            scope,
            Some(scope_value(value)),
        ),
    }
}

/// The login-user control: the image does not offer [`AMI_SUPERUSER`] as the
/// account a buyer logs in as. An option that declares no login user carries
/// nothing for this control to read, so nothing ran.
pub(super) fn ami_login_user_control(details: &Map<String, Value>) -> ControlAssessment {
    let Some(user) = detail_at(details, AMI_LOGIN_USER_PATH).and_then(Value::as_str) else {
        return control_not_executed(CONTROL_AMI_LOGIN_USER);
    };
    if !user.eq_ignore_ascii_case(AMI_SUPERUSER) {
        return control_passed(CONTROL_AMI_LOGIN_USER);
    }
    control_failed(
        CONTROL_AMI_LOGIN_USER,
        CONTROL_ERROR_INVALID_VALUE,
        format!(
            "{} of the assessed delivery option names the superuser account.",
            AMI_LOGIN_USER_PATH.join(".")
        ),
        SCOPE_USER_NAME,
        Some(scope_text(user)),
    )
}

/// The ingress control: no rule of the security groups the option declares
/// admits every address ([`UNRESTRICTED_IP_RANGES`]). An option that declares
/// no address range opens nothing for this control to read, so nothing ran.
pub(super) fn ami_ingress_control(details: &Map<String, Value>) -> ControlAssessment {
    let groups = detail_at(details, AMI_SECURITY_GROUPS_PATH)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let ranges = groups
        .iter()
        .filter_map(|group| group.get(SECURITY_GROUP_IP_RANGES_MEMBER))
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str);
    let mut evaluated = 0usize;
    for range in ranges {
        evaluated += 1;
        if UNRESTRICTED_IP_RANGES.contains(&range) {
            // The quoted range is cut to `ScopeValue`'s length, which keeps the
            // sentence built around it inside `ErrorMessage`'s 2048 characters.
            let quoted = scope_text(range);
            return control_failed(
                CONTROL_AMI_INGRESS,
                CONTROL_ERROR_INVALID_VALUE,
                format!(
                    "An ingress rule of the assessed delivery option admits every address \
                     ({quoted})."
                ),
                SCOPE_IP_RANGES,
                Some(quoted),
            );
        }
    }
    if evaluated == 0 {
        return control_not_executed(CONTROL_AMI_INGRESS);
    }
    control_passed(CONTROL_AMI_INGRESS)
}

/// The image-declared control: the option names at least one container image to
/// deliver, without which there is nothing to assess or to run.
pub(super) fn container_images_declared_control(details: &Map<String, Value>) -> ControlAssessment {
    let member = CONTAINER_IMAGES_PATH.join(".");
    let Some(value) = detail_at(details, CONTAINER_IMAGES_PATH) else {
        return control_failed(
            CONTROL_CONTAINER_IMAGES,
            CONTROL_ERROR_MISSING_VALUE,
            format!("The assessed delivery option does not declare {member}."),
            SCOPE_CONTAINER_IMAGES,
            None,
        );
    };
    if container_image_references(value).is_empty() {
        return control_failed(
            CONTROL_CONTAINER_IMAGES,
            CONTROL_ERROR_INVALID_VALUE,
            format!("{member} of the assessed delivery option names no container image."),
            SCOPE_CONTAINER_IMAGES,
            Some(scope_value(value)),
        );
    }
    control_passed(CONTROL_CONTAINER_IMAGES)
}

/// The pinning control: every image the option delivers is pinned to one image
/// ([`pins_container_image`]). An option that names no image carries nothing
/// for this control to read, so nothing ran.
pub(super) fn container_images_pinned_control(details: &Map<String, Value>) -> ControlAssessment {
    let references = detail_at(details, CONTAINER_IMAGES_PATH)
        .map(container_image_references)
        .unwrap_or_default();
    if references.is_empty() {
        return control_not_executed(CONTROL_CONTAINER_IMAGES_PINNED);
    }
    for reference in references {
        if !pins_container_image(reference) {
            // Cut for the same reason as in [`ami_ingress_control`].
            let quoted = scope_text(reference);
            return control_failed(
                CONTROL_CONTAINER_IMAGES_PINNED,
                CONTROL_ERROR_INVALID_VALUE,
                format!(
                    "A container image of the assessed delivery option is not pinned to one \
                     image ({quoted})."
                ),
                SCOPE_CONTAINER_IMAGES,
                Some(quoted),
            );
        }
    }
    control_passed(CONTROL_CONTAINER_IMAGES_PINNED)
}

/// The image references a submitted `ContainerImages` value names.
pub(super) fn container_image_references(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .map(|images| {
            images
                .iter()
                .filter_map(Value::as_str)
                .filter(|image| !image.trim().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `reference` pins the image it names.
///
/// A reference carrying a digest (`…@sha256:…`) names one image for ever, and
/// so does one carrying a tag other than [`FLOATING_IMAGE_TAG`]; a reference
/// with neither resolves to whatever the registry holds when it is pulled. The
/// tag is read from the LAST path segment, since the registry host before it
/// may itself carry a port.
pub(super) fn pins_container_image(reference: &str) -> bool {
    let name = reference.split('/').next_back().unwrap_or(reference);
    if name.contains('@') {
        return true;
    }
    name.rsplit_once(':')
        .is_some_and(|(_, tag)| !tag.is_empty() && tag != FLOATING_IMAGE_TAG)
}
