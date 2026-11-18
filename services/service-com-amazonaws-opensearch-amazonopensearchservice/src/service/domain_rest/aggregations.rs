// SPDX-License-Identifier: CC0-1.0

//! The AGGREGATIONS section of a `_search` body: the summaries a request asks OVER the
//! documents its query selected, rather than the hits themselves.
//!
//! An aggregation reads a field's PER-DOCUMENT VALUES — the same values a sort key orders by
//! and a `term` clause compares against ([`Terms::held`]) — once per document the scan
//! selects, so what it reports is computed from the documents the caller's query really
//! matched and from nothing else. That is also what makes it answer the FIELD-LEVEL SECURITY
//! in force: the scan is handed each document as the caller may SEE it, so a field hidden
//! from that caller holds no value to aggregate and contributes to no bucket and no metric —
//! reading a field by aggregating it is reading it.
//!
//! The aggregations served are the ones this engine can compute over those values: the
//! `terms` buckets, and the single-value metrics `value_count`, `min`, `max`, `sum` and `avg`.
//! A request naming any other type is REFUSED, rather than answered with a section it did not
//! ask for or with one silently left out.
//!
//! Both factors of the work one request causes are bounded: how many aggregations one request
//! may name ([`MAX_AGGREGATIONS`]) and how many distinct buckets one of them may open
//! ([`MAX_BUCKETS`]), so neither the body a caller wrote nor the documents the domain holds
//! can make one response unbounded.

use std::collections::BTreeMap;

use ::serde_json::{json, Map, Value};
use aws_smithy_runtime_api::http::Response as RtResponse;

use super::{error, scalar_text, Terms, PARSING_EXCEPTION};

/// How many aggregations one request may name.
///
/// The emulator's OWN bound, like the one on a query's clauses: every aggregation reads every
/// document the scan selects, so this is what keeps that work the documents selected rather
/// than their product with a count the request chose.
const MAX_AGGREGATIONS: usize = 32;

/// How many distinct buckets ONE aggregation may open.
///
/// The engine's own `search.max_buckets` is the bound of this kind; this is the value this
/// cluster holds it at, because every bucket lives in this process's heap until the response
/// is rendered.
const MAX_BUCKETS: usize = 1_000;

/// How many buckets a `terms` aggregation REPORTS when the request names no `size`, which is
/// that aggregation's own documented default.
const DEFAULT_TERMS_SIZE: usize = 10;

/// The two members a search names its aggregations under, which are the two spellings the
/// engine takes.
const AGGS: [&str; 2] = ["aggs", "aggregations"];

/// The member every aggregation below names the field it reads under.
const FIELD: &str = "field";

/// The member a `terms` aggregation names how many buckets it wants under.
const SIZE: &str = "size";

/// What one aggregation computes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `terms`: "a multi-bucket aggregation where each bucket is a unique value for the
    /// field it is run on", reporting the `size` most frequent of them.
    Terms { size: usize },
    /// `value_count`: how many values the selected documents hold for the field.
    Count,
    /// The single-value metrics over those values.
    Min,
    Max,
    Sum,
    Avg,
}

/// The names those kinds are asked for under.
const TERMS: &str = "terms";
const VALUE_COUNT: &str = "value_count";
const MIN: &str = "min";
const MAX: &str = "max";
const SUM: &str = "sum";
const AVG: &str = "avg";

/// What one aggregation has observed of the documents the scan has reached so far.
#[derive(Debug, Clone, Default)]
struct Observed {
    /// The buckets a `terms` aggregation has opened, keyed by the TEXT of the value they
    /// bucket — the form every value of a field is compared by — holding that value as it
    /// was read and how many documents fell in it.
    buckets: BTreeMap<String, (Value, usize)>,
    /// How many values the field held over the documents selected.
    values: usize,
    /// The arithmetic the metrics report, over the values that ARE numbers: a value a field
    /// holds no number for is read by no metric.
    numbers: usize,
    sum: f64,
    least: Option<f64>,
    greatest: Option<f64>,
}

