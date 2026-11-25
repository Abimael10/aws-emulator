// SPDX-License-Identifier: CC0-1.0

//! What a DELETE leaves behind of a billing-period-scoped record — one rule for every
//! resource of this store a delete removes whole.
//!
//! A delete that names no billing-period range of its own ends the record it takes FROM
//! the billing period the call is made in and never earlier: the periods before it are
//! closed bills the resource really was there for, so a read scoped to one of them still
//! reports it, as the generation that was in force then. So the removed record is KEPT
//! here with the instant the delete was made at, and the periods it still answers for are
//! derived from that instant ([`DeletedRecord::reported_in`]) rather than from whatever
//! the record itself carried.
//!
//! Every versioned resource a delete removes ([`VersionedResource`]) answers those
//! questions the same way, so they are written once: a resource's own deleted-record type
//! is this one applied to its record ([`DeletedBillingGroupRecord`],
//! [`DeletedPricingRuleRecord`]), and neither can drift from the other.

use super::*;

/// A record a delete REMOVED, with the instant the delete was made at: the record of the
/// billing periods the resource was there for, which the delete does not erase.
///
/// The whole record is kept rather than just the members a period reports, because the
/// values a period reports are the ones the generation in force THEN carried
/// ([`VersionedResource::effective_in`]).
#[derive(Debug, Clone)]
pub struct DeletedRecord<Record> {
    /// The resource as it stood when the delete took it, with every generation of it the
    /// store still held.
    pub record: Record,
    /// The instant the delete was made at; the billing period it falls in is the LAST one
    /// this resource existed in ([`Self::deletion_billing_period`]).
    pub deletion_time_millis: i64,
}

impl<Record: VersionedResource> DeletedRecord<Record> {
    /// The billing period the delete was made in — the last period this resource existed
    /// in, since a resource deleted part-way through a month was there for that month.
    pub fn deletion_billing_period(&self) -> String {
        billing_period_at(self.deletion_time_millis)
    }

    /// Did this resource exist in `billing_period`? From the period it was created in
    /// through the one it was deleted in, both inclusive; a value that is not a billing
    /// period at all names no period at all.
    pub fn existed_in(&self, billing_period: &str) -> bool {
        self.period_keys(billing_period)
            .is_some_and(|(wanted, created, deleted)| (created..=deleted).contains(&wanted))
    }

    /// Does a read scoped to `billing_period` still REPORT this deleted resource? From
    /// the period it was created in up to, but NOT including, the period the delete was
    /// made in.
    ///
    /// A delete takes effect in the billing period it is made in — that is the bound every
    /// billing-period-scoped record it takes with it ends at
    /// ([`BillingPeriodSpan::ending_now`]) — so from that period on no read reports the
    /// resource, while every earlier period it was there for still reports it as it stood
    /// then. That is narrower than [`Self::existed_in`], which spans the periods the
    /// resource's own identity (a name a period's record occupies) is still taken for.
    pub fn reported_in(&self, billing_period: &str) -> bool {
        self.period_keys(billing_period)
            .is_some_and(|(wanted, created, deleted)| (created..deleted).contains(&wanted))
    }

    /// The generations of this deleted resource a period-scoped read can still REPORT,
    /// oldest first: the generation in force in each of the periods [`Self::reported_in`]
    /// answers for.
    ///
    /// A generation is in force from its own start period up to the next generation's
    /// ([`VersionedResource::effective_in`]), so a generation that took effect in the
    /// period of the delete or later is reported by no period at all, and a resource
    /// created and deleted within ONE period is reported by none of its generations. The
    /// OLDEST generation still kept answers for the periods before it, so it is reported
    /// whenever any period reports the resource at all.
    pub fn reported_versions(&self) -> Vec<Record::Generation> {
        let (Some(created), Some(deleted)) = (
            billing_period_key(&self.record.creation_billing_period()),
            billing_period_key(&self.deletion_billing_period()),
        ) else {
            return Vec::new();
        };
        if created >= deleted {
            return Vec::new();
        }
        self.record
            .versions()
            .into_iter()
            .enumerate()
            .filter(|(index, version)| {
                *index == 0
                    || billing_period_key(&version.start_billing_period())
                        .is_some_and(|start| start < deleted)
            })
            .map(|(_, version)| version)
            .collect()
    }

    /// The three billing periods every answer above compares, as the month counts
    /// [`billing_period_key`] reads them — the period asked about, the one the resource
    /// was created in and the one the delete was made in — or `None` when any of them is
    /// not a billing period at all, which names no period at all.
    fn period_keys(&self, billing_period: &str) -> Option<(i64, i64, i64)> {
        Some((
            billing_period_key(billing_period)?,
            billing_period_key(&self.record.creation_billing_period())?,
            billing_period_key(&self.deletion_billing_period())?,
        ))
    }
}

