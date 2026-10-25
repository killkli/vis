//! Interactive and batch entry points for the Rust port.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use vis::file::{Document, SaveMethod};
use vis::{sam, text::Text};

const USAGE: &str = "Usage: vis-rs [-R] [+COMMAND] [--] [FILE ...]
       vis-rs (--command SCRIPT | --batch SCRIPT_FILE) [--write] [--atomic] [--force] [--] [FILE]
       vis-rs --help | --version

Execute structural commands against FILE or standard input. Without --write,
write the resulting buffer to standard output. --write saves FILE after successful
execution; --atomic selects atomic replacement (does not copy ACLs/xattrs).
--force permits overwriting external file changes. Without a script, open the Unix
terminal editor. See rust/PORTING.md for the full migration status.";

enum Options {
    Batch(BatchOptions),
    Interactive {
        paths: Vec<PathBuf>,
        read_only: bool,
        commands: Vec<InitialCommand>,
    },
}

#[cfg_attr(not(all(unix, feature = "terminal")), allow(dead_code))]
struct InitialCommand {
    window: usize,
    command: String,
}

struct BatchOptions {
    script: String,
    file: Option<PathBuf>,
    write: bool,
    atomic: bool,
    force: bool,
}

#[cfg(all(unix, feature = "terminal"))]
struct Signals {
    terminate: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    interrupt: std::sync::Arc<std::sync::atomic::AtomicBool>,
    suspend: std::sync::Arc<std::sync::atomic::AtomicBool>,
    registrations: Vec<signal_hook::SigId>,
}

#[cfg(all(unix, feature = "terminal"))]
impl Signals {
    fn new() -> io::Result<Self> {
        use signal_hook::consts::signal::*;
        use std::sync::Arc;
        let mut signals = Self {
            terminate: Arc::default(),
            interrupt: Arc::default(),
            suspend: Arc::default(),
            registrations: Vec::new(),
        };
        for signal in [SIGTERM, SIGHUP] {
            signals
                .registrations
                .push(signal_hook::flag::register_usize(
                    signal,
                    Arc::clone(&signals.terminate),
                    signal as usize,
                )?);
            signals.registrations.push(signal_hook::flag::register(
                signal,
                Arc::clone(&signals.interrupt),
            )?);
        }
        signals.registrations.push(signal_hook::flag::register(
            SIGINT,
            Arc::clone(&signals.interrupt),
        )?);
        // Match vis: SIGQUIT is ignored; SIGINT cancels the active command.
        signals
            .registrations
            .push(signal_hook::flag::register(SIGQUIT, Arc::default())?);
        signals.registrations.push(signal_hook::flag::register(
            SIGTSTP,
            Arc::clone(&signals.suspend),
        )?);
        Ok(signals)
    }
}

