// SPDX-License-Identifier: CC0-1.0

//! Which ARN each component operation TAKES, and the one sentence this service refuses a
//! value that is not one with.
//!
//! The model gives the component operations two ARN shapes — `ComponentVersionARN`
//! (`…:components:<name>:versions:<version>`) and `ComponentARN`
//! (`…:components:<name>`) — and declares each as a `@pattern` on the operation's `arn`
//! member. IoT Greengrass does NOT answer a value outside that pattern with the generic
//! constraint envelope: it states its own sentence, naming the kind of ARN it expected and
//! echoing the value it was handed. So the value is judged HERE, by the operation that
//! takes it (`skip_validation` on each of those inputs in `src/ops/types/`), and the
//! sentence is stated once for every one of them rather than per handler.
//!
//! Judging it here is also what lets `ListComponentVersions` take the ARN its own sibling
//! MINTS: the versions of a component are listed for either the component ARN
//! `ListComponents` reports or the component-VERSION ARN `CreateComponentVersion` returns,
//! so the natural client flow — publish a version, keep the ARN, list that component's
//! versions with it — is served rather than refused ([`addressed_component`]).
//!
//! The syntax each reading holds a value to is the one its member's `@pattern` states, so
//! taking the check off the boundary changes WHICH sentence a bad value is refused with
//! and nothing about which values are refused: the partition and the region are any
//! colon-free text, the account field is `aws` or an account id's digits
//! ([`super::arn_account_field`]), and the name and the version are colon-free and
//! non-empty.

use super::*;
use crate::ops::types::ValidationException;

/// How AWS names a component-VERSION ARN in the sentence below.
const COMPONENT_VERSION_ARN_NOUN: &str = "Component version ARN";

/// How it names a component ARN there — a different noun phrase, so a client can tell
/// which of the two shapes the operation it called takes.
const COMPONENT_ARN_NOUN: &str = "Component ARN";

/// The sentence IoT Greengrass refuses an `arn` outside the shape an operation takes with:
/// fixed text around the one value it echoes, which is the `arn` the request carried.
///
/// That value is echoed ONCE, so the refusal is the size of the request rather than a
/// multiple of it — and the `arn` of every operation here is a URI path label, whose length
/// the front door has already bounded by the request line it accepted.
fn invalid_arn_message(noun: &str, arn: &str) -> String {
    format!("Invalid Input: {noun} {arn} is invalid")
}

/// [`invalid_arn_message`] for an operation that takes a component-VERSION ARN.
pub fn invalid_component_version_arn_message(arn: &str) -> String {
    invalid_arn_message(COMPONENT_VERSION_ARN_NOUN, arn)
}

/// [`invalid_arn_message`] for one that takes a component ARN.
pub fn invalid_component_arn_message(arn: &str) -> String {
    invalid_arn_message(COMPONENT_ARN_NOUN, arn)
}

/// The modelled refusal (`ValidationException`, HTTP 400) carrying `message`.
///
/// It states neither a `reason` nor a `fields` clause: the sentence already names the
/// value and the shape it had to have, and the ARN is the member the route itself
/// addresses — as [`super::target_not_found`], the other refusal of this service that is
/// one fixed sentence about one value, states neither.
fn invalid_arn(message: String) -> ValidationException {
    ValidationException {
        message,
        reason: None,
        fields: None,
    }
}

/// The component name and version an operation taking a component-VERSION ARN addresses,
/// or the refusal for a value that is not one.
pub fn addressed_component_version(arn: &str) -> Result<(String, String), ValidationException> {
    parse_component_version_arn(arn)
        .ok_or_else(|| invalid_arn(invalid_component_version_arn_message(arn)))
}

