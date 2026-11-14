// SPDX-License-Identifier: CC0-1.0

//! What a pipeline's Data Prepper configuration SAYS about its ingest surface: which paths
//! accept records, and where the records a path accepts are written.
//!
//! `CreatePipeline$PipelineConfigurationBody` is "The pipeline configuration in YAML format"
//! — a Data Prepper pipeline set: a `version` key beside one or more named sub-pipelines,
//! each with a `source`, optional `processor`s and one or more `sink`s. A pipeline whose
//! source is `http` serves records POSTed to that source's `path`, and the sink names where
//! they go. Both are READ here, out of the configuration the caller stored, so the address
//! the pipeline publishes serves exactly the paths its own configuration declares and the
//! records land exactly where it says.
//!
//! The reader is deliberately narrow: it answers the two questions the ingest plane asks
//! ([`ingest_routes`]) and nothing else. A configuration member it does not read is left
//! alone rather than guessed at, and a sink kind this emulator cannot write is reported as
//! such ([`PipelineSink::Unsupported`]) rather than quietly dropped — the plane answers for
//! it instead of reporting a delivery nothing made.
//!
//! The document is a CALLER's, so it is read through the crate's one YAML reading
//! ([`super::document_as_json`]) and every member is treated as optional: a body that does
//! not parse, or that declares no http source, yields no route at all, which is an address
//! that accepts nothing. How deeply it NESTS is bounded before it is handed to the parser
//! ([`MAX_CONFIGURATION_NESTING`]), because a parse costs a level of recursion per nested
//! collection and the text's shape is the caller's to choose.

use ::serde_json::Value;

/// How deeply a pipeline configuration this emulator reads may NEST.
///
/// EMULATOR RESOURCE GUARD, not an AWS limit: `PipelineConfigurationBody` is
/// `@length(1, 100000)`, which bounds the document's SIZE but not its shape, and a level of
/// nesting costs as little as one byte — so a body is measured against this bound before it
/// is parsed (`service_common::yaml_nesting`), and one past it declares no route rather than
/// recursing. It is far past any Data Prepper pipeline set, which nests a handful of levels.
pub const MAX_CONFIGURATION_NESTING: usize = 200;

/// The configuration key naming a sub-pipeline's source.
const SOURCE_KEY: &str = "source";

/// The configuration key naming a sub-pipeline's sinks, which is a SEQUENCE of single-key
/// maps (`- s3: {...}`).
const SINK_KEY: &str = "sink";

/// The top-level key that names the Data Prepper configuration version rather than a
/// sub-pipeline.
const VERSION_KEY: &str = "version";

/// The source kind that accepts records over HTTP, which is the one an ingestion endpoint
/// serves.
pub const HTTP_SOURCE: &str = "http";

/// The `http` source's path option.
const PATH_OPTION: &str = "path";

/// The path an `http` source that names none accepts records at.
///
/// Source: the Data Prepper `http` source documentation, whose `path` option defaults to
/// `/log/ingest`.
pub const DEFAULT_HTTP_SOURCE_PATH: &str = "/log/ingest";

/// The sink kind that writes into an Amazon S3 bucket.
pub const S3_SINK: &str = "s3";

/// The `s3` sink's bucket option.
const BUCKET_OPTION: &str = "bucket";

/// The `s3` sink's object-key container, and the prefix within it every written object lands
/// under (the Data Prepper `s3` sink's `object_key.path_prefix`, which defaults to the root
/// of the bucket).
const OBJECT_KEY_OPTION: &str = "object_key";
const PATH_PREFIX_OPTION: &str = "path_prefix";

/// The `s3` sink's codec container, which names the FORMAT the written object carries.
const CODEC_OPTION: &str = "codec";

/// The `aws` container a sink reads its Region from, and the Region option in it.
const AWS_OPTION: &str = "aws";
const REGION_OPTION: &str = "region";