#[cfg(all(unix, feature = "terminal"))]
impl Drop for Signals {
    fn drop(&mut self) {
        for registration in self.registrations.drain(..) {
            signal_hook::low_level::unregister(registration);
        }
    }
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Option<Options>, String> {
    let mut args = args.into_iter();
    let mut script = None;
    let mut paths = Vec::new();
    let mut read_only = false;
    let mut commands = Vec::new();
    let mut pending_command = None;
    let mut write = false;
    let mut atomic = false;
    let mut force = false;
    let mut positional = false;
    while let Some(arg) = args.next() {
        if !positional {
            if arg == "--help" || arg == "-h" {
                println!("{USAGE}");
                return Ok(None);
            }
            if arg == "--version" || arg == "-v" {
                println!(
                    "vis-rs {} (Rust port in progress; {})",
                    env!("CARGO_PKG_VERSION"),
                    if cfg!(feature = "lua") {
                        "+lua"
                    } else {
                        "-lua"
                    }
                );
                return Ok(None);
            }
            if arg == "--" {
                positional = true;
                continue;
            }
            if arg == "-R" || arg == "--readonly" {
                read_only = true;
                continue;
            }
            if let Some(command) = arg.to_str().and_then(|arg| arg.strip_prefix('+')) {
                pending_command = Some(command.to_owned());
                continue;
            }
            if arg == "--write" {
                write = true;
                continue;
            }
            if arg == "--atomic" {
                atomic = true;
                continue;
            }
            if arg == "--force" {
                force = true;
                continue;
            }
            if arg == "--command" || arg == "--batch" {
                if script.is_some() {
                    return Err("specify one script source".into());
                }
                let value = args.next().ok_or("missing script argument")?;
                script = Some(if arg == "--batch" {
                    std::fs::read_to_string(&value)
                        .map_err(|e| format!("cannot read script: {e}"))?
                } else {
                    value.into_string().map_err(|_| "script must be UTF-8")?
                });
                continue;
            }
            if arg != "-" && arg.to_string_lossy().starts_with('-') {
                return Err(format!("unknown option: {}", arg.to_string_lossy()));
            }
        }
        if let Some(command) = pending_command.take() {
            commands.push(InitialCommand {
                window: paths.len(),
                command,
            });
        }
        paths.push(if positional && arg == "-" {
            PathBuf::from("./-")
        } else {
            PathBuf::from(arg)
        });
    }
    let Some(script) = script else {
        if write || atomic || force {
            return Err("--write, --atomic and --force require a batch script".into());
        }
        if paths.is_empty() {
            if let Some(command) = pending_command {
                commands.push(InitialCommand { window: 0, command });
            }
        }
        return Ok(Some(Options::Interactive {
            paths,
            read_only,
            commands,
        }));
    };
    if paths.len() > 1 {
        return Err("batch mode accepts one input file".into());
    }
    if read_only || !commands.is_empty() || pending_command.is_some() {
        return Err("-R and +COMMAND are interactive options".into());
    }
    let file = paths.pop().filter(|path| path.as_os_str() != "-");
    if write && file.is_none() {
        return Err("--write requires FILE".into());
    }
    if !write && (atomic || force) {
        return Err("--atomic and --force require --write".into());
    }
    Ok(Some(Options::Batch(BatchOptions {
        script,
        file,
        write,
        atomic,
        force,
    })))
}

fn run_batch(options: BatchOptions) -> Result<(), Box<dyn std::error::Error>> {
    let mut document = options.file.as_ref().map(Document::open).transpose()?;
    let mut stdin_text;
    let text = if let Some(document) = &mut document {
        &mut document.text
    } else {
        let mut bytes = Vec::new();
        io::stdin().read_to_end(&mut bytes)?;
        stdin_text = Text::from_bytes(bytes);
        &mut stdin_text
    };
    let length = text.len();
    sam::execute(text, &options.script, 0..length)?;
    if options.write {
        let method = if options.atomic {
            SaveMethod::Atomic
        } else {
            SaveMethod::Auto
        };
        document
            .as_mut()
            .expect("validated file option")
            .save(method, options.force)?;
    } else {
        let mut stdout = io::stdout().lock();
        for chunk in text.chunks() {
            stdout.write_all(chunk)?;
        }
        stdout.flush()?;
    }
    Ok(())
}

#[cfg(all(unix, feature = "terminal"))]
type HostResult<T> = Result<T, Box<dyn std::error::Error>>;

#[cfg(all(unix, feature = "terminal"))]
type HostGroupObserver = std::sync::Arc<dyn Fn(Option<u32>) -> io::Result<()> + Send + Sync>;

/// The terminal loop uses one host interface. Optional Lua runs between short
/// workspace borrows; no Ref/RefMut survives a call into user configuration.
#[cfg(all(unix, feature = "terminal"))]
struct InteractiveHost {
    workspace: std::rc::Rc<std::cell::RefCell<vis::workspace::Workspace>>,
    #[cfg(feature = "lua")]
    runtime: vis::lua::Runtime,
}

#[cfg(all(unix, feature = "terminal"))]
struct InteractiveTerminal {
    slot: std::rc::Rc<std::cell::RefCell<Option<vis::terminal::Terminal>>>,
}

/// Job-control operations temporarily block SIGTTOU only on this thread. The
/// guard cannot move to a different thread before restoring its original mask.
#[cfg(all(unix, feature = "terminal"))]
struct BlockedTtou {
    previous: nix::sys::signal::SigSet,
    thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(all(unix, feature = "terminal"))]
impl BlockedTtou {
    fn new() -> io::Result<Self> {
        use nix::sys::signal::{SigSet, SigmaskHow, Signal};
        let mut signals = SigSet::empty();
        signals.add(Signal::SIGTTOU);
        let previous = signals.thread_swap_mask(SigmaskHow::SIG_BLOCK)?;
        Ok(Self {
            previous,
            thread: std::marker::PhantomData,
        })
    }
}

#[cfg(all(unix, feature = "terminal"))]
impl Drop for BlockedTtou {
    fn drop(&mut self) {
        let _ = self.previous.thread_set_mask();
    }
}

#[cfg(all(unix, feature = "terminal"))]
struct JobControlTerminal {
    tty: std::fs::File,
    baseline: rustix::termios::Termios,
    state: std::sync::Mutex<JobControlState>,
}

#[cfg(all(unix, feature = "terminal"))]
#[derive(Default)]
struct JobControlState {
    previous: Option<rustix::process::Pid>,
    attributes: Option<rustix::termios::Termios>,
}

#[cfg(all(unix, feature = "terminal"))]
impl JobControlTerminal {
    fn open() -> io::Result<Option<Self>> {
        use std::os::unix::fs::OpenOptionsExt;
        let tty = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(rustix::fs::OFlags::NOCTTY.bits() as _)
            .open("/dev/tty")
        {
            Ok(tty) => tty,
            Err(error) if matches!(error.raw_os_error(), Some(code) if code == rustix::io::Errno::NXIO.raw_os_error() || code == rustix::io::Errno::NODEV.raw_os_error() || code == rustix::io::Errno::NOTTY.raw_os_error()) =>
            {
                return Ok(None);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        match rustix::termios::tcgetpgrp(&tty) {
            Ok(_) => {
                let baseline = rustix::termios::tcgetattr(&tty)?;
                Ok(Some(Self {
                    tty,
                    baseline,
                    state: std::sync::Mutex::default(),
                }))
            }
            Err(rustix::io::Errno::NOTTY) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn transfer(&self, group: Option<u32>) -> io::Result<()> {
        let _blocked = BlockedTtou::new()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("terminal job-control lock poisoned"))?;
        if let Some(group) = group {
            let group = i32::try_from(group)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid child process group")
                })?;
            let previous = rustix::termios::tcgetpgrp(&self.tty)?;
            let attributes = rustix::termios::tcgetattr(&self.tty)?;
            // Retain restoration state before either terminal mutation, so the
            // observer's error cleanup and Drop can retry a partial handoff.
            state.previous = Some(previous);
            state.attributes = Some(attributes.clone());
            // Native termkey_stop restores the pre-editor terminal for every
            // synchronous pipe, including overlays that retain the alternate
            // screen. Terminal applications may then enter their own raw mode.
            rustix::termios::tcsetattr(
                &self.tty,
                rustix::termios::OptionalActions::Now,
                &self.baseline,
            )?;
            if let Err(error) = rustix::termios::tcsetpgrp(&self.tty, group) {
                let _ = rustix::termios::tcsetattr(
                    &self.tty,
                    rustix::termios::OptionalActions::Now,
                    &attributes,
                );
                return Err(error.into());
            }
        } else if let Some(previous) = state.previous {
            rustix::termios::tcsetpgrp(&self.tty, previous)?;
            if let Some(attributes) = state.attributes.as_ref() {
                rustix::termios::tcsetattr(
                    &self.tty,
                    rustix::termios::OptionalActions::Now,
                    attributes,
                )?;
            }
            state.attributes.take();
            state.previous = None;
        }
        Ok(())
    }
}

#[cfg(all(unix, feature = "terminal"))]
impl Drop for JobControlTerminal {
    fn drop(&mut self) {
        let _ = self.transfer(None);
    }
}

#[cfg(all(unix, feature = "terminal"))]
impl InteractiveTerminal {
    fn new() -> Self {
        Self {
            slot: std::rc::Rc::default(),
        }
    }
    #[cfg(feature = "lua")]
    fn process_observer(&self) -> vis::lua::ProcessObserver {
        let slot = std::rc::Rc::clone(&self.slot);
        let resume = std::cell::Cell::new(false);
        std::rc::Rc::new(move |before| {
            if before {
                let mut slot = slot.borrow_mut();
                resume.set(slot.is_some());
                if let Some(terminal) = slot.as_mut() {
                    terminal.restore()?;
                }
            } else if resume.replace(false) {
                if let Some(terminal) = slot.borrow_mut().as_mut() {
                    terminal.resume()?;
                }
            }
            Ok(())
        })
    }
    fn initialize(&self) -> io::Result<()> {
        let terminal = vis::terminal::Terminal::new()?;
        *self.slot.borrow_mut() = Some(terminal);
        Ok(())
    }
    fn with<T>(
        &self,
        operation: impl FnOnce(&mut vis::terminal::Terminal) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut slot = self.slot.borrow_mut();
        let terminal = slot
            .as_mut()
            .ok_or_else(|| io::Error::other("terminal is suspended"))?;
        operation(terminal)
    }
    fn restore(&self) -> io::Result<()> {
        let terminal = self.slot.borrow_mut().take();
        if let Some(mut terminal) = terminal {
            terminal.restore()?;
        }
        Ok(())
    }
    fn suspend(&self) -> io::Result<()> {
        self.with(vis::terminal::Terminal::restore)
    }
    fn resume(&self) -> io::Result<()> {
        self.with(vis::terminal::Terminal::resume)
    }
}

#[cfg(all(unix, feature = "terminal"))]
impl Drop for InteractiveTerminal {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(all(unix, feature = "terminal"))]
impl InteractiveHost {
    fn new(
        interrupt: std::sync::Arc<std::sync::atomic::AtomicBool>,
        terminal: &InteractiveTerminal,
    ) -> HostResult<Self> {
        let job_control = std::sync::Arc::new(JobControlTerminal::open()?);
        let group_job_control = std::sync::Arc::clone(&job_control);
        let groups: HostGroupObserver = std::sync::Arc::new(move |group| {
            if let Some(job_control) = group_job_control.as_ref() {
                job_control.transfer(group)?;
            }
            Ok(())
        });
        #[cfg(feature = "lua")]
        {
            let observer_job_control = std::sync::Arc::clone(&job_control);
            let screen_observer = terminal.process_observer();
            let before_after: vis::lua::ProcessObserver = std::rc::Rc::new(move |before| {
                if !before {
                    // Foreground ownership must return before raw mode or any
                    // editor output, including after a child/observer failure.
                    if let Some(job_control) = observer_job_control.as_ref() {
                        job_control.transfer(None)?;
                    }
                }
                screen_observer(before)
            });
            let runtime =
                vis::lua::Runtime::new_with_observers(&[], interrupt, before_after, groups)?;
            Ok(Self {
                workspace: runtime.workspace(),
                runtime,
            })
        }
        #[cfg(not(feature = "lua"))]
        {
            let _ = terminal;
            let mut workspace = vis::workspace::Workspace::empty()?;
            workspace.editor.set_interrupt(interrupt);
            workspace.editor.set_process_group_observer(groups);
            Ok(Self {
                workspace: std::rc::Rc::new(std::cell::RefCell::new(workspace)),
            })
        }
    }

