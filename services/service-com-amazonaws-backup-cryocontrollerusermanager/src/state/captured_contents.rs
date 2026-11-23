// SPDX-License-Identifier: CC0-1.0

//! The CONTENTS of a protected resource a backup captures into the recovery point it
//! writes — the data a restore of that point puts back.
//!
//! # The rule this module implements
//!
//! A backup is a copy of the protected resource's DATA, not a note of its shape: the
//! recovery point a backup job writes carries the resource's contents as they stood when
//! the backup read them, and a restore of that point re-creates the resource holding
//! exactly those contents. So the contents are read from the service that OWNS the
//! resource when the backup is taken — over the emulator's own in-process transport
//! ([`smithy_server_runtime::EmbeddedEmulator`]), the hop the backup already describes the
//! resource through ([`super::captured_restore_metadata`]) — and stored on the recovery
//! point ([`super::RecoveryPoint::captured_contents`]), never read again: a point describes
//! the resource as it WAS, so a later write to that resource, and its deletion, leave what
//! the point holds alone. The restore writes them back through the owning service's own API
//! ([`super::perform_restore`]), so what a restored resource answers reads for is what that
//! service holds.
//!
//! One arm per resource type whose owning service this emulator can read: the first is
//! Amazon DynamoDB, whose table contents are its items, read with `Scan` page by page. A
//! type with no arm here, and a hop that answers nothing, capture NOTHING — and nothing is
//! then put back by a restore of that point, rather than a resource being reported restored
//! with data nobody captured.
//!
//! # What one recovery point may hold
//!
//! The capture copies item data into this service's store, so it is a growth site of that
//! store and is budgeted like every other ([`MAX_CAPTURED_CONTENT_BYTES`]). A protected
//! resource holding more than the budget leaves is NOT captured in part: `StartBackupJob`
//! refuses the call with the modelled limit error, so no recovery point is ever stored
//! holding some of a resource while reporting a backup of all of it.

use std::collections::BTreeMap;

use smithy_server_runtime::EmbeddedEmulator;

/// One item of a captured Amazon DynamoDB table, keyed by attribute name — the shape
/// `PutItem` takes and `Scan` answers with, ordered so a captured item has one reading.
pub type CapturedItem = BTreeMap<String, ::aws_sdk_dynamodb::types::AttributeValue>;

/// How much captured item data all the recovery points of this service may hold together.
///
/// A HOST-PROTECTION ceiling, not an AWS limit: the captured contents live in memory beside
/// every other record, so the store cannot be allowed to grow with whatever a protected
/// resource holds. What a capture may allocate is the part of this ceiling the store has not
/// used yet ([`ComAmazonawsBackupCryocontrollerusermanagerState::captured_content_budget`]),
/// read BEFORE the capture starts and carried into it, so the bound precedes the allocation
/// rather than following it; a backup that would take the total past it is refused when it is
/// started, with the modelled error AWS Backup answers a limit with.
pub const MAX_CAPTURED_CONTENT_BYTES: i64 = 64 * 1024 * 1024;

/// How many items of one protected resource a single recovery point may hold, for the same
/// reason: an item may be tiny, so the byte budget alone does not bound the record count.
pub const MAX_CAPTURED_ITEMS: usize = 100_000;

/// The contents of one protected resource, as the service that owns it answered when the
/// backup read them.
///
/// `Default` is "nothing was captured": the empty capture of a resource type no arm here
/// reads, which is complete — there is nothing of it to put back — as opposed to
/// [`Self::over_budget`], which is a capture this emulator declined to hold.
#[derive(Debug, Clone, Default)]
pub struct CapturedContents {
    /// The items of a captured DynamoDB table, in the order the owning service answered
    /// with.
    pub dynamodb_items: Vec<CapturedItem>,
    /// How much item data this capture holds, by the rule [`captured_item_size`] states.
    /// It is what the recovery point and the job that wrote it report as the backup's size,
    /// so the size reported is the size of what was captured.
    pub size_in_bytes: i64,
    /// Whether the resource holds more than one recovery point of this emulator may
    /// ([`MAX_CAPTURED_CONTENT_BYTES`], [`MAX_CAPTURED_ITEMS`]) — the capture was stopped,
    /// so what it carries is a PART of the resource and must not be stored as a backup of
    /// it.
    pub over_budget: bool,
}

