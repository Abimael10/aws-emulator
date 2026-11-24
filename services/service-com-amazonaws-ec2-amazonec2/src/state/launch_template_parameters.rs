// SPDX-License-Identifier: CC0-1.0

//! The launch parameters a request SPECIFIED, out of the launch template data it carried.
//!
//! EC2 speaks `ec2Query`: a request's parameters are form fields, and a field carrying no text
//! is indistinguishable from a field the request never sent — `LaunchTemplateData.KeyName=` and
//! a request without that field are the same bytes to any reader of the pair list. So a launch
//! parameter supplied with no text is a parameter the request DID NOT SPECIFY: the version
//! stores nothing for it, reports nothing for it, and a version that names a `SourceVersion`
//! inherits the source's value for it instead of blanking it.
//!
//! The same reading the rest of this store already takes of one such reference — an instance
//! profile, an instance type or a security group with no text names none
//! (`super::launch_template_warnings`, `super::launch_template_security_groups`) — taken ONCE
//! over the whole shape here, at the point the data enters the store, so every later reader of
//! a stored version sees the parameters the request specified and nothing else.

use crate::ops::types::RequestLaunchTemplateData;

/// The launch parameters `data` SPECIFIED: every member the shape spells as text, kept when it
/// carries text and dropped when it does not, with every other member as given.
///
/// Both writes of launch template data answer from here — `CreateLaunchTemplate` for a
/// template's first version and `CreateLaunchTemplateVersion` for every later one — so the two
/// cannot read an empty parameter differently, and the inheritance a `SourceVersion` performs
/// is over the parameters the request specified rather than over the fields it sent.
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

/// One text parameter as the request specified it: the value when it carries text, and nothing
/// when it carries none.
fn with_text(value: &Option<String>) -> Option<String> {
    value.clone().filter(|text| !text.is_empty())
}
