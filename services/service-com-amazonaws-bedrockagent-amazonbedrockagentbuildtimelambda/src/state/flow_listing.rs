// SPDX-License-Identifier: CC0-1.0

//! The LISTING of the account's flows: what `ListFlows` enumerates, the order it
//! reports it in, the page it hands out and the cursor a page resumes from.
//!
//! One summary per stored flow, each projected off the SAME record `GetFlow`
//! reports the flow from, so a summary and a read of one flow never disagree. A
//! flow appears here exactly while the store holds it, whatever status it stands
//! in: a refused create is in no listing, and a delete takes its flow out of this
//! one.
//!
//! The order is the one every other listing of this service pages over: when the
//! flow was created, ties broken by its minted id, neither of which ever changes,
//! which is what lets a page resume exactly after the one before it.

use super::*;

use crate::ops::types::FlowSummary;

/// The most flow summaries one `ListFlows` page carries: the response member
/// `flowSummaries` is modelled `length ..10`, so a page never exceeds it however
/// large the request's `maxResults` (`range 1..1000`) is.
pub const MAX_FLOWS_PER_PAGE: usize = 10;

impl FlowRecord {
    /// This record as the modelled `FlowSummary` shape, the `ListFlows` item.
    /// Every member comes from the same stored value [`FlowRecord::to_get_output`]
    /// reports it from, so the listing and the read of one flow agree.
    pub fn to_summary(&self) -> FlowSummary {
        FlowSummary {
            name: self.name.clone(),
            description: self.description.clone(),
            id: self.flow_id.clone(),
            arn: self.arn.clone(),
            status: self.status.as_wire().to_string(),
            created_at: to_date_time(self.created_at),
            updated_at: to_date_time(self.updated_at),
            version: self.version.clone(),
        }
    }

    /// The total order `ListFlows` pages the account's flows over: when the flow
    /// was created, ties broken by its minted id.
    fn sort_key(&self) -> (u128, &str) {
        (epoch_nanos(self.created_at), self.flow_id.as_str())
    }

    /// The `nextToken` a page ending at this flow hands out.
    pub fn page_cursor(&self) -> String {
        let (created_at_nanos, flow_id) = self.sort_key();
        format!("{created_at_nanos}-{flow_id}")
    }
}

/// A `ListFlows` resume cursor, decoded back into the position in the listing's
/// order that it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowPageCursor {
    created_at_nanos: u128,
    flow_id: String,
}

impl FlowPageCursor {
    /// Decode a `nextToken` a previous page handed out. `None` when the token was
    /// not produced by [`FlowRecord::page_cursor`] — only the one spelling that
    /// function writes names a position in this listing.
    pub fn parse(token: &str) -> Option<Self> {
        let (created_at_nanos, flow_id) = parse_composite_cursor(token)?;
        Some(Self {
            created_at_nanos,
            flow_id: flow_id.to_string(),
        })
    }

    /// Whether `record` sorts after this cursor, i.e. belongs on the next page.
    fn precedes(&self, record: &FlowRecord) -> bool {
        (self.created_at_nanos, self.flow_id.as_str()) < record.sort_key()
    }
}

impl ComAmazonawsBedrockagentAmazonbedrockagentbuildtimelambdaState {
    /// One `ListFlows` page: the flows after `cursor` in creation order, at most
    /// `page_size` of them, plus the token to resume from when more remain.
    ///
    /// Every flow the store holds is reported, whatever status it stands in, and
    /// an account holding none reports an empty listing — there is no flow whose
    /// absence makes this a missing resource.
    pub fn page_of_flows(
        &self,
        cursor: Option<&FlowPageCursor>,
        page_size: usize,
    ) -> (Vec<FlowSummary>, Option<String>) {
        let mut ordered: Vec<&FlowRecord> = self
            .flows
            .values()
            .filter(|record| cursor.is_none_or(|cursor| cursor.precedes(record)))
            .collect();
        ordered.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));

        let page: Vec<&FlowRecord> = ordered.iter().take(page_size.max(1)).copied().collect();
        let next_token = (ordered.len() > page.len())
            .then(|| page.last().map(|record| record.page_cursor()))
            .flatten();
        (
            page.iter().map(|record| record.to_summary()).collect(),
            next_token,
        )
    }
}
