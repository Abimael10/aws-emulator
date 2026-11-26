// SPDX-License-Identifier: CC0-1.0

//! WHO MAY LAUNCH an instance from one AMI of the account, beside the account that owns it: the
//! launch permissions `ModifyImageAttribute` grants and revokes, `DescribeImageAttribute`
//! reports and `DescribeImages` narrows its `ExecutableUsers` on.
//!
//! "A new launch permission for the AMI" (`ModifyImageAttributeRequest$LaunchPermission`) is a
//! GRANT recorded against the image's id, and the modelled `LaunchPermission` shape says what one
//! can name: "the Amazon Web Services account ID" (`$UserId`), "the Amazon Resource Name (ARN) of
//! an organization" (`$OrganizationArn`), "the Amazon Resource Name (ARN) of an organizational
//! unit (OU)" (`$OrganizationalUnitArn`), or "the name of the group" (`$Group`, whose only
//! modelled member is [`LAUNCH_PERMISSION_GROUP_ALL`]).
//!
//! The GROUP is not held here. "The value is `true` if this image has public launch permissions"
//! (`Image$Public`) is the stored form of the one group a launch permission can name, so granting
//! that group is what makes an image public and revoking it is what makes it private again — one
//! fact in one place, which is why [`ComAmazonawsEc2Amazonec2State::image_launch_permissions`]
//! keys only the three EXPLICIT grantee kinds, each as a [`BTreeSet`]: a grant is a membership, so
//! granting the same account twice leaves one permission and the order a read reports them in is
//! stable.
//!
//! The OWNER's permission is never recorded either: an account may always launch from an AMI it
//! owns, so that is a fact of the image's `OwnerId` and not of this store — which is why an image
//! nobody has shared holds no entry at all.
//!
//! Three readers ask it, and each asks exactly one of the questions below so they cannot disagree
//! about one grant: the attribute read reports the grants
//! ([`ComAmazonawsEc2Amazonec2State::image_launch_permissions_of`]), the inventory's
//! `ExecutableUsers` narrowing asks whether one account may launch an image
//! ([`ComAmazonawsEc2Amazonec2State::image_is_launchable_by`]), and a permanently deleted AMI
//! drops every grant at once
//! ([`ComAmazonawsEc2Amazonec2State::forget_image_launch_permissions`]): an image the account no
//! longer holds can share nothing.
//!
//! A granted account id or ARN is caller-chosen, so every stored grant is charged against the byte
//! budget this crate's stores share ([`super::MAX_STORED_BYTES`], through
//! [`image_launch_permission_bytes`]) and given back when the grant is revoked: without that
//! charge a caller could share one image with new grantees without bound.

use std::collections::BTreeSet;

use crate::ops::types::{Image, LaunchPermission};

use super::{
    ComAmazonawsEc2Amazonec2State, EXECUTABLE_BY_ALL, STORED_RECORD_OVERHEAD_BYTES, arn_field,
    arn_resource,
};

/// "The name of the group" a launch permission names when it is granted to every account rather
/// than to one (`LaunchPermission$Group`): the `PermissionGroup` enum's only member (the generated
/// catalog [`crate::ops::types::PERMISSION_GROUP_VALUES`]), which is the same term
/// `DescribeImages` selects public AMIs with through its `ExecutableUsers` member.
///
/// DEFINED as that one term ([`EXECUTABLE_BY_ALL`]), so the group a permission is granted to, the
/// group a read reports and the term that selects on it cannot drift apart.
pub const LAUNCH_PERMISSION_GROUP_ALL: &str = EXECUTABLE_BY_ALL;

/// How many grantees one call may name: "Constraints: Up to 10 000 account IDs can be specified in
/// a single request" (`LaunchPermission$UserId`).
///
/// A published bound on the REQUEST rather than on what is stored, and the model declares no
/// `@length` on any list the grantees arrive in, so a handler is the only place it can be enforced
/// — asked before any per-grantee work, so the cost of serving one call is bounded whatever the
/// caller sent.
pub const MAX_IMAGE_LAUNCH_PERMISSION_MODIFICATIONS: usize = 10_000;

