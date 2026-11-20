// SPDX-License-Identifier: CC0-1.0

//! The lookups the RESOURCE-POLICY surface addresses a table by.
//!
//! The same lookup [`super::ComAmazonawsDynamodbDynamodb20120810State::table_by_arn`]
//! makes, with the refusal that surface words: it reports a missing resource as the
//! RESOURCE THE REQUEST ADDRESSED — the table by the bare name inside the `ResourceArn`
//! the request carried, and a stream ARN as that stream ([`PolicyResource::not_found`]) —
//! while the tagging surface names the member it was addressed by. One sentence per surface
//! rather than one shared one: a message is part of each surface's contract.

use super::*;

impl ComAmazonawsDynamodbDynamodb20120810State {
    /// The table a RESOURCE-POLICY operation names, or the not-found that surface words.
    ///
    /// A policy addresses a table or that table's stream, and both live on the table
    /// record, so both are looked up here; which of the two a request addressed is
    /// [`PolicyTarget`], and it is the one the refusal names.
    pub fn table_for_policy(&self, resource: &PolicyResource) -> Result<&Table, RequestError> {
        self.tables_in(&resource.region)
            .and_then(|tables| tables.get(&resource.table))
            .ok_or_else(|| resource.not_found())
    }

    /// The same, mutably, for the policy operations that attach or remove a document.
    pub fn table_for_policy_mut(
        &mut self,
        resource: &PolicyResource,
    ) -> Result<&mut Table, RequestError> {
        self.table_mut(&resource.region, &resource.table)
            .map_err(|_| resource.not_found())
    }
}