/// The format a delivered object carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkCodec {
    /// Newline-delimited JSON: one record per line. The Data Prepper `ndjson` codec, and
    /// what a sink naming no codec is written as.
    Ndjson,
    /// One JSON document holding the records of the delivery. The Data Prepper `json`
    /// codec.
    Json,
    /// A codec this emulator does not write (`parquet`, `avro`): named so the plane can say
    /// so rather than writing bytes in another format under it.
    Unsupported(String),
}

impl SinkCodec {
    /// The codec `options` names — a single-key map (`ndjson:`), or the bare name.
    fn parse(options: &Value) -> Self {
        let named = match options {
            Value::String(name) => name.clone(),
            Value::Object(members) => match members.keys().next() {
                Some(name) => name.clone(),
                None => return Self::Ndjson,
            },
            _ => return Self::Ndjson,
        };
        match named.as_str() {
            "ndjson" => Self::Ndjson,
            "json" => Self::Json,
            _ => Self::Unsupported(named),
        }
    }
}

/// An Amazon S3 sink as its configuration declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Sink {
    /// The bucket the records are written to.
    pub bucket: String,
    /// The key prefix every written object lands under, empty for the root of the bucket.
    pub path_prefix: String,
    /// The Region the bucket is addressed in, when the sink names one; the pipeline's own
    /// Region otherwise.
    pub region: Option<String>,
    /// The format the written object carries.
    pub codec: SinkCodec,
}

/// One sink of a sub-pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineSink {
    /// An Amazon S3 bucket, which this emulator writes.
    S3(S3Sink),
    /// A sink KIND this emulator does not write, named by its configuration key.
    UnsupportedKind(String),
    /// A sink declaration that names no destination to write records to: an `s3` sink with
    /// no `bucket`, or a sequence entry that names no sink kind at all (`None`). Unlike
    /// [`Self::UnsupportedKind`] this is a configuration its author can correct.
    NoDestination(Option<String>),
}

/// One ingest route a pipeline's configuration declares: the path its `http` source accepts
/// records at, and where that sub-pipeline writes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestRoute {
    /// The sub-pipeline the route belongs to, as the configuration names it.
    pub sub_pipeline: String,
    /// The path within the pipeline's ingestion endpoint, always absolute and without a
    /// trailing separator.
    pub path: String,
    /// Every sink that sub-pipeline declares, in configuration order.
    pub sinks: Vec<PipelineSink>,
}

/// Every ingest route `configuration_body` declares, in the order the document names its
/// sub-pipelines.
///
/// Empty for a body that does not parse as one YAML document, one that is not a mapping, and
/// one whose sub-pipelines declare no `http` source: each of those is a pipeline with no
/// HTTP ingest surface at all.
pub fn ingest_routes(configuration_body: &str) -> Vec<IngestRoute> {
    if ::service_common::yaml_nesting::nesting_depth_bound(
        configuration_body,
        MAX_CONFIGURATION_NESTING,
    ) > MAX_CONFIGURATION_NESTING
    {
        return Vec::new();
    }
    let Ok(document) = super::document_as_json(configuration_body) else {
        return Vec::new();
    };
    let Some(members) = document.as_object() else {
        return Vec::new();
    };
    members
        .iter()
        .filter(|(name, _)| name.as_str() != VERSION_KEY)
        .filter_map(|(name, declared)| route_of(name, declared))
        .collect()
}

/// The route `routes` serves `path` at, or `None` when no source of this pipeline accepts
/// records there.
///
/// The comparison is on the normalised path ([`normalized_path`]), so a request that wrote
/// the source's path with or without a trailing separator reaches the same source.
pub fn route_at<'routes>(routes: &'routes [IngestRoute], path: &str) -> Option<&'routes IngestRoute> {
    let addressed = normalized_path(path);
    routes.iter().find(|route| route.path == addressed)
}

/// One sub-pipeline's route, or `None` when it declares no `http` source.
fn route_of(sub_pipeline: &str, declared: &Value) -> Option<IngestRoute> {
    let source = declared.get(SOURCE_KEY)?.get(HTTP_SOURCE)?;
    let path = source
        .get(PATH_OPTION)
        .and_then(Value::as_str)
        .map_or_else(
            || DEFAULT_HTTP_SOURCE_PATH.to_string(),
            normalized_path,
        );
    let sinks = declared
        .get(SINK_KEY)
        .and_then(Value::as_array)
        .map(|declared| declared.iter().map(sink_of).collect())
        .unwrap_or_default();
    Some(IngestRoute {
        sub_pipeline: sub_pipeline.to_string(),
        path,
        sinks,
    })
}

