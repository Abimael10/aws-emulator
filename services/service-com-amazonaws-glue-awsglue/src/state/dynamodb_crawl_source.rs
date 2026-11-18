// SPDX-License-Identifier: CC0-1.0

//! Where a CRAWL of an Amazon DynamoDB target reads its data from, and how it resolves the
//! items behind that target into the table the Data Catalog is populated with.
//!
//! A `DynamoDBTarget$Path` is "the name of the DynamoDB table to crawl", so a crawl of one
//! decides two things from what is really in that table: its COLUMNS (one per attribute the
//! items carry, typed by what the attribute holds) and the ORDER they are declared in (the
//! table's own key attributes first, as its definition lists them, then every other attribute
//! by name — an item's attributes are a map and carry no order of their own). A DynamoDB
//! table is not a path, so the dataset has no partition keys and no partitions: the items of
//! one table are one unpartitioned dataset.
//!
//! The table is read out of the emulator's OWN DynamoDB, through the framework's in-process
//! callback ([`smithy_server_runtime::OperationContext::callback`]) — the same transport a
//! client's own DynamoDB request takes, so a crawl classifies exactly the items that were
//! written. Nothing here opens a socket, resolves a host or reads an ambient credential.
//!
//! HOW MUCH IS READ IS BOUNDED, before any of it is taken into this process: how many items
//! the whole walk scans ([`MAX_CRAWLED_ITEMS`], the budget the Amazon S3 walk's
//! [`MAX_CRAWLED_OBJECTS`] is the other half of), how many one page of a scan returns
//! ([`SCANNED_ITEMS_PER_PAGE`]) and how many are read from a target that asked to be SAMPLED
//! rather than scanned whole ([`SAMPLED_CRAWLED_ITEMS`]). The whole walk also runs inside the
//! crawl's own wall clock ([`CRAWL_BUDGET`]) and holds one of its permits
//! ([`MAX_CONCURRENT_CRAWLS`]), because it is performed by the same crawl. A target holding
//! more items than one crawl scans is data this crawl could not read, which is a crawl that
//! failed rather than one reporting part of the data as the whole of it.

use ::aws_sdk_dynamodb::types::AttributeValue;
use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

use super::*;

/// How many items ONE CRAWL scans, across every Amazon DynamoDB target it walks.
///
/// CHOSEN, not an AWS quota: the items behind a target are written by callers, so the scan is
/// bounded here — across the whole crawl rather than per target, because a crawler names as
/// many targets as a create cared to give it. This is the item-side counterpart of
/// [`MAX_CRAWLED_OBJECTS`].
pub const MAX_CRAWLED_ITEMS: usize = 1_000;

/// How many items ONE PAGE of a scan asks for, which is what `Scan$Limit` carries.
///
/// CHOSEN: it bounds how much of the table arrives in one answer, so the walk reads a large
/// table in pages rather than in one response, and small enough that a target whose items are
/// past [`MAX_CRAWLED_ITEMS`] is recognised without buffering much more than the ceiling.
pub const SCANNED_ITEMS_PER_PAGE: usize = 100;

/// How many PAGES of a scan one crawl reads, whatever each of them held.
///
/// The items themselves are bounded by [`MAX_CRAWLED_ITEMS`], and a page carries at least one
/// item while there is one left — so one page per item the crawl may read is an upper bound
/// that cannot be reached in practice and still stops the walk where a store keeps handing
/// back a continuation token without items. A paging loop over an answer this process does
/// not control needs a bound of its own, which this is.
pub const MAX_SCANNED_PAGES: usize = MAX_CRAWLED_ITEMS + 1;

/// How many items are read from a target that asked to be SAMPLED: "a value of `true` means
/// to scan all records, while a value of `false` means to sample the records"
/// (`DynamoDBTarget$scanAll`).
///
/// CHOSEN, not an AWS figure: Glue publishes no sample size, and the schema a crawl infers is
/// read off the records it read — so a sampled target reads this many items and the figures
/// its table records are those of that sample.
pub const SAMPLED_CRAWLED_ITEMS: usize = 50;

