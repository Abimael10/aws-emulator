// SPDX-License-Identifier: CC0-1.0

//! The ASYNCHRONOUS-INVOCATION CONFIGURATION slice of this service's state: the settings
//! `PutFunctionEventInvokeConfig` "Configures ... for asynchronous invocation on a
//! function, version, or alias", the record the five operations on that resource read and
//! write, and whether the function's execution role lets Lambda deliver an invocation
//! record to the destinations a configuration names.
//!
//! It lives in its own module rather than in the already long [`crate::state`], which
//! re-exports every item here, so a handler still names them as `crate::state::…`. The
//! records themselves hang off the function
//! ([`crate::state::FunctionRecord::event_invoke_configs`]), one per QUALIFIER a
//! configuration was put on, because the resource is "a function, version, or alias": a
//! configuration put on an alias is its own and never the one the version that alias
//! invokes holds.
//!
//! What the write does and what a read reports are the same stored thing: a put REPLACES
//! the addressed qualifier's configuration whole ("If a configuration already exists for a
//! function, version, or alias, this operation overwrites it. If you exclude any settings,
//! they are removed"), an update changes only the settings its request carries ("To set one
//! option without affecting existing settings for other options, use
//! UpdateFunctionEventInvokeConfig"), and a qualifier nobody has configured holds nothing
//! at all — which is what makes the reads answer the modelled 404 rather than a
//! configuration nobody asked for.

use std::time::SystemTime;

use crate::ops::types::{
    DestinationConfig, GetFunctionEventInvokeConfigOutput, OnFailure, OnSuccess,
};

use super::event_source_authorization::{function_role_refusal_message, iam_state, withholds_call};

/// How a read of an asynchronous-invocation configuration no qualifier holds is refused:
/// the resource is the configuration rather than the function, so the sentence names the
/// function ARN the request addressed and says the configuration is what is missing.
#[must_use]
pub fn no_event_invoke_config_message(function_arn: &str) -> String {
    format!("The function {function_arn} doesn't have an EventInvokeConfig")
}

/// The `FunctionArn` a configuration is reported on: the function's ARN qualified by the
/// qualifier the configuration belongs to.
///
/// A configuration of the UNPUBLISHED version is reported on the `:$LATEST`-qualified ARN
/// rather than the bare one — AWS's own documented answer to a put that named no qualifier
/// carries `"FunctionArn": "arn:aws:lambda:<region>:<account>:function:<name>:$LATEST"`, and
/// `FunctionArn`'s pattern admits exactly that suffix — because the resource is the
/// configuration OF A QUALIFIER, so the ARN names which one answered.
#[must_use]
pub fn configured_function_arn(function_arn: &str, qualifier: &str) -> String {
    format!("{function_arn}:{qualifier}")
}

/// One asynchronous-invocation configuration, as one qualifier of one function holds it.
///
/// Only the settings a caller actually SET are held: the model gives
/// `MaximumRetryAttempts` and `MaximumEventAgeInSeconds` no default of their own, and the
/// documented behaviour of an event invoked without them ("Lambda retries an asynchronous
/// invocation twice ... It retains events in a queue for up to six hours") is what DELIVERY
/// applies, not a value this resource holds. So a setting nobody stated is absent here and
/// absent from every report of it.
#[derive(Debug, Clone)]
pub struct EventInvokeConfigRecord {
    /// `MaximumRetryAttempts`, when a request set it.
    pub maximum_retry_attempts: Option<i32>,
    /// `MaximumEventAgeInSeconds`, when a request set it.
    pub maximum_event_age_in_seconds: Option<i32>,
    /// The `DestinationConfig$OnSuccess` destination, when a request named one.
    pub on_success: Option<String>,
    /// The `DestinationConfig$OnFailure` destination, when a request named one.
    pub on_failure: Option<String>,
    /// When this configuration was last written, which is what `LastModified` ("The date
    /// and time that the configuration was last updated") reports.
    pub last_modified: SystemTime,
}