/// The service an AWS Organizations ARN names in its service field: "the Amazon Resource Name
/// (ARN) of an organization" is an ARN of AWS Organizations, not of EC2.
const ORGANIZATIONS_ARN_SERVICE: &str = "organizations";

/// The resource type an organization's own ARN names —
/// `arn:<partition>:organizations::<account>:organization/o-<id>`.
const ORGANIZATION_RESOURCE_TYPE: &str = "organization";

/// The resource type an organizational unit's ARN names —
/// `arn:<partition>:organizations::<account>:ou/o-<org>/ou-<org>-<id>`.
const ORGANIZATIONAL_UNIT_RESOURCE_TYPE: &str = "ou";

/// The printed prefix of an organization id.
const ORGANIZATION_ID_PREFIX: &str = "o-";

/// The printed prefix of an organizational unit id.
const ORGANIZATIONAL_UNIT_ID_PREFIX: &str = "ou-";

/// The bytes one stored launch permission is charged against [`super::MAX_STORED_BYTES`]: the
/// fixed per-entry overhead every stored record of this crate costs
/// ([`STORED_RECORD_OVERHEAD_BYTES`]) plus the caller-chosen grantee it names.
///
/// The ONE definition of that charge: a grant asks it before the write, the write charges exactly
/// it, and a revoke gives back exactly it, so the budget cannot drift from what is stored.
pub fn image_launch_permission_bytes(grantee: &str) -> usize {
    STORED_RECORD_OVERHEAD_BYTES.saturating_add(grantee.len())
}

/// Is `arn` the ARN of an AWS Organizations organization —
/// `arn:<partition>:organizations::<account>:organization/o-<id>`?
///
/// The shape of "the Amazon Resource Name (ARN) of an organization"
/// (`LaunchPermission$OrganizationArn`), so a value that names some other service's resource, or
/// no resource at all, grants nothing and is refused rather than recorded.
pub fn organization_arn_is_well_formed(arn: &str) -> bool {
    organizations_resource_id(arn, ORGANIZATION_RESOURCE_TYPE)
        .is_some_and(|id| id.starts_with(ORGANIZATION_ID_PREFIX))
}

/// Is `arn` the ARN of an AWS Organizations ORGANIZATIONAL UNIT —
/// `arn:<partition>:organizations::<account>:ou/o-<org>/ou-<org>-<id>`?
///
/// The shape of "the Amazon Resource Name (ARN) of an organizational unit (OU)"
/// (`LaunchPermission$OrganizationalUnitArn`): the resource field names the organization the OU
/// belongs to and then the OU itself, so the LAST path segment is the one held to the OU prefix.
pub fn organizational_unit_arn_is_well_formed(arn: &str) -> bool {
    organizations_resource_id(arn, ORGANIZATIONAL_UNIT_RESOURCE_TYPE).is_some_and(|id| {
        id.split('/')
            .next_back()
            .is_some_and(|unit| unit.starts_with(ORGANIZATIONAL_UNIT_ID_PREFIX))
    })
}

/// The id the AWS Organizations ARN `arn` names for the resource type `resource_type`, or `None`
/// when `arn` is no ARN of that service and type.
///
/// The ONE reading of an Organizations ARN, so the organization and the OU validators above differ
/// only in the resource type and the prefix they hold the id to.
fn organizations_resource_id<'arn>(arn: &'arn str, resource_type: &str) -> Option<&'arn str> {
    if arn_field(arn, 2)? != ORGANIZATIONS_ARN_SERVICE {
        return None;
    }
    let (kind, id) = arn_resource(arn)?;
    (kind == resource_type).then_some(id)
}

