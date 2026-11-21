// SPDX-License-Identifier: CC0-1.0

//! The LISTING of the prompt library: what `ListPrompts` enumerates, the order it
//! reports it in, the page it hands out and the cursor a page resumes from.
//!
//! `ListPrompts` is documented "Returns either information about the working draft
//! (DRAFT version) of each prompt in an account, or information about of all versions of
//! a prompt, depending on whether you include the promptIdentifier field or not", so the
//! listing has two enumerations and one summary shape:
//!
//! * no `promptIdentifier` — one summary per stored prompt, each the prompt's WORKING
//!   DRAFT, in the order every other listing of this service pages over: when the prompt
//!   was created, ties broken by its minted id, neither of which ever changes, which is
//!   what lets a page resume exactly after the one before it;
//! * a `promptIdentifier` — every version the prompt it names holds: its working draft,
//!   which the listing reports first, followed by the numbered snapshots cut from it in
//!   the order they were cut, paged over the same [`VersionCursor`] every version
//!   listing of this service shares.
//!
//! Both enumerations project onto the modelled `PromptSummary` off the stored record the
//! full read reports, so a summary and a read of the same version never disagree.

use super::*;

use crate::ops::types::PromptSummary;

/// The most prompt summaries one `ListPrompts` page carries: the response member
/// `promptSummaries` is modelled `length ..10`, so a page never exceeds it however large
/// the request's `maxResults` (`range 1..1000`) is.
pub const MAX_PROMPTS_PER_PAGE: usize = 10;

impl PromptRecord {
    /// This record as the modelled `PromptSummary` shape: the prompt's WORKING DRAFT, as
    /// `ListPrompts` reports it for each prompt in the account. Every member comes from
    /// the same stored value the full projection reports it from.
    pub fn to_summary(&self) -> PromptSummary {
        PromptSummary {
            name: self.name.clone(),
            description: self.description.clone(),
            id: self.prompt_id.clone(),
            arn: self.arn.clone(),
            version: self.version.clone(),
            created_at: to_date_time(self.created_at),
            updated_at: to_date_time(self.updated_at),
        }
    }

    /// The total order `ListPrompts` pages the account's prompts over: when the prompt
    /// was created, ties broken by its minted id.
    fn sort_key(&self) -> (u128, &str) {
        (epoch_nanos(self.created_at), self.prompt_id.as_str())
    }

    /// The `nextToken` a page ending at this prompt hands out.
    pub fn page_cursor(&self) -> String {
        let (created_at_nanos, prompt_id) = self.sort_key();
        format!("{created_at_nanos}-{prompt_id}")
    }
}

impl PromptVersionRecord {
    /// This snapshot as the modelled `PromptSummary` shape, the item one prompt's version
    /// listing reports for a numbered version: the values the version was cut with, from
    /// the stored snapshot, so a later change to the draft never moves it. Its `arn` is
    /// the version's own — the prompt's ARN qualified with the version number — which is
    /// what `PromptSummary$arn` documents as "the ARN of the prompt or the prompt version
    /// (if you specified a version in the request)".
    pub fn to_summary(&self) -> PromptSummary {
        PromptSummary {
            name: self.name.clone(),
            description: self.description.clone(),
            id: self.prompt_id.clone(),
            arn: self.arn.clone(),
            version: self.version.to_string(),
            created_at: to_date_time(self.created_at),
            updated_at: to_date_time(self.updated_at),
        }
    }
}

/// A `ListPrompts` resume cursor over the ACCOUNT's prompts, decoded back into the
/// position in the listing's order that it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptPageCursor {
    created_at_nanos: u128,
    prompt_id: String,
}

impl PromptPageCursor {
    /// Decode a `nextToken` a previous page handed out. `None` when the token was not
    /// produced by [`PromptRecord::page_cursor`] — only the one spelling that function
    /// writes names a position in this listing.
    pub fn parse(token: &str) -> Option<Self> {
        let (created_at_nanos, prompt_id) = parse_composite_cursor(token)?;
        Some(Self {
            created_at_nanos,
            prompt_id: prompt_id.to_string(),
        })
    }

    /// Whether `record` sorts after this cursor, i.e. belongs on the next page.
    fn precedes(&self, record: &PromptRecord) -> bool {
        (self.created_at_nanos, self.prompt_id.as_str()) < record.sort_key()
    }
}

impl ComAmazonawsBedrockagentAmazonbedrockagentbuildtimelambdaState {
    /// One `ListPrompts` page of the ACCOUNT's prompts: the working draft of each prompt
    /// after `cursor` in creation order, at most `page_size` of them, plus the token to
    /// resume from when more remain.
    ///
    /// Every prompt the store holds is reported, and an account holding none reports an
    /// empty listing — there is no prompt whose absence makes this a missing resource.
    pub fn page_of_prompts(
        &self,
        cursor: Option<&PromptPageCursor>,
        page_size: usize,
    ) -> (Vec<PromptSummary>, Option<String>) {
        let mut ordered: Vec<&PromptRecord> = self
            .prompts
            .values()
            .filter(|record| cursor.is_none_or(|cursor| cursor.precedes(record)))
            .collect();
        ordered.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));

        let page: Vec<&PromptRecord> = ordered.iter().take(page_size.max(1)).copied().collect();
        let next_token = (ordered.len() > page.len())
            .then(|| page.last().map(|record| record.page_cursor()))
            .flatten();
        (
            page.iter().map(|record| record.to_summary()).collect(),
            next_token,
        )
    }

    /// One `ListPrompts` page of ONE prompt's versions: the versions this prompt holds
    /// after `after` in listing order, at most `page_size` of them, plus the token to
    /// resume from when more remain.
    ///
    /// A prompt that exists always has a version to report: its working draft, which the
    /// listing reports first, followed by the numbered versions cut from it in the order
    /// they were cut. An unknown prompt has nothing to enumerate — the caller refuses it
    /// as a missing resource before asking.
    pub fn page_of_prompt_versions(
        &self,
        prompt_id: &str,
        after: Option<VersionCursor>,
        page_size: usize,
    ) -> (Vec<PromptSummary>, Option<String>) {
        let Some(prompt) = self.prompts.get(prompt_id) else {
            return (Vec::new(), None);
        };

        let mut numbered: Vec<&PromptVersionRecord> = self
            .prompt_versions
            .values()
            .filter(|record| record.prompt_id == prompt_id)
            .collect();
        numbered.sort_by_key(|record| record.version);

        let ordered: Vec<(VersionCursor, PromptSummary)> =
            ::std::iter::once((VersionCursor::Draft, prompt.to_summary()))
                .chain(numbered.into_iter().map(|record| {
                    (VersionCursor::Numbered(record.version), record.to_summary())
                }))
                .filter(|(cursor, _)| after.is_none_or(|after| *cursor > after))
                .collect();

        let page: Vec<&(VersionCursor, PromptSummary)> =
            ordered.iter().take(page_size.max(1)).collect();
        let next_token = (ordered.len() > page.len())
            .then(|| page.last().map(|(cursor, _)| cursor.token()))
            .flatten();
        (
            page.iter().map(|(_, summary)| summary.clone()).collect(),
            next_token,
        )
    }
}
