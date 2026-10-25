//! Asynchronous communicate streams with bounded, nonblocking pipe I/O.
//!
//! The manager owns children; Lua owns writable stdin handles. Closing or
//! collecting a handle requests EOF and cancellation, as in vis-subprocess.c.
//! No worker threads or Lua callbacks run inside the process manager.
//!
//! Writes/flush release buffered bytes to an asynchronous queue; actual pipe
//! errors become visible at the next operation/poll. Queues are bounded to
//! 8 MiB per stream and 32 simultaneous children. Close permits queued input
//! to progress, then terminates a stalled peer after 150 ms and escalates TERM
//! to KILL after another 150 ms. stdout/stderr are drained before termination
//! events. A leader is observed with waitid(NOWAIT) and reaped only after group
//! cleanup, preventing PID/PGID reuse while cleanup signals remain pending.
//!
//! The writable pipe works with io.output/io.write and can be selected using
//! io.input. Selecting it as input does not make it readable:
//! reads fail with EBADF, lines iterators raise that error, and seeks fail with
//! ESPIPE after validating their arguments. Buffered negative capacities are
//! rejected with EINVAL; libc's ambient allocation-error diagnostics are not
//! reproduced. Native Lua files continue through their original io functions.

use mlua::{
    AnyUserData, Function, Lua, MetaMethod, MultiValue, Result, UserData, UserDataMethods, Value,
};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_PROCESSES: usize = 32;
const MAX_PENDING_INPUT: usize = 8 * 1024 * 1024;
const MAX_SEGMENTS: usize = 1024;
const DEFAULT_BUFFER: usize = 8192;
const CHUNK: usize = 8192;
const IO_ROUNDS: usize = 64;
const CLOSE_GRACE: Duration = Duration::from_millis(150);
const TERM_GRACE: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ResponseKind {
    Stdout,
    Stderr,
    Exit,
    Signal,
}

impl ResponseKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "STDOUT",
            Self::Stderr => "STDERR",
            Self::Exit => "EXIT",
            Self::Signal => "SIGNAL",
        }
    }
}

#[derive(Debug)]
pub(super) struct Response {
    pub(super) name: String,
    pub(super) kind: ResponseKind,
    pub(super) code: Option<i32>,
    pub(super) data: Option<Vec<u8>>,
}

#[derive(Clone, Copy)]
enum Buffering {
    Full(usize),
    Line,
    None,
}

struct Input {
    buffer: Vec<u8>,
    queue: VecDeque<Vec<u8>>,
    offset: usize,
    pending: usize,
    mode: Buffering,
    closed: bool,
    cancel: bool,
    error: Option<(io::ErrorKind, String, Option<i32>)>,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            buffer: Vec::new(),
            queue: VecDeque::new(),
            offset: 0,
            pending: 0,
            mode: Buffering::Full(DEFAULT_BUFFER),
            closed: false,
            cancel: false,
            error: None,
        }
    }
}

impl Input {
    fn check(&self) -> io::Result<()> {
        if let Some((kind, message, code)) = &self.error {
            return Err(code
                .map(io::Error::from_raw_os_error)
                .unwrap_or_else(|| io::Error::new(*kind, message.clone())));
        }
        if self.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "attempt to use a closed file",
            ));
        }
        Ok(())
    }

    fn release(&mut self, length: usize) {
        if length == 0 {
            return;
        }
        let remainder = self.buffer.split_off(length);
        let ready = std::mem::replace(&mut self.buffer, remainder);
        self.pending += ready.len();
        self.queue.push_back(ready);
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.check()?;
        if self
            .pending
            .saturating_add(self.buffer.len())
            .saturating_add(data.len())
            > MAX_PENDING_INPUT
            || self.queue.len() >= MAX_SEGMENTS
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "subprocess input queue is full",
            ));
        }
        self.buffer.extend_from_slice(data);
        let release = match self.mode {
            Buffering::None => self.buffer.len(),
            Buffering::Line => self
                .buffer
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |index| index + 1),
            Buffering::Full(size) => self.buffer.len() / size * size,
        };
        self.release(release);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check()?;
        if self.queue.len() >= MAX_SEGMENTS {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "subprocess input queue is full",
            ));
        }
        self.release(self.buffer.len());
        Ok(())
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.release(self.buffer.len());
        self.closed = true;
        self.cancel = true;
    }

    fn fail(&mut self, error: io::Error) {
        self.error = Some((error.kind(), error.to_string(), error.raw_os_error()));
        self.discard();
    }

    fn discard(&mut self) {
        self.buffer.clear();
        self.queue.clear();
        self.pending = 0;
        self.offset = 0;
    }
}

pub(super) struct ProcessStream {
    input: Rc<RefCell<Input>>,
}

impl ProcessStream {
    fn ensure_open(&self) -> Result<()> {
        if self.input.borrow().closed {
            return Err(mlua::Error::runtime("attempt to use a closed file"));
        }
        Ok(())
    }

    fn close(&self) -> Result<()> {
        self.ensure_open()?;
        self.input.borrow_mut().close();
        Ok(())
    }
}

impl Drop for ProcessStream {
    fn drop(&mut self) {
        self.input.borrow_mut().close();
    }
}

fn io_result(lua: &Lua, result: io::Result<()>) -> Result<MultiValue> {
    match result {
        Ok(()) => Ok(MultiValue::from_vec(vec![Value::Boolean(true)])),
        Err(error) => Ok(MultiValue::from_vec(vec![
            Value::Nil,
            Value::String(lua.create_string(error.to_string())?),
            Value::Integer(i64::from(error_number(&error))),
        ])),
    }
}

fn error_number(error: &io::Error) -> i32 {
    if let Some(code) = error.raw_os_error() {
        return code;
    }
    #[cfg(unix)]
    {
        match error.kind() {
            io::ErrorKind::WouldBlock => rustix::io::Errno::AGAIN,
            io::ErrorKind::BrokenPipe => rustix::io::Errno::PIPE,
            io::ErrorKind::InvalidInput => rustix::io::Errno::INVAL,
            _ => rustix::io::Errno::IO,
        }
        .raw_os_error()
    }
    #[cfg(not(unix))]
    {
        5
    }
}

