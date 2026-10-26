//! Safe, synchronous Lua host. Lua callbacks never run with a workspace borrow.
//!
//! Userdata keeps weak runtime references; the runtime owns the interpreter and
//! the editor. Stable host IDs are checked on every operation.
mod lpeg;
mod objects;
#[cfg(unix)]
mod process;

use crate::editor::{
    CallbackKind, CallbackRequest, CallbackResponse, CallbackType, EditorAction, Key, Mode,
};
use crate::style::{DrawOperation, Style};
use crate::workspace::{Outcome, Workspace};
use mlua::{
    AnyUserData, Function, HookTriggers, Lua, LuaOptions, MetaMethod, MultiValue, ObjectLike,
    Result, StdLib, Table, UserData, UserDataMethods, Value, VmState,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(crate) type Context = Weak<Inner>;

pub(crate) fn upgrade(context: &Context) -> Result<Rc<Inner>> {
    context
        .upgrade()
        .ok_or_else(|| mlua::Error::runtime("editor has closed"))
}

#[derive(Clone)]
struct OptionDefinition {
    kind: OptionKind,
    set: Function,
    get: Option<Function>,
}
#[derive(Clone, Copy)]
enum OptionKind {
    Bool,
    String,
    Number,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OptionScope {
    Global,
    Window,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OptionTarget {
    Global,
    Window(usize),
    Command,
}

#[derive(Default)]
struct DrawRecording {
    base: Option<Style>,
    whitespace: Option<Style>,
    operations: Vec<DrawOperation>,
}

pub type ProcessObserver = Rc<dyn Fn(bool) -> std::io::Result<()>>;
pub type ProcessGroupObserver = Arc<dyn Fn(Option<u32>) -> std::io::Result<()> + Send + Sync>;

pub(crate) struct Inner {
    pub(crate) lua: Lua,
    pub(crate) workspace: Rc<RefCell<Workspace>>,
    pub(crate) types: Table,
    pub(crate) object_cache: RefCell<HashMap<String, AnyUserData>>,
    pub(crate) object_state: RefCell<objects::ObjectState>,
    options: RefCell<HashMap<String, OptionDefinition>>,
    commands: RefCell<HashMap<String, Function>>,
    callbacks: RefCell<HashMap<u64, Function>>,
    next_callback: Cell<u64>,
    style_count: Cell<usize>,
    draw_records: RefCell<HashMap<usize, DrawRecording>>,
    messages: RefCell<Vec<String>>,
    lua_errors: RefCell<Vec<String>>,
    reporting_lua_error: Cell<bool>,
    outcome: RefCell<Outcome>,
    exit_code: Cell<Option<i32>>,
    quitting: Cell<bool>,
    process_observer: RefCell<Option<ProcessObserver>>,
    process_group_observer: RefCell<Option<ProcessGroupObserver>>,
    callback_help: RefCell<HashMap<u64, String>>,
    interrupt: Arc<AtomicBool>,
    #[cfg(unix)]
    processes: RefCell<process::Manager>,
}

/// Native vis opens the whole `debug` library. This port keeps the unsafe
/// libraries closed and installs a curated subset instead: `debug.getinfo` and
/// `debug.traceback` are served by the interpreter's own stack inspection, so
/// configuration that reports its own source location keeps working.
fn install_debug_library(lua: &Lua) -> Result<()> {
    let table = lua.create_table()?;
    let getinfo = lua.create_function(|lua, (level, what): (Value, Option<String>)| {
        let level = match level {
            Value::Integer(number) if number >= 0 => number as usize,
            _ => return Ok(Value::Nil),
        };
        let what = what.unwrap_or_else(|| "flnStu".to_string());
        let Some(frames) = lua.inspect_stack(level, |debug| {
            let source = debug.source();
            let names = debug.names();
            (
                source.source.map(|value| value.into_owned()),
                source.short_src.map(|value| value.into_owned()),
                source.line_defined,
                source.last_line_defined,
                source.what,
                names.name.map(|value| value.into_owned()),
                names.name_what,
                debug.current_line(),
                debug.function(),
                debug.stack(),
            )
        }) else {
            return Ok(Value::Nil);
        };
        let (
            source,
            short_src,
            line_defined,
            last_line_defined,
            source_what,
            name,
            name_what,
            current_line,
            function,
            stack,
        ) = frames;
        let info = lua.create_table()?;
        if what.contains('S') {
            if let Some(value) = source {
                info.set("source", value)?;
            }
            if let Some(value) = short_src {
                info.set("short_src", value)?;
            }
            if let Some(value) = line_defined {
                info.set("linedefined", value)?;
            }
            if let Some(value) = last_line_defined {
                info.set("lastlinedefined", value)?;
            }
            info.set("what", source_what)?;
        }
        if what.contains('l') {
            if let Some(value) = current_line {
                info.set("currentline", value)?;
            }
        }
        if what.contains('n') {
            if let Some(value) = name {
                info.set("name", value)?;
            }
            if let Some(value) = name_what {
                info.set("namewhat", value)?;
            }
        }
        if what.contains('f') {
            info.set("func", function)?;
        }
        if what.contains('u') {
            let upvalues = lua.create_table()?;
            upvalues.set("nups", stack.num_ups)?;
            upvalues.set("nparams", stack.num_params)?;
            upvalues.set("isvararg", stack.is_vararg)?;
            info.set("nups", stack.num_ups)?;
            info.set("nparams", stack.num_params)?;
            info.set("isvararg", stack.is_vararg)?;
        }
        Ok(Value::Table(info))
    })?;
    table.set("getinfo", getinfo)?;
    let traceback =
        lua.create_function(|lua, (message, level): (Option<String>, Option<usize>)| {
            lua.traceback(message.as_deref(), level.unwrap_or(1))
                .map(Value::String)
        })?;
    table.set("traceback", traceback)?;
    lua.globals().set("debug", table)?;
    Ok(())
}

/// An optional Lua compatibility host around the same editor and workspace core.
#[derive(Clone)]
pub struct Runtime {
    inner: Rc<Inner>,
}

impl Runtime {
    /// Initialize API and configuration, then emit INIT before opening any file.
    /// Additional search directories have highest precedence, in supplied order.
    pub fn new(search_paths: &[PathBuf]) -> Result<Self> {
        Self::new_with_interrupt(search_paths, Arc::new(AtomicBool::new(false)))
    }
    /// Install cancellation before loading configuration or dispatching INIT.
    pub fn new_with_interrupt(
        search_paths: &[PathBuf],
        interrupt: Arc<AtomicBool>,
    ) -> Result<Self> {
        Self::initialize(search_paths, interrupt, None, None)
    }
    /// Host process hooks are available during configuration and INIT as well
    /// as during the interactive loop.
    pub fn new_with_observers(
        search_paths: &[PathBuf],
        interrupt: Arc<AtomicBool>,
        before_after: ProcessObserver,
        groups: ProcessGroupObserver,
    ) -> Result<Self> {
        Self::initialize(search_paths, interrupt, Some(before_after), Some(groups))
    }
    fn initialize(
        search_paths: &[PathBuf],
        interrupt: Arc<AtomicBool>,
        process_observer: Option<ProcessObserver>,
        process_group_observer: Option<ProcessGroupObserver>,
    ) -> Result<Self> {
        // Native vis opens every library, but mlua only exposes `debug` and
        // `ffi` through its unsafe constructor, which this crate forbids.
        // Configuration needing `debug.getinfo` therefore fails; see
        // LUA_PORTING.md.
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;
        install_debug_library(&lua)?;
        let hook_interrupt = Arc::clone(&interrupt);
        lua.set_global_hook(
            HookTriggers::new().every_nth_instruction(10_000),
            move |_, _| {
                if hook_interrupt.swap(false, Ordering::Relaxed) {
                    Err(mlua::Error::runtime("Lua execution interrupted"))
                } else {
                    Ok(VmState::Continue)
                }
            },
        )?;
        let types = lua.create_table()?;
        for name in [
            "vis",
            "file",
            "window",
            "selection",
            "text",
            "window_options",
            "window_selections",
            "window_marks",
            "keyaction",
            "visoptions",
        ] {
            types.set(name, lua.create_table()?)?;
        }
        let inner = Rc::new(Inner {
            lua,
            types,
            workspace: Rc::new(RefCell::new(
                Workspace::empty().map_err(mlua::Error::external)?,
            )),
            object_cache: RefCell::new(HashMap::new()),
            object_state: RefCell::new(objects::ObjectState::default()),
            options: RefCell::new(HashMap::new()),
            commands: RefCell::new(HashMap::new()),
            callbacks: RefCell::new(HashMap::new()),
            next_callback: Cell::new(1),
            style_count: Cell::new(14),
            draw_records: RefCell::new(HashMap::new()),
            messages: RefCell::new(Vec::new()),
            lua_errors: RefCell::new(Vec::new()),
            reporting_lua_error: Cell::new(false),
            outcome: RefCell::new(Outcome::Continue),
            exit_code: Cell::new(None),
            quitting: Cell::new(false),
            process_observer: RefCell::new(process_observer),
            process_group_observer: RefCell::new(process_group_observer),
            callback_help: RefCell::new(HashMap::new()),
            interrupt,
            #[cfg(unix)]
            processes: RefCell::new(process::Manager::default()),
        });
        inner
            .workspace
            .borrow_mut()
            .editor
            .set_input_interception(true);
        #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
        inner
            .workspace
            .borrow_mut()
            .editor
            .set_interrupt(Arc::clone(&inner.interrupt));
        #[cfg(unix)]
        inner
            .processes
            .borrow_mut()
            .set_interrupt(Arc::clone(&inner.interrupt));
        #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
        if let Some(observer) = inner.process_group_observer.borrow().as_ref() {
            inner
                .workspace
                .borrow_mut()
                .editor
                .set_process_group_observer(Arc::clone(observer));
        }
        inner.install(search_paths)?;
        if let Err(error) = inner
            .lua
            .load("require('visrc')")
            .set_name("vis startup")
            .exec()
        {
            inner.report_lua_error(error);
        }
        if let Err(error) = inner.emit("init", ()) {
            inner.report_lua_error(error);
        }
        Ok(Self { inner })
    }
    pub fn set_process_observer(&self, observer: impl Fn(bool) -> std::io::Result<()> + 'static) {
        *self.inner.process_observer.borrow_mut() = Some(Rc::new(observer));
    }
    pub fn set_process_group_observer(
        &self,
        observer: impl Fn(Option<u32>) -> std::io::Result<()> + Send + Sync + 'static,
    ) {
        let observer: ProcessGroupObserver = Arc::new(observer);
        #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
        self.inner
            .workspace
            .borrow_mut()
            .editor
            .set_process_group_observer(Arc::clone(&observer));
        *self.inner.process_group_observer.borrow_mut() = Some(observer);
    }
    pub fn lua(&self) -> &Lua {
        &self.inner.lua
    }
    pub fn workspace(&self) -> Rc<RefCell<Workspace>> {
        self.inner.workspace.clone()
    }
    pub fn eval(&self, source: &str) -> Result<()> {
        self.inner.lua.load(source).exec()
    }
    /// Match C's parser/transcript acceptance result. Handler failures are
    /// reported separately and can still return true.
    pub fn command(&self, command: &str) -> Result<bool> {
        self.inner.command(command)
    }
    pub fn feedkeys(&self, keys: &str) -> Result<()> {
        self.inner.feedkeys(keys)
    }
    pub fn poll_processes(&self) -> Result<()> {
        self.inner.poll_processes()
    }
    pub fn has_processes(&self) -> bool {
        #[cfg(unix)]
        {
            self.inner.processes.borrow().pending()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
    pub fn take_lua_errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.inner.lua_errors.borrow_mut())
    }
    pub fn take_messages(&self) -> Vec<String> {
        std::mem::take(&mut *self.inner.messages.borrow_mut())
    }
    pub fn exit_code(&self) -> Option<i32> {
        self.inner.exit_code.get()
    }
    pub fn take_outcome(&self) -> Outcome {
        std::mem::replace(&mut *self.inner.outcome.borrow_mut(), Outcome::Continue)
    }
    pub fn start(&self) -> Result<()> {
        self.inner.notify("start", ());
        Ok(())
    }
    pub fn shutdown(&self) -> Result<()> {
        if self.inner.quitting.replace(true) {
            return Ok(());
        }
        self.inner.notify("quit", ());
        #[cfg(unix)]
        self.inner.processes.borrow_mut().cancel_all();
        loop {
            let id = { self.inner.workspace.borrow().window_ids().first().copied() };
            let Some(id) = id else { break };
            self.inner.close_window(id, true)?;
        }
        #[cfg(unix)]
        self.inner.processes.borrow_mut().cancel_all();
        Ok(())
    }
}

#[derive(Clone)]
struct GlobalOptions(Context);
impl UserData for GlobalOptions {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function(MetaMethod::Index, |_, (ud, key): (AnyUserData, Value)| {
            let context = ud.borrow::<GlobalOptions>()?.0.clone();
            let inner = upgrade(&context)?;
            if let Value::String(name) = &key {
                if let Ok(name) = name.to_str() {
                    if inner
                        .resolve_option_target(&name, OptionTarget::Global)
                        .is_some()
                    {
                        return inner.get_option(&name, OptionTarget::Global);
                    }
                }
            }
            let methods: Table = inner.types.get("visoptions")?;
            let native: Value = methods.get(key.clone())?;
            if !native.is_nil() {
                return Ok(native);
            }
            ud.user_value::<Table>()?.get(key)
        });
        methods.add_meta_function(
            MetaMethod::NewIndex,
            |_, (ud, key, value): (AnyUserData, Value, Value)| {
                let context = ud.borrow::<GlobalOptions>()?.0.clone();
                let inner = upgrade(&context)?;
                if let Value::String(name) = &key {
                    if let Ok(name) = name.to_str() {
                        if inner.option_set(&name, value.clone(), None)? {
                            return Ok(());
                        }
                    }
                }
                ud.user_value::<Table>()?.set(key, value)
            },
        );
    }
}

#[derive(Clone)]
struct KeyAction {
    context: Context,
    id: u64,
}
impl UserData for KeyAction {}
#[derive(Clone)]
struct Vis(Context);
impl UserData for Vis {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function(
            MetaMethod::Index,
            |lua, (ud, key): (AnyUserData, String)| {
                let context = ud.borrow::<Vis>()?.0.clone();
                let inner = upgrade(&context)?;
                inner.vis_get(lua, &ud, &key)
            },
        );
        methods.add_meta_function(
            MetaMethod::NewIndex,
            |_, (ud, key, value): (AnyUserData, String, Value)| {
                let context = ud.borrow::<Vis>()?.0.clone();
                upgrade(&context)?.vis_set(&ud, &key, value)
            },
        );
    }
}

impl Inner {
    fn install(self: &Rc<Self>, search_paths: &[PathBuf]) -> Result<()> {
        self.install_paths(search_paths)?;
        lpeg::install(&self.lua)?;
        #[cfg(unix)]
        process::install_io(&self.lua)?;
        let vis = self.lua.create_userdata(Vis(Rc::downgrade(self)))?;
        vis.set_user_value(self.lua.create_table()?)?;
        self.lua.globals().set("vis", vis)?;
        objects::register(self)?;
        let original: Function = self.lua.globals().get("getmetatable")?;
        self.lua
            .set_named_registry_value("vis.getmetatable", original)?;
        let context = Rc::downgrade(self);
        self.lua.globals().set(
            "getmetatable",
            self.lua.create_function(move |lua, value: Value| {
                let inner = upgrade(&context)?;
                if let Value::UserData(ud) = &value {
                    let name = if ud.is::<Vis>() {
                        Some("vis")
                    } else if ud.is::<KeyAction>() {
                        Some("keyaction")
                    } else if ud.is::<GlobalOptions>() {
                        Some("visoptions")
                    } else {
                        objects::object_type(ud)
                    };
                    if let Some(name) = name {
                        return inner.types.get::<Value>(name);
                    }
                }
                let original: Function = lua.named_registry_value("vis.getmetatable")?;
                original.call(value)
            })?,
        )?;
        self.install_vis_methods()?;
        Ok(())
    }
    fn install_paths(&self, explicit: &[PathBuf]) -> Result<()> {
        let package: Table = self.lua.globals().get("package")?;
        let previous: String = package.get("path")?;
        let mut directories = explicit.to_vec();
        if let Some(path) = std::env::var_os("VIS_PATH") {
            directories.push(path.into());
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                directories.push(parent.join("lua"));
            }
        }
        if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
            directories.push(PathBuf::from(config).join("vis"));
        } else if let Some(home) = std::env::var_os("HOME") {
            directories.push(PathBuf::from(home).join(".config/vis"));
        }
        directories.extend([
            PathBuf::from("/etc/vis"),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../lua"),
            PathBuf::from("/usr/local/share/vis"),
            PathBuf::from("/usr/share/vis"),
        ]);
        let mut parts = Vec::new();
        for path in directories {
            let path = path.to_string_lossy();
            parts.push(format!("{path}/?.lua"));
            parts.push(format!("{path}/?/init.lua"));
        }
        parts.extend(
            previous
                .split(';')
                .filter(|s| !s.is_empty() && !s.contains("./"))
                .map(str::to_owned),
        );
        package.set("path", parts.join(";"))?;
        // Lua::new leaves C loaders disabled. Do not restore them through cpath.
        Ok(())
    }
    pub(crate) fn emit<A: mlua::IntoLuaMulti>(&self, event: &str, args: A) -> Result<Value> {
        let vis: AnyUserData = self.lua.globals().get("vis")?;
        let events: Value = vis.get("events")?;
        let Value::Table(events) = events else {
            return Ok(Value::Nil);
        };
        let handler: Option<Function> = events.get(event)?;
        match handler {
            Some(handler) => handler.call(args),
            None => Ok(Value::Nil),
        }
    }
    fn notify<A: mlua::IntoLuaMulti>(self: &Rc<Self>, event: &str, args: A) {
        if let Err(error) = self.emit(event, args) {
            self.report_lua_error(error);
        }
    }
    fn report_lua_error(self: &Rc<Self>, error: mlua::Error) {
        let diagnostic = error.to_string();
        self.lua_errors.borrow_mut().push(diagnostic.clone());
        if self.reporting_lua_error.replace(true) {
            return;
        }
        struct ReportingGuard<'a>(&'a Cell<bool>);
        impl Drop for ReportingGuard<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let _guard = ReportingGuard(&self.reporting_lua_error);
        let displayed = (|| -> Result<()> {
            let plan = {
                self.workspace
                    .try_borrow_mut()
                    .map_err(mlua::Error::external)?
                    .begin_message(diagnostic.into_bytes())
            }
            .map_err(mlua::Error::external)?;
            self.run_plan(plan)?;
            Ok(())
        })();
        if let Err(display_error) = displayed {
            self.messages
                .borrow_mut()
                .push(format!("could not display Lua error: {display_error}"));
        }
    }
    fn vis_get(self: &Rc<Self>, lua: &Lua, ud: &AnyUserData, key: &str) -> Result<Value> {
        match key {
            "win" => {
                let id = {
                    let ws = self.workspace.borrow();
                    (ws.window_count() > 0).then(|| ws.active_window_id())
                };
                return id
                    .map(|id| objects::window(self, id).map(Value::UserData))
                    .unwrap_or(Ok(Value::Nil));
            }
            "mode" => {
                return Ok(Value::Integer(mode_number(
                    self.workspace.borrow().editor.mode,
                )));
            }
            "types" => return Ok(Value::Table(self.types.clone())),
            "count" => {
                return Ok(self
                    .workspace
                    .borrow()
                    .editor
                    .current_count()
                    .map(|n| Value::Integer(n as i64))
                    .unwrap_or(Value::Nil));
            }
            "recording" => return Ok(Value::Boolean(self.workspace.borrow().editor.recording())),
            "input_queue" => {
                return Ok(Value::String(
                    self.lua
                        .create_string(self.workspace.borrow().editor.input_queue())?,
                ));
            }
            "register" => {
                return Ok(self
                    .workspace
                    .borrow()
                    .editor
                    .selected_register()
                    .map(|c| self.lua.create_string(c.to_string()).map(Value::String))
                    .transpose()?
                    .unwrap_or(Value::Nil));
            }
            "mark" => {
                return Ok(Value::String(self.lua.create_string(
                    self.workspace.borrow().editor.selected_mark().to_string(),
                )?));
            }
            _ => {}
        }
        let methods: Table = self.types.get("vis")?;
        let value: Value = methods.get(key)?;
        if !value.is_nil() {
            return Ok(value);
        }
        let fields: Table = ud.user_value()?;
        let _ = lua;
        fields.get(key)
    }
    fn vis_set(self: &Rc<Self>, ud: &AnyUserData, key: &str, value: Value) -> Result<()> {
        match key {
            "mode" => {
                let mode = number_mode(
                    exact_integer(&value)
                        .ok_or_else(|| mlua::Error::runtime("mode must be an integer"))?,
                )?;
                self.workspace
                    .borrow_mut()
                    .editor
                    .switch_mode(mode)
                    .map_err(mlua::Error::external)?;
            }
            "count" => {
                let count = if value.is_nil() {
                    None
                } else {
                    Some(objects::position(value)?)
                };
                self.workspace
                    .borrow_mut()
                    .editor
                    .set_count(count)
                    .map_err(mlua::Error::external)?;
            }
            "register" => {
                let c = match value {
                    Value::String(s) => s.to_str()?.chars().next(),
                    Value::Nil => None,
                    _ => return Err(mlua::Error::runtime("expected register name")),
                };
                self.workspace.borrow_mut().editor.set_selected_register(c);
            }
            "mark" => {
                let Value::String(s) = value else {
                    return Err(mlua::Error::runtime("expected mark name"));
                };
                if let Some(c) = s.to_str()?.chars().next() {
                    self.workspace.borrow_mut().editor.set_selected_mark(c);
                }
            }
            "win" => {
                let Value::UserData(win) = value else {
                    return Err(mlua::Error::runtime("expected window"));
                };
                let id = objects::window_id(&win)?;
                self.workspace
                    .borrow_mut()
                    .focus_window(id)
                    .map_err(mlua::Error::external)?;
            }
            "options" => {
                let Value::Table(table) = value else {
                    return Err(mlua::Error::runtime("options must be a table"));
                };
                for pair in table.pairs::<String, Value>() {
                    let (name, value) = pair?;
                    self.option_set(&name, value, None)?;
                }
            }
            _ => {
                let fields: Table = ud.user_value()?;
                fields.set(key, value)?;
            }
        }
        Ok(())
    }
    fn option_scope(&self, name: &str) -> Option<OptionScope> {
        if self.options.borrow().contains_key(name) {
            return Some(OptionScope::Global);
        }
        match name {
            "autoindent" | "ai" | "shell" | "escdelay" | "ignorecase" | "ic"
            | "loadmethod" | "change256colors" => Some(OptionScope::Global),
            "tabwidth" | "tw" | "expandtab" | "et" | "oneline" | "statusbar" | "showeof"
            | "numbers" | "nu" | "relativenumbers" | "rnu" | "numberwidth" | "nuw"
            | "cursorline" | "cul" | "colorcolumn" | "cc" | "showspaces" | "showtabs"
            | "shownewlines" | "wrapcolumn" | "wc" => Some(OptionScope::Window),
            _ => None,
        }
    }
    fn resolve_option_target(&self, name: &str, target: OptionTarget) -> Option<OptionTarget> {
        match (self.option_scope(name)?, target) {
            (OptionScope::Global, OptionTarget::Global | OptionTarget::Command) => {
                Some(OptionTarget::Global)
            }
            (OptionScope::Window, OptionTarget::Window(id)) => Some(OptionTarget::Window(id)),
            (OptionScope::Window, OptionTarget::Command) => {
                let ws = self.workspace.borrow();
                (ws.window_count() > 0).then(|| OptionTarget::Window(ws.active_window_id()))
            }
            _ => None,
        }
    }
    pub(crate) fn option_get(self: &Rc<Self>, name: &str, window: Option<usize>) -> Result<Value> {
        self.get_option(
            name,
            window
                .map(OptionTarget::Window)
                .unwrap_or(OptionTarget::Global),
        )
    }
    fn get_option(self: &Rc<Self>, name: &str, target: OptionTarget) -> Result<Value> {
        let Some(target) = self.resolve_option_target(name, target) else {
            return Ok(Value::Nil);
        };
        match target {
            OptionTarget::Global => {
                let custom = self.options.borrow().get(name).cloned();
                if let Some(custom) = custom {
                    return match custom.get {
                        None => Ok(Value::Nil),
                        Some(get) => match get.call(name) {
                            Ok(value) => Ok(value),
                            Err(error) => {
                                self.report_lua_error(error);
                                Ok(Value::Nil)
                            }
                        },
                    };
                }
                match name {
                    "autoindent" | "ai" => {
                        Ok(Value::Boolean(self.workspace.borrow().editor.auto_indent))
                    }
                    "ignorecase" | "ic" => {
                        Ok(Value::Boolean(self.workspace.borrow().editor.ignorecase()))
                    }
                    "change256colors" => Ok(Value::Boolean(
                        self.workspace.borrow().editor.change256colors(),
                    )),
                    "loadmethod" => match self.workspace.borrow().editor.loadmethod() {
                        Some(method) => Ok(Value::String(
                            self.lua.create_string(method)?,
                        )),
                        None => Ok(Value::Nil),
                    },
                    "shell" => {
                        let shell = self.workspace.borrow().editor.shell();
                        Ok(Value::String(self.lua.create_string(shell)?))
                    }
                    "escdelay" => Ok(match self.workspace.borrow().editor.escape_delay() {
                        Some(delay) => Value::Integer(delay.as_millis() as i64),
                        None => Value::Nil,
                    }),
                    _ => Ok(Value::Nil),
                }
            }
            OptionTarget::Window(id) => {
                let ws = self.workspace.borrow();
                let Some((tabwidth, expandtab, _)) = ws.window_options(id) else {
                    return Ok(Value::Nil);
                };
                Ok(match name {
                    "tabwidth" | "tw" => Value::Integer(tabwidth as i64),
                    "expandtab" | "et" => Value::Boolean(expandtab),
                    "numberwidth" | "nuw" => match ws.window_display(id) {
                        Some(display) => Value::Integer(display.numberwidth as i64),
                        None => Value::Nil,
                    },
                    "colorcolumn" | "cc" => match ws.window_display(id) {
                        Some(display) => Value::Integer(display.colorcolumn as i64),
                        None => Value::Nil,
                    },
                    "wrapcolumn" | "wc" => match ws.window_display(id) {
                        Some(display) => Value::Integer(display.wrapcolumn as i64),
                        None => Value::Nil,
                    },
                    _ => match ws.window_display(id) {
                        Some(display) => Value::Boolean(match name {
                            "oneline" => display.oneline,
                            "statusbar" => display.statusbar,
                            "numbers" | "nu" => display.numbers,
                            "relativenumbers" | "rnu" => display.relativenumbers,
                            "cursorline" | "cul" => display.cursorline,
                            "showspaces" => display.showspaces,
                            "showtabs" => display.showtabs,
                            "shownewlines" => display.shownewlines,
                            _ => display.showeof,
                        }),
                        None => Value::Nil,
                    },
                })
            }
            OptionTarget::Command => unreachable!("command options resolve to a concrete target"),
        }
    }
    pub(crate) fn option_set(
        self: &Rc<Self>,
        name: &str,
        value: Value,
        window: Option<usize>,
    ) -> Result<bool> {
        let target = window
            .map(OptionTarget::Window)
            .unwrap_or(OptionTarget::Global);
        let Some(target) = self.resolve_option_target(name, target) else {
            return Ok(false);
        };
        self.set_option(name, value, false, target)?;
        // A recognized native property never falls back to a user field merely
        // because a callback rejected the value.
        Ok(true)
    }
    fn set_option(
        self: &Rc<Self>,
        name: &str,
        value: Value,
        toggle: bool,
        target: OptionTarget,
    ) -> Result<bool> {
        let Some(target) = self.resolve_option_target(name, target) else {
            return Ok(false);
        };
        if target == OptionTarget::Global {
            let custom = self.options.borrow().get(name).cloned();
            if let Some(custom) = custom {
                let value = if matches!(custom.kind, OptionKind::Number) && !value.is_nil() {
                    Value::Integer(
                        exact_integer(&value)
                            .ok_or_else(|| mlua::Error::runtime("invalid integer option value"))?,
                    )
                } else {
                    value
                };
                let valid = matches!(
                    (custom.kind, &value),
                    (OptionKind::Bool, Value::Boolean(_))
                        | (OptionKind::String, Value::String(_))
                        | (OptionKind::Number, Value::Integer(_))
                ) || value.is_nil();
                if !valid {
                    return Err(mlua::Error::runtime("invalid option value"));
                }
                let returns: MultiValue = match custom.set.call((value, toggle, name)) {
                    Ok(values) => values,
                    Err(error) => {
                        self.report_lua_error(error);
                        return Ok(false);
                    }
                };
                return Ok(!matches!(returns.back(), Some(Value::Boolean(false))));
            }
            let mut ws = self.workspace.borrow_mut();
            match name {
                "ignorecase" | "ic" => {
                    let current = ws.editor.ignorecase();
                    let enabled = if toggle { !current } else { truth(&value) };
                    ws.editor.set_ignorecase(enabled);
                    return Ok(true);
                }
                "change256colors" => {
                    let current = ws.editor.change256colors();
                    let enabled = if toggle { !current } else { truth(&value) };
                    ws.editor.set_change256colors(enabled);
                    return Ok(true);
                }
                "loadmethod" => {
                    let method = match value {
                        Value::String(method) => Some(method.to_str()?.to_string()),
                        Value::Nil => None,
                        _ => return Err(mlua::Error::runtime("expected load method name")),
                    };
                    ws.editor.set_loadmethod(method);
                    return Ok(true);
                }
                "shell" => {
                    let shell = match value {
                        Value::String(shell) => Some(shell.to_str()?.to_string()),
                        Value::Nil => None,
                        _ => return Err(mlua::Error::runtime("expected shell program name")),
                    };
                    ws.editor.set_shell(shell);
                    return Ok(true);
                }
                "escdelay" => {
                    let delay = match value {
                        Value::Integer(milliseconds) if milliseconds >= 0 => {
                            Some(std::time::Duration::from_millis(milliseconds as u64))
                        }
                        Value::Nil => None,
                        _ => {
                            return Err(mlua::Error::runtime(
                                "expected escape delay in milliseconds",
                            ));
                        }
                    };
                    ws.editor.set_escape_delay(delay);
                    return Ok(true);
                }
                _ => {}
            }
            ws.editor.auto_indent = if toggle {
                !ws.editor.auto_indent
            } else {
                truth(&value)
            };
            return Ok(true);
        }
        let OptionTarget::Window(id) = target else {
            unreachable!("command target resolved")
        };
        let mut ws = self.workspace.borrow_mut();
        if matches!(
            name,
            "oneline"
                | "statusbar"
                | "showeof"
                | "numbers"
                | "nu"
                | "relativenumbers"
                | "rnu"
                | "cursorline"
                | "cul"
                | "showspaces"
                | "showtabs"
                | "shownewlines"
        ) {
            let Some(mut display) = ws.window_display(id) else {
                return Ok(false);
            };
            // The native absolute and relative number options are mutually
            // exclusive.
            let relative = matches!(name, "relativenumbers" | "rnu");
            match name {
                "oneline" => {
                    display.oneline = if toggle {
                        !display.oneline
                    } else {
                        truth(&value)
                    };
                }
                "statusbar" => {
                    display.statusbar = if toggle {
                        !display.statusbar
                    } else {
                        truth(&value)
                    };
                }
                "showeof" => {
                    display.showeof = if toggle {
                        !display.showeof
                    } else {
                        truth(&value)
                    };
                }
                "cursorline" | "cul" => {
                    display.cursorline = if toggle {
                        !display.cursorline
                    } else {
                        truth(&value)
                    };
                }
                "showspaces" => {
                    display.showspaces = if toggle {
                        !display.showspaces
                    } else {
                        truth(&value)
                    };
                }
                "showtabs" => {
                    display.showtabs = if toggle {
                        !display.showtabs
                    } else {
                        truth(&value)
                    };
                }
                "shownewlines" => {
                    display.shownewlines = if toggle {
                        !display.shownewlines
                    } else {
                        truth(&value)
                    };
                }
                _ => {
                    let current = if relative {
                        display.relativenumbers
                    } else {
                        display.numbers
                    };
                    let enabled = if toggle { !current } else { truth(&value) };
                    display.numbers = enabled && !relative;
                    display.relativenumbers = enabled && relative;
                }
            }
            ws.set_window_display(id, display)
                .map_err(mlua::Error::external)?;
            return Ok(true);
        }
        if matches!(
            name,
            "numberwidth" | "nuw" | "colorcolumn" | "cc" | "wrapcolumn" | "wc"
        ) {
            let Some(mut display) = ws.window_display(id) else {
                return Ok(false);
            };
            let number = exact_integer(&value)
                .ok_or_else(|| mlua::Error::runtime("option value must be a number"))?
                as usize;
            match name {
                "numberwidth" | "nuw" => display.numberwidth = number,
                "colorcolumn" | "cc" => display.colorcolumn = number,
                _ => display.wrapcolumn = number,
            }
            ws.set_window_display(id, display)
                .map_err(mlua::Error::external)?;
            return Ok(true);
        }
        let Some((mut tabwidth, mut expandtab, _)) = ws.window_options(id) else {
            return Ok(false);
        };
        match name {
            "tabwidth" | "tw" => {
                tabwidth = exact_integer(&value)
                    .filter(|n| (1..=8).contains(n))
                    .ok_or_else(|| mlua::Error::runtime("tab width must be 1..8"))?
                    as usize
            }
            "expandtab" | "et" => expandtab = if toggle { !expandtab } else { truth(&value) },
            _ => return Ok(false),
        }
        let autoindent = ws.editor.auto_indent;
        ws.set_window_options(id, tabwidth, expandtab, autoindent)
            .map_err(mlua::Error::external)?;
        Ok(true)
    }
}

