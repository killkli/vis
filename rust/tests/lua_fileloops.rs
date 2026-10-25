//! Shared native X/Y command-tree and lifecycle assertions.
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
#[test]
fn shared_native_file_loop_contract() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "vis-file-loop-contract-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("visrc.lua"), "require('vis')").unwrap();
    let input = path.join("input.txt");
    std::fs::write(&input, b"abc xyz\n").unwrap();
    let _config = Config(path.clone());
    let runtime = Runtime::new(&[path]).unwrap();
    runtime.open_cli_path(&input, None, false).unwrap();
    runtime
        .eval(include_str!("fixtures/file-loop-contract.lua"))
        .unwrap();
}
