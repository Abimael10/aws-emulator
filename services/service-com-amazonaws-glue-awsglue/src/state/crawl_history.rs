// SPDX-License-Identifier: CC0-1.0

//! The CRAWL HISTORY of a crawler: how one crawl is identified, how it is reported as a
//! `CrawlerHistory`, and how a request's `CrawlsFilter` list narrows the crawls reported.
//!
//! A crawl is RECORDED where it is started and comes to rest when the clock settles it
//! ([`super::crawl`]); this module is what a reader of that record sees. Each crawl carries
//! an id of its own, minted once ([`new_crawl_id`]), and the crawler keeps the records of
//! the crawls it has run, newest first and bounded ([`CrawlerEntry::crawl_history`]) — so
//! the history a listing pages over is the same set of crawls `LastCrawl` and the crawler's
//! metrics are read off.

use super::*;

/// How many crawls of ONE crawler this store keeps the record of.
///
/// CHOSEN, not an AWS retention: Glue "retains up to 12 months of crawls"
/// (`ListCrawls`), which a store that lives in resident memory and that any caller can
/// grow cannot be bounded by — so the number of records one crawler holds is bounded
/// here, the oldest dropped first.
pub const MAX_CRAWL_HISTORY: usize = 20;

/// `CrawlerHistory$State` of a crawl that is still under way: of the modelled
/// `CrawlerHistoryState` values (`RUNNING | COMPLETED | FAILED | STOPPED`, whose published
/// list the generated [`crate::ops::types::CRAWLER_HISTORY_STATE_VALUES`] carries), the one
/// that names a crawl the service is still working on.
pub const CRAWLER_HISTORY_STATE_RUNNING: &str = "RUNNING";

/// `CrawlerHistory$State` of a crawl that ran to its end: the `CrawlerHistoryState` the
/// `LastCrawlStatus` of a finished crawl ([`LAST_CRAWL_STATUS_SUCCEEDED`]) is reported as.
pub const CRAWLER_HISTORY_STATE_COMPLETED: &str = "COMPLETED";

/// `CrawlerHistory$State` of a crawl that could not be carried out, which carries the reason
/// in `CrawlerHistory$ErrorMessage`. The state [`LAST_CRAWL_STATUS_FAILED`] is reported as.
pub const CRAWLER_HISTORY_STATE_FAILED: &str = "FAILED";

/// `CrawlerHistory$State` of a crawl that was cut short rather than finishing or failing: the
/// state [`LAST_CRAWL_STATUS_CANCELLED`] is reported as.
pub const CRAWLER_HISTORY_STATE_STOPPED: &str = "STOPPED";

/// How many 64-bit words a crawl id carries: two, i.e. the 128 bits a UUID is.
const CRAWL_ID_WORDS: usize = 2;

/// How many crawl ids this process has minted. A nonce source for [`new_crawl_id`], exactly
/// as the counters beside it are for the other ids this crate mints.
static MINTED_CRAWL_IDS: AtomicU64 = AtomicU64::new(0);

/// A fresh `CrawlId`: "a UUID identifier for each crawl" (`CrawlerHistory$CrawlId`).
///
/// 128 bits spelled the way a UUID is, so two crawls started in the same nanosecond — of one
/// crawler or of two — cannot be given the same identity. Minted once, where the crawl is
/// recorded, and stored on it, so every later read of that crawl reports the same id.
///
/// This names a RESOURCE, not a credential: a credential would come from the credential
/// store, never from here.
#[must_use]
pub fn new_crawl_id() -> String {
    super::iceberg_metadata::uuid_spelling(&mint_hex_id(&MINTED_CRAWL_IDS, CRAWL_ID_WORDS))
}

/// The `CrawlsFilter$FieldName` keying on the crawl's own id.
///
/// Source: the modelled `FieldName` enum, whose published values the generated
/// [`crate::ops::types::FIELD_NAME_VALUES`] carries (`CRAWL_ID`, `STATE`, `START_TIME`,
/// `END_TIME`, `DPU_HOUR`) — each documented by `CrawlsFilter$FieldName` itself.
pub const CRAWLS_FIELD_CRAWL_ID: &str = "CRAWL_ID";

/// The `CrawlsFilter$FieldName` keying on "a string representing the state of the crawl".
/// See [`CRAWLS_FIELD_CRAWL_ID`].
pub const CRAWLS_FIELD_STATE: &str = "STATE";