    fn open_path(
        &self,
        path: &std::path::Path,
        input: Option<Vec<u8>>,
        read_only: bool,
    ) -> HostResult<vis::workspace::Outcome> {
        #[cfg(feature = "lua")]
        {
            Ok(self.runtime.open_cli_path(path, input, read_only)?)
        }
        #[cfg(not(feature = "lua"))]
        {
            self.workspace
                .borrow_mut()
                .open_cli_path(path, input, read_only)?;
            Ok(vis::workspace::Outcome::Continue)
        }
    }

    fn open_scratch(
        &self,
        input: Option<Vec<u8>>,
        read_only: bool,
    ) -> HostResult<vis::workspace::Outcome> {
        #[cfg(feature = "lua")]
        {
            Ok(self.runtime.open_scratch(input, read_only)?)
        }
        #[cfg(not(feature = "lua"))]
        {
            self.workspace.borrow_mut().open_scratch(input, read_only)?;
            Ok(vis::workspace::Outcome::Continue)
        }
    }

    fn handle_key(&self, key: vis::editor::Key) -> HostResult<vis::workspace::Outcome> {
        #[cfg(feature = "lua")]
        {
            Ok(self.runtime.handle_key(key)?)
        }
        #[cfg(not(feature = "lua"))]
        {
            Ok(self.workspace.borrow_mut().handle_key(key)?)
        }
    }

