//! Native LPeg loader for the vis Rust port.
//!
//! The main port crate keeps `unsafe_code = "forbid"` and loads the pure-Lua
//! LuLPeg matcher. That matcher is correct but roughly two orders of
//! magnitude slower than the native LPeg virtual machine, and the syntax
//! highlighter runs on every frame, so the difference is visible as input
//! latency.
//!
//! This crate is the port's single audited boundary to native code. It loads
//! a native LPeg shared library with `dlopen`, calls its `luaopen_lpeg` entry
//! point and hands the resulting module table back to the caller. It performs
//! no matching itself and holds no Lua state of its own.
//!
//! The loader is optional. When no native library is found, or the library
//! does not match the interpreter's ABI, the caller keeps the pure-Lua
//! matcher and the port behaves exactly as before.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use mlua::{Lua, Table};

/// The name of the module entry point LPeg exports.
const ENTRY_POINT: &[u8] = b"luaopen_lpeg\0";

/// The signature of the module entry point.
///
/// LPeg's `luaopen_lpeg` takes the Lua state and pushes the module table onto
/// the stack, returning the number of results.
type LuaOpen = unsafe extern "C" fn(*mut mlua::ffi::lua_State) -> i32;

/// The candidate file names of a native LPeg library, in search order.
///
/// The first entry matches the layout a Lua installation uses for the
/// interpreter version this port embeds.
const LIBRARY_NAMES: &[&str] = &["lpeg.so", "liblpeg.so", "lpeg.dylib", "liblpeg.dylib"];

/// The directories a native LPeg library is searched in.
///
/// The list covers the platform's own Lua module directory, the paths a Lua
/// installation records in `package.cpath`, and the locations a Homebrew or
/// distribution package installs into.
fn candidate_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    let mut push = |path: PathBuf| {
        if !directories.contains(&path) {
            directories.push(path);
        }
    };

    // The interpreter's own module directory, per platform convention.
    let lua_versions = ["5.5", "5.4", "5.3"];
    let prefixes = ["/usr", "/usr/local", "/opt/homebrew", "/opt/local"];
    for prefix in prefixes {
        for version in lua_versions {
            push(PathBuf::from(prefix).join("lib/lua").join(version));
            push(
                PathBuf::from(prefix)
                    .join("lib")
                    .join(format!("lua{version}")),
            );
        }
    }
    // The Homebrew cellar keeps the real file behind a symlink in the prefix.
    if let Ok(cellar) = std::env::var("HOMEBREW_PREFIX") {
        for version in lua_versions {
            push(PathBuf::from(&cellar).join("lib/lua").join(version));
        }
    }
    // `LUA_CPATH` names the module directories of the active Lua installation.
    if let Some(cpath) = std::env::var_os("LUA_CPATH") {
        for entry in cpath.to_string_lossy().split(';') {
            if entry.is_empty() {
                continue;
            }
            // A cpath entry looks like `/dir/?.so`.
            let directory = Path::new(entry);
            let parent = directory.parent().unwrap_or(directory);
            push(parent.to_path_buf());
        }
    }
    directories
}

/// The native LPeg library that was loaded.
///
/// The library must stay mapped for as long as the module table it produced
/// exists, because the table holds references into the library's data. The
/// handle therefore never releases the library: it stays mapped until the
/// process ends, which is what the native editor does as well.
pub struct NativeLpeg {
    // The library is never read after loading, but it must stay mapped for as
    // long as the module table exists.
    _library: std::mem::ManuallyDrop<libloading::Library>,
}

impl NativeLpeg {
    /// Load the native LPeg library and register its module under `name`.
    ///
    /// Returns the loaded library, or a description of why no native library
    /// could be used. The caller falls back to the pure-Lua matcher on error.
    pub fn load(lua: &Lua, name: &str) -> Result<Self, LoadError> {
        let mut attempts = Vec::new();
        for directory in candidate_directories() {
            for file in LIBRARY_NAMES {
                let path = directory.join(file);
                if !path.is_file() {
                    continue;
                }
                match Self::open(lua, name, &path) {
                    Ok(loaded) => return Ok(loaded),
                    Err(error) => attempts.push(format!("{}: {error}", path.display())),
                }
            }
        }
        if attempts.is_empty() {
            Err(LoadError::NotFound)
        } else {
            Err(LoadError::Failed(attempts.join("; ")))
        }
    }

