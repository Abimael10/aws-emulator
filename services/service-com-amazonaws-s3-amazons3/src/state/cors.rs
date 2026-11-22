// SPDX-License-Identifier: CC0-1.0

//! A bucket's CORS configuration: the rules it holds, the published limits each rule is
//! judged against, and the refusal a configuration that breaks one of them goes out with.
//!
//! `PutBucketCors` writes the configuration, `GetBucketCors` reports it back and the
//! preflight in `service/cors_preflight.rs` matches a browser request against it, so the
//! stored shape ([`BucketCorsRule`]) and the validation every write passes through
//! ([`validate_cors_configuration`]) live here, in one place, rather than beside any one of
//! them.

use super::*;

/// How many `CORSRule`s one bucket's cors configuration may carry.
///
/// The published limit, from `CORSConfiguration$CORSRules`'s own documentation: "A set of
/// origins and methods (cross-origin access that you want to allow). You can add up to 100
/// rules to the configuration." The model bounds the list at neither end
/// (`smithy_query(query="shape",shape="CORSRules")`), so this check is what enforces it —
/// and it also bounds what one bucket's record parks in this process's memory. A put past
/// it is refused rather than recorded.
pub const MAX_CORS_RULES: usize = 100;

/// The `<Message>` S3 sends for a cors configuration carrying more than [`MAX_CORS_RULES`]
/// rules.
///
/// Fixed text stating the PUBLISHED limit the configuration was judged against — the rule
/// count the document carried is no part of it, and neither is the member path the rules
/// arrived on.
pub fn too_many_cors_rules_message() -> String {
    format!("The number of CORS rules should not exceed allowed limit of {MAX_CORS_RULES} rules.")
}

/// The longest `CORSRule$ID` a stored rule may carry, in characters.
///
/// That member's own documentation: "Unique identifier for the rule. The value cannot be
/// longer than 255 characters." `ID` is a plain string with no `@length` in the model
/// (`smithy_query(query="shape",shape="ID")`), so nothing refuses a longer id before this
/// check does.
pub const MAX_CORS_RULE_ID_CHARS: usize = 255;

/// The HTTP methods a `CORSRule$AllowedMethods` entry may name.
///
/// That member's own documentation: "An HTTP method that you allow the origin to execute.
/// Valid values are `GET`, `PUT`, `HEAD`, `POST`, and `DELETE`." `AllowedMethod` is a plain
/// string in the model (`smithy_query(query="shape",shape="AllowedMethods")`), not an enum,
/// so this catalog is what refuses a method S3 does not support — including one that names
/// a supported method in another case, since the published values are upper case.
pub const CORS_ALLOWED_METHODS: [&str; 5] = ["GET", "PUT", "HEAD", "POST", "DELETE"];

/// The character a cors pattern may carry to stand for a span of values: `*` in an
/// `AllowedOrigin` (`https://*.example.com`) or an `AllowedHeader` (`x-amz-*`). It is also
/// the entry that stands for every value, which is what the preflight matcher reads it as
/// (`service/cors_preflight.rs`).
pub const CORS_WILDCARD: &str = "*";

/// How many [`CORS_WILDCARD`]s one `AllowedOrigin` or `AllowedHeader` entry may carry.
///
/// A cors pattern is matched by comparing the text before the wildcard and the text after it
/// against the request's origin or header name, so ONE wildcard is all a pattern can have a
/// meaning with: a second one names no further span a match could be made against. An entry
/// carrying more is refused rather than stored
/// ([`CorsConfigurationRejection::SeveralWildcardsInOrigin`]).
pub const MAX_CORS_PATTERN_WILDCARDS: usize = 1;

/// How many [`CORS_WILDCARD`]s one `ExposeHeader` entry may carry: none.
///
/// `ExposeHeaders` names "one or more headers in the response that you want customers to be
/// able to access from their applications" (`CORSRule$ExposeHeaders`) — the response headers
/// a browser is told it may READ, which the preflight answer has to spell out one by one, so
/// a pattern standing for a span of them is not something S3 serves.
pub const MAX_CORS_EXPOSE_HEADER_WILDCARDS: usize = 0;