    fn paste(&self, bytes: &[u8]) -> HostResult<vis::workspace::Outcome> {
        #[cfg(feature = "lua")]
        {
            Ok(self.runtime.paste(bytes)?)
        }
        #[cfg(not(feature = "lua"))]
        {
            Ok(self.workspace.borrow_mut().paste(bytes)?)
        }
    }

    fn initial_command(&self, command: &str) -> HostResult<vis::workspace::Outcome> {
        #[cfg(feature = "lua")]
        {
            if command.is_empty() {
                return Ok(vis::workspace::Outcome::Continue);
            }
            if command.starts_with('/') || command.starts_with('?') {
                for character in command.chars() {
                    self.runtime.handle_key(vis::editor::Key::Char(character))?;
                }
                return Ok(self.runtime.handle_key(vis::editor::Key::Enter)?);
            }
            if !self
                .runtime
                .command(command.strip_prefix(':').unwrap_or(command))?
            {
                return Err(format!("startup command failed: {command}").into());
            }
            Ok(vis::workspace::Outcome::Continue)
        }
        #[cfg(not(feature = "lua"))]
        {
            Ok(self.workspace.borrow_mut().initial_command(command)?)
        }
    }

    fn start(&self) -> HostResult<vis::workspace::Outcome> {
        #[cfg(feature = "lua")]
        self.runtime.start()?;
        Ok(vis::workspace::Outcome::Continue)
    }