fn truth(value: &Value) -> bool {
    !matches!(value, Value::Nil | Value::Boolean(false))
}
pub(crate) fn mode_number(mode: Mode) -> i64 {
    match mode {
        Mode::Normal => 0,
        Mode::OperatorPending => 1,
        Mode::Visual => 2,
        Mode::VisualLine => 3,
        Mode::Insert => 4,
        Mode::Replace => 5,
        Mode::Command => 6,
    }
}
pub(crate) fn number_mode(n: i64) -> Result<Mode> {
    match n {
        0 => Ok(Mode::Normal),
        1 => Ok(Mode::OperatorPending),
        2 => Ok(Mode::Visual),
        3 => Ok(Mode::VisualLine),
        4 => Ok(Mode::Insert),
        5 => Ok(Mode::Replace),
        _ => Err(mlua::Error::runtime("invalid mode")),
    }
}

impl Inner {
    fn install_vis_methods(self: &Rc<Self>) -> Result<()> {
        let methods: Table = self.types.get("vis")?;
        methods.set("VERSION", concat!("vis-rs ", env!("CARGO_PKG_VERSION")))?;
        methods.set("API", 0)?;
        let modes = self.lua.create_table()?;
        for (name, id) in [
            ("NORMAL", 0),
            ("OPERATOR_PENDING", 1),
            ("VISUAL", 2),
            ("VISUAL_LINE", 3),
            ("INSERT", 4),
            ("REPLACE", 5),
        ] {
            modes.set(name, id)?;
        }
        methods.set("modes", modes)?;
        macro_rules! method {
            ($name:literal,$closure:expr) => {{
                let context = Rc::downgrade(self);
                methods.set(
                    $name,
                    self.lua.create_function(move |lua, args: MultiValue| {
                        let inner = upgrade(&context)?;
                        let mut args = args;
                        args.pop_front();
                        ($closure)(&inner, lua, args)
                    })?,
                )?;
            }};
        }
        method!("command", |inner: &Rc<Inner>, _: &Lua, args: MultiValue| {
            inner.command(&string_arg(&args, 0)?)
        });
        method!("info", |inner: &Rc<Inner>, _: &Lua, args: MultiValue| {
            inner.messages.borrow_mut().push(string_arg(&args, 0)?);
            Ok(())
        });
        method!("message", |inner: &Rc<Inner>, _: &Lua, args: MultiValue| {
            let plan = {
                inner
                    .workspace
                    .borrow_mut()
                    .begin_message(bytes_arg(&args, 0)?)
            }
            .map_err(mlua::Error::external)?;
            inner.run_plan(plan)?;
            Ok(())
        });
        method!("feedkeys", |inner: &Rc<Inner>,
                             _: &Lua,
                             args: MultiValue| {
            inner.feedkeys(&string_arg(&args, 0)?)
        });
        method!("insert", |inner: &Rc<Inner>, _: &Lua, args: MultiValue| {
            inner.insert(&bytes_arg(&args, 0)?, false)
        });
        method!("replace", |inner: &Rc<Inner>, _: &Lua, args: MultiValue| {
            inner.insert(&bytes_arg(&args, 0)?, true)
        });
        method!("windows", |inner: &Rc<Inner>, _: &Lua, _: MultiValue| {
            objects::windows_iterator(inner)
        });
        method!("files", |inner: &Rc<Inner>, _: &Lua, _: MultiValue| {
            objects::files_iterator(inner)
        });
        method!("map", |inner: &Rc<Inner>, _: &Lua, args: MultiValue| inner
            .map(None, args));
        method!("unmap", |inner: &Rc<Inner>, _: &Lua, args: MultiValue| {
            inner.unmap(None, args)
        });
        method!(
            "option_register",
            |inner: &Rc<Inner>, _: &Lua, args: MultiValue| {
                let name = string_arg(&args, 0)?;
                let kind = match string_arg(&args, 1)?.as_str() {
                    "bool" => OptionKind::Bool,
                    "string" => OptionKind::String,
                    "number" => OptionKind::Number,
                    _ => return Err(mlua::Error::runtime("invalid option type")),
                };
                let set = function_arg(&args, 2)?;
                let get = match args.get(3) {
                    Some(Value::Function(f)) => Some(f.clone()),
                    _ => None,
                };
                let mut options = inner.options.borrow_mut();
                if options.contains_key(&name) || builtin_option_kind(&name).is_some() {
                    return Ok(false);
                }
                inner
                    .workspace
                    .borrow_mut()
                    .editor
                    .register_external_option(&name);
                options.insert(name, OptionDefinition { kind, set, get });
                Ok(true)
            }
        );
        method!(
            "option_unregister",
            |inner: &Rc<Inner>, _: &Lua, args: MultiValue| {
                let name = string_arg(&args, 0)?;
                let removed = inner.options.borrow_mut().remove(&name).is_some();
                if removed {
                    inner
                        .workspace
                        .borrow_mut()
                        .editor
                        .unregister_external_option(&name);
                }
                Ok(removed)
            }
        );
        method!(
            "command_register",
            |inner: &Rc<Inner>, _: &Lua, args: MultiValue| {
                let name = string_arg(&args, 0)?;
                let function = function_arg(&args, 1)?;
                if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    return Ok(false);
                }
                let mut commands = inner.commands.borrow_mut();
                if commands.contains_key(&name) || NATIVE_COMMANDS.contains(&name.as_str()) {
                    return Ok(false);
                }
                inner
                    .workspace
                    .borrow_mut()
                    .editor
                    .register_external_command(&name);
                commands.insert(name, function);
                Ok(true)
            }
        );
        method!("mappings", |inner: &Rc<Inner>,
                             lua: &Lua,
                             args: MultiValue| {
            let mode = number_mode(integer_arg(&args, 0)?)?;
            let mappings = {
                let ws = inner.workspace.borrow();
                let mut maps = ws.editor.mapping_aliases(mode, None);
                if ws.window_count() > 0 {
                    maps.extend(
                        ws.editor
                            .mapping_aliases(mode, Some(ws.active_window_id() as u64)),
                    );
                }
                maps
            };
            let table = lua.create_table()?;
            for (key, alias, callback) in mappings {
                let help = alias
                    .or_else(|| {
                        callback.and_then(|id| inner.callback_help.borrow().get(&id).cloned())
                    })
                    .unwrap_or_default();
                table.set(key, help)?;
            }
            Ok(table)
        });
        method!(
            "complete_command",
            |inner: &Rc<Inner>, lua: &Lua, args: MultiValue| {
                let prefix = string_arg(&args, 0)?;
                let mut names = [
                    "w", "wq", "q", "qall", "edit", "open", "split", "vsplit", "new", "vnew",
                    "buffer", "bdelete", "cd", "pwd", "files", "wincmd", "help", "set", "map",
                    "unmap",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
                names.extend(inner.commands.borrow().keys().cloned());
                names.sort();
                names.dedup();
                let input = names
                    .into_iter()
                    .filter(|name| name.starts_with(&prefix))
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n";
                let directory = inner.workspace.borrow().directory().to_owned();
                let (status, out, err) = inner
                    .run_pipe("vis-menu -b", input.as_bytes(), &directory)
                    .map_err(mlua::Error::external)?;
                Ok((status, lua.create_string(out)?, lua.create_string(err)?))
            }
        );
        #[cfg(unix)]
        method!(
            "communicate",
            |inner: &Rc<Inner>, lua: &Lua, args: MultiValue| {
                let checked_string = |index: usize| -> Result<String> {
                    lua.coerce_string(args.get(index).cloned().unwrap_or(Value::Nil))?
                        .ok_or_else(|| {
                            mlua::Error::runtime(format!("argument {} must be a string", index + 1))
                        })?
                        .to_str()
                        .map(|value| value.to_owned())
                };
                let name = checked_string(0)?;
                let command = checked_string(1)?;
                let directory = inner.workspace.borrow().directory().to_owned();
                let spawned = inner.processes.borrow_mut().spawn(
                    &name,
                    &command,
                    Path::new("/bin/sh"),
                    &directory,
                );
                match spawned {
                    Ok(stream) => Ok(MultiValue::from_vec(vec![Value::UserData(
                        lua.create_userdata(stream)?,
                    )])),
                    Err(error) => Ok(MultiValue::from_vec(vec![
                        Value::Nil,
                        Value::String(lua.create_string(error.to_string())?),
                        error
                            .raw_os_error()
                            .map(|n| Value::Integer(n as i64))
                            .unwrap_or(Value::Nil),
                    ])),
                }
            }
        );
        method!("pipe", |inner: &Rc<Inner>, lua: &Lua, args: MultiValue| {
            inner.pipe(lua, args)
        });
        method!("exit", |inner: &Rc<Inner>, _: &Lua, args: MultiValue| {
            let code = integer_arg(&args, 0)?;
            inner
                .exit_code
                .set(Some(i32::try_from(code).map_err(mlua::Error::external)?));
            *inner.outcome.borrow_mut() = Outcome::Quit;
            Ok(())
        });
        method!("redraw", |inner: &Rc<Inner>, _: &Lua, _: MultiValue| inner
            .redraw());
        for (name, kind) in [
            ("motion_register", CallbackType::Motion),
            ("operator_register", CallbackType::Operator),
            ("textobject_register", CallbackType::TextObject),
        ] {
            let context = Rc::downgrade(self);
            methods.set(
                name,
                self.lua
                    .create_function(move |_, (_, function): (AnyUserData, Function)| {
                        upgrade(&context)?.register_callback(kind, function)
                    })?,
            )?;
        }
        for (name, kind) in [
            ("motion", CallbackType::Motion),
            ("operator", CallbackType::Operator),
            ("textobject", CallbackType::TextObject),
        ] {
            let context = Rc::downgrade(self);
            methods.set(
                name,
                self.lua
                    .create_function(move |_, (_, id): (AnyUserData, u64)| {
                        upgrade(&context)?.invoke_callback(kind, id)
                    })?,
            )?;
        }
        method!(
            "action_register",
            |inner: &Rc<Inner>, _: &Lua, args: MultiValue| {
                let name = string_arg(&args, 0)?;
                let function = function_arg(&args, 1)?;
                let Some(action) = inner.register_action(&name, function)? else {
                    return Ok(None);
                };
                if let Some(Value::String(help)) = args.get(2) {
                    inner
                        .callback_help
                        .borrow_mut()
                        .insert(action.borrow::<KeyAction>()?.id, help.to_str()?.to_string());
                }
                Ok(Some(action))
            }
        );
        let options = self
            .lua
            .create_userdata(GlobalOptions(Rc::downgrade(self)))?;
        options.set_user_value(self.lua.create_table()?)?;
        methods.set("options", options)?;
        self.install_registers(&methods)?;
        self.install_ui(&methods)?;
        Ok(())
    }
    pub(crate) fn allocated_style_count(&self) -> usize {
        self.style_count.get()
    }
    fn install_ui(self: &Rc<Self>, methods: &Table) -> Result<()> {
        let ui = self.lua.create_table()?;
        ui.set("colors", 256)?;
        let layouts = self.lua.create_table()?;
        layouts.set("HORIZONTAL", 0)?;
        layouts.set("VERTICAL", 1)?;
        ui.set("layouts", layouts)?;
        let styles = self.lua.create_table()?;
        for (id, name) in [
            "DEFAULT",
            "CURSOR",
            "CURSOR_PRIMARY",
            "CURSOR_LINE",
            "SELECTION",
            "LINENUMBER",
            "LINENUMBER_CURSOR",
            "COLOR_COLUMN",
            "STATUS",
            "STATUS_FOCUSED",
            "SEPARATOR",
            "INFO",
            "EOF",
            "WHITESPACE",
        ]
        .into_iter()
        .enumerate()
        {
            styles.set(name, id)?;
        }
        styles.set("MAX", 512)?;
        ui.set("style_ids", styles)?;
        let context = Rc::downgrade(self);
        ui.set(
            "style_define",
            self.lua
                .create_function(move |_, (_, id, spec): (Table, usize, String)| {
                    let inner = upgrade(&context)?;
                    if id >= inner.allocated_style_count() {
                        return Ok(false);
                    }
                    inner.object_state.borrow_mut().styles.insert(id, spec);
                    Ok(true)
                })?,
        )?;
        let context = Rc::downgrade(self);
        ui.set(
            "style_push",
            self.lua
                .create_function(move |_, (_, spec): (Table, Option<String>)| {
                    let inner = upgrade(&context)?;
                    let id = inner.style_count.get();
                    if id >= 512 {
                        return Ok(None);
                    }
                    inner.style_count.set(id + 1);
                    if let Some(spec) = spec {
                        inner.object_state.borrow_mut().styles.insert(id, spec);
                    }
                    Ok(Some(id))
                })?,
        )?;
        let meta = self.lua.create_table()?;
        let context = Rc::downgrade(self);
        meta.set(
            "__index",
            self.lua
                .create_function(move |_, (_, name): (Table, String)| {
                    let inner = upgrade(&context)?;
                    Ok(if name == "layout" {
                        Value::Integer(match inner.workspace.borrow().layout {
                            crate::workspace::Layout::Horizontal => 0,
                            crate::workspace::Layout::Vertical => 1,
                        })
                    } else {
                        Value::Nil
                    })
                })?,
        )?;
        let context = Rc::downgrade(self);
        meta.set(
            "__newindex",
            self.lua
                .create_function(move |_, (table, name, value): (Table, String, Value)| {
                    let inner = upgrade(&context)?;
                    if name == "layout" {
                        inner.workspace.borrow_mut().layout = match exact_integer(&value) {
                            Some(0) => crate::workspace::Layout::Horizontal,
                            Some(1) => crate::workspace::Layout::Vertical,
                            _ => return Err(mlua::Error::runtime("invalid layout")),
                        };
                        Ok(())
                    } else {
                        table.raw_set(name, value)
                    }
                })?,
        )?;
        ui.set_metatable(Some(meta))?;
        methods.set("ui", ui)?;
        Ok(())
    }
    fn pipe(
        self: &Rc<Self>,
        lua: &Lua,
        args: MultiValue,
    ) -> Result<(i32, mlua::String, mlua::String)> {
        let bad = || {
            mlua::Error::runtime(
                "expected command[, fullscreen], text, command[, fullscreen], or file, range, command[, fullscreen]",
            )
        };
        let (input, command, fullscreen) = match args.as_slices() {
            _ if args.len() == 1 => (Vec::new(), string_arg(&args, 0)?, false),
            _ if args.len() == 2 && matches!(args.get(1), Some(Value::Boolean(_))) => {
                (Vec::new(), string_arg(&args, 0)?, truth(&args[1]))
            }
            _ if args.len() == 2 => (bytes_arg(&args, 0)?, string_arg(&args, 1)?, false),
            _ if args.len() == 3 && matches!(args.get(2), Some(Value::Boolean(_))) => {
                (bytes_arg(&args, 0)?, string_arg(&args, 1)?, truth(&args[2]))
            }
            _ if args.len() == 3 || args.len() == 4 => {
                let Some(Value::UserData(file)) = args.front() else {
                    return Err(bad());
                };
                objects::file_id(file)?;
                let Some(Value::Table(range)) = args.get(1) else {
                    return Err(bad());
                };
                // Invoke the native file content method after all host borrows end.
                let content: Function = file.get("content")?;
                let bytes: mlua::String = content.call((file.clone(), range.clone()))?;
                let full = if args.len() == 4 {
                    match args[3] {
                        Value::Boolean(b) => b,
                        _ => return Err(bad()),
                    }
                } else {
                    false
                };
                (bytes.as_bytes().to_vec(), string_arg(&args, 2)?, full)
            }
            _ => return Err(bad()),
        };
        let directory = self.workspace.borrow().directory().to_owned();
        let observer = self.process_observer.borrow().clone();
        if fullscreen {
            if let Some(observer) = &observer {
                observer(true).map_err(mlua::Error::external)?;
            }
        }
        let output = self.run_pipe(&command, &input, &directory);
        let restored = if fullscreen {
            observer
                .as_ref()
                .map(|observer| observer(false))
                .transpose()
        } else {
            Ok(None)
        };
        restored.map_err(mlua::Error::external)?;
        let output = output.map_err(mlua::Error::external)?;
        self.redraw()?;
        Ok((
            output.0,
            lua.create_string(output.1)?,
            lua.create_string(output.2)?,
        ))
    }
    fn style_snapshot(&self, id: usize) -> Style {
        let definition = self.object_state.borrow().styles.get(&id).cloned();
        if let Some(definition) = definition {
            let style = Style::parse(&definition);
            return if id == 0 {
                Style::terminal_default().merge(style)
            } else {
                style
            };
        }
        let attributes = match id {
            1 | 4 | 7 | 8 => "reverse",
            2 => "reverse,blink",
            9 => "reverse,bold",
            11 => "bold",
            _ => "",
        };
        Style::terminal_default().merge(Style::parse(attributes))
    }
    pub(crate) fn record_style_range(
        self: &Rc<Self>,
        window: usize,
        style: usize,
        range: std::ops::Range<usize>,
    ) -> Result<()> {
        let snapshot = self.style_snapshot(style);
        self.object_state
            .borrow_mut()
            .windows
            .entry(window)
            .or_default()
            .ranges
            .push((style, range.clone()));
        self.draw_records
            .borrow_mut()
            .entry(window)
            .or_default()
            .operations
            .push(DrawOperation::Range {
                style: snapshot,
                range,
            });
        Ok(())
    }
    pub(crate) fn record_style_pos(
        self: &Rc<Self>,
        window: usize,
        style: usize,
        x: usize,
        y: usize,
    ) -> Result<bool> {
        {
            let state = self.object_state.borrow();
            let Some(visual) = state.windows.get(&window) else {
                return Ok(false);
            };
            if x >= visual.width || y >= visual.height {
                return Ok(false);
            }
        }
        let snapshot = self.style_snapshot(style);
        self.object_state
            .borrow_mut()
            .windows
            .entry(window)
            .or_default()
            .cells
            .insert((x, y), style);
        self.draw_records
            .borrow_mut()
            .entry(window)
            .or_default()
            .operations
            .push(DrawOperation::Cell {
                x,
                y,
                style: snapshot,
            });
        Ok(true)
    }
    pub(crate) fn record_status(
        self: &Rc<Self>,
        window: usize,
        left: Vec<u8>,
        right: Vec<u8>,
    ) -> Result<()> {
        let focused = {
            let ws = self.workspace.borrow();
            ws.window_count() > 0 && ws.active_window_id() == window
        };
        let style = self
            .style_snapshot(0)
            .merge(self.style_snapshot(if focused { 9 } else { 8 }));
        {
            let mut state = self.object_state.borrow_mut();
            let visual = state.windows.entry(window).or_default();
            visual.status_left = left.clone();
            visual.status_right = right.clone();
        }
        self.draw_records
            .borrow_mut()
            .entry(window)
            .or_default()
            .operations
            .push(DrawOperation::Status { left, right, style });
        Ok(())
    }
    pub(crate) fn record_window_draw(self: &Rc<Self>, window: usize) -> Result<()> {
        let base = self.style_snapshot(0);
        {
            let mut state = self.object_state.borrow_mut();
            let visual = state.windows.entry(window).or_default();
            visual.ranges.clear();
            visual.cells.clear();
            visual.redraw = true;
        }
        // C view_draw resets the view, without copying it into the UI or
        // recursively generating another highlight/status event.
        self.draw_records
            .borrow_mut()
            .entry(window)
            .or_default()
            .operations
            .push(DrawOperation::Range {
                style: base,
                range: 0..usize::MAX,
            });
        Ok(())
    }
    fn begin_window_draw(&self, window: usize) {
        let base = self.style_snapshot(0);
        let whitespace = self.style_snapshot(13);
        let mut records = self.draw_records.borrow_mut();
        let recording = records.entry(window).or_default();
        recording.base = Some(base);
        recording.whitespace = Some(whitespace);
        // A target view update resets earlier view styles, while UI writes
        // directed at this window before its update retain their order.
        recording.operations.retain(|operation| {
            matches!(
                operation,
                DrawOperation::Cell { .. } | DrawOperation::Status { .. }
            )
        });
    }
    fn poll_processes(self: &Rc<Self>) -> Result<()> {
        #[cfg(unix)]
        {
            let responses = self.processes.borrow_mut().poll();
            self.deliver_process_responses(responses)?;
        }
        Ok(())
    }
    fn interrupt_processes(self: &Rc<Self>) -> Result<()> {
        #[cfg(unix)]
        self.processes.borrow_mut().request_cancel_all();
        self.poll_processes()
    }
    #[cfg(unix)]
    fn deliver_process_responses(self: &Rc<Self>, responses: Vec<process::Response>) -> Result<()> {
        for response in responses {
            let data = response
                .data
                .map(|bytes| self.lua.create_string(bytes))
                .transpose()?;
            self.notify(
                "process_response",
                (response.name, response.kind.as_str(), response.code, data),
            );
        }
        Ok(())
    }
    fn draw_window(self: &Rc<Self>, id: usize, events: bool) -> Result<()> {
        let internal = {
            let ws = self.workspace.borrow();
            let Some(internal) = ws.window_internal(id) else {
                return Ok(());
            };
            internal
        };
        self.begin_window_draw(id);
        let win = objects::window(self, id)?;
        if events && !internal {
            self.notify("win_highlight", win.clone());
        }
        if self.workspace.borrow().window_file_id(id).is_none() {
            return Ok(());
        }
        let operation = DrawOperation::CoreOverlays {
            selection: self.style_snapshot(4),
            cursor: self.style_snapshot(1),
            primary: self.style_snapshot(2),
            eof: self.style_snapshot(12),
        };
        self.draw_records
            .borrow_mut()
            .entry(id)
            .or_default()
            .operations
            .push(operation);
        let statusbar = self
            .workspace
            .borrow()
            .window_display(id)
            .is_some_and(|display| display.statusbar);
        if events && !internal && statusbar {
            self.notify("win_status", win);
        }
        self.draw_records
            .borrow_mut()
            .entry(id)
            .or_default()
            .operations
            .push(DrawOperation::BodyCopy);
        Ok(())
    }
    pub(crate) fn redraw(self: &Rc<Self>) -> Result<()> {
        self.poll_processes()?;
        let windows = self.workspace.borrow().window_ids();
        for id in windows {
            self.draw_window(id, true)?;
        }
        // A callback may have opened an internal error/prompt window. It still
        // needs its view copied before final UI_DRAW cell operations.
        let windows = self.workspace.borrow().window_ids();
        for id in windows {
            let copied = self.draw_records.borrow().get(&id).is_some_and(|record| {
                record
                    .operations
                    .iter()
                    .any(|operation| matches!(operation, DrawOperation::BodyCopy))
            });
            if !copied {
                self.draw_window(id, false)?;
            }
        }
        self.notify("ui_draw", ());
        Ok(())
    }
}