/// How many [`CORS_WILDCARD`]s `value` carries.
fn wildcards(value: &str) -> usize {
    value.matches(CORS_WILDCARD).count()
}

/// The most bytes a bucket's cors configuration document may carry.
///
/// `PutBucketCors`'s own documentation: "The cors subresource is an XML document in which
/// you configure rules that identify origins and the HTTP methods that can be executed on
/// your bucket. The document is limited to 64 KB in size." Measured over the stored rules by
/// [`cors_configuration_bytes`], which is also what keeps one bucket's record from parking
/// an arbitrarily large configuration in this process's memory.
pub const MAX_CORS_CONFIGURATION_BYTES: usize = 64 * 1024;

/// One `CORSRule` of a bucket's cors configuration: "Specifies a cross-origin access rule
/// for an Amazon S3 bucket."
///
/// Every member is optional exactly where the model makes it so
/// (`smithy_query(query="shape",shape="CORSRule")`): `AllowedMethods` and `AllowedOrigins`
/// are REQUIRED, the other three are not. The two optional lists are plain `Vec`s rather
/// than `Option`s for the reason [`BucketWebsite::routing_rules`] is: both are
/// `xmlFlattened`, so the XML boundary parses a document naming no `AllowedHeader` to the
/// same absent list as one naming none at all, and there is nothing for a `None` to mean
/// that an empty list does not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BucketCorsRule {
    /// `CORSRule$ID`, or `None` when the rule names none.
    pub id: Option<String>,
    /// `CORSRule$AllowedHeaders`: "Headers that are specified in the
    /// `Access-Control-Request-Headers` header", in the order the document named them.
    pub allowed_headers: Vec<String>,
    /// `CORSRule$AllowedMethods`, a REQUIRED member: each one of
    /// [`CORS_ALLOWED_METHODS`], in the order the document named them.
    pub allowed_methods: Vec<String>,
    /// `CORSRule$AllowedOrigins`, a REQUIRED member: "One or more origins you want
    /// customers to be able to access the bucket from", in the order the document named
    /// them.
    pub allowed_origins: Vec<String>,
    /// `CORSRule$ExposeHeaders`: "One or more headers in the response that you want
    /// customers to be able to access from their applications", in document order.
    pub expose_headers: Vec<String>,
    /// `CORSRule$MaxAgeSeconds`: "The time in seconds that your browser is to cache the
    /// preflight response for the specified resource", or `None` when the rule names none.
    pub max_age_seconds: Option<i32>,
}

/// The size of the smallest `CORSConfiguration` document that carries `rules`, in bytes.
///
/// A LOWER bound on the document the request carried — the markup of each element plus the
/// text inside it, with no attributes, namespace or whitespace — so a configuration this
/// reports more than [`MAX_CORS_CONFIGURATION_BYTES`] for could not have arrived in a
/// document within the published 64 KB. Element names are the `xmlName`s the model puts on
/// the members (`smithy_query(query="shape",shape="CORSRule")`), which is also how
/// `GetBucketCors` spells them on the way out.
pub fn cors_configuration_bytes(rules: &[BucketCorsRule]) -> usize {
    /// The bytes `<name>text</name>` takes.
    fn element(name: &str, text: &str) -> usize {
        name.len() * 2 + "<></>".len() + text.len()
    }
    /// The bytes every entry of a flattened list named `name` takes.
    fn elements(name: &str, texts: &[String]) -> usize {
        texts.iter().map(|text| element(name, text)).sum()
    }

    let mut bytes = element("CORSConfiguration", "");
    for rule in rules {
        bytes += element("CORSRule", "");
        if let Some(id) = &rule.id {
            bytes += element("ID", id);
        }
        bytes += elements("AllowedHeader", &rule.allowed_headers);
        bytes += elements("AllowedMethod", &rule.allowed_methods);
        bytes += elements("AllowedOrigin", &rule.allowed_origins);
        bytes += elements("ExposeHeader", &rule.expose_headers);
        if let Some(max_age_seconds) = rule.max_age_seconds {
            bytes += element("MaxAgeSeconds", &max_age_seconds.to_string());
        }
    }
    bytes
}