    fn shutdown(&self) -> HostResult<()> {
        #[cfg(feature = "lua")]
        self.runtime.shutdown()?;
        Ok(())
    }

    fn exit_code(&self) -> Option<i32> {
        #[cfg(feature = "lua")]
        {
            self.runtime.exit_code()
        }
        #[cfg(not(feature = "lua"))]
        {
            None
        }
    }

    /// Notifications and explicit quit requests can be raised by any callback,
    /// including one nested inside an open/save plan or drawing event.
    fn report(&self, result: HostResult<vis::workspace::Outcome>, message: &mut String) -> bool {
        use vis::workspace::Outcome;
        let quit = match result {
            Ok(Outcome::Quit) => true,
            Ok(Outcome::Message(text)) => {
                *message = text;
                false
            }
            Ok(Outcome::Continue | Outcome::Suspend) => false,
            Err(error) => {
                *message = error.to_string();
                false
            }
        };
        #[cfg(feature = "lua")]
        let quit = {
            for text in self.runtime.take_messages() {
                *message = text;
            }
            match self.runtime.take_outcome() {
                Outcome::Quit => true,
                Outcome::Message(text) => {
                    *message = text;
                    quit
                }
                Outcome::Continue | Outcome::Suspend => quit,
            }
        };
        quit || self.exit_code().is_some()
    }

    fn snapshot(&self) -> Option<(Vec<vis::workspace::Frame>, vis::terminal::Layout, usize)> {
        let workspace = self.workspace.borrow();
        if workspace.window_count() == 0 {
            return None;
        }
        let layout = match workspace.layout {
            vis::workspace::Layout::Horizontal => vis::terminal::Layout::Horizontal,
            vis::workspace::Layout::Vertical => vis::terminal::Layout::Vertical,
        };
        Some((workspace.frames(), layout, workspace.active_window_id()))
    }

    #[cfg(feature = "lua")]
    fn poll_processes(&self, message: &mut String) -> Option<bool> {
        self.runtime.has_processes().then(|| {
            self.report(
                self.runtime
                    .poll_processes()
                    .map(|()| vis::workspace::Outcome::Continue)
                    .map_err(Into::into),
                message,
            )
        })
    }

    fn draw(
        &self,
        terminal: &InteractiveTerminal,
        message: &str,
    ) -> HostResult<(bool, Option<String>)> {
        #[cfg(feature = "lua")]
        {
            let mut message = message.to_owned();
            let quit = self.draw_lua(terminal, &mut message)?;
            Ok((quit, Some(message)))
        }
        #[cfg(not(feature = "lua"))]
        {
            Ok((self.render_current(terminal, message)?, None))
        }
    }