/// The component `ListComponentVersions` addresses — its name and the NAME-level component
/// ARN its versions are held under — or the refusal for a value that is neither shape.
///
/// Either shape names ONE component, so a component-version ARN is normalised to the
/// component ARN by dropping the version it names: the versions listed are the component's,
/// whichever of its versions the caller happened to hold an ARN for.
pub fn addressed_component(arn: &str) -> Result<(String, String), ValidationException> {
    if let Some(name) = parse_component_arn(arn) {
        return Ok((name, arn.to_string()));
    }
    if let Some((name, version)) = parse_component_version_arn(arn) {
        let component_arn = arn
            .strip_suffix(&format!(":versions:{version}"))
            .unwrap_or(arn)
            .to_string();
        return Ok((name, component_arn));
    }
    Err(invalid_arn(invalid_component_arn_message(arn)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ARN of this service in `region`, for the rows below.
    fn version_arn(name: &str, version: &str) -> String {
        component_version_arn("us-west-2", name, version)
    }

    /// RULE: either ARN shape names one component, and the component ARN its versions are
    /// held under is the same for both — so the ARN a create returns lists the versions the
    /// ARN a listing reports lists.
    #[test]
    fn both_arn_shapes_address_the_same_component() {
        let name_arn = component_name_arn("us-west-2", "com.example.Probe");
        for (case, arn) in [
            ("the component ARN", name_arn.clone()),
            (
                "the component-version ARN of its first version",
                version_arn("com.example.Probe", "1.0.0"),
            ),
            (
                "the component-version ARN of a later version",
                version_arn("com.example.Probe", "2.11.3"),
            ),
        ] {
            let addressed = addressed_component(&arn);
            assert_eq!(
                addressed.as_ref().ok(),
                Some(&("com.example.Probe".to_string(), name_arn.clone())),
                "case {case:?}: expected the one component {name_arn} is held under, got \
                 {addressed:?}"
            );
        }
    }

    /// RULE: a value outside the shape an operation takes is refused with that shape's own
    /// noun phrase and the value the request carried, and nothing else.
    #[test]
    fn a_value_outside_the_shape_is_refused_with_that_shapes_sentence() {
        // (case, the value, whether a component-version ARN reading takes it, whether a
        // component reading does)
        let rows: &[(&str, &str, bool, bool)] = &[
            ("no ARN at all", "not-an-arn", false, false),
            (
                "another vendor's ARN",
                "arn:aws:iot:us-west-2:000000000000:thing/Core",
                false,
                false,
            ),
            (
                "an account field that is neither `aws` nor digits",
                "arn:aws:greengrass:us-west-2:my-account:components:com.example.Probe",
                false,
                false,
            ),
            (
                "a component ARN",
                "arn:aws:greengrass:us-west-2:000000000000:components:com.example.Probe",
                false,
                true,
            ),
            (
                "a component-version ARN",
                "arn:aws:greengrass:us-west-2:000000000000:components:com.example.Probe:versions:1.0.0",
                true,
                true,
            ),
            (
                "a component-version ARN with no version after `versions`",
                "arn:aws:greengrass:us-west-2:000000000000:components:com.example.Probe:versions:",
                false,
                false,
            ),
            (
                "an AWS-published component's ARN",
                "arn:aws:greengrass:us-west-2:aws:components:aws.greengrass.Nucleus:versions:2.9.1",
                true,
                true,
            ),
        ];
        for (case, value, version_takes_it, component_takes_it) in rows {
            let version_reading = addressed_component_version(value);
            assert_eq!(
                version_reading.is_ok(),
                *version_takes_it,
                "case {case:?}: a component-version ARN reading must {} it, got \
                 {version_reading:?}",
                if *version_takes_it { "take" } else { "refuse" }
            );
            if let Err(refusal) = version_reading {
                assert_eq!(
                    refusal.message,
                    format!("Invalid Input: Component version ARN {value} is invalid"),
                    "case {case:?}: expected the component-version sentence, got {:?}",
                    refusal.message
                );
            }
            let component_reading = addressed_component(value);
            assert_eq!(
                component_reading.is_ok(),
                *component_takes_it,
                "case {case:?}: a component reading must {} it, got {component_reading:?}",
                if *component_takes_it { "take" } else { "refuse" }
            );
            if let Err(refusal) = component_reading {
                assert_eq!(
                    refusal.message,
                    format!("Invalid Input: Component ARN {value} is invalid"),
                    "case {case:?}: expected the component sentence, got {:?}",
                    refusal.message
                );
            }
        }
    }
}