/// Why a cors configuration is not one S3 would apply to a bucket.
///
/// Each variant names the member that is wrong and the published constraint it breaks, so
/// the refusal's message states the rule the caller has to satisfy rather than one request's
/// wording. [`Self::code`] is the wire error code each one goes out with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorsConfigurationRejection {
    /// A `CORSConfiguration` naming no `CORSRule` at all: `CORSConfiguration$CORSRules` is
    /// a REQUIRED member, so a document naming none does not validate against the schema
    /// S3 publishes.
    NoRules,
    /// More rules than [`MAX_CORS_RULES`].
    TooManyRules {
        /// How many the document carried.
        count: usize,
    },
    /// A rule naming no `AllowedMethod`: `CORSRule$AllowedMethods` is a REQUIRED member.
    NoAllowedMethods {
        /// Which rule carries it, counted from 1 as the document orders them.
        rule: usize,
    },
    /// A rule naming no `AllowedOrigin`: `CORSRule$AllowedOrigins` is a REQUIRED member.
    NoAllowedOrigins {
        /// Which rule carries it, counted from 1.
        rule: usize,
    },
    /// An `AllowedMethod` outside [`CORS_ALLOWED_METHODS`].
    UnsupportedMethod {
        /// Which rule carries it, counted from 1.
        rule: usize,
        /// The value that is not a published method.
        method: String,
    },
    /// An `AllowedOrigin` carrying more than [`MAX_CORS_PATTERN_WILDCARDS`] wildcard.
    SeveralWildcardsInOrigin {
        /// Which rule carries it, counted from 1.
        rule: usize,
        /// The entry that carries them, as the request spelled it.
        origin: String,
    },
    /// An `AllowedHeader` carrying more than [`MAX_CORS_PATTERN_WILDCARDS`] wildcard.
    SeveralWildcardsInHeader {
        /// Which rule carries it, counted from 1.
        rule: usize,
        /// The entry that carries them, as the request spelled it.
        header: String,
    },
    /// An `ExposeHeader` carrying a wildcard at all — see
    /// [`MAX_CORS_EXPOSE_HEADER_WILDCARDS`].
    WildcardInExposeHeader {
        /// Which rule carries it, counted from 1.
        rule: usize,
        /// The entry that carries one, as the request spelled it.
        header: String,
    },
    /// An `ID` longer than [`MAX_CORS_RULE_ID_CHARS`] characters.
    IdTooLong {
        /// Which rule carries it, counted from 1.
        rule: usize,
        /// How many characters it carried.
        length: usize,
    },
    /// A `MaxAgeSeconds` below zero, which is no cache time: the member is "the time in
    /// seconds that your browser is to cache the preflight response".
    NegativeMaxAgeSeconds {
        /// Which rule carries it, counted from 1.
        rule: usize,
        /// The value that is not a time in seconds.
        seconds: i32,
    },
    /// A configuration whose document could not fit in
    /// [`MAX_CORS_CONFIGURATION_BYTES`]. See [`cors_configuration_bytes`].
    DocumentTooLarge {
        /// The size of the smallest document carrying these rules.
        bytes: usize,
    },
}

impl CorsConfigurationRejection {
    /// The wire error code S3 answers this refusal with.
    ///
    /// Two codes from S3's published "Error responses" list, both HTTP 400: a document that
    /// does not satisfy the schema the API publishes is [`MALFORMED_XML_CODE`] ("The XML
    /// provided does not match the schema"), and a document that satisfies the schema but
    /// carries a value or a count the API does not accept — an HTTP method outside the
    /// published set above all — is [`INVALID_REQUEST_CODE`].
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoRules | Self::NoAllowedMethods { .. } | Self::NoAllowedOrigins { .. } => {
                MALFORMED_XML_CODE
            }
            Self::TooManyRules { .. }
            | Self::UnsupportedMethod { .. }
            | Self::SeveralWildcardsInOrigin { .. }
            | Self::SeveralWildcardsInHeader { .. }
            | Self::WildcardInExposeHeader { .. }
            | Self::IdTooLong { .. }
            | Self::NegativeMaxAgeSeconds { .. }
            | Self::DocumentTooLarge { .. } => INVALID_REQUEST_CODE,
        }
    }

    /// The `<Message>` this refusal goes out with.
    ///
    /// A refusal S3 answers [`MALFORMED_XML_CODE`] with carries that code's own FIXED
    /// published text ([`MALFORMED_XML_MESSAGE`]) rather than anything about this
    /// configuration — the code says the document did not validate against the published
    /// schema, and the schema is the same for every document — the way
    /// [`LifecycleConfigurationRejection::message`] answers the same code. Every other
    /// refusal states the published limit the configuration broke, in the sentence S3 states
    /// it in, which is what [`Display`](std::fmt::Display) writes.
    pub fn message(&self) -> String {
        if self.code() == MALFORMED_XML_CODE {
            return MALFORMED_XML_MESSAGE.to_string();
        }
        self.to_string()
    }
}