fn write_stream(lua: &Lua, handle: AnyUserData, arguments: MultiValue) -> Result<MultiValue> {
    let stream = handle.borrow::<ProcessStream>()?;
    stream.ensure_open()?;
    let mut result = Ok(());
    for argument in arguments {
        let text = lua
            .coerce_string(argument)?
            .ok_or_else(|| mlua::Error::runtime("string or number expected in file:write"))?;
        // Lua's native f_write processes arguments in order. A later type
        // error leaves earlier writes intact; it does not validate atomically.
        if result.is_ok() {
            result = stream.input.borrow_mut().write(&text.as_bytes());
        }
    }
    drop(stream);
    match result {
        Ok(()) => Ok(MultiValue::from_vec(vec![Value::UserData(handle)])),
        Err(error) => io_result(lua, Err(error)),
    }
}

fn validate_read_arguments(lua: &Lua, arguments: &MultiValue) -> Result<()> {
    // Native g_read stops on the first failed read. read-all reports success
    // internally even with EBADF, so it validates subsequent arguments before
    // reporting the final I/O error. Line/number/count reads stop immediately.
    for argument in arguments {
        if matches!(argument, Value::Integer(_) | Value::Number(_)) {
            let count = lua
                .coerce_integer(argument.clone())?
                .ok_or_else(|| mlua::Error::runtime("read count has no integer representation"))?;
            if count < 0 {
                return Err(mlua::Error::runtime("resulting string too large"));
            }
            return Ok(());
        } else {
            let format = lua
                .coerce_string(argument.clone())?
                .ok_or_else(|| mlua::Error::runtime("string or number expected in file:read"))?;
            let bytes = format.as_bytes();
            let format = bytes.strip_prefix(b"*").unwrap_or(&bytes);
            match format.first() {
                Some(b'a') => {}
                Some(b'n' | b'l' | b'L') => return Ok(()),
                _ => return Err(mlua::Error::runtime("invalid read format")),
            }
        }
    }
    Ok(())
}

fn read_stream(lua: &Lua, stream: &ProcessStream, arguments: MultiValue) -> Result<MultiValue> {
    stream.ensure_open()?;
    validate_read_arguments(lua, &arguments)?;
    io_result(lua, Err(io::Error::from_raw_os_error(9)))
}

fn lines_stream(lua: &Lua, handle: AnyUserData, arguments: MultiValue) -> Result<Function> {
    handle.borrow::<ProcessStream>()?.ensure_open()?;
    lua.create_function(move |lua, _: MultiValue| -> Result<()> {
        handle.borrow::<ProcessStream>()?.ensure_open()?;
        validate_read_arguments(lua, &arguments)?;
        Err(mlua::Error::runtime(
            io::Error::from_raw_os_error(9).to_string(),
        ))
    })
}

impl UserData for ProcessStream {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_function("write", |lua, mut args: MultiValue| {
            let Some(Value::UserData(handle)) = args.pop_front() else {
                return Err(mlua::Error::runtime("file expected"));
            };
            write_stream(lua, handle, args)
        });
        methods.add_method("flush", |lua, stream, ()| {
            stream.ensure_open()?;
            io_result(lua, stream.input.borrow_mut().flush())
        });
        methods.add_method("close", |_, stream, ()| {
            stream.close()?;
            Ok(true)
        });
        methods.add_method(
            "setvbuf",
            |lua, stream, (mode, size): (Value, Option<Value>)| {
                stream.ensure_open()?;
                let mode = lua
                    .coerce_string(mode)?
                    .ok_or_else(|| mlua::Error::runtime("buffering mode must be a string"))?;
                let mode = mode.to_str()?;
                if !matches!(mode.as_ref(), "no" | "line" | "full") {
                    return Err(mlua::Error::runtime("invalid buffering mode"));
                }
                let size = size
                    .map(|value| {
                        lua.coerce_integer(value)?
                            .ok_or_else(|| mlua::Error::runtime("buffer size must be an integer"))
                    })
                    .transpose()?;
                if mode.as_ref() != "no" && size.is_some_and(|size| size < 0) {
                    return io_result(
                        lua,
                        Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "invalid buffered stream capacity",
                        )),
                    );
                }
                let mode = match mode.as_ref() {
                    "no" => Buffering::None,
                    "line" => Buffering::Line,
                    "full" => Buffering::Full(size.map_or(DEFAULT_BUFFER, |n| n as usize).max(1)),
                    _ => return Err(mlua::Error::runtime("invalid buffering mode")),
                };
                let mut input = stream.input.borrow_mut();
                let result = input.flush();
                if result.is_ok() {
                    input.mode = mode;
                }
                io_result(lua, result)
            },
        );
        methods.add_method("seek", |lua, stream, arguments: MultiValue| {
            stream.ensure_open()?;
            if let Some(value) = arguments.front().filter(|value| !value.is_nil()) {
                let mode = lua
                    .coerce_string(value.clone())?
                    .ok_or_else(|| mlua::Error::runtime("seek mode must be a string"))?;
                if !matches!(mode.as_bytes().as_ref(), b"set" | b"cur" | b"end") {
                    return Err(mlua::Error::runtime("invalid seek mode"));
                }
            }
            if let Some(value) = arguments.get(1).filter(|value| !value.is_nil()) {
                lua.coerce_integer(value.clone())?
                    .ok_or_else(|| mlua::Error::runtime("seek offset must be an integer"))?;
            }
            io_result(lua, Err(io::Error::from_raw_os_error(29)))
        });
        methods.add_method("read", read_stream);
        methods.add_function("lines", |lua, mut arguments: MultiValue| {
            let Some(Value::UserData(handle)) = arguments.pop_front() else {
                return Err(mlua::Error::runtime("file expected"));
            };
            lines_stream(lua, handle, arguments)
        });
        methods.add_meta_method(MetaMethod::Close, |_, stream, _: Value| {
            if !stream.input.borrow().closed {
                stream.input.borrow_mut().close();
            }
            Ok(())
        });
    }
}

