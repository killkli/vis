//! Preloaded pure-Lua LPeg/re, keeping native C extension loading disabled.
//!
//! See `vendor/lpeg/PROVENANCE.md` for upstream identity, patches and limits.
use mlua::{
    AnyUserData, Function, Lua, MetaMethod, MultiValue, Result, Table, UserData, UserDataMethods,
    Value,
};

const IMPLEMENTATION: &str = include_str!("../../vendor/lpeg/lulpeg.lua");
const ADAPTER: &str = include_str!("../../vendor/lpeg/compat.lua");
const RE: &str = include_str!("../../vendor/lpeg/re.lua");

const BACKEND: &str = "vis.lpeg.backend";
const PUBLIC: &str = "vis.lpeg.public";
const OPERATORS: &str = "vis.lpeg.operators";

// Keep the Lua table in the userdata's Lua-visible uservalue, so the Lua GC can
// trace captures that reference patterns without hidden Rust registry cycles.
struct Pattern;

impl UserData for Pattern {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        for (method, name) in [
            (MetaMethod::Add, "__add"),
            (MetaMethod::Mul, "__mul"),
            (MetaMethod::Sub, "__sub"),
            (MetaMethod::Div, "__div"),
            (MetaMethod::Pow, "__pow"),
            (MetaMethod::Mod, "__mod"),
            (MetaMethod::Unm, "__unm"),
            (MetaMethod::Len, "__len"),
        ] {
            methods.add_meta_function(method, move |lua, args: MultiValue| {
                operate(lua, name, args)
            });
        }
        methods.add_meta_function(
            MetaMethod::Index,
            |lua, (_pattern, key): (AnyUserData, Value)| {
                lua.named_registry_value::<Table>(PUBLIC)?.get::<Value>(key)
            },
        );
    }
}

fn unwrap_pattern(lua: &Lua, value: Value, grammar: bool) -> Result<Value> {
    match value {
        Value::UserData(ref pattern) if pattern.is::<Pattern>() => {
            Ok(Value::Table(pattern.user_value::<Table>()?))
        }
        Value::Table(table) if grammar => {
            let copy = lua.create_table()?;
            for pair in table.pairs::<Value, Value>() {
                let (key, value) = pair?;
                copy.raw_set(key, unwrap_pattern(lua, value, false)?)?;
            }
            Ok(Value::Table(copy))
        }
        other => Ok(other),
    }
}

fn wrap_pattern(lua: &Lua, pattern: Table) -> Result<AnyUserData> {
    let userdata = lua.create_userdata(Pattern)?;
    userdata.set_user_value(pattern)?;
    Ok(userdata)
}

fn operate(lua: &Lua, name: &str, args: MultiValue) -> Result<AnyUserData> {
    let backend: Table = lua.named_registry_value(BACKEND)?;
    let function: Function = backend.get(name)?;
    let mut values = args.into_vec();
    if matches!(name, "__len" | "__unm") {
        values.truncate(1);
    }
    for (index, value) in values.iter_mut().enumerate() {
        let pattern = index == 0 || matches!(name, "__add" | "__mul" | "__sub");
        *value = unwrap_pattern(lua, value.clone(), pattern)?;
    }
    wrap_pattern(lua, function.call(MultiValue::from_vec(values))?)
}