/// The `<Message>` S3 refuses an `AllowedMethod` outside [`CORS_ALLOWED_METHODS`] with.
///
/// Fixed text apart from the method the rule named, which is echoed as the request spelled
/// it: the sentence states that the document carries a method cors does not support and then
/// which value that was, and nothing of which rule of the document carried it.
fn unsupported_cors_method_message(method: &str) -> String {
    format!("Found unsupported HTTP method in CORS config. Unsupported method is {method}")
}

/// The `<Message>` S3 refuses a cors pattern carrying more than
/// [`MAX_CORS_PATTERN_WILDCARDS`] wildcard with.
///
/// Fixed text around the MEMBER the entry arrived on (`AllowedOrigin` or `AllowedHeader` —
/// the `xmlName`s the model puts on those lists) and the entry itself, quoted as the request
/// spelled it.
fn several_wildcards_message(member: &str, value: &str) -> String {
    format!("{member} \"{value}\" can not have more than one wildcard.")
}

/// The member an `AllowedOrigins` entry arrives on, as the document spells it
/// (`CORSRule$AllowedOrigins` carries `xmlName` `AllowedOrigin`), which is how a refusal
/// about one names it.
const ALLOWED_ORIGIN_MEMBER: &str = "AllowedOrigin";
/// The same for `CORSRule$AllowedHeaders` (`xmlName` `AllowedHeader`).
const ALLOWED_HEADER_MEMBER: &str = "AllowedHeader";
/// The same for `CORSRule$ExposeHeaders` (`xmlName` `ExposeHeader`).
const EXPOSE_HEADER_MEMBER: &str = "ExposeHeader";

impl std::fmt::Display for CorsConfigurationRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The three refusals S3 answers `MalformedXML` carry that code's own published
            // sentence on the wire ([`Self::message`]); what they write here is the schema
            // rule the document broke, for a caller reading this as a Rust error.
            Self::NoRules => write!(
                f,
                "CORSConfiguration.CORSRules names no CORSRule; a cors configuration \
                 carries at least one rule"
            ),
            Self::TooManyRules { .. } => write!(f, "{}", too_many_cors_rules_message()),
            Self::NoAllowedMethods { rule } => write!(
                f,
                "CORSRule {rule}: AllowedMethods names no AllowedMethod; a cors rule \
                 allows at least one method"
            ),
            Self::NoAllowedOrigins { rule } => write!(
                f,
                "CORSRule {rule}: AllowedOrigins names no AllowedOrigin; a cors rule \
                 allows at least one origin"
            ),
            Self::UnsupportedMethod { method, .. } => {
                write!(f, "{}", unsupported_cors_method_message(method))
            }
            Self::SeveralWildcardsInOrigin { origin, .. } => write!(
                f,
                "{}",
                several_wildcards_message(ALLOWED_ORIGIN_MEMBER, origin)
            ),
            Self::SeveralWildcardsInHeader { header, .. } => write!(
                f,
                "{}",
                several_wildcards_message(ALLOWED_HEADER_MEMBER, header)
            ),
            Self::WildcardInExposeHeader { header, .. } => write!(
                f,
                "{EXPOSE_HEADER_MEMBER} \"{header}\" contains wildcard. We currently do not \
                 support wildcard for {EXPOSE_HEADER_MEMBER}."
            ),
            Self::IdTooLong { .. } => write!(
                f,
                "CORS rule ID exceeds the maximum length of {MAX_CORS_RULE_ID_CHARS} \
                 characters."
            ),
            Self::NegativeMaxAgeSeconds { .. } => {
                write!(f, "The MaxAgeSeconds of a CORS rule should not be negative.")
            }
            Self::DocumentTooLarge { .. } => write!(
                f,
                "The CORS configuration should not exceed allowed limit of \
                 {MAX_CORS_CONFIGURATION_BYTES} bytes."
            ),
        }
    }
}