/// The `CrawlsFilter$FieldName` keying on when the crawl started, which the member documents
/// as "the epoch timestamp in milliseconds". See [`CRAWLS_FIELD_CRAWL_ID`].
pub const CRAWLS_FIELD_START_TIME: &str = "START_TIME";

/// The `CrawlsFilter$FieldName` keying on when the crawl ended, in the same milliseconds.
/// See [`CRAWLS_FIELD_CRAWL_ID`].
pub const CRAWLS_FIELD_END_TIME: &str = "END_TIME";

/// The `CrawlsFilter$FieldName` keying on "the number of data processing unit (DPU) hours
/// used for the crawl". See [`CRAWLS_FIELD_CRAWL_ID`].
pub const CRAWLS_FIELD_DPU_HOUR: &str = "DPU_HOUR";

/// The `CrawlsFilter$FilterOperator` values, as the member documents them: `GT` greater
/// than, `GE` greater than or equal to, `LT` less than, `LE` less than or equal to, `EQ`
/// equal to and `NE` not equal to.
///
/// Source: the modelled `FilterOperator` enum, whose published values the generated
/// [`crate::ops::types::FILTER_OPERATOR_VALUES`] carries.
pub const CRAWLS_OPERATOR_GT: &str = "GT";
/// See [`CRAWLS_OPERATOR_GT`].
pub const CRAWLS_OPERATOR_GE: &str = "GE";
/// See [`CRAWLS_OPERATOR_GT`].
pub const CRAWLS_OPERATOR_LT: &str = "LT";
/// See [`CRAWLS_OPERATOR_GT`].
pub const CRAWLS_OPERATOR_LE: &str = "LE";
/// See [`CRAWLS_OPERATOR_GT`].
pub const CRAWLS_OPERATOR_EQ: &str = "EQ";
/// See [`CRAWLS_OPERATOR_GT`].
pub const CRAWLS_OPERATOR_NE: &str = "NE";

/// The members of the `Summary` a finished crawl reports, which is "a run summary for the
/// specific crawl in JSON. Contains the catalog tables and partitions that were added,
/// updated, or deleted" (`CrawlerHistory$Summary`).
///
/// Named here rather than written inline so the document one crawl reports and the one a
/// reader of it expects come from one description.
const SUMMARY_TABLES_CREATED: &str = "TablesCreated";
/// See [`SUMMARY_TABLES_CREATED`].
const SUMMARY_TABLES_UPDATED: &str = "TablesUpdated";
/// See [`SUMMARY_TABLES_CREATED`].
const SUMMARY_TABLES_DELETED: &str = "TablesDeleted";
/// See [`SUMMARY_TABLES_CREATED`].
const SUMMARY_PARTITIONS_CREATED: &str = "PartitionsCreated";
/// See [`SUMMARY_TABLES_CREATED`].
const SUMMARY_PARTITIONS_UPDATED: &str = "PartitionsUpdated";
/// See [`SUMMARY_TABLES_CREATED`].
const SUMMARY_PARTITIONS_DELETED: &str = "PartitionsDeleted";