impl EventInvokeConfigRecord {
    /// The configuration a PUT of these settings leaves behind: exactly what the request
    /// carried, so a setting it excluded is removed rather than carried over from whatever
    /// the qualifier held before.
    #[must_use]
    pub fn of_request(
        maximum_retry_attempts: Option<i32>,
        maximum_event_age_in_seconds: Option<i32>,
        destination_config: Option<&DestinationConfig>,
        now: SystemTime,
    ) -> Self {
        Self {
            maximum_retry_attempts,
            maximum_event_age_in_seconds,
            on_success: named_destination(destination_config.and_then(|config| {
                config
                    .on_success
                    .as_ref()
                    .and_then(|slot| slot.destination.as_deref())
            })),
            on_failure: named_destination(destination_config.and_then(|config| {
                config
                    .on_failure
                    .as_ref()
                    .and_then(|slot| slot.destination.as_deref())
            })),
            last_modified: now,
        }
    }

    /// Apply an UPDATE of these settings to the configuration this qualifier already
    /// holds: each setting the request carries replaces the held one, and each setting it
    /// does not carry is left exactly as it stands.
    pub fn apply_update(
        &mut self,
        maximum_retry_attempts: Option<i32>,
        maximum_event_age_in_seconds: Option<i32>,
        destination_config: Option<&DestinationConfig>,
        now: SystemTime,
    ) {
        if let Some(attempts) = maximum_retry_attempts {
            self.maximum_retry_attempts = Some(attempts);
        }
        if let Some(age) = maximum_event_age_in_seconds {
            self.maximum_event_age_in_seconds = Some(age);
        }
        // Each destination SLOT is a setting of its own, so the rule this operation applies
        // to every other setting applies to the slots one at a time: the slot a request
        // STATES replaces the held destination, and the slot it states nothing about keeps
        // the destination the configuration holds. A slot stated with no destination — the
        // empty string the member's pattern admits, or no `Destination` member at all — is
        // how a caller takes that destination away (see [`named_destination`]), which is
        // what lets one slot be cleared while the other stands.
        if let Some(config) = destination_config {
            if let Some(slot) = config.on_success.as_ref() {
                self.on_success = named_destination(slot.destination.as_deref());
            }
            if let Some(slot) = config.on_failure.as_ref() {
                self.on_failure = named_destination(slot.destination.as_deref());
            }
        }
        self.last_modified = now;
    }

    /// The `FunctionEventInvokeConfig` the wire reports for this configuration, on the
    /// qualified ARN `function_arn` the request addressed.
    ///
    /// Both destination slots are reported whether or not they carry a destination, which
    /// is how AWS answers a configuration whose on-success slot is unset ("DestinationConfig":
    /// {"OnSuccess": {}, "OnFailure": {"Destination": "..."}}), so a client reads the pair
    /// unconditionally.
    #[must_use]
    pub fn reported(&self, function_arn: String) -> GetFunctionEventInvokeConfigOutput {
        GetFunctionEventInvokeConfigOutput {
            last_modified: Some(::aws_smithy_types::DateTime::from(self.last_modified)),
            function_arn: Some(function_arn),
            maximum_retry_attempts: self.maximum_retry_attempts,
            maximum_event_age_in_seconds: self.maximum_event_age_in_seconds,
            destination_config: Some(DestinationConfig {
                on_success: Some(OnSuccess {
                    destination: self.on_success.clone(),
                }),
                on_failure: Some(OnFailure {
                    destination: self.on_failure.clone(),
                }),
            }),
        }
    }
}

/// The destination ONE slot of a `DestinationConfig` names, or `None` for a slot that
/// names none.
///
/// `DestinationArn`'s own pattern is `^$|kafka://…|arn:…`, so the EMPTY STRING is a value
/// the member admits and the only one that is no resource at all: it is how a caller takes
/// a destination away, and it is held as "no destination" rather than as an ARN of nothing.
pub(crate) fn named_destination(destination: Option<&str>) -> Option<String> {
    destination
        .filter(|destination| !destination.is_empty())
        .map(str::to_string)
}

