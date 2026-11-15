// SPDX-License-Identifier: CC0-1.0

//! Whether a request may address a layer that belongs to ANOTHER account.
//!
//! # The rule
//!
//! A layer is an account's own resource, and every ARN that names one carries the account
//! that owns it (`arn:<partition>:lambda:<region>:<account>:layer:<name>[:<version>]`). Each
//! operation on a layer takes that ARN as an alternative to the bare name — "the name or
//! Amazon Resource Name (ARN) of the layer" — so a caller can address a layer of any account
//! at all, and what decides such a call is the OWNING account's grant, not whether this
//! endpoint happens to hold a layer of the same name: "to grant layer-usage permission to
//! another account, add a statement to the layer version's permissions policy"
//! (`AddLayerVersionPermission`, whose `Action` admits `lambda:GetLayerVersion` alone).
//!
//! This endpoint serves ONE account ([`::engine::emulator_account_id`]) and holds that
//! account's layers alone, so a request naming another account's layer is a request about a
//! resource whose permissions policy this endpoint does not hold: nothing can have granted
//! the caller anything on it, and IAM's default deny stands. AWS answers such a call at the
//! IAM layer, ahead of any lookup, with the AWS COMMON `AccessDeniedException` — HTTP 403,
//! error code `AccessDeniedException` — which this service answers through
//! [`crate::scaffolded::Scaffolded::AccessDenied`] because its model declares no access error
//! on any operation (`smithy_query(query="digest")`).
//!
//! Answering the modelled 404 instead would say "there is no such layer" for a layer that
//! does exist and is simply not the caller's, so a client cannot tell a missing resource from
//! one it may not reach.
//!
//! # What the refusal says
//!
//! The one sentence every service in this tree refuses a resource-policy decision with
//! ([`::service_common::iam_authorization::not_authorized_by_resource_policy`]): the principal
//! that signed, the action it asked for, and the layer version ARN it addressed. The ARN is
//! rebuilt from the fields the request's own value carried, so it names the account, Region,
//! layer and version the call addressed and nothing of this endpoint's state.
//!
//! The action is the one the IAM Service Authorization Reference publishes for the operation
//! ("Actions defined by AWS Lambda"), which is `lambda:<ApiName>` for each of them except the
//! by-ARN read, that reference publishing no action of its own for an entry point performing
//! the same read ([`layer_action`]).
//!
//! # What it does not decide
//!
//! The layers of THIS account, which the operations resolve out of the store as they always
//! did: a bare `LayerName` names one of them, and so does an ARN carrying this account's own
//! id. Whether the caller's identity-based policies allow the action is the separate
//! control-plane decision ([`super::caller_authorization`]), and an ARN of another REGION
//! names no layer of this endpoint at all, which the one reading of a `LayerName`
//! ([`super::layer_name_in`]) already states.
//!
//! A pure READ of the request and of this emulator's account contract, plus the one reading of
//! the request's principal this crate has ([`super::invoke_authorization::caller`]): nothing
//! here writes, and this service's own store is never locked, so a handler takes this decision
//! before it locks anything.

use ::service_common::iam_authorization as iam;

use super::invoke_authorization::{caller, ACTION_PREFIX};
use crate::scaffolded::AccessDeniedException;

/// The action a READ of a layer version is performed under, as the IAM Service Authorization
/// Reference publishes it.
///
/// Both read entry points are judged under it: `GetLayerVersionByArn` addresses the same
/// resource by a different member, and that reference publishes no action of its own for it.
pub const GET_LAYER_VERSION_ACTION: &str = "lambda:GetLayerVersion";

/// The IAM action `operation` is performed under: `lambda:<ApiName>`, which is what the IAM
/// Service Authorization Reference publishes for every layer operation of this service bar
/// the by-ARN read ([`GET_LAYER_VERSION_ACTION`]).
#[must_use]
pub fn layer_action(operation: &str) -> String {
    match operation {
        "GetLayerVersionByArn" => GET_LAYER_VERSION_ACTION.to_string(),
        named => format!("{ACTION_PREFIX}:{named}"),
    }
}