impl CrawlRecord {
    /// This crawl's `CrawlerHistoryState`: the crawl still in flight is the one that is
    /// RUNNING, and a crawl that has finished reports the state its `LastCrawlStatus`
    /// names — so the two vocabularies this store publishes a crawl's outcome in cannot
    /// disagree about one crawl.
    pub fn history_state(&self) -> &'static str {
        if self.finished_at.is_none() {
            return CRAWLER_HISTORY_STATE_RUNNING;
        }
        match self.status.as_str() {
            LAST_CRAWL_STATUS_SUCCEEDED => CRAWLER_HISTORY_STATE_COMPLETED,
            LAST_CRAWL_STATUS_CANCELLED => CRAWLER_HISTORY_STATE_STOPPED,
            _ => CRAWLER_HISTORY_STATE_FAILED,
        }
    }

    /// "The number of data processing units (DPU) used in hours for the crawl"
    /// (`CrawlerHistory$DPUHour`).
    ///
    /// The crawl walks its targets in this process rather than on a fleet this emulator
    /// provisions, so no data processing unit was used and the figure stays at the member's
    /// own default of 0 — exactly as a column statistics task run reports its `DpuSeconds`
    /// and a job run its worker count.
    pub fn dpu_hour(&self) -> f64 {
        0.0
    }

    /// This crawl as the modelled `CrawlerHistory`: "the information for a run of a
    /// crawler".
    ///
    /// Every member is this crawl's own: the id minted for it, the state it stands in, the
    /// instants it ran between — a crawl still in flight has not ended, so it reports no
    /// end instant — the summary of what it wrote and the reason it failed, if it did.
    ///
    /// `LogGroup`, `LogStream` and `MessagePrefix` are absent for the reason
    /// [`CrawlRecord::to_last_crawl_info`] gives: they address the CloudWatch Logs stream a
    /// crawl writes its log to, and nothing here writes one.
    pub fn to_crawler_history(&self) -> CrawlerHistory {
        CrawlerHistory {
            crawl_id: Some(self.crawl_id.clone()),
            state: Some(self.history_state().to_string()),
            start_time: Some(to_date_time(self.started_at)),
            end_time: self.finished_at.map(to_date_time),
            // What the crawl wrote is published when it comes to rest, exactly as its
            // `LastCrawl` is: a crawl still in flight has produced no summary.
            summary: self.finished_at.is_some().then(|| self.run_summary()),
            error_message: self.error_message.clone(),
            log_group: None,
            log_stream: None,
            message_prefix: None,
            dpu_hour: self.dpu_hour(),
        }
    }

    /// This crawl's place in a listing of a crawler's crawls, as that listing's continuation
    /// token renders it: the shared history cursor ([`history_page_cursor`]) over the instant
    /// the crawl started and its own id, exactly as this crate's other run histories page.
    pub fn page_cursor(&self) -> String {
        history_page_cursor(self.started_at, &self.crawl_id)
    }

    /// "A run summary for the specific crawl in JSON. Contains the catalog tables and
    /// partitions that were added, updated, or deleted" — the counts this crawl's own writes
    /// produced, rendered as one JSON object.
    fn run_summary(&self) -> String {
        ::serde_json::json!({
            SUMMARY_TABLES_CREATED: self.tables_created,
            SUMMARY_TABLES_UPDATED: self.tables_updated,
            SUMMARY_TABLES_DELETED: self.tables_deleted,
            SUMMARY_PARTITIONS_CREATED: self.partitions_created,
            SUMMARY_PARTITIONS_UPDATED: 0,
            SUMMARY_PARTITIONS_DELETED: self.partitions_deleted,
        })
        .to_string()
    }

    /// The value this crawl carries for the field a filter keys on, or `None` when the crawl
    /// has none: a crawl still in flight has no end instant, so there is no value of that
    /// field to compare against.
    fn filter_value(&self, field: &str) -> Option<CrawlFieldValue> {
        match field {
            CRAWLS_FIELD_CRAWL_ID => Some(CrawlFieldValue::Text(self.crawl_id.clone())),
            CRAWLS_FIELD_STATE => Some(CrawlFieldValue::Text(self.history_state().to_string())),
            CRAWLS_FIELD_START_TIME => Some(CrawlFieldValue::Number(
                to_unix_millis(self.started_at) as f64,
            )),
            CRAWLS_FIELD_END_TIME => self
                .finished_at
                .map(|at| CrawlFieldValue::Number(to_unix_millis(at) as f64)),
            CRAWLS_FIELD_DPU_HOUR => Some(CrawlFieldValue::Number(self.dpu_hour())),
            _ => None,
        }
    }
}

/// What one field of a crawl is COMPARED as: the two kinds of value the fields a
/// `CrawlsFilter` keys on carry.
///
/// A crawl's id and its state are text, so they compare as text; the instants it ran between
/// and its DPU hours are numbers — "the epoch timestamp in milliseconds" and "the number of
/// data processing unit (DPU) hours used for the crawl" (`CrawlsFilter$FieldName`) — so they
/// compare as numbers, which is why `10` and `9` order the way the numbers do and not the way
/// their digits do.
#[derive(Debug, Clone)]
enum CrawlFieldValue {
    /// A field compared as text.
    Text(String),
    /// A field compared as a number.
    Number(f64),
}

/// ONE `CrawlsFilter` of a request, read against the field it keys on: "a list of fields,
/// comparators and value that you can use to filter the crawler runs for a specified
/// crawler".
///
/// Read once per request rather than per crawl, so a filter the request cannot be served
/// with is refused before any crawl is walked and the value it compares against is parsed
/// exactly once.
#[derive(Debug, Clone)]
pub struct CrawlsPredicate {
    /// The `FieldName` this filter keys on, one of the modelled values.
    field: String,
    /// The `FilterOperator` it compares with, one of the modelled values.
    operator: String,
    /// The `FieldValue` it compares against, read in the kind the field is compared as.
    value: CrawlFieldValue,
}

