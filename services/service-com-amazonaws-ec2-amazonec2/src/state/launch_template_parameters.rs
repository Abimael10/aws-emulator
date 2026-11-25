// SPDX-License-Identifier: CC0-1.0

//! What the launch template data a request carried SAYS, as against the fields it sent.
//!
//! EC2 speaks `ec2Query`: a request's parameters are form fields, and a field carrying no text
//! sets no VALUE for the parameter it spells — `LaunchTemplateData.KeyName=` names no key pair
//! — while the pair list still names the field, so the request did send that parameter. The
//! two readings answer two different questions, and both are taken here, once, so no writer of
//! launch template data can read an empty field differently from its neighbour:
//!
//! * WAS a launch parameter given at all, which is what the documented "you must specify at
//!   least one parameter for the launch template data" requirement asks
//!   ([`carries_a_launch_parameter`]);
//! * WHICH parameters carry a value to overwrite an inherited one with, which is what the
//!   inheritance a `SourceVersion` performs asks ([`specified_launch_template_data`]).
//!
//! What a version STORES is neither: it is the data the request carried, each parameter as
//! sent, because a launch template is the set of launch parameters a client gave it. A value
//! with no text names no resource where a parameter is RESOLVED as a reference — an instance
//! profile, an instance type or a security group with no text names none
//! (`super::launch_template_warnings`, `super::launch_template_security_groups`) — which is
//! that resolution's own reading, taken wherever the reference is read.

use crate::ops::types::RequestLaunchTemplateData;

/// The launch parameters `data` carries a VALUE for: every member the shape spells as text,
/// kept when it carries text and dropped when it does not, with every other member as given.
///
/// This is the overlay a new version puts over the one it inherits: "if you specify additional
/// launch parameters for the new version, they overwrite any corresponding launch parameters
/// inherited from the source version" (`CreateLaunchTemplateVersionRequest$SourceVersion`), and
/// a field carrying no text carries nothing to overwrite one with, so the source's parameter
/// stands.
///
/// The members taken through this reading are the ones the model targets a string with
/// (`smithy_query(query="shape",shape="RequestLaunchTemplateData")`): the rest of the shape is
/// copied as given, so a member the model gains later keeps whatever the request carried until
/// it is named here.
pub fn specified_launch_template_data(
    data: &RequestLaunchTemplateData,
) -> RequestLaunchTemplateData {
    RequestLaunchTemplateData {
        kernel_id: with_text(&data.kernel_id),
        image_id: with_text(&data.image_id),
        instance_type: with_text(&data.instance_type),
        key_name: with_text(&data.key_name),
        ram_disk_id: with_text(&data.ram_disk_id),
        instance_initiated_shutdown_behavior: with_text(
            &data.instance_initiated_shutdown_behavior,
        ),
        user_data: with_text(&data.user_data),
        ..data.clone()
    }
}

/// One text parameter as the value it carries: the text when it carries some, and nothing when
/// it carries none.
fn with_text(value: &Option<String>) -> Option<String> {
    value.clone().filter(|text| !text.is_empty())
}

/// Whether `data` carries a launch parameter FIELD at all — the one test of the documented
/// "you must specify at least one parameter for the launch template data"
/// (`smithy_query(query="shape",shape="RequestLaunchTemplateData")`) requirement.
///
/// The requirement is about the parameters the request NAMED, which an ec2Query pair list
/// carries whether or not they hold text: a request that sends a launch parameter with no text
/// has named that parameter, so the data it carried is not empty. Only a request that names NO
/// launch parameter at all has given no launch template data.
///
/// The shape carries dozens of members, so emptiness is decided by comparing the data with a
/// default one rather than by naming each member — which also means a member the model gains
/// later counts without this reading being revisited. The rendering is used for this
/// comparison only; it is never reported or logged, because `UserData` is `@sensitive`.
pub fn carries_a_launch_parameter(data: &RequestLaunchTemplateData) -> bool {
    format!("{data:?}") != format!("{:?}", RequestLaunchTemplateData::default())
}
