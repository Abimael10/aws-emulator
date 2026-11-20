// SPDX-License-Identifier: CC0-1.0

//! The PRINCIPAL an origin access identity is named by in another service's resource-based
//! policy, and whether this account holds the identity such a value names.
//!
//! An origin access identity exists so that "viewers can only access objects in an Amazon S3
//! bucket through CloudFront": the grant is written in the ORIGIN's own policy, naming the
//! identity rather than the caller's account, and AWS publishes two spellings for it — the
//! IAM principal of CloudFront's OWN account
//! (`arn:aws:iam::cloudfront:user/CloudFront Origin Access Identity <id>`, the spelling the
//! Amazon S3 User Guide's origin-access-identity bucket policy carries) and the
//! `S3CanonicalUserId` the identity was minted with, which is the value a `CanonicalUser`
//! principal names it by.
//!
//! The first spelling can name NOTHING ELSE: its account field is this service's own name
//! where an ARN of a customer resource carries an account id, and its resource is the fixed
//! phrase below followed by the identity's `Id`. So it is the one value of another service's
//! policy document whose existence question only CloudFront can answer, and this module is
//! both halves of that answer — the one place the spelling is BUILT
//! ([`origin_access_identity_principal_arn`], read by the edge when it makes an origin request
//! as the identity) and the one place it is READ back
//! ([`origin_access_identity_principal_id`], asked by the service holding the document). The
//! `S3CanonicalUserId` spelling is deliberately not read here: a canonical user id names an
//! account owner of any account just as well as an identity of this one, so nothing in it says
//! it was meant as an origin access identity.
//!
//! [`holds_origin_access_identity`] is the existence read itself. It takes and releases this
//! service's own store lock inside the call, with no guard of the caller's store held, which
//! is what keeps the cross-service read one-directional.

use super::*;

/// The account field of the IAM principal an origin access identity is named by: an origin
/// access identity belongs to CloudFront's own account rather than to the caller's, so the
/// field carries this service's name where an ARN of a customer's resource carries an id.
///
/// PRIVATE, like the vendor and the resource phrase beside it: this module is the one builder
/// and the one reader of the spelling, so nothing outside it has a use for the pieces.
const ORIGIN_ACCESS_IDENTITY_ACCOUNT: &str = "cloudfront";

/// The vendor field of that principal: the identity is an IAM user of that account.
const ORIGIN_ACCESS_IDENTITY_VENDOR: &str = "iam";

/// The resource that principal names, ahead of the identity's own id — the fixed phrase of
/// `arn:aws:iam::cloudfront:user/CloudFront Origin Access Identity <id>`.
const ORIGIN_ACCESS_IDENTITY_USER_PREFIX: &str = "user/CloudFront Origin Access Identity ";

/// The IAM principal the origin access identity `id` is named by, in `partition`.
///
/// IAM is a GLOBAL service, so the Region field is empty; the partition is the caller's own,
/// read from the ARN of the resource the grant is about rather than fixed here.
#[must_use]
pub fn origin_access_identity_principal_arn(partition: &str, id: &str) -> String {
    format!(
        "arn:{partition}:{ORIGIN_ACCESS_IDENTITY_VENDOR}::\
         {ORIGIN_ACCESS_IDENTITY_ACCOUNT}:{ORIGIN_ACCESS_IDENTITY_USER_PREFIX}{id}"
    )
}

/// The `Id` of the origin access identity `principal` names, or `None` for a value that names
/// no origin access identity at all.
///
/// The inverse of [`origin_access_identity_principal_arn`] over every partition: an `iam` ARN
/// of CloudFront's own account, carrying no Region, whose resource is the fixed phrase
/// followed by a non-empty id. A value failing any of those names some other principal — a
/// customer's IAM entity, a service principal, a canonical user — and is none of this
/// service's business.
#[must_use]
pub fn origin_access_identity_principal_id(principal: &str) -> Option<&str> {
    let arn = ::service_common::arn::parse(principal)?;
    if arn.vendor != ORIGIN_ACCESS_IDENTITY_VENDOR
        || !arn.region.is_empty()
        || arn.account != ORIGIN_ACCESS_IDENTITY_ACCOUNT
    {
        return None;
    }
    arn.resource
        .strip_prefix(ORIGIN_ACCESS_IDENTITY_USER_PREFIX)
        .filter(|id| !id.is_empty())
}

/// Whether this account holds an origin access identity under `id`.
///
/// `Some(true)` for an identity the account holds, `Some(false)` for an id it holds none
/// under — the answer a service resolving a policy principal acts on. `None` when this engine
/// carries no CloudFront store at all: a store that cannot be read answers the existence
/// question not at all, and is never a reason to treat the identity as absent.
#[must_use]
pub fn holds_origin_access_identity<Eng: Send + Sync + 'static>(
    engine: &Eng,
    id: &str,
) -> Option<bool> {
    let state = service_state(engine)?;
    let held = state.lock().origin_access_identity(id).is_some();
    Some(held)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id form `CloudFrontOriginAccessIdentity$Id` documents by example
    /// ("E74FTE3AJFJ256A").
    const AN_IDENTITY_ID: &str = "E74FTE3AJFJ256A";

    /// RULE: the principal this module builds for an identity is the one it reads that
    /// identity's id back out of, in every partition — and a value that is not that spelling
    /// names no origin access identity.
    ///
    /// The rows differ along what the rule generalises over: the partition the ARN is written
    /// in, and HOW a value misses the spelling (another vendor, a customer's account, a
    /// Region, another resource type, an empty id, a canonical user id, a value that is no ARN
    /// at all).
    #[test]
    fn the_identity_principal_round_trips_and_nothing_else_names_one() {
        for partition in ["aws", "aws-cn", "aws-us-gov"] {
            let built = origin_access_identity_principal_arn(partition, AN_IDENTITY_ID);
            let read = origin_access_identity_principal_id(&built);
            assert_eq!(
                read,
                Some(AN_IDENTITY_ID),
                "case {partition:?}: the principal {built:?} must name {AN_IDENTITY_ID:?}, got \
                 {read:?}"
            );
        }

        let rows: [(&str, &str); 7] = [
            (
                "a customer's IAM user",
                "arn:aws:iam::000000000000:user/builder",
            ),
            (
                "the same phrase under another vendor",
                "arn:aws:cloudfront::cloudfront:user/CloudFront Origin Access Identity \
                 E74FTE3AJFJ256A",
            ),
            (
                "the same phrase carrying a Region",
                "arn:aws:iam:us-east-1:cloudfront:user/CloudFront Origin Access Identity \
                 E74FTE3AJFJ256A",
            ),
            (
                "another resource of CloudFront's own account",
                "arn:aws:iam::cloudfront:role/CloudFront Origin Access Identity \
                 E74FTE3AJFJ256A",
            ),
            (
                "the phrase followed by no id at all",
                "arn:aws:iam::cloudfront:user/CloudFront Origin Access Identity ",
            ),
            (
                "a canonical user id",
                "4bbd0ac5a9ab4a0b0b4a0b4a0b4a0b4a0b4a0b4a0b4a0b4a0b4a0b4a0b4a0b4a",
            ),
            ("a value that is no ARN at all", "nobody"),
        ];
        for (case, value) in rows {
            let read = origin_access_identity_principal_id(value);
            assert_eq!(
                read, None,
                "case {case:?}: {value:?} must name no origin access identity, got {read:?}"
            );
        }
    }
}
