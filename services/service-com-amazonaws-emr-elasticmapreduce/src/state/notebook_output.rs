// SPDX-License-Identifier: CC0-1.0

//! The OUTPUT NOTEBOOK a finished notebook execution leaves in Amazon S3: the executed
//! notebook, and the Amazon S3 address it was stored at.
//!
//! One concern of this service's state, split out of the run ([`super::notebook_run`]) the
//! way the run itself is split out of the lifecycle: the run owns what the notebook's cells
//! CAME TO, and this owns the document that run produced — "the Amazon S3 location that
//! stores the notebook execution output" (`OutputNotebookS3Location` doc, Smithy model) —
//! and `NotebookExecution$OutputNotebookURI`, "the location of the notebook execution's
//! output file in Amazon S3", which names the object this module really wrote.
//!
//! The document is the notebook that RAN: the input notebook's own cells, each code cell
//! carrying the output its own execution produced ([`executed_notebook`]) in the order the
//! kernel ran them. Nothing here invents a cell, an output or an execution count that the
//! run did not produce, and an execution whose notebook did not run leaves no output object
//! at all.
//!
//! "The output format for the notebook execution" (`OutputNotebookFormat` doc, whose one
//! declared value is `HTML`) chooses what is stored: an execution asking for `HTML` gets
//! that same executed notebook rendered as one HTML document ([`notebook_html`]), and one
//! asking for no format gets the notebook document itself, which is the format its input
//! arrived in.

use smithy_server_runtime::EmbeddedEmulator;

use crate::ops::types::OutputNotebookS3LocationFromInput;

/// `OutputNotebookFormat`'s one declared value (`smithy_query(query="shape",
/// shape="OutputNotebookFormat")`): the output notebook is stored as one HTML document.
pub const OUTPUT_NOTEBOOK_FORMAT_HTML: &str = "HTML";

/// The media type the output object is stored under, per format: the HTML rendering is an
/// HTML document, and the notebook document is a notebook — the media type the Jupyter
/// notebook format registers for an `.ipynb` document.
const HTML_MEDIA_TYPE: &str = "text/html";
const NOTEBOOK_MEDIA_TYPE: &str = "application/x-ipynb+json";

/// The extension the stored object's key carries, per format.
const HTML_EXTENSION: &str = "html";
const NOTEBOOK_EXTENSION: &str = "ipynb";

/// What one execution's output notebook is stored AS: the document's bytes, the media type
/// it is stored under, and the extension its key carries.
struct OutputDocument {
    body: Vec<u8>,
    media_type: &'static str,
    extension: &'static str,
}

/// Store the output notebook of the execution `execution_id` and answer the Amazon S3 URI of
/// the object that was really stored, or `None` when there is no output object: an execution
/// naming no output location ("the Amazon S3 location that stores the notebook execution
/// output" is optional input), one whose location names no bucket or key, and one whose
/// write the object store did not accept.
///
/// Called with NO store guard held: it writes over the emulator's own in-process transport
/// ([`::service_common::s3_client`]), the same hop every other write of this service's
/// artifacts travels. The address is the bucket and key the request named — no part of it
/// becomes a host, a scheme, a URL or a filesystem path — and the object is the document the
/// run produced.
pub async fn store_output_notebook(
    callback: &EmbeddedEmulator,
    region: &str,
    location: Option<&OutputNotebookS3LocationFromInput>,
    format: Option<&str>,
    execution_id: &str,
    notebook: &::serde_json::Value,
    cell_outputs: &[String],
) -> Option<String> {
    let location = location?;
    let bucket = location.bucket.as_deref().filter(|name| !name.is_empty())?;
    let requested_key = location.key.as_deref().filter(|key| !key.is_empty())?;
    let document = output_document(format, execution_id, notebook, cell_outputs)?;
    let key = output_notebook_key(requested_key, execution_id, document.extension);
    let stored = ::service_common::s3_client::s3_client(callback, region)
        .put_object()
        .bucket(bucket)
        .key(&key)
        .content_type(document.media_type)
        .body(::aws_sdk_s3::primitives::ByteStream::from(document.body))
        .send()
        .await;
    if let Err(error) = stored {
        // Nothing points a caller at an object the store does not hold: an execution whose
        // output could not be stored reports no `OutputNotebookURI`.
        ::tracing::warn!(
            execution_id = %execution_id,
            bucket = %bucket,
            key = %key,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "an Amazon EMR notebook execution's output notebook could not be stored in \
             Amazon S3"
        );
        return None;
    }
    Some(format!("s3://{bucket}/{key}"))
}