impl CrawlsPredicate {
    /// Read every filter the request stated, in the order it stated them.
    ///
    /// `Err` is the reason the request is no filter on a crawler's runs, for the caller to
    /// refuse it with its own `InvalidInputException`: a filter that states no field, no
    /// comparator or no value compares nothing, a field or a comparator outside the modelled
    /// values names no comparison, and a value that is no number cannot be compared against
    /// a field that is a number.
    pub fn parse_all(filters: &[CrawlsFilter]) -> Result<Vec<Self>, String> {
        filters.iter().map(Self::parse).collect()
    }

    /// Read ONE filter. See [`Self::parse_all`].
    fn parse(filter: &CrawlsFilter) -> Result<Self, String> {
        let field = filter
            .field_name
            .as_deref()
            .ok_or_else(|| messages::crawls_filter_without_member("FieldName"))?;
        let operator = filter
            .filter_operator
            .as_deref()
            .ok_or_else(|| messages::crawls_filter_without_member("FilterOperator"))?;
        let stated = filter
            .field_value
            .as_deref()
            .ok_or_else(|| messages::crawls_filter_without_member("FieldValue"))?;
        if !crate::ops::types::FILTER_OPERATOR_VALUES.contains(&operator) {
            return Err(messages::crawls_filter_operator_unknown(operator));
        }
        // The kind the field is compared as is what the field IS, so an unknown field name
        // is the one case there is no kind for.
        let value = match field {
            CRAWLS_FIELD_CRAWL_ID | CRAWLS_FIELD_STATE => CrawlFieldValue::Text(stated.to_string()),
            CRAWLS_FIELD_START_TIME | CRAWLS_FIELD_END_TIME | CRAWLS_FIELD_DPU_HOUR => {
                CrawlFieldValue::Number(
                    stated
                        .trim()
                        .parse::<f64>()
                        .map_err(|_| messages::crawls_filter_value_not_a_number(field, stated))?,
                )
            }
            _ => return Err(messages::crawls_filter_field_unknown(field)),
        };
        Ok(Self {
            field: field.to_string(),
            operator: operator.to_string(),
            value,
        })
    }

    /// Whether `crawl` satisfies this filter.
    ///
    /// A crawl that carries NO value for the field keyed on satisfies no comparison on it:
    /// there is nothing to compare, so a filter on a crawl's end instant selects the crawls
    /// that have ended.
    pub fn admits(&self, crawl: &CrawlRecord) -> bool {
        let Some(held) = crawl.filter_value(&self.field) else {
            return false;
        };
        let Some(order) = compare(&held, &self.value) else {
            return false;
        };
        match self.operator.as_str() {
            CRAWLS_OPERATOR_GT => order.is_gt(),
            CRAWLS_OPERATOR_GE => order.is_ge(),
            CRAWLS_OPERATOR_LT => order.is_lt(),
            CRAWLS_OPERATOR_LE => order.is_le(),
            CRAWLS_OPERATOR_EQ => order.is_eq(),
            CRAWLS_OPERATOR_NE => order.is_ne(),
            // Every modelled operator is answered above; a value outside them never reaches
            // here, because the read refuses it.
            _ => false,
        }
    }
}

/// Whether every one of `filters` admits `crawl`, which is what filtering "by the criteria
/// you specify in a list of `CrawlsFilter` objects" (`ListCrawlsRequest$Filters`) asks: each
/// criterion narrows the crawls further, so a crawl is reported only when it satisfies them
/// all. An empty list narrows nothing.
pub fn crawl_admitted(crawl: &CrawlRecord, filters: &[CrawlsPredicate]) -> bool {
    filters.iter().all(|filter| filter.admits(crawl))
}

/// How the value a crawl holds for a field orders against the value a filter compares it
/// with, or `None` when the two are of different kinds — which cannot happen, because both
/// are read off the same field name.
fn compare(held: &CrawlFieldValue, against: &CrawlFieldValue) -> Option<::std::cmp::Ordering> {
    match (held, against) {
        (CrawlFieldValue::Text(held), CrawlFieldValue::Text(against)) => Some(held.cmp(against)),
        (CrawlFieldValue::Number(held), CrawlFieldValue::Number(against)) => {
            held.partial_cmp(against)
        }
        _ => None,
    }
}
