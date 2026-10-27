//! Live Lua handles. Handles contain IDs and a weak runtime reference, never a
//! borrowed editor or text pointer. Every host borrow ends before returning to Lua.
use super::{Context, Inner, upgrade};
use crate::file::SaveMethod;
use crate::motion;
use crate::selection::{Selection, SelectionId};
use crate::text::{Mark, Text};
use mlua::{
    AnyUserData, Error, Function, Lua, MetaMethod, MultiValue, Result, Table, UserData,
    UserDataMethods, Value,
};
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;

#[derive(Default)]
pub(crate) struct ObjectState {
    pub(crate) styles: HashMap<usize, String>,
    pub(crate) windows: HashMap<usize, WindowVisual>,
}

#[derive(Default)]
pub(crate) struct WindowVisual {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) viewport_width: usize,
    pub(crate) viewport_height: usize,
    pub(crate) viewport_bytes: Range<usize>,
    pub(crate) viewport_lines: Range<usize>,
    pub(crate) ranges: Vec<(usize, Range<usize>)>,
    pub(crate) cells: HashMap<(usize, usize), usize>,
    pub(crate) status_left: Vec<u8>,
    pub(crate) status_right: Vec<u8>,
    pub(crate) redraw: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    File,
    Window,
    Lines,
    Selections,
    Options,
    Marks,
    Selection(SelectionId),
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Window => "window",
            Self::Lines => "text",
            Self::Selections => "selections",
            Self::Options => "winoptions",
            Self::Marks => "marks",
            Self::Selection(_) => "selection",
        }
    }
}

#[derive(Clone)]
struct Handle {
    context: Context,
    id: usize,
    kind: Kind,
}

#[derive(Clone)]
struct LuaMark {
    file: usize,
    mark: Mark,
}
impl UserData for LuaMark {}

fn invalid(message: impl Into<String>) -> Error {
    Error::RuntimeError(message.into())
}

fn path_string(lua: &Lua, path: &Path) -> Result<mlua::String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        lua.create_string(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        lua.create_string(path.to_string_lossy().as_bytes())
    }
}

pub(crate) fn position(value: Value) -> Result<usize> {
    match value {
        Value::Integer(n) if n >= 0 => usize::try_from(n).map_err(Error::external),
        Value::Number(n)
            if n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n < usize::MAX as f64 =>
        {
            Ok(n as usize)
        }
        _ => Err(invalid("expected nonnegative integral position")),
    }
}

fn range(value: Value, length: Option<Value>) -> Result<Range<usize>> {
    if let Value::Table(t) = value {
        let start = position(t.get("start")?)?;
        let end = position(t.get("finish")?)?;
        Ok(start..end)
    } else {
        let start = position(value)?;
        let len = position(length.unwrap_or(Value::Nil))?;
        Ok(start
            ..start
                .checked_add(len)
                .ok_or_else(|| invalid("position overflow"))?)
    }
}

pub(crate) fn push_range(lua: &Lua, range: Range<usize>) -> Result<Table> {
    let t = lua.create_table()?;
    t.set("start", range.start)?;
    t.set("finish", range.end)?;
    Ok(t)
}

fn new_handle(inner: &Rc<Inner>, id: usize, kind: Kind) -> Result<AnyUserData> {
    let ud = inner.lua.create_userdata(Handle {
        context: Rc::downgrade(inner),
        id,
        kind,
    })?;
    ud.set_user_value(inner.lua.create_table()?)?;
    Ok(ud)
}

fn cached(inner: &Rc<Inner>, id: usize, kind: Kind) -> Result<AnyUserData> {
    let key = format!("{kind:?}:{id}");
    if let Some(ud) = inner.object_cache.borrow().get(&key).cloned() {
        return Ok(ud);
    }
    let ud = new_handle(inner, id, kind)?;
    inner.object_cache.borrow_mut().insert(key, ud.clone());
    Ok(ud)
}

pub(crate) fn file(inner: &Rc<Inner>, id: usize) -> Result<AnyUserData> {
    cached(inner, id, Kind::File)
}
pub(crate) fn window(inner: &Rc<Inner>, id: usize) -> Result<AnyUserData> {
    cached(inner, id, Kind::Window)
}
pub(crate) fn selection(inner: &Rc<Inner>, window: usize, id: SelectionId) -> Result<AnyUserData> {
    // C obj_lightref_new allocates on every lookup. Only the logical target is
    // shared: each wrapper has its own Lua identity and user fields.
    new_handle(inner, window, Kind::Selection(id))
}
pub(crate) fn selection_by_raw_id(
    inner: &Rc<Inner>,
    window: usize,
    raw_id: u64,
) -> Result<Option<AnyUserData>> {
    let id = inner
        .workspace
        .try_borrow()
        .map_err(Error::external)?
        .window_identified_selections(window)
        .and_then(|selections| {
            selections
                .into_iter()
                .find(|(id, _)| id.get() == raw_id)
                .map(|(id, _)| id)
        });
    id.map(|id| selection(inner, window, id)).transpose()
}
pub(crate) fn file_id(ud: &AnyUserData) -> Result<usize> {
    let h = ud.borrow::<Handle>()?;
    if h.kind != Kind::File {
        return Err(invalid("expected file"));
    }
    h.with_text(|_| ())?;
    Ok(h.id)
}
pub(crate) fn window_id(ud: &AnyUserData) -> Result<usize> {
    let h = ud.borrow::<Handle>()?;
    if h.kind != Kind::Window {
        return Err(invalid("expected window"));
    }
    h.window_file()?;
    Ok(h.id)
}

/// Public Lua type tables are facades over mlua's protected Rust metatables.
/// The host's safe getmetatable adapter uses this tag for native objects.
pub(crate) fn object_type(ud: &AnyUserData) -> Option<&'static str> {
    if let Ok(handle) = ud.borrow::<Handle>() {
        return Some(handle.kind.name());
    }
    if ud.is::<LuaMark>() {
        return Some("mark");
    }
    None
}

pub(crate) fn cleanup(inner: &Rc<Inner>) -> Result<()> {
    let workspace = inner.workspace.try_borrow().map_err(Error::external)?;
    inner.object_cache.borrow_mut().retain(|_, ud| {
        let Ok(h) = ud.borrow::<Handle>() else {
            return false;
        };
        match h.kind {
            Kind::File | Kind::Lines => workspace.with_buffer(h.id, |_| ()).is_some(),
            Kind::Selection(id) => workspace
                .window_identified_selections(h.id)
                .is_some_and(|sels| sels.iter().any(|(candidate, _)| *candidate == id)),
            _ => workspace.window_file_id(h.id).is_some(),
        }
    });
    inner
        .object_state
        .borrow_mut()
        .windows
        .retain(|id, _| workspace.window_file_id(*id).is_some());
    Ok(())
}

