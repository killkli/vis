//! Command-level chronological traversal, separate from undo-tree traversal.
//!
//! The plain argument count intentionally retains `cmd_earlier_later`'s
//! shadowed-local behavior: it is validated, but traversal takes one step.
use crate::text::Text;
use std::fmt;

#[derive(Debug)]
pub(crate) enum HistoryError {
    Arguments(std::io::Error),
    InvalidNumber(String),
    UnknownUnit,
    OutOfRange,
}

impl HistoryError {
    pub(crate) fn is_parse(&self) -> bool {
        matches!(self, Self::Arguments(_))
    }
}

impl fmt::Display for HistoryError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arguments(error) => error.fmt(output),
            Self::InvalidNumber(argument) => write!(output, "Invalid number: {argument}"),
            Self::UnknownUnit => output.write_str("Unknown time specifier (use: s,m,h or d)"),
            Self::OutOfRange => output.write_str("History timestamp is out of range"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    Revision,
    Seconds(i64),
    // A quoted argument ending in whitespace first restores, then steps: the
    // native handler tests the trimmed suffix after its restore call.
    SecondsThenRevision(i64),
}

fn request(words: &[String]) -> Result<Request, HistoryError> {
    let Some(argument) = words.first() else {
        return Ok(Request::Revision);
    };
    let bytes = argument.as_bytes();
    let negative = bytes.first() == Some(&b'-');
    let mut index = usize::from(negative);
    let hexadecimal = if bytes.len().saturating_sub(index) > 2
        && bytes.get(index) == Some(&b'0')
        && matches!(bytes.get(1), Some(b'x' | b'X'))
    {
        index += 2;
        true
    } else if bytes.len().saturating_sub(index) > 1 && bytes.get(index) == Some(&b'#') {
        index += 1;
        true
    } else {
        false
    };
    let radix = if hexadecimal { 16 } else { 10 };
    let mut magnitude = 0u64;
    while let Some(digit) = bytes
        .get(index)
        .and_then(|byte| char::from(*byte).to_digit(radix))
    {
        magnitude = magnitude
            .checked_mul(u64::from(radix))
            .and_then(|value| value.checked_add(u64::from(digit)))
            .ok_or_else(|| HistoryError::InvalidNumber(argument.clone()))?;
        index += 1;
    }
    let count = if negative {
        magnitude.wrapping_neg() as i64
    } else {
        magnitude as i64
    };
    if index == 0 || count < 0 {
        return Err(HistoryError::InvalidNumber(argument.clone()));
    }
    if index == bytes.len() {
        return Ok(Request::Revision);
    }
    let suffix = bytes[index..].trim_ascii_start();
    let scale = match suffix.first() {
        None | Some(b's') => 1,
        Some(b'm') => 60,
        Some(b'h') => 60 * 60,
        Some(b'd') => 24 * 60 * 60,
        _ => return Err(HistoryError::UnknownUnit),
    };
    let seconds = count.checked_mul(scale).ok_or(HistoryError::OutOfRange)?;
    Ok(if suffix.is_empty() {
        Request::SecondsThenRevision(seconds)
    } else {
        Request::Seconds(seconds)
    })
}

/// Validate before changing history. Arithmetic overflow is a reported error;
/// it never inherits C's undefined signed timestamp overflow.
pub(crate) fn execute(
    text: &mut Text,
    earlier: bool,
    arguments: &str,
) -> Result<Option<usize>, HistoryError> {
    let words = crate::workspace::command_words(arguments).map_err(HistoryError::Arguments)?;
    execute_words(text, earlier, &words)
}

/// Execute an already parsed argv without interpreting quotes a second time.
pub(crate) fn execute_words(
    text: &mut Text,
    earlier: bool,
    arguments: &[String],
) -> Result<Option<usize>, HistoryError> {
    let request = request(arguments)?;
    let restored = match request {
        Request::Revision => None,
        Request::Seconds(seconds) | Request::SecondsThenRevision(seconds) => {
            let target = if earlier {
                text.state().checked_sub(seconds)
            } else {
                text.state().checked_add(seconds)
            }
            .ok_or(HistoryError::OutOfRange)?;
            text.restore(target)
        }
    };
    Ok(
        if matches!(request, Request::Revision | Request::SecondsThenRevision(_)) {
            if earlier {
                text.earlier()
            } else {
                text.later()
            }
        } else {
            restored
        },
    )
}
