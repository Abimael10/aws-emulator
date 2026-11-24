// SPDX-License-Identifier: CC0-1.0

//! How a paginated read of this service pages over the list it answers from.
//!
//! One list, one position in it: a `NextToken` this service issues names the offset the next
//! page starts at ([`encode_next_token`], [`decode_next_token`]), so a token it never issued
//! resumes nothing and is refused instead — in the wording the read that was asked for states
//! it. How large a page is comes from that request's own `MaxRecords`, bounded by what the
//! model documents for the member and by the cap the read holds its pages to
//! ([`page_limit`], [`max_records_out_of_range_within`]), so no read answers more than it says
//! it does.

use super::*;

/// Decode a `NextToken` into the offset it names, or `None` when the token is not
/// one this service issued.
///
/// The paginated describes in this service page over a name-ordered list, so an
/// issued token is the offset the next page starts at, behind a marker: anything
/// without the marker, and the offset 0 (the first page is reached with no token
/// at all, so no token for it is ever issued), is not a token this service gave
/// out.
pub fn decode_next_token(token: &str) -> Option<usize> {
    token
        .strip_prefix(NEXT_TOKEN_MARKER)?
        .parse::<usize>()
        .ok()
        .filter(|offset| *offset > 0)
}

/// The message a paginated read refuses an unusable `NextToken` with in the PAST tense,
/// the form `DescribeAutoScalingGroups` and `DescribeScalingActivities` serve.
///
/// The refusal quotes the token the caller sent — that is the only thing the caller can
/// act on, since the token it is holding is not one this service will resume from. The
/// quoted token is the only variable part; the rest is fixed text, as the recorded
/// refusals of the reads that serve this sentence state it.
pub fn invalid_next_token_message(token: &str) -> String {
    format!("The token '{token}' was invalid")
}

/// The same sentence in the PRESENT tense, the form `DescribeNotificationConfigurations`,
/// `DescribeScheduledActions`, `DescribeAutoScalingInstances` and `DescribeWarmPool`
/// serve: the sentence is about the token the caller is holding now.
///
/// Separate from [`invalid_next_token_message`] because the sentence is each read's own —
/// the reads that answer the past-tense form state it as recorded for them, and
/// `DescribePolicies` names the member by its role ([`invalid_pagination_token_message`]) —
/// so one read's wording is not the others' to change.
pub fn invalid_next_token_message_in_present_tense(token: &str) -> String {
    format!("The token '{token}' is invalid")
}

/// The message `DescribeInstanceRefreshes` refuses an unusable `NextToken` with: the
/// present-tense sentence about the token the caller is holding, TERMINATED by a period,
/// as that read's own recorded refusal states it.
///
/// Separate from the two above because how the sentence ends is each read's own: this one
/// closes it, the reads above leave it open, and `DescribePolicies` names the member by its
/// role instead ([`invalid_pagination_token_message`]). The token the request sent, quoted,
/// is the only variable part.
pub fn invalid_next_token_message_terminated(token: &str) -> String {
    format!("{}.", invalid_next_token_message_in_present_tense(token))
}

/// The refusal the reads that treat an unusable `NextToken` as a bad member VALUE answer:
/// the member as the model names it, and the value the request submitted for it.
///
/// Those reads answer the service-common `ValidationError` rather than the modelled
/// `InvalidNextToken`, so this is the whole message and the token is its only variable
/// part.
pub fn next_token_value_violation_message(token: &str) -> String {
    format!("NextToken: {token}")
}

/// The refusal those same reads answer a request that carries a `NextToken` WITHOUT a
/// `MaxRecords` with.
///
/// `DescribeLoadBalancers` and `DescribeTrafficSources` page a group's attachments against the
/// page size the request states, and their recorded refusal of a token sent with no page size
/// is this sentence: a resumption token names a position inside a page sequence, so the size
/// that sequence was produced under has to be stated beside it. Wholly fixed text — it names
/// the two members the rule is about and nothing the request carried — and it is answered
/// BEFORE the token itself is read, because a token without the page size is unusable whatever
/// it says. `DescribeLoadBalancerTargetGroups` answers its own recorded refusal for that
/// request ([`next_token_value_violation_message`]), so the precedence is each read's.
pub fn next_token_without_max_records_message() -> String {
    "Cannot specify NextToken without MaxRecords".to_string()
}