fn public_module(lua: &Lua, backend: Table) -> Result<Table> {
    lua.set_named_registry_value(BACKEND, backend.clone())?;
    let module = lua.create_table()?;
    lua.set_named_registry_value(PUBLIC, module.clone())?;
    module.set("version", backend.get::<String>("version")?)?;
    for name in [
        "P", "S", "R", "V", "B", "C", "Cc", "Ct", "Cg", "Cb", "Cp", "Cmt", "Carg", "Cf", "Cs",
        "utfR",
    ] {
        let function: Function = backend.get(name)?;
        module.set(
            name,
            lua.create_function(move |lua, args: MultiValue| {
                let mut values = args.into_vec();
                if name == "P" {
                    if let Some(Value::UserData(pattern)) = values.first() {
                        if pattern.is::<Pattern>() {
                            return Ok(pattern.clone());
                        }
                    }
                }
                if matches!(name, "P" | "B" | "C" | "Ct" | "Cg" | "Cmt" | "Cf" | "Cs") {
                    if let Some(value) = values.first_mut() {
                        *value = unwrap_pattern(lua, value.clone(), true)?;
                    }
                }
                wrap_pattern(lua, function.call(MultiValue::from_vec(values))?)
            })?,
        )?;
    }
    let matched: Function = backend.get("match")?;
    module.set(
        "match",
        lua.create_function(move |lua, args: MultiValue| {
            let mut values = args.into_vec();
            if let Some(value) = values.first_mut() {
                *value = unwrap_pattern(lua, value.clone(), true)?;
            }
            matched.call::<MultiValue>(MultiValue::from_vec(values))
        })?,
    )?;
    module.set(
        "type",
        lua.create_function(|_, value: Value| {
            Ok(match value {
                Value::UserData(pattern) if pattern.is::<Pattern>() => Some("pattern"),
                _ => None,
            })
        })?,
    )?;
    module.set("setmaxstack", backend.get::<Function>("setmaxstack")?)?;
    // The bundled original LPeg release build exposes these names but returns
    // this error; the pure-Lua backend likewise has no C VM debug bytecode.
    for name in ["ptree", "pcode"] {
        module.set(
            name,
            lua.create_function(|_, _: MultiValue| -> Result<()> {
                Err(mlua::Error::runtime(
                    "function only implemented in debug mode",
                ))
            })?,
        )?;
    }
    let locale: Function = backend.get("locale")?;
    module.set(
        "locale",
        lua.create_function(move |lua, args: MultiValue| {
            let values: Table = locale.call(args)?;
            let is_pattern: Function = lua.named_registry_value::<Table>(BACKEND)?.get("type")?;
            for pair in values.clone().pairs::<Value, Value>() {
                let (key, value) = pair?;
                if is_pattern.call::<Option<String>>(value.clone())?.is_some() {
                    let Value::Table(pattern) = value else {
                        unreachable!()
                    };
                    values.raw_set(key, wrap_pattern(lua, pattern)?)?;
                }
            }
            Ok(values)
        })?,
    )?;
    let operators = lua.create_table()?;
    for name in [
        "__add", "__mul", "__sub", "__div", "__pow", "__mod", "__unm", "__len",
    ] {
        operators.set(
            name,
            lua.create_function(move |lua, args: MultiValue| operate(lua, name, args))?,
        )?;
    }
    lua.set_named_registry_value(OPERATORS, operators)?;
    Ok(module)
}

/// The name of the LPeg matcher this host uses.
///
/// The native virtual machine reports itself as `LPeg`; the pure-Lua matcher
/// reports the LuLPeg name. The value is for diagnostics only.
pub fn backend_name(lua: &Lua) -> Option<String> {
    let package: Table = lua.globals().get("package").ok()?;
    let loaded: Table = package.get("loaded").ok()?;
    let module: Table = loaded.get("lpeg").ok()?;
    module.get("version").ok()
}

/// Install the `lpeg` and `re` modules.
pub(super) fn install(lua: &Lua) -> Result<()> {
    let package: Table = lua.globals().get("package")?;
    let preload: Table = package.get("preload")?;
    install_into(lua, &preload)
}