impl Handle {
    fn inner(&self) -> Result<Rc<Inner>> {
        upgrade(&self.context)
    }
    fn with_text<R>(&self, f: impl FnOnce(&Text) -> R) -> Result<R> {
        let inner = self.inner()?;
        let result = inner
            .workspace
            .try_borrow()
            .map_err(Error::external)?
            .with_buffer(self.id, f);
        result.ok_or_else(|| invalid("file is closed"))
    }
    fn with_text_mut<R>(&self, f: impl FnOnce(&mut Text) -> R) -> Result<R> {
        let inner = self.inner()?;
        let result = inner
            .workspace
            .try_borrow_mut()
            .map_err(Error::external)?
            .with_buffer_mut(self.id, f);
        result.ok_or_else(|| invalid("file is closed"))
    }
    fn window_file(&self) -> Result<usize> {
        let inner = self.inner()?;
        let id = inner
            .workspace
            .try_borrow()
            .map_err(Error::external)?
            .window_file_id(self.id);
        id.ok_or_else(|| invalid("window is closed"))
    }
    fn selections(&self) -> Result<Vec<(SelectionId, Selection)>> {
        let inner = self.inner()?;
        let selections = inner
            .workspace
            .try_borrow()
            .map_err(Error::external)?
            .window_identified_selections(self.id);
        selections.ok_or_else(|| invalid("window is closed"))
    }
    fn live_selection(&self) -> Result<Option<Selection>> {
        let Kind::Selection(id) = self.kind else {
            return Err(invalid("expected selection"));
        };
        let inner = self.inner()?;
        let selections = inner
            .workspace
            .try_borrow()
            .map_err(Error::external)?
            .window_identified_selections(self.id)
            .unwrap_or_default();
        Ok(selections
            .into_iter()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, sel)| sel))
    }
    fn lookup(&self, ud: &AnyUserData, key: Value) -> Result<Value> {
        let inner = self.inner()?;
        let t: Table = inner.types.get(self.kind.name())?;
        let value: Value = t.get(key.clone())?;
        if !value.is_nil() {
            return Ok(value);
        }
        ud.user_value::<Table>()?.get(key)
    }
    fn assign(&self, ud: &AnyUserData, key: Value, value: Value) -> Result<()> {
        ud.user_value::<Table>()?.set(key, value)
    }
}

fn style_id(inner: &Inner, id: usize) -> usize {
    if id < inner.allocated_style_count() {
        id
    } else {
        0
    }
}

fn string_key(key: &Value) -> Option<String> {
    match key {
        Value::String(s) => Some(s.to_string_lossy()),
        _ => None,
    }
}

fn line_start(bytes: &[u8], line: usize) -> Option<usize> {
    if line <= 1 {
        return Some(0);
    }
    bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(line - 2)
        .map(|(p, _)| p + 1)
}
fn line_count(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        0
    } else {
        bytes.iter().filter(|b| **b == b'\n').count() + usize::from(bytes.last() != Some(&b'\n'))
    }
}
fn line_column(bytes: &[u8], position: usize) -> (usize, usize) {
    let p = position.min(bytes.len());
    (
        1 + bytes[..p].iter().filter(|b| **b == b'\n').count(),
        motion::line_char_get(bytes, p) + 1,
    )
}

/// Copy only the requested logical line, traversing the persistent pieces. A
/// live iterator must observe later edits, so it cannot freeze a file snapshot.
fn next_line(text: &Text, pos: usize) -> Option<(Vec<u8>, usize)> {
    if pos >= text.len() {
        return None;
    }
    let mut offset = 0;
    let mut bytes = Vec::new();
    for chunk in text.chunks() {
        let end = offset + chunk.len();
        if pos < end {
            let start = pos.saturating_sub(offset);
            let tail = &chunk[start..];
            if let Some(newline) = tail.iter().position(|byte| *byte == b'\n') {
                bytes.extend_from_slice(&tail[..newline]);
                return Some((bytes, offset + start + newline + 1));
            }
            bytes.extend_from_slice(tail);
        }
        offset = end;
    }
    Some((bytes, text.len()))
}

impl UserData for Handle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function(MetaMethod::Index, |lua, (ud, key): (AnyUserData, Value)| {
            let h = ud.borrow::<Handle>()?.clone();
            h.index(lua, &ud, key)
        });
        methods.add_meta_function(
            MetaMethod::NewIndex,
            |lua, (ud, key, value): (AnyUserData, Value, Value)| {
                let h = ud.borrow::<Handle>()?.clone();
                h.newindex(lua, &ud, key, value)
            },
        );
        methods.add_meta_method(MetaMethod::Len, |_, h, ()| match h.kind {
            Kind::Lines => h.with_text(|t| line_count(t.shared_bytes().as_slice())),
            Kind::Selections => Ok(h.selections()?.len()),
            Kind::Marks => {
                h.window_file()?;
                Ok(28)
            }
            _ => Err(invalid("object has no length")),
        });
    }
}

