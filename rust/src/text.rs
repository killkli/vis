//! Byte-oriented piece table and branching undo history.
//!
//! Document bytes live in immutable, shared allocations. Editing copies piece
//! descriptors, never the unchanged document bytes. Each snapshot ends an undo
//! group; the next change starts a new revision. Old branches remain available
//! through chronological history traversal.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn timestamp_now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX),
        Err(error) => {
            let elapsed = error.duration();
            // Unix seconds use the floor, including before the epoch.
            i64::try_from(elapsed.as_secs())
                .map(|seconds| -seconds)
                .unwrap_or(i64::MIN)
                .saturating_sub(i64::from(elapsed.subsec_nanos() != 0))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextError {
    InvalidRange {
        start: usize,
        end: usize,
        len: usize,
    },
    TooLarge,
}

impl fmt::Display for TextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRange { start, end, len } => {
                write!(
                    f,
                    "invalid byte range {start}..{end} for text of length {len}"
                )
            }
            Self::TooLarge => f.write_str("text size exceeds the addressable range"),
        }
    }
}

impl std::error::Error for TextError {}

#[derive(Clone, Debug)]
struct Piece {
    data: Arc<[u8]>,
    range: Range<usize>,
}

impl Piece {
    fn len(&self) -> usize {
        self.range.end - self.range.start
    }
}

/// An immutable, independently owned view of a document's bytes.
///
/// Creating a read snapshot copies only piece descriptors and shares their byte
/// allocations. Cloning a read snapshot is constant-time. Future edits and
/// history traversal do not change its content, and it may outlive the `Text`.
#[derive(Clone, Debug)]
pub struct TextSnapshot {
    pieces: Arc<[Piece]>,
    len: usize,
}

impl TextSnapshot {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Contiguous byte slices in snapshot order, without allocating.
    pub fn chunks(&self) -> impl DoubleEndedIterator<Item = &[u8]> {
        self.pieces
            .iter()
            .map(|piece| &piece.data[piece.range.clone()])
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = u8> + '_ {
        self.chunks().flat_map(|chunk| chunk.iter().copied())
    }
}

/// A stable reference to an original byte, or to the moving end of a document.
///
/// Insertion before a marked byte moves its reported position. Deleting that
/// byte invalidates lookup until an undo restores it. Marks keep their backing
/// allocation alive and cannot accidentally refer to another document.
#[derive(Clone, Debug)]
pub struct Mark {
    owner: Arc<()>,
    data: Option<Arc<[u8]>>,
    offset: usize,
}

impl PartialEq for Mark {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner)
            && self.offset == other.offset
            && match (&self.data, &other.data) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for Mark {}

#[derive(Debug)]
struct Revision {
    pieces: Vec<Piece>,
    len: usize,
    parent: Option<usize>,
    /// The selected redo branch. Other children remain in `Text::revisions`.
    next: Option<usize>,
    undo_pos: usize,
    redo_pos: usize,
    time: i64,
}

/// Text accepts arbitrary bytes, including NUL and invalid UTF-8.
///
/// Piece lookup and editing are linear in the number of pieces. Snapshots share
/// all byte allocations; only piece metadata is copied for a new undo group.
#[derive(Debug)]
pub struct Text {
    owner: Arc<()>,
    revisions: Vec<Revision>,
    current: usize,
    saved: usize,
    group_open: bool,
}

impl Default for Text {
    fn default() -> Self {
        Self::new()
    }
}

impl Text {
    pub fn new() -> Self {
        Self::from_bytes([])
    }

    pub fn from_bytes(data: impl AsRef<[u8]>) -> Self {
        Self::from_bytes_at(data, timestamp_now())
    }

    fn from_bytes_at(data: impl AsRef<[u8]>, time: i64) -> Self {
        let data = data.as_ref();
        let len = data.len();
        let pieces = if data.is_empty() {
            Vec::new()
        } else {
            vec![Piece {
                data: Arc::from(data),
                range: 0..len,
            }]
        };
        Self {
            owner: Arc::new(()),
            revisions: vec![Revision {
                pieces,
                len,
                parent: None,
                next: None,
                undo_pos: 0,
                redo_pos: 0,
                time,
            }],
            current: 0,
            saved: 0,
            group_open: false,
        }
    }

