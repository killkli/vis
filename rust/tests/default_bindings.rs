//! Every default key binding of `config.def.h` must be routed.
//!
//! The test parses the native default tables, resolves each binding to the
//! key sequence it stands for and drives a fresh editor in the mode the
//! native table applies to. A binding the editor rejects is a missing default
//! key.

use vis::editor::{Editor, Mode};
use vis::text::Text;

/// One row of a native `KeyBinding` table.
struct Binding {
    table: String,
    /// The key sequence the binding stands for.
    probe: String,
    /// The native action the binding names, for reporting.
    action: String,
}

/// Tables of `config.def.h` and the mode they are active in.
fn tables_by_mode() -> Vec<(Mode, Vec<&'static str>)> {
    vec![
        (
            Mode::Normal,
            vec![
                "bindings_normal",
                "bindings_selections",
                "bindings_operators",
                "bindings_motions",
                "bindings_basic",
            ],
        ),
        (
            Mode::Visual,
            vec![
                "bindings_visual",
                "bindings_selections",
                "bindings_textobjects",
                "bindings_operators",
                "bindings_motions",
                "bindings_basic",
            ],
        ),
        (Mode::VisualLine, vec!["bindings_visual_line"]),
        (
            Mode::Insert,
            vec!["bindings_insert", "bindings_readline", "bindings_basic"],
        ),
        (Mode::Replace, vec!["bindings_replace"]),
    ]
}

/// Parse every binding row of `config.def.h`.
fn parse_bindings(source: &str) -> Vec<Binding> {
    let mut bindings = Vec::new();
    let mut table = String::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("static const KeyBinding ") {
            if let Some(name) = rest.split('[').next() {
                table = name.to_string();
            }
            continue;
        }
        if !trimmed.starts_with('{') {
            continue;
        }
        let Some(keys) = trimmed.split('"').nth(1) else {
            continue;
        };
        // C string escapes: a backslash before a quote is only an escape.
        let keys = keys.replace("\\'", "'");
        let rest = trimmed.split('"').nth(2).unwrap_or_default();
        let (probe, action) = if let Some(enum_name) = rest
            .split_once("ACTION(")
            .and_then(|(_, tail)| tail.split_once(')'))
            .map(|(name, _)| name.trim().to_string())
        {
            (keys.to_string(), enum_name)
        } else if let Some(alias) = rest
            .split_once("ALIAS(")
            .and_then(|(_, tail)| tail.split_once(')'))
            .map(|(name, _)| name.trim().trim_matches('"').to_string())
        {
            let alias = alias.replace("\\'", "'");
            (alias.clone(), format!("ALIAS {alias}"))
        } else {
            continue;
        };
        bindings.push(Binding {
            table: table.clone(),
            probe,
            action,
        });
    }
    bindings
}

/// Errors that only depend on the sample text or on earlier state.
fn contextual(error: &str) -> bool {
    error.contains("text object not found")
        || error.contains("no previous search")
        || error.contains("no previous character search")
        || error.contains("no previous register")
}

/// Sample text with brackets, quotes and words, so text objects and searches
/// have something to find.
const SAMPLE: &[u8] = b"one (two) three\n\"quoted\" word\n{brace} [bracket]\nlast line\n";

#[test]
fn every_default_binding_is_routed() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate parent");
    let config = std::fs::read_to_string(root.join("config.def.h")).expect("config.def.h");
    let bindings = parse_bindings(&config);
    assert!(!bindings.is_empty(), "no default bindings parsed");
    let mut missing = Vec::new();
    let mut total = 0;
    for (mode, tables) in tables_by_mode() {
        for table in tables {
            for binding in bindings.iter().filter(|b| b.table == table) {
                let keys = match Editor::notation_keys(&binding.probe) {
                    Ok(keys) => keys,
                    Err(error) => {
                        missing.push(format!(
                            "{} {:?} -> {}: {}",
                            mode.name(),
                            binding.probe,
                            binding.action,
                            error
                        ));
                        continue;
                    }
                };
                total += 1;
                let mut editor = Editor::new(Text::from_bytes(SAMPLE));
                editor.mode = mode;
                let mut failure = None;
                for key in keys {
                    if let Err(error) = editor.handle_key(key) {
                        failure = Some(error.0);
                        break;
                    }
                }
                if let Some(error) = failure {
                    if !contextual(&error) {
                        missing.push(format!(
                            "{} {:?} -> {}: {error}",
                            mode.name(),
                            binding.probe,
                            binding.action
                        ));
                    }
                }
            }
        }
    }
    assert!(total > 200, "only {total} default bindings probed");
    assert!(
        missing.is_empty(),
        "unrouted default bindings: {missing:#?}"
    );
}