/// The EXPLICIT launch permissions recorded against ONE AMI: the accounts, organizations and
/// organizational units it has been shared with.
///
/// The `all` group is NOT one of these — it is the image's own `Public` member (see the module
/// docs).
#[derive(Debug, Clone, Default)]
pub struct ImageLaunchPermissions {
    /// "The Amazon Web Services account ID" (`LaunchPermission$UserId`), one entry per account
    /// that may launch an instance from the AMI.
    pub user_ids: BTreeSet<String>,
    /// "The Amazon Resource Name (ARN) of an organization" (`LaunchPermission$OrganizationArn`).
    pub organization_arns: BTreeSet<String>,
    /// "The Amazon Resource Name (ARN) of an organizational unit (OU)"
    /// (`LaunchPermission$OrganizationalUnitArn`).
    pub organizational_unit_arns: BTreeSet<String>,
}

impl ImageLaunchPermissions {
    /// Has every explicit grant been revoked, so the image is shared with nobody by name?
    fn is_empty(&self) -> bool {
        self.user_ids.is_empty()
            && self.organization_arns.is_empty()
            && self.organizational_unit_arns.is_empty()
    }

    /// Every grantee this record holds, so the charge it costs and the refund it gives back are
    /// counted over one list.
    fn grantees(&self) -> impl Iterator<Item = &String> {
        self.user_ids
            .iter()
            .chain(self.organization_arns.iter())
            .chain(self.organizational_unit_arns.iter())
    }
}

/// The grantees ONE direction of a launch permission modification names: the accounts, the
/// organizations, the organizational units and the groups.
///
/// Kept apart by KIND because the store records each on its own and because a group is held to a
/// catalog an account id and an ARN are not. One value of this type is what a handler hands the
/// store, so the add and the remove path write and give back exactly the same shape.
#[derive(Debug, Clone, Default)]
pub struct ImageLaunchPermissionGrantees {
    /// "The Amazon Web Services account ID" (`LaunchPermission$UserId`), in the order the
    /// request named them.
    pub user_ids: Vec<String>,
    /// "The Amazon Resource Name (ARN) of an organization" (`LaunchPermission$OrganizationArn`).
    pub organization_arns: Vec<String>,
    /// "The Amazon Resource Name (ARN) of an organizational unit (OU)"
    /// (`LaunchPermission$OrganizationalUnitArn`).
    pub organizational_unit_arns: Vec<String>,
    /// "The name of the group" (`LaunchPermission$Group`), each a member of the modelled
    /// `PermissionGroup` catalog.
    pub groups: Vec<String>,
}

impl ImageLaunchPermissionGrantees {
    /// Does this direction name no grantee at all?
    pub fn is_empty(&self) -> bool {
        self.user_ids.is_empty()
            && self.organization_arns.is_empty()
            && self.organizational_unit_arns.is_empty()
            && self.groups.is_empty()
    }

    /// How many modifications these grantees make: one per grantee named, whichever kind it is.
    pub fn len(&self) -> usize {
        self.user_ids
            .len()
            .saturating_add(self.organization_arns.len())
            .saturating_add(self.organizational_unit_arns.len())
            .saturating_add(self.groups.len())
    }

    /// Does this direction name `group` — the question the rules on PUBLIC sharing are asked
    /// about?
    pub fn names_group(&self, group: &str) -> bool {
        self.groups.iter().any(|named| named == group)
    }

    /// Take on the grantees of `other`, which is how the flat form's lists join the structured
    /// form's list for the same direction.
    pub fn extend(&mut self, other: Self) {
        self.user_ids.extend(other.user_ids);
        self.organization_arns.extend(other.organization_arns);
        self.organizational_unit_arns
            .extend(other.organizational_unit_arns);
        self.groups.extend(other.groups);
    }

    /// The EXPLICIT grantees — everything but the group, which is the image's `Public` member.
    fn explicit(&self) -> impl Iterator<Item = &String> {
        self.user_ids
            .iter()
            .chain(self.organization_arns.iter())
            .chain(self.organizational_unit_arns.iter())
    }
}

