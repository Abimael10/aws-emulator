// SPDX-License-Identifier: CC0-1.0

//! What a `ResourceARN` the tagging family addresses resolves to, and the provenance record
//! that makes that answer possible.
//!
//! The three ARN-addressed operations — `TagResource`, `UntagResource` and
//! `ListTagsForResource` — name the resource they act on by the scalable-target ARN this
//! service minted for it, so each of them has three conditions to tell apart, and each is a
//! different answer:
//!
//! * the ARN names a scalable target this Region holds right now, and the call is served;
//! * the ARN is one this Region MINTED, for a target that is no longer registered: the
//!   resource the ARN names is reported ABSENT, because the service knows which resource was
//!   asked about and that it is gone;
//! * the ARN is one this Region never minted: the service holds no resource of that identity
//!   to speak about at all, so the call is refused as a request about a resource the caller
//!   does not reach, disclosing nothing about whether such a resource exists.
//!
//! What separates the last two is the ARN's PROVENANCE, not a miss in the live-target map —
//! both of them miss it. An endpoint answers for the scalable targets of its own Region, so
//! provenance is per Region too: an ARN minted for another Region's target is one this
//! endpoint never minted. [`RetiredTargetArns`] is what keeps an ARN's provenance after the
//! target it was minted for has gone.

use std::collections::BTreeMap;

use super::{ComAmazonawsApplicationautoscalingAnyscalefrontendserviceState as Store, TargetKey};

/// How many retired scalable-target ARNs the provenance record keeps before the
/// longest-retired are dropped.
///
/// The EMULATOR's own bound, for the reason [`super::MAX_SCALABLE_TARGETS`] is one: `targets`
/// bounds how many ARNs are live at once, but nothing bounds how many register/deregister
/// cycles a long-lived emulator serves, so without a ceiling N cycles leave N resident entries
/// for ever. It is set far above any realistic workflow, so nothing a test or a client does
/// legitimately reaches it, and past it the longest-retired ARN is forgotten first.
pub const MAX_RETIRED_TARGET_ARNS: usize = 10_000;

/// What a `ResourceARN` of the tagging family resolves to at one Region's endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaggedTargetArn {
    /// A scalable target this Region holds, named by its identity.
    Registered(TargetKey),
    /// An ARN this Region minted, for a target it no longer holds.
    Retired,
    /// An ARN this Region never minted.
    NeverMinted,
}

/// One retired ARN's provenance: the Region that minted it, and when it was retired.
#[derive(Debug, Clone)]
struct RetiredTargetArn {
    /// The Region of the target the ARN was minted for, which the ARN itself names.
    region: String,
    /// Which retirement this was. Only grows, so it orders the record.
    sequence: u64,
}

/// The scalable-target ARNs this service minted for targets that are no longer registered.
///
/// Bounded by [`MAX_RETIRED_TARGET_ARNS`]: past it the longest-retired entry is dropped, and
/// its ARN becomes one this Region cannot tell from an ARN it never minted.
#[derive(Debug, Default)]
pub struct RetiredTargetArns {
    /// ARN -> its provenance.
    entries: BTreeMap<String, RetiredTargetArn>,
    /// Retirement order -> the ARN retired then, so the longest-retired entry is the first key
    /// rather than a scan of [`Self::entries`]. One entry per entry above.
    order: BTreeMap<u64, String>,
    /// How many ARNs have ever been retired, which is what orders the record.
    retired: u64,
}

impl RetiredTargetArns {
    /// Keep `arn` as one `region` minted for a target that is no longer registered.
    ///
    /// Recording the same ARN again refreshes its place in the order rather than adding a
    /// second entry: an ARN is minted once, so it is one resource however often it is retired.
    pub fn record(&mut self, arn: &str, region: &str) {
        let sequence = self.retired;
        self.retired += 1;
        let replaced = self.entries.insert(
            arn.to_string(),
            RetiredTargetArn {
                region: region.to_string(),
                sequence,
            },
        );
        if let Some(replaced) = replaced {
            self.order.remove(&replaced.sequence);
        }
        self.order.insert(sequence, arn.to_string());
        while self.entries.len() > MAX_RETIRED_TARGET_ARNS {
            let longest_retired = self.order.keys().next().copied();
            match longest_retired.and_then(|sequence| self.order.remove(&sequence)) {
                Some(arn) => {
                    self.entries.remove(&arn);
                }
                None => break,
            }
        }
    }