fn string_arg(args: &MultiValue, index: usize) -> Result<String> {
    match args.get(index) {
        Some(Value::String(s)) => Ok(s.to_str()?.to_owned()),
        _ => Err(mlua::Error::runtime(format!(
            "argument {} must be a string",
            index + 1
        ))),
    }
}
fn bytes_arg(args: &MultiValue, index: usize) -> Result<Vec<u8>> {
    match args.get(index) {
        Some(Value::String(s)) => Ok(s.as_bytes().to_vec()),
        _ => Err(mlua::Error::runtime(format!(
            "argument {} must be a string",
            index + 1
        ))),
    }
}
fn integer_arg(args: &MultiValue, index: usize) -> Result<i64> {
    args.get(index)
        .and_then(exact_integer)
        .ok_or_else(|| mlua::Error::runtime(format!("argument {} must be an integer", index + 1)))
}
fn function_arg(args: &MultiValue, index: usize) -> Result<Function> {
    match args.get(index) {
        Some(Value::Function(f)) => Ok(f.clone()),
        _ => Err(mlua::Error::runtime(format!(
            "argument {} must be a function",
            index + 1
        ))),
    }
}

impl Inner {
    fn run_pipe(
        &self,
        command: &str,
        input: &[u8],
        directory: &Path,
    ) -> std::io::Result<(i32, Vec<u8>, Vec<u8>)> {
        #[cfg(unix)]
        {
            let observer = self.process_group_observer.borrow().clone();
            process::run_pipe_with_observer(
                command,
                input,
                Path::new("/bin/sh"),
                directory,
                Arc::clone(&self.interrupt),
                observer
                    .as_ref()
                    .map(|observer| &**observer as &dyn Fn(Option<u32>) -> std::io::Result<()>),
            )
        }
        #[cfg(not(unix))]
        {
            let _ = (command, input, directory);
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "subprocesses require Unix",
            ))
        }
    }
}

