//! Shared C/Rust contracts for real, editable internal prompt windows.
#![cfg(feature = "lua")]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use vis::lua::Runtime;

struct Config(PathBuf);
impl Drop for Config {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn configured_runtime() -> (Config, Runtime) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "vis-prompt-contract-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("visrc.lua"), "require('vis')").unwrap();
    let source = path.join("input.txt");
    std::fs::write(&source, b"abc xyz\n").unwrap();
    let config = Config(path.clone());
    let runtime = Runtime::new(&[path]).unwrap();
    runtime.open_cli_path(&source, None, false).unwrap();
    (config, runtime)
}
#[test]
fn shared_native_prompt_contract() {
    let (_config, runtime) = configured_runtime();
    runtime
        .eval(include_str!("fixtures/prompt-contract.lua"))
        .unwrap();
}
#[test]
fn shared_native_command_result_contract() {
    let (_config, runtime) = configured_runtime();
    runtime
        .eval(include_str!("fixtures/command-result-contract.lua"))
        .unwrap();
}
#[test]
fn shared_native_lua_error_contract() {
    let (_config, runtime) = configured_runtime();
    runtime
        .eval(include_str!("fixtures/lua-error-contract.lua"))
        .unwrap();
}
#[test]
fn shared_native_history_command_contract() {
    let (_config, runtime) = configured_runtime();
    runtime
        .eval(include_str!("fixtures/history-command-contract.lua"))
        .unwrap();
}
#[test]
fn shared_native_command_callback_contract() {
    let (_config, runtime) = configured_runtime();
    runtime
        .eval(include_str!("fixtures/command-callback-contract.lua"))
        .unwrap();
}
#[test]
fn shared_native_selection_handle_contract() {
    let (_config, runtime) = configured_runtime();
    runtime
        .eval(include_str!("fixtures/selection-handle-contract.lua"))
        .unwrap();
}
#[test]
fn shared_native_command_registration_contract() {
    let (_config, runtime) = configured_runtime();
    runtime
        .eval(include_str!("fixtures/command-registration-contract.lua"))
        .unwrap();
}