/// Route ordinary Lua io operations for the safe writable stream userdata.
/// Native files retain their existing behavior; Lua never receives raw FILE*.
pub(super) fn install_io(lua: &Lua) -> Result<()> {
    let io: mlua::Table = lua.globals().get("io")?;
    let input: Rc<RefCell<Option<AnyUserData>>> = Rc::new(RefCell::new(None));
    let output: Rc<RefCell<Option<AnyUserData>>> = Rc::new(RefCell::new(None));
    let original: Function = io.get("type")?;
    io.set(
        "type",
        lua.create_function(move |lua, value: Value| {
            if let Value::UserData(handle) = &value {
                if handle.is::<ProcessStream>() {
                    return Ok(Value::String(lua.create_string(
                        if handle.borrow::<ProcessStream>()?.input.borrow().closed {
                            "closed file"
                        } else {
                            "file"
                        },
                    )?));
                }
            }
            original.call::<Value>(value)
        })?,
    )?;
    let original: Function = io.get("input")?;
    let current = input.clone();
    io.set(
        "input",
        lua.create_function(move |_, arguments: MultiValue| {
            if matches!(arguments.front(), None | Some(Value::Nil)) {
                if let Some(handle) = current.borrow().as_ref() {
                    return Ok(Value::UserData(handle.clone()));
                }
            } else if let Some(Value::UserData(handle)) = arguments.front() {
                if handle.is::<ProcessStream>() {
                    handle.borrow::<ProcessStream>()?.ensure_open()?;
                    *current.borrow_mut() = Some(handle.clone());
                    return Ok(Value::UserData(handle.clone()));
                }
            }
            let result = original.call::<Value>(arguments)?;
            if matches!(result, Value::UserData(_)) {
                *current.borrow_mut() = None;
            }
            Ok(result)
        })?,
    )?;
    let original: Function = io.get("read")?;
    let current = input.clone();
    io.set(
        "read",
        lua.create_function(move |lua, arguments: MultiValue| {
            let handle = current.borrow().clone();
            if let Some(handle) = handle {
                let stream = handle.borrow::<ProcessStream>()?;
                read_stream(lua, &stream, arguments)
            } else {
                original.call(arguments)
            }
        })?,
    )?;
    let original: Function = io.get("lines")?;
    io.set(
        "lines",
        lua.create_function(move |lua, mut arguments: MultiValue| {
            if matches!(arguments.front(), None | Some(Value::Nil)) {
                let handle = input.borrow().clone();
                if let Some(handle) = handle {
                    arguments.pop_front();
                    return lines_stream(lua, handle, arguments);
                }
            }
            original.call(arguments)
        })?,
    )?;
    let original: Function = io.get("output")?;
    let current = output.clone();
    io.set(
        "output",
        lua.create_function(move |_, args: MultiValue| {
            if matches!(args.front(), None | Some(Value::Nil)) {
                if let Some(handle) = current.borrow().as_ref() {
                    return Ok(Value::UserData(handle.clone()));
                }
            } else if let Some(Value::UserData(handle)) = args.front() {
                if handle.is::<ProcessStream>() {
                    handle.borrow::<ProcessStream>()?.ensure_open()?;
                    *current.borrow_mut() = Some(handle.clone());
                    return Ok(Value::UserData(handle.clone()));
                }
            }
            let result = original.call::<Value>(args)?;
            if matches!(result, Value::UserData(_)) {
                *current.borrow_mut() = None;
            }
            Ok(result)
        })?,
    )?;
    let original: Function = io.get("write")?;
    let current = output.clone();
    io.set(
        "write",
        lua.create_function(move |lua, args: MultiValue| {
            let handle = current.borrow().clone();
            if let Some(handle) = handle {
                write_stream(lua, handle, args)
            } else {
                original.call(args)
            }
        })?,
    )?;
    let original: Function = io.get("flush")?;
    let current = output.clone();
    io.set(
        "flush",
        lua.create_function(move |lua, args: MultiValue| {
            let handle = current.borrow().clone();
            if let Some(handle) = handle {
                let stream = handle.borrow::<ProcessStream>()?;
                stream.ensure_open()?;
                io_result(lua, stream.input.borrow_mut().flush())
            } else {
                original.call(args)
            }
        })?,
    )?;
    let original: Function = io.get("close")?;
    io.set(
        "close",
        lua.create_function(move |_, args: MultiValue| {
            let handle = match args.front() {
                Some(Value::UserData(handle)) if handle.is::<ProcessStream>() => {
                    Some(handle.clone())
                }
                None | Some(Value::Nil) => output.borrow().clone(),
                _ => None,
            };
            if let Some(handle) = handle {
                handle.borrow::<ProcessStream>()?.close()?;
                Ok(MultiValue::from_vec(vec![Value::Boolean(true)]))
            } else {
                original.call(args)
            }
        })?,
    )?;
    Ok(())
}

#[derive(Default)]
pub(super) struct Manager {
    #[cfg(unix)]
    entries: Vec<Entry>,
    interrupt: Option<Arc<AtomicBool>>,
}

