//! Parses an ArchieML file and prints it as JSON.
//!
//! ```sh
//! cargo run --example to_json -- path/to/document.aml
//! ```

use std::process::ExitCode;

use archier::Value;
use serde_json::Value as Json;

fn to_json(value: &Value<'_>) -> Json {
    match value {
        Value::String(s) => Json::String(s.to_string()),
        Value::Array(items) => Json::Array(items.iter().map(to_json).collect()),
        Value::Object(map) => Json::Object(
            map.iter()
                .map(|(k, v)| (k.to_owned(), to_json(v)))
                .collect(),
        ),
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(path), None) = (args.next(), args.next()) else {
        eprintln!("usage: to_json <path>");
        return ExitCode::from(2);
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("{}: {error}", path.to_string_lossy());
            return ExitCode::FAILURE;
        }
    };
    let json = to_json(&Value::Object(archier::load(&text)));
    println!(
        "{}",
        serde_json::to_string_pretty(&json).expect("JSON values serialize")
    );
    ExitCode::SUCCESS
}