impl CapturedContents {
    /// Whether this capture holds nothing at all, so a restore of it writes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dynamodb_items.is_empty()
    }
}

/// The contents of the protected resource `resource_arn` names, as the service that owns it
/// answers them, for the recovery point a backup of it is about to store.
///
/// `budget` is how many bytes of item data this capture may hold — the part of
/// [`MAX_CAPTURED_CONTENT_BYTES`] the store has not used yet, read before this is called, so
/// nothing is allocated ahead of the ceiling that caps it. A capture that reaches it stops
/// there and says so ([`CapturedContents::over_budget`]).
///
/// Asked with NO store guard held: it makes in-process calls and opens no socket. An empty
/// capture is "nothing was captured" — see the module docs.
pub async fn captured_contents(
    callback: &EmbeddedEmulator,
    region: &str,
    resource_arn: &str,
    budget: i64,
) -> CapturedContents {
    match super::resource_type_of(resource_arn).as_deref() {
        Some(super::RESOURCE_TYPE_DYNAMODB) => {
            captured_dynamodb_items(callback, region, resource_arn, budget).await
        }
        // A type whose owning service this emulator cannot read the contents of captures
        // nothing.
        _ => CapturedContents::default(),
    }
}

/// The Amazon DynamoDB arm: every item the table the ARN names holds, read through that
/// service's own `Scan`.
///
/// The client and the table-name reading are the ones every other direction of a question
/// about one table uses ([`super::dynamodb_client`], [`super::table_name_of`]). The read is
/// PAGED, because a scan answers one page at a time and reports the key to resume from, so
/// the capture is of the whole table rather than of its first page; it stops as soon as the
/// budget is reached, reporting the capture as one this emulator will not hold. A scan the
/// hop could not complete captures nothing at all, the way a description it could not get
/// does.
async fn captured_dynamodb_items(
    callback: &EmbeddedEmulator,
    region: &str,
    resource_arn: &str,
    budget: i64,
) -> CapturedContents {
    let Some(table_name) = super::table_name_of(resource_arn) else {
        return CapturedContents::default();
    };
    let client = super::dynamodb_client(callback, region);
    let mut captured = CapturedContents::default();
    let mut resume_from = None;
    loop {
        let page = client
            .scan()
            .table_name(table_name)
            .set_exclusive_start_key(resume_from)
            .send()
            .await;
        let Ok(page) = page else {
            return CapturedContents::default();
        };
        for item in page.items.unwrap_or_default() {
            captured.size_in_bytes = captured
                .size_in_bytes
                .saturating_add(captured_item_size(&item));
            captured.dynamodb_items.push(item.into_iter().collect());
            if captured.size_in_bytes > budget
                || captured.dynamodb_items.len() > MAX_CAPTURED_ITEMS
            {
                captured.over_budget = true;
                return captured;
            }
        }
        // The key to resume from is the one that service reported; its absence is that
        // service saying the scan has read the whole table.
        resume_from = page.last_evaluated_key;
        if resume_from.is_none() {
            return captured;
        }
    }
}

/// The size of one captured item: every attribute's name plus its value.
///
/// SOURCE: the rule the Amazon DynamoDB Developer Guide gives under "Item sizes and
/// formats" — a string or binary value counts its raw bytes, a number about one byte per
/// two significant digits plus one, a boolean or a null one byte, and a list or map three
/// bytes of overhead plus one byte and the size of each element. Measured here rather than
/// asked of DynamoDB because the figures that service REPORTS for a table are metered on
/// its own schedule, so they do not describe a write made an instant ago, while the size of
/// a backup is the size of the data this capture actually holds.
///
/// The walk is ITERATIVE, over an explicit stack: how deeply a captured value nests is the
/// writer's choice, so recursing on it would make stack use a function of stored data.
/// Sizes accumulate saturatingly, so the arithmetic cannot overflow.
#[must_use]
pub fn captured_item_size(
    item: &::std::collections::HashMap<String, ::aws_sdk_dynamodb::types::AttributeValue>,
) -> i64 {
    item.iter()
        .map(|(name, value)| {
            byte_count(name.len()).saturating_add(captured_attribute_size(value))
        })
        .fold(0, i64::saturating_add)
}