impl Manager {
    pub(super) fn set_interrupt(&mut self, interrupt: Arc<AtomicBool>) {
        self.interrupt = Some(interrupt);
    }
    pub(super) fn pending(&self) -> bool {
        #[cfg(unix)]
        {
            !self.entries.is_empty()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    #[cfg(all(
        unix,
        not(any(target_os = "openbsd", target_os = "redox", target_os = "cygwin"))
    ))]
    pub(super) fn spawn(
        &mut self,
        name: &str,
        command: &str,
        shell: &Path,
        directory: &Path,
    ) -> io::Result<ProcessStream> {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        if self.entries.len() >= MAX_PROCESSES {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "too many asynchronous subprocesses",
            ));
        }
        if name.len() > 4096 || name.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid subprocess name",
            ));
        }
        let mut command_line = Command::new(shell);
        command_line
            .args(["-c", command])
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command_line.spawn()?;
        let mut entry = Entry {
            name: name.into(),
            input: child.stdin.take(),
            output: child.stdout.take(),
            errors: child.stderr.take(),
            child,
            shared: Rc::new(RefCell::new(Input::default())),
            exited: false,
            owned: true,
            closing: None,
            terminating: None,
            killed: false,
        };
        let setup = (|| {
            nonblocking(entry.input.as_ref().unwrap())?;
            nonblocking(entry.output.as_ref().unwrap())?;
            nonblocking(entry.errors.as_ref().unwrap())
        })();
        if let Err(error) = setup {
            entry.signal(rustix::process::Signal::KILL);
            let _ = entry.child.wait();
            return Err(error);
        }
        let stream = ProcessStream {
            input: entry.shared.clone(),
        };
        self.entries.push(entry);
        Ok(stream)
    }

    #[cfg(any(
        not(unix),
        target_os = "openbsd",
        target_os = "redox",
        target_os = "cygwin"
    ))]
    pub(super) fn spawn(
        &mut self,
        _: &str,
        _: &str,
        _: &Path,
        _: &Path,
    ) -> io::Result<ProcessStream> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "asynchronous communicate requires Unix nonblocking pipes and waitid(NOWAIT)",
        ))
    }

    /// Persist cancellation independently of the shared physical interrupt bit.
    /// Keep entries until normal polling can deliver their terminal responses.
    pub(super) fn request_cancel_all(&mut self) {
        #[cfg(unix)]
        for entry in &mut self.entries {
            entry.input.take();
            let mut input = entry.shared.borrow_mut();
            input.closed = true;
            input.cancel = true;
            input.discard();
            drop(input);
            if entry.terminating.is_none() {
                entry.signal(rustix::process::Signal::TERM);
                entry.terminating = Some(Instant::now());
            }
        }
    }

    pub(super) fn poll(&mut self) -> Vec<Response> {
        // Without a restoration callback this path cannot return an error.
        self.poll_before_reap(None)
            .expect("process polling without a callback")
    }

    fn poll_before_reap(
        &mut self,
        before_reap: Option<&dyn Fn() -> io::Result<()>>,
    ) -> io::Result<Vec<Response>> {
        let mut responses = Vec::new();
        let mut restore_error = None;
        #[cfg(unix)]
        {
            let interrupted = self
                .interrupt
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Relaxed));
            for entry in &mut self.entries {
                entry.tick(interrupted, &mut responses);
            }
            let mut index = 0;
            while index < self.entries.len() {
                if self.entries[index].finished() {
                    let mut entry = self.entries.remove(index);
                    entry.shared.borrow_mut().closed = true;
                    entry.shared.borrow_mut().discard();
                    entry.input.take();
                    // tick() has sent every cleanup signal, but the zombie
                    // leader still reserves its PID until this observer returns.
                    if let Some(restore) = before_reap {
                        if let Err(error) = restore() {
                            restore_error = Some(error);
                        }
                    }
                    if let Ok(status) = entry.child.wait() {
                        use std::os::unix::process::ExitStatusExt;
                        let (kind, code) = match status.signal() {
                            Some(signal) => (ResponseKind::Signal, signal),
                            None => (ResponseKind::Exit, status.code().unwrap_or(255)),
                        };
                        responses.push(Response {
                            name: entry.name,
                            kind,
                            code: Some(code),
                            data: None,
                        });
                    } else {
                        responses.push(Response {
                            name: entry.name,
                            kind: ResponseKind::Exit,
                            code: Some(255),
                            data: None,
                        });
                    }
                } else {
                    index += 1;
                }
            }
        }
        match restore_error {
            Some(error) => Err(error),
            None => Ok(responses),
        }
    }

    pub(super) fn cancel_all(&mut self) {
        let _ = self.cancel_before_reap(None);
    }

    fn cancel_before_reap(
        &mut self,
        before_reap: Option<&dyn Fn() -> io::Result<()>>,
    ) -> io::Result<()> {
        let mut restored = Ok(());
        #[cfg(unix)]
        {
            if self.entries.is_empty() {
                return before_reap.map_or(Ok(()), |restore| restore());
            }
            for entry in &mut self.entries {
                entry.input.take();
                entry.shared.borrow_mut().closed = true;
                entry.shared.borrow_mut().discard();
                entry.signal(rustix::process::Signal::TERM);
            }
            let deadline = Instant::now() + TERM_GRACE;
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            for entry in &mut self.entries {
                // Kill the group even after its leader exited: descendants may
                // ignore TERM and close all inherited pipe descriptors.
                entry.signal(rustix::process::Signal::KILL);
            }
            // Foreground ownership is restored after cleanup signals and
            // before any leader is reaped, including handoff/error cleanup.
            if let Some(restore) = before_reap {
                restored = restore();
            }
            for entry in &mut self.entries {
                if entry.owned {
                    let _ = entry.child.wait();
                }
            }
            self.entries.clear();
        }
        restored
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

/// Synchronous pipe collection driven by the same nonblocking process manager.
/// SIGINT remains observable until the caller consumes its shared flag.
#[cfg(test)]
pub(super) fn run_pipe(
    command: &str,
    input: &[u8],
    shell: &Path,
    directory: &Path,
    interrupt: Arc<AtomicBool>,
) -> io::Result<(i32, Vec<u8>, Vec<u8>)> {
    run_pipe_with_observer(command, input, shell, directory, interrupt, None)
}

/// Hand off foreground ownership to the new group, then resume it in case its
/// first TTY read raced the handoff. Restore ownership after group cleanup and
/// before reaping the reserved leader, on success and ordinary error paths.
pub(super) fn run_pipe_with_observer(
    command: &str,
    input: &[u8],
    shell: &Path,
    directory: &Path,
    interrupt: Arc<AtomicBool>,
    observer: Option<&dyn Fn(Option<u32>) -> io::Result<()>>,
) -> io::Result<(i32, Vec<u8>, Vec<u8>)> {
    let mut manager = Manager::default();
    manager.set_interrupt(interrupt);
    let stream = manager.spawn("pipe", command, shell, directory)?;
    let restored = Cell::new(false);
    let restore = || {
        restored.set(true);
        observer.map_or(Ok(()), |observe| observe(None))
    };
    #[cfg(unix)]
    if let Some(observe) = observer {
        let child = manager.entries[0].child.id();
        if let Err(error) = observe(Some(child)) {
            let _ = manager.cancel_before_reap(Some(&restore));
            return Err(error);
        }
        // A foreground reader can have received SIGTTIN between spawn and
        // handoff. Resume the still-owned group after the handoff completes.
        manager.entries[0].signal(rustix::process::Signal::CONT);
    }
    let result = collect_pipe(&mut manager, &stream, input, Some(&restore));
    if result.is_err() {
        let restore = if restored.get() {
            None
        } else {
            Some(&restore as &dyn Fn() -> io::Result<()>)
        };
        let _ = manager.cancel_before_reap(restore);
    }
    result
}

fn collect_pipe(
    manager: &mut Manager,
    stream: &ProcessStream,
    input: &[u8],
    before_reap: Option<&dyn Fn() -> io::Result<()>>,
) -> io::Result<(i32, Vec<u8>, Vec<u8>)> {
    const MAX_COLLECTED: usize = 32 * 1024 * 1024;
    stream.input.borrow_mut().mode = Buffering::None;
    let (mut output, mut errors) = (Vec::new(), Vec::new());
    let mut cursor = 0;
    let mut status = -1;
    while manager.pending() {
        {
            let mut writer = stream.input.borrow_mut();
            // A closed peer can legitimately ignore input (e.g. `head -c 1`).
            if writer.error.as_ref().is_some_and(|(kind, _, _)| {
                matches!(kind, io::ErrorKind::BrokenPipe | io::ErrorKind::WriteZero)
            }) {
                cursor = input.len();
            }
            if !writer.closed && writer.error.is_none() && cursor < input.len() {
                let end = (cursor + 64 * 1024).min(input.len());
                match writer.write(&input[cursor..end]) {
                    Ok(()) => cursor = end,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error),
                }
            }
            if cursor == input.len() && !writer.closed {
                let buffered = writer.buffer.len();
                writer.release(buffered);
                // EOF without cancellation: unlike communicate:close(), a
                // synchronous pipe may continue working after its input ends.
                writer.closed = true;
            }
        }
        for response in manager.poll_before_reap(before_reap)? {
            match response.kind {
                ResponseKind::Stdout | ResponseKind::Stderr => {
                    let bytes = response.data.unwrap_or_default();
                    if output
                        .len()
                        .saturating_add(errors.len())
                        .saturating_add(bytes.len())
                        > MAX_COLLECTED
                    {
                        return Err(io::Error::other(
                            "subprocess output exceeds 32 MiB collection bound",
                        ));
                    }
                    if response.kind == ResponseKind::Stdout {
                        output.extend(bytes);
                    } else {
                        errors.extend(bytes);
                    }
                }
                ResponseKind::Exit => status = response.code.unwrap_or(-1),
                ResponseKind::Signal => status = -1,
            }
        }
        if manager.pending() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    Ok((status, output, errors))
}

#[cfg(unix)]
fn nonblocking(fd: &impl std::os::fd::AsFd) -> io::Result<()> {
    rustix::fs::fcntl_setfl(
        fd,
        rustix::fs::fcntl_getfl(fd)? | rustix::fs::OFlags::NONBLOCK,
    )?;
    Ok(())
}

#[cfg(unix)]
struct Entry {
    name: String,
    child: std::process::Child,
    input: Option<std::process::ChildStdin>,
    output: Option<std::process::ChildStdout>,
    errors: Option<std::process::ChildStderr>,
    shared: Rc<RefCell<Input>>,
    exited: bool,
    owned: bool,
    closing: Option<Instant>,
    terminating: Option<Instant>,
    killed: bool,
}

#[cfg(unix)]
impl Entry {
    fn signal(&mut self, signal: rustix::process::Signal) {
        use rustix::process::{Pid, kill_process_group};
        if !self.owned {
            return;
        }
        if let Some(group) = i32::try_from(self.child.id())
            .ok()
            .and_then(Pid::from_raw)
            .filter(|pid| *pid != Pid::INIT)
        {
            let _ = kill_process_group(group, signal);
        } else {
            let _ = self.child.kill();
        }
    }

    #[cfg(not(any(target_os = "openbsd", target_os = "redox", target_os = "cygwin")))]
    fn observe_exit(&self) -> io::Result<bool> {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        let pid = i32::try_from(self.child.id())
            .ok()
            .and_then(Pid::from_raw)
            .ok_or_else(|| io::Error::other("invalid child process ID"))?;
        // Do not reap here: holding the zombie leader reserves its PID/PGID
        // until all descendant cleanup signals have been sent.
        Ok(waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )?
        .is_some())
    }

    #[cfg(any(target_os = "openbsd", target_os = "redox", target_os = "cygwin"))]
    fn observe_exit(&self) -> io::Result<bool> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "waitid(NOWAIT) is unavailable",
        ))
    }

    fn pump_input(&mut self) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(input) = self.input.as_mut() else {
            return false;
        };
        let before = shared.pending;
        for _ in 0..IO_ROUNDS {
            let Some(data) = shared.queue.front() else {
                break;
            };
            match input.write(&data[shared.offset..]) {
                Ok(0) => {
                    shared.fail(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "subprocess stdin closed",
                    ));
                    break;
                }
                Ok(written) => {
                    shared.offset += written;
                    shared.pending -= written;
                    if shared.offset == shared.queue.front().unwrap().len() {
                        shared.queue.pop_front();
                        shared.offset = 0;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    shared.fail(error);
                    break;
                }
            }
        }
        let progress = shared.pending < before;
        if (shared.closed || shared.error.is_some()) && shared.pending == 0 {
            self.input.take();
        }
        progress
    }

    fn tick(&mut self, interrupted: bool, responses: &mut Vec<Response>) {
        let input_progress = self.pump_input();
        read_pipe(
            &self.name,
            &mut self.output,
            ResponseKind::Stdout,
            responses,
        );
        read_pipe(
            &self.name,
            &mut self.errors,
            ResponseKind::Stderr,
            responses,
        );
        if !self.exited {
            match self.observe_exit() {
                Ok(exited) => self.exited = exited,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    let no_child =
                        error.raw_os_error() == Some(rustix::io::Errno::CHILD.raw_os_error());
                    responses.push(Response {
                        name: self.name.clone(),
                        kind: ResponseKind::Stderr,
                        code: None,
                        data: Some(format!("subprocess wait: {error}").into_bytes()),
                    });
                    self.shared.borrow_mut().fail(error);
                    if no_child {
                        // Another reaper consumed this PID: never signal a PID
                        // or process group that the OS could already reuse.
                        self.owned = false;
                        self.exited = true;
                        self.killed = true;
                        self.input.take();
                        self.output.take();
                        self.errors.take();
                    } else if self.terminating.is_none() {
                        self.signal(rustix::process::Signal::TERM);
                        self.terminating = Some(Instant::now());
                    }
                }
            }
        }
        let closed = self.shared.borrow().cancel;
        if closed && (self.closing.is_none() || input_progress) {
            self.closing = Some(Instant::now());
        }
        if self.exited && self.output.is_none() && self.errors.is_none() {
            self.signal(rustix::process::Signal::KILL);
            self.killed = true;
        } else if self.terminating.is_none()
            && (interrupted
                || self.exited
                || self
                    .closing
                    .is_some_and(|start| start.elapsed() >= CLOSE_GRACE))
        {
            self.input.take();
            self.shared.borrow_mut().closed = true;
            self.shared.borrow_mut().discard();
            self.signal(rustix::process::Signal::TERM);
            self.terminating = Some(Instant::now());
        }
        if !self.killed
            && self
                .terminating
                .is_some_and(|start| start.elapsed() >= TERM_GRACE)
        {
            self.signal(rustix::process::Signal::KILL);
            self.killed = true;
        }
    }

    fn finished(&self) -> bool {
        self.exited
            && self.output.is_none()
            && self.errors.is_none()
            && (self.terminating.is_none() || self.killed)
    }
}