/// The suffix the NAME of a FIFO resource ends with — the one thing that tells a FIFO
/// queue's or FIFO topic's ARN from a standard one's, because Amazon SQS and Amazon SNS
/// both require it of every FIFO queue's and FIFO topic's name.
const FIFO_RESOURCE_SUFFIX: &str = ".fifo";

/// The ARN vendor fields of the destination services whose resources come in a FIFO variant
/// beside their standard one, as [`service_common::arn::vendor`] reads one.
///
/// Of the destination kinds `FunctionEventInvokeConfig$DestinationConfig` documents, exactly
/// these two are documented as the STANDARD resource of their service — "Queue - The ARN of
/// a standard SQS queue", "Topic - The ARN of a standard SNS topic" — and they are also the
/// only two whose service HAS a FIFO resource at all, so a destination of any other kind has
/// no FIFO variant to be refused for.
const FIFO_CAPABLE_DESTINATION_VENDORS: &[&str] = &["sqs", "sns"];

/// How a member holding nothing is written inside the rendering of a `DestinationConfig`
/// ([`rendered_destination_config`]).
const NULL_RENDERING: &str = "null";

/// Whether `destination` names a FIFO resource of a service that has one: its NAME — the
/// whole resource field of a queue's or a topic's ARN — ending in
/// [`FIFO_RESOURCE_SUFFIX`] is what says so.
#[must_use]
pub fn names_fifo_destination(destination: &str) -> bool {
    ::service_common::arn::parse(destination).is_some_and(|arn| {
        FIFO_CAPABLE_DESTINATION_VENDORS.contains(&arn.vendor)
            && arn.resource.ends_with(FIFO_RESOURCE_SUFFIX)
    })
}

/// The sentence a request whose `DestinationConfig` names a destination an asynchronous
/// invocation's record cannot be delivered to is refused with — and `None` when every
/// destination it names is one that can be, which includes a configuration naming none.
///
/// An asynchronous-invocation destination is a resource Lambda writes ONE record to per
/// event, with no ordering and no deduplication to honour, so the kinds it may be is the
/// STANDARD resource of each service the member enumerates ("Queue - The ARN of a standard
/// SQS queue", "Topic - The ARN of a standard SNS topic"): a FIFO queue or FIFO topic in
/// either slot is a destination no record is ever delivered to, so the pair is refused by
/// the call that CONFIGURES it, before anything is persisted, rather than stored as a
/// delivery path that silently drops every record.
///
/// The sentence renders the `DestinationConfig` the request carried WHOLE
/// ([`rendered_destination_config`]) rather than the one slot at fault, because what is
/// invalid is the configuration asked for: an operator reads both slots back to see which
/// pair was refused.
///
/// A READ of the request: it decides, and changes nothing.
#[must_use]
pub fn invalid_destination_config(
    destination_config: Option<&DestinationConfig>,
) -> Option<String> {
    let config = destination_config?;
    // The destinations the request STATES, in the one reading of a `DestinationConfig` this
    // crate has — which holds the empty string the member's pattern admits as no destination
    // at all, so taking a destination away is not judged as a destination of anything.
    requested_destinations(Some(config))
        .iter()
        .any(|destination| names_fifo_destination(destination))
        .then(|| {
            format!(
                "The provided destination config {} is invalid.",
                rendered_destination_config(config)
            )
        })
}