impl Handle {
    fn index(&self, lua: &Lua, ud: &AnyUserData, key: Value) -> Result<Value> {
        let inner = self.inner()?;
        let name = string_key(&key);
        match self.kind {
            Kind::File => {
                self.with_text(|_| ())?;
                match name.as_deref() {
                    Some("size") => return Ok(Value::Integer(self.with_text(Text::len)? as i64)),
                    Some("modified") => return Ok(Value::Boolean(self.with_text(Text::modified)?)),
                    Some("lines") => {
                        return Ok(Value::UserData(cached(&inner, self.id, Kind::Lines)?));
                    }
                    Some("name" | "path") => {
                        let workspace = inner.workspace.try_borrow().map_err(Error::external)?;
                        let path = workspace.buffer_path(self.id);
                        let value = path.map(|path| {
                            if name.as_deref() == Some("name") {
                                path.strip_prefix(workspace.directory())
                                    .unwrap_or(&path)
                                    .to_path_buf()
                            } else if path.is_absolute() {
                                path
                            } else {
                                workspace.directory().join(path)
                            }
                        });
                        drop(workspace);
                        return match value {
                            Some(path) => Ok(Value::String(path_string(lua, &path)?)),
                            None => Ok(Value::Nil),
                        };
                    }
                    Some("permission") => {
                        return Ok(Value::Integer(
                            inner
                                .workspace
                                .try_borrow()
                                .map_err(Error::external)?
                                .buffer_permission(self.id)
                                .unwrap_or(0) as i64
                                & 0o777,
                        ));
                    }
                    Some("savemethod") => {
                        let method = inner
                            .workspace
                            .try_borrow()
                            .map_err(Error::external)?
                            .buffer_save_method(self.id)
                            .unwrap_or_default();
                        return Ok(Value::String(lua.create_string(match method {
                            SaveMethod::Auto => "auto",
                            SaveMethod::Atomic => "atomic",
                            SaveMethod::InPlace => "inplace",
                        })?));
                    }
                    _ => {}
                }
            }
            Kind::Lines => {
                let line = position(key)?;
                let bytes = self.with_text(|text| text.shared_bytes())?;
                return match line_start(&bytes, line) {
                    Some(start) => Ok(Value::String(
                        lua.create_string(&bytes[start..motion::line_end(&bytes, start)])?,
                    )),
                    None => Ok(Value::Nil),
                };
            }
            Kind::Window => {
                let file_id = self.window_file()?;
                match name.as_deref() {
                    Some("file") => return Ok(Value::UserData(file(&inner, file_id)?)),
                    Some("selection") => {
                        let primary = inner
                            .workspace
                            .try_borrow()
                            .map_err(Error::external)?
                            .window_primary_selection(self.id);
                        return match primary {
                            Some(id) => Ok(Value::UserData(selection(&inner, self.id, id)?)),
                            None => Ok(Value::Nil),
                        };
                    }
                    Some("selections" | "marks" | "options") => {
                        let kind = match name.as_deref() {
                            Some("selections") => Kind::Selections,
                            Some("marks") => Kind::Marks,
                            _ => Kind::Options,
                        };
                        return Ok(Value::UserData(cached(&inner, self.id, kind)?));
                    }
                    Some("width" | "height" | "viewport") => {
                        let state = inner.object_state.borrow();
                        let default = WindowVisual::default();
                        let visual = state.windows.get(&self.id).unwrap_or(&default);
                        if name.as_deref() == Some("width") {
                            return Ok(Value::Integer(visual.width as i64));
                        }
                        if name.as_deref() == Some("height") {
                            return Ok(Value::Integer(visual.height as i64));
                        }
                        let t = lua.create_table()?;
                        t.set("width", visual.viewport_width)?;
                        t.set("height", visual.viewport_height)?;
                        t.set("bytes", push_range(lua, visual.viewport_bytes.clone())?)?;
                        t.set("lines", push_range(lua, visual.viewport_lines.clone())?)?;
                        return Ok(Value::Table(t));
                    }
                    _ => {}
                }
            }
            Kind::Selections => {
                let index = position(key)?;
                return match self.selections()?.get(index.wrapping_sub(1)) {
                    Some((id, _)) => Ok(Value::UserData(selection(&inner, self.id, *id)?)),
                    None => Ok(Value::Nil),
                };
            }
            Kind::Options => {
                self.window_file()?;
                if let Some(name) = &name {
                    let result = inner.option_get(name, Some(self.id))?;
                    if !result.is_nil() {
                        return Ok(result);
                    }
                }
            }
            Kind::Marks => {
                self.window_file()?;
                let t = lua.create_table()?;
                if let Some(symbol) = name
                    .as_deref()
                    .filter(|s| s.len() == 1)
                    .and_then(|s| s.chars().next())
                    .filter(|symbol| symbol.is_ascii_lowercase() || matches!(symbol, '\'' | '^'))
                {
                    let ranges = inner
                        .workspace
                        .try_borrow()
                        .map_err(Error::external)?
                        .window_mark_get(self.id, symbol)
                        .unwrap_or_default();
                    for (i, range) in ranges.into_iter().enumerate() {
                        t.raw_set(i + 1, push_range(lua, range)?)?;
                    }
                }
                return Ok(Value::Table(t));
            }
            Kind::Selection(id) => {
                let Some(sel) = self.live_selection()? else {
                    return Ok(Value::Nil);
                };
                let file_id = self.window_file()?;
                let bytes = inner
                    .workspace
                    .try_borrow()
                    .map_err(Error::external)?
                    .with_buffer(file_id, |text| text.shared_bytes())
                    .ok_or_else(|| invalid("file is closed"))?;
                match name.as_deref() {
                    Some("pos") => {
                        return Ok(inner
                            .workspace
                            .try_borrow()
                            .map_err(Error::external)?
                            .window_selection_position(self.id, id)
                            .map_or(Value::Nil, |pos| Value::Integer(pos as i64)));
                    }
                    Some("line") => {
                        return Ok(Value::Integer(line_column(&bytes, sel.cursor).0 as i64));
                    }
                    Some("col") => {
                        return Ok(Value::Integer(line_column(&bytes, sel.cursor).1 as i64));
                    }
                    Some("number") => {
                        return Ok(Value::Integer(
                            (self
                                .selections()?
                                .iter()
                                .position(|(other, _)| *other == id)
                                .unwrap_or(0)
                                + 1) as i64,
                        ));
                    }
                    Some("range") => {
                        let r = inner
                            .workspace
                            .try_borrow()
                            .map_err(Error::external)?
                            .window_selection_range(self.id, id);
                        return r
                            .map(|r| push_range(lua, r).map(Value::Table))
                            .unwrap_or(Ok(Value::Nil));
                    }
                    Some("anchored") => {
                        return Ok(Value::Boolean(
                            inner
                                .workspace
                                .try_borrow()
                                .map_err(Error::external)?
                                .window_selection_anchored(self.id, id)
                                .unwrap_or(false),
                        ));
                    }
                    _ => {}
                }
            }
        }
        self.lookup(ud, key)
    }