/// The message `DescribePolicies` refuses an unusable `NextToken` with: the sentence that
/// operation's own recorded refusal states, which names the member by its role (a
/// pagination token) and quotes the token the caller sent.
///
/// Separate from [`invalid_next_token_message`] because the sentence is this
/// operation's: the one above is the example wording its callers already answer with,
/// and rewording them is not this operation's to do.
pub fn invalid_pagination_token_message(token: &str) -> String {
    format!("The pagination token '{token}' is invalid")
}

/// Encode the offset the next page starts at as a `NextToken`.
pub fn encode_next_token(offset: usize) -> String {
    format!("{NEXT_TOKEN_MARKER}{offset}")
}

/// The marker every `NextToken` this service issues carries, so a token it never
/// issued is refused rather than read as an offset.
const NEXT_TOKEN_MARKER: &str = "next-";

/// One page of a name-ordered list, plus the `NextToken` that follows it.
///
/// `max_records` is the request's `MaxRecords`; absent or out of range it falls
/// back to [`DEFAULT_MAX_RECORDS`], and it is capped at [`MAX_MAX_RECORDS`].
pub fn page<T: Clone>(items: &[T], offset: usize, max_records: Option<i32>) -> (Vec<T>, Option<String>) {
    let limit = page_limit(max_records);
    let start = offset.min(items.len());
    let end = start.saturating_add(limit).min(items.len());
    let next = (end < items.len()).then(|| encode_next_token(end));
    (items[start..end].to_vec(), next)
}

/// How many items one page holds, from the request's `MaxRecords`: absent or
/// non-positive it falls back to [`DEFAULT_MAX_RECORDS`], and it is capped at
/// [`MAX_MAX_RECORDS`].
///
/// Separate from [`page`] because a describe that pages over its own cursor
/// rather than over an offset still bounds its page the same way.
pub fn page_limit(max_records: Option<i32>) -> usize {
    max_records
        .filter(|value| *value > 0)
        .map_or(DEFAULT_MAX_RECORDS, |value| {
            (value as usize).min(MAX_MAX_RECORDS)
        })
}

/// A request's `MaxRecords` when it is outside the range the model documents
/// ("The default value is 50 and the maximum value is 100"), for an operation to
/// refuse: `None` when the request omits it or carries an acceptable value.
pub fn max_records_out_of_range(max_records: Option<i32>) -> Option<i32> {
    max_records_out_of_range_within(
        max_records,
        i32::try_from(MAX_MAX_RECORDS).unwrap_or(i32::MAX),
    )
}

/// The same test for a read whose own ceiling is not [`MAX_MAX_RECORDS`] — the
/// name-ordered describes honour 100 and `DescribeWarmPool` honours 50 — so the bound
/// checked and the bound stated ([`max_records_violation_message_within`]) cannot drift
/// apart.
pub fn max_records_out_of_range_within(max_records: Option<i32>, ceiling: i32) -> Option<i32> {
    max_records.filter(|value| *value < MIN_MAX_RECORDS || *value > ceiling)
}

/// The message a paginated read refuses a `MaxRecords` outside that range with: the
/// sentence that belongs to the bound the value missed
/// ([`max_records_violation_message_within`]).
pub fn max_records_violation_message(max_records: i32) -> String {
    max_records_violation_message_within(
        max_records,
        i32::try_from(MAX_MAX_RECORDS).unwrap_or(i32::MAX),
    )
}