impl Runtime {
    pub fn open_cli_path(
        &self,
        path: &Path,
        input: Option<Vec<u8>>,
        read_only: bool,
    ) -> Result<Outcome> {
        let plan = self
            .inner
            .workspace
            .borrow_mut()
            .begin_open(path, input, read_only)
            .map_err(mlua::Error::external)?;
        self.inner.run_plan(plan)
    }
    pub fn open_scratch(&self, input: Option<Vec<u8>>, read_only: bool) -> Result<Outcome> {
        let plan = self
            .inner
            .workspace
            .borrow_mut()
            .begin_scratch(input, read_only)
            .map_err(mlua::Error::external)?;
        self.inner.run_plan(plan)
    }
    pub fn handle_key(&self, key: Key) -> Result<Outcome> {
        if key == Key::Ctrl('c') {
            self.inner.interrupt_processes()?;
        }
        self.inner.handle_key(key)
    }
    pub fn paste(&self, bytes: &[u8]) -> Result<Outcome> {
        self.inner
            .workspace
            .borrow_mut()
            .editor
            .paste(bytes)
            .map_err(mlua::Error::external)?;
        Ok(Outcome::Continue)
    }
}
impl Inner {
    fn run_plan(self: &Rc<Self>, mut plan: crate::workspace::HostPlan) -> Result<Outcome> {
        use crate::workspace::HostStep;
        let mut allow = true;
        loop {
            let next = { self.workspace.borrow_mut().advance(&mut plan, allow) };
            let step = match next {
                Ok(step) => step,
                Err(error) => {
                    self.workspace.borrow_mut().cancel_plan(&plan);
                    return Err(mlua::Error::external(error));
                }
            };
            allow = true;
            let result = match step {
                HostStep::TargetCommand { token, request } => {
                    let success = match self.target_command(&request) {
                        Ok(success) => success,
                        Err(error) => {
                            self.messages.borrow_mut().push(error.to_string());
                            false
                        }
                    };
                    let resumed = self
                        .workspace
                        .borrow_mut()
                        .resume_target_command(&mut plan, token, success);
                    if let Err(error) = resumed {
                        self.workspace.borrow_mut().cancel_plan(&plan);
                        return Err(mlua::Error::external(error));
                    }
                    Ok(Value::Nil)
                }
                HostStep::PromptCommand { token, command } => {
                    let success = match self.command(&command) {
                        Ok(success) => success,
                        Err(error) => {
                            self.report_lua_error(error);
                            false
                        }
                    };
                    let resumed = {
                        self.workspace
                            .borrow_mut()
                            .resume_prompt_command(&mut plan, token, success)
                    };
                    if let Err(error) = resumed {
                        self.workspace.borrow_mut().cancel_plan(&plan);
                        return Err(mlua::Error::external(error));
                    }
                    Ok(Value::Nil)
                }

                HostStep::Input(request) => {
                    let internal = {
                        let ws = self.workspace.borrow();
                        ws.window_count() == 0
                            || ws.window_internal(ws.active_window_id()) == Some(true)
                    };
                    let consumed = if internal {
                        false
                    } else {
                        match self.emit("input", self.lua.create_string(&request.bytes)?) {
                            Ok(value) => truth(&value),
                            Err(error) => {
                                self.report_lua_error(error);
                                false
                            }
                        }
                    };
                    let resumed = {
                        self.workspace
                            .borrow_mut()
                            .resume_input(&mut plan, request.token, consumed)
                    };
                    if let Err(error) = resumed {
                        self.workspace.borrow_mut().cancel_plan(&plan);
                        return Err(mlua::Error::external(error));
                    }
                    Ok(Value::Nil)
                }

                HostStep::Command(command) => {
                    if !self.command(&command)? {
                        self.messages
                            .borrow_mut()
                            .push(format!("command failed: {command}"));
                    }
                    Ok(Value::Nil)
                }
                HostStep::Callback(request) => {
                    let token = request.token;
                    let fallback = match &request.kind {
                        CallbackKind::Mapping { .. } => CallbackResponse::Mapping(None),
                        CallbackKind::TextObject { .. } => CallbackResponse::Range(None),
                        _ => CallbackResponse::Position(None),
                    };
                    let response = match self.dispatch_callback(request) {
                        Ok(response) => response,
                        Err(error) => {
                            self.report_lua_error(error);
                            fallback
                        }
                    };
                    let resumed = {
                        self.workspace
                            .borrow_mut()
                            .resume_callback(&mut plan, token, response)
                    };
                    if let Err(error) = resumed {
                        self.workspace.borrow_mut().cancel_plan(&plan);
                        return Err(mlua::Error::external(error));
                    }
                    Ok(Value::Nil)
                }
                HostStep::FileOpen(id) => {
                    if self.workspace.borrow().buffer_internal(id) == Some(true) {
                        Ok(Value::Nil)
                    } else {
                        self.emit("file_open", objects::file(self, id)?)
                    }
                }
                HostStep::WindowOpen(id) => {
                    if self.workspace.borrow().window_internal(id) == Some(true) {
                        Ok(Value::Nil)
                    } else {
                        self.emit("win_open", objects::window(self, id)?)
                    }
                }
                HostStep::WindowClose(id) => {
                    if self.workspace.borrow().window_internal(id) == Some(true) {
                        Ok(Value::Nil)
                    } else {
                        self.emit("win_close", objects::window(self, id)?)
                    }
                }
                HostStep::FileClose(id) => {
                    if self.workspace.borrow().buffer_internal(id) == Some(true) {
                        Ok(Value::Nil)
                    } else {
                        self.emit("file_close", objects::file(self, id)?)
                    }
                }
                HostStep::SavePre(save) => {
                    if self.workspace.borrow().buffer_internal(save.buffer) == Some(true) {
                        continue;
                    }
                    let result = self.emit(
                        "file_save_pre",
                        (
                            objects::file(self, save.buffer)?,
                            save.path.to_string_lossy().to_string(),
                        ),
                    );
                    allow = match result {
                        Ok(Value::Boolean(false)) => false,
                        Ok(_) => true,
                        Err(error) => {
                            self.report_lua_error(error);
                            false
                        }
                    };
                    if save.force {
                        allow = true;
                    }
                    Ok(Value::Nil)
                }
                HostStep::SavePost { buffer, path } => {
                    if self.workspace.borrow().buffer_internal(buffer) == Some(true) {
                        Ok(Value::Nil)
                    } else {
                        self.emit(
                            "file_save_post",
                            (
                                objects::file(self, buffer)?,
                                path.to_string_lossy().to_string(),
                            ),
                        )
                    }
                }
                HostStep::Complete(outcome) => {
                    objects::cleanup(self)?;
                    match &outcome {
                        Outcome::Message(message) => {
                            self.messages.borrow_mut().push(message.clone())
                        }
                        Outcome::Quit => *self.outcome.borrow_mut() = Outcome::Quit,
                        Outcome::Continue | Outcome::Suspend => {}
                    }
                    if matches!(*self.outcome.borrow(), Outcome::Quit) {
                        return Ok(Outcome::Quit);
                    }
                    return Ok(outcome);
                }
            };
            if let Err(error) = result {
                // Ordinary notification errors are reported, not editor panics.
                self.report_lua_error(error);
            }
        }
    }
    pub(crate) fn command(self: &Rc<Self>, command: &str) -> Result<bool> {
        let command = command.trim().trim_start_matches(':').trim();
        let Some(raw_name) = command.split_whitespace().next() else {
            return Ok(true);
        };
        // Structural addresses and registered commands belong to the typed
        // command parser; only direct option argv use this local adapter.
        let words = if raw_name == "set" {
            match crate::workspace::command_words(command) {
                Ok(words) => words,
                Err(error) => {
                    self.messages.borrow_mut().push(error.to_string());
                    return Ok(false);
                }
            }
        } else {
            Vec::new()
        };
        if raw_name == "set" {
            match self.command_option(&words[1..]) {
                Ok(Some(_)) => return Ok(true),
                Ok(None) => {}
                Err(error) => {
                    self.messages.borrow_mut().push(error.to_string());
                    return Ok(true);
                }
            }
        }
        let prepared = { self.workspace.borrow_mut().prepare_command(command) };
        let plan = match prepared {
            Ok(plan) => plan,
            Err(error) => {
                self.messages.borrow_mut().push(error.to_string());
                return Ok(false);
            }
        };
        let completed = match self.run_plan(plan) {
            Ok(_) => true,
            Err(error) => {
                let rejected = error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(crate::workspace::command_rejected);
                self.messages.borrow_mut().push(error.to_string());
                if rejected {
                    return Ok(false);
                }
                false
            }
        };
        if completed {
            let mut workspace = self.workspace.borrow_mut();
            workspace.finish_command();
        }
        Ok(true)
    }
    fn target_command(self: &Rc<Self>, request: &crate::sam::HostRequest) -> Result<bool> {
        if self.workspace.borrow().window_file_id(request.window) != Some(request.file) {
            return Ok(false);
        }
        if let crate::sam::HostCommand::User(name) = &request.command {
            let function = self.commands.borrow().get(name).cloned();
            let Some(function) = function else {
                return Ok(false);
            };
            let argv = self.lua.create_table()?;
            argv.set(0, name.as_str())?;
            for (index, argument) in request.args.iter().enumerate() {
                argv.set(index + 1, argument.as_str())?;
            }
            let window = objects::window(self, request.window)?;
            let selection = match request.selection {
                Some(id) => objects::selection_by_raw_id(self, request.window, id)?
                    .map(Value::UserData)
                    .unwrap_or(Value::Nil),
                None => window.get("selection")?,
            };
            let range = objects::push_range(&self.lua, request.range.clone())?;
            return match function.call::<Value>((argv, request.force, window, selection, range)) {
                Ok(value) => Ok(truth(&value)),
                Err(error) => {
                    self.report_lua_error(error);
                    Ok(false)
                }
            };
        }
        if request.command == crate::sam::HostCommand::Set {
            if let Some(success) = self.command_option_for(&request.args, Some(request.window))? {
                return Ok(success);
            }
        }
        let action = self
            .workspace
            .borrow_mut()
            .execute_target_command(request)
            .map_err(mlua::Error::external)?;
        match action {
            crate::editor::EditorAction::Continue => Ok(true),
            crate::editor::EditorAction::Message(message) => {
                self.messages.borrow_mut().push(message);
                Ok(false)
            }
            _ => Err(mlua::Error::runtime(
                "unexpected native target command action",
            )),
        }
    }
    fn command_option(self: &Rc<Self>, words: &[String]) -> Result<Option<bool>> {
        self.command_option_for(words, None)
    }
    fn command_option_for(
        self: &Rc<Self>,
        words: &[String],
        window: Option<usize>,
    ) -> Result<Option<bool>> {
        if words.is_empty() {
            return Ok(None);
        }
        let raw = &words[0];
        let (raw, value_from_eq) = raw
            .split_once('=')
            .map(|(a, b)| (a, Some(b)))
            .unwrap_or((raw.as_str(), None));
        let toggle = raw.ends_with('!');
        let raw = raw.trim_end_matches('!');
        let (name, negated) =
            if self.options.borrow().contains_key(raw) || builtin_option_kind(raw).is_some() {
                (raw, false)
            } else if let Some(n) = raw.strip_prefix("no") {
                (n, true)
            } else {
                (raw, false)
            };
        let kind = self
            .options
            .borrow()
            .get(name)
            .map(|option| option.kind)
            .or_else(|| builtin_option_kind(name));
        let Some(kind) = kind else { return Ok(None) };
        let text = value_from_eq.or_else(|| words.get(1).map(String::as_str));
        let value = match kind {
            OptionKind::Bool => Value::Boolean(if negated || (toggle && text.is_none()) {
                false
            } else {
                match text {
                    Some("false" | "off" | "0") => false,
                    Some("true" | "on" | "1") | None => true,
                    Some(_) => return Ok(Some(false)),
                }
            }),
            OptionKind::String => match text {
                Some(text) => Value::String(self.lua.create_string(text)?),
                None => Value::Nil,
            },
            OptionKind::Number => match text {
                Some(text) => Value::Integer(text.parse().map_err(mlua::Error::external)?),
                None => Value::Nil,
            },
        };
        let target = match (window, self.option_scope(name)) {
            (Some(id), Some(OptionScope::Window)) => OptionTarget::Window(id),
            (Some(_), Some(OptionScope::Global)) => OptionTarget::Global,
            _ => OptionTarget::Command,
        };
        self.set_option(name, value, toggle, target).map(Some)
    }
    fn handle_key(self: &Rc<Self>, key: Key) -> Result<Outcome> {
        let action = { self.workspace.borrow_mut().editor.handle_key(key) }
            .map_err(mlua::Error::external)?;
        self.drive_action_queue(action)
    }
    pub(crate) fn close_window(self: &Rc<Self>, id: usize, force: bool) -> Result<bool> {
        let plan = { self.workspace.borrow_mut().begin_close_window(id, force) }
            .map_err(mlua::Error::external)?;
        self.run_plan(plan)?;
        Ok(true)
    }
    pub(crate) fn change_window_file(self: &Rc<Self>, id: usize, path: &str) -> Result<bool> {
        let plan = {
            self.workspace
                .borrow_mut()
                .begin_change_window_file(id, Path::new(path))
        }
        .map_err(mlua::Error::external)?;
        self.run_plan(plan)?;
        Ok(true)
    }
}