    fn newindex(&self, _lua: &Lua, ud: &AnyUserData, key: Value, value: Value) -> Result<()> {
        let inner = self.inner()?;
        let name = string_key(&key);
        match self.kind {
            Kind::File => match name.as_deref() {
                Some("modified") => {
                    self.with_text_mut(|text| {
                        if matches!(value, Value::Boolean(true)) {
                            text.insert(0, b" ")?;
                            text.delete(0..1)
                        } else {
                            text.mark_saved();
                            Ok(())
                        }
                    })?
                    .map_err(Error::external)?;
                    return Ok(());
                }
                Some("savemethod") if matches!(value, Value::String(_)) => {
                    let method = match string_key(&value).as_deref() {
                        Some("auto") => Some(SaveMethod::Auto),
                        Some("atomic") => Some(SaveMethod::Atomic),
                        Some("inplace") => Some(SaveMethod::InPlace),
                        _ => None,
                    };
                    if let Some(method) = method {
                        inner
                            .workspace
                            .try_borrow_mut()
                            .map_err(Error::external)?
                            .set_buffer_save_method(self.id, method)
                            .map_err(Error::external)?;
                    }
                    return Ok(());
                }
                _ => {
                    self.with_text(|_| ())?;
                }
            },
            Kind::Lines => {
                let line = position(key)?;
                let Value::String(data) = value else {
                    return Err(invalid("expected line string"));
                };
                let data = data.as_bytes();
                self.with_text_mut(|text| {
                    if line == 0 {
                        let mut bytes = data.to_vec();
                        bytes.push(b'\n');
                        return text.insert(0, &bytes);
                    }
                    let bytes = text.to_vec();
                    if let Some(start) = line_start(&bytes, line) {
                        let end = motion::line_end(&bytes, start);
                        text.replace(start..end, &data)?;
                        if text.len() == start + data.len() {
                            text.insert(text.len(), b"\n")?;
                        }
                    }
                    Ok(())
                })?
                .map_err(Error::external)?;
                return Ok(());
            }
            Kind::Window => {
                self.window_file()?;
                match (name.as_deref(), &value) {
                    (Some("file"), Value::String(path)) => {
                        if !inner.change_window_file(self.id, &path.to_str()?)? {
                            return Err(invalid("failed to open"));
                        }
                        return Ok(());
                    }
                    (Some("options"), Value::Table(t)) => {
                        for pair in t.clone().pairs::<Value, Value>() {
                            let (key, value) = pair?;
                            if let Some(name) = string_key(&key) {
                                inner.option_set(&name, value, Some(self.id))?;
                            }
                        }
                        return Ok(());
                    }
                    _ => {}
                }
            }
            Kind::Options => {
                self.window_file()?;
                if let Some(name) = &name {
                    if inner.option_set(name, value.clone(), Some(self.id))? {
                        return Ok(());
                    }
                }
            }
            Kind::Marks => {
                self.window_file()?;
                if let Some(symbol) = name
                    .as_deref()
                    .filter(|s| s.len() == 1)
                    .and_then(|s| s.chars().next())
                    .filter(|symbol| symbol.is_ascii_lowercase() || matches!(symbol, '\'' | '^'))
                {
                    let mut ranges = Vec::new();
                    if let Value::Table(t) = value {
                        for pair in t.pairs::<Value, Value>() {
                            let (_, value) = pair?;
                            let r = range(value, None)?;
                            if r.start <= r.end {
                                ranges.push(r);
                            }
                        }
                    }
                    inner
                        .workspace
                        .try_borrow_mut()
                        .map_err(Error::external)?
                        .window_mark_set(self.id, symbol, ranges)
                        .map_err(Error::external)?;
                }
                return Ok(());
            }
            Kind::Selection(id) => {
                let Some(mut sel) = self.live_selection()? else {
                    return Ok(());
                };
                let file_id = self.window_file()?;
                let bytes = inner
                    .workspace
                    .try_borrow()
                    .map_err(Error::external)?
                    .with_buffer(file_id, |text| text.shared_bytes())
                    .ok_or_else(|| invalid("file is closed"))?;
                match name.as_deref() {
                    Some("pos") => {
                        let pos = position(value)?.min(bytes.len());
                        let anchored = inner
                            .workspace
                            .try_borrow()
                            .map_err(Error::external)?
                            .window_selection_anchored(self.id, id)
                            .unwrap_or(false);
                        sel.cursor = pos;
                        if !anchored {
                            sel.anchor = pos;
                        }
                    }
                    Some("range") => {
                        if value.is_nil() {
                            return Err(invalid("expected range"));
                        } else {
                            let r = range(value, None)?;
                            if r.start <= r.end {
                                sel = Selection::from_range(
                                    &bytes,
                                    r.start.min(bytes.len())..r.end.min(bytes.len()),
                                    sel.direction(),
                                )
                                .map_err(Error::external)?;
                                inner
                                    .workspace
                                    .try_borrow_mut()
                                    .map_err(Error::external)?
                                    .set_window_selection_anchored(self.id, id, true)
                                    .map_err(Error::external)?;
                            } else {
                                sel.anchor = sel.cursor;
                                inner
                                    .workspace
                                    .try_borrow_mut()
                                    .map_err(Error::external)?
                                    .set_window_selection_anchored(self.id, id, false)
                                    .map_err(Error::external)?;
                            }
                        }
                    }
                    Some("anchored") => {
                        inner
                            .workspace
                            .try_borrow_mut()
                            .map_err(Error::external)?
                            .set_window_selection_anchored(
                                self.id,
                                id,
                                !matches!(value, Value::Nil | Value::Boolean(false)),
                            )
                            .map_err(Error::external)?;
                        return Ok(());
                    }
                    _ => return self.assign(ud, key, value),
                }
                inner
                    .workspace
                    .try_borrow_mut()
                    .map_err(Error::external)?
                    .window_update_selection(self.id, id, sel)
                    .map_err(Error::external)?;
                return Ok(());
            }
            Kind::Selections => {}
        }
        self.assign(ud, key, value)
    }
}

fn handle(ud: AnyUserData, kind: Kind) -> Result<Handle> {
    let h = ud.borrow::<Handle>()?.clone();
    if h.kind != kind {
        return Err(invalid(format!("expected {}", kind.name())));
    }
    Ok(h)
}

