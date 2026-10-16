//! Generates the control plane's Rust code from `proto/fleet/v1/`, and renders the
//! compiled wire schema as text for the snapshot test that pins it (ADR-0019).
//!
//! protox compiles the files in pure Rust, so no system `protoc` is needed, and
//! tonic-prost-build writes the messages, the client and the server into `OUT_DIR`.
//! Nothing generated is committed. Every message that holds a secret is listed in
//! `REDACTED`: it gets no derived `Debug`, and `src/redact.rs` writes one that never
//! prints the secret.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::Write as _;
use std::path::PathBuf;

use protox::prost_reflect::{Cardinality, DescriptorPool, FieldDescriptor, Kind};

/// The protocol's files, relative to `proto/`.
const PROTOS: [&str; 6] = [
    "fleet/v1/agent.proto",
    "fleet/v1/bot.proto",
    "fleet/v1/chat.proto",
    "fleet/v1/common.proto",
    "fleet/v1/enrollment.proto",
    "fleet/v1/mode.proto",
];

/// The messages that hold a secret, by their full protobuf name.
const REDACTED: [&str; 2] = ["fleet.v1.EnrollRequest", "fleet.v1.OnlineSession"];

fn main() -> Result<(), Box<dyn Error>> {
    // Cargo reads build-script directives from stdout, so clippy doesn't apply
    // `print_stdout` to build scripts and no lint exception is needed.
    println!("cargo::rerun-if-changed=proto");
    let files = protox::compile(PROTOS, ["proto"])?;
    let pool = DescriptorPool::from_file_descriptor_set(files.clone())?;
    let out = PathBuf::from(std::env::var("OUT_DIR")?);
    std::fs::write(out.join("wire-schema.txt"), render(&pool)?)?;
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .build_transport(false)
        .codec_path("crate::codec::BoundedCodec")
        .skip_debug(REDACTED.map(|name| format!(".{name}")))
        .compile_fds(files)?;
    Ok(())
}

/// Renders what the wire depends on: every message with its fields' numbers,
/// names, types and oneofs, its reserved numbers and names, every enum with its
/// values, every service with its methods, and the redacted messages. Comments
/// and source positions are left out, so the text is the same on every platform.
fn render(pool: &DescriptorPool) -> Result<String, std::fmt::Error> {
    let mut text = String::new();
    let messages: BTreeMap<_, _> = pool
        .all_messages()
        .map(|message| (message.full_name().to_owned(), message))
        .collect();
    for (name, message) in &messages {
        writeln!(text, "message {name}")?;
        for field in message.fields() {
            writeln!(text, "  {}", field_line(&field))?;
        }
        let proto = message.descriptor_proto();
        for range in &proto.reserved_range {
            writeln!(
                text,
                "  reserved {}..{}",
                range.start.unwrap_or_default(),
                range.end.unwrap_or_default()
            )?;
        }
        for reserved in &proto.reserved_name {
            writeln!(text, "  reserved \"{reserved}\"")?;
        }
    }
    let enums: BTreeMap<_, _> = pool
        .all_enums()
        .map(|item| (item.full_name().to_owned(), item))
        .collect();
    for (name, item) in &enums {
        writeln!(text, "enum {name}")?;
        for value in item.values() {
            writeln!(text, "  {} {}", value.number(), value.name())?;
        }
    }
    let services: BTreeMap<_, _> = pool
        .services()
        .map(|service| (service.full_name().to_owned(), service))
        .collect();
    for (name, service) in &services {
        writeln!(text, "service {name}")?;
        for method in service.methods() {
            writeln!(
                text,
                "  rpc {}({}{}) returns ({}{})",
                method.name(),
                if method.is_client_streaming() {
                    "stream "
                } else {
                    ""
                },
                method.input().full_name(),
                if method.is_server_streaming() {
                    "stream "
                } else {
                    ""
                },
                method.output().full_name()
            )?;
        }
    }
    for name in REDACTED {
        writeln!(text, "redacted {name}")?;
    }
    Ok(text)
}

/// One field: its number, name, label, type and oneof.
fn field_line(field: &FieldDescriptor) -> String {
    let oneof = field.containing_oneof();
    let label = match (field.cardinality(), &oneof) {
        (Cardinality::Repeated, _) => "repeated ",
        (_, Some(oneof)) if oneof.is_synthetic() => "optional ",
        (Cardinality::Required, _) => "required ",
        (Cardinality::Optional, _) => "",
    };
    let within = match &oneof {
        Some(oneof) if !oneof.is_synthetic() => format!(" (oneof {})", oneof.name()),
        _ => String::new(),
    };
    format!(
        "{} {}: {label}{}{within}",
        field.number(),
        field.name(),
        kind_name(&field.kind())
    )
}

/// The type of a field: a scalar's protobuf name, or a message's or enum's full name.
fn kind_name(kind: &Kind) -> String {
    match kind {
        Kind::Double => "double".to_owned(),
        Kind::Float => "float".to_owned(),
        Kind::Int32 => "int32".to_owned(),
        Kind::Int64 => "int64".to_owned(),
        Kind::Uint32 => "uint32".to_owned(),
        Kind::Uint64 => "uint64".to_owned(),
        Kind::Sint32 => "sint32".to_owned(),
        Kind::Sint64 => "sint64".to_owned(),
        Kind::Fixed32 => "fixed32".to_owned(),
        Kind::Fixed64 => "fixed64".to_owned(),
        Kind::Sfixed32 => "sfixed32".to_owned(),
        Kind::Sfixed64 => "sfixed64".to_owned(),
        Kind::Bool => "bool".to_owned(),
        Kind::String => "string".to_owned(),
        Kind::Bytes => "bytes".to_owned(),
        Kind::Message(message) => message.full_name().to_owned(),
        Kind::Enum(item) => item.full_name().to_owned(),
    }
}