    fn revision(&self) -> &Revision {
        &self.revisions[self.current]
    }

    pub fn len(&self) -> usize {
        self.revision().len
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Capture the current content without ending the current undo group.
    pub fn read_snapshot(&self) -> TextSnapshot {
        TextSnapshot {
            pieces: self.revision().pieces.clone().into(),
            len: self.len(),
        }
    }

    /// Concatenate byte ranges into an immutable snapshot without copying bytes.
    ///
    /// Ranges retain their input order, including overlaps and duplicates. All
    /// ranges, including empty ones, must lie within the document. Validation
    /// and checked length calculation finish before allocating piece metadata.
    /// Like `read_snapshot`, this does not end the current undo group.
    pub fn read_ranges(&self, ranges: &[Range<usize>]) -> Result<TextSnapshot, TextError> {
        let len = ranges.iter().try_fold(0usize, |len, range| {
            self.validate(range)?;
            len.checked_add(range.end - range.start)
                .ok_or(TextError::TooLarge)
        })?;
        let mut pieces = Vec::new();
        for range in ranges {
            if !range.is_empty() {
                self.copy_range(range.clone(), &mut pieces);
            }
        }
        Ok(TextSnapshot {
            pieces: pieces.into(),
            len,
        })
    }

    /// Contiguous byte slices in document order, without allocating.
    pub fn chunks(&self) -> impl DoubleEndedIterator<Item = &[u8]> {
        self.revision()
            .pieces
            .iter()
            .map(|piece| &piece.data[piece.range.clone()])
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = u8> + '_ {
        self.chunks().flat_map(|chunk| chunk.iter().copied())
    }

    pub fn to_vec(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.len());
        for chunk in self.chunks() {
            bytes.extend_from_slice(chunk);
        }
        bytes
    }

    pub fn byte(&self, pos: usize) -> Option<u8> {
        let (piece, offset) = self.locate(pos)?;
        Some(piece.data[offset])
    }

    pub fn bytes(&self, range: Range<usize>) -> Option<Vec<u8>> {
        self.validate(&range).ok()?;
        let mut bytes = Vec::with_capacity(range.end - range.start);
        let mut start = 0;
        for piece in &self.revision().pieces {
            let end = start + piece.len();
            let left = range.start.max(start);
            let right = range.end.min(end);
            if left < right {
                bytes.extend_from_slice(
                    &piece.data
                        [piece.range.start + left - start..piece.range.start + right - start],
                );
            }
            if end >= range.end {
                break;
            }
            start = end;
        }
        Some(bytes)
    }

    fn locate(&self, mut pos: usize) -> Option<(&Piece, usize)> {
        for piece in &self.revision().pieces {
            if pos < piece.len() {
                return Some((piece, piece.range.start + pos));
            }
            pos -= piece.len();
        }
        None
    }

    fn validate(&self, range: &Range<usize>) -> Result<(), TextError> {
        if range.start <= range.end && range.end <= self.len() {
            Ok(())
        } else {
            Err(TextError::InvalidRange {
                start: range.start,
                end: range.end,
                len: self.len(),
            })
        }
    }

    pub fn insert(&mut self, pos: usize, data: &[u8]) -> Result<(), TextError> {
        // The C API explicitly accepts empty insertions at any position.
        if data.is_empty() {
            return Ok(());
        }
        self.replace(pos..pos, data)
    }

    pub fn delete(&mut self, range: Range<usize>) -> Result<(), TextError> {
        // Match the C no-op deletion, while still rejecting reversed ranges.
        if range.start == range.end {
            return Ok(());
        }
        self.replace(range, &[])
    }

    /// Replace a valid range atomically as part of the current undo group.
    pub fn replace(&mut self, range: Range<usize>, data: &[u8]) -> Result<(), TextError> {
        self.replace_at(range, data, timestamp_now())
    }