impl Runtime {
    /// Supply measured frame geometry before emitting redraw callbacks.
    pub fn set_viewport(
        &self,
        window: usize,
        width: usize,
        height: usize,
        total_height: usize,
        bytes: std::ops::Range<usize>,
        lines: std::ops::Range<usize>,
    ) {
        self.inner.draw_records.borrow_mut().remove(&window);
        let mut state = self.inner.object_state.borrow_mut();
        let visual = state.windows.entry(window).or_default();
        visual.width = width;
        visual.height = total_height;
        visual.viewport_width = width;
        visual.viewport_height = height;
        visual.viewport_bytes = bytes;
        visual.viewport_lines = lines;
        visual.ranges.clear();
        visual.cells.clear();
    }
    pub fn redraw(&self) -> Result<()> {
        self.inner.redraw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn native_command_registry_tracks_original_exact_names() {
        let definitions = include_str!("../../../sam.c")
            .split_once("static const CommandDef vis_sam_command_definitions[] = {")
            .unwrap()
            .1
            .split_once("\n};")
            .unwrap()
            .0;
        let names: Vec<_> = definitions
            .lines()
            .filter_map(|line| line.split_once("str8_comp(\""))
            .map(|(_, rest)| rest.split_once('"').unwrap().0)
            .collect();
        assert_eq!(NATIVE_COMMANDS, names);
    }

    #[test]
    fn unsafe_libraries_stay_unavailable_to_configuration() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime
            .eval(
                r#"
            assert(ffi == nil, 'ffi')
            local ok = pcall(function() return package.loadlib('/nonexistent', 'luaopen_x') end)
            assert(not ok, 'pcall')
            "#,
            )
            .unwrap();
    }