/// The value [`CLASSIFICATION_PARAMETER`] carries for a dataset that is the items of an
/// Amazon DynamoDB table: the store the data sits in, which is what a reader of the table
/// keys its reader off.
pub const CLASSIFICATION_DYNAMODB: &str = "dynamodb";

/// `Column$Type` of an attribute holding binary data (`AttributeValue$B`), which Hive spells
/// this way.
pub const BINARY_TYPE: &str = "binary";

/// `Column$Type` of an attribute holding a SET (`AttributeValue$SS`, `$NS`, `$BS`): a set is a
/// collection of one scalar type, which Hive spells as the array of that type.
///
/// The element type is the set's own — text, the number type its first member reads as, or
/// binary.
fn set_type(element: &str) -> String {
    format!("array<{element}>")
}

/// `LastCrawlInfo$ErrorMessage` of a crawl whose DynamoDB target holds more items than one
/// crawl scans ([`MAX_CRAWLED_ITEMS`]).
///
/// A condition on the DATA rather than on the store's answer about it, so it is worded apart
/// from [`crawl_target_unreadable`]: the crawl says the table holds more than it reads rather
/// than reporting part of the table as the whole of it. The variable part is the table the
/// crawl was reading.
pub fn crawl_target_past_the_item_ceiling(table: &str) -> String {
    format!(
        "The crawl target {table} holds more than the {MAX_CRAWLED_ITEMS} items one crawl \
         scans, so no table was created for it."
    )
}