impl ComAmazonawsEc2Amazonec2State {
    /// "The launch permissions" of `image` (`ImageAttribute$LaunchPermissions`), out of what the
    /// store holds about it.
    ///
    /// Two sources, one answer: an image with public launch permissions ("the value is `true` if
    /// this image has public launch permissions", `Image$Public`) carries the one permission AWS
    /// reports as a GROUP rather than as a grantee, and beside it the image carries whatever
    /// `ModifyImageAttribute` has granted on it by name. The owner's own permission is implicit
    /// and so is not one of the permissions this attribute lists.
    ///
    /// The group is reported first, then each kind of explicit grant in the stable order its set
    /// holds, so two reads of one image report one list.
    pub fn image_launch_permissions_of(&self, image: &Image) -> Vec<LaunchPermission> {
        let mut reported: Vec<LaunchPermission> = Vec::new();
        if image.public.unwrap_or(false) {
            reported.push(LaunchPermission {
                group: Some(LAUNCH_PERMISSION_GROUP_ALL.to_string()),
                ..LaunchPermission::default()
            });
        }
        let Some(held) = image
            .image_id
            .as_deref()
            .and_then(|image_id| self.image_launch_permissions.get(image_id))
        else {
            return reported;
        };
        reported.extend(held.user_ids.iter().map(|user_id| LaunchPermission {
            user_id: Some(user_id.clone()),
            ..LaunchPermission::default()
        }));
        reported.extend(held.organization_arns.iter().map(|arn| LaunchPermission {
            organization_arn: Some(arn.clone()),
            ..LaunchPermission::default()
        }));
        reported.extend(
            held.organizational_unit_arns
                .iter()
                .map(|arn| LaunchPermission {
                    organizational_unit_arn: Some(arn.clone()),
                    ..LaunchPermission::default()
                }),
        );
        reported
    }

    /// May the account `user_id` launch an instance from `image`?
    ///
    /// "Only images with public launch permissions and images owned by the specified account, or
    /// for which the specified account has explicit launch permissions, are returned"
    /// (`DescribeImagesRequest$ExecutableUsers`): the owner's implicit permission, or an explicit
    /// grant to that ACCOUNT. An organization or OU grant names no account id — this emulator
    /// holds no organization membership, so it cannot say which accounts one covers — so it is
    /// reported as a permission without being read as a permission of any particular account.
    pub fn image_is_launchable_by(&self, image: &Image, user_id: &str) -> bool {
        if image.owner_id.as_deref() == Some(user_id) {
            return true;
        }
        image
            .image_id
            .as_deref()
            .and_then(|image_id| self.image_launch_permissions.get(image_id))
            .is_some_and(|held| held.user_ids.contains(user_id))
    }

    /// The bytes granting `grantees` on `image_id` would ADD to the store: the charge of the
    /// grants that are not recorded yet, a grant that already stands costing nothing further.
    ///
    /// Asked by a handler BEFORE it writes, so a request the budget has no room for is refused
    /// rather than half-applied.
    pub fn image_launch_permission_charge(
        &self,
        image_id: &str,
        grantees: &ImageLaunchPermissionGrantees,
    ) -> usize {
        let recorded = self.image_launch_permissions.get(image_id);
        let charge_of = |grantee: &String, held: Option<&BTreeSet<String>>| {
            if held.is_some_and(|held| held.contains(grantee)) {
                0
            } else {
                image_launch_permission_bytes(grantee)
            }
        };
        let accounts = grantees
            .user_ids
            .iter()
            .map(|user_id| charge_of(user_id, recorded.map(|held| &held.user_ids)));
        let organizations = grantees
            .organization_arns
            .iter()
            .map(|arn| charge_of(arn, recorded.map(|held| &held.organization_arns)));
        let units = grantees
            .organizational_unit_arns
            .iter()
            .map(|arn| charge_of(arn, recorded.map(|held| &held.organizational_unit_arns)));
        accounts
            .chain(organizations)
            .chain(units)
            .fold(0_usize, |total, bytes| total.saturating_add(bytes))
    }