    #[test]
    fn the_curated_debug_library_reports_source_locations() {
        let dir = Directory::new(
            r#"
            local info = debug.getinfo(1, "Sl")
            assert(type(info) == "table")
            assert(info.short_src:find("visrc.lua") ~= nil)
            assert(type(info.currentline) == "number")
            assert(type(info.what) == "string")
            assert(type(debug.traceback("here")) == "string")
            assert(debug.getinfo(99) == nil)
            "#,
        );
        let runtime = dir.runtime();
        runtime.eval("require('vis')").unwrap();
    }
    struct Directory(PathBuf);
    impl Directory {
        fn new(config: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "vis-lua-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("visrc.lua"), config).unwrap();
            Self(path)
        }
        fn runtime(&self) -> Runtime {
            Runtime::new(std::slice::from_ref(&self.0)).unwrap()
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn config_init_precedes_open_and_reentrant_events_keep_live_identity() {
        let dir = Directory::new(
            r#"
            require('vis')
            log={}
            vis.events.subscribe(vis.events.INIT,function()
                assert(vis.win == nil)
                table.insert(log,'init')
                vis:option_register('inside','bool',function(value)
                    vis:info('inside callback')
                    vis.inside=value
                end,function() return vis.inside end)
                assert(vis:command('set inside true'))
            end)
            vis.events.subscribe(vis.events.FILE_OPEN,function(file)
                table.insert(log,'file')
                file.marker='persistent'
                file:insert(0,'a')
            end)
            vis.events.subscribe(vis.events.WIN_OPEN,function(win)
                table.insert(log,'window')
                assert(win.file.marker=='persistent')
                assert(vis.win == win)
                vis:feedkeys('A!<Escape>')
            end)
            vis.events.subscribe(vis.events.START,function() table.insert(log,'start') end)
        "#,
        );
        let runtime = dir.runtime();
        runtime.open_scratch(None, false).unwrap();
        runtime.start().unwrap();
        runtime.eval("assert(table.concat(log,',')=='init,file,window,start'); assert(vis.win.file:content(0,vis.win.file.size)=='a!');assert(vis.options.inside==true)").unwrap();
    }
    #[test]
    fn presave_recomputes_whole_file_after_growing_and_shrinking() {
        for replacement in ["longer replacement", ""] {
            let dir = Directory::new("require('vis')");
            let path = dir.0.join("save");
            std::fs::write(&path, "old").unwrap();
            let runtime = dir.runtime();
            runtime.open_cli_path(&path, None, false).unwrap();
            runtime
                .lua()
                .globals()
                .set("replacement", replacement)
                .unwrap();
            runtime
                .eval(
                    r#"
                posts=0
                vis.events.subscribe(vis.events.FILE_SAVE_PRE,function(file,path)
                    file:delete(0,file.size);file:insert(0,replacement)
                end)
                vis.events.subscribe(vis.events.FILE_SAVE_POST,function() posts=posts+1 end)
            "#,
                )
                .unwrap();
            assert!(runtime.command("w").unwrap());
            assert_eq!(std::fs::read(&path).unwrap(), replacement.as_bytes());
            runtime.eval("assert(posts==1)").unwrap();
        }
    }
    #[test]
    fn presave_veto_keeps_edits_force_bypasses_and_post_only_follows_success() {
        let dir = Directory::new("require('vis')");
        let path = dir.0.join("save");
        std::fs::write(&path, "old").unwrap();
        let runtime = dir.runtime();
        runtime.open_cli_path(&path, None, false).unwrap();
        runtime
            .eval(
                r#"
            posts=0
            vis.events.subscribe(vis.events.FILE_SAVE_PRE,function(file)
                file:insert(file.size,'!');return false
            end)
            vis.events.subscribe(vis.events.FILE_SAVE_POST,function() posts=posts+1 end)
        "#,
            )
            .unwrap();
        // A veto prevents the write, while C's command API reports acceptance.
        assert!(runtime.command("w").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        runtime
            .eval("assert(vis.win.file:content(0,vis.win.file.size)=='old!');assert(posts==0)")
            .unwrap();
        assert!(runtime.command("w!").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"old!!");
        runtime.eval("assert(posts==1)").unwrap();
    }
    #[test]
    fn callback_mapping_can_feedkeys_synchronously_and_request_more_input() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime.open_scratch(None, false).unwrap();
        runtime
            .eval(
                r#"
            vis:map(vis.modes.NORMAL,'K',function() vis:feedkeys('iNested<Escape>') end)
            vis:feedkeys('K')
            assert(vis.win.file:content(0,vis.win.file.size)=='Nested')
            vis:map(vis.modes.INSERT,'<C-z>',function(keys)
                if #keys<2 then return -1 end
                vis:insert(keys:sub(1,2));return 2
            end)
            vis:feedkeys('A<C-z>a')
            assert(vis.win.file:content(0,vis.win.file.size)=='Nested')
            vis:feedkeys('b<Escape>')
            assert(vis.win.file:content(0,vis.win.file.size)=='Nestedab')
        "#,
            )
            .unwrap();
    }
    #[test]
    fn presave_remains_on_original_target_when_callback_changes_focus() {
        let dir = Directory::new("require('vis')");
        let a = dir.0.join("a.txt");
        let b = dir.0.join("b.txt");
        std::fs::write(&a, "alpha").unwrap();
        std::fs::write(&b, "beta").unwrap();
        let runtime = dir.runtime();
        runtime.open_cli_path(&a, None, false).unwrap();
        runtime.eval("save_target_window=vis.win").unwrap();
        runtime.open_cli_path(&b, None, false).unwrap();
        runtime
            .eval(
                r#"
            local other=vis.win
            vis.win=save_target_window
            vis.events.subscribe(vis.events.FILE_SAVE_PRE,function(file)
                file:insert(file.size,'!')
                vis.win=other
            end)
        "#,
            )
            .unwrap();
        assert!(runtime.command("w").unwrap());
        assert_eq!(std::fs::read(&a).unwrap(), b"alpha!");
        assert_eq!(std::fs::read(&b).unwrap(), b"beta");
    }
    #[test]
    fn close_callbacks_keep_objects_valid_and_then_invalidate_saved_handles() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime.open_scratch(None, false).unwrap();
        runtime
            .eval(
                r#"
            saved=vis.win;saved_file=saved.file;events={}
            vis.events.subscribe(vis.events.WIN_CLOSE,function(win)
                assert(win==saved);assert(win.file==saved_file);table.insert(events,'window')
                assert(vis:command('pwd'))
            end)
            vis.events.subscribe(vis.events.FILE_CLOSE,function(file)
                assert(file==saved_file);assert(file.size==0);table.insert(events,'file')
            end)
        "#,
            )
            .unwrap();
        assert!(runtime.command("q").unwrap());
        runtime.eval("assert(table.concat(events,',')=='window,file');assert(not pcall(function()return saved.file end));assert(not pcall(function()return saved_file.size end))").unwrap();
    }
    #[test]
    fn feedkeys_accumulates_pending_mapping_input_one_symbolic_key_at_a_time() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime.open_scratch(None, false).unwrap();
        runtime
            .eval(
                r#"
            vis:map(vis.modes.INSERT,'<C-z>',function(keys)
                observed=keys
                if #keys<2 then return -1 end
                vis:insert(keys:sub(1,2))
                return 2
            end)
            vis:feedkeys('i<C-z>ab<Escape>')
            assert(observed=='ab')
            assert(vis.mode==vis.modes.NORMAL)
            assert(vis.win.file:content(0,vis.win.file.size)=='ab')
        "#,
            )
            .unwrap();
    }
    #[test]
    fn keyboard_commands_route_custom_options_and_registered_handlers() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime.open_scratch(None, false).unwrap();
        runtime.eval(r#"
            vis:option_register('custom','number',function(value)vis.custom=value end,function()return vis.custom end)
            vis:command_register('customcmd',function(argv,force,win,selection,range)
                assert(argv[0]=='customcmd');assert(argv[1]=='argument');assert(force)
                assert(win==vis.win)
                assert(selection.number==win.selection.number and selection.pos==win.selection.pos)
                assert(range.start==0 and range.finish==win.file.size)
                vis:insert('ok');return true
            end)
            vis:feedkeys(':set custom 42<Enter>:customcmd! argument<Enter>')
            assert(vis.options.custom==42)
            assert(vis.win.file:content(0,vis.win.file.size)=='ok')
        "#).unwrap();
    }
    #[test]
    fn registered_actions_and_nested_feedkeys_preserve_later_input() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime.open_scratch(None, false).unwrap();
        runtime
            .eval(
                r#"
            local action=vis:action_register('custom-action',function()
                vis:feedkeys('iabc<Escape>')
            end,'Custom action help')
            assert(getmetatable(action)==vis.types.keyaction)
            assert(vis:map(vis.modes.NORMAL,'K',action))
            assert(vis:mappings(vis.modes.NORMAL).K=='Custom action help')
            vis:feedkeys('K0x')
            assert(vis.win.file:content(0,vis.win.file.size)=='bc',vis.win.file:content(0,vis.win.file.size))
            vis.registers.a={'one','two\0'}
            assert(vis.registers.a[1]=='one' and vis.registers.a[2]=='two\0')
            vis:option_register('temporary','bool',function()end)
            assert(vis:option_unregister('temporary'))
            assert(vis:command('set temporary true'))
        "#,
            )
            .unwrap();
    }
    #[test]
    fn custom_motions_apply_count_to_each_live_selection() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime
            .open_scratch(Some(b"abcdefghijkl".to_vec()), false)
            .unwrap();
        runtime
            .workspace()
            .borrow_mut()
            .editor
            .create_selection(
                crate::selection::Selection {
                    anchor: 5,
                    cursor: 5,
                },
                false,
            )
            .unwrap();
        runtime
            .eval(
                r#"
            calls=0
            vis:motion_new('Q',function(win,pos)calls=calls+1;return pos+1 end)
            vis:feedkeys('2Q')
            assert(calls==4)
            assert(vis.win.selections[1].pos==2 and vis.win.selections[2].pos==7)
        "#,
            )
            .unwrap();
    }
    #[test]
    fn custom_textobjects_drive_native_operators_for_multiple_selections() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime
            .open_scratch(Some(b"abcdefghijkl".to_vec()), false)
            .unwrap();
        runtime
            .workspace()
            .borrow_mut()
            .editor
            .create_selection(
                crate::selection::Selection {
                    anchor: 5,
                    cursor: 5,
                },
                false,
            )
            .unwrap();
        runtime
            .eval(
                r#"
            calls=0
            vis:textobject_new('Q',function(win,pos)calls=calls+1;return pos,pos+2 end)
            vis:feedkeys('dQ')
            assert(calls==2)
            assert(vis.win.file:content(0,vis.win.file.size)=='cdehijkl')
        "#,
            )
            .unwrap();
    }
    #[test]
    fn message_window_reuses_internal_readonly_file_without_external_events() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime.open_scratch(None, false).unwrap();
        runtime
            .eval(
                r#"
            normal=vis.win;opened=0;files=0
            vis.events.subscribe(vis.events.WIN_OPEN,function()opened=opened+1 end)
            vis.events.subscribe(vis.events.FILE_OPEN,function()files=files+1 end)
            vis:message('first')
            message=vis.win;message_file=message.file
            assert(message~=normal and opened==0 and files==0)
            assert(message.file:content(0,message.file.size)=='first\n')
            assert(not message.file.modified)
            local count=0;for file in vis:files()do count=count+1 end;assert(count==3)
            vis:message('second')
            assert(vis.win==message and opened==0)
            assert(message.file:content(0,message.file.size)=='first\nsecond\n')
            assert(vis:command('q!'))
            vis:message('third')
            assert(opened==0 and vis.win.file==message_file)
            assert(vis.win.file:content(0,vis.win.file.size)=='first\nsecond\nthird\n')
        "#,
            )
            .unwrap();
    }
    #[test]
    fn inactive_window_file_assignment_preserves_window_identity_and_focus() {
        let dir = Directory::new("require('vis')");
        let a = dir.0.join("a.txt");
        let b = dir.0.join("b.txt");
        let c = dir.0.join("c.txt");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        std::fs::write(&c, "c").unwrap();
        let runtime = dir.runtime();
        runtime.open_cli_path(&a, None, false).unwrap();
        runtime
            .eval("first=vis.win;first_file=first.file;first_selection=first.selection")
            .unwrap();
        runtime.open_cli_path(&b, None, false).unwrap();
        runtime
            .lua()
            .globals()
            .set("target", c.to_string_lossy().as_ref())
            .unwrap();
        runtime
            .eval(
                r#"
            focused=vis.win;opened=0;closed=0
            vis.events.subscribe(vis.events.WIN_OPEN,function()opened=opened+1 end)
            vis.events.subscribe(vis.events.WIN_CLOSE,function()closed=closed+1 end)
            first.file=target
            assert(vis.win==focused)
            assert(first.selection.number==first_selection.number and first.selection.pos==first_selection.pos)
            assert(opened==0 and closed==0)
            assert(first.file:content(0,first.file.size)=='c')
            assert(not pcall(function()return first_file.size end))
        "#,
            )
            .unwrap();
    }
    #[test]
    fn input_events_follow_mappings_and_can_consume_or_reenter_literal_insertion() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime.open_scratch(None, false).unwrap();
        runtime
            .eval(
                r#"
            inputs={};local source=vis.win
            vis.events.subscribe(vis.events.INPUT,function(key)
                table.insert(inputs,key)
                if key=='x'then vis:insert('!');return true end
                if key=='z'then error('input hook error')end
            end)
            vis:map(vis.modes.INSERT,'a','b')
            vis:map(vis.modes.INSERT,'k',function()vis:insert('M')end)
            vis:feedkeys('iaxk<Enter>z<Escape>')
            assert(table.concat(inputs,',')=='b,x,z')
            assert(vis.win~=source)
            assert(source.file:content(0,source.file.size)=='b!M\n\n')
            assert(vis.win.file:content(0,vis.win.file.size):sub(1,1)=='z')
            vis:message('internal')
            vis:feedkeys('iQ<Escape>')
            assert(table.concat(inputs,',')=='b,x,z')
        "#,
            )
            .unwrap();
        assert!(
            runtime
                .take_lua_errors()
                .iter()
                .any(|message| message.contains("input hook error"))
        );
    }
    #[test]
    fn startup_and_coroutine_lua_can_be_interrupted_before_terminal_initialization() {
        let dir = Directory::new("while true do end");
        let interrupt = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&interrupt);
        let sender = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            signal.store(true, Ordering::Relaxed);
        });
        let runtime =
            Runtime::new_with_interrupt(std::slice::from_ref(&dir.0), Arc::clone(&interrupt))
                .unwrap();
        sender.join().unwrap();
        assert!(
            runtime
                .take_lua_errors()
                .iter()
                .any(|message| message.contains("Lua execution interrupted"))
        );
        let sender = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            interrupt.store(true, Ordering::Relaxed);
        });
        runtime.eval("local co=coroutine.create(function()while true do end end);local ok,error=coroutine.resume(co);assert(not ok and tostring(error):find('interrupted'))").unwrap();
        sender.join().unwrap();
        runtime.eval("assert(1+1==2)").unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn communicate_success_has_one_result_and_accepts_numeric_strings() {
        let dir = Directory::new("");
        let runtime = dir.runtime();
        runtime.eval("local results=table.pack(vis:communicate(123,456));assert(results.n==1);assert(io.type(results[1])=='file');results[1]:close()").unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn communicate_delivers_binary_responses_and_allows_reentrant_process_callbacks() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime.open_scratch(None, false).unwrap();
        runtime
            .eval(
                r#"
            output='';errors='';terminal={}
            vis.events.subscribe(vis.events.PROCESS_RESPONSE,function(name,kind,code,data)
                assert(vis:command('pwd'))
                if name=='first' and kind=='STDOUT'then output=output..data end
                if name=='first' and kind=='STDERR'then errors=errors..data end
                if kind=='EXIT'or kind=='SIGNAL'then
                    assert(terminal[name]==nil);terminal[name]={kind,code}
                    if name=='first'then follow=assert(vis:communicate('second','printf second'))end
                end
            end)
            stream=assert(vis:communicate('first','head -c 3; printf ERR >&2'))
            assert(io.type(stream)=='file')
            assert(stream:write('a\0b'));assert(stream:flush())
        "#,
            )
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while runtime.has_processes() && std::time::Instant::now() < deadline {
            runtime.poll_processes().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!runtime.has_processes(), "processes failed to finish");
        runtime.eval("assert(output=='a\\0b');assert(errors=='ERR');assert(terminal.first[1]=='EXIT'and terminal.first[2]==0);assert(terminal.second[1]=='EXIT'and terminal.second[2]==0)").unwrap();
    }
    #[test]
    fn structural_commands_preserve_mark_and_literal_quote_syntax() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime
            .open_scratch(Some(b"source\n".to_vec()), false)
            .unwrap();
        assert!(runtime.command("'ap").unwrap());
        assert!(runtime.command("0i/'hello/").unwrap());
        runtime
            .eval("assert(vis.win.file:content(0,vis.win.file.size)==\"'hellosource\\n\")")
            .unwrap();
        assert!(!runtime.command("x/[/p").unwrap());
        assert!(!runtime.command("not-a-command").unwrap());
    }
    #[test]
    fn prompt_parser_result_retains_unknown_command_and_closes_success() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime
            .open_scratch(Some(b"source".to_vec()), false)
            .unwrap();
        runtime.eval(r#"
            source=vis.win
            vis:command_register('successfulprompt',function()assert(vis.win==source);return true end)
            vis:feedkeys(':missingpromptcommand')
            prompt=vis.win;history=prompt.file
            vis:feedkeys('<Enter>')
            assert(vis.win==prompt and vis.mode==vis.modes.INSERT)
            assert(history:content(0,history.size)==':missingpromptcommand\n')
            history:delete(0,history.size);history:insert(0,':successfulprompt\n');prompt.selection.pos=history.size-1
            vis:feedkeys('<Enter>')
            assert(vis.win==source and vis.mode==vis.modes.NORMAL)
            assert(not pcall(function()return prompt.file end))
        "#).unwrap();
    }
    #[test]
    fn successful_reentrant_command_closes_only_its_original_prompt() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime.open_scratch(None, false).unwrap();
        runtime
            .eval(
                r#"
            source=vis.win
            vis:command_register('nestedprompt',function()
                assert(vis.win==source)
                vis:feedkeys(':childdraft')
                child=vis.win
                return true
            end)
            vis:feedkeys(':nestedprompt');original_prompt=vis.win
            vis:feedkeys('<Enter>')
            assert(child~=source and child~=original_prompt)
            assert(vis.win==child and vis.mode==vis.modes.NORMAL)
            assert(not pcall(function()return original_prompt.file end))
            assert(child.file:content(0,child.file.size):find(':childdraft\n',1,true))
        "#,
            )
            .unwrap();
    }
    #[test]
    fn native_option_scopes_and_user_fields_remain_distinct() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime.open_scratch(None, false).unwrap();
        runtime.eval(r#"
            local source=vis.win
            assert(getmetatable(vis.options)==vis.types.visoptions)
            assert(type(vis.options.shell)=='string')
            assert(vis.options.shell~='')
            vis.options.shell='/bin/dash'
            assert(vis.options.shell=='/bin/dash')
            vis.options.escdelay=50
            assert(vis.options.escdelay==50)
            source.options.tabwidth=4
            assert(vis.options.tabwidth==nil and source.options.autoindent==nil)
            vis.options.tabwidth=99
            source.options.autoindent=false
            vis.options.autoindent=true
            assert(source.options.tabwidth==4 and vis.options.tabwidth==99)
            assert(vis.options.autoindent and source.options.autoindent==false)
            local calls,value=0,false
            vis:option_register('scopedcustom','bool',function(v)calls=calls+1;value=v end,function()return value end)
            assert(source.options.scopedcustom==nil)
            source.options.scopedcustom=true
            assert(calls==0 and source.options.scopedcustom and not vis.options.scopedcustom)
            vis.options.scopedcustom=true
            assert(calls==1 and vis.options.scopedcustom)
            vis.options.shadowed=3
            vis:option_register('shadowed','number',function()end,function()return 9 end)
            assert(vis.options.shadowed==9)
            assert(vis:option_unregister('shadowed') and vis.options.shadowed==3)
            assert(vis:command('set tabwidth 6'))
            assert(source.options.tabwidth==6 and vis.options.tabwidth==99)
            assert(vis:command('open'))
            assert(vis.options.autoindent and vis.win.options.autoindent==nil)
            assert(vis.win.options.tabwidth==8)
            vis.win=source
            assert(vis.options.autoindent and source.options.tabwidth==6)
        "#).unwrap();
    }
    #[test]
    fn action_registration_rejects_native_and_dynamic_name_collisions() {
        let dir = Directory::new("require('vis')");
        let runtime = dir.runtime();
        runtime.open_scratch(None, false).unwrap();
        runtime.eval(r#"
            calls=0
            assert(vis:action_register('vis-nop',function()error('native overwritten')end)==nil)
            local action=vis:action_register('phase-five-action',function()calls=calls+1 end)
            assert(action~=nil)
            assert(vis:action_register('phase-five-action',function()error('dynamic overwritten')end)==nil)
            vis:feedkeys('<vis-nop><phase-five-action>')
            assert(calls==1)
        "#).unwrap();
    }
    #[test]
    fn style_allocation_is_independent_of_definitions_and_bounded() {
        let dir = Directory::new("");
        let runtime = dir.runtime();
        runtime
            .eval(
                r#"
            local ui=vis.ui
            assert(ui:style_define(13,'unknown-token,fore:invalid'))
            assert(not ui:style_define(500,'bold'))
            assert(ui:style_push('unknown-token')==14)
            assert(ui:style_define(14,'fore:red'))
            assert(not ui:style_define(16,'bold'))
            assert(ui:style_push()==15)
            assert(ui:style_push('')==16)
            for id=17,511 do assert(ui:style_push()==id)end
            assert(ui:style_push()==nil)
            assert(ui:style_define(511,'fore:blue'))
            assert(not ui:style_define(512,''))
        "#,
            )
            .unwrap();
        assert_eq!(runtime.inner.style_snapshot(15), Style::terminal_default());
        assert_eq!(runtime.inner.style_snapshot(16), Style::default());
    }
    #[test]
    fn configuration_errors_focus_a_real_internal_message_window() {
        let dir = Directory::new("error('broken configuration')");
        let runtime = dir.runtime();
        let ws = runtime.workspace();
        let ws = ws.borrow();
        assert_eq!(ws.window_count(), 1);
        assert!(ws.external_window_ids().is_empty());
        drop(ws);
        runtime.eval("assert(vis.win.file:content(0,vis.win.file.size):find('broken configuration',1,true))").unwrap();
        assert_eq!(runtime.take_lua_errors().len(), 1);
        assert!(runtime.take_messages().is_empty());
    }
    #[test]
    fn structural_callbacks_keep_explicit_targets_and_reentrant_focus() {
        let dir = Directory::new("require('vis')");
        let path = dir.0.join("target.txt");
        std::fs::write(&path, b"abc").unwrap();
        let runtime = dir.runtime();
        runtime.open_cli_path(&path, None, false).unwrap();
        runtime
            .eval("target=vis.win;target_selection=target.selection")
            .unwrap();
        runtime
            .open_scratch(Some(b"other".to_vec()), false)
            .unwrap();
        runtime
            .eval(
                r#"
            chosen=vis.win;calls=0
            assert(vis:command_register('targetprobe',function(argv,force,win,selection,range)
                calls=calls+1
                assert(argv[0]=='targetprobe'and argv[1]=='two words'and force)
                assert(win==target and vis.win==chosen)
                assert(selection.number==target_selection.number and selection.pos==target_selection.pos)
                assert(range.start==0 and range.finish==2)
                assert(vis:command('pwd'))
                win.file:insert(0,'!')
                vis.win=target
                return true
            end))
            assert(vis:command("X/target[.]txt/ { #0,#2 targetprobe! 'two words'\n #0i/Q/ }"))
            assert(calls==1 and vis.win==target)
            assert(target.file:content(0,target.file.size)=='Q!abc')
            assert(chosen.file:content(0,chosen.file.size)=='other')
        "#,
            )
            .unwrap();
    }
    #[test]
    fn structural_handler_false_short_circuits_without_rejecting_the_command() {
        let dir = Directory::new("require('vis')");
        let path = dir.0.join("target.txt");
        std::fs::write(&path, b"abc").unwrap();
        let runtime = dir.runtime();
        runtime.open_cli_path(&path, None, false).unwrap();
        runtime
            .eval(
                r#"
            calls=0
            assert(vis:command_register('rejecttarget',function()calls=calls+1;return false end))
            assert(vis:command('X/target[.]txt/ { #0i/Q/ rejecttarget\n #0i/NO/ }'))
            assert(calls==1)
            assert(vis.win.file:content(0,vis.win.file.size)=='Qabc')
        "#,
            )
            .unwrap();
    }
    #[test]
    fn structural_transcript_rejection_after_a_callback_returns_false() {
        let dir = Directory::new("require('vis')");
        let path = dir.0.join("target.txt");
        std::fs::write(&path, b"abc").unwrap();
        let runtime = dir.runtime();
        runtime.open_cli_path(&path, None, false).unwrap();
        runtime
            .eval(
                r#"
            calls=0
            assert(vis:command_register('touch',function()calls=calls+1;return true end))
            assert(not vis:command('X/target[.]txt/ { touch\n #0i/Q/ w }'))
            assert(calls==1)
            assert(vis.win.file:content(0,vis.win.file.size)=='abc')
        "#,
            )
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"abc");
    }
    #[test]
    fn structural_options_and_window_maps_apply_to_the_inactive_target() {
        let dir = Directory::new("require('vis')");
        let path = dir.0.join("target.txt");
        std::fs::write(&path, b"abc").unwrap();
        let runtime = dir.runtime();
        runtime.open_cli_path(&path, None, false).unwrap();
        runtime.eval("target=vis.win").unwrap();
        runtime
            .open_scratch(Some(b"other".to_vec()), false)
            .unwrap();
        runtime
            .eval(
                r#"
            chosen=vis.win
            assert(vis:command('X/target[.]txt/ set tabwidth 3'))
            assert(target.options.tabwidth==3 and chosen.options.tabwidth==8 and vis.win==chosen)
            assert(vis:option_register('targetoption','bool',function(value)
                assert(value and vis.win==chosen);return false
            end))
            assert(vis:command('X/target[.]txt/ { set targetoption true\n #0i/NO/ }'))
            assert(target.file:content(0,target.file.size)=='abc')
            assert(vis:command('X/target[.]txt/ map-window normal Z iZ<Escape>'))
            assert(vis.win==chosen)
            vis.win=target;vis:feedkeys('Z')
            assert(target.file:content(0,target.file.size)=='Zabc')
        "#,
            )
            .unwrap();
    }
}

