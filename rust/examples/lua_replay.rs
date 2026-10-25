//! Headless execution of the real Lua startup and fixture configuration.
//!
//! `lua_replay INPUT --config-dir DIR [--native-keys KEYS]`
//! The differential harness supplies an observation-only visrc which writes
//! `state`, `observed`, and `output` using the public, live Lua userdata API.
//! Fixture callbacks and saves run through Runtime and Workspace unchanged.

#[cfg(feature = "lua")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use std::path::PathBuf;
    use vis::editor::{Editor, Key};
    use vis::lua::Runtime;

    let mut args = std::env::args_os().skip(1);
    let input = args
        .next()
        .map(PathBuf::from)
        .ok_or("usage: lua_replay INPUT --config-dir DIR [--native-keys KEYS]")?;
    let mut config = None;
    let mut native_keys = None;
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--config-dir") => {
                config = Some(PathBuf::from(
                    args.next().ok_or("missing configuration directory")?,
                ))
            }
            Some("--native-keys") => {
                native_keys = Some(PathBuf::from(args.next().ok_or("missing native key file")?))
            }
            _ => return Err("unexpected lua_replay argument".into()),
        }
    }
    let config = config.ok_or("--config-dir is required")?;
    let runtime = Runtime::new(&[config])?;
    runtime.open_cli_path(&input, None, false)?;
    runtime.start()?;
    if let Some(path) = native_keys {
        let source = std::fs::read_to_string(path)?;
        // This route deliberately uses real host key events: macro recording
        // differs from Lua feedkeys in the C reference editor.
        for key in Editor::notation_keys(&source)? {
            runtime.handle_key(key)?;
        }
        runtime.handle_key(Key::Function(12))?;
    }
    if runtime.workspace().borrow().window_count() > 0 {
        runtime.command("qall!")?;
    }
    runtime.shutdown()?;
    for message in runtime.take_messages() {
        eprintln!("lua_replay: {message}");
    }
    for error in runtime.take_lua_errors() {
        eprintln!("lua_replay: Lua error: {error}");
    }
    if let Some(code) = runtime.exit_code().filter(|code| *code != 0) {
        return Err(format!("Lua requested exit status {code}").into());
    }
    let observed = std::fs::read_to_string("observed")?;
    if !matches!(observed.as_str(), "before_escape" | "closed_during_replay") {
        return Err("Lua replay did not verify its before-Escape observer".into());
    }
    let state = std::fs::read_to_string("state")?;
    if !state.contains("before_escape\n") {
        return Err("Lua replay is missing the before-Escape selection snapshot".into());
    }
    std::io::stdout()
        .lock()
        .write_all(&std::fs::read("output")?)?;
    Ok(())
}

#[cfg(feature = "lua")]
fn main() {
    if let Err(error) = run() {
        eprintln!("lua_replay: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(feature = "lua"))]
fn main() {
    eprintln!("lua_replay requires --features lua");
    std::process::exit(2);
}
