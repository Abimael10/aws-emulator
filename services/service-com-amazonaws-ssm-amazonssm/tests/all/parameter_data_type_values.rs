// SPDX-License-Identifier: CC0-1.0

//! What a parameter's `DataType` decides about a put, end to end over the awsJson1_1 wire.
//!
//! RULE under test: **`PutParameter` accepts the data type the request declares whatever it
//! names, and holds the VALUE and the TIER to what that data type states.**
//! `ParameterDataType` is a
//! plain string carrying a length (0..128) and no enum and no pattern
//! (`smithy_query(query="shape",shape="ParameterDataType")`), so every data-type string of
//! that length is a value the member admits — `PutParameter`'s documentation LISTS the data
//! types it describes ("The following data type values are supported. text aws:ec2:image
//! aws:ssm:integration", `smithy_query(query="doc",shape="PutParameterRequest",
//! member="DataType")`) rather than closing the set the wire takes. What the service refuses
//! is a value that is not in the form its data type states, and the sentence it is refused
//! with names the VALUE, because the value is what the caller has to change; a data type the
//! service serves only on an ADVANCED parameter is refused for every put that resolved to
//! another tier, with the sentence naming that data type.
//!
//! The rows differ along what the rule generalises over: WHICH data type the put declares
//! (each one the documentation lists, each one whose value form the service states, each one
//! served only on an advanced parameter, one no documentation names, one at the modelled
//! length limit), WHICH value it carries (one in the stated form, and each way that form can
//! be broken), which TIER it resolves to — and whether the data type came from this request
//! or from the parameter it is overwriting.
//!
//! Every case goes through the front door a client uses, and every write is read back through
//! `GetParameter` and ended with `DeleteParameter`, so no row passes on a put that stored
//! nothing.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::parameter_store::{call, error_code, error_message, router, ACCOUNT, REGION};

/// The sentences a value outside the form its data type states is refused with: FIXED text
/// per data type, carrying nothing from the request or from the store, written out here
/// independently of the service. Each states the form the value has to be in, and the rows
/// below are built from those forms.
const AWS_REGION_REFUSAL: &str = "The parameter value is not a valid AWS Region. Please check \
     https://docs.aws.amazon.com/global-infrastructure/latest/regions/aws-regions.html for the \
     list of valid regions.";

/// See [`AWS_REGION_REFUSAL`].
const AWS_ACCOUNT_ID_REFUSAL: &str = "The parameter value is not a valid AWS account ID.";

/// See [`AWS_REGION_REFUSAL`].
const BOOLEAN_REFUSAL: &str = "The parameter value is not a valid boolean. Accepted values \
     are true, True, TRUE, false, False, FALSE, 1 and 0.";

/// See [`AWS_REGION_REFUSAL`].
const INTEGER_REFUSAL: &str = "The parameter value is not a valid integer. A value must be an \
     optional minus sign followed by digits, with no leading zeros, no plus sign, no decimal \
     point and no whitespace.";

/// See [`AWS_REGION_REFUSAL`].
const FLOAT_REFUSAL: &str = "The parameter value is not a valid float. A value must be a \
     decimal number such as 1.5, .5 or 2.5e-8, with an optional minus sign and no leading \
     zeros. NaN and Infinity are not accepted.";

/// See [`AWS_REGION_REFUSAL`].
const BINARY_REFUSAL: &str = "The parameter value is not a valid binary number. A value must \
     be one or more binary digits, 0 or 1, with no prefix, sign, separator or whitespace.";

/// See [`AWS_REGION_REFUSAL`]. A host name's refusal names the DEFECT the value has where
/// the service's words for it are known; this is the one a value carrying an underscore gets.
const HOSTNAME_UNDERSCORE_REFUSAL: &str = "The parameter value is not a valid host name. It \
     contains an underscore, which host names do not allow.";