/// Walk the Amazon DynamoDB targets of `plan` and answer the datasets behind them, one per
/// target that holds items, in the order the crawler names them.
///
/// A target whose table holds no item at all yields no dataset — there is nothing there to
/// read a column off — and so does one whose items carry no attribute the catalog can hold a
/// column name for: a crawl writes a table for data it read, never an empty table for data it
/// could not.
///
/// `Err` is why the crawl could not read a target, which is what it reports in
/// `LastCrawlInfo$ErrorMessage`: the store answered that it does not serve the table
/// ([`crawl_target_unreadable`]), or the table holds more items than one crawl scans
/// ([`crawl_target_past_the_item_ceiling`]).
///
/// A target the crawl role may not read is not read at all ([`CrawlReadAuthorization`]): the
/// definition read and the scan are each asked for before they are made, and a target the
/// account's records withhold yields no dataset, so no table is written for it — the rule the
/// Amazon S3 walk follows for its listing and its object read.
pub(super) async fn walk_dynamodb_targets(
    client: &::aws_sdk_dynamodb::Client,
    region: &str,
    plan: &CrawlPlan,
    authorization: &CrawlReadAuthorization,
) -> Result<Vec<DiscoveredTable>, String> {
    let mut discovered: Vec<DiscoveredTable> = Vec::new();
    let mut named: HashSet<String> = HashSet::new();
    // How many items the whole crawl may still scan: one budget across the targets, charged
    // by what each scan really returned.
    let mut budget = MAX_CRAWLED_ITEMS;
    for target in &plan.dynamo_db_targets {
        let Some(table) = target
            .path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
        else {
            continue;
        };
        // "A value of true means to scan all records, while a value of false means to sample
        // the records. If no value is specified, the value defaults to true."
        let scan_all = target.scan_all.unwrap_or(true);
        let ceiling = if scan_all {
            budget
        } else {
            SAMPLED_CRAWLED_ITEMS.min(budget)
        };
        // The two reads the crawl is about to make against this table are the two the records
        // decide: a crawl role that may not read the table's definition, or may not scan its
        // items, reads none of it, and the target yields no dataset.
        let table_arn = dynamodb_table_arn(region, table);
        let withheld = DYNAMODB_TARGET_ACTIONS
            .iter()
            .find_map(|action| authorization.withholds_table_read(action, &table_arn));
        if let Some(detail) = withheld {
            ::tracing::debug!(
                target = ?table,
                detail = %detail,
                "a Glue crawl is not permitted to read one of its DynamoDB targets"
            );
            continue;
        }
        let described = match describe_table(client, table).await {
            Ok(described) => described,
            Err(detail) => {
                // The target's own value is rendered in its escaped `Debug` form, so a table
                // name carrying a newline cannot forge a log line.
                ::tracing::debug!(
                    target = ?table,
                    detail = %detail,
                    "a Glue crawl could not read the definition of one of its DynamoDB targets"
                );
                return Err(crawl_target_unreadable(table));
            }
        };
        let items = match scan_items(client, table, ceiling, scan_all).await {
            Ok(items) => items,
            Err(ScanRefusal::Unreadable(detail)) => {
                ::tracing::debug!(
                    target = ?table,
                    detail = %detail,
                    "a Glue crawl could not read the items of one of its DynamoDB targets"
                );
                return Err(crawl_target_unreadable(table));
            }
            // The table holds more than one crawl reads, which is a condition on the DATA
            // rather than on the store's answer about it — so the crawl reports which of the
            // two it was.
            Err(ScanRefusal::PastTheItemCeiling) => {
                return Err(crawl_target_past_the_item_ceiling(table));
            }
        };
        budget = budget.saturating_sub(items.len());
        if items.is_empty() {
            continue;
        }
        let columns = columns_of(&items, &described.key_attributes);
        if columns.is_empty() {
            continue;
        }
        let Some(name) = crawled_table_name(table) else {
            continue;
        };
        let size_bytes: u64 = items.iter().map(item_bytes).sum();
        let dataset = DiscoveredTable {
            name,
            classification: CLASSIFICATION_DYNAMODB.to_string(),
            columns,
            // A DynamoDB table is one dataset with no folder levels below it, so nothing
            // partitions it.
            partition_keys: Vec::new(),
            partitions: Vec::new(),
            record_count: u64::try_from(items.len()).unwrap_or(u64::MAX),
            size_bytes,
            storage: DatasetStorage::StoreItems {
                // WHERE the data sits is the table itself, as the store that owns it
                // addresses the table: the ARN its own description reported, falling back to
                // the form that service mints a table ARN of this account in.
                location: described
                    .arn
                    .unwrap_or_else(|| dynamodb_table_arn(region, table)),
            },
        };
        // Two targets that resolve to the same table name describe one dataset, and the
        // first of them is the one that described it — the rule the Amazon S3 walk follows.
        if named.insert(dataset.name.clone()) {
            discovered.push(dataset);
        }
    }
    Ok(discovered)
}

/// One item a crawl scanned: the attributes it carries, as the table's own data plane
/// reported them.
type ScannedItem = ::std::collections::HashMap<String, AttributeValue>;

/// What a crawl reads off one DynamoDB table's own DESCRIPTION: where the table is, and which
/// of its attributes are key attributes.
struct DescribedTable {
    /// The resource that addresses the table, as the store itself reported it
    /// (`TableDescription$TableArn`), which is the dataset's location. `None` when the
    /// description carried none.
    arn: Option<String>,
    /// The table's KEY attributes, in the order its definition declares them — the partition
    /// key first and the sort key, where it has one, after it.
    key_attributes: Vec<String>,
}