    fn replace_at(&mut self, range: Range<usize>, data: &[u8], time: i64) -> Result<(), TextError> {
        self.validate(&range)?;
        if range.is_empty() && data.is_empty() {
            return Ok(());
        }
        let new_len = (self.len() - (range.end - range.start))
            .checked_add(data.len())
            .ok_or(TextError::TooLarge)?;

        // Build the replacement before touching history or document state.
        let mut pieces = Vec::new();
        self.copy_range(0..range.start, &mut pieces);
        if !data.is_empty() {
            pieces.push(Piece {
                data: Arc::from(data),
                range: 0..data.len(),
            });
        }
        self.copy_range(range.end..self.len(), &mut pieces);

        if !self.group_open {
            let next = self.revisions.len();
            self.revisions[self.current].next = Some(next);
            self.revisions.push(Revision {
                pieces,
                len: new_len,
                parent: Some(self.current),
                next: None,
                undo_pos: range.start,
                redo_pos: range.start + data.len(),
                time,
            });
            self.current = next;
            self.group_open = true;
        } else {
            let revision = &mut self.revisions[self.current];
            revision.pieces = pieces;
            revision.len = new_len;
            revision.redo_pos = range.start + data.len();
        }
        Ok(())
    }

    fn copy_range(&self, range: Range<usize>, output: &mut Vec<Piece>) {
        let mut start = 0;
        for piece in &self.revision().pieces {
            let end = start + piece.len();
            let left = range.start.max(start);
            let right = range.end.min(end);
            if left < right {
                let source = piece.range.start + left - start..piece.range.start + right - start;
                if let Some(previous) = output.last_mut().filter(|previous| {
                    Arc::ptr_eq(&previous.data, &piece.data) && previous.range.end == source.start
                }) {
                    previous.range.end = source.end;
                } else {
                    output.push(Piece {
                        data: Arc::clone(&piece.data),
                        range: source,
                    });
                }
            }
            if end >= range.end {
                break;
            }
            start = end;
        }
    }

    pub fn snapshot(&mut self) {
        self.group_open = false;
    }

    pub fn undo(&mut self) -> Option<usize> {
        self.snapshot();
        let parent = self.revision().parent?;
        let pos = self.revision().undo_pos;
        self.current = parent;
        Some(pos)
    }

    pub fn redo(&mut self) -> Option<usize> {
        self.snapshot();
        self.current = self.revision().next?;
        Some(self.revision().redo_pos)
    }

    /// Visit the previous revision in creation order, including old branches.
    pub fn earlier(&mut self) -> Option<usize> {
        self.visit(self.current.checked_sub(1)?)
    }

    /// Visit the next revision in creation order, including old branches.
    pub fn later(&mut self) -> Option<usize> {
        self.visit(self.current.checked_add(1)?)
    }

    /// Creation time of the current undo group, in signed Unix seconds.
    ///
    /// Like C `text_state`, this is captured on the group's first change, not
    /// when `snapshot` closes it. Multiple revisions may have the same time,
    /// and system-clock adjustments may make later revisions' times smaller.
    pub fn state(&self) -> i64 {
        self.revision().time
    }

    /// Restore a revision near `timestamp`, following C `text_restore` rules.
    ///
    /// Search follows revision creation order across all branches. Equal times
    /// keep the current revision; requests before or after all recorded times
    /// reach the corresponding endpoint. Nearest-time ties prefer the later
    /// candidate reached during the search. The departure revision is excluded
    /// from the neighboring-candidate checks, as in C.
    ///
    /// The return value is the undo/redo cursor position for the final step.
    /// Restoring the current revision returns zero (the C line cache's default)
    /// and does not end the current undo group.
    pub fn restore(&mut self, timestamp: i64) -> Option<usize> {
        let mut target = self.current;
        while timestamp < self.revisions[target].time && target > 0 {
            target -= 1;
        }
        while timestamp > self.revisions[target].time && target + 1 < self.revisions.len() {
            target += 1;
        }
        let difference = self.revisions[target].time.abs_diff(timestamp);
        if target > 0
            && target - 1 != self.current
            && self.revisions[target - 1].time.abs_diff(timestamp) < difference
        {
            target -= 1;
        }
        if target + 1 < self.revisions.len()
            && target + 1 != self.current
            && self.revisions[target + 1].time.abs_diff(timestamp) < difference
        {
            target += 1;
        }
        self.visit(target)
    }