pub(crate) fn register(inner: &Rc<Inner>) -> Result<()> {
    for name in [
        "file",
        "window",
        "text",
        "selections",
        "winoptions",
        "marks",
        "mark",
        "selection",
    ] {
        if !matches!(inner.types.get::<Value>(name)?, Value::Table(_)) {
            inner.types.set(name, inner.lua.create_table()?)?;
        }
    }
    let lua = &inner.lua;
    let files: Table = inner.types.get("file")?;
    files.set(
        "content",
        lua.create_function(|lua, (ud, a, b): (AnyUserData, Value, Option<Value>)| {
            let h = handle(ud, Kind::File)?;
            let r = range(a, b)?;
            let bytes = h.with_text(|t| {
                if r.start <= r.end {
                    t.bytes(r.start.min(t.len())..r.end.min(t.len()))
                } else {
                    None
                }
            })?;
            bytes.map(|b| lua.create_string(b)).transpose()
        })?,
    )?;
    files.set(
        "insert",
        lua.create_function(|_, (ud, p, data): (AnyUserData, Value, mlua::String)| {
            let h = handle(ud, Kind::File)?;
            let pos = position(p)?;
            Ok(h.with_text_mut(|t| t.insert(pos, &data.as_bytes()))?
                .is_ok())
        })?,
    )?;
    files.set(
        "delete",
        lua.create_function(|_, (ud, a, b): (AnyUserData, Value, Option<Value>)| {
            let h = handle(ud, Kind::File)?;
            let r = range(a, b)?;
            Ok(h.with_text_mut(|t| t.delete(r))?.is_ok())
        })?,
    )?;
    files.set(
        "snapshot",
        lua.create_function(|_, ud: AnyUserData| {
            handle(ud, Kind::File)?.with_text_mut(|t| t.snapshot())?;
            Ok(())
        })?,
    )?;
    files.set(
        "mark_set",
        lua.create_function(|lua, (ud, p): (AnyUserData, Value)| {
            let h = handle(ud, Kind::File)?;
            let pos = position(p)?;
            h.with_text(|t| t.mark(pos))?
                .map(|mark| lua.create_userdata(LuaMark { file: h.id, mark }))
                .transpose()
        })?,
    )?;
    files.set(
        "mark_get",
        lua.create_function(|_, (ud, mark): (AnyUserData, AnyUserData)| {
            let h = handle(ud, Kind::File)?;
            let mark = mark.borrow::<LuaMark>()?;
            h.with_text(|t| {
                if mark.file == h.id {
                    t.mark_get(mark.mark.clone())
                } else {
                    None
                }
            })
        })?,
    )?;
    files.set(
        "offset_from_line_column",
        lua.create_function(|_, (ud, l, c): (AnyUserData, Value, Option<Value>)| {
            let h = handle(ud, Kind::File)?;
            let line = position(l)?;
            let column = c.map(position).transpose()?;
            h.with_text(|t| {
                let bytes = t.shared_bytes();
                let p = line_start(bytes.as_slice(), line)?;
                Some(column.map_or(p, |c| {
                    motion::line_char_set(bytes.as_slice(), p, c.saturating_sub(1))
                }))
            })
        })?,
    )?;
    files.set(
        "line_column_from_offset",
        lua.create_function(|_, (ud, p): (AnyUserData, Value)| {
            let h = handle(ud, Kind::File)?;
            let pos = position(p)?;
            h.with_text(|t| line_column(t.shared_bytes().as_slice(), pos))
        })?,
    )?;
    for (name, big) in [("text_object_word", false), ("text_object_longword", true)] {
        files.set(
            name,
            lua.create_function(move |lua, (ud, p): (AnyUserData, Value)| {
                let h = handle(ud, Kind::File)?;
                let pos = position(p)?;
                let r = h.with_text(|t| {
                    motion::word_object(t.shared_bytes().as_slice(), pos, false, big)
                })?;
                if r.start <= r.end {
                    Ok(Some(push_range(lua, r)?))
                } else {
                    Ok(None)
                }
            })?,
        )?;
    }
    files.set(
        "lines_iterator",
        lua.create_function(|lua, (ud, start): (AnyUserData, Option<Value>)| {
            let h = handle(ud, Kind::File)?;
            let line = start.map(position).transpose()?.unwrap_or(1);
            let mut pos =
                h.with_text(|t| line_start(t.shared_bytes().as_slice(), line).unwrap_or(t.len()))?;
            lua.create_function_mut(move |lua, (): ()| {
                let Some((bytes, next)) = h.with_text(|t| next_line(t, pos))? else {
                    return Ok(None);
                };
                let result = lua.create_string(bytes)?;
                pos = next;
                Ok(Some(result))
            })
        })?,
    )?;
    register_window(inner)?;
    Ok(())
}