/// The size of one captured attribute value, by the rule [`captured_item_size`] states.
fn captured_attribute_size(value: &::aws_sdk_dynamodb::types::AttributeValue) -> i64 {
    use ::aws_sdk_dynamodb::types::AttributeValue;

    let mut total: i64 = 0;
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        let own = match value {
            AttributeValue::S(text) => byte_count(text.len()),
            AttributeValue::N(text) => number_size(text),
            AttributeValue::B(bytes) => byte_count(bytes.as_ref().len()),
            AttributeValue::Bool(_) | AttributeValue::Null(_) => 1,
            AttributeValue::Ss(values) => values
                .iter()
                .map(|text| byte_count(text.len()))
                .fold(0, i64::saturating_add),
            AttributeValue::Ns(values) => values
                .iter()
                .map(|text| number_size(text))
                .fold(0, i64::saturating_add),
            AttributeValue::Bs(values) => values
                .iter()
                .map(|bytes| byte_count(bytes.as_ref().len()))
                .fold(0, i64::saturating_add),
            AttributeValue::L(values) => {
                pending.extend(values.iter());
                3i64.saturating_add(byte_count(values.len()))
            }
            AttributeValue::M(entries) => {
                let names = entries
                    .keys()
                    .map(|name| byte_count(name.len()).saturating_add(1))
                    .fold(0, i64::saturating_add);
                pending.extend(entries.values());
                3i64.saturating_add(names)
            }
            // The SDK's value type is open: a variant this build does not know the shape of
            // is counted as nothing rather than guessed at.
            _ => 0,
        };
        total = total.saturating_add(own);
    }
    total
}

/// A byte count as a size, saturating rather than wrapping on a length no `i64` can hold.
fn byte_count(length: usize) -> i64 {
    i64::try_from(length).unwrap_or(i64::MAX)
}

/// The size of a number's text, by the Developer Guide's rule: about one byte per two
/// significant digits, plus one.
fn number_size(text: &str) -> i64 {
    let digits = text.chars().filter(char::is_ascii_digit).count();
    byte_count(digits / 2 + 1)
}

impl super::ComAmazonawsBackupCryocontrollerusermanagerState {
    /// How much captured item data the recovery points this store holds carry together —
    /// what the budget a further capture is measured against is measured on
    /// ([`MAX_CAPTURED_CONTENT_BYTES`]).
    ///
    /// Summed from the stored points rather than kept as a running total, so it cannot
    /// drift from what the store actually holds: a point that was deleted, or one a copy
    /// wrote, is counted exactly as it stands.
    #[must_use]
    pub fn captured_content_bytes(&self) -> i64 {
        self.recovery_points
            .values()
            .map(|point| point.captured_contents.size_in_bytes)
            .fold(0, i64::saturating_add)
    }

    /// How much item data a FURTHER capture may hold: the part of
    /// [`MAX_CAPTURED_CONTENT_BYTES`] the stored points have not used.
    ///
    /// Read before a capture is made and carried into it ([`captured_contents`]), so the
    /// ceiling is applied as the data is read rather than after it has been held: a
    /// capture can never allocate more than the store had room for. The admission the
    /// backup is finally granted is decided again under the store's lock, where a point
    /// another call stored in between is counted too.
    #[must_use]
    pub fn captured_content_budget(&self) -> i64 {
        MAX_CAPTURED_CONTENT_BYTES.saturating_sub(self.captured_content_bytes())
    }
}