    fn visit(&mut self, target: usize) -> Option<usize> {
        if target >= self.revisions.len() {
            return None;
        }
        if target == self.current {
            return Some(0);
        }
        self.snapshot();

        // Revision indices increase with allocation. Walking the larger index
        // toward its parent finds the common ancestor without an auxiliary set.
        let mut left = self.current;
        let mut right = target;
        while left != right {
            if left > right {
                left = self.revisions[left].parent?;
            } else {
                right = self.revisions[right].parent?;
            }
        }
        let ancestor = left;

        // Selecting a historical branch also selects it for subsequent redo.
        let mut child = target;
        while let Some(parent) = self.revisions[child].parent {
            self.revisions[parent].next = Some(child);
            child = parent;
        }
        let mut pos = 0;
        while self.current != ancestor {
            pos = self.revision().undo_pos;
            self.current = self.revision().parent?;
        }
        if target != ancestor {
            pos = self.revisions[target].redo_pos;
        }
        self.current = target;
        Some(pos)
    }

    pub fn mark(&self, pos: usize) -> Option<Mark> {
        if pos == self.len() {
            return Some(Mark {
                owner: Arc::clone(&self.owner),
                data: None,
                offset: 0,
            });
        }
        let (piece, offset) = self.locate(pos)?;
        Some(Mark {
            owner: Arc::clone(&self.owner),
            data: Some(Arc::clone(&piece.data)),
            offset,
        })
    }

    pub fn mark_get(&self, mark: Mark) -> Option<usize> {
        if !Arc::ptr_eq(&self.owner, &mark.owner) {
            return None;
        }
        let Some(data) = mark.data else {
            return Some(self.len());
        };
        let mut pos = 0;
        for piece in &self.revision().pieces {
            if Arc::ptr_eq(&piece.data, &data) && piece.range.contains(&mark.offset) {
                return Some(pos + mark.offset - piece.range.start);
            }
            pos += piece.len();
        }
        None
    }

    pub fn mark_saved(&mut self) {
        self.snapshot();
        self.saved = self.current;
    }