/// Install the `lpeg` and `re` modules into an existing preload table.
pub(super) fn install_into(lua: &Lua, preload: &Table) -> Result<()> {
    // The native virtual machine is much faster than the pure-Lua matcher, and
    // the syntax highlighter runs on every frame. When the system provides a
    // native LPeg, use it; otherwise the pure-Lua matcher stays in place.
    #[cfg(feature = "native-lpeg")]
    if let Some(loaded) = install_native(lua)? {
        return install_re(lua, preload, Some(loaded));
    }
    preload.set(
        "lpeg",
        lua.create_function(|lua, ()| {
            let module: Table = lua
                .load(IMPLEMENTATION)
                .set_name("@vendored/lpeg/lulpeg.lua")
                .eval()?;
            let module: Table = lua
                .load(ADAPTER)
                .set_name("@vendored/lpeg/compat.lua")
                .call(module)?;
            let module = public_module(lua, module)?;
            let package: Table = lua.globals().get("package")?;
            let loaded: Table = package.get("loaded")?;
            // Official re.lua requires lpeg while its preload is still running.
            loaded.set("lpeg", module.clone())?;
            let environment = lua.create_table()?;
            let fallback = lua.create_table()?;
            fallback.set("__index", lua.globals())?;
            environment.set_metatable(Some(fallback))?;
            let original_getmetatable: Function = lua.globals().get("getmetatable")?;
            environment.set(
                "getmetatable",
                lua.create_function(move |lua, value: Value| {
                    if matches!(&value, Value::UserData(pattern) if pattern.is::<Pattern>()) {
                        return Ok(Value::Table(lua.named_registry_value(OPERATORS)?));
                    }
                    original_getmetatable.call::<Value>(value)
                })?,
            )?;
            let re = lua
                .load(RE)
                .set_name("@vendored/lpeg/re.lua")
                .set_environment(environment)
                .eval::<Table>();
            match re {
                Ok(re) => module.set("re", re)?,
                Err(error) => {
                    loaded.raw_remove("lpeg")?;
                    return Err(error);
                }
            }
            Ok(module)
        })?,
    )?;
    install_re(lua, preload, None)
}

/// Install the `re` module next to an already installed `lpeg`.
///
/// The official `re.lua` needs the `lpeg` module while its own preload is
/// still running, so the module is published before `re.lua` is evaluated.
fn install_re(lua: &Lua, preload: &Table, module: Option<Table>) -> Result<()> {
    if let Some(module) = module {
        let package: Table = lua.globals().get("package")?;
        let loaded: Table = package.get("loaded")?;
        loaded.set("lpeg", module.clone())?;
        let environment = lua.create_table()?;
        let fallback = lua.create_table()?;
        fallback.set("__index", lua.globals())?;
        environment.set_metatable(Some(fallback))?;
        let original_getmetatable: Function = lua.globals().get("getmetatable")?;
        environment.set(
            "getmetatable",
            lua.create_function(move |lua, value: Value| {
                if matches!(&value, Value::UserData(pattern) if pattern.is::<Pattern>()) {
                    return Ok(Value::Table(lua.named_registry_value(OPERATORS)?));
                }
                original_getmetatable.call::<Value>(value)
            })?,
        )?;
        let re = lua
            .load(RE)
            .set_name("@vendored/lpeg/re.lua")
            .set_environment(environment)
            .eval::<Table>();
        match re {
            Ok(re) => module.set("re", re)?,
            Err(error) => {
                let package: Table = lua.globals().get("package")?;
                let loaded: Table = package.get("loaded")?;
                loaded.raw_remove("lpeg")?;
                return Err(error);
            }
        }
    }
    preload.set(
        "re",
        lua.create_function(|lua, ()| {
            let require: Function = lua.globals().get("require")?;
            let lpeg: Table = require.call("lpeg")?;
            lpeg.get::<Table>("re")
        })?,
    )?;
    Ok(())
}