/// The sentence a host name with any other defect gets: the class alone, which is the
/// opening of [`HOSTNAME_UNDERSCORE_REFUSAL`].
const HOSTNAME_CLASS_REFUSAL: &str = "The parameter value is not a valid host name.";

/// See [`AWS_REGION_REFUSAL`]. An email address's refusal names the DEFECT too where the
/// service's words for it are known; this is the one a value carrying whitespace gets.
const EMAIL_WHITESPACE_REFUSAL: &str = "The parameter value is not a valid email address. It \
     contains whitespace or a control character.";

/// The sentence an email address with any other defect gets: the class alone.
const EMAIL_CLASS_REFUSAL: &str = "The parameter value is not a valid email address.";

/// See [`AWS_REGION_REFUSAL`]. An IPv4 address's refusal names the DEFECT too where the
/// service's words for it are known; this is the one a value carrying a character no address
/// is written with gets.
const IPV4_CHARACTER_REFUSAL: &str = "The parameter value is not a valid IPv4 address. It may \
     contain only the digits 0 to 9 and dots.";

/// The sentence an IPv4 address with any other defect gets: the class alone.
const IPV4_CLASS_REFUSAL: &str = "The parameter value is not a valid IPv4 address.";

/// See [`IPV4_CHARACTER_REFUSAL`], for the other address form.
const IPV6_CHARACTER_REFUSAL: &str = "The parameter value is not a valid IPv6 address. It may \
     contain only hexadecimal digits, colons, and the dots of an embedded IPv4 address. Zone \
     IDs, brackets and prefix lengths are not accepted.";

/// The sentence an IPv6 address with any other defect gets: the class alone.
const IPV6_CLASS_REFUSAL: &str = "The parameter value is not a valid IPv6 address.";

/// The data type whose value names an AWS Region.
const AWS_REGION: &str = "aws:region";

/// The data type whose value is an AWS account id.
const AWS_ACCOUNT_ID: &str = "aws:account:id";

/// The data type whose value is a truth value.
const BOOLEAN: &str = "boolean";

/// The data type whose value is a whole number, and whose refusal [`INTEGER_REFUSAL`] is.
const INTEGER: &str = "integer";

/// The data type whose value may carry a fraction.
const FLOAT: &str = "float";

/// The data type whose value is a binary number — its binary digits, as [`BINARY_REFUSAL`]
/// states.
const BINARY: &str = "binary";

/// The data type whose value is a host name.
const HOSTNAME: &str = "hostname";

/// The data type whose value is an email address.
const EMAIL: &str = "email";

/// The data type whose value is an IPv4 address.
const IPV4_ADDRESS: &str = "ipv4-address";

/// The data type whose value is an IPv6 address.
const IPV6_ADDRESS: &str = "ipv6-address";

/// The data types the service serves only on an ADVANCED parameter: a standard parameter may
/// not declare one at all.
const ADVANCED_ONLY_DATA_TYPES: &[&str] = &["url", "json", "jsonc"];

/// The one of those this suite's tier rows are driven with; every one of them is driven by
/// [`every_advanced_only_data_type_is_refused_on_the_standard_tier`].
const ADVANCED_ONLY: &str = "url";

/// The sentence a put declaring [`ADVANCED_ONLY`] outside the advanced tier is refused
/// with — fixed text around the data type the request declared.
fn advanced_only_refusal(data_type: &str) -> String {
    format!("The {data_type} data type is only supported for advanced parameters.")
}

/// The modelled length of `ParameterDataType`
/// (`smithy_query(query="shape",shape="ParameterDataType")`: min 0, max 128) — the one
/// constraint the member carries, so a data type this long is still a value it admits.
const MAX_DATA_TYPE_LENGTH: usize = 128;

/// The content size limit of the standard tier ("Standard parameters have a content size
/// limit of 4 KB", `smithy_query(query="doc",shape="PutParameterRequest",member="Tier")`), so
/// a value past it is one only an advanced parameter holds.
const STANDARD_VALUE_LIMIT: usize = 4 * 1024;