    pub fn modified(&self) -> bool {
        self.saved != self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_ranges_share_fragmented_bytes_in_requested_order_and_survive_edits() {
        let mut text = Text::from_bytes(b"ab\0\xffcd");
        text.insert(2, b"XY").unwrap();
        let projected = text.read_ranges(&[6..8, 1..6, 0..2, 1..6, 4..4]).unwrap();
        assert_eq!(projected.len(), 14);
        assert!(!projected.is_empty());
        assert_eq!(
            projected.iter().collect::<Vec<_>>(),
            b"cdbXY\0\xffabbXY\0\xff"
        );
        let original: Vec<_> = text.chunks().collect();
        assert_eq!(
            projected
                .chunks()
                .map(|chunk| chunk.as_ptr())
                .collect::<Vec<_>>(),
            vec![
                original[2][2..].as_ptr(),
                original[0][1..].as_ptr(),
                original[1].as_ptr(),
                original[2].as_ptr(),
                original[0].as_ptr(),
                original[0][1..].as_ptr(),
                original[1].as_ptr(),
                original[2].as_ptr(),
            ]
        );

        // A projection must leave the insertion's undo group open.
        text.insert(0, b"prefix").unwrap();
        text.undo();
        assert_eq!(text.to_vec(), b"ab\0\xffcd");
        text.replace(0..text.len(), b"another branch").unwrap();
        drop(text);
        assert_eq!(
            projected.iter().collect::<Vec<_>>(),
            b"cdbXY\0\xffabbXY\0\xff"
        );
    }

    #[test]
    fn read_ranges_validate_every_range_and_allow_empty_projections() {
        let mut text = Text::from_bytes(b"abc");
        text.insert(1, b"\0\xff").unwrap();
        let before = text.read_snapshot();
        let invalid = TextError::InvalidRange {
            start: 6,
            end: 6,
            len: 5,
        };
        assert_eq!(text.read_ranges(&[0..2, 6..6]).unwrap_err(), invalid);
        assert_eq!(
            text.read_ranges(&[1..3, Range { start: 4, end: 2 }])
                .unwrap_err(),
            TextError::InvalidRange {
                start: 4,
                end: 2,
                len: 5
            },
        );
        assert!(
            text.read_ranges(std::slice::from_ref(&(0..usize::MAX)))
                .is_err()
        );
        for ranges in [&[][..], &[0..0, text.len()..text.len()][..]] {
            let empty = text.read_ranges(ranges).unwrap();
            assert!(empty.is_empty());
            assert_eq!(empty.len(), 0);
            assert_eq!(empty.chunks().count(), 0);
        }
        assert_eq!(text.to_vec(), before.iter().collect::<Vec<_>>());
        text.insert(0, b"prefix").unwrap();
        text.undo();
        assert_eq!(text.to_vec(), b"abc");
    }

    #[test]
    fn state_records_group_creation_and_allows_duplicate_or_reversed_times() {
        let mut text = Text::from_bytes_at([], 100);
        assert_eq!(text.state(), 100);
        text.replace_at(0..0, b"a", 110).unwrap();
        text.replace_at(1..1, b"b", 999).unwrap();
        assert_eq!(text.state(), 110);
        text.snapshot();
        text.replace_at(2..2, b"c", 110).unwrap();
        assert_eq!(text.state(), 110);
        text.snapshot();
        text.replace_at(3..3, b"d", 90).unwrap();
        assert_eq!(text.state(), 90);
        text.undo();
        assert_eq!(text.state(), 110);
        assert_eq!(text.to_vec(), b"abc");
        text.undo();
        assert_eq!(text.state(), 110);
        assert_eq!(text.to_vec(), b"ab");
        text.undo();
        assert_eq!(text.state(), 100);
    }

    #[test]
    fn timed_restore_crosses_original_branch_scenario_and_preserves_saved_marks() {
        // Original test/core/text-test.c topology:
        //  initial -- 0 -- 1 -- 2 -- 3
        //                   `-- 4 -- 5 -- 6 -- 7
        let mut text = Text::from_bytes_at([], 100);
        for (index, value) in b"0123".iter().enumerate() {
            text.replace_at(0..text.len(), &[*value], 101 + index as i64)
                .unwrap();
            text.snapshot();
        }
        text.mark_saved();
        text.undo();
        text.undo();
        let one = text.mark(0).unwrap();
        for (index, value) in b"4567".iter().enumerate() {
            text.replace_at(0..text.len(), &[*value], 105 + index as i64)
                .unwrap();
            text.snapshot();
        }
        assert_eq!(text.mark_get(one.clone()), None);
        for (index, value) in b"01234567".iter().enumerate() {
            assert!(text.restore(101 + index as i64).is_some());
            assert_eq!(text.to_vec(), [*value]);
            assert_eq!(text.state(), 101 + index as i64);
            assert_eq!(text.modified(), *value != b'3');
            assert_eq!(text.mark_get(one.clone()), (*value == b'1').then_some(0));
        }
        for (index, value) in b"01234567".iter().enumerate().rev() {
            assert!(text.restore(101 + index as i64).is_some());
            assert_eq!(text.to_vec(), [*value]);
        }
        text.redo();
        text.redo();
        text.redo();
        assert_eq!(text.to_vec(), b"3");
        assert_eq!(text.redo(), None);
        assert!(!text.modified());
        assert!(text.restore(i64::MIN).is_some());
        assert!(text.is_empty());
        assert_eq!(text.state(), 100);
        assert!(text.restore(i64::MAX).is_some());
        assert_eq!(text.to_vec(), b"7");
        assert_eq!(text.state(), 108);
    }

    #[test]
    fn restore_preserves_c_ties_duplicate_times_and_departure_exclusion() {
        let mut text = Text::from_bytes_at(b"initial", 100);
        text.replace_at(0..text.len(), b"first", 110).unwrap();
        text.snapshot();
        text.replace_at(0..text.len(), b"second", 110).unwrap();
        text.snapshot();
        text.replace_at(0..text.len(), b"third", 120).unwrap();
        text.snapshot();

        // With equal timestamps, restoration keeps the current revision.
        text.undo();
        assert_eq!(text.restore(110), Some(0));
        assert_eq!(text.to_vec(), b"second");
        text.undo();
        text.restore(110);
        assert_eq!(text.to_vec(), b"first");
        text.restore(105);
        assert_eq!(text.to_vec(), b"first"); // halfway selects later
        text.restore(101);
        assert_eq!(text.to_vec(), b"initial");
        text.restore(101);
        assert_eq!(text.to_vec(), b"first"); // departure candidate excluded
        text.restore(119);
        assert_eq!(text.to_vec(), b"third");

        // A no-movement restore, like C, does not end a live undo group.
        let mut grouped = Text::from_bytes_at([], 1);
        grouped.replace_at(0..0, b"a", 2).unwrap();
        grouped.restore(2);
        grouped.replace_at(1..1, b"b", 3).unwrap();
        assert_eq!(grouped.state(), 2);
        grouped.undo();
        assert!(grouped.is_empty());

        grouped.replace_at(0..0, b"a", 4).unwrap();
        assert_eq!(grouped.later(), None);
        grouped.replace_at(1..1, b"b", 5).unwrap();
        assert_eq!(grouped.state(), 4);
        grouped.undo();
        assert!(grouped.is_empty());
    }

    #[test]
    fn history_cursor_follows_final_undo_or_redo_step_across_branches() {
        let mut text = Text::from_bytes_at(b"abcdef", 100);
        text.replace_at(2..2, b"X", 110).unwrap();
        text.snapshot();
        text.replace_at(5..5, b"Y", 120).unwrap();
        text.snapshot();
        text.undo();
        text.replace_at(0..0, b"Z", 130).unwrap();
        text.snapshot();
        assert_eq!(text.earlier(), Some(6)); // undo Z, redo Y
        assert_eq!(text.to_vec(), b"abXcdYef");
        assert_eq!(text.later(), Some(1)); // undo Y, redo Z
        assert_eq!(text.to_vec(), b"ZabXcdef");
        assert_eq!(text.restore(100), Some(2)); // final undo is X
        assert_eq!(text.to_vec(), b"abcdef");
    }

    #[test]
    fn read_snapshots_share_bytes_and_survive_edits_history_and_document_drop() {
        let empty = Text::new().read_snapshot();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.chunks().count(), 0);

        let mut text = Text::from_bytes(b"ab\0\xffcd");
        text.insert(2, b"XY").unwrap();
        let snapshot = text.read_snapshot();
        assert!(!snapshot.is_empty());
        assert_eq!(snapshot.len(), 8);
        assert_eq!(snapshot.iter().collect::<Vec<_>>(), b"abXY\0\xffcd");
        for (original, shared) in text.chunks().zip(snapshot.chunks()) {
            assert_eq!(original.as_ptr(), shared.as_ptr());
            assert_eq!(original.len(), shared.len());
        }

        // Capturing a read view must not introduce an undo boundary.
        text.insert(0, b"prefix").unwrap();
        text.undo();
        assert_eq!(text.to_vec(), b"ab\0\xffcd");
        assert_eq!(snapshot.iter().collect::<Vec<_>>(), b"abXY\0\xffcd");
        text.redo();
        text.snapshot();
        text.replace(0..text.len(), b"branch one").unwrap();
        text.snapshot();
        text.undo();
        text.replace(0..text.len(), b"branch two").unwrap();
        text.earlier();
        assert_eq!(text.to_vec(), b"branch one");
        text.later();
        assert_eq!(text.to_vec(), b"branch two");
        drop(text);

        let clone = snapshot.clone();
        assert!(Arc::ptr_eq(&snapshot.pieces, &clone.pieces));
        assert_eq!(clone.iter().rev().collect::<Vec<_>>(), b"dc\xff\0YXba");
        drop(snapshot);
        assert_eq!(clone.iter().collect::<Vec<_>>(), b"abXY\0\xffcd");
    }