/// Keep the record of a deleted resource in `deletions`, so the billing periods it was
/// there for still hold it, and enforce that store's cap.
///
/// Past `cap` the OLDEST deletion is forgotten, which reads back the way that period read
/// before the resource ever existed; a forgotten record also frees whatever its periods
/// still held (a name a later create can reuse), which is the one thing about this store a
/// caller cannot see in a response, so it is logged — only the resource's ARN, never a
/// `@sensitive` member such as its name.
///
/// The ONE writer of either deletion store, so the cap holds wherever a resource is
/// deleted.
pub fn record_deletion<Record: VersionedResource>(
    deletions: &mut VecDeque<DeletedRecord<Record>>,
    record: Record,
    deletion_time_millis: i64,
    cap: usize,
    resource_type: &'static str,
) {
    deletions.push_back(DeletedRecord {
        record,
        deletion_time_millis,
    });
    while deletions.len() > cap {
        let Some(forgotten) = deletions.pop_front() else {
            break;
        };
        ::tracing::warn!(
            resource_type,
            resource_arn = %forgotten.record.arn(),
            cap,
            "the oldest deleted resource was forgotten at the store's cap"
        );
    }
}

/// Every resource a read scoped to `billing_period` reports, keyed by full ARN and paired
/// with the generation of itself that was in force in that period.
///
/// Which resources a billing period holds is fixed by THAT period, not by what the store
/// holds now: the records `stored` still holds that existed by then
/// ([`VersionedResource::effective_in`]) together with the records a delete has since
/// removed, for the periods before that delete took effect ([`DeletedRecord::reported_in`]).
/// A stored record wins over a deletion of the same ARN, which no live store can hold at
/// once — an id is minted once — so one resource is reported once.
///
/// The ONE projection behind every period-scoped listing of a resource a delete removes
/// whole, so the stores cannot answer it differently.
pub fn resources_in<'store, Record: VersionedResource>(
    stored: &'store BTreeMap<String, Record>,
    deletions: &'store VecDeque<DeletedRecord<Record>>,
    billing_period: &str,
) -> BTreeMap<String, (&'store Record, Record::Generation)> {
    let mut reported: BTreeMap<String, (&Record, Record::Generation)> = BTreeMap::new();
    for deleted in deletions {
        if !deleted.reported_in(billing_period) {
            continue;
        }
        if let Some(version) = deleted.record.effective_in(billing_period) {
            reported.insert(
                deleted.record.arn().to_string(),
                (&deleted.record, version),
            );
        }
    }
    for (arn, record) in stored {
        if let Some(version) = record.effective_in(billing_period) {
            reported.insert(arn.clone(), (record, version));
        }
    }
    reported
}

/// A billing group a delete removed — the record of the billing periods it BILLED in.
///
/// A billing group bills a consolidated family FOR A BILLING PERIOD, so a period it
/// billed in keeps a record of it after it is gone — which is what the model's
/// `BILLING_GROUP_ALREADY_EXIST_IN_CURRENT_BILLING_PERIOD` refusal is decided from
/// (`smithy_query(query="shape",shape="ValidationExceptionReason")`): a name a group
/// occupied in a billing period stays occupied for that period ([`Self::name_in`]).
pub type DeletedBillingGroupRecord = DeletedRecord<BillingGroupRecord>;

/// A pricing rule a delete removed — the record of the billing periods it EXISTED in.
///
/// A pricing rule is read for a billing period exactly as a billing group is (a
/// period-scoped listing reports it only for the periods it existed in, and a read that
/// addresses one rule by ARN agrees with that listing), so a delete that names no range
/// of its own takes it out of the period the call is made in and every later one, and out
/// of no earlier one.
pub type DeletedPricingRuleRecord = DeletedRecord<PricingRuleRecord>;

impl DeletedBillingGroupRecord {
    /// The name this group carried in `billing_period` — the name it OCCUPIED there —
    /// or `None` when it did not exist in that period.
    ///
    /// A rename takes effect in the period it is made in, so the name a period holds is
    /// the one the generation in force then carried
    /// ([`BillingGroupRecord::effective_in`]), not whichever name the group happened to
    /// end with.
    pub fn name_in(&self, billing_period: &str) -> Option<String> {
        if !self.existed_in(billing_period) {
            return None;
        }
        self.record
            .effective_in(billing_period)
            .map(|version| version.name)
    }
}