/// One aggregation a request asked for: the name it is reported under, the field it reads and
/// what it computes from it.
#[derive(Debug, Clone)]
struct Requested {
    name: String,
    field: String,
    kind: Kind,
    observed: Observed,
}

/// The aggregations a search asked for — empty for a request that named none.
#[derive(Debug, Clone, Default)]
pub(super) struct Aggregations {
    requested: Vec<Requested>,
}

impl Aggregations {
    /// The aggregations `body` names, or the engine's refusal of a section it does not read.
    pub(super) fn of(body: &Value) -> Result<Self, Box<RtResponse>> {
        let Some(named) = AGGS.iter().find_map(|member| body.get(*member)) else {
            return Ok(Self::default());
        };
        let Some(named) = named.as_object() else {
            return Err(unreadable("[aggs] must be an object naming each aggregation"));
        };
        if named.len() > MAX_AGGREGATIONS {
            return Err(unreadable(&format!(
                "a request may name at most {MAX_AGGREGATIONS} aggregations, and this one \
                 names {}",
                named.len()
            )));
        }
        let mut requested = Vec::new();
        for (name, definition) in named {
            requested.push(one(name, definition)?);
        }
        Ok(Self { requested })
    }

    /// Whether this names no aggregation at all.
    pub(super) fn is_empty(&self) -> bool {
        self.requested.is_empty()
    }

    /// Read one SELECTED document into every aggregation.
    ///
    /// `source` is the document as the caller may see it, which is what makes a field hidden
    /// from that caller hold no value here.
    pub(super) fn consider(
        &mut self,
        terms: &Terms<'_>,
        source: &Value,
    ) -> Result<(), Box<RtResponse>> {
        for requested in &mut self.requested {
            requested.consider(terms, source)?;
        }
        Ok(())
    }

    /// What the aggregations report over the documents the scan selected.
    pub(super) fn reported(&self) -> Value {
        let mut reported = Map::new();
        for requested in &self.requested {
            reported.insert(requested.name.clone(), requested.reported());
        }
        Value::Object(reported)
    }
}

impl Requested {
    /// Read one document's values for this aggregation's field into it.
    fn consider(&mut self, terms: &Terms<'_>, source: &Value) -> Result<(), Box<RtResponse>> {
        for held in terms.held(source, &self.field) {
            if held.is_null() {
                continue;
            }
            self.observed.values = self.observed.values.saturating_add(1);
            if let Some(number) = held.as_f64() {
                self.observed.numbers = self.observed.numbers.saturating_add(1);
                self.observed.sum += number;
                self.observed.least = Some(
                    self.observed
                        .least
                        .map_or(number, |least| if number < least { number } else { least }),
                );
                self.observed.greatest = Some(
                    self.observed
                        .greatest
                        .map_or(number, |most| if number > most { number } else { most }),
                );
            }
            if !matches!(self.kind, Kind::Terms { .. }) {
                continue;
            }
            let key = scalar_text(&held);
            if !self.observed.buckets.contains_key(&key)
                && self.observed.buckets.len() >= MAX_BUCKETS
            {
                return Err(too_many_buckets(self.observed.buckets.len()));
            }
            let bucket = self
                .observed
                .buckets
                .entry(key)
                .or_insert_with(|| (held.clone(), 0));
            bucket.1 = bucket.1.saturating_add(1);
        }
        Ok(())
    }