/// One accepted-data-type row: the case, the parameter name, the data type the put declares
/// and the value it carries.
struct Accepted {
    case: &'static str,
    name: &'static str,
    data_type: String,
    value: &'static str,
}

/// The class the rule covers on the accepting side: every data type the documentation lists
/// except the asynchronously validated one (whose write is gated elsewhere,
/// `all/parameter_image_data_type_validation.rs`), every data type whose value form the
/// service states, a data type no documentation names, and one at the modelled length limit.
fn accepted() -> Vec<Accepted> {
    vec![
        Accepted {
            case: "the default data type",
            name: "/data-type/text",
            data_type: "text".to_string(),
            value: "plain text",
        },
        Accepted {
            case: "the integration data type",
            name: "/data-type/integration",
            data_type: "aws:ssm:integration".to_string(),
            value: "subscription",
        },
        Accepted {
            case: "a commercial Region",
            name: "/data-type/region",
            data_type: AWS_REGION.to_string(),
            value: "us-east-1",
        },
        Accepted {
            case: "a Region of another partition",
            name: "/data-type/region-partition",
            data_type: AWS_REGION.to_string(),
            value: "us-gov-west-1",
        },
        Accepted {
            case: "an account id",
            name: "/data-type/account",
            data_type: AWS_ACCOUNT_ID.to_string(),
            value: "123456789012",
        },
        Accepted {
            case: "a truth value",
            name: "/data-type/boolean",
            data_type: BOOLEAN.to_string(),
            value: "true",
        },
        Accepted {
            case: "a truth value written as a digit",
            name: "/data-type/boolean-digit",
            data_type: BOOLEAN.to_string(),
            value: "0",
        },
        Accepted {
            case: "a whole-number data type",
            name: "/data-type/integer",
            data_type: INTEGER.to_string(),
            value: "443",
        },
        Accepted {
            case: "a negative whole number",
            name: "/data-type/negative",
            data_type: INTEGER.to_string(),
            value: "-17",
        },
        Accepted {
            case: "zero",
            name: "/data-type/zero",
            data_type: INTEGER.to_string(),
            value: "0",
        },
        Accepted {
            case: "a long run of digits",
            name: "/data-type/long",
            data_type: INTEGER.to_string(),
            value: "900719925474099",
        },
        Accepted {
            case: "a fraction",
            name: "/data-type/float",
            data_type: FLOAT.to_string(),
            value: "3.14159",
        },
        Accepted {
            case: "a number in exponent notation",
            name: "/data-type/float-exponent",
            data_type: FLOAT.to_string(),
            value: "-2.5e-8",
        },
        Accepted {
            case: "a fraction with no whole part",
            name: "/data-type/float-fraction",
            data_type: FLOAT.to_string(),
            value: ".5",
        },
        Accepted {
            case: "a binary number",
            name: "/data-type/binary",
            data_type: BINARY.to_string(),
            value: "1010101",
        },
        Accepted {
            case: "a qualified host name",
            name: "/data-type/hostname",
            data_type: HOSTNAME.to_string(),
            value: "db.example.com",
        },
        Accepted {
            case: "a host name of one label",
            name: "/data-type/hostname-single",
            data_type: HOSTNAME.to_string(),
            value: "localhost",
        },
        Accepted {
            case: "a mailbox",
            name: "/data-type/email",
            data_type: EMAIL.to_string(),
            value: "ops-team@example.com",
        },
        Accepted {
            case: "a dotted quad",
            name: "/data-type/ipv4",
            data_type: IPV4_ADDRESS.to_string(),
            value: "10.11.12.13",
        },
        Accepted {
            case: "an abbreviated IPv6 address",
            name: "/data-type/ipv6",
            data_type: IPV6_ADDRESS.to_string(),
            value: "2001:db8::1",
        },
        Accepted {
            case: "a data type no documentation names",
            name: "/data-type/invented",
            data_type: "text/plain".to_string(),
            value: "whatever this caller means by it",
        },
        Accepted {
            case: "a data type at the modelled length limit",
            name: "/data-type/longest",
            data_type: "d".repeat(MAX_DATA_TYPE_LENGTH),
            value: "stored all the same",
        },
    ]
}

