// SPDX-License-Identifier: CC0-1.0

//! PAGINATION: the `Marker` every paginated report of this front door hands a caller, and the
//! page it resumes from.
//!
//! One reading for every report, so two of them cannot disagree about where a page ends or
//! what a token means: the marker names the LAST record a page SERVED, a token this codec
//! could not have produced is refused, and a marker is handed out only while records remain.

/// One page of an ordered result set, plus the `Marker` to resume from.
///
/// The marker names the LAST record the page SERVED, because that is the position a resume
/// is defined against: "the response includes only records beyond the marker"
/// (`DescribeDBParametersMessage$Marker` and every other paginated report of this front
/// door). So the next page is whatever the report holds after that position, and a page
/// boundary cannot drift — nor repeat a record — when a record is added or removed between
/// calls. A marker is returned only while records remain, which is what `@paginated`
/// callers loop on.
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_marker: Option<String>,
}

/// The `Marker` this front door hands a caller for the result-set key `key`.
///
/// A `Marker` is "an optional pagination token provided by a previous request"
/// (`DBEngineVersionMessage$Marker` and every other paginated report of this front door):
/// the token is OPAQUE, so a caller echoes it back verbatim and parses nothing out of it,
/// and the service is free to carry in it whatever it needs to resume. This one carries
/// the key of the last record the page served, encoded so that nothing of the result set's
/// internal key form reaches the wire and so that a token is recognisably a token rather
/// than a name a caller could have invented.
///
/// The codec is `aws_smithy_types::base64` — already in this crate's dependency tree,
/// because it is the one the wire protocols use for a blob — rather than a hand-rolled
/// encoder.
pub fn encode_marker(key: &str) -> String {
    ::aws_smithy_types::base64::encode(key)
}

/// The result-set key a `Marker` carries, or `None` when the token is not one this front
/// door's codec could have produced at all.
pub fn decode_marker(marker: &str) -> Option<String> {
    String::from_utf8(::aws_smithy_types::base64::decode(marker).ok()?).ok()
}

/// The refusal Amazon RDS answers a `Marker` it cannot place in the report with.
///
/// Entirely FIXED text, with no variable part at all: the token the request carried is NOT
/// reflected back, and the sentence names the two ways to ask again. This is the wording
/// every report of this front door answers with, bar the parameter reports, whose own
/// wording is [`invalid_parameter_marker_message`] — the refusal sentence belongs to the
/// report that rejected the token, not to the paginator.
pub fn invalid_marker_message() -> String {
    "Invalid marker. Please use the marker returned from a prior request or omit the marker \
     parameter."
        .to_string()
}

/// The refusal the PARAMETER reports — the ones whose paginated items are a parameter list
/// (`DescribeDBParameters`, `DescribeDBClusterParameters` and the two engine-default
/// reports) — answer a `Marker` they cannot place with.
///
/// Those reports word it with one variable part: the token the REQUEST carried, echoed
/// verbatim so a caller looping over a group's parameters can see which of the markers it
/// holds was rejected. The article differs from [`invalid_marker_message`]'s too, which is
/// why one shared sentence cannot serve both classes of report.
pub fn invalid_parameter_marker_message(marker: &str) -> String {
    format!(
        "Invalid marker: {marker}. Please use the marker returned from the prior request or \
         omit the marker parameter."
    )
}

/// Take the page of `records` that resumes after `marker` and holds at most `page_size`
/// items. `records` must be in the order the report serves them.
///
/// A `Marker` names a POSITION in that order — the last record the page that issued it
/// served — and the page it resumes is "only records beyond the marker"
/// (`DescribeDBParametersMessage$Marker`). So the token is placed rather than required to
/// still be a record: the report resumes at the first record after the key the token
/// carries, which is the same page whether or not that record is still there. A report
/// whose own order is NOT its key order can only place a key it still holds, so a token
/// naming a key outside it is refused there.
///
/// A token this front door's codec could not have produced is REFUSED (the returned message
/// is what the caller refuses the request with): it names no position at all, and
/// resynchronising it to the first page would report a page the caller cannot place. The
/// refusal carries [`invalid_marker_message`], the wording of every report bar the parameter
/// ones, which page through [`paginate_parameters`] instead.
pub fn paginate<T>(
    records: Vec<(String, T)>,
    marker: Option<&str>,
    page_size: usize,
) -> Result<Page<T>, String> {
    paginate_refusing_with(records, marker, page_size, |_| invalid_marker_message())
}

/// [`paginate`], for the PARAMETER reports: the page is cut the same way, and a token this
/// codec could not have produced is refused with those reports' own wording
/// ([`invalid_parameter_marker_message`]), which echoes the token the request carried.
pub fn paginate_parameters<T>(
    records: Vec<(String, T)>,
    marker: Option<&str>,
    page_size: usize,
) -> Result<Page<T>, String> {
    paginate_refusing_with(records, marker, page_size, invalid_parameter_marker_message)
}

/// The page cut both paginators share: `refuse` is the rejecting report's own invalid-marker
/// sentence, which is the only thing that differs between them.
fn paginate_refusing_with<T>(
    records: Vec<(String, T)>,
    marker: Option<&str>,
    page_size: usize,
    refuse: fn(&str) -> String,
) -> Result<Page<T>, String> {
    let start = match marker {
        None => 0,
        Some(marker) => resume_after(&records, marker).ok_or_else(|| refuse(marker))?,
    };
    let end = start.saturating_add(page_size).min(records.len());
    let next_marker = (end < records.len())
        .then(|| end.checked_sub(1))
        .flatten()
        .and_then(|last| records.get(last))
        .map(|(key, _)| encode_marker(key));
    let items = records
        .into_iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|(_, record)| record)
        .collect();
    Ok(Page { items, next_marker })
}

/// Where in `records` a report resumes for the token `marker`: the index of the first record
/// beyond the position that token names, or `None` when the token names no position in this
/// report at all.
///
/// The key the token carries is placed two ways, in this order:
///
/// * the record that holds it, when the report still holds it — the common case, and the
///   only one a report in any order can answer;
/// * otherwise, in a report whose records are in ascending key order, the key's own place in
///   that order, so a record that was removed between two calls does not cost the caller the
///   rest of the report.
fn resume_after<T>(records: &[(String, T)], marker: &str) -> Option<usize> {
    let key = decode_marker(marker)?;
    if let Some(index) = records.iter().position(|(held, _)| *held == key) {
        return Some(index.saturating_add(1));
    }
    let ascending = records.is_sorted_by_key(|(key, _)| key);
    ascending.then(|| records.partition_point(|(held, _)| *held <= key))
}