/// The `DestinationConfig` of a request as the refusal above quotes it: the structure's own
/// name, then each of its two slots in the order the shape declares them, with a slot the
/// request did not state written as holding nothing.
///
/// A slot the request DID state is written as its own structure carrying the one
/// `Destination` member it has, so a slot stated with no destination at all is told apart
/// from a slot that was not stated.
fn rendered_destination_config(config: &DestinationConfig) -> String {
    let on_success = config.on_success.as_ref().map_or_else(
        || NULL_RENDERING.to_string(),
        |slot| rendered_slot("OnSuccess", slot.destination.as_deref()),
    );
    let on_failure = config.on_failure.as_ref().map_or_else(
        || NULL_RENDERING.to_string(),
        |slot| rendered_slot("OnFailure", slot.destination.as_deref()),
    );
    format!("DestinationConfig(onSuccess={on_success}, onFailure={on_failure})")
}

/// ONE slot of a rendered `DestinationConfig`: the slot structure's name and the
/// `Destination` it carries, as the request carried it.
fn rendered_slot(slot: &str, destination: Option<&str>) -> String {
    format!(
        "{slot}(destination={})",
        destination.unwrap_or(NULL_RENDERING)
    )
}

/// Every destination a request's `DestinationConfig` names, on-success first.
#[must_use]
pub fn requested_destinations(destination_config: Option<&DestinationConfig>) -> Vec<String> {
    let Some(config) = destination_config else {
        return Vec::new();
    };
    [
        config
            .on_success
            .as_ref()
            .and_then(|slot| slot.destination.as_deref()),
        config
            .on_failure
            .as_ref()
            .and_then(|slot| slot.destination.as_deref()),
    ]
    .into_iter()
    .filter_map(named_destination)
    .collect()
}

/// What delivering an invocation record to ONE KIND of destination takes, and how a role
/// that may not do it is refused.
struct DestinationWrite {
    /// The ARN vendor field of the destinations this entry is about, as
    /// [`service_common::arn::vendor`] reads one.
    vendor: &'static str,
    /// The IAM action namespace that call is named in, which is the destination service's
    /// own.
    namespace: &'static str,
    /// The call Lambda makes ON the named destination to deliver the record there.
    action: &'static str,
    /// How the refusal names the service that call is made to.
    service: &'static str,
}

/// What a write to each KIND of destination takes.
///
/// One entry per destination kind `FunctionEventInvokeConfig$DestinationConfig` documents
/// — "Function - The Amazon Resource Name (ARN) of a Lambda function. Queue - The ARN of a
/// standard SQS queue. Bucket - The ARN of an Amazon S3 bucket. Topic - The ARN of a
/// standard SNS topic. Event Bus - The ARN of an Amazon EventBridge event bus." — whose
/// write is a plain resource-level call on the destination the request named, with the IAM
/// action being the destination service's action namespace and the name of that call.
///
/// An Amazon S3 BUCKET is deliberately not here: Lambda writes the record as an object
/// INSIDE the bucket, and `s3:PutObject` is grantable only on object ARNs, so holding a
/// role to that call on the bucket ARN the request names would refuse a correctly scoped
/// least-privilege policy. A destination kind with no entry is evidence of nothing and
/// refuses no configuration; it is left to the change that implements delivery to one.
const DESTINATION_WRITES: &[DestinationWrite] = &[
    DestinationWrite {
        vendor: "sqs",
        namespace: "sqs",
        action: "SendMessage",
        service: "SQS",
    },
    DestinationWrite {
        vendor: "sns",
        namespace: "sns",
        action: "Publish",
        service: "SNS",
    },
    DestinationWrite {
        vendor: "events",
        namespace: "events",
        action: "PutEvents",
        service: "EventBridge",
    },
    DestinationWrite {
        vendor: "lambda",
        namespace: "lambda",
        action: "InvokeFunction",
        service: "Lambda",
    },
];

/// What writing to the destination `destination` takes, or `None` for a destination kind
/// this module states no write for (see [`DESTINATION_WRITES`]).
fn write_of(destination: &str) -> Option<&'static DestinationWrite> {
    let vendor = service_common::arn::vendor(destination)?;
    DESTINATION_WRITES
        .iter()
        .find(|entry| entry.vendor == vendor)
}