    /// Whether `region` minted `arn` for a target it no longer holds.
    pub fn holds(&self, arn: &str, region: &str) -> bool {
        self.entries
            .get(arn)
            .is_some_and(|entry| entry.region == region)
    }

    /// How many retirements are kept, which [`MAX_RETIRED_TARGET_ARNS`] bounds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing has been retired yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Store {
    /// What `resource_arn` names for a request served at `region`.
    ///
    /// The one resolution the whole tagging family shares, so the three operations cannot tell
    /// the same ARN apart in three ways. The live index is consulted first and DECIDES: an
    /// entry in it is itself proof that this Region minted the ARN, so one whose target the
    /// store no longer holds is a retirement — whether or not the removal paired with
    /// [`Store::retire_target_arn`] — and only an ARN neither source knows is one never minted
    /// here.
    pub fn resolve_tagged_target_arn(&self, resource_arn: &str, region: &str) -> TaggedTargetArn {
        let minted_here = self
            .arn_index
            .get(resource_arn)
            .filter(|key| key.region == region);
        if let Some(key) = minted_here {
            return if self.targets.contains_key(key) {
                TaggedTargetArn::Registered(key.clone())
            } else {
                TaggedTargetArn::Retired
            };
        }
        if self.retired_target_arns.holds(resource_arn, region) {
            return TaggedTargetArn::Retired;
        }
        TaggedTargetArn::NeverMinted
    }

    /// Retire `arn`: the target `key` named is no longer registered, so the ARN stops resolving
    /// to it — and this Region keeps it as one it MINTED, which is what tells it apart from an
    /// ARN nothing here ever minted.
    pub fn retire_target_arn(&mut self, arn: &str, key: &TargetKey) {
        self.arn_index.remove(arn);
        self.retired_target_arns.record(arn, &key.region);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ARN of `region`, as this service's own minting shapes one.
    fn arn(region: &str, id: usize) -> String {
        format!("arn:aws:application-autoscaling:{region}:000000000000:scalable-target/{id:036}")
    }

    /// The record is per Region: an ARN one Region retired is not one another Region minted.
    #[test]
    fn provenance_is_scoped_to_the_region_that_minted_the_arn() {
        let mut retired = RetiredTargetArns::default();
        assert!(retired.is_empty(), "expected a fresh record to hold nothing");
        let subject = arn("us-east-1", 1);
        retired.record(&subject, "us-east-1");
        assert!(
            retired.holds(&subject, "us-east-1"),
            "case \"the minting Region\": expected the ARN to be held, got {retired:?}"
        );
        assert!(
            !retired.holds(&subject, "eu-west-1"),
            "case \"another Region\": expected the ARN not to be held, got {retired:?}"
        );
        // Retiring the same ARN again is the same one resource, not a second entry.
        retired.record(&subject, "us-east-1");
        assert_eq!(
            retired.len(),
            1,
            "case \"the same ARN twice\": expected one entry, got {retired:?}"
        );
    }

    /// The record is BOUNDED: past the cap the longest-retired ARN is forgotten, and the
    /// most recent retirements are the ones still told apart from an ARN never minted.
    #[test]
    fn the_record_drops_the_longest_retired_arn_past_its_bound() {
        let mut retired = RetiredTargetArns::default();
        let first = arn("us-east-1", 0);
        for id in 0..=MAX_RETIRED_TARGET_ARNS {
            retired.record(&arn("us-east-1", id), "us-east-1");
        }
        assert_eq!(
            retired.len(),
            MAX_RETIRED_TARGET_ARNS,
            "case \"one past the bound\": expected the record to stay at its bound"
        );
        assert!(
            !retired.holds(&first, "us-east-1"),
            "case \"the longest-retired ARN\": expected it to be forgotten past the bound"
        );
        for id in 1..=MAX_RETIRED_TARGET_ARNS {
            let kept = arn("us-east-1", id);
            assert!(
                retired.holds(&kept, "us-east-1"),
                "case \"retirement {id}\": expected it to still be held"
            );
        }
    }
}