fn register_window(inner: &Rc<Inner>) -> Result<()> {
    let lua = &inner.lua;
    let windows: Table = inner.types.get("window")?;
    windows.set(
        "selections_iterator",
        lua.create_function(|lua, ud: AnyUserData| {
            let h = handle(ud, Kind::Window)?;
            let ids: Vec<_> = h.selections()?.into_iter().map(|(id, _)| id).collect();
            let mut ids = ids.into_iter();
            lua.create_function_mut(move |_, (): ()| {
                let inner = h.inner()?;
                for id in ids.by_ref() {
                    if h.selections()?
                        .iter()
                        .any(|(candidate, _)| *candidate == id)
                    {
                        return Ok(Some(selection(&inner, h.id, id)?));
                    }
                }
                Ok(None)
            })
        })?,
    )?;
    windows.set(
        "close",
        lua.create_function(|_, (ud, force): (AnyUserData, Option<bool>)| {
            let h = handle(ud, Kind::Window)?;
            let file = h.window_file()?;
            let inner = h.inner()?;
            let force = force.unwrap_or(false);
            {
                let ws = inner.workspace.try_borrow().map_err(Error::external)?;
                if ws.external_window_ids().len() <= 1 {
                    return Ok(false);
                }
                let shared = ws
                    .window_ids()
                    .into_iter()
                    .filter(|id| ws.window_file_id(*id) == Some(file))
                    .count()
                    > 1;
                if !force && !shared && ws.with_buffer(file, Text::modified).unwrap_or(false) {
                    return Ok(false);
                }
            }
            inner.close_window(h.id, force)
        })?,
    )?;
    windows.set(
        "map",
        lua.create_function(|_, mut args: MultiValue| {
            let ud = match args.pop_front() {
                Some(Value::UserData(ud)) => ud,
                _ => return Err(invalid("expected window")),
            };
            let h = handle(ud, Kind::Window)?;
            h.window_file()?;
            h.inner()?.map(Some(h.id), args)
        })?,
    )?;
    windows.set(
        "unmap",
        lua.create_function(|_, mut args: MultiValue| {
            let ud = match args.pop_front() {
                Some(Value::UserData(ud)) => ud,
                _ => return Err(invalid("expected window")),
            };
            let h = handle(ud, Kind::Window)?;
            h.window_file()?;
            h.inner()?.unmap(Some(h.id), args)
        })?,
    )?;
    windows.set(
        "style",
        lua.create_function(
            |_, (ud, id, start, end): (AnyUserData, Value, Value, Value)| {
                let h = handle(ud, Kind::Window)?;
                h.window_file()?;
                let (id, start, end) = (position(id)?, position(start)?, position(end)?);
                let inner = h.inner()?;
                let id = style_id(&inner, id);
                // Native vis_win_style (view.c) styles while pos <= end.
                // The renderer consumes exclusive Rust ranges instead.
                inner.record_style_range(h.id, id, start..end.saturating_add(1))
            },
        )?,
    )?;
    windows.set(
        "style_pos",
        lua.create_function(|_, (ud, id, x, y): (AnyUserData, Value, Value, Value)| {
            let h = handle(ud, Kind::Window)?;
            h.window_file()?;
            let (id, x, y) = (position(id)?, position(x)?, position(y)?);
            let inner = h.inner()?;
            let id = style_id(&inner, id);
            inner.record_style_pos(h.id, id, x, y)
        })?,
    )?;
    windows.set(
        "status",
        lua.create_function(
            |_, (ud, left, right): (AnyUserData, mlua::String, Option<mlua::String>)| {
                let h = handle(ud, Kind::Window)?;
                h.window_file()?;
                let inner = h.inner()?;
                inner.record_status(
                    h.id,
                    left.as_bytes().to_vec(),
                    right.map(|s| s.as_bytes().to_vec()).unwrap_or_default(),
                )
            },
        )?,
    )?;
    windows.set(
        "draw",
        lua.create_function(|_, ud: AnyUserData| {
            let h = handle(ud, Kind::Window)?;
            h.window_file()?;
            let inner = h.inner()?;
            inner.record_window_draw(h.id)
        })?,
    )?;
    let selections: Table = inner.types.get("selection")?;
    selections.set(
        "to",
        lua.create_function(|_, (ud, line, col): (AnyUserData, Value, Value)| {
            let h = ud.borrow::<Handle>()?.clone();
            let Kind::Selection(id) = h.kind else {
                return Err(invalid("expected selection"));
            };
            let (line, col) = (position(line)?, position(col)?);
            let Some(mut sel) = h.live_selection()? else {
                return Ok(());
            };
            let inner = h.inner()?;
            let file_id = h.window_file()?;
            let bytes = inner
                .workspace
                .try_borrow()
                .map_err(Error::external)?
                .with_buffer(file_id, |text| text.shared_bytes())
                .ok_or_else(|| invalid("file is closed"))?;
            let pos = line_start(&bytes, line)
                .map(|start| motion::line_char_set(&bytes, start, col.saturating_sub(1)))
                .unwrap_or(bytes.len());
            let anchored = inner
                .workspace
                .try_borrow()
                .map_err(Error::external)?
                .window_selection_anchored(h.id, id)
                .unwrap_or(false);
            sel.cursor = pos;
            if !anchored {
                sel.anchor = pos;
            }
            inner
                .workspace
                .try_borrow_mut()
                .map_err(Error::external)?
                .window_update_selection(h.id, id, sel)
                .map_err(Error::external)?;
            Ok(())
        })?,
    )?;
    selections.set(
        "remove",
        lua.create_function(|_, ud: AnyUserData| {
            let h = ud.borrow::<Handle>()?.clone();
            let Kind::Selection(id) = h.kind else {
                return Err(invalid("expected selection"));
            };
            let inner = h.inner()?;
            inner
                .workspace
                .try_borrow_mut()
                .map_err(Error::external)?
                .window_remove_selection(h.id, id)
                .map_err(Error::external)?;
            Ok(())
        })?,
    )?;
    Ok(())
}

pub(crate) fn windows_iterator(inner: &Rc<Inner>) -> Result<Function> {
    let ids = inner
        .workspace
        .try_borrow()
        .map_err(Error::external)?
        .external_window_ids();
    let context = Rc::downgrade(inner);
    let mut ids = ids.into_iter();
    inner.lua.create_function_mut(move |_, (): ()| {
        let inner = upgrade(&context)?;
        for id in ids.by_ref() {
            if inner
                .workspace
                .try_borrow()
                .map_err(Error::external)?
                .window_file_id(id)
                .is_some()
            {
                return Ok(Some(window(&inner, id)?));
            }
        }
        Ok(None)
    })
}
pub(crate) fn files_iterator(inner: &Rc<Inner>) -> Result<Function> {
    let ids = inner
        .workspace
        .try_borrow()
        .map_err(Error::external)?
        .all_file_ids();
    let context = Rc::downgrade(inner);
    let mut ids = ids.into_iter();
    inner.lua.create_function_mut(move |_, (): ()| {
        let inner = upgrade(&context)?;
        for id in ids.by_ref() {
            if inner
                .workspace
                .try_borrow()
                .map_err(Error::external)?
                .with_buffer(id, |_| ())
                .is_some()
            {
                return Ok(Some(file(&inner, id)?));
            }
        }
        Ok(None)
    })
}