/// One refused-value row: the case, the data type the put declares, the value it carries and
/// the sentence that data type refuses it with.
struct Refused {
    case: &'static str,
    data_type: &'static str,
    value: &'static str,
    refusal: &'static str,
}

/// The class the rule covers on the refusing side: each way the form each data type states
/// can be broken — a value of a neighbouring form, a value of the right shape but outside
/// the set, and text that is no value of the type at all.
fn refused() -> Vec<Refused> {
    vec![
        Refused {
            case: "a Region-shaped code nothing publishes",
            data_type: AWS_REGION,
            value: "us-east-9",
            refusal: AWS_REGION_REFUSAL,
        },
        Refused {
            case: "a Region's long name",
            data_type: AWS_REGION,
            value: "US East (N. Virginia)",
            refusal: AWS_REGION_REFUSAL,
        },
        Refused {
            case: "eleven digits",
            data_type: AWS_ACCOUNT_ID,
            value: "12345678901",
            refusal: AWS_ACCOUNT_ID_REFUSAL,
        },
        Refused {
            case: "an account id written with separators",
            data_type: AWS_ACCOUNT_ID,
            value: "1234-5678-9012",
            refusal: AWS_ACCOUNT_ID_REFUSAL,
        },
        Refused {
            case: "a spelling of true the sentence does not accept",
            data_type: BOOLEAN,
            value: "tRuE",
            refusal: BOOLEAN_REFUSAL,
        },
        Refused {
            case: "a word that is not a truth value",
            data_type: BOOLEAN,
            value: "yes",
            refusal: BOOLEAN_REFUSAL,
        },
        Refused {
            case: "a plus sign",
            data_type: INTEGER,
            value: "+443",
            refusal: INTEGER_REFUSAL,
        },
        Refused {
            case: "a leading zero",
            data_type: INTEGER,
            value: "0443",
            refusal: INTEGER_REFUSAL,
        },
        Refused {
            case: "a decimal point",
            data_type: INTEGER,
            value: "80.5",
            refusal: INTEGER_REFUSAL,
        },
        Refused {
            case: "leading whitespace",
            data_type: INTEGER,
            value: " 443",
            refusal: INTEGER_REFUSAL,
        },
        Refused {
            case: "trailing whitespace",
            data_type: INTEGER,
            value: "443 ",
            refusal: INTEGER_REFUSAL,
        },
        Refused {
            case: "whitespace between digits",
            data_type: INTEGER,
            value: "4 43",
            refusal: INTEGER_REFUSAL,
        },
        Refused {
            case: "a minus sign alone",
            data_type: INTEGER,
            value: "-",
            refusal: INTEGER_REFUSAL,
        },
        Refused {
            case: "words",
            data_type: INTEGER,
            value: "four hundred and forty three",
            refusal: INTEGER_REFUSAL,
        },
        Refused {
            case: "an exponent",
            data_type: INTEGER,
            value: "4e3",
            refusal: INTEGER_REFUSAL,
        },
        Refused {
            case: "a thousands separator",
            data_type: INTEGER,
            value: "1,000",
            refusal: INTEGER_REFUSAL,
        },
        Refused {
            case: "hexadecimal",
            data_type: INTEGER,
            value: "0x1f",
            refusal: INTEGER_REFUSAL,
        },
        Refused {
            case: "two decimal points",
            data_type: FLOAT,
            value: "1.2.3",
            refusal: FLOAT_REFUSAL,
        },
        Refused {
            case: "a float with a leading zero",
            data_type: FLOAT,
            value: "007.5",
            refusal: FLOAT_REFUSAL,
        },
        Refused {
            case: "a float with a plus sign",
            data_type: FLOAT,
            value: "+1.5",
            refusal: FLOAT_REFUSAL,
        },
        Refused {
            case: "a name for an infinity",
            data_type: FLOAT,
            value: "Infinity",
            refusal: FLOAT_REFUSAL,
        },
        Refused {
            case: "a name for something that is not a number",
            data_type: FLOAT,
            value: "NaN",
            refusal: FLOAT_REFUSAL,
        },
        Refused {
            case: "a digit that is not binary",
            data_type: BINARY,
            value: "1021",
            refusal: BINARY_REFUSAL,
        },
        Refused {
            case: "a prefixed binary number",
            data_type: BINARY,
            value: "0b1010",
            refusal: BINARY_REFUSAL,
        },
        Refused {
            case: "a host name carrying a space",
            data_type: HOSTNAME,
            value: "my host",
            refusal: HOSTNAME_CLASS_REFUSAL,
        },
        Refused {
            case: "a host name carrying an underscore",
            data_type: HOSTNAME,
            value: "my_host",
            refusal: HOSTNAME_UNDERSCORE_REFUSAL,
        },
        Refused {
            case: "a label opening with a hyphen",
            data_type: HOSTNAME,
            value: "-host.example",
            refusal: HOSTNAME_CLASS_REFUSAL,
        },
        Refused {
            case: "an address with no domain separator",
            data_type: EMAIL,
            value: "ops.example.com",
            refusal: EMAIL_CLASS_REFUSAL,
        },
        Refused {
            case: "an address naming no mailbox",
            data_type: EMAIL,
            value: "@example.com",
            refusal: EMAIL_CLASS_REFUSAL,
        },
        Refused {
            case: "an address carrying whitespace",
            data_type: EMAIL,
            value: "ops team@example.com",
            refusal: EMAIL_WHITESPACE_REFUSAL,
        },
        Refused {
            case: "an octet past 255",
            data_type: IPV4_ADDRESS,
            value: "10.0.0.256",
            refusal: IPV4_CLASS_REFUSAL,
        },
        Refused {
            case: "three octets",
            data_type: IPV4_ADDRESS,
            value: "10.0.0",
            refusal: IPV4_CLASS_REFUSAL,
        },
        Refused {
            case: "an IPv6 address under the IPv4 data type",
            data_type: IPV4_ADDRESS,
            value: "2001:db8::1",
            refusal: IPV4_CHARACTER_REFUSAL,
        },
        Refused {
            case: "an IPv4 address under the IPv6 data type",
            data_type: IPV6_ADDRESS,
            value: "10.0.0.1",
            refusal: IPV6_CLASS_REFUSAL,
        },
        Refused {
            case: "a digit that is not hexadecimal",
            data_type: IPV6_ADDRESS,
            value: "2001:dg8::1",
            refusal: IPV6_CHARACTER_REFUSAL,
        },
        Refused {
            case: "an address carrying a zone id",
            data_type: IPV6_ADDRESS,
            value: "fe80::1%eth0",
            refusal: IPV6_CHARACTER_REFUSAL,
        },
        Refused {
            case: "an address in brackets",
            data_type: IPV6_ADDRESS,
            value: "[2001:db8::1]",
            refusal: IPV6_CHARACTER_REFUSAL,
        },
        Refused {
            case: "an address carrying a prefix length",
            data_type: IPV6_ADDRESS,
            value: "2001:db8::1/64",
            refusal: IPV6_CHARACTER_REFUSAL,
        },
        Refused {
            case: "two abbreviations in one address",
            data_type: IPV6_ADDRESS,
            value: "2001::db8::1",
            refusal: IPV6_CLASS_REFUSAL,
        },
    ]
}

