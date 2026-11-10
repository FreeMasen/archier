//! Runs the official ArchieML 1.0 test suite from the `conformance/archieml.org`
//! submodule (`git submodule update --init` to fetch it). Without the submodule the test
//! is skipped, unless `ARCHIER_REQUIRE_CONFORMANCE` is set, as it is in CI.
//!
//! Each test file is itself ArchieML: its `test` key describes the case and its `result`
//! key holds the expected output as JSON. Those two keys are removed from the parsed
//! document before comparing.
//!
//! `all.0.aml` is skipped. It concatenates the other tests, which turns nested blocks
//! such as `{.image}` inside a freeform array into top-level ones like `{00067.image}`,
//! and expects the literal key `"00067.image"`. That reproduces an archieml-js bug (it
//! reads the freeform state of the block being closed) and contradicts dot-notation as
//! defined by the spec and tested by `scopes.9` and `arrays.7`.

use std::fs;
use std::path::Path;

use archier::Value;
use serde_json::Value as Json;

fn to_json(value: &Value) -> Json {
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

#[test]
fn conformance_suite() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/archieml.org/test/1.0");
    let Ok(entries) = fs::read_dir(&dir) else {
        let message = format!(
            "conformance suite not found at {}; run `git submodule update --init`",
            dir.display()
        );
        assert!(
            std::env::var_os("ARCHIER_REQUIRE_CONFORMANCE").is_none(),
            "{message}"
        );
        eprintln!("skipping: {message}");
        return;
    };

    let mut paths: Vec<_> = entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "aml"))
        .filter(|path| !path.ends_with("all.0.aml"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no tests found in {}", dir.display());

    let mut failures = Vec::new();
    for path in &paths {
        let name = path.file_stem().unwrap().to_string_lossy();
        let source = fs::read_to_string(path).unwrap();
        let mut doc = archier::load(&source);
        let description = doc
            .remove("test")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let Some(Value::String(expected)) = doc.remove("result") else {
            failures.push(format!("{name}: missing `result` key"));
            continue;
        };
        let expected: Json = dbg!(serde_json::from_str(&expected).unwrap());
        let actual = dbg!(to_json(&Value::Object(doc)));
        if actual != expected {
            failures.push(format!(
                "{name}: {description}\n  expected: {expected}\n    actual: {actual}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} conformance tests failed:\n{}",
        failures.len(),
        paths.len(),
        failures.join("\n")
    );
}