    /// What this one aggregation reports.
    ///
    /// A `terms` aggregation reports its buckets most frequent first, and a tie between two
    /// equally frequent values is broken by the value itself — so one set of documents
    /// reports the same buckets in the same order on every read. The documents whose value
    /// fell outside the reported buckets are counted in `sum_other_doc_count`, which is what
    /// tells a caller there were more.
    fn reported(&self) -> Value {
        match self.kind {
            Kind::Terms { size } => {
                let mut ranked: Vec<(&String, &(Value, usize))> =
                    self.observed.buckets.iter().collect();
                ranked.sort_by(|(left_key, (_, left)), (right_key, (_, right))| {
                    right.cmp(left).then_with(|| left_key.cmp(right_key))
                });
                let other: usize = ranked
                    .iter()
                    .skip(size)
                    .map(|(_, (_, count))| *count)
                    .sum();
                let buckets: Vec<Value> = ranked
                    .iter()
                    .take(size)
                    .map(|(_, (key, count))| json!({ "key": key, "doc_count": count }))
                    .collect();
                json!({
                    // Every document the scan selected was read, so no bucket's count is an
                    // estimate and none was dropped from a shard's own report.
                    "doc_count_error_upper_bound": 0,
                    "sum_other_doc_count": other,
                    "buckets": buckets,
                })
            }
            Kind::Count => json!({ "value": self.observed.values }),
            Kind::Min => json!({ "value": number(self.observed.least) }),
            Kind::Max => json!({ "value": number(self.observed.greatest) }),
            // "sum: a sum of the values": over no value at all it is zero, while an average,
            // a minimum and a maximum of no value is no value.
            Kind::Sum => json!({ "value": number(Some(self.observed.sum)) }),
            Kind::Avg => {
                #[allow(clippy::cast_precision_loss)]
                let average = (self.observed.numbers > 0)
                    .then(|| self.observed.sum / self.observed.numbers as f64);
                json!({ "value": number(average) })
            }
        }
    }
}

/// One aggregation of the section: the single type it names, and the field that type reads.
fn one(name: &str, definition: &Value) -> Result<Requested, Box<RtResponse>> {
    let Some(members) = definition.as_object() else {
        return Err(unreadable(&format!(
            "[{name}] must be an object naming the aggregation it asks for"
        )));
    };
    let mut entries = members.iter();
    let (Some((named, options)), None) = (entries.next(), entries.next()) else {
        return Err(unreadable(&format!(
            "[{name}] aggregation must name exactly one type"
        )));
    };
    let empty = Map::new();
    let options = options.as_object().unwrap_or(&empty);
    let kind = match named.as_str() {
        TERMS => Kind::Terms {
            size: options
                .get(SIZE)
                .and_then(Value::as_u64)
                .and_then(|size| usize::try_from(size).ok())
                .unwrap_or(DEFAULT_TERMS_SIZE)
                .min(MAX_BUCKETS),
        },
        VALUE_COUNT => Kind::Count,
        MIN => Kind::Min,
        MAX => Kind::Max,
        SUM => Kind::Sum,
        AVG => Kind::Avg,
        other => return Err(unreadable(&format!("Unknown aggregation type [{other}]"))),
    };
    let Some(field) = options
        .get(FIELD)
        .and_then(Value::as_str)
        .filter(|field| !field.is_empty())
    else {
        return Err(unreadable(&format!(
            "[{named}] aggregation [{name}] must name the [field] it reads"
        )));
    };
    Ok(Requested {
        name: name.to_string(),
        field: field.to_string(),
        kind,
        observed: Observed::default(),
    })
}

/// A metric's value as the engine answers it: the number it computed, and `null` for a metric
/// that read no value to compute one from.
fn number(value: Option<f64>) -> Value {
    value
        .and_then(::serde_json::Number::from_f64)
        .map_or(Value::Null, Value::Number)
}

/// The engine's refusal of an aggregations section it cannot read.
fn unreadable(reason: &str) -> Box<RtResponse> {
    Box::new(error(400, PARSING_EXCEPTION, reason))
}

/// The engine's refusal of a request whose buckets would pass the cluster's own ceiling,
/// which names that ceiling and the setting that carries it.
fn too_many_buckets(opened: usize) -> Box<RtResponse> {
    Box::new(error(
        400,
        TOO_MANY_BUCKETS_EXCEPTION,
        &format!(
            "Trying to create too many buckets. Must be less than or equal to: \
             [{MAX_BUCKETS}] but was [{}]. This limit can be set by changing the \
             [search.max_buckets] cluster level setting.",
            opened.saturating_add(1)
        ),
    ))
}

/// The engine's error type for a request past that ceiling.
const TOO_MANY_BUCKETS_EXCEPTION: &str = "too_many_buckets_exception";