/// `GetParameter` of `name`, returning `(status, body)`.
async fn get(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(router, "GetParameter", &json!({ "Name": name })).await
}

#[tokio::test]
async fn a_put_takes_the_data_type_it_declares_and_the_parameter_reads_back_under_it() {
    let router = router();
    for row in accepted() {
        let (status, written) = call(
            &router,
            "PutParameter",
            &json!({
                "Name": row.name,
                "Type": "String",
                "Value": row.value,
                "DataType": row.data_type,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: PutParameter under DataType {:?} must succeed, got {status} {written}",
            row.case, row.data_type
        );
        assert_eq!(
            written["Version"].as_i64(),
            Some(1),
            "case {:?}: the create reports the version it allocated, got {written}",
            row.case
        );
        assert_eq!(
            written["Tier"].as_str(),
            Some("Standard"),
            "case {:?}: the create reports the tier it resolved, got {written}",
            row.case
        );

        // The write is read back through the reader a client uses, so the row cannot pass on
        // a put that stored nothing.
        let (status, read) = get(&router, row.name).await;
        assert_eq!(
            status, 200,
            "case {:?}: GetParameter must succeed, got {status} {read}",
            row.case
        );
        let parameter = &read["Parameter"];
        assert_eq!(
            parameter["DataType"].as_str(),
            Some(row.data_type.as_str()),
            "case {:?}: expected the stored DataType, got {parameter}",
            row.case
        );
        assert_eq!(
            parameter["Value"].as_str(),
            Some(row.value),
            "case {:?}: expected the stored Value, got {parameter}",
            row.case
        );
        assert_eq!(
            parameter["Type"].as_str(),
            Some("String"),
            "case {:?}: expected the stored Type, got {parameter}",
            row.case
        );
        assert_eq!(
            parameter["Version"].as_i64(),
            Some(1),
            "case {:?}: expected the stored Version, got {parameter}",
            row.case
        );
        assert_eq!(
            parameter["ARN"].as_str(),
            Some(
                format!(
                    "arn:aws:ssm:{REGION}:{ACCOUNT}:parameter{}",
                    row.name
                )
                .as_str()
            ),
            "case {:?}: expected the parameter's own ARN, got {parameter}",
            row.case
        );
        assert!(
            parameter["LastModifiedDate"].is_number(),
            "case {:?}: expected a LastModifiedDate, got {parameter}",
            row.case
        );

        // ...and the lifecycle a client built on such a parameter ends the same way.
        let (status, deleted) = call(
            &router,
            "DeleteParameter",
            &json!({ "Name": row.name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: DeleteParameter must succeed, got {status} {deleted}",
            row.case
        );
        let (status, gone) = get(&router, row.name).await;
        assert_eq!(
            status, 400,
            "case {:?}: the deleted parameter is gone, got {status} {gone}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_value_outside_the_form_its_data_type_states_is_refused_for_the_value() {
    let router = router();
    for row in refused() {
        let name = "/data-type/refused";
        let (status, response) = call(
            &router,
            "PutParameter",
            &json!({
                "Name": name,
                "Type": "String",
                "Value": row.value,
                "DataType": row.data_type,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {:?}: PutParameter of {:?} under {:?} must be refused, got {status} {response}",
            row.case, row.value, row.data_type
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {:?}: expected ValidationException, got {response}",
            row.case
        );
        assert_eq!(
            error_message(&response),
            row.refusal,
            "case {:?}: expected the value-level sentence of {:?}, got {response}",
            row.case, row.data_type
        );
        // A refused put wrote nothing, so the name resolves to no parameter.
        let (status, gone) = get(&router, name).await;
        assert_eq!(
            status, 400,
            "case {:?}: the refused put stored nothing, got {status} {gone}",
            row.case
        );
    }
}

/// Every data type the service serves only on an advanced parameter is refused on the
/// STANDARD tier with the sentence naming it, whatever value it carries — the refusal names
/// the remedy (the tier) rather than claiming the data type does not exist — and is served
/// once the put resolves to the advanced tier.
#[tokio::test]
async fn every_advanced_only_data_type_is_refused_on_the_standard_tier() {
    let router = router();
    for (index, data_type) in ADVANCED_ONLY_DATA_TYPES.iter().enumerate() {
        let name = format!("/data-type/advanced-only-set/{index}");
        let value = "{\"stored\":true}";
        let (status, response) = call(
            &router,
            "PutParameter",
            &json!({
                "Name": &name,
                "Type": "String",
                "Value": value,
                "DataType": data_type,
                "Tier": "Standard",
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {data_type:?}: the standard-tier put must be refused, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {data_type:?}: expected ValidationException, got {response}"
        );
        assert_eq!(
            error_message(&response),
            advanced_only_refusal(data_type),
            "case {data_type:?}: expected the tier-level sentence, got {response}"
        );

        // The same data type on the advanced tier is served, and the parameter reads back
        // under it.
        let (status, written) = call(
            &router,
            "PutParameter",
            &json!({
                "Name": &name,
                "Type": "String",
                "Value": value,
                "DataType": data_type,
                "Tier": "Advanced",
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {data_type:?}: the advanced-tier put must succeed, got {status} {written}"
        );
        let (status, read) = get(&router, &name).await;
        assert_eq!(
            status, 200,
            "case {data_type:?}: GetParameter must succeed, got {status} {read}"
        );
        assert_eq!(
            read["Parameter"]["DataType"].as_str(),
            Some(*data_type),
            "case {data_type:?}: expected the stored DataType, got {read}"
        );
    }
}

#[tokio::test]
async fn a_put_with_no_value_at_all_is_refused_under_a_data_type_that_states_a_form() {
    let router = router();
    let name = "/data-type/empty-value";
    let (status, response) = call(
        &router,
        "PutParameter",
        &json!({ "Name": name, "Type": "String", "Value": "", "DataType": INTEGER }),
    )
    .await;
    assert_eq!(
        status, 400,
        "an empty value is no whole number, got {status} {response}"
    );
    assert_eq!(
        error_message(&response),
        INTEGER_REFUSAL,
        "expected the value-level sentence, got {response}"
    );
}

#[tokio::test]
async fn a_data_type_served_only_on_an_advanced_parameter_turns_on_the_resolved_tier() {
    let router = router();
    // (case, the `Tier` the put names, the value's size, whether the put is served)
    let rows: Vec<(&str, Option<&str>, usize, bool)> = vec![
        ("a put naming no tier", None, 32, false),
        ("a put naming the standard tier", Some("Standard"), 32, false),
        (
            "a put asking the service to choose, with a value a standard parameter holds",
            Some("Intelligent-Tiering"),
            32,
            false,
        ),
        ("a put naming the advanced tier", Some("Advanced"), 32, true),
        (
            "a put asking the service to choose, with a value only an advanced parameter holds",
            Some("Intelligent-Tiering"),
            STANDARD_VALUE_LIMIT + 1,
            true,
        ),
    ];
    for (index, (case, tier, size, served)) in rows.into_iter().enumerate() {
        let name = format!("/data-type/advanced-only/{index}");
        let value = format!("https://example.invalid/{}", "p".repeat(size));
        let mut body = json!({
            "Name": &name,
            "Type": "String",
            "Value": value,
            "DataType": ADVANCED_ONLY,
        });
        if let Some(tier) = tier {
            body["Tier"] = json!(tier);
        }
        let (status, response) = call(&router, "PutParameter", &body).await;
        if !served {
            assert_eq!(
                status, 400,
                "case {case:?}: the put must be refused, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {case:?}: expected ValidationException, got {response}"
            );
            assert_eq!(
                error_message(&response),
                advanced_only_refusal(ADVANCED_ONLY),
                "case {case:?}: expected the tier-level sentence, got {response}"
            );
            let (status, gone) = get(&router, &name).await;
            assert_eq!(
                status, 400,
                "case {case:?}: the refused put stored nothing, got {status} {gone}"
            );
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: the put must succeed, got {status} {response}"
        );
        assert_eq!(
            response["Tier"].as_str(),
            Some("Advanced"),
            "case {case:?}: the served put reports the advanced tier, got {response}"
        );
        let (status, read) = get(&router, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetParameter must succeed, got {status} {read}"
        );
        assert_eq!(
            read["Parameter"]["DataType"].as_str(),
            Some(ADVANCED_ONLY),
            "case {case:?}: expected the stored DataType, got {read}"
        );
    }

    // An update that states no data type and no tier inherits both, so the parameter keeps
    // the data type its advanced tier admits.
    let name = "/data-type/advanced-only/0";
    let (status, written) = call(
        &router,
        "PutParameter",
        &json!({
            "Name": name,
            "Type": "String",
            "Value": "https://example.invalid/first",
            "DataType": ADVANCED_ONLY,
            "Tier": "Advanced",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the advanced create must succeed, got {status} {written}"
    );
    let (status, written) = call(
        &router,
        "PutParameter",
        &json!({
            "Name": name,
            "Value": "https://example.invalid/second",
            "Overwrite": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "an update of the advanced parameter must succeed, got {status} {written}"
    );
    let (status, read) = get(&router, name).await;
    assert_eq!(status, 200, "the update is readable, got {status} {read}");
    assert_eq!(
        (
            read["Parameter"]["Value"].as_str(),
            read["Parameter"]["DataType"].as_str()
        ),
        (Some("https://example.invalid/second"), Some(ADVANCED_ONLY)),
        "expected the updated value under the inherited data type, got {read}"
    );
}

#[tokio::test]
async fn an_overwrite_is_judged_against_the_data_type_the_parameter_already_carries() {
    let router = router();
    let name = "/data-type/inherited";
    let (status, written) = call(
        &router,
        "PutParameter",
        &json!({ "Name": name, "Type": "String", "Value": "1", "DataType": INTEGER }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the create must succeed, got {status} {written}"
    );

    // An update states no data type, so the one in force is the parameter's own — and the
    // value is held to the form that data type states.
    let (status, response) = call(
        &router,
        "PutParameter",
        &json!({ "Name": name, "Value": "two", "Overwrite": true }),
    )
    .await;
    assert_eq!(
        status, 400,
        "the inherited data type still judges the value, got {status} {response}"
    );
    assert_eq!(
        error_message(&response),
        INTEGER_REFUSAL,
        "expected the value-level sentence, got {response}"
    );

    // The refused overwrite left the parameter exactly as it was.
    let (status, read) = get(&router, name).await;
    assert_eq!(status, 200, "the parameter survives, got {status} {read}");
    assert_eq!(
        (
            read["Parameter"]["Value"].as_str(),
            read["Parameter"]["Version"].as_i64()
        ),
        (Some("1"), Some(1)),
        "expected the refused overwrite to change nothing, got {read}"
    );

    // ...and an update carrying a value the data type admits is served.
    let (status, written) = call(
        &router,
        "PutParameter",
        &json!({ "Name": name, "Value": "-2", "Overwrite": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "an update carrying a whole number must succeed, got {status} {written}"
    );
    assert_eq!(
        written["Version"].as_i64(),
        Some(2),
        "the update reports the version it created, got {written}"
    );
    let (status, read) = get(&router, name).await;
    assert_eq!(status, 200, "the update is readable, got {status} {read}");
    assert_eq!(
        (
            read["Parameter"]["Value"].as_str(),
            read["Parameter"]["DataType"].as_str()
        ),
        (Some("-2"), Some(INTEGER)),
        "expected the updated value under the inherited data type, got {read}"
    );
}
