//! Unicode digraph companion: UTF-8 output, preserving the C CLI status codes.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;
use vis::digraph::{DIGRAPHS, Lookup, MAX_DIGRAPH_LENGTH, lookup};

fn unknown(name: &[u8]) {
    let end = name
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(name.len());
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(b"Unknown digraph: `");
    let _ = stderr.write_all(&name[..end]);
    let _ = stderr.write_all(b"'\n");
}

fn write_character(output: &mut impl Write, character: char) -> io::Result<()> {
    let mut bytes = [0; 4];
    output.write_all(character.encode_utf8(&mut bytes).as_bytes())
}

fn from_stdin(output: &mut impl Write) -> io::Result<u8> {
    let stdin = io::stdin();
    let raw = RawInput::new(&stdin)?;
    let mut input = input_reader(&stdin)?;
    let mut name = [0; MAX_DIGRAPH_LENGTH];
    for length in 1..=name.len() {
        let count = loop {
            match input.read(&mut name[length - 1..length]) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => break result?,
            }
        };
        // On EOF the zero-filled byte remains, just as in the C buffer.
        match lookup(&name[..length]) {
            Lookup::Found(character) => {
                drop(raw);
                write_character(output, character)?;
                return Ok(0);
            }
            Lookup::Unknown => {
                drop(raw);
                unknown(&name[..length]);
                return Ok(1);
            }
            Lookup::Prefix if count == 0 => {
                drop(raw);
                unknown(&name[..length]);
                return Ok(1);
            }
            Lookup::Prefix => {}
        }
    }
    Ok(1)
}

// Duplicate the descriptor safely and read the file directly: StdinLock's
// buffering could otherwise consume bytes after the recognized mnemonic.
#[cfg(unix)]
fn input_reader(stdin: &io::Stdin) -> io::Result<std::fs::File> {
    use std::os::fd::AsFd;
    Ok(std::fs::File::from(stdin.as_fd().try_clone_to_owned()?))
}

#[cfg(not(unix))]
fn input_reader(stdin: &io::Stdin) -> io::Result<io::StdinLock<'_>> {
    Ok(stdin.lock())
}

fn run(arguments: &[OsString], output: &mut impl Write) -> io::Result<u8> {
    if arguments.is_empty() {
        for entry in DIGRAPHS {
            output.write_all(entry.name.as_bytes())?;
            output.write_all(b" ")?;
            write_character(output, entry.character)?;
            output.write_all(b" ")?;
            output.write_all(entry.description.as_bytes())?;
            output.write_all(b"\n")?;
        }
        return Ok(0);
    }
    if arguments[0] == "-" {
        return from_stdin(output);
    }
    // Validate all arguments before emitting anything. A later invalid name or
    // prefix must not leave an apparently successful partial conversion.
    let mut characters = Vec::with_capacity(arguments.len());
    for argument in arguments {
        match lookup(argument.as_encoded_bytes()) {
            Lookup::Found(character) => characters.push(character),
            Lookup::Prefix => return Ok(2),
            Lookup::Unknown => {
                unknown(argument.as_encoded_bytes());
                return Ok(1);
            }
        }
    }
    for character in characters {
        write_character(output, character)?;
    }
    Ok(0)
}

#[cfg(all(unix, feature = "terminal"))]
struct RawInput(Option<rustix::termios::Termios>);

#[cfg(all(unix, feature = "terminal"))]
impl RawInput {
    fn new(stdin: &io::Stdin) -> io::Result<Self> {
        if !stdin.is_terminal() {
            return Ok(Self(None));
        }
        let original = rustix::termios::tcgetattr(stdin)?;
        let mut raw = original.clone();
        raw.make_raw();
        rustix::termios::tcsetattr(stdin, rustix::termios::OptionalActions::Now, &raw)?;
        Ok(Self(Some(original)))
    }
}

#[cfg(all(unix, feature = "terminal"))]
impl Drop for RawInput {
    fn drop(&mut self) {
        if let Some(original) = &self.0 {
            let _ = rustix::termios::tcsetattr(
                io::stdin(),
                rustix::termios::OptionalActions::Now,
                original,
            );
        }
    }
}

#[cfg(not(all(unix, feature = "terminal")))]
struct RawInput;

#[cfg(not(all(unix, feature = "terminal")))]
impl RawInput {
    fn new(stdin: &io::Stdin) -> io::Result<Self> {
        if stdin.is_terminal() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "interactive digraph input requires the Unix terminal feature",
            ));
        }
        Ok(Self)
    }
}

#[cfg(not(all(unix, feature = "terminal")))]
impl Drop for RawInput {
    fn drop(&mut self) {}
}

fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let mut stdout = io::stdout().lock();
    match run(&arguments, &mut stdout).and_then(|status| {
        stdout.flush()?;
        Ok(status)
    }) {
        Ok(status) => ExitCode::from(status),
        Err(_) => {
            let _ = io::stderr().write_all(b"I/O error");
            ExitCode::from(3)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_preserve_unicode_nul_and_validation_atomicity() {
        let mut output = Vec::new();
        assert_eq!(
            run(&["a:".into(), "NU".into(), "SP".into()], &mut output).unwrap(),
            0
        );
        assert_eq!(output, "ä\0 ".as_bytes());
        output.clear();
        assert_eq!(run(&["SP".into(), "a".into()], &mut output).unwrap(), 2);
        assert!(output.is_empty());
    }

    #[test]
    fn listing_contains_every_record_in_order() {
        let mut output = Vec::new();
        assert_eq!(run(&[], &mut output).unwrap(), 0);
        assert!(output.starts_with(b"SP   SPACE\n"));
        let expected = DIGRAPHS
            .iter()
            .map(|entry| format!("{} {} {}\n", entry.name, entry.character, entry.description))
            .collect::<String>();
        assert_eq!(output, expected.as_bytes());
    }

    #[test]
    fn write_failures_propagate() {
        struct FailingWriter;
        impl Write for FailingWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("failed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(run(&["SP".into()], &mut FailingWriter).is_err());
    }
}