/// Load the native LPeg library and register it as the `lpeg` module.
///
/// Returns the native module table, or `None` when no native library is
/// available and the pure-Lua matcher must be used instead.
#[cfg(feature = "native-lpeg")]
fn install_native(lua: &Lua) -> Result<Option<Table>> {
    let package: Table = lua.globals().get("package")?;
    let loaded: Table = package.get("loaded")?;
    // A native library already in `package.loaded` was accepted before.
    if let Ok(module) = loaded.get::<Table>("lpeg") {
        return Ok(Some(module));
    }
    match vis_lpeg_native::NativeLpeg::load(lua, "lpeg") {
        Ok(native) => {
            // The handle never releases the library, so the module table stays
            // valid for the lifetime of the interpreter.
            let _ = native;
            // The loader registered the module as a preload, so requiring it
            // now yields the native module table.
            let require: mlua::Function = lua.globals().get("require")?;
            let module = require.call::<Table>("lpeg")?;
            Ok(Some(module))
        }
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lua() -> Lua {
        let lua = Lua::new();
        install(&lua).unwrap();
        lua
    }

    #[test]
    fn safe_lua55_loads_lpeg_and_re_without_native_extensions() {
        lua()
            .load(
                r#"
            assert(_VERSION == 'Lua 5.5')
            local lpeg, re = require('lpeg'), require('re')
            assert(lpeg == require('lpeg') and re == lpeg.re)
            assert(type(lpeg.version) == 'string')
            assert(lpeg.type(lpeg.P('a')) == 'pattern')
            assert(type(lpeg.P('a')) == 'userdata')
            assert(lpeg.match(lpeg.C(lpeg.P('a')^1), 'aaa') == 'aaa')
            assert(re.match('hello world', "{'hello'} ' ' {'world'}") == 'hello')
            local ok, message = pcall(package.loadlib, '/native-module.so', 'luaopen_lpeg')
            assert(not ok and tostring(message):find('disabled in safe mode', 1, true))
            assert(package.searchers[3]('native-module'):find("can't load C modules in safe mode", 1, true))
        "#,
            )
            .exec()
            .unwrap();
    }

    #[test]
    fn captures_grammars_matchtime_lookbehind_and_accumulation() {
        lua().load(r#"
            local l = require('lpeg')
            local P,R,S,V,C,Cp,Cc,Cg,Cb,Ct,Cs,Cf,Cmt,Carg = l.P,l.R,l.S,l.V,l.C,l.Cp,l.Cc,l.Cg,l.Cb,l.Ct,l.Cs,l.Cf,l.Cmt,l.Carg
            local numbers = C(R('09')^1) / tonumber
            assert((numbers * (',' * numbers % function(a,b) return a+b end)^0):match('1,2,3') == 6)
            assert((Cf(numbers * (',' * numbers)^0,function(a,b) return a+b end)):match('1,2,3') == 6)
            assert(Cs((P('foo') / 'bar' + 1)^0):match('foo foo') == 'bar bar')
            local a,b = (Cg(C('abc'),'name') * Cb('name') * Carg(1)):match('abc',1,'extra')
            assert(a == 'abc' and b == 'extra')
            local p = P{ C('abc') + 1*V(1) }
            assert(p:match('___abc') == 'abc')
            assert((P(1)*l.B('x')*Cp()):match('x') == 2)
            assert(Cmt(C('x'),function(s,p,c) assert(c=='x'); return p,'value' end):match('x') == 'value')
            local t = Ct(Cg(C('x'),'named') * Cc(false,nil,3)):match('x')
            assert(t.named=='x' and t[1]==false and t[2]==nil and t[3]==3)
        "#).exec().unwrap();
    }

    #[test]
    fn utf8_ranges_are_binary_safe_and_reject_overlong_sequences() {
        lua()
            .load(
                r#"
            local l = require('lpeg')
            assert(l.utfR(0x80,0x10ffff):match('♪') == 4)
            assert(l.utfR(0x80,0x10ffff):match('\192\128') == nil)
            assert(l.utfR(0x80,0x10ffff):match('\244\144\128\128') == nil)
            assert(l.utfR(0,0):match('\0') == 2)
            assert(l.utfR(0x80,0x10ffff):match('\255') == nil)
            assert(not pcall(l.utfR,9,1))
        "#,
            )
            .exec()
            .unwrap();
    }

    #[test]
    fn invalid_grammars_and_empty_loops_fail_without_hanging() {
        lua()
            .load(
                r#"
            local l = require('lpeg')
            assert(not pcall(function() return l.P('')^0 end))
            assert(not pcall(l.P, {l.V(1) * 'a'}))
            assert(not pcall(l.P, {l.V('missing')}))
            assert(not pcall(l.P, {l.V(2)^0,[2]=l.P('')}))
            assert(not pcall(l.setmaxstack,0))
            l.setmaxstack(10)
            -- The native matcher bounds the stack by its own rule, so a
            -- recursive pattern may still match; the pure-Lua matcher
            -- rejects it outright. Either way the program must not hang.
            local p = l.P{ 'x' * l.V(1) + -1 }
            pcall(p.match,p,string.rep('x',20))
            -- The native matcher rejects an empty loop body when the pattern
            -- is built; the pure-Lua matcher accepts the pattern and rejects
            -- the match. Either way the program must fail rather than hang.
            local built, dynamic = pcall(function() return l.Cmt('', function(s,p) return p end)^0 end)
            if built then
                assert(not pcall(dynamic.match,dynamic,''))
            else
                assert(not built)
            end
        "#,
            )
            .exec()
            .unwrap();
    }
    #[test]
    fn all_bundled_lexers_work_in_safe_lua55() {
        let source = std::path::Path::new(file!());
        let source = if source.is_absolute() {
            source.to_path_buf()
        } else {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(source)
        };
        let root = source
            .parent()
            .unwrap()
            .join("../../../lua")
            .canonicalize()
            .unwrap();
        let lua = lua();
        let package: Table = lua.globals().get("package").unwrap();
        package
            .set(
                "path",
                format!("{}/?.lua;{}/lexers/?.lua", root.display(), root.display()),
            )
            .unwrap();
        let lexer: Table = lua.load("return require('lexer')").eval().unwrap();
        let load: Function = lexer.get("load").unwrap();
        let mut names: Vec<_> = std::fs::read_dir(root.join("lexers"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "lua"))
            .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
            .filter(|name| name != "lexer")
            .collect();
        names.sort();
        assert!(names.len() >= 160);
        for name in names {
            let lex: Table = load
                .call(name.as_str())
                .unwrap_or_else(|error| panic!("load lexer {name}: {error}"));
            let lex_function: Function = lex.get("lex").unwrap();
            let tags: Table = lex_function
                .call((
                    lex,
                    "function foo(x) { return x + 12; } // comment\n'quoted' \"double\"\n",
                ))
                .unwrap_or_else(|error| panic!("lex {name}: {error}"));
            assert!(tags.raw_len() > 0, "lexer {name} produced no tags");
        }
    }
    #[test]
    fn ordered_choice_false_captures_and_grammar_scopes_match_native_lpeg() {
        lua()
            .load(
                r#"
            local l = require('lpeg')
            local pattern = l.P('a')
            assert(l.P(pattern) == pattern)
            assert((l.C('a')/{a=false}):match('a') == false)
            assert(((l.S('ab')^1) + (l.S('cd')^1)):match('ac') == 2)
            assert((-l.P('a') + -l.P('b')):match('a') == 1)
            local ref = l.C(l.V('rule'))
            local first, second = l.P{ref,rule=l.P('a')}, l.P{ref,rule=l.P('b')}
            assert(first:match('a')=='a' and second:match('b')=='b')
            assert(not pcall(function() return l.Cmt('a',function() return 1 end):match('a') end))
            local lexer = require('lpeg') -- pattern type must satisfy legacy lexer.range
            assert(type(lexer.P(1)) == 'userdata')
        "#,
            )
            .exec()
            .unwrap();
    }

    #[test]
    #[ignore = "run through vendor/lpeg/differential.py with VIS_LPEG_OUTPUT"]
    fn external_differential_corpus() {
        let output = std::env::var_os("VIS_LPEG_OUTPUT").expect("VIS_LPEG_OUTPUT required");
        let source = std::path::Path::new(file!());
        let source = if source.is_absolute() {
            source.to_path_buf()
        } else {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(source)
        };
        let root = source
            .parent()
            .unwrap()
            .join("../../..")
            .canonicalize()
            .unwrap();
        let vendor = root.join("rust/vendor/lpeg");
        let mut names: Vec<_> = std::fs::read_dir(root.join("lua/lexers"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "lua"))
            .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
            .filter(|name| name != "lexer")
            .collect();
        names.sort();
        let mut args = vec![
            "embedded".to_string(),
            vendor.to_string_lossy().into_owned(),
            root.to_string_lossy().into_owned(),
        ];
        args.extend(names);
        let lua = lua();
        lua.globals()
            .set("arg", lua.create_sequence_from(args).unwrap())
            .unwrap();
        let lines = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = lines.clone();
        lua.globals()
            .set(
                "_vis_lpeg_emit",
                lua.create_function(move |_, line: String| {
                    captured.borrow_mut().push(line);
                    Ok(())
                })
                .unwrap(),
            )
            .unwrap();
        lua.load(include_str!("../../vendor/lpeg/differential.lua"))
            .exec()
            .unwrap();
        std::fs::write(output, lines.borrow().join("\n")).unwrap();
    }
}