/// The sentence Lambda refuses a configuration naming one of `destinations` with, because
/// the policies in force on the function's execution role `role_arn` withhold the write
/// Lambda would make to deliver a record there — or `None` when the role may make every one
/// of those writes, or when nothing of the kind can be shown.
///
/// Lambda delivers an invocation record ACTING AS the function's execution role, so a
/// configuration whose role may not write the destination is a destination no record could
/// ever reach: the question is settled by the call that CONFIGURES the pair, before anything
/// is persisted, exactly as it is settled for the event source a mapping reads
/// ([`super::event_source_authorization`]) — and on the same evidence, so an engine carrying
/// no IAM store, a role ARN that store holds no role for, and a role carrying a grant this
/// emulator cannot read all refuse nothing.
///
/// The sentence names the DESTINATION the request asked for, by the ARN the request carried:
/// "The function execution role does not have permissions to call SendMessage on
/// arn:aws:sqs:…". Only the call and the destination vary with the request; the rest is fixed
/// text ([`function_role_refusal_message`]). A configuration may name a destination in each
/// of its two slots, and an operator reads this sentence to learn WHICH of them was refused,
/// so the value that identifies it — its ARN — is what the sentence carries rather than the
/// name of the service it belongs to.
///
/// A READ of both stores: it decides, and changes nothing.
#[must_use]
pub fn unwritable_destination<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arn: &str,
    destinations: &[String],
) -> Option<String> {
    let iam = iam_state(engine)?;
    let held = iam.lock();
    destinations.iter().find_map(|destination| {
        withheld_destination_write(&held, role_arn, destination)
            .map(|(action, _service)| function_role_refusal_message(action, destination))
    })
}