/// One entry of a `sink` sequence, which Data Prepper writes as a single-key map naming the
/// sink kind.
///
/// Every entry is reported, including one this emulator cannot write and one that names no
/// destination: a sink a POST was answered for without writing to it would be a success for
/// records that sink never held, so the plane has to see each of them.
fn sink_of(declared: &Value) -> PipelineSink {
    let Some((kind, options)) = declared
        .as_object()
        .and_then(|members| members.iter().next())
    else {
        return PipelineSink::NoDestination(None);
    };
    if kind != S3_SINK {
        return PipelineSink::UnsupportedKind(kind.clone());
    }
    match s3_sink(options) {
        Some(sink) => PipelineSink::S3(sink),
        None => PipelineSink::NoDestination(Some(kind.clone())),
    }
}

/// An `s3` sink's options, or `None` when it names no bucket — which names no destination at
/// all.
fn s3_sink(options: &Value) -> Option<S3Sink> {
    let bucket = options.get(BUCKET_OPTION)?.as_str()?.trim();
    if bucket.is_empty() {
        return None;
    }
    let path_prefix = options
        .get(OBJECT_KEY_OPTION)
        .and_then(|object_key| object_key.get(PATH_PREFIX_OPTION))
        .and_then(Value::as_str)
        .map(normalized_prefix)
        .unwrap_or_default();
    let region = options
        .get(REGION_OPTION)
        .or_else(|| options.get(AWS_OPTION).and_then(|aws| aws.get(REGION_OPTION)))
        .and_then(Value::as_str)
        .map(str::trim)
        // A Region read here is not just data: it ends up in the authority of the endpoint
        // the delivery is addressed to, so only a Region LABEL is taken (the tree's one
        // reading of that question), and anything else leaves the sink on the pipeline's own
        // Region rather than pointing a write at a value a caller wrote.
        .and_then(::service_common::arn::region_label)
        .map(str::to_string);
    Some(S3Sink {
        bucket: bucket.to_string(),
        path_prefix,
        region,
        codec: options.get(CODEC_OPTION).map_or(SinkCodec::Ndjson, SinkCodec::parse),
    })
}

/// A source path as both the configuration and a request are read by: absolute, and without
/// a trailing separator unless it is the root itself.
pub fn normalized_path(path: &str) -> String {
    let trimmed = path.trim();
    let absolute = match trimmed.strip_prefix('/') {
        Some(rest) => rest,
        None => trimmed,
    };
    let without_trailing = absolute.trim_end_matches('/');
    format!("/{without_trailing}")
}

