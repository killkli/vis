//! Reentrant Lua actions operate on their captured object without stealing focus.
#![cfg(feature = "lua")]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use vis::lua::Runtime;

struct Config(PathBuf);
impl Config {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vis-lua-host-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("visrc.lua"), "require('vis')").unwrap();
        Self(path)
    }
}
impl Drop for Config {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn two_windows() -> (Config, Runtime) {
    let config = Config::new();
    let runtime = Runtime::new(std::slice::from_ref(&config.0)).unwrap();
    runtime.open_scratch(Some(b"abc".to_vec()), false).unwrap();
    runtime.eval("source=vis.win").unwrap();
    runtime.open_scratch(Some(b"xyz".to_vec()), false).unwrap();
    runtime.eval("destination=vis.win;vis.win=source").unwrap();
    (config, runtime)
}
#[test]
fn motion_callback_applies_to_origin_and_retains_the_callback_chosen_focus() {
    let (_config, runtime) = two_windows();
    runtime
        .eval(
            r#"
        local motion=vis:motion_register(function(win,pos)
            assert(win==source);vis.win=destination;return pos+1
        end)
        assert(vis:motion(motion))
        assert(source.selection.pos==1)
        assert(destination.selection.pos==0)
        assert(vis.win==destination)
    "#,
        )
        .unwrap();
}
#[test]
fn mapping_callback_focus_change_routes_remaining_keys_to_the_chosen_window() {
    let (_config, runtime) = two_windows();
    runtime
        .eval(
            r#"
        vis:map(vis.modes.NORMAL,'Q',function() vis.win=destination end)
        vis:feedkeys('Qi!<Escape>')
        assert(vis.win==destination)
        assert(destination.file:content(0,destination.file.size)=='!xyz')
        assert(source.file:content(0,source.file.size)=='abc')
    "#,
        )
        .unwrap();
}
#[test]
fn closing_the_motion_origin_cancels_its_continuation() {
    let (_config, runtime) = two_windows();
    runtime
        .eval(
            r#"
        local motion=vis:motion_register(function(win,pos)
            assert(win==source);assert(vis:command('q!'));return pos+1
        end)
        assert(vis:motion(motion))
        assert(vis.win==destination)
        assert(destination.selection.pos==0)
        assert(not pcall(function() return source.selection end))
    "#,
        )
        .unwrap();
}