    #[test]
    fn byte_storage_boundaries_and_atomic_errors() {
        let mut text = Text::from_bytes(b"a\0\xff\nb");
        assert_eq!(text.byte(2), Some(0xff));
        assert_eq!(text.byte(5), None);
        assert_eq!(text.bytes(1..4), Some(vec![0, 0xff, b'\n']));
        assert_eq!(text.bytes(Range { start: 4, end: 3 }), None);
        assert!(text.insert(6, b"x").is_err());
        assert!(text.delete(2..usize::MAX).is_err());
        assert!(text.replace(Range { start: 3, end: 2 }, b"x").is_err());
        assert_eq!(text.to_vec(), b"a\0\xff\nb");
        assert!(!text.modified());
        text.insert(usize::MAX, b"").unwrap();
        text.delete(usize::MAX..usize::MAX).unwrap();
        assert_eq!(text.undo(), None);
    }

    #[test]
    fn grouped_edits_and_snapshots() {
        let mut text = Text::new();
        for (pos, data) in [(0, "3"), (0, "1"), (1, "2"), (3, "4")] {
            text.insert(pos, data.as_bytes()).unwrap();
        }
        assert_eq!(text.to_vec(), b"1234");
        assert_eq!(text.undo(), Some(0));
        assert!(text.is_empty());
        assert!(text.redo().is_some());
        assert_eq!(text.to_vec(), b"1234");
        text.snapshot();
        text.delete(1..3).unwrap();
        text.snapshot();
        text.insert(1, b"xy").unwrap();
        assert_eq!(text.to_vec(), b"1xy4");
        text.undo();
        assert_eq!(text.to_vec(), b"14");
        text.undo();
        assert_eq!(text.to_vec(), b"1234");
        text.redo();
        text.redo();
        assert_eq!(text.iter().collect::<Vec<_>>(), b"1xy4");
        assert_eq!(text.iter().rev().collect::<Vec<_>>(), b"4yx1");
    }

