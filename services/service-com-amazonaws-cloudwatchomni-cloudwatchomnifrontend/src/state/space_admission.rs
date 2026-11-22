// SPDX-License-Identifier: CC0-1.0

//! The rule a space create is admitted under, and the text its refusal carries.
//!
//! A CloudWatchOmni space is the account's single resource of its kind in a Region: an
//! account holds at most ONE space per Region, whichever domain the create names. A
//! create made while the account already holds a space in the Region the request is
//! signed for is therefore a conflict with that space rather than a second record, and
//! the Region's slot is free again only once that space is deleted.
//!
//! The model fixes no text for the refusal — `ConflictException` carries a required
//! `message` and nothing more — so the wording is recorded behavior and lives here as ONE
//! template, reached by every operation that applies the rule.

/// The message a create that would mint a second space for one account in one Region
/// earns.
///
/// Fixed apart from the two values the rule itself is about: the account the caller acts
/// in and the Region the request was signed for. Nothing else of the request reaches it —
/// neither the name it asked for nor the space already held, because the conflict is with
/// the account's Region rather than with either name.
pub fn space_already_exists_message(account_id: &str, region: &str) -> String {
    format!("A space already exists for account {account_id} in region {region}")
}

impl super::ComAmazonawsCloudwatchomniCloudwatchomnifrontendState {
    /// The space the account already holds in `region`, if it holds one.
    ///
    /// Read from the one collection of spaces every other operation reads, so a space
    /// just created occupies the Region's slot for the create that follows it and a space
    /// just deleted releases it.
    ///
    /// Linear in the number of stored spaces, which [`super::SPACE_QUOTA`] bounds.
    #[must_use]
    pub fn space_in_region(&self, region: &str) -> Option<&super::StoredSpace> {
        self.spaces
            .values()
            .find(|stored| stored.space.region == region)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the refusal states the caller's account and the request's Region, and takes
    /// both verbatim.
    #[test]
    fn the_refusal_names_the_account_and_the_region_it_is_about() {
        // Rows span the two variables: the emulator's own account id beside another
        // well-formed 12-digit id, and Regions from three partitions' naming forms
        // (`AccountId`'s modelled `^[0-9]{12}$`; the Region is the credential scope's).
        for (case, account_id, region, expected) in [
            (
                "the emulator's account in a us Region",
                "000000000000",
                "us-west-2",
                "A space already exists for account 000000000000 in region us-west-2",
            ),
            (
                "another account in an eu Region",
                "123456789012",
                "eu-central-1",
                "A space already exists for account 123456789012 in region eu-central-1",
            ),
            (
                "an ap Region",
                "000000000000",
                "ap-southeast-4",
                "A space already exists for account 000000000000 in region ap-southeast-4",
            ),
        ] {
            let actual = space_already_exists_message(account_id, region);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