/// A key prefix as every written object lands under it: no leading separator (a key is not a
/// path), and ending in one when it names anything at all, so the object's own name follows
/// it as a segment.
fn normalized_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim().trim_start_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    format!("{}/", trimmed.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the routes a pipeline serves, and where each writes, are read out of the
    /// configuration body the caller stored.
    ///
    /// The rows differ along what the reading generalises over: whether the source names a
    /// path, the shape of the path it names, how many sub-pipelines the document declares,
    /// the sink kind, whether the sink names a prefix, a Region and a codec, and the two
    /// surface syntaxes one document can be written in (block YAML and JSON).
    #[test]
    fn the_routes_and_sinks_are_the_ones_the_configuration_declares() {
        struct Row {
            case: &'static str,
            body: &'static str,
            routes: Vec<IngestRoute>,
        }
        let rows = vec![
            Row {
                case: "an http source naming its path, and an s3 sink",
                body: "version: \"2\"\nlog-pipeline:\n  source:\n    http:\n      path: \"/ingest\"\n  \
                       sink:\n    - s3:\n        bucket: \"records\"\n        object_key:\n          \
                       path_prefix: \"logs/\"\n        codec:\n          ndjson:\n",
                routes: vec![IngestRoute {
                    sub_pipeline: "log-pipeline".to_string(),
                    path: "/ingest".to_string(),
                    sinks: vec![PipelineSink::S3(S3Sink {
                        bucket: "records".to_string(),
                        path_prefix: "logs/".to_string(),
                        region: None,
                        codec: SinkCodec::Ndjson,
                    })],
                }],
            },
            Row {
                case: "an http source naming no path takes the documented default",
                body: "version: \"2\"\nlog-pipeline:\n  source:\n    http:\n  sink:\n    - s3:\n        \
                       bucket: \"records\"\n",
                routes: vec![IngestRoute {
                    sub_pipeline: "log-pipeline".to_string(),
                    path: DEFAULT_HTTP_SOURCE_PATH.to_string(),
                    sinks: vec![PipelineSink::S3(S3Sink {
                        bucket: "records".to_string(),
                        path_prefix: String::new(),
                        region: None,
                        codec: SinkCodec::Ndjson,
                    })],
                }],
            },
            Row {
                case: "a path written without a leading separator and with a trailing one, \
                       and a sink naming its Region and the json codec",
                body: "version: \"2\"\ndeep:\n  source:\n    http:\n      path: \"deep/ingest/\"\n  \
                       sink:\n    - s3:\n        bucket: \"records\"\n        aws:\n          \
                       region: \"eu-west-1\"\n        codec:\n          json:\n",
                routes: vec![IngestRoute {
                    sub_pipeline: "deep".to_string(),
                    path: "/deep/ingest".to_string(),
                    sinks: vec![PipelineSink::S3(S3Sink {
                        bucket: "records".to_string(),
                        path_prefix: String::new(),
                        region: Some("eu-west-1".to_string()),
                        codec: SinkCodec::Json,
                    })],
                }],
            },
            Row {
                case: "a document in JSON syntax declaring two sub-pipelines, one of which \
                       has no http source",
                body: "{\"version\": \"2\", \
                       \"a-pipeline\": {\"source\": {\"http\": {\"path\": \"/a\"}}, \
                       \"sink\": [{\"opensearch\": {\"index\": \"logs\"}}]}, \
                       \"b-pipeline\": {\"source\": {\"s3\": {}}, \
                       \"sink\": [{\"s3\": {\"bucket\": \"records\"}}]}}",
                routes: vec![IngestRoute {
                    sub_pipeline: "a-pipeline".to_string(),
                    path: "/a".to_string(),
                    sinks: vec![PipelineSink::UnsupportedKind("opensearch".to_string())],
                }],
            },
            Row {
                case: "a sink naming a codec this emulator does not write",
                body: "version: \"2\"\np:\n  source:\n    http:\n      path: \"/ingest\"\n  \
                       sink:\n    - s3:\n        bucket: \"records\"\n        codec:\n          \
                       parquet:\n            schema: \"{}\"\n",
                routes: vec![IngestRoute {
                    sub_pipeline: "p".to_string(),
                    path: "/ingest".to_string(),
                    sinks: vec![PipelineSink::S3(S3Sink {
                        bucket: "records".to_string(),
                        path_prefix: String::new(),
                        region: None,
                        codec: SinkCodec::Unsupported("parquet".to_string()),
                    })],
                }],
            },
            Row {
                case: "an s3 sink naming no bucket names no destination",
                body: "version: \"2\"\np:\n  source:\n    http:\n      path: \"/ingest\"\n  \
                       sink:\n    - s3:\n        object_key:\n          path_prefix: \"logs/\"\n",
                routes: vec![IngestRoute {
                    sub_pipeline: "p".to_string(),
                    path: "/ingest".to_string(),
                    sinks: vec![PipelineSink::NoDestination(Some("s3".to_string()))],
                }],
            },
            Row {
                case: "a sink sequence entry that names no sink kind at all",
                body: "version: \"2\"\np:\n  source:\n    http:\n      path: \"/ingest\"\n  \
                       sink:\n    - \"records\"\n",
                routes: vec![IngestRoute {
                    sub_pipeline: "p".to_string(),
                    path: "/ingest".to_string(),
                    sinks: vec![PipelineSink::NoDestination(None)],
                }],
            },
            Row {
                case: "a sub-pipeline declaring an s3 sink BESIDE a kind this emulator does \
                       not write: both are reported, so neither is answered for silently",
                body: "version: \"2\"\np:\n  source:\n    http:\n      path: \"/ingest\"\n  \
                       sink:\n    - s3:\n        bucket: \"records\"\n    - opensearch:\n        \
                       index: \"logs\"\n",
                routes: vec![IngestRoute {
                    sub_pipeline: "p".to_string(),
                    path: "/ingest".to_string(),
                    sinks: vec![
                        PipelineSink::S3(S3Sink {
                            bucket: "records".to_string(),
                            path_prefix: String::new(),
                            region: None,
                            codec: SinkCodec::Ndjson,
                        }),
                        PipelineSink::UnsupportedKind("opensearch".to_string()),
                    ],
                }],
            },
            Row {
                case: "a sink naming a Region that is not a Region label at all, which \
                       leaves the pipeline's own Region to address the write",
                body: "version: \"2\"\np:\n  source:\n    http:\n      path: \"/ingest\"\n  \
                       sink:\n    - s3:\n        bucket: \"records\"\n        aws:\n          \
                       region: \"records.example.invalid/path\"\n",
                routes: vec![IngestRoute {
                    sub_pipeline: "p".to_string(),
                    path: "/ingest".to_string(),
                    sinks: vec![PipelineSink::S3(S3Sink {
                        bucket: "records".to_string(),
                        path_prefix: String::new(),
                        region: None,
                        codec: SinkCodec::Ndjson,
                    })],
                }],
            },
            Row {
                case: "a body that is not a configuration at all",
                body: "not: [a, pipeline\n",
                routes: Vec::new(),
            },
            Row {
                // Past the nesting bound: measured before the parse, so it declares no
                // route rather than recursing per level.
                case: "a body that nests deeper than this emulator reads",
                body: "version: \"2\"\np:\n  source:\n    http:\n      path: \"/ingest\"\n  \
                       sink:\n    - s3:\n        bucket: \"records\"\n        deep: \
                       [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[\
                       [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[\
                       [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[\n",
                routes: Vec::new(),
            },
            Row {
                case: "a configuration whose only sub-pipeline has no http source",
                body: "version: \"2\"\nfrom-s3:\n  source:\n    s3:\n      bucket: \"in\"\n  \
                       sink:\n    - s3:\n        bucket: \"out\"\n",
                routes: Vec::new(),
            },
        ];
        for Row { case, body, routes } in rows {
            let read = ingest_routes(body);
            assert_eq!(
                read, routes,
                "case {case:?}: expected {routes:?}, got {read:?}"
            );
        }
    }

    /// RULE: a request reaches a source when it addresses that source's path, however the
    /// trailing separator is written; every other path reaches none.
    #[test]
    fn a_request_reaches_the_source_whose_path_it_addresses() {
        let routes = ingest_routes(
            "version: \"2\"\nfirst:\n  source:\n    http:\n      path: \"/ingest\"\n  \
             sink:\n    - s3:\n        bucket: \"records\"\nsecond:\n  source:\n    http:\n      \
             path: \"/log/ingest\"\n  sink:\n    - s3:\n        bucket: \"records\"\n",
        );
        let rows: &[(&str, &str, Option<&str>)] = &[
            ("the first source's path", "/ingest", Some("first")),
            ("the same path with a trailing separator", "/ingest/", Some("first")),
            ("the second source's path", "/log/ingest", Some("second")),
            ("a prefix of a source's path", "/log", None),
            ("a path below a source's path", "/ingest/more", None),
            ("the endpoint's own root", "/", None),
        ];
        for (case, path, expected) in rows {
            let reached = route_at(&routes, path).map(|route| route.sub_pipeline.as_str());
            assert_eq!(
                reached, *expected,
                "case {case:?}: expected {expected:?}, got {reached:?}"
            );
        }
    }
}