/// The table's own description: where it is and which attributes are its keys.
///
/// Read with the table's own `DescribeTable`, which is also the call that establishes the
/// table is there at all: a target the store does not serve answers an error here, and that
/// is a crawl that failed rather than one writing a table for data nothing held.
async fn describe_table(
    client: &::aws_sdk_dynamodb::Client,
    table: &str,
) -> Result<DescribedTable, String> {
    let described = client
        .describe_table()
        .table_name(table)
        .send()
        .await
        .map_err(|error| {
            format!(
                "the table store answered {:?} to the description: {}",
                error.code(),
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        })?;
    let description = described.table();
    Ok(DescribedTable {
        arn: description
            .and_then(|description| description.table_arn())
            .map(str::to_string),
        key_attributes: description
            .map(|description| {
                description
                    .key_schema()
                    .iter()
                    .map(|element| element.attribute_name().to_string())
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Why a crawl could not scan the items behind one target.
///
/// Two conditions, told APART because they are about different things and the crawl reports
/// each in its own words: the store would not serve the scan, and the table holding more items
/// than one crawl reads.
enum ScanRefusal {
    /// The table store answered something other than the items, for the crawl's own log.
    Unreadable(String),
    /// The table holds more than the items one crawl scans ([`MAX_CRAWLED_ITEMS`]).
    PastTheItemCeiling,
}

/// The items behind one target, at most `ceiling` of them.
///
/// A target scanned WHOLE (`scanAll`) whose table holds more than the ceiling is one this
/// crawl could not read, so it answers why rather than reporting part of the table as the
/// whole of it; a target asking to be SAMPLED stops at the ceiling, which is the sample it
/// asked for.
async fn scan_items(
    client: &::aws_sdk_dynamodb::Client,
    table: &str,
    ceiling: usize,
    scan_all: bool,
) -> Result<Vec<ScannedItem>, ScanRefusal> {
    // A whole scan reads ONE item past the ceiling, which is how a table holding more than a
    // crawl reads is recognised; a sample never reads past it.
    let stop_at = if scan_all {
        ceiling.saturating_add(1)
    } else {
        ceiling
    };
    // `Scan$Limit` is "the maximum number of items to evaluate", which has to be at least
    // one for the request to be served at all.
    let page_limit = i32::try_from(stop_at.clamp(1, SCANNED_ITEMS_PER_PAGE)).unwrap_or(1);
    let mut items: Vec<ScannedItem> = Vec::new();
    let mut start_key: Option<ScannedItem> = None;
    // The paging is bounded on its own account ([`MAX_SCANNED_PAGES`]), so a store that keeps
    // answering with a continuation token cannot keep the walk going without end.
    for _page in 0..MAX_SCANNED_PAGES {
        let page = client
            .scan()
            .table_name(table)
            .limit(page_limit)
            .set_exclusive_start_key(start_key.clone())
            .send()
            .await
            .map_err(|error| {
                ScanRefusal::Unreadable(format!(
                    "the table store answered {:?} to the scan: {}",
                    error.code(),
                    ::aws_smithy_types::error::display::DisplayErrorContext(&error)
                ))
            })?;
        for item in page.items.unwrap_or_default() {
            items.push(item);
            if items.len() >= stop_at {
                break;
            }
        }
        if items.len() >= stop_at {
            break;
        }
        match page.last_evaluated_key {
            Some(key) if !key.is_empty() => start_key = Some(key),
            _ => break,
        }
    }
    if scan_all && items.len() > ceiling {
        return Err(ScanRefusal::PastTheItemCeiling);
    }
    Ok(items)
}

/// The columns of the dataset `items` are: one per attribute they carry, named as the catalog
/// holds a column name and typed by what the attribute holds.
///
/// The ORDER is the table's own key attributes first, as its definition declares them, then
/// every other attribute by name: an item's attributes are a map and carry no order of their
/// own, so the order a crawl declares them in is decided here rather than by which item
/// happened to be scanned first.
///
/// An attribute two items hold DIFFERENT types in reads as text, which is what a value
/// nothing single can be inferred from reads as. Two attributes the catalog holds under one
/// column name are one column, and the first of them is the one that named it.
fn columns_of(items: &[ScannedItem], key_attributes: &[String]) -> Vec<Column> {
    let mut inferred: BTreeMap<String, String> = BTreeMap::new();
    for item in items {
        for (attribute, value) in item {
            let rendered = attribute_column_type(value, 0);
            match inferred.get(attribute) {
                None => {
                    inferred.insert(attribute.clone(), rendered);
                }
                Some(held) if *held != rendered => {
                    inferred.insert(attribute.clone(), STRING_TYPE.to_string());
                }
                Some(_) => {}
            }
        }
    }

    let ordered = key_attributes
        .iter()
        .filter(|attribute| inferred.contains_key(*attribute))
        .cloned()
        .chain(
            inferred
                .keys()
                .filter(|attribute| !key_attributes.contains(attribute))
                .cloned(),
        );
    let mut held: HashSet<String> = HashSet::new();
    let mut columns: Vec<Column> = Vec::new();
    for attribute in ordered {
        let Some(name) = catalog_name(&attribute) else {
            continue;
        };
        if !held.insert(name.clone()) {
            continue;
        }
        columns.push(Column {
            name,
            r#type: inferred.get(&attribute).cloned(),
            ..Column::default()
        });
    }
    columns
}

/// The `Column$Type` one attribute's value reads as.
///
/// Each DynamoDB attribute type is rendered as the Hive type that holds it: text, the number
/// type its text reads as (which is the reading a delimited cell's number gets), a boolean,
/// binary, the array of a list's or a set's element type, and the struct of a map's members —
/// each spelled the way Hive spells a nested type. A value nothing can be inferred from —
/// DynamoDB's own null, an empty list or map — reads as text, and so does everything past
/// [`MAX_JSON_TYPE_DEPTH`] levels of nesting, which bounds how deep a caller-written document
/// is rendered.
fn attribute_column_type(value: &AttributeValue, depth: usize) -> String {
    if depth >= MAX_JSON_TYPE_DEPTH {
        return STRING_TYPE.to_string();
    }
    match value {
        AttributeValue::S(_) | AttributeValue::Null(_) => STRING_TYPE.to_string(),
        AttributeValue::N(number) => delimited_column_type(number).to_string(),
        AttributeValue::Bool(_) => BOOLEAN_TYPE.to_string(),
        AttributeValue::B(_) => BINARY_TYPE.to_string(),
        AttributeValue::Ss(_) => set_type(STRING_TYPE),
        AttributeValue::Ns(numbers) => set_type(
            numbers
                .first()
                .map_or(STRING_TYPE, |number| delimited_column_type(number)),
        ),
        AttributeValue::Bs(_) => set_type(BINARY_TYPE),
        AttributeValue::L(elements) => {
            let element = elements.first().map_or_else(
                || STRING_TYPE.to_string(),
                |first| attribute_column_type(first, depth + 1),
            );
            format!("array<{element}>")
        }
        AttributeValue::M(members) => {
            // A map's members are rendered in NAME order, for the reason the columns
            // themselves are: the map carries no order of its own.
            let ordered: BTreeMap<&String, &AttributeValue> = members.iter().collect();
            let rendered: Vec<String> = ordered
                .into_iter()
                .filter_map(|(member, value)| {
                    catalog_name(member)
                        .map(|name| format!("{name}:{}", attribute_column_type(value, depth + 1)))
                })
                .collect();
            format!("struct<{}>", rendered.join(","))
        }
        // The attribute types of the data plane are the ones above; a value of a type this
        // build of the SDK does not name is one nothing can be inferred from.
        _ => STRING_TYPE.to_string(),
    }
}

/// How many bytes one scanned item holds: "the size of an item is the sum of the lengths of
/// its attribute names and values" (the DynamoDB item-size rule), which is the measure the
/// dataset's `sizeKey` reports.
///
/// BOUNDED the way the type reading of the same values is ([`MAX_JSON_TYPE_DEPTH`]): the
/// values are caller-written documents, so the walk stops at that depth rather than recursing
/// as deep as a stored item nests.
fn item_bytes(item: &ScannedItem) -> u64 {
    item.iter()
        .map(|(attribute, value)| {
            u64::try_from(attribute.len()).unwrap_or(u64::MAX) + attribute_bytes(value, 0)
        })
        .sum()
}

/// How many bytes one attribute VALUE holds, by the rule [`item_bytes`] states: the length of
/// the text a scalar carries, the bytes a binary value carries, one byte for a value that is
/// its own type (a boolean, a null), and the sum of a collection's members — a map counting
/// its member names as well.
///
/// A value nested past [`MAX_JSON_TYPE_DEPTH`] levels counts for nothing, which is where the
/// walk stops: how deep a caller's document nests is the caller's choice, so the recursion is
/// bounded by the same constant the type reading of those values honours.
fn attribute_bytes(value: &AttributeValue, depth: usize) -> u64 {
    fn length(bytes: usize) -> u64 {
        u64::try_from(bytes).unwrap_or(u64::MAX)
    }

    if depth >= MAX_JSON_TYPE_DEPTH {
        return 0;
    }
    match value {
        AttributeValue::S(text) => length(text.len()),
        AttributeValue::N(number) => length(number.len()),
        AttributeValue::B(blob) => length(blob.as_ref().len()),
        AttributeValue::Bool(_) | AttributeValue::Null(_) => 1,
        AttributeValue::Ss(members) => members.iter().map(|text| length(text.len())).sum(),
        AttributeValue::Ns(members) => members.iter().map(|number| length(number.len())).sum(),
        AttributeValue::Bs(members) => members.iter().map(|blob| length(blob.as_ref().len())).sum(),
        AttributeValue::L(elements) => elements
            .iter()
            .map(|element| attribute_bytes(element, depth + 1))
            .sum(),
        AttributeValue::M(members) => members
            .iter()
            .map(|(member, value)| length(member.len()) + attribute_bytes(value, depth + 1))
            .sum(),
        _ => 0,
    }
}

/// The name the catalog holds the table a DynamoDB target's dataset is written as: the
/// table's own name, as the catalog holds a column or table name a crawl read off its data
/// ([`catalog_name`]) — lowercased, with every character the catalog does not hold a name in
/// replaced by an underscore, which is the rule the name of a dataset behind an Amazon S3
/// target is held to as well.
///
/// `None` for a name there is nothing left of, which is not a name the catalog can address a
/// table by.
fn crawled_table_name(table: &str) -> Option<String> {
    catalog_name(table)
}

/// An Amazon DynamoDB client over the in-process callback into this same emulator.
///
/// The callback is the framework's documented seam for calling another emulated service: it
/// routes IN PROCESS, so no network client is configured here and the `endpoint_url` below is
/// an address the in-process router reads rather than a host any resolver looks up. `region`
/// is the request's own signing region, so a crawl reads where the caller's own DynamoDB
/// requests go; it is narrowed before it gets here ([`request_region`] admits only a value
/// shaped like a Region identifier).
///
/// There is nothing to retry, time out or watch for a stalled stream, each of which would
/// need a sleep implementation this client is not given. The same goes for caching the
/// identity: the credentials the callback hands over are used directly, and they only sign
/// this operation's in-process calls. Written the way this crate's Amazon S3 client is, so
/// every read a crawl makes of another service goes over one client configuration per
/// service.
pub(super) fn dynamodb_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_dynamodb::Client {
    let config = ::aws_sdk_dynamodb::Config::builder()
        .behavior_version(::aws_sdk_dynamodb::config::BehaviorVersion::latest())
        .region(::aws_sdk_dynamodb::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://dynamodb.{region}.amazonaws.com"))
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .http_client(ctx.callback().http_client())
        .retry_config(::aws_sdk_dynamodb::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_dynamodb::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_dynamodb::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_dynamodb::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_dynamodb::Client::from_conf(config)
}