impl Inner {
    fn store_callback(&self, function: Function) -> u64 {
        let id = self.next_callback.get();
        self.next_callback.set(id + 1);
        self.callbacks.borrow_mut().insert(id, function);
        id
    }
    fn register_callback(&self, kind: CallbackType, function: Function) -> Result<u64> {
        let id = self.store_callback(function);
        self.workspace
            .borrow_mut()
            .editor
            .register_callback(kind, id)
            .map_err(mlua::Error::external)?;
        Ok(id)
    }
    fn register_action(
        self: &Rc<Self>,
        name: &str,
        function: Function,
    ) -> Result<Option<AnyUserData>> {
        if self.workspace.borrow().editor.action_exists(name) {
            return Ok(None);
        }
        let id = self.register_callback(CallbackType::Mapping, function)?;
        self.workspace
            .borrow_mut()
            .editor
            .register_action(name, id)
            .map_err(mlua::Error::external)?;
        self.lua
            .create_userdata(KeyAction {
                context: Rc::downgrade(self),
                id,
            })
            .map(Some)
    }
    pub(crate) fn map(self: &Rc<Self>, window: Option<usize>, args: MultiValue) -> Result<bool> {
        let mode = number_mode(integer_arg(&args, 0)?)?;
        let key = string_arg(&args, 1)?;
        match args.get(2) {
            Some(Value::String(alias)) => self
                .workspace
                .borrow_mut()
                .editor
                .map_alias(mode, &key, &alias.to_str()?, window.map(|id| id as u64))
                .map_err(mlua::Error::external)?,
            Some(Value::Function(function)) => {
                let id = self.register_callback(CallbackType::Mapping, function.clone())?;
                if let Some(Value::String(help)) = args.get(3) {
                    self.callback_help
                        .borrow_mut()
                        .insert(id, help.to_str()?.to_string());
                }
                self.workspace
                    .borrow_mut()
                    .editor
                    .map_callback(mode, &key, id, window.map(|id| id as u64))
                    .map_err(mlua::Error::external)?;
            }
            Some(Value::UserData(action)) => {
                let action = action.borrow::<KeyAction>()?.clone();
                if !Rc::ptr_eq(self, &upgrade(&action.context)?) {
                    return Err(mlua::Error::runtime("action belongs to another runtime"));
                }
                self.workspace
                    .borrow_mut()
                    .editor
                    .map_callback(mode, &key, action.id, window.map(|id| id as u64))
                    .map_err(mlua::Error::external)?;
            }
            _ => {
                return Err(mlua::Error::runtime(
                    "mapping must be a string, function, or key action",
                ));
            }
        }
        Ok(true)
    }
    pub(crate) fn unmap(self: &Rc<Self>, window: Option<usize>, args: MultiValue) -> Result<bool> {
        let mode = number_mode(integer_arg(&args, 0)?)?;
        let key = string_arg(&args, 1)?;
        self.workspace
            .borrow_mut()
            .editor
            .unmap(mode, &key, window.map(|id| id as u64))
            .map_err(mlua::Error::external)
    }
    fn invoke_callback(self: &Rc<Self>, kind: CallbackType, id: u64) -> Result<bool> {
        let pos = self.workspace.borrow().editor.cursor;
        let kind = match kind {
            CallbackType::Motion => CallbackKind::Motion { pos },
            CallbackType::Operator => CallbackKind::Operator {
                range: pos..pos,
                pos,
            },
            CallbackType::TextObject => CallbackKind::TextObject { pos },
            CallbackType::Mapping => CallbackKind::Mapping {
                keys: String::new(),
            },
        };
        let action = { self.workspace.borrow_mut().editor.invoke_callback(kind, id) }
            .map_err(mlua::Error::external)?;
        let plan = self
            .workspace
            .borrow_mut()
            .begin_action(action)
            .map_err(mlua::Error::external)?;
        self.run_plan(plan)?;
        Ok(true)
    }
    fn dispatch_callback(self: &Rc<Self>, request: CallbackRequest) -> Result<CallbackResponse> {
        let function = self
            .callbacks
            .borrow()
            .get(&request.callback)
            .cloned()
            .ok_or_else(|| mlua::Error::runtime("unknown Lua callback"))?;
        let window_id = request
            .window
            .map(|id| id as usize)
            .unwrap_or_else(|| self.workspace.borrow().active_window_id());
        let win = objects::window(self, window_id)?;
        match request.kind {
            CallbackKind::Mapping { keys } => {
                let response: Value = function.call(keys)?;
                Ok(CallbackResponse::Mapping(
                    exact_integer(&response).and_then(|n| isize::try_from(n).ok()),
                ))
            }
            CallbackKind::Motion { pos } => {
                let response: Value = function.call((win, pos))?;
                Ok(CallbackResponse::Position(if response.is_nil() {
                    None
                } else {
                    Some(objects::position(response)?)
                }))
            }
            CallbackKind::Operator { range, pos } => {
                let file: AnyUserData = win.get("file")?;
                let response: Value =
                    function.call((file, objects::push_range(&self.lua, range)?, pos))?;
                Ok(CallbackResponse::Position(if response.is_nil() {
                    None
                } else {
                    Some(objects::position(response)?)
                }))
            }
            CallbackKind::TextObject { pos } => {
                let values: MultiValue = function.call((win, pos))?;
                let range = match (values.front(), values.get(1)) {
                    (Some(a), Some(b)) if !a.is_nil() && !b.is_nil() => {
                        let a = objects::position(a.clone())?;
                        let b = objects::position(b.clone())?;
                        (a <= b).then_some(a..b)
                    }
                    _ => None,
                };
                Ok(CallbackResponse::Range(range))
            }
        }
    }
    fn feedkeys(self: &Rc<Self>, keys: &str) -> Result<()> {
        let parsed = { self.workspace.borrow().editor.parse_keys(keys) };
        let keys = match parsed {
            Ok(keys) => keys,
            Err(error) => {
                self.messages.borrow_mut().push(error.to_string());
                return Ok(());
            }
        };
        // C vis_keys_feed replays one symbolic key at a time. Pending callbacks
        // see only the prefix that has actually arrived, as with physical input.
        for key in keys {
            let action = { self.workspace.borrow_mut().editor.feed_keys(vec![key]) };
            match action
                .map_err(mlua::Error::external)
                .and_then(|action| self.drive_action_queue(action))
            {
                Ok(Outcome::Quit) => break,
                Ok(_) => {}
                Err(error) => self.messages.borrow_mut().push(error.to_string()),
            }
            if self.exit_code.get().is_some() {
                break;
            }
        }
        Ok(())
    }
    fn drive_action_queue(self: &Rc<Self>, mut action: EditorAction) -> Result<Outcome> {
        let mut outcome = Outcome::Continue;
        loop {
            if action != EditorAction::Continue {
                let plan = { self.workspace.borrow_mut().begin_action(action) }
                    .map_err(mlua::Error::external)?;
                outcome = self.run_plan(plan)?;
                if outcome == Outcome::Quit || self.exit_code.get().is_some() {
                    return Ok(outcome);
                }
            }
            action = { self.workspace.borrow_mut().editor.next_queued() }
                .map_err(mlua::Error::external)?;
            if action == EditorAction::Continue {
                return Ok(outcome);
            }
        }
    }
    fn insert(&self, bytes: &[u8], replace: bool) -> Result<()> {
        self.workspace
            .borrow_mut()
            .editor
            .insert_raw(bytes, replace)
            .map_err(mlua::Error::external)
    }
}