/// The layer an ARN-form `LayerName` or `LayerVersionArn` names, in the fields that say WHICH
/// layer it is.
///
/// Borrowed from the value that was parsed, because the refusal names the layer the request
/// addressed rather than anything stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AddressedLayer<'a> {
    /// The partition the ARN names.
    partition: &'a str,
    /// The Region the ARN names, which is the Region the layer was published into.
    region: &'a str,
    /// The account the ARN names, which is the account that OWNS the layer.
    account: &'a str,
    /// The bare layer name.
    name: &'a str,
    /// The version number the ARN qualifies the layer with, for the ARN form that carries
    /// one.
    version: Option<i64>,
}

impl AddressedLayer<'_> {
    /// The ARN of the layer at `version`, spelled as the model spells a `LayerVersionArn` —
    /// or of the layer itself, for a call that addresses no single version.
    fn arn(&self, version: Option<i64>) -> String {
        let arn = format!(
            "arn:{partition}:{ARN_NAMESPACE}:{region}:{account}:layer:{name}",
            partition = self.partition,
            ARN_NAMESPACE = super::ARN_NAMESPACE,
            region = self.region,
            account = self.account,
            name = self.name,
        );
        match self.version.or(version) {
            Some(version) => format!("{arn}:{version}"),
            None => arn,
        }
    }
}

/// The layer `value` names, or `None` when `value` is not a layer ARN at all — a bare
/// `LayerName`, which names a layer of this account, or a value in neither documented form,
/// which the operation's own reading of the member refuses.
fn addressed_layer(value: &str) -> Option<AddressedLayer<'_>> {
    let parsed = ::service_common::arn::parse(value)?;
    if parsed.vendor != super::ARN_NAMESPACE {
        return None;
    }
    let mut fields = parsed.resource.split(':');
    if fields.next() != Some("layer") {
        return None;
    }
    let name = fields.next().filter(|name| super::is_layer_name(name))?;
    let version = match fields.next() {
        Some(carried) => Some(carried.parse::<i64>().ok()?),
        None => None,
    };
    // `layer:<name>[:<version>]` and nothing further: a resource field carrying more than
    // that names no layer version.
    if fields.next().is_some() {
        return None;
    }
    Some(AddressedLayer {
        partition: parsed.partition,
        region: parsed.region,
        account: parsed.account,
        name,
        version,
    })
}