    #[test]
    fn chronological_traversal_preserves_all_branches() {
        let mut text = Text::new();
        for value in b"0123" {
            text.replace(0..text.len(), &[*value]).unwrap();
            text.snapshot();
        }
        text.undo();
        text.undo();
        assert_eq!(text.to_vec(), b"1");
        for value in b"4567" {
            text.replace(0..text.len(), &[*value]).unwrap();
            text.snapshot();
        }
        while text.undo().is_some() {}
        assert!(text.is_empty());
        for value in b"01234567" {
            assert!(text.later().is_some());
            assert_eq!(text.to_vec(), [*value]);
        }
        assert_eq!(text.later(), None);
        for value in b"6543210" {
            assert!(text.earlier().is_some());
            assert_eq!(text.to_vec(), [*value]);
        }
        text.redo();
        text.redo();
        text.redo();
        assert_eq!(text.to_vec(), b"3");
        assert_eq!(text.redo(), None);
    }

    #[test]
    fn stable_marks_follow_bytes_and_reappear_after_undo() {
        let mut text = Text::from_bytes(b"hello");
        let start = text.mark(0).unwrap();
        let middle = text.mark(2).unwrap();
        let end = text.mark(5).unwrap();
        assert_eq!(text.mark(6), None);
        text.insert(0, b"abc").unwrap();
        text.snapshot();
        assert_eq!(text.mark_get(start.clone()), Some(3));
        assert_eq!(text.mark_get(middle.clone()), Some(5));
        assert_eq!(text.mark_get(end.clone()), Some(8));
        text.delete(4..7).unwrap();
        assert_eq!(text.mark_get(middle.clone()), None);
        text.undo();
        assert_eq!(text.mark_get(middle.clone()), Some(5));
        text.undo();
        assert_eq!(text.mark_get(start), Some(0));
        assert_eq!(text.mark_get(middle), Some(2));
        assert_eq!(text.mark_get(end.clone()), Some(5));
        assert_eq!(Text::from_bytes(b"hello").mark_get(end), None);
    }

    #[test]
    fn saved_revision_is_a_snapshot_boundary() {
        let mut text = Text::new();
        text.insert(0, b"first").unwrap();
        assert!(text.modified());
        text.mark_saved();
        assert!(!text.modified());
        text.insert(5, b" second").unwrap();
        assert!(text.modified());
        text.undo();
        assert!(!text.modified());
        assert_eq!(text.to_vec(), b"first");
        text.redo();
        assert!(text.modified());
    }