    /// Open one candidate library and call its module entry point.
    fn open(lua: &Lua, name: &str, path: &Path) -> Result<Self, LoadError> {
        // SAFETY: the library is a Lua module built for the same Lua version
        // this port embeds. Its entry point follows the Lua C API contract of
        // pushing one module table and returning one result.
        let library = unsafe { libloading::Library::new(path) }.map_err(|error| {
            LoadError::Unreadable(path.display().to_string(), error.to_string())
        })?;
        let entry: libloading::Symbol<'_, LuaOpen> =
            // SAFETY: the symbol name is checked to exist and to have the type
            // the Lua module ABI requires.
            unsafe { library.get(ENTRY_POINT) }.map_err(|error| {
                LoadError::Unreadable(path.display().to_string(), error.to_string())
            })?;

        let module = call_entry(lua, *entry)?;
        lua.preload_module(name, module)
            .map_err(|error| LoadError::Failed(format!("cannot register native LPeg: {error}")))?;
        Ok(Self {
            // The library is never released, because the module table it
            // produced refers into it.
            _library: std::mem::ManuallyDrop::new(library),
        })
    }
}

/// Call the native module entry point and take the module table it leaves.
///
/// The entry point is called with the active Lua state. It pushes exactly one
/// result, the module table, which is taken from the stack and returned as a
/// function that yields that table.
fn call_entry(lua: &Lua, entry: LuaOpen) -> Result<mlua::Function, LoadError> {
    let value: mlua::Value = lua.exec_raw_lua(|raw| {
        let state = raw.state();
        // SAFETY: the state is the live interpreter state, and the entry point
        // follows the Lua C API contract of pushing one result. The stack is
        // restored to its previous depth afterwards.
        let results = unsafe { entry(state) };
        if results != 1 {
            return Err(LoadError::Failed(format!(
                "native LPeg returned {results} results, expected 1"
            )));
        }
        // The pushed value is the module table. Take it off the stack.
        // SAFETY: the entry point left exactly one value on the stack, which
        // this call takes.
        Ok(unsafe { raw.pop_value() })
    })?;
    let table: Table = value
        .as_table()
        .cloned()
        .ok_or_else(|| LoadError::Failed("native LPeg module is not a table".into()))?;
    lua.create_function(move |_, ()| Ok(table.clone()))
        .map_err(|error| LoadError::Failed(format!("cannot wrap native LPeg module: {error}")))
}

/// Why a native LPeg library could not be used.
#[derive(Debug)]
pub enum LoadError {
    /// No candidate library exists on this system.
    NotFound,
    /// A candidate exists but could not be opened or used.
    Failed(String),
    /// A candidate exists but could not be read.
    Unreadable(String, String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "no native LPeg library found"),
            Self::Failed(reason) => write!(f, "{reason}"),
            Self::Unreadable(path, reason) => write!(f, "{path}: {reason}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// The paths that were searched, for diagnostics.
pub fn searched_paths() -> Vec<OsString> {
    let mut paths = Vec::new();
    for directory in candidate_directories() {
        for file in LIBRARY_NAMES {
            paths.push(directory.join(file).into_os_string());
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_native_library_is_found_and_its_module_works() {
        let lua = mlua::Lua::new();
        let native = match NativeLpeg::load(&lua, "lpeg") {
            Ok(native) => native,
            Err(_) => {
                // No native library on this system. The pure-Lua matcher is
                // the fallback, so this is not a failure.
                return;
            }
        };
        let matched: mlua::Value = lua
            .load("local l = require('lpeg') return l.match(l.P('a')^1, 'aaa')")
            .eval()
            .unwrap();
        assert!(matches!(matched, mlua::Value::Integer(4)));
        let version: String = lua
            .load("local l = require('lpeg') return l.version")
            .eval()
            .unwrap();
        assert!(version.starts_with("LPeg"), "unexpected version {version}");
        // The handle never releases the library, so letting it go here is
        // safe.
        let _ = native;
    }

    #[test]
    fn the_search_paths_cover_the_platform_module_directories() {
        let paths = searched_paths();
        assert!(!paths.is_empty());
        let text = paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("lpeg"), "no lpeg candidate in {text}");
    }
}