    /// Grant `grantees` launch permissions on the AMI `image_id` names.
    ///
    /// The ONE writer of a grant, so every grant is charged against the byte budget exactly once:
    /// a grantee already recorded is left as it stands and charged nothing, which is what makes
    /// repeating a grant idempotent rather than a way to grow the store. Granting the
    /// [`LAUNCH_PERMISSION_GROUP_ALL`] group is what makes the image PUBLIC, so it is written onto
    /// the image's own `Public` member rather than recorded here.
    pub fn grant_image_launch_permissions(
        &mut self,
        image_id: &str,
        grantees: &ImageLaunchPermissionGrantees,
    ) {
        if grantees.names_group(LAUNCH_PERMISSION_GROUP_ALL) {
            self.set_image_public(image_id, true);
        }
        if grantees.explicit().next().is_none() {
            return;
        }
        let charged = {
            let held = self
                .image_launch_permissions
                .entry(image_id.to_string())
                .or_default();
            let mut charged = 0_usize;
            for (granted, into) in [
                (&grantees.user_ids, &mut held.user_ids),
                (&grantees.organization_arns, &mut held.organization_arns),
                (
                    &grantees.organizational_unit_arns,
                    &mut held.organizational_unit_arns,
                ),
            ] {
                for grantee in granted {
                    if into.insert(grantee.clone()) {
                        charged = charged.saturating_add(image_launch_permission_bytes(grantee));
                    }
                }
            }
            charged
        };
        self.add_stored_bytes(charged);
    }

    /// Revoke the launch permissions `grantees` names on the AMI `image_id` names.
    ///
    /// The mirror of [`Self::grant_image_launch_permissions`]: the bytes each revoked grant was
    /// charged are given back, and an image left with no explicit grant at all drops its entry, so
    /// the store holds only the images that are actually shared by name. A grant that was never
    /// made is nothing to revoke, which is what makes a repeated remove idempotent — and revoking
    /// the group is what makes a public image private again.
    pub fn revoke_image_launch_permissions(
        &mut self,
        image_id: &str,
        grantees: &ImageLaunchPermissionGrantees,
    ) {
        if grantees.names_group(LAUNCH_PERMISSION_GROUP_ALL) {
            self.set_image_public(image_id, false);
        }
        let Some(held) = self.image_launch_permissions.get_mut(image_id) else {
            return;
        };
        let mut refund = 0_usize;
        for (revoked, from) in [
            (&grantees.user_ids, &mut held.user_ids),
            (&grantees.organization_arns, &mut held.organization_arns),
            (
                &grantees.organizational_unit_arns,
                &mut held.organizational_unit_arns,
            ),
        ] {
            for grantee in revoked {
                if from.remove(grantee) {
                    refund = refund.saturating_add(image_launch_permission_bytes(grantee));
                }
            }
        }
        if held.is_empty() {
            self.image_launch_permissions.remove(image_id);
        }
        self.release_stored_bytes(refund);
    }

    /// Clear every explicit launch permission recorded against the AMI `image_id` names, giving
    /// back the bytes they were charged, and answer whether there were any.
    ///
    /// An AMI that is permanently gone can share nothing and must not pass its grants on, which is
    /// the same rule the UEFI variable store held beside an image leaves under
    /// ([`ComAmazonawsEc2Amazonec2State::image_uefi_data`]). The image's own `Public` member is
    /// not touched: it travels with the [`Image`] record itself.
    pub fn forget_image_launch_permissions(&mut self, image_id: &str) -> bool {
        let Some(held) = self.image_launch_permissions.remove(image_id) else {
            return false;
        };
        let refund = held
            .grantees()
            .map(|grantee| image_launch_permission_bytes(grantee))
            .fold(0_usize, |total, bytes| total.saturating_add(bytes));
        self.release_stored_bytes(refund);
        true
    }

    /// Write whether the AMI `image_id` names has PUBLIC launch permissions.
    ///
    /// The one writer of `Image$Public` after a register, so the member a read reports and the
    /// group a launch permission is granted to cannot disagree. A fixed-size member, so nothing is
    /// charged for it.
    fn set_image_public(&mut self, image_id: &str, public: bool) {
        if let Some(image) = self.images.get_mut(image_id) {
            image.public = Some(public);
        }
    }
}