    #[test]
    fn randomized_edit_and_history_model() {
        let mut seed = 0x93a8_72c4_ee91_1603_u64;
        let mut random = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as usize
        };
        let mut text = Text::new();
        let mut model = Vec::new();
        let mut states = vec![model.clone()];
        for step in 0..1200 {
            let start = random() % (model.len() + 1);
            let end = start + random() % (model.len() - start + 1);
            let bytes: Vec<_> = (0..random() % 9).map(|_| random() as u8).collect();
            if start == end && bytes.is_empty() {
                continue;
            }
            text.replace(start..end, &bytes).unwrap();
            model.splice(start..end, bytes);
            assert_eq!(text.to_vec(), model, "edit {step}");
            for pos in 0..=model.len() {
                assert_eq!(text.byte(pos), model.get(pos).copied());
            }
            text.snapshot();
            states.push(model.clone());
        }
        for state in states.iter().rev().skip(1) {
            assert!(text.undo().is_some());
            assert_eq!(&text.to_vec(), state);
        }
        assert_eq!(text.undo(), None);
        for state in states.iter().skip(1) {
            assert!(text.redo().is_some());
            assert_eq!(&text.to_vec(), state);
        }
        assert_eq!(text.redo(), None);
    }
}

#[cfg(test)]
mod history_command_tests {
    use super::Text;
    use crate::history::{execute, execute_words};

    fn history() -> Text {
        let mut text = Text::from_bytes_at(b"A", 1_000);
        text.replace_at(1..1, b"B", 1_060).unwrap();
        text.snapshot();
        text.replace_at(2..2, b"C", 1_120).unwrap();
        text.snapshot();
        text
    }

    #[test]
    fn time_units_reach_timestamped_revisions_across_branches() {
        let mut text = history();
        execute(&mut text, true, "1m").unwrap();
        assert_eq!(text.to_vec(), b"AB");
        text.replace_at(2..2, b"D", 1_180).unwrap();
        text.snapshot();
        execute(&mut text, true, "60s").unwrap();
        assert_eq!(text.to_vec(), b"ABC");
        execute(&mut text, false, "1hour").unwrap();
        assert_eq!(text.to_vec(), b"ABD");
        execute(&mut text, true, "1day").unwrap();
        assert_eq!(text.to_vec(), b"A");
    }

    #[test]
    fn native_numeric_quirks_and_quoted_suffix_are_preserved() {
        let mut text = history();
        execute(&mut text, true, "999 extra ignored").unwrap();
        assert_eq!(text.to_vec(), b"AB");
        execute(&mut text, true, "0").unwrap();
        assert_eq!(text.to_vec(), b"A");
        execute(&mut text, false, "-").unwrap();
        assert_eq!(text.to_vec(), b"AB");
        execute(&mut text, true, "'0 '").unwrap();
        assert_eq!(text.to_vec(), b"A");
        execute(&mut text, false, "0x1minute").unwrap();
        assert_eq!(text.to_vec(), b"AB");
    }

    #[test]
    fn invalid_arguments_and_timestamp_overflow_do_not_modify_history() {
        for argument in [
            "-1",
            "+1",
            "word",
            "2q",
            "1H",
            "18446744073709551616",
            "9223372036854775807d",
        ] {
            let mut text = history();
            assert!(execute(&mut text, true, argument).is_err(), "{argument}");
            assert_eq!(text.to_vec(), b"ABC");
            assert_eq!(text.state(), 1_120);
        }
        let mut text = Text::from_bytes_at(Vec::new(), i64::MAX);
        assert!(execute(&mut text, false, "1s").is_err());
        assert_eq!(text.state(), i64::MAX);
        assert!(
            execute(&mut text, false, "'unterminated")
                .unwrap_err()
                .is_parse()
        );
    }

    #[test]
    fn typed_history_arguments_keep_quoted_whitespace_literal() {
        let mut text = history();
        execute_words(&mut text, true, &["0 ".into(), "ignored".into()]).unwrap();
        assert_eq!(text.to_vec(), b"AB");
        assert!(execute_words(&mut text, false, &["'1m'".into()]).is_err());
        assert_eq!(text.to_vec(), b"AB");
    }
}