/// The same sentence for a read whose own ceiling is not [`MAX_MAX_RECORDS`]: the
/// name-ordered describes honour 100 and `DescribeWarmPool` honours 50, so the ceiling
/// the value missed is the caller's to name.
///
/// THE ONE COPY of that sentence for every paginated read of this service, whichever
/// ceiling it holds its pages to — and a value is weighed against TWO bounds, under the
/// sentence each of them carries:
///
/// * the member's own bounds, which every paginated read of the service shares
///   ([`MIN_MAX_RECORDS`] to [`MAX_MAX_RECORDS`]): a value outside them is a member value
///   outside its bounds and is stated as one ([`constraint_violation_message`]), naming the
///   bound of the MEMBER rather than the read's own cap;
/// * the page cap of THIS read, where it is smaller than the member's ceiling: a value
///   inside the member's bounds but past that cap asks for more records than the read
///   returns, which is the service's own limit ([`over_page_cap_message`]).
pub fn max_records_violation_message_within(max_records: i32, ceiling: i32) -> String {
    let member_ceiling = i32::try_from(MAX_MAX_RECORDS).unwrap_or(i32::MAX);
    if max_records < MIN_MAX_RECORDS {
        return constraint_violation_message(
            max_records,
            "maxRecords",
            &format!("Member must have value greater than or equal to {MIN_MAX_RECORDS}"),
        );
    }
    if max_records > member_ceiling {
        return constraint_violation_message(
            max_records,
            "maxRecords",
            &format!("Member must have value less than or equal to {member_ceiling}"),
        );
    }
    over_page_cap_message(ceiling)
}

/// The sentence a paginated read refuses a `MaxRecords` inside the member's own bounds but
/// ABOVE the page cap it holds its pages to with, which names that cap.
///
/// `MaxRecords` is a plain integer in this service's model, with no range of its own
/// (`smithy_query(query="shape",shape="MaxRecords")`), so a cap smaller than the member's
/// ceiling is not a constraint the request failed to satisfy: it is how many records the
/// read is willing to return, and the refusal says so. The cap is the operation's own — the
/// name-ordered describes hold their pages to [`MAX_MAX_RECORDS`] and the instance and
/// warm-pool reads to [`DEFAULT_MAX_RECORDS`] — so it is the only variable part of the
/// sentence; the rest is fixed text.
pub fn over_page_cap_message(ceiling: i32) -> String {
    format!("The number of records returned is limited to {ceiling}")
}

/// The most names a describe may filter by, which the model documents on
/// `AutoScalingGroupNames`: "By default, you can only specify up to 50 names. You
/// can optionally increase this limit using the MaxRecords property."
pub fn max_filter_names(max_records: Option<i32>) -> usize {
    max_records
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(DEFAULT_MAX_RECORDS)
        .max(DEFAULT_MAX_RECORDS)
}

/// `MaxRecords` when the request omits it.
///
/// From the model's own documentation for `AutoScalingGroupNamesType$MaxRecords`:
/// "The default value is 50 and the maximum value is 100."
pub const DEFAULT_MAX_RECORDS: usize = 50;

/// The largest `MaxRecords` the service honours (same documentation).
pub const MAX_MAX_RECORDS: usize = 100;

/// The smallest `MaxRecords` a request may ask for: a page holds items, so it holds at
/// least one.
pub const MIN_MAX_RECORDS: i32 = 1;

/// `MaxRecords` when a read of the scaling activities omits it.
///
/// From the model's own documentation for `DescribeScalingActivitiesType$MaxRecords`:
/// "The default value is 100 and the maximum value is 100" — a larger default than the
/// name-ordered describes take, so that operation names it where it pages.
pub const DEFAULT_ACTIVITY_MAX_RECORDS: i32 = 100;

/// `MaxRecords` when a read of a group's load-balancer attachments omits it.
///
/// From the model's own documentation for
/// `DescribeLoadBalancerTargetGroupsRequest$MaxRecords`: "The default value is 100 and the
/// maximum value is 100" — the same larger default the scaling activities take, stated
/// separately because it is that operation's documentation that says so.
pub const DEFAULT_ATTACHMENT_MAX_RECORDS: i32 = 100;
