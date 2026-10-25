//! Selection geometry is view-local while modes are global, shared with C vis.
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
fn shared_native_inactive_visual_geometry_contract() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "vis-view-contract-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("visrc.lua"), "require('vis')").unwrap();
    let _config = Config(path.clone());
    let runtime = Runtime::new(&[path]).unwrap();
    runtime
        .open_scratch(Some(b"abc xyz\n".to_vec()), false)
        .unwrap();
    runtime
        .eval(include_str!("fixtures/inactive-visual-contract.lua"))
        .unwrap();
}