    #[cfg(feature = "lua")]
    fn draw_lua(&self, terminal: &InteractiveTerminal, message: &mut String) -> HostResult<bool> {
        let Some((frames, layout, active)) = self.snapshot() else {
            return Ok(true);
        };
        let views = render_views(&frames);
        let geometry = terminal.with(|terminal| {
            terminal.measure_views_with_message(&views, active, layout, message)
        })?;
        self.publish_geometry(&geometry);
        for geometry in geometry {
            self.runtime.set_viewport(
                geometry.id,
                geometry.width,
                geometry.height,
                geometry.total_height,
                geometry.bytes,
                geometry.lines,
            );
        }
        if self.report(
            self.runtime
                .redraw()
                .map(|()| vis::workspace::Outcome::Continue)
                .map_err(Into::into),
            message,
        ) {
            return Ok(true);
        }
        self.render_current(terminal, message)
    }

    fn render_current(&self, terminal: &InteractiveTerminal, message: &str) -> HostResult<bool> {
        // Lua redraw callbacks may edit a buffer, focus or close a window.
        // Rebuild borrowed descriptors from a fresh owned frame snapshot.
        let Some((frames, layout, active)) = self.snapshot() else {
            return Ok(true);
        };
        let views = render_views(&frames);
        #[cfg(feature = "lua")]
        let geometry = {
            let decorations = self.runtime.render_decorations();
            terminal.with(|terminal| {
                terminal.render_views_styled(&views, active, layout, message, &decorations)
            })?
        };
        #[cfg(not(feature = "lua"))]
        let geometry =
            terminal.with(|terminal| terminal.render_views(&views, active, layout, message))?;
        self.publish_geometry(&geometry);
        Ok(false)
    }