/// Owned Lua drawing output for a terminal renderer. Byte ranges use file
/// offsets; cell coordinates use the window's measured text viewport.
#[derive(Clone, Debug, Default)]
pub struct Decoration {
    pub styles: HashMap<usize, String>,
    pub ranges: Vec<(usize, std::ops::Range<usize>)>,
    pub cells: HashMap<(usize, usize), usize>,
    pub status_left: Vec<u8>,
    pub status_right: Vec<u8>,
}
impl Runtime {
    pub fn decoration(&self, window: usize) -> Decoration {
        let state = self.inner.object_state.borrow();
        let Some(visual) = state.windows.get(&window) else {
            return Decoration::default();
        };
        Decoration {
            styles: state.styles.clone(),
            ranges: visual.ranges.clone(),
            cells: visual.cells.clone(),
            status_left: visual.status_left.clone(),
            status_right: visual.status_right.clone(),
        }
    }
}

#[cfg(all(unix, feature = "terminal"))]
impl Runtime {
    pub fn render_decorations(
        &self,
    ) -> std::collections::BTreeMap<usize, crate::terminal::ViewDecorations> {
        let state = self.inner.object_state.borrow();
        let mut styles = state
            .styles
            .iter()
            .map(|(id, spec)| (*id, crate::style::Style::parse(spec)))
            .collect::<std::collections::BTreeMap<_, _>>();
        let default = styles.get(&0).copied().unwrap_or_default();
        styles.insert(0, crate::style::Style::terminal_default().merge(default));
        state
            .windows
            .iter()
            .map(|(window, visual)| {
                (
                    *window,
                    crate::terminal::ViewDecorations {
                        base: self
                            .inner
                            .draw_records
                            .borrow()
                            .get(window)
                            .and_then(|record| record.base),
                        whitespace: self
                            .inner
                            .draw_records
                            .borrow()
                            .get(window)
                            .and_then(|record| record.whitespace),
                        operations: self
                            .inner
                            .draw_records
                            .borrow()
                            .get(window)
                            .map(|record| record.operations.clone())
                            .unwrap_or_default(),
                        styles: styles.clone(),
                        ranges: visual.ranges.clone(),
                        cells: visual.cells.iter().map(|(cell, id)| (*cell, *id)).collect(),
                        status_left: visual.status_left.clone(),
                        status_right: visual.status_right.clone(),
                    },
                )
            })
            .collect()
    }
}

const REGISTER_NAMES: &str = "\"0&123456789_+*./:!#abcdefghijklmnopqrstuvwxyz";
impl Inner {
    fn install_registers(self: &Rc<Self>, methods: &Table) -> Result<()> {
        let registers = self.lua.create_table()?;
        let meta = self.lua.create_table()?;
        let context = Rc::downgrade(self);
        meta.set(
            "__index",
            self.lua
                .create_function(move |lua, (_, name): (Table, String)| {
                    let inner = upgrade(&context)?;
                    let table = lua.create_table()?;
                    if name.chars().count() != 1 {
                        return Ok(table);
                    }
                    let name = name.chars().next().expect("one char");
                    let slots = inner.workspace.borrow().editor.get_register_slots(name);
                    for (i, bytes) in slots.into_iter().enumerate() {
                        table.set(i + 1, lua.create_string(bytes)?)?;
                    }
                    Ok(table)
                })?,
        )?;
        let context = Rc::downgrade(self);
        meta.set(
            "__newindex",
            self.lua
                .create_function(move |_, (_, name, value): (Table, String, Value)| {
                    let inner = upgrade(&context)?;
                    if name.chars().count() != 1 {
                        return Ok(());
                    }
                    let mut slots = Vec::new();
                    if let Value::Table(table) = value {
                        for value in table.sequence_values::<mlua::String>() {
                            slots.push(value?.as_bytes().to_vec());
                        }
                    }
                    inner
                        .workspace
                        .borrow_mut()
                        .editor
                        .set_register_slots(name.chars().next().expect("one char"), slots)
                        .map_err(mlua::Error::external)
                })?,
        )?;
        meta.set("__len", self.lua.create_function(|_, _: Table| Ok(48))?)?;
        registers.set_metatable(Some(meta))?;
        methods.set("registers", registers)?;
        methods.set(
            "register_names",
            self.lua.create_function(|lua, _: AnyUserData| {
                let mut names = REGISTER_NAMES.chars();
                lua.create_function_mut(move |lua, ()| {
                    names
                        .next()
                        .map(|c| lua.create_string(c.to_string()))
                        .transpose()
                })
            })?,
        )?;
        methods.set(
            "mark_names",
            self.lua.create_function(|lua, _: AnyUserData| {
                let mut names = "'^abcdefghijklmnopqrstuvwxyz".chars();
                lua.create_function_mut(move |lua, ()| {
                    names
                        .next()
                        .map(|c| lua.create_string(c.to_string()))
                        .transpose()
                })
            })?,
        )?;
        Ok(())
    }
}

// Only exact entries in C's registry prevent user registration. Rust's extra
// command aliases may be replaced by a registered command, as they can in C.
const NATIVE_COMMANDS: &[&str] = &[
    "a",
    "c",
    "d",
    "g",
    "i",
    "p",
    "s",
    "v",
    "x",
    "y",
    "X",
    "Y",
    ">",
    "<",
    "|",
    "!",
    "w",
    "r",
    "{",
    "}",
    "e",
    "q",
    "cd",
    "help",
    "map",
    "map-window",
    "unmap",
    "unmap-window",
    "langmap",
    "new",
    "open",
    "qall",
    "set",
    "split",
    "vnew",
    "vsplit",
    "wq",
    "earlier",
    "later",
];

fn builtin_option_kind(name: &str) -> Option<OptionKind> {
    match name {
        "tabwidth" | "tw" => Some(OptionKind::Number),
        "expandtab" | "et" | "autoindent" | "ai" | "oneline" | "statusbar" | "showeof"
        | "cursorline" | "cul" | "showspaces" | "showtabs" | "shownewlines" | "ignorecase"
        | "ic" | "change256colors" => Some(OptionKind::Bool),
        "loadmethod" => Some(OptionKind::String),
        "numberwidth" | "nuw" | "colorcolumn" | "cc" | "wrapcolumn" | "wc" => {
            Some(OptionKind::Number)
        }
        _ => None,
    }
}

fn exact_integer(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(n) => Some(*n),
        Value::Number(n)
            if n.is_finite()
                && n.fract() == 0.0
                && *n >= i64::MIN as f64
                && *n < (i64::MAX as f64) =>
        {
            Some(*n as i64)
        }
        _ => None,
    }
}
