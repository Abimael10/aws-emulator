// SPDX-License-Identifier: CC0-1.0

//! Reading an Amazon Resource Name.
//!
//! "The following are the general formats for ARNs […]
//! `arn:<partition>:<service>:<region>:<account-id>:<resource-id>`" (AWS General Reference,
//! "Amazon Resource Names"), so an ARN is six colon-separated fields and the LAST one is the
//! resource — which itself names its type and its id as `<type>/<id>` or `<type>:<id>`.
//!
//! One reader for every operation of this service that is handed an ARN where it could equally
//! have been handed a bare id: the operation that VALIDATES such a value and the one that
//! RESOLVES it later cannot then disagree about which resource it names.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

/// The scheme every ARN begins with (AWS General Reference, "Amazon Resource Names").
pub const ARN_SCHEME: &str = "arn:";

/// Is `value` an ARN at all, as opposed to a bare resource id?
pub fn is_arn(value: &str) -> bool {
    value.starts_with(ARN_SCHEME)
}

/// The `field`th colon-separated field of the ARN `value`, or `None` when `value` is no ARN or
/// carries no such field.
///
/// The fields are numbered as the published format writes them: 0 `arn`, 1 the partition, 2 the
/// service, 3 the Region, 4 the account, 5 the resource — and the resource keeps whatever
/// colons it contains, which is why it is the LAST field rather than one of six.
pub fn arn_field(value: &str, field: usize) -> Option<&str> {
    if !is_arn(value) {
        return None;
    }
    value.splitn(6, ':').nth(field)
}

/// The Region the ARN `value` names, or `None` when `value` is no ARN or names no Region.
pub fn arn_region(value: &str) -> Option<&str> {
    arn_field(value, 3).filter(|region| !region.is_empty())
}

/// The resource type and the resource id the ARN `value` names — `<type>/<id>` or `<type>:<id>`
/// inside its resource field — or `None` when `value` is no ARN or its resource field names no
/// type.
pub fn arn_resource(value: &str) -> Option<(&str, &str)> {
    let resource = arn_field(value, 5)?;
    resource
        .split_once('/')
        .or_else(|| resource.split_once(':'))
}

/// The resource `value` names, whether it was given as an ARN or as a bare id.
///
/// "If the resource is in another account, you must specify an ARN"
/// (`CreateNetworkInsightsPathRequest$Source`) is the shape of every member that admits either
/// form: an ARN names its resource in its resource field, and anything else IS the resource id.
pub fn arn_or_bare_resource_id(value: &str) -> &str {
    match arn_resource(value) {
        Some((_, id)) => id,
        // An ARN whose resource field names no type carries that field itself as the resource,
        // and a value that is no ARN is the id as given.
        None => arn_field(value, 5).unwrap_or(value),
    }
}