    /// Hand the body geometry of every drawn window to its viewport motions.
    fn publish_geometry(&self, geometry: &[vis::terminal::ViewGeometry]) {
        let mut workspace = self.workspace.borrow_mut();
        for item in geometry {
            workspace.set_window_viewport(
                item.id,
                vis::view::ViewportMetrics {
                    width: item.width,
                    height: item.height,
                    top: item.top,
                },
            );
        }
    }
}

#[cfg(all(unix, feature = "terminal"))]
fn render_views(frames: &[vis::workspace::Frame]) -> Vec<vis::terminal::RenderView<'_>> {
    frames
        .iter()
        .map(|frame| vis::terminal::RenderView {
            id: frame.id,
            data: frame.bytes.as_slice(),
            cursor: frame.selections[frame.primary].cursor,
            selections: &frame.selections,
            anchored: &frame.anchored,
            selections_visible: frame.selections_visible,
            primary_index: frame.primary,
            mode: frame.mode,
            path: &frame.path,
            modified: frame.modified,
            prompt: None,
            display: frame.display,
            prompt_executing: frame.prompt_executing,
            tab_width: frame.tab_width,
        })
        .collect()
}

#[cfg(all(unix, feature = "terminal"))]
fn run_interactive(
    paths: Vec<PathBuf>,
    read_only: bool,
    commands: Vec<InitialCommand>,
) -> HostResult<i32> {
    // Runtime initialization/configuration and INIT happen before any CLI file
    // or stdin buffer is opened. The default host has no configuration callbacks.
    let signals = Signals::new()?;
    let terminal = InteractiveTerminal::new();
    let host = InteractiveHost::new(std::sync::Arc::clone(&signals.interrupt), &terminal)?;
    let result = run_interactive_session(&host, &terminal, &signals, paths, read_only, commands);
    let restored = terminal.restore();
    // QUIT runs after releasing the raw terminal, including on ordinary errors.
    // A Lua callback cannot keep a terminal guard alive through its observer Rc.
    if let Err(error) = host.shutdown() {
        eprintln!("vis-rs: shutdown: {error}");
    }
    let code = result?;
    restored?;
    Ok(host.exit_code().unwrap_or(code))
}

#[cfg(all(unix, feature = "terminal"))]
fn read_startup_input(signals: &Signals) -> io::Result<Vec<u8>> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use std::sync::atomic::Ordering;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut bytes = Vec::new();
    loop {
        let signal = signals.terminate.load(Ordering::Relaxed);
        if signal != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                format!("terminated by signal {signal}"),
            ));
        }
        if signals.interrupt.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "interrupted while reading stdin",
            ));
        }
        let mut descriptors = [PollFd::new(&stdin, PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: 0,
            tv_nsec: 100_000_000,
        };
        match poll(&mut descriptors, Some(&timeout)) {
            Ok(0) | Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        if descriptors[0]
            .revents()
            .intersects(PollFlags::ERR | PollFlags::NVAL)
        {
            return Err(io::Error::other("cannot read stdin"));
        }
        let mut chunk = [0u8; 8192];
        match input.read(&mut chunk) {
            Ok(0) => return Ok(bytes),
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(all(unix, feature = "terminal"))]
fn run_interactive_session(
    host: &InteractiveHost,
    terminal: &InteractiveTerminal,
    signals: &Signals,
    paths: Vec<PathBuf>,
    read_only: bool,
    commands: Vec<InitialCommand>,
) -> HostResult<i32> {
    use std::io::IsTerminal;
    use std::sync::atomic::Ordering;
    use vis::terminal::InputEvent;
    use vis::workspace::Outcome;

    if signals.terminate.load(Ordering::Relaxed) != 0 {
        return Err("terminated during initialization".into());
    }
    let mut message = if read_only {
        "Read-only; use :w! to override".to_owned()
    } else {
        String::new()
    };
    if host.report(Ok(Outcome::Continue), &mut message) {
        return Ok(host.exit_code().unwrap_or(0));
    }
    let need_scratch = paths.is_empty() && host.workspace.borrow().window_count() == 0;
    let mut input = if paths.iter().any(|path| path.as_os_str() == "-")
        || (need_scratch && !io::stdin().is_terminal())
    {
        Some(read_startup_input(signals)?)
    } else {
        None
    };

    for (index, path) in paths
        .iter()
        .map(Some)
        .chain(paths.is_empty().then_some(None))
        .enumerate()
    {
        let opened = if let Some(path) = path {
            let data = if path.as_os_str() == "-" {
                input.take()
            } else {
                None
            };
            host.open_path(path, data, read_only)?
        } else if need_scratch {
            host.open_scratch(input.take(), read_only)?
        } else {
            Outcome::Continue
        };
        if host.report(Ok(opened), &mut message) {
            return Ok(host.exit_code().unwrap_or(0));
        }
        for initial in commands.iter().filter(|command| command.window == index) {
            if host.report(host.initial_command(&initial.command), &mut message) {
                return Ok(host.exit_code().unwrap_or(0));
            }
            if signals.terminate.load(Ordering::Relaxed) != 0 {
                return Err("terminated during startup".into());
            }
            if signals.interrupt.swap(false, Ordering::Relaxed) {
                break;
            }
        }
    }
    // START observes all argument files and their +COMMAND effects. It may exit
    // without a controlling terminal, as the original Lua test runner requires.
    if host.report(host.start(), &mut message) {
        return Ok(host.exit_code().unwrap_or(0));
    }
    if host.workspace.borrow().window_count() == 0 {
        return Ok(host.exit_code().unwrap_or(0));
    }
    terminal.initialize()?;
    loop {
        let (quit, updated) = host.draw(terminal, &message)?;
        if let Some(updated) = updated {
            message = updated;
        }
        if quit {
            break;
        }
        let event = loop {
            let signal = signals.terminate.load(Ordering::Relaxed);
            if signal != 0 {
                return Err(format!("terminated by signal {signal}").into());
            }
            if signals.interrupt.swap(false, Ordering::Relaxed) {
                terminal.with(|terminal| {
                    terminal.cancel_input();
                    Ok(())
                })?;
                break InputEvent::Key(vis::editor::Key::Ctrl('c'));
            }
            if signals.suspend.swap(false, Ordering::Relaxed) {
                // An external SIGTSTP stopped the job; restore the terminal
                // attributes and stop again so the shell sees a clean state,
                // then redraw after continuing.
                terminal.suspend()?;
                signal_hook::low_level::raise(signal_hook::consts::signal::SIGSTOP)?;
                terminal.resume()?;
                break InputEvent::Timeout;
            }
            let event = match terminal.with(vis::terminal::Terminal::read_event) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if !matches!(event, InputEvent::Timeout) {
                break event;
            }
            #[cfg(feature = "lua")]
            if let Some(quit) = host.poll_processes(&mut message) {
                if quit {
                    return Ok(host.exit_code().unwrap_or(0));
                }
                // Process callbacks can mutate text, styles, focus or messages
                // even while the user is not entering terminal input.
                break InputEvent::Timeout;
            }
        };
        let outcome = match event {
            InputEvent::Message(text) => Ok(Outcome::Message(text)),
            InputEvent::Key(key) => host.handle_key(key),
            InputEvent::Paste(bytes) => host.paste(&bytes),
            InputEvent::Resize => continue,
            InputEvent::Eof => {
                if host.workspace.borrow().modified() {
                    return Err("terminal closed with unsaved changes".into());
                }
                break;
            }
            InputEvent::Timeout => continue,
        };
        if matches!(outcome, Ok(Outcome::Suspend)) {
            terminal.suspend()?;
            signal_hook::low_level::raise(signal_hook::consts::signal::SIGSTOP)?;
            terminal.resume()?;
            continue;
        }
        message.clear();
        if host.report(outcome, &mut message) {
            break;
        }
    }
    Ok(host.exit_code().unwrap_or(0))
}

#[cfg(not(all(unix, feature = "terminal")))]
fn run_interactive(
    _paths: Vec<PathBuf>,
    _read_only: bool,
    _commands: Vec<InitialCommand>,
) -> Result<i32, Box<dyn std::error::Error>> {
    Err("interactive editing requires Unix and the terminal Cargo feature; batch editing remains available".into())
}

fn run(options: Options) -> Result<i32, Box<dyn std::error::Error>> {
    match options {
        Options::Batch(options) => run_batch(options).map(|()| 0),
        Options::Interactive {
            paths,
            read_only,
            commands,
        } => run_interactive(paths, read_only, commands),
    }
}

fn main() -> std::process::ExitCode {
    match parse(std::env::args_os().skip(1)) {
        Ok(Some(options)) => match run(options) {
            Ok(code) => std::process::ExitCode::from(code as u8),
            Err(err) => {
                eprintln!("vis-rs: {err}");
                std::process::ExitCode::FAILURE
            }
        },
        Ok(None) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("vis-rs: {err}\nUse --help for usage.");
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options(args: &[&str]) -> Options {
        parse(args.iter().map(OsString::from)).unwrap().unwrap()
    }

    #[test]
    fn startup_commands_follow_the_next_file_and_last_pending_command_wins() {
        let Options::Interactive {
            paths, commands, ..
        } = options(&["+first", "a", "+discard", "+second", "b", "+ignored"])
        else {
            panic!("interactive options")
        };
        assert_eq!(paths, [PathBuf::from("a"), PathBuf::from("b")]);
        assert_eq!(
            commands
                .iter()
                .map(|command| (command.window, command.command.as_str()))
                .collect::<Vec<_>>(),
            [(0, "first"), (1, "second")]
        );
        let Options::Interactive { commands, .. } = options(&["+pwd"]) else {
            panic!("interactive options")
        };
        assert_eq!(
            (commands[0].window, commands[0].command.as_str()),
            (0, "pwd")
        );
    }

    #[test]
    fn end_of_options_keeps_dash_and_plus_names_as_files() {
        let Options::Interactive {
            paths, commands, ..
        } = options(&["--", "-", "+literal"])
        else {
            panic!("interactive options")
        };
        assert_eq!(paths[0].as_os_str(), "./-");
        assert_eq!(paths[1].as_os_str(), "+literal");
        assert!(commands.is_empty());
        let Options::Batch(batch) = options(&["--command", ",p", "--", "-"]) else {
            panic!("batch options")
        };
        assert_eq!(batch.file.unwrap().as_os_str(), "./-");
        let Options::Batch(batch) = options(&["--command", ",p", "-"]) else {
            panic!("batch options")
        };
        assert!(batch.file.is_none());
    }
}