impl std::error::Error for CorsConfigurationRejection {}

/// Whether `rules` may be stored as a bucket's cors configuration, reporting the first
/// published rule they break.
///
/// Checked BEFORE anything is written, so a refused put leaves the bucket's current cors
/// configuration exactly as it was — the order every other bucket subresource put keeps.
/// The counts are judged before the per-rule values so an oversized document is refused for
/// what it is rather than for whichever rule happens to come first.
///
/// Two of the rules here (both required lists being non-empty) are ALSO what the modelled
/// `@required` traits express, and the boundary validator refuses a payload missing either
/// member before any handler runs. They are still checked here because this function, not
/// the wire, is what guards the STORE: a configuration no reader could report back is never
/// recorded, whoever writes it.
pub fn validate_cors_configuration(
    rules: &[BucketCorsRule],
) -> Result<(), CorsConfigurationRejection> {
    if rules.is_empty() {
        return Err(CorsConfigurationRejection::NoRules);
    }
    if rules.len() > MAX_CORS_RULES {
        return Err(CorsConfigurationRejection::TooManyRules { count: rules.len() });
    }
    let bytes = cors_configuration_bytes(rules);
    if bytes > MAX_CORS_CONFIGURATION_BYTES {
        return Err(CorsConfigurationRejection::DocumentTooLarge { bytes });
    }

    for (index, rule) in rules.iter().enumerate() {
        // Counted the way the document orders the rules, so the message names the rule a
        // caller can find.
        let position = index + 1;
        if rule.allowed_methods.is_empty() {
            return Err(CorsConfigurationRejection::NoAllowedMethods { rule: position });
        }
        if rule.allowed_origins.is_empty() {
            return Err(CorsConfigurationRejection::NoAllowedOrigins { rule: position });
        }
        for method in &rule.allowed_methods {
            if !CORS_ALLOWED_METHODS.contains(&method.as_str()) {
                return Err(CorsConfigurationRejection::UnsupportedMethod {
                    rule: position,
                    method: method.clone(),
                });
            }
        }
        // The wildcard budget of each list, judged entry by entry: a pattern no request
        // could be matched against is refused rather than stored, so a bucket never carries
        // a cors configuration whose preflight answer is undefined.
        for origin in &rule.allowed_origins {
            if wildcards(origin) > MAX_CORS_PATTERN_WILDCARDS {
                return Err(CorsConfigurationRejection::SeveralWildcardsInOrigin {
                    rule: position,
                    origin: origin.clone(),
                });
            }
        }
        for header in &rule.allowed_headers {
            if wildcards(header) > MAX_CORS_PATTERN_WILDCARDS {
                return Err(CorsConfigurationRejection::SeveralWildcardsInHeader {
                    rule: position,
                    header: header.clone(),
                });
            }
        }
        for header in &rule.expose_headers {
            if wildcards(header) > MAX_CORS_EXPOSE_HEADER_WILDCARDS {
                return Err(CorsConfigurationRejection::WildcardInExposeHeader {
                    rule: position,
                    header: header.clone(),
                });
            }
        }
        if let Some(id) = rule.id.as_deref() {
            let length = id.chars().count();
            if length > MAX_CORS_RULE_ID_CHARS {
                return Err(CorsConfigurationRejection::IdTooLong {
                    rule: position,
                    length,
                });
            }
        }
        if let Some(seconds) = rule.max_age_seconds {
            if seconds < 0 {
                return Err(CorsConfigurationRejection::NegativeMaxAgeSeconds {
                    rule: position,
                    seconds,
                });
            }
        }
    }

    Ok(())
}