/// Whether the caller may address the layer `layer_name` names under `operation`.
///
/// `layer_name` is the `LayerName` or `LayerVersionArn` member exactly as the request supplied
/// it, and `version_number` the version the call addresses, for an operation that addresses
/// one (`None` for the operations whose resource is the layer itself).
///
/// `Ok(())` for every layer of the account this endpoint serves, however the request named it;
/// the 403 refusal for a layer of any other account (see this module's header). Call it BEFORE
/// this service's store is locked and before anything is resolved, so a refused call changes
/// nothing, reads nothing back and tells the caller nothing about what this account holds.
pub fn authorize_layer_access<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    layer_name: &str,
    version_number: Option<i64>,
) -> Result<(), AccessDeniedException> {
    let Some(addressed) = addressed_layer(layer_name) else {
        // A bare name addresses this account's own layers, and a value in neither form
        // addresses no layer at all.
        return Ok(());
    };
    if addressed.account == ::engine::emulator_account_id() {
        return Ok(());
    }
    let action = layer_action(operation);
    Err(AccessDeniedException::new(
        iam::not_authorized_by_resource_policy(
            &caller(engine, ctx).arn,
            &action,
            &addressed.arn(version_number),
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One value a request can name a layer by, and what reading it answers.
    struct Row<'a> {
        /// What makes this value different from the others.
        case: &'a str,
        /// The value as a request supplied it.
        value: &'a str,
        /// The version the call addresses in its own member, when it has one.
        version: Option<i64>,
        /// The account the value names as the layer's owner, or `None` for a value that
        /// names no layer of any account.
        owner: Option<&'a str>,
        /// The ARN a refusal about it names.
        arn: Option<&'a str>,
    }

    /// RULE: a layer ARN is read for the account that OWNS the layer, and the ARN a refusal
    /// names is that layer AT the version the call addressed — whichever of the two members
    /// carried the version.
    #[test]
    fn a_layer_arn_names_its_owner_and_the_version_addressed() {
        let account = ::engine::emulator_account_id();
        let own = format!("arn:aws:lambda:us-west-2:{account}:layer:probe");
        let own_version = format!("{own}:3");
        let rows = &[
            Row {
                case: "a bare layer name",
                value: "probe",
                version: Some(1),
                owner: None,
                arn: None,
            },
            Row {
                case: "this account's own layer ARN",
                value: own.as_str(),
                version: Some(3),
                owner: Some(account),
                arn: Some(own_version.as_str()),
            },
            Row {
                case: "another account's layer ARN, the version in the other member",
                value: "arn:aws:lambda:us-west-2:111122223333:layer:probe",
                version: Some(7),
                owner: Some("111122223333"),
                arn: Some("arn:aws:lambda:us-west-2:111122223333:layer:probe:7"),
            },
            Row {
                case: "another account's layer VERSION ARN",
                value: "arn:aws:lambda:eu-west-1:111122223333:layer:probe:2",
                version: None,
                owner: Some("111122223333"),
                arn: Some("arn:aws:lambda:eu-west-1:111122223333:layer:probe:2"),
            },
            Row {
                case: "a version ARN whose own version wins over the member",
                value: "arn:aws:lambda:us-west-2:111122223333:layer:probe:2",
                version: Some(9),
                owner: Some("111122223333"),
                arn: Some("arn:aws:lambda:us-west-2:111122223333:layer:probe:2"),
            },
            Row {
                case: "another partition",
                value: "arn:aws-us-gov:lambda:us-gov-west-1:111122223333:layer:probe:4",
                version: None,
                owner: Some("111122223333"),
                arn: Some("arn:aws-us-gov:lambda:us-gov-west-1:111122223333:layer:probe:4"),
            },
            Row {
                case: "a layer Lambda itself provides, which belongs to no account",
                value: "arn:aws:lambda:::awslayer:AWSLambda-Python37-SciPy1x",
                version: None,
                owner: None,
                arn: None,
            },
            Row {
                case: "an ARN of another Lambda resource",
                value: "arn:aws:lambda:us-west-2:111122223333:function:probe",
                version: Some(1),
                owner: None,
                arn: None,
            },
            Row {
                case: "an ARN of another service",
                value: "arn:aws:s3:::probe",
                version: Some(1),
                owner: None,
                arn: None,
            },
            Row {
                case: "a layer ARN whose version is not a number",
                value: "arn:aws:lambda:us-west-2:111122223333:layer:probe:latest",
                version: Some(1),
                owner: None,
                arn: None,
            },
        ];

        for row in rows {
            let case = row.case;
            let addressed = addressed_layer(row.value);
            let named = addressed.map(|addressed| addressed.account.to_string());
            assert_eq!(
                named.as_deref(),
                row.owner,
                "case {case:?}: expected the owner {:?}, got {named:?}",
                row.owner
            );
            let reported = addressed.map(|addressed| addressed.arn(row.version));
            assert_eq!(
                reported.as_deref(),
                row.arn,
                "case {case:?}: expected the ARN {:?}, got {reported:?}",
                row.arn
            );
        }
    }

    /// RULE: the action a layer call is judged under is the one IAM publishes for it —
    /// `lambda:<ApiName>` for each, and the plain read action for the by-ARN entry point.
    #[test]
    fn every_layer_operation_names_its_published_action() {
        // Every layer operation this service serves, with the action the IAM Service
        // Authorization Reference publishes for it ("Actions defined by AWS Lambda").
        let rows: &[(&str, &str)] = &[
            ("GetLayerVersion", "lambda:GetLayerVersion"),
            ("GetLayerVersionByArn", "lambda:GetLayerVersion"),
            ("GetLayerVersionPolicy", "lambda:GetLayerVersionPolicy"),
            ("AddLayerVersionPermission", "lambda:AddLayerVersionPermission"),
            (
                "RemoveLayerVersionPermission",
                "lambda:RemoveLayerVersionPermission",
            ),
            ("DeleteLayerVersion", "lambda:DeleteLayerVersion"),
            ("ListLayerVersions", "lambda:ListLayerVersions"),
            ("PublishLayerVersion", "lambda:PublishLayerVersion"),
        ];
        for (operation, expected) in rows {
            let actual = layer_action(operation);
            assert_eq!(
                actual, *expected,
                "case {operation:?}: expected the action {expected:?}, got {actual:?}"
            );
        }
    }
}