#[cfg(test)]
mod tests {
    use crate::lua::Runtime;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Config(PathBuf);
    impl Config {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "vis-lua-objects-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("visrc.lua"), "-- isolated native object tests\n").unwrap();
            Self(path)
        }
    }
    impl Drop for Config {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn runtime(bytes: &[u8]) -> (Config, Runtime) {
        let config = Config::new();
        let runtime = Runtime::new(std::slice::from_ref(&config.0)).unwrap();
        runtime
            .workspace()
            .borrow_mut()
            .open_scratch(Some(bytes.to_vec()), false)
            .unwrap();
        (config, runtime)
    }

    #[test]
    fn file_iterator_includes_eager_internal_files_and_reuses_message_file() {
        let config = Config::new();
        std::fs::write(
            config.0.join("visrc.lua"),
            r#"
            require('vis')
            vis.events.subscribe(vis.events.INIT, function()
                init_files={}
                init_file_count=0
                for file in vis:files() do
                    init_files[file]=true
                    init_file_count=init_file_count+1
                end
            end)
        "#,
        )
        .unwrap();
        let runtime = Runtime::new(std::slice::from_ref(&config.0)).unwrap();
        runtime.eval("assert(init_file_count == 2)").unwrap();
        runtime
            .open_scratch(Some(b"document\n".to_vec()), false)
            .unwrap();
        runtime
            .eval(
                r#"
            local count=0
            for file in vis:files() do count=count+1 end
            assert(count == 3)
            local document=vis.win
            assert(not init_files[document.file])
            vis:message('object iterator regression')
            count=0
            for file in vis:files() do count=count+1 end
            assert(count == 3 and init_files[vis.win.file])
            local windows=0
            for window in vis:windows() do
                windows=windows+1
                assert(window == document)
            end
            assert(windows == 1)
        "#,
            )
            .unwrap();
    }

    #[test]
    fn binary_content_ranges_lines_and_marks_are_live() {
        let (_config, runtime) = runtime(b"");
        runtime
            .eval(
                r#"
            local f = vis.win.file
            assert(f.size == 0 and #f.lines == 0 and f.lines[1] == '')
            assert(f:insert(0, 'a\0b\nlast\n'))
            assert(f:content(0, f.size) == 'a\0b\nlast\n')
            assert(f:content({start=1,finish=3}) == '\0b')
            assert(f:content(100, 5) == '')
            local mark = f:mark_set(4)
            f:snapshot()
            assert(f:insert(0, 'first\n'))
            assert(f:mark_get(mark) == 10)
            assert(#f.lines == 3 and f.lines[2] == 'a\0b')
            f.lines[0] = 'head'
            f.lines[#f.lines+1] = 'tail'
            assert(f.lines[1] == 'head' and f.lines[#f.lines] == 'tail')
            local n = 0
            for line in f:lines_iterator() do n=n+1; assert(line == f.lines[n]) end
            assert(n == #f.lines)
            for _, invalid in ipairs({-1, 1.5}) do
                assert(not pcall(function() f:insert(invalid, '') end))
                assert(not pcall(function() return f:content(invalid, 1) end))
                assert(not pcall(function() f.lines[invalid] = '' end))
            end
        "#,
            )
            .unwrap();
    }

    #[test]
    fn userdata_identity_fields_and_mutable_type_precedence() {
        let (_config, runtime) = runtime(b"hello\n");
        runtime
            .eval(
                r#"
            local w = vis.win
            assert(w == vis.win and w.file == vis.win.file)
            assert(w.selection ~= w.selections[1])
            assert(w.selection ~= w:selections_iterator()())
            w.custom = 'window'
            w.file.custom = 'file'
            w.file.test_method = 'user value'
            vis.types.file.test_method = function(self) return self.custom .. ':' .. self.size end
            assert(w.file:test_method() == 'file:6')
            vis.types.file.test_method = nil
            assert(w.file.test_method == 'user value')
            local original = vis.types.file.content
            vis.types.file.content = function(self) return self.custom end
            assert(w.file:content() == 'file')
            vis.types.file.content = original
            assert(w.custom == 'window' and w.file.custom == 'file')
            local selection=w.selection
            selection.custom = 42
            assert(selection.custom == 42 and w.selection.custom == nil)
            assert(getmetatable(w) == vis.types.window)
            assert(getmetatable(w.file) == vis.types.file)
        "#,
            )
            .unwrap();
    }

    #[test]
    fn logical_selection_identity_survives_reordering_and_removal() {
        let (_config, runtime) = runtime(b"abcdefgh\n");
        let workspace = runtime.workspace();
        workspace
            .borrow_mut()
            .editor
            .create_selection(
                crate::selection::Selection {
                    anchor: 6,
                    cursor: 6,
                },
                false,
            )
            .unwrap();
        runtime
            .eval(
                "first=vis.win.selections[1]; second=vis.win.selections[2]; second.custom='second'",
            )
            .unwrap();
        {
            let mut ws = workspace.borrow_mut();
            let mut pairs = ws.editor.view_identified();
            let primary = pairs[0].0;
            pairs.reverse();
            let mode = ws.editor.mode;
            ws.editor
                .restore_view_with_ids(pairs, primary, mode)
                .unwrap();
        }
        runtime
            .eval(
                r#"
            assert(first ~= vis.win.selections[2] and second ~= vis.win.selections[1])
            assert(first.number == 2 and second.number == 1 and second.custom == 'second')
            first.pos=2
            assert(vis.win.selections[2].pos==2 and second.pos==6)
            second:remove()
            assert(second.pos == nil and #vis.win.selections == 1 and first.pos == vis.win.selection.pos)
            assert(vis.win.selections[0] == nil and vis.win.selections[2] == nil)
        "#,
            )
            .unwrap();
    }

    #[test]
    fn anchored_selection_and_deleted_position_use_core_state() {
        let (_config, runtime) = runtime(b"abcdefgh\n");
        runtime
            .eval(
                r#"
            local s=vis.win.selection
            assert(not s.anchored)
            s.anchored=true; s.pos=3
            assert(s.range.start == 0 and s.range.finish == 4)
            s.anchored=false; s.pos=5
            assert(s.range.start == 5 and s.range.finish == 6)
            assert(not pcall(function() s.range=nil end))
            vis.win.file:snapshot()
            assert(vis.win.file:delete(5,1))
            assert(s.pos == nil)
            assert(s.range == nil)
            vis.win.file:snapshot()
            assert(s.pos == nil)
        "#,
            )
            .unwrap();
    }

    #[test]
    fn closed_window_handles_are_invalid_and_shared_file_survives() {
        let (_config, runtime) = runtime(b"abcdef\n");
        runtime
            .eval(
                r#"
            local old = vis.win
            local file = old.file
            local selection = old.selection
            assert(vis:command('split'))
            assert(vis.win ~= old and vis.win.file == file)
            assert(old:close(true))
            assert(file:content(0,file.size) == 'abcdef\n')
            assert(not pcall(function() return old.file end))
            assert(selection.pos == nil)
            assert(not vis.win:close(true))
        "#,
            )
            .unwrap();
    }

    #[test]
    fn retained_lua_handles_do_not_keep_editor_runtime_alive() {
        let (_config, runtime) = runtime(b"abcdef\n");
        runtime
            .eval("saved_file=vis.win.file; saved_selection=vis.win.selection")
            .unwrap();
        let lua = runtime.lua().clone();
        drop(runtime);
        assert!(lua.load("return saved_file.size").eval::<usize>().is_err());
        assert!(
            lua.load("return saved_selection.pos")
                .eval::<usize>()
                .is_err()
        );
    }

    #[test]
    fn invalid_selection_position_survives_focus_and_revives_after_undo() {
        let (_config, runtime) = runtime(b"abcdefgh\n");
        runtime
            .eval(
                r#"
            local original=vis.win
            local selection=original.selection
            selection.pos=3
            original.file:snapshot()
            assert(original.file:delete(3,1))
            original.file:snapshot()
            assert(selection.pos == nil and selection.range == nil)
            assert(vis:command('new'))
            assert(vis.win ~= original)
            assert(selection.pos == nil and selection.range == nil)
            vis.win=original
            assert(original.selection ~= selection)
            assert(selection.pos == nil and selection.range == nil)
            vis:feedkeys('u')
            assert(original.selection.pos == 3 and selection.pos == 3)
            assert(original.file:content(0,original.file.size) == 'abcdefgh\n')
        "#,
            )
            .unwrap();
    }

    #[test]
    fn last_visual_marks_are_window_local_across_shared_file_focus() {
        let (_config, runtime) = runtime(b"abcdefgh\n");
        runtime
            .eval(
                r#"
            local old=vis.win
            old.marks['^']={{start=1,finish=3}}
            assert(vis:command('split'))
            local new=vis.win
            assert(old.file == new.file)
            new.marks['^']={{start=5,finish=7}}
            assert(old.marks['^'][1].start == 1 and old.marks['^'][1].finish == 3)
            assert(new.marks['^'][1].start == 5 and new.marks['^'][1].finish == 7)
            vis.win=old
            assert(old.marks['^'][1].start == 1 and new.marks['^'][1].start == 5)
            old.marks['^']={{start=2,finish=4}}
            vis.win=new
            assert(old.marks['^'][1].start == 2 and new.marks['^'][1].start == 5)
        "#,
            )
            .unwrap();
    }

    #[test]
    fn named_marks_are_shared_with_native_editor_and_split_views() {
        let (_config, runtime) = runtime(b"abcdefgh\n");
        runtime
            .eval(
                r#"
            old=vis.win
            old.marks.a={{start=2,finish=4}}
            assert(#old.marks == 28 and old.marks.a[1].finish == 4)
            vis:feedkeys("'aM")
            assert(vis.win.selection.pos == 3)
            vis.win.selection.pos=6
            vis:feedkeys("'bm")
            assert(old.marks.b[1].start == 6)
            assert(vis:command('split'))
            assert(vis.win.marks.a[1].start == 2)
            assert(old.marks.b[1].start == 6)
            vis.win.marks.a={{start=1,finish=2}}
            assert(old.marks.a[1].start == 1)
            vis.win.marks['?']={{start=-1,finish=-2}}
            assert(#vis.win.marks['?'] == 0)
        "#,
            )
            .unwrap();
    }

    #[test]
    fn measured_viewport_styles_and_status_reach_owned_render_model() {
        let (_config, runtime) = runtime(b"abcdefgh\n");
        let window = runtime.workspace().borrow().active_window_id();
        runtime.set_viewport(window, 40, 10, 11, 0..9, 1..2);
        runtime
            .eval(
                r#"
            local w=vis.win
            assert(w.width == 40 and w.height == 11)
            assert(w.viewport.width == 40 and w.viewport.height == 10)
            assert(w.viewport.bytes.start == 0 and w.viewport.bytes.finish == 9)
            assert(w.viewport.lines.start == 1 and w.viewport.lines.finish == 2)
            visual_style=vis.ui:style_push('fore:red')
            assert(vis.ui:style_define(visual_style, 'fore:red'))
            assert(not vis.ui:style_define(500, 'fore:red'))
            w:style(visual_style,1,4)
            assert(w:style_pos(visual_style,39,9))
            assert(not w:style_pos(visual_style,40,9))
            assert(w:style_pos(visual_style,0,10))
            assert(not w:style_pos(visual_style,0,11))
            w:status('left','right')
        "#,
            )
            .unwrap();
        let decoration = runtime.decoration(window);
        let style = runtime
            .lua()
            .globals()
            .get::<usize>("visual_style")
            .unwrap();
        assert_eq!(decoration.ranges, vec![(style, 1..5)]);
        assert_eq!(decoration.cells.get(&(39, 9)), Some(&style));
        assert_eq!(decoration.status_left, b"left");
        assert_eq!(decoration.status_right, b"right");
        runtime.eval("vis.win:draw()").unwrap();
        let decoration = runtime.decoration(window);
        assert!(decoration.ranges.is_empty() && decoration.cells.is_empty());
    }

    #[test]
    fn native_style_finish_is_inclusive_for_points_utf8_and_eof() {
        let (_config, runtime) = runtime("aλz".as_bytes());
        let window = runtime.workspace().borrow().active_window_id();
        runtime.set_viewport(window, 20, 4, 5, 0..4, 1..1);
        runtime
            .eval(
                r#"
            local w=vis.win
            endpoint_style=vis.ui:style_push('fore:red')
            -- view.c:vis_win_style styles cells while pos <= end. These
            -- endpoint rules differ from the exclusive file content range.
            w:style(endpoint_style,0,0)
            w:style(endpoint_style,1,2)
            w:style(endpoint_style,3,3)
            w:style(endpoint_style,w.file.size,w.file.size)
            assert(w.file:content({start=1,finish=3}) == 'λ')
            assert(w.file:content({start=3,finish=3}) == '')
            assert(not vis.ui:style_define(500,'fore:blue'))
            w:style(500,0,1)
            assert(w:style_pos(500,0,0))
        "#,
            )
            .unwrap();
        let id = runtime
            .lua()
            .globals()
            .get::<usize>("endpoint_style")
            .unwrap();
        let decoration = runtime.decoration(window);
        assert_eq!(
            decoration.ranges,
            vec![(id, 0..1), (id, 1..3), (id, 3..4), (id, 4..5), (0, 0..2)]
        );
        assert_eq!(decoration.cells.get(&(0, 0)), Some(&0));
        assert!(decoration.ranges[0].1.contains(&0));
        assert!(decoration.ranges[1].1.contains(&2));
        // Preserve the EOF request in the DTO. Native line/cell eligibility
        // still decides painting; C skips zero-byte EOF padding cells.
        assert!(decoration.ranges[3].1.contains(&4));
    }

    fn run_original_fixture(name: &str) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test/lua");
        let bytes = std::fs::read(root.join(format!("{name}.in"))).unwrap();
        let (_config, runtime) = runtime(&bytes);
        runtime.eval(r#"
            local check = assert
            describe = function(_, fn) fn() end
            it = function(_, fn) fn() end
            local same
            same = function(a,b)
                check(type(a) == type(b))
                if type(a) ~= 'table' then check(a == b, tostring(a)..' ~= '..tostring(b)); return end
                for k,v in pairs(a) do same(v,b[k]) end
                for k,v in pairs(b) do same(a[k],v) end
            end
            assert = {
                are = {equal=function(a,b) check(a == b, tostring(a)..' ~= '..tostring(b)) end, same=same},
                has_error = function(fn) check(not pcall(fn), 'expected an error') end,
            }
        "#).unwrap();
        runtime
            .lua()
            .load(std::fs::read_to_string(root.join(format!("{name}.lua"))).unwrap())
            .set_name(name)
            .exec()
            .unwrap();
    }

    #[test]
    fn authoritative_empty_file_fixture() {
        run_original_fixture("file-empty");
    }
    #[test]
    fn authoritative_file_positions_fixture() {
        run_original_fixture("file-positions");
    }
    #[test]
    fn authoritative_lines_fixture() {
        run_original_fixture("lines");
    }
    #[test]
    fn authoritative_cursor_fixture() {
        run_original_fixture("cursor");
    }
}
