//! Cache for syntax highlighting results.
//!
//! The `vis-std` module lexes the visible window on every `WIN_HIGHLIGHT`
//! event, which the editor emits once per frame. Lexing the same bytes again
//! for every keystroke is the single most expensive part of a frame, so the
//! result is kept while the input is unchanged.
//!
//! The cache lives on this side of the Lua boundary, so the Lua modules stay
//! unchanged and the native editor keeps its own behaviour. The key is the
//! lexer's identity together with a digest of the bytes that were lexed, so a
//! different lexer or different bytes never share an entry.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use mlua::{Function, Table, Value};

/// The number of entries the cache keeps.
///
/// The cache only needs to cover the windows that are visible, so a small
/// number of entries is enough. The limit keeps a long editing session from
/// growing the cache without bound.
const MAX_ENTRIES: usize = 16;

/// A hash of the lexed bytes.
///
/// The bytes are hashed rather than kept, so the cache does not duplicate the
/// document. A collision would show a stale highlight, so the hash covers the
/// whole input together with its length.
fn digest(bytes: &[u8]) -> u64 {
    // FNV-1a, with the length mixed in so that a prefix and a longer input
    // cannot share a digest.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^ (bytes.len() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// The cached result of one lexing run.
struct Entry {
    /// The digest of the lexed bytes.
    digest: u64,
    /// The length of the lexed bytes, which distinguishes inputs of the same
    /// hash.
    length: usize,
    /// The tags the lexer returned.
    tags: Value,
}

/// The lexing results of the recent past.
#[derive(Default)]
pub struct LexCache {
    entries: RefCell<HashMap<u64, Entry>>,
    /// The identity given to the next lexer.
    next_identity: Cell<u64>,
    /// How often a lookup found a cached result.
    hits: Cell<u64>,
    /// How often a lookup found nothing.
    misses: Cell<u64>,
}

impl LexCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of lookups that found a cached result.
    pub fn hits(&self) -> u64 {
        self.hits.get()
    }

    /// The number of lookups that found nothing.
    pub fn misses(&self) -> u64 {
        self.misses.get()
    }

    /// The identity of the next lexer.
    ///
    /// Every lexer gets its own identity, so the entries of two lexers never
    /// share a slot.
    pub fn next_identity(&self) -> u64 {
        let identity = self.next_identity.get() + 1;
        self.next_identity.set(identity);
        identity
    }

    /// Look up the result of lexing `bytes` with the lexer `identity`.
    pub fn get(&self, identity: u64, digest: u64, length: usize) -> Option<Value> {
        let entries = self.entries.borrow();
        let Some(entry) = entries.get(&identity) else {
            // The lexer was not seen before.
            self.misses.set(self.misses.get() + 1);
            return None;
        };
        let found = entry.digest == digest && entry.length == length;
        if found {
            self.hits.set(self.hits.get() + 1);
        } else {
            self.misses.set(self.misses.get() + 1);
        }
        found.then(|| entry.tags.clone())
    }

    /// Remember the result of lexing `bytes` with the lexer `identity`.
    pub fn insert(&self, identity: u64, digest: u64, length: usize, tags: Value) {
        let mut entries = self.entries.borrow_mut();
        if entries.len() >= MAX_ENTRIES && !entries.contains_key(&identity) {
            // The cache is full. Drop an entry so the newest result is always
            // kept; the cache only exists to serve repeated frames.
            if let Some(oldest) = entries.keys().next().copied() {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            identity,
            Entry {
                digest,
                length,
                tags,
            },
        );
    }
}

/// Install the cache in front of the lexers `vis.lexers` exposes.
///
/// The `vis` module creates the lexers, and the module is loaded by the
/// configuration. The cache therefore sits in front of `require` and wraps
/// the lexers as soon as the module appears.
///
/// A failure to install the cache leaves the lexers untouched, so the editor
/// still highlights correctly, only without the speed-up.
pub fn install(lua: &mlua::Lua, cache: std::rc::Rc<LexCache>) -> mlua::Result<()> {
    let require: Function = lua.globals().get("require")?;
    let wrapped = lua.create_function({
        let cache = std::rc::Rc::clone(&cache);
        move |lua, name: mlua::String| {
            // The original `require` returns the module, or a nil and an
            // error message. Every value is passed on, so a caller that reads
            // the error still sees it.
            let results: mlua::MultiValue =
                require.call::<mlua::MultiValue>(name.to_string_lossy().to_string())?;
            if name.to_string_lossy() == "vis" {
                // The module loaded. Only the cache failed to install.
                let _ = wrap_lexers(lua, &cache);
            }
            Ok(results)
        }
    })?;
    lua.globals().set("require", wrapped)?;
    Ok(())
}

/// Wrap the lexers the `vis` module exposes.
fn wrap_lexers(lua: &mlua::Lua, cache: &std::rc::Rc<LexCache>) -> mlua::Result<()> {
    // The `vis` module keeps its tables in the userdata's user value, so the
    // lexers are reached through Lua's own indexing.
    let lexers: Table = lua
        .load("return vis.lexers")
        .set_name("@vis lexers")
        .eval()
        .map_err(|error| mlua::Error::external(format!("cannot read vis.lexers: {error}")))?;
    let load: Function = lexers.get("load")?;

    // Wrap the loader, so every lexer it returns carries the cached `lex`.
    //
    // The loader is the one the `vis` module installed, which keeps the
    // lexers it cached. Wrapping it again keeps both: the `vis` module still
    // sees the loader it installed, and the lexer it returns carries the
    // cached `lex`.
    let wrapped = lua.create_function({
        let cache = std::rc::Rc::clone(cache);
        move |lua, (name, alt_name, use_cache): (String, Option<String>, Option<bool>)| {
            let lexer: Value = load.call((name.clone(), alt_name.clone(), use_cache))?;
            if let Value::Table(table) = &lexer {
                wrap_lexer(lua, table, &cache)?;
            }
            Ok(lexer)
        }
    })?;
    lexers.set("load", wrapped)?;
    Ok(())
}

/// Wrap one lexer's `lex` method with the cache.
///
/// A lexer table is returned by the loader on every frame, so the table is
/// marked once and never wrapped twice. Wrapping it again would nest the
/// wrappers, and every frame would add another layer.
fn wrap_lexer(lua: &mlua::Lua, lexer: &Table, cache: &std::rc::Rc<LexCache>) -> mlua::Result<()> {
    // The marker is kept in the lexer table itself, next to the method.
    let wrapped: bool = lexer.get("__vis_lex_cached")?;
    if wrapped {
        return Ok(());
    }
    let lex: Value = lexer.get("lex")?;
    let Value::Function(lex) = lex else {
        return Ok(());
    };
    // The identity of the lexer separates the cache entries of different
    // lexers. A number is used rather than the table's address, so a lexer
    // that is released and a new one that takes its place cannot share an
    // entry.
    let identity = match lexer.get::<Option<u64>>("__vis_lex_identity")? {
        Some(identity) => identity,
        None => {
            let identity = cache.next_identity();
            lexer.set("__vis_lex_identity", identity)?;
            identity
        }
    };
    let cache = std::rc::Rc::clone(cache);
    // The `lex` method of a lexer is the module's function, and the lexer
    // table is passed as its first argument. The wrapper therefore receives
    // the table, the text and the initial style, and passes them on.
    //
    // The text is a byte string in the native editor, and a document may hold
    // bytes that are not valid UTF-8. The wrapper therefore takes the text as
    // a value and passes the bytes on unchanged, so a binary document is lexed
    // the way the native editor lexes it.
    let wrapped = lua.create_function(
        move |_, (table, text, style): (Table, Value, Option<Value>)| {
            let bytes = match &text {
                Value::String(bytes) => bytes.as_bytes().to_vec(),
                value => {
                    return Err(mlua::Error::FromLuaConversionError {
                        from: value.type_name(),
                        to: "string".to_owned(),
                        message: Some("the text to lex must be a string".to_owned()),
                    });
                }
            };
            let digest = digest(&bytes);
            let length = bytes.len();
            if let Some(tags) = cache.get(identity, digest, length) {
                return Ok(tags);
            }
            let tags: Value = lex.call((table, text, style))?;
            cache.insert(identity, digest, length, tags.clone());
            Ok(tags)
        },
    )?;
    lexer.set("lex", wrapped)?;
    lexer.set("__vis_lex_cached", true)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// The repository root, found from the source file.
    fn root() -> PathBuf {
        let source = PathBuf::from(file!());
        let source = if source.is_absolute() {
            source
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(source)
        };
        source
            .parent()
            .unwrap()
            .join("../../..")
            .canonicalize()
            .unwrap()
    }

    /// A runtime with the `vis` module loadable.
    ///
    /// The runtime installs the `vis` global and the search paths, so the
    /// module loads the way it loads in the editor. It also installs the
    /// highlighting cache, so this is the cached path.
    fn runtime(root: &Path) -> crate::lua::Runtime {
        let directory = root.join("lua");
        crate::lua::Runtime::new(std::slice::from_ref(&directory)).unwrap()
    }

    /// A runtime whose lexers are not cached.
    ///
    /// The cache is what the editor installs, so this runtime reaches the
    /// matcher directly and serves as the reference.
    fn runtime_without_cache(root: &Path) -> crate::lua::Runtime {
        let directory = root.join("lua");
        crate::lua::Runtime::new_without_lex_cache(std::slice::from_ref(&directory)).unwrap()
    }

    /// The tags a lexer produces, read through a file so the value crosses
    /// the runtime's boundary unchanged.
    fn tags_of(runtime: &crate::lua::Runtime, lexer: &str, text: &str, path: &Path) -> String {
        let script = format!(
            r#"
            local lexers = require('lexer')
            local lexer = lexers.load({lexer:?}, nil, true)
            local tags = lexer:lex({text:?}, 1)
            local out = assert(io.open({path:?}, 'w'))
            out:write(table.concat(tags, ','))
            out:close()
            "#,
        );
        runtime.eval(&script).unwrap();
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn the_cached_tags_are_the_same_as_the_uncached_tags() {
        let root = root();
        let mut names: Vec<String> = std::fs::read_dir(root.join("lua/lexers"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "lua"))
            .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
            .filter(|name| name != "lexer")
            .collect();
        names.sort();

        let samples = [
            "local x = 1\nprint(x)\n",
            "function f(a, b) return a + b end\n",
            "# comment\nname = 'value'\n",
            "",
            "plain text without any syntax\n",
        ];
        let output = std::env::temp_dir().join(format!("vis-lexcache-{}", std::process::id()));

        // Every bundled lexer is checked, so a cache that returned the wrong
        // tags for one of them is caught.
        for name in &names {
            // The runtime installs the cache itself, so the cached path is the
            // one the editor uses.
            let cached = runtime(&root);
            let uncached = runtime_without_cache(&root);

            for sample in &samples {
                let expected = tags_of(&uncached, name, sample, &output);
                let actual = tags_of(&cached, name, sample, &output);
                assert_eq!(
                    expected, actual,
                    "the cached tags differ for {name} on {sample:?}"
                );
                // A second run must return the same result, now from the
                // cache.
                let again = tags_of(&cached, name, sample, &output);
                assert_eq!(expected, again, "the cached tags changed for {name}");
            }
        }
        let _ = std::fs::remove_file(&output);
    }

    #[test]
    fn the_cache_serves_a_repeated_input_and_a_changed_one_separately() {
        let root = root();
        let runtime = runtime(&root);
        let output = std::env::temp_dir().join(format!(
            "vis-lexcache-repeat-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("repeat")
        ));

        let first = tags_of(&runtime, "lua", "local x = 1\n", &output);
        let misses = runtime.lex_cache_misses();
        let hits = runtime.lex_cache_hits();
        let second = tags_of(&runtime, "lua", "local x = 1\n", &output);
        assert_eq!(first, second);
        // The second run was served by the cache.
        assert_eq!(
            runtime.lex_cache_hits(),
            hits + 1,
            "the repeated input did not hit the cache"
        );
        assert_eq!(runtime.lex_cache_misses(), misses);
        // Different bytes must not share the cached result. The tag names are
        // the same for both inputs, but the positions are not, so the result
        // differs.
        let other = tags_of(&runtime, "lua", "local y = 22\n", &output);
        assert_ne!(first, other);
        assert_eq!(runtime.lex_cache_misses(), misses + 1);
        let _ = std::fs::remove_file(&output);
    }
}