#[cfg(unix)]
fn read_pipe<R: Read>(
    name: &str,
    pipe: &mut Option<R>,
    kind: ResponseKind,
    responses: &mut Vec<Response>,
) {
    let Some(reader) = pipe.as_mut() else {
        return;
    };
    let mut buffer = [0; CHUNK];
    for _ in 0..IO_ROUNDS {
        match reader.read(&mut buffer) {
            Ok(0) => {
                pipe.take();
                break;
            }
            Ok(length) => responses.push(Response {
                name: name.into(),
                kind,
                code: None,
                data: Some(buffer[..length].to_vec()),
            }),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                pipe.take();
                break;
            }
        }
    }
}

#[cfg(all(
    test,
    unix,
    not(any(target_os = "openbsd", target_os = "redox", target_os = "cygwin"))
))]
mod tests {
    use super::*;

    fn spawn(manager: &mut Manager, command: &str) -> ProcessStream {
        manager
            .spawn("test", command, Path::new("/bin/sh"), &std::env::temp_dir())
            .unwrap()
    }

    fn collect(manager: &mut Manager) -> Vec<Response> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut responses = Vec::new();
        while manager.pending() {
            responses.extend(manager.poll());
            assert!(Instant::now() < deadline, "process did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
        responses
    }

    fn bytes(responses: &[Response], kind: ResponseKind) -> Vec<u8> {
        responses
            .iter()
            .filter(|response| response.kind == kind)
            .flat_map(|response| response.data.as_deref().unwrap_or_default())
            .copied()
            .collect()
    }

    #[test]
    fn binary_stdout_stderr_are_drained_before_exactly_one_exit() {
        let mut manager = Manager::default();
        let _stream = spawn(
            &mut manager,
            "printf 'a\\000b'; printf 'err\\000or' >&2; exit 7",
        );
        let responses = collect(&mut manager);
        assert_eq!(bytes(&responses, ResponseKind::Stdout), b"a\0b");
        assert_eq!(bytes(&responses, ResponseKind::Stderr), b"err\0or");
        assert_eq!(
            responses
                .iter()
                .filter(|response| response.code.is_some())
                .count(),
            1
        );
        assert_eq!(responses.last().unwrap().kind, ResponseKind::Exit);
        assert_eq!(responses.last().unwrap().code, Some(7));
        assert!(responses.iter().all(|response| response.name == "test"));
        assert!(manager.poll().is_empty());
        assert_eq!(ResponseKind::Stderr.as_str(), "STDERR");
    }

    #[test]
    fn large_bidirectional_io_does_not_deadlock_and_close_flushes_input() {
        let mut manager = Manager::default();
        let stream = spawn(
            &mut manager,
            "dd if=/dev/zero bs=1024 count=1024 2>/dev/null; dd if=/dev/zero bs=1024 count=1024 >&2 2>/dev/null; cat",
        );
        let input: Vec<_> = (0..2 * 1024 * 1024)
            .map(|index| (index % 256) as u8)
            .collect();
        stream.input.borrow_mut().mode = Buffering::None;
        stream.input.borrow_mut().write(&input).unwrap();
        stream.close().unwrap();
        let responses = collect(&mut manager);
        let output = bytes(&responses, ResponseKind::Stdout);
        assert_eq!(output.len(), 3 * 1024 * 1024);
        assert!(output[..1024 * 1024].iter().all(|byte| *byte == 0));
        assert!(output[1024 * 1024..] == input);
        let errors = bytes(&responses, ResponseKind::Stderr);
        assert_eq!(errors.len(), 1024 * 1024);
        assert!(errors.iter().all(|byte| *byte == 0));
        assert_eq!(responses.last().unwrap().code, Some(0));
    }

    #[test]
    fn buffering_and_queue_limits_are_real_and_recoverable() {
        let mut input = Input::default();
        input.write(b"small\n").unwrap();
        assert_eq!(input.pending, 0);
        input.flush().unwrap();
        assert_eq!(input.pending, 6);
        input.mode = Buffering::Line;
        input.write(b"line\nrest").unwrap();
        assert_eq!(input.buffer, b"rest");
        assert_eq!(input.pending, 11);
        assert_eq!(
            input.write(&vec![0; MAX_PENDING_INPUT]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(input.buffer, b"rest");
        input.close();
        assert_eq!(input.pending, 15);
        assert_eq!(
            input.write(b"x").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn lua_file_methods_and_io_routing_preserve_native_files() {
        let lua = Lua::new();
        install_io(&lua).unwrap();
        let mut manager = Manager::default();
        let stream = spawn(&mut manager, "cat");
        lua.globals()
            .set("stream", lua.create_userdata(stream).unwrap())
            .unwrap();
        lua.load(
            r#"
            local previous = io.output()
            assert(io.type(previous)=='file' and io.type({})==nil)
            assert(io.type(stream)=='file')
            assert(stream:setvbuf('no'))
            assert(stream:write('bin\0',123)==stream)
            assert(io.output(stream)==stream and io.output()==stream)
            assert(io.write('global\n')==stream)
            assert(io.flush())
            assert(io.output(previous)==previous)
            assert(io.close(stream))
            assert(io.type(stream)=='closed file')
            assert(not pcall(stream.write,stream,'closed'))
            local temporary = assert(io.tmpfile())
            assert(temporary:write('native'))
            assert(io.type(temporary)=='file' and io.close(temporary))
            assert(io.type(temporary)=='closed file')
        "#,
        )
        .exec()
        .unwrap();
        let responses = collect(&mut manager);
        assert_eq!(
            bytes(&responses, ResponseKind::Stdout),
            b"bin\x00123global\n"
        );
        assert_eq!(responses.last().unwrap().code, Some(0));
    }

    #[test]
    fn writable_stream_input_routing_and_partial_write_match_native_file_contract() {
        let lua = Lua::new();
        install_io(&lua).unwrap();
        let mut manager = Manager::default();
        let stream = spawn(&mut manager, "head -c 6");
        lua.globals()
            .set("stream", lua.create_userdata(stream).unwrap())
            .unwrap();
        lua.load(
            r#"
            original_input=io.input()
            local original_output=io.output()
            assert(io.input(stream)==stream and io.input()==stream)
            assert(io.output()==original_output)
            local result,message,code=io.read()
            assert(result==nil and type(message)=='string' and code==9)
            result,message,code=stream:read('*l',{})
            assert(result==nil and code==9)
            assert(not pcall(stream.read,stream,'*a',{}))
            assert(not pcall(stream.read,stream,'invalid'))
            assert(not pcall(stream.read,stream,true))
            assert(not pcall(stream.read,stream,-1))
            local iterator=io.lines()
            assert(type(iterator)=='function' and not pcall(iterator))
            iterator=stream:lines('invalid')
            assert(type(iterator)=='function' and not pcall(iterator))
            result,message,code=stream:seek()
            assert(result==nil and type(message)=='string' and code==29)
            assert(not pcall(stream.seek,stream,'invalid'))
            assert(not pcall(stream.seek,stream,'cur','invalid'))
            assert(not pcall(stream.seek,stream,'cur',1.5))
            assert(stream:setvbuf('no',-1))
            assert(stream:setvbuf('full',-1)==nil)
            assert(stream:setvbuf('line',-1)==nil)
            assert(stream:setvbuf('no','17'))
            assert(stream:setvbuf('full',0))
            assert(not pcall(stream.setvbuf,stream,'invalid'))
            assert(not pcall(stream.setvbuf,stream,'full',1.5))
            local native=assert(io.tmpfile())
            assert(native:write('native'))
            assert(native:seek('set',0)==0)
            assert(io.input(native)==native and io.read('*a')=='native')
            assert(io.input(stream)==stream)
            assert(native:close())
            assert(stream:setvbuf('full',8192))
            assert(not pcall(stream.write,stream,'prefix',{}))
            assert(stream:flush())
        "#,
        )
        .exec()
        .unwrap();
        let responses = collect(&mut manager);
        assert_eq!(bytes(&responses, ResponseKind::Stdout), b"prefix");
        assert_eq!(responses.last().unwrap().code, Some(0));
        lua.load(
            r#"
            assert(io.type(stream)=='closed file' and io.input()==stream)
            assert(not pcall(io.read))
            assert(not pcall(io.input,stream))
            assert(io.input(original_input)==original_input)
        "#,
        )
        .exec()
        .unwrap();
    }

    #[test]
    fn gc_and_explicit_close_cancel_term_resistant_groups() {
        let lua = Lua::new();
        let mut manager = Manager::default();
        let stream = spawn(&mut manager, "trap '' TERM; while :; do sleep 30; done");
        lua.globals()
            .set("stream", lua.create_userdata(stream).unwrap())
            .unwrap();
        lua.load("stream=nil; collectgarbage('collect'); collectgarbage('collect')")
            .exec()
            .unwrap();
        let started = Instant::now();
        let responses = collect(&mut manager);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(responses.last().unwrap().kind, ResponseKind::Signal);
        assert_eq!(responses.last().unwrap().code, Some(9));
    }

    #[test]
    fn runtime_drop_kills_group_even_after_leader_and_pipes_exit() {
        let mut manager = Manager::default();
        let _stream = spawn(
            &mut manager,
            "(trap '' TERM; exec 0<&- 1>&- 2>&-; sleep 30) & printf '%s' \"$!\"; exit 0",
        );
        let responses = collect(&mut manager);
        let pid: i32 = String::from_utf8(bytes(&responses, ResponseKind::Stdout))
            .unwrap()
            .parse()
            .unwrap();
        // A dead zombie can briefly remain until the OS init process reaps it;
        // it must never remain a running background process.
        let output = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&output.stdout);
        assert!(
            state.trim().is_empty() || state.trim_start().starts_with('Z'),
            "orphan still running: {state}"
        );

        let mut manager = Manager::default();
        let _stream = spawn(&mut manager, "trap '' TERM; while :; do sleep 30; done");
        let started = Instant::now();
        drop(manager);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn synchronous_runner_handles_duplex_closed_peers_and_sigint() {
        let directory = std::env::temp_dir();
        let input = vec![b'x'; 2 * 1024 * 1024];
        let flag = Arc::new(AtomicBool::new(false));
        let (status, output, errors) = run_pipe(
            "dd if=/dev/zero bs=1024 count=1024 2>/dev/null; cat",
            &input,
            Path::new("/bin/sh"),
            &directory,
            flag.clone(),
        )
        .unwrap();
        assert_eq!(status, 0);
        assert!(errors.is_empty());
        assert_eq!(&output[1024 * 1024..], &input);
        let (status, output, _) = run_pipe(
            "head -c 1",
            &input,
            Path::new("/bin/sh"),
            &directory,
            flag.clone(),
        )
        .unwrap();
        assert_eq!((status, output), (0, vec![b'x']));
        let trigger = flag.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            trigger.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let (status, _, _) = run_pipe(
            "trap '' TERM; while :; do sleep 30; done",
            b"",
            Path::new("/bin/sh"),
            &directory,
            flag,
        )
        .unwrap();
        thread.join().unwrap();
        assert_eq!(status, -1);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn spawn_failure_leaves_no_pending_child() {
        let mut manager = Manager::default();
        assert!(
            manager
                .spawn(
                    "bad",
                    "echo x",
                    Path::new("/nonexistent/vis-shell"),
                    &std::env::temp_dir()
                )
                .is_err()
        );
        assert!(!manager.pending());
    }

    #[test]
    fn leader_pid_remains_reserved_until_group_cleanup_and_final_reap() {
        use rustix::process::{Pid, WaitId, WaitIdOptions, WaitOptions, waitid, waitpid};
        let mut manager = Manager::default();
        let _stream = spawn(&mut manager, "exit 0");
        let pid = Pid::from_raw(manager.entries[0].child.id() as i32).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !manager.entries[0].exited {
            manager.entries[0].tick(false, &mut Vec::new());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        // Observation must leave the leader waitable, reserving its PID while
        // the manager still owns the group and can issue cleanup signals.
        assert!(
            waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT
            )
            .unwrap()
            .is_some()
        );
        let responses = collect(&mut manager);
        assert_eq!(responses.last().unwrap().code, Some(0));
        assert_eq!(
            waitpid(Some(pid), WaitOptions::NOHANG).unwrap_err(),
            rustix::io::Errno::CHILD
        );
    }

    #[test]
    fn observer_restores_foreground_before_the_leader_is_reaped() {
        use rustix::process::{Pid, WaitId, WaitIdOptions, WaitOptions, waitid, waitpid};
        let recorded = Cell::new(None);
        let events = RefCell::new(Vec::new());
        let observe = |group: Option<u32>| {
            events.borrow_mut().push(group);
            if let Some(group) = group {
                recorded.set(Some(group));
            } else {
                let pid = Pid::from_raw(recorded.get().unwrap() as i32).unwrap();
                assert!(
                    waitid(
                        WaitId::Pid(pid),
                        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT
                    )?
                    .is_some()
                );
            }
            Ok(())
        };
        let (status, output, errors) = run_pipe_with_observer(
            "printf done; exit 23",
            b"",
            Path::new("/bin/sh"),
            &std::env::temp_dir(),
            Arc::new(AtomicBool::new(false)),
            Some(&observe),
        )
        .unwrap();
        assert_eq!((status, output, errors), (23, b"done".to_vec(), Vec::new()));
        assert_eq!(&*events.borrow(), &[recorded.get(), None]);
        let pid = Pid::from_raw(recorded.get().unwrap() as i32).unwrap();
        assert_eq!(
            waitpid(Some(pid), WaitOptions::NOHANG).unwrap_err(),
            rustix::io::Errno::CHILD
        );
    }

    #[test]
    fn handoff_error_cancels_restores_and_reaps_the_owned_group() {
        use rustix::process::{Pid, WaitOptions, test_kill_process, waitpid};
        let recorded = Cell::new(None);
        let restored = Cell::new(false);
        let observe = |group: Option<u32>| {
            if let Some(group) = group {
                recorded.set(Some(group));
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "handoff failed",
                ));
            }
            let pid = Pid::from_raw(recorded.get().unwrap() as i32).unwrap();
            assert!(
                test_kill_process(pid).is_ok(),
                "leader PID must still be reserved during restoration"
            );
            restored.set(true);
            Ok(())
        };
        let started = Instant::now();
        let error = run_pipe_with_observer(
            "trap '' TERM; sleep 30",
            b"",
            Path::new("/bin/sh"),
            &std::env::temp_dir(),
            Arc::new(AtomicBool::new(false)),
            Some(&observe),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(restored.get());
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid = Pid::from_raw(recorded.get().unwrap() as i32).unwrap();
        assert_eq!(
            waitpid(Some(pid), WaitOptions::NOHANG).unwrap_err(),
            rustix::io::Errno::CHILD
        );
    }

    #[test]
    fn restoration_error_is_reported_after_reaping_without_a_second_restore() {
        use rustix::process::{Pid, WaitOptions, waitpid};
        let recorded = Cell::new(None);
        let restored = Cell::new(0);
        let observe = |group: Option<u32>| {
            if let Some(group) = group {
                recorded.set(Some(group));
                Ok(())
            } else {
                restored.set(restored.get() + 1);
                Err(io::Error::other("restore failed"))
            }
        };
        let error = run_pipe_with_observer(
            "exit 0",
            b"",
            Path::new("/bin/sh"),
            &std::env::temp_dir(),
            Arc::new(AtomicBool::new(false)),
            Some(&observe),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "restore failed");
        assert_eq!(restored.get(), 1);
        let pid = Pid::from_raw(recorded.get().unwrap() as i32).unwrap();
        assert_eq!(
            waitpid(Some(pid), WaitOptions::NOHANG).unwrap_err(),
            rustix::io::Errno::CHILD
        );
    }

    #[test]
    fn explicit_interrupt_request_persists_and_retains_terminal_events() {
        let mut manager = Manager::default();
        let flag = Arc::new(AtomicBool::new(false));
        manager.set_interrupt(flag.clone());
        let _stream = spawn(
            &mut manager,
            "trap '' TERM; printf ready; while :; do sleep 30; done",
        );
        let mut responses = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while bytes(&responses, ResponseKind::Stdout) != b"ready" {
            responses.extend(manager.poll());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        manager.request_cancel_all();
        assert!(manager.pending());
        assert!(!flag.load(Ordering::Relaxed));
        responses.extend(collect(&mut manager));
        assert_eq!(
            responses
                .iter()
                .filter(|response| response.code.is_some())
                .count(),
            1
        );
        assert_eq!(responses.last().unwrap().kind, ResponseKind::Signal);
        assert_eq!(responses.last().unwrap().code, Some(9));
    }
}