/// The write that delivers a record to `destination` which the policies in force on `role_arn`
/// WITHHOLD — as the call it is and the service it would have been made to — and `None` when
/// that role may make the write, or when nothing of the kind can be shown.
///
/// The ONE reading of this question in this crate, because it is one rule wherever it is
/// asked: Lambda delivers to a destination ACTING AS the invoked function's execution role, so
/// a destination whose write that role may not make is a destination no record reaches. Three
/// places ask it — the call that configures an asynchronous invocation's destination
/// ([`unwritable_destination`]), the call that configures an event source mapping's
/// ([`super::event_source_mapping_destination`]) and the poller that would write a discarded
/// batch's record ([`super::event_source_poller::failure_destination`]) — and each WORDS the
/// refusal its own error class is worded in, which is why the answer is the call rather than a
/// sentence. A destination kind [`DESTINATION_WRITES`] states no write for is evidence of
/// nothing and withholds nothing.
#[must_use]
pub(crate) fn withheld_destination_write(
    iam: &super::event_source_authorization::IamState,
    role_arn: &str,
    destination: &str,
) -> Option<(&'static str, &'static str)> {
    let entry = write_of(destination)?;
    let action = format!("{}:{}", entry.namespace, entry.action);
    withholds_call(iam, role_arn, &action, destination)
        .filter(|withheld| *withheld)
        .map(|_| (entry.action, entry.service))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The empty string is the one `DestinationArn` value that is no resource, so it is
    /// held as "no destination"; every other value the member admits is held as it stands.
    #[test]
    fn the_empty_destination_arn_names_no_destination() {
        // (case, the slot's value, what is held for it)
        let rows: [(&str, Option<&str>, Option<&str>); 4] = [
            ("no slot at all", None, None),
            ("the empty string the pattern admits", Some(""), None),
            (
                "a queue",
                Some("arn:aws:sqs:us-east-1:000000000000:records"),
                Some("arn:aws:sqs:us-east-1:000000000000:records"),
            ),
            (
                "an event bus",
                Some("arn:aws:events:us-east-1:000000000000:event-bus/records"),
                Some("arn:aws:events:us-east-1:000000000000:event-bus/records"),
            ),
        ];
        for (case, value, expected) in rows {
            let held = named_destination(value);
            assert_eq!(
                held.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {held:?}"
            );
        }
    }

    /// A destination is a FIFO one exactly when it names a resource of a service that HAS
    /// a FIFO variant and its name carries the FIFO suffix; every other value — a standard
    /// resource of those services, a resource of a service with no FIFO variant, a name
    /// carrying the suffix somewhere other than its end, a value that is no ARN — is not.
    #[test]
    fn a_fifo_destination_is_a_queue_or_topic_whose_name_carries_the_suffix() {
        // (case, the destination, whether it names a FIFO resource)
        let rows: [(&str, &str, bool); 10] = [
            (
                "a FIFO queue",
                "arn:aws:sqs:us-east-1:000000000000:records.fifo",
                true,
            ),
            (
                "a FIFO topic",
                "arn:aws:sns:eu-west-1:000000000000:records.fifo",
                true,
            ),
            (
                "a FIFO queue of a partition of its own",
                "arn:aws-us-gov:sqs:us-gov-west-1:000000000000:records.fifo",
                true,
            ),
            (
                "a standard queue",
                "arn:aws:sqs:us-east-1:000000000000:records",
                false,
            ),
            (
                "a standard topic",
                "arn:aws:sns:us-east-1:000000000000:records",
                false,
            ),
            (
                "a queue whose name carries the suffix in the middle",
                "arn:aws:sqs:us-east-1:000000000000:records.fifo.audit",
                false,
            ),
            (
                "an event bus, of a service with no FIFO resource",
                "arn:aws:events:us-east-1:000000000000:event-bus/records.fifo",
                false,
            ),
            (
                "a Lambda function, of a service with no FIFO resource",
                "arn:aws:lambda:us-east-1:000000000000:function:records",
                false,
            ),
            ("a bucket, of a service with no FIFO resource", "arn:aws:s3:::records.fifo", false),
            ("a name that is no ARN at all", "records.fifo", false),
        ];
        for (case, destination, expected) in rows {
            let decided = names_fifo_destination(destination);
            assert_eq!(
                decided, expected,
                "case {case:?}: expected names_fifo_destination({destination:?}) to be \
                 {expected:?}, got {decided:?}"
            );
        }
    }

    /// The refusal renders the configuration the request carried WHOLE: both slots, in the
    /// order the shape declares them, a slot the request did not state as holding nothing,
    /// and a stated slot as its own structure carrying the destination it carried.
    #[test]
    fn the_refusal_renders_the_requested_destination_config() {
        const FIFO: &str = "arn:aws:sqs:us-east-1:000000000000:records.fifo";
        const QUEUE: &str = "arn:aws:sqs:us-east-1:000000000000:records";
        const FIFO_TOPIC: &str = "arn:aws:sns:us-east-1:000000000000:records.fifo";

        // (case, the configuration, the rendering the refusal carries, or none for an
        // accepted configuration)
        let rows: [(&str, Option<DestinationConfig>, Option<&str>); 8] = [
            ("no configuration at all", None, None),
            (
                "a configuration stating neither slot",
                Some(DestinationConfig {
                    on_success: None,
                    on_failure: None,
                }),
                None,
            ),
            (
                "a standard queue on failure",
                Some(config(None, Some(Some(QUEUE)))),
                None,
            ),
            (
                "a FIFO queue on failure, with no on-success slot",
                Some(config(None, Some(Some(FIFO)))),
                Some(
                    "DestinationConfig(onSuccess=null, \
                     onFailure=OnFailure(destination=arn:aws:sqs:us-east-1:000000000000:records.fifo))",
                ),
            ),
            (
                "a FIFO topic on success, beside a standard queue on failure",
                Some(config(Some(Some(FIFO_TOPIC)), Some(Some(QUEUE)))),
                Some(
                    "DestinationConfig(onSuccess=OnSuccess(destination=arn:aws:sns:us-east-1:000000000000:records.fifo), \
                     onFailure=OnFailure(destination=arn:aws:sqs:us-east-1:000000000000:records))",
                ),
            ),
            (
                "a FIFO queue on failure, beside an on-success slot stating no destination",
                Some(config(Some(None), Some(Some(FIFO)))),
                Some(
                    "DestinationConfig(onSuccess=OnSuccess(destination=null), \
                     onFailure=OnFailure(destination=arn:aws:sqs:us-east-1:000000000000:records.fifo))",
                ),
            ),
            (
                "a FIFO queue on failure, beside an on-success slot cleared with the empty \
                 destination",
                Some(config(Some(Some("")), Some(Some(FIFO)))),
                Some(
                    "DestinationConfig(onSuccess=OnSuccess(destination=), \
                     onFailure=OnFailure(destination=arn:aws:sqs:us-east-1:000000000000:records.fifo))",
                ),
            ),
            (
                "a FIFO queue in both slots",
                Some(config(Some(Some(FIFO)), Some(Some(FIFO)))),
                Some(
                    "DestinationConfig(onSuccess=OnSuccess(destination=arn:aws:sqs:us-east-1:000000000000:records.fifo), \
                     onFailure=OnFailure(destination=arn:aws:sqs:us-east-1:000000000000:records.fifo))",
                ),
            ),
        ];

        for (case, configuration, rendering) in rows {
            let message = invalid_destination_config(configuration.as_ref());
            let expected = rendering
                .map(|rendering| format!("The provided destination config {rendering} is invalid."));
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }

    /// A `DestinationConfig` stating each slot as given: `None` for a slot the request does
    /// not state at all, `Some(None)` for a slot stated with no `Destination` member.
    fn config(
        on_success: Option<Option<&str>>,
        on_failure: Option<Option<&str>>,
    ) -> DestinationConfig {
        DestinationConfig {
            on_success: on_success.map(|destination| OnSuccess {
                destination: destination.map(str::to_string),
            }),
            on_failure: on_failure.map(|destination| OnFailure {
                destination: destination.map(str::to_string),
            }),
        }
    }

    /// Each entry is read under the vendor field of the ARNs it is about, and no two
    /// entries claim the same vendor: one kind of destination takes one write.
    #[test]
    fn each_kind_of_destination_states_one_write() {
        for entry in DESTINATION_WRITES {
            let same = DESTINATION_WRITES
                .iter()
                .filter(|other| other.vendor == entry.vendor)
                .count();
            assert_eq!(
                same, 1,
                "case {:?}: expected one entry for the vendor, got {same}",
                entry.vendor
            );
            assert!(
                !entry.action.is_empty() && !entry.service.is_empty(),
                "case {:?}: an entry naming no call refuses nothing legibly",
                entry.vendor
            );
        }
    }

    /// A destination the catalog states a write for is decided on that write; one it
    /// states none for is evidence of nothing.
    #[test]
    fn the_write_a_destination_takes_is_the_one_its_service_names() {
        // (the destination, the IAM action its write is named by)
        let rows: [(&str, Option<&str>); 5] = [
            (
                "arn:aws:sqs:us-east-1:000000000000:records",
                Some("sqs:SendMessage"),
            ),
            (
                "arn:aws:sns:us-east-1:000000000000:records",
                Some("sns:Publish"),
            ),
            (
                "arn:aws:events:us-east-1:000000000000:event-bus/records",
                Some("events:PutEvents"),
            ),
            (
                "arn:aws:lambda:us-east-1:000000000000:function:records",
                Some("lambda:InvokeFunction"),
            ),
            // A bucket's write is an object-level call, so no write is stated for it.
            ("arn:aws:s3:::records", None),
        ];
        for (destination, expected) in rows {
            let stated = write_of(destination)
                .map(|entry| format!("{}:{}", entry.namespace, entry.action));
            assert_eq!(
                stated.as_deref(),
                expected,
                "case {destination:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }
}