/// The KEY the output object is stored under, for the key the request named.
///
/// "The key to the Amazon S3 location that stores the notebook execution output"
/// (`OutputNotebookS3Location$Key` doc) is where the output goes, so a key naming an object
/// is used as it stands. A key naming a PREFIX — one ending in the separator, which is how a
/// caller names a location many executions' outputs are stored under — holds ONE object per
/// execution, named after the execution and the format it was stored in, so two executions
/// sharing one prefix do not overwrite each other's output.
fn output_notebook_key(requested: &str, execution_id: &str, extension: &str) -> String {
    if requested.ends_with('/') {
        return format!("{requested}{execution_id}.{extension}");
    }
    requested.to_string()
}

/// The document this execution's output notebook is stored as, for the format it asked for.
///
/// An execution naming a format this emulator stores nothing for leaves no output object,
/// rather than an object stored as something other than what was asked for.
fn output_document(
    format: Option<&str>,
    execution_id: &str,
    notebook: &::serde_json::Value,
    cell_outputs: &[String],
) -> Option<OutputDocument> {
    let executed = executed_notebook(notebook, cell_outputs);
    match format {
        Some(OUTPUT_NOTEBOOK_FORMAT_HTML) => Some(OutputDocument {
            body: notebook_html(execution_id, &executed).into_bytes(),
            media_type: HTML_MEDIA_TYPE,
            extension: HTML_EXTENSION,
        }),
        None => Some(OutputDocument {
            body: ::serde_json::to_vec(&executed).ok()?,
            media_type: NOTEBOOK_MEDIA_TYPE,
            extension: NOTEBOOK_EXTENSION,
        }),
        Some(_) => None,
    }
}

/// The EXECUTED notebook: `notebook` with each code cell carrying what its own execution
/// produced.
///
/// The notebook format records a cell's execution on the cell itself: `execution_count` is
/// which execution of the kernel ran it, and `outputs` holds what it produced, a `stream`
/// output carrying the text the cell wrote to the standard output stream. The cells the
/// outputs are matched to are the ones the KERNEL was handed
/// ([`super::notebook_cell_program`], the one reading of that), in the order it ran them,
/// which is the order the notebook holds them — so a cell's recorded output is its own. A
/// cell that wrote nothing records no output, and a cell the run never reached (one after a
/// cell that raised) keeps the cell as the input notebook held it.
pub fn executed_notebook(
    notebook: &::serde_json::Value,
    cell_outputs: &[String],
) -> ::serde_json::Value {
    let mut executed = notebook.clone();
    let Some(cells) = executed
        .get_mut("cells")
        .and_then(::serde_json::Value::as_array_mut)
    else {
        return executed;
    };
    let mut ran = 0_usize;
    for cell in cells.iter_mut() {
        if super::notebook_cell_program(cell).is_none() {
            continue;
        }
        let Some(output) = cell_outputs.get(ran) else {
            continue;
        };
        ran += 1;
        let mut outputs: Vec<::serde_json::Value> = Vec::new();
        if !output.is_empty() {
            outputs.push(::serde_json::json!({
                "output_type": "stream",
                "name": "stdout",
                "text": output,
            }));
        }
        cell["execution_count"] = ::serde_json::json!(ran);
        cell["outputs"] = ::serde_json::Value::Array(outputs);
    }
    executed
}

/// The executed notebook rendered as ONE HTML document, which is what an execution asking
/// for the `HTML` output format is stored as.
///
/// It holds what the notebook holds, in the notebook's own order: each cell's source, and
/// for a code cell the output its execution produced. Every piece of it comes from the
/// notebook — the text is escaped with the maintained HTML escaper this workspace already
/// uses, so a cell whose source or output carries markup is rendered as that text rather
/// than as markup.
pub fn notebook_html(execution_id: &str, executed: &::serde_json::Value) -> String {
    let escape = |text: &str| ::html_escape::encode_safe(text).into_owned();
    let mut document = String::new();
    document.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    document.push_str("<meta charset=\"utf-8\">\n<title>");
    document.push_str(&escape(execution_id));
    document.push_str("</title>\n</head>\n<body>\n");
    for cell in executed
        .get("cells")
        .and_then(::serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let kind = cell
            .get("cell_type")
            .and_then(::serde_json::Value::as_str)
            .unwrap_or("raw");
        document.push_str("<div class=\"cell ");
        document.push_str(&escape(kind));
        document.push_str("\">\n<pre class=\"source\">");
        document.push_str(&escape(&cell_text(cell.get("source"))));
        document.push_str("</pre>\n");
        for output in cell
            .get("outputs")
            .and_then(::serde_json::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            document.push_str("<pre class=\"output\">");
            document.push_str(&escape(&cell_text(output.get("text"))));
            document.push_str("</pre>\n");
        }
        document.push_str("</div>\n");
    }
    document.push_str("</body>\n</html>\n");
    document
}

/// The text of a notebook member that is either one string or the list of its lines, which
/// is how the notebook format spells both a cell's `source` and a stream output's `text` —
/// read through the crate's ONE reading of that spelling ([`super::notebook_text`]).
fn cell_text(value: Option<&::serde_json::Value>) -> String {
    super::notebook_text(value).unwrap_or_default()
}
