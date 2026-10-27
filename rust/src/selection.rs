//! Oriented, inclusive editor selections and half-open range algebra.
//!
//! The cursor and anchor point at the first bytes of the selected endpoint
//! characters, as in `view.c`. Range algebra keeps adjacent ranges separate:
//! only strict overlap merges them. Algebra may return no ranges; an editor
//! selection set always retains at least one cursor.
use crate::motion;
use crate::text::{Mark, Text, TextError};
use std::collections::BTreeSet;
use std::fmt;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

/// Identity of a logical selection object, independent of its vector position.
/// IDs are never recycled; closing or merging a selection invalidates its ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SelectionId(u64);
impl SelectionId {
    pub fn fresh() -> Result<Self, SelectionError> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        // The method is deprecated in favour of `try_update`, which needs a
        // newer Rust than the crate supports.
        #[allow(deprecated)]
        NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map(Self)
        .map_err(|_| SelectionError::IdentityExhausted)
    }
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectionError {
    IdentityExhausted,
    UnknownIdentity(SelectionId),
    DuplicateIdentity(SelectionId),
    InvalidPosition {
        position: usize,
        len: usize,
    },
    InvalidRange {
        start: usize,
        end: usize,
        len: usize,
    },
    InvalidPrimary {
        index: usize,
        count: usize,
    },
    EmptySet,
    UnrepresentableRange {
        start: usize,
        end: usize,
    },
    Text(TextError),
}
impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdentityExhausted => f.write_str("selection identity space exhausted"),
            Self::UnknownIdentity(id) => write!(f, "selection {} is no longer live", id.get()),
            Self::DuplicateIdentity(id) => write!(f, "duplicate selection identity {}", id.get()),
            Self::InvalidPosition { position, len } => {
                write!(f, "selection position {position} exceeds text length {len}")
            }
            Self::InvalidRange { start, end, len } => write!(
                f,
                "invalid selection range {start}..{end} for text length {len}"
            ),
            Self::InvalidPrimary { index, count } => {
                write!(f, "primary index {index} exceeds selection count {count}")
            }
            Self::EmptySet => f.write_str("an editor requires at least one selection"),
            Self::UnrepresentableRange { start, end } => {
                write!(f, "range {start}..{end} splits a character endpoint")
            }
            Self::Text(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for SelectionError {}
impl From<TextError> for SelectionError {
    fn from(error: TextError) -> Self {
        Self::Text(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Forward,
    Backward,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Parity {
    Odd,
    Even,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Selection {
    pub anchor: usize,
    pub cursor: usize,
}
impl Selection {
    pub fn new(data: &[u8], anchor: usize, cursor: usize) -> Result<Self, SelectionError> {
        for position in [anchor, cursor] {
            validate_position(position, data.len())?;
        }
        Ok(Self { anchor, cursor })
    }
    pub fn direction(self) -> Direction {
        if self.anchor > self.cursor {
            Direction::Backward
        } else {
            Direction::Forward
        }
    }
    pub fn range(self, data: &[u8]) -> Result<Range<usize>, SelectionError> {
        Self::new(data, self.anchor, self.cursor)?;
        Ok(self.anchor.min(self.cursor)..motion::char_next(data, self.anchor.max(self.cursor)))
    }
    /// Empty ranges before EOF select the character at their start, matching
    /// `view_selections_set`; EOF itself is the only zero-byte selection.
    /// Nonempty ranges must have character-representable endpoints.
    pub fn from_range(
        data: &[u8],
        range: Range<usize>,
        direction: Direction,
    ) -> Result<Self, SelectionError> {
        validate_range(&range, data.len())?;
        let last = if range.is_empty() {
            range.start
        } else {
            motion::char_prev(data, range.end)
        };
        if last < range.start || (!range.is_empty() && motion::char_next(data, last) != range.end) {
            return Err(SelectionError::UnrepresentableRange {
                start: range.start,
                end: range.end,
            });
        }
        let (anchor, cursor) = match direction {
            Direction::Forward => (range.start, last),
            Direction::Backward => (last, range.start),
        };
        Ok(Self { anchor, cursor })
    }
    pub fn flip(&mut self) {
        std::mem::swap(&mut self.anchor, &mut self.cursor);
    }
    pub fn collapse(&mut self) {
        self.anchor = self.cursor;
    }
    pub fn save(self, text: &Text) -> Result<SelectionMarks, SelectionError> {
        let anchor = text
            .mark(self.anchor)
            .ok_or(SelectionError::InvalidPosition {
                position: self.anchor,
                len: text.len(),
            })?;
        let cursor = text
            .mark(self.cursor)
            .ok_or(SelectionError::InvalidPosition {
                position: self.cursor,
                len: text.len(),
            })?;
        Ok(SelectionMarks {
            anchor,
            cursor,
            document: text.mark(text.len()).expect("EOF is always markable"),
        })
    }
}
#[derive(Clone, Debug)]
pub struct SelectionMarks {
    document: Mark,
    anchor: Mark,
    cursor: Mark,
}
impl SelectionMarks {
    pub fn cursor_position(&self, text: &Text) -> Option<usize> {
        text.mark_get(self.cursor.clone())
    }
    pub fn anchor_position(&self, text: &Text) -> Option<usize> {
        text.mark_get(self.anchor.clone())
    }
    pub fn belongs_to(&self, text: &Text) -> bool {
        text.mark(text.len()).as_ref() == Some(&self.document)
    }
    /// Deleted endpoints invalidate the selection until undo restores them.
    pub fn restore(&self, text: &Text) -> Option<Selection> {
        Some(Selection {
            anchor: text.mark_get(self.anchor.clone())?,
            cursor: text.mark_get(self.cursor.clone())?,
        })
    }
}
fn validate_position(position: usize, len: usize) -> Result<(), SelectionError> {
    if position > len {
        Err(SelectionError::InvalidPosition { position, len })
    } else {
        Ok(())
    }
}
fn validate_range(range: &Range<usize>, len: usize) -> Result<(), SelectionError> {
    if range.start > range.end || range.end > len {
        Err(SelectionError::InvalidRange {
            start: range.start,
            end: range.end,
            len,
        })
    } else {
        Ok(())
    }
}
fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Sort and merge strict overlaps. Adjacency remains separate, including the
/// boundaries used by vis's line-selection union/intersection fixtures.
pub fn normalize_ranges(
    ranges: &[Range<usize>],
    len: usize,
) -> Result<Vec<Range<usize>>, SelectionError> {
    for r in ranges {
        validate_range(r, len)?;
    }
    let mut sorted = ranges.to_vec();
    sorted.sort_by_key(|r| (r.start, r.end));
    let mut result: Vec<Range<usize>> = Vec::with_capacity(sorted.len());
    for range in sorted {
        if let Some(last) = result.last_mut() {
            if overlaps(last, &range) {
                last.end = last.end.max(range.end);
                continue;
            }
        }
        result.push(range);
    }
    Ok(result)
}
pub fn union(
    a: &[Range<usize>],
    b: &[Range<usize>],
    len: usize,
) -> Result<Vec<Range<usize>>, SelectionError> {
    let mut ranges = a.to_vec();
    ranges.extend_from_slice(b);
    normalize_ranges(&ranges, len)
}
pub fn intersection(
    a: &[Range<usize>],
    b: &[Range<usize>],
    len: usize,
) -> Result<Vec<Range<usize>>, SelectionError> {
    let a = normalize_ranges(a, len)?;
    let b = normalize_ranges(b, len)?;
    let mut result = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let x = &a[i];
        let y = &b[j];
        if overlaps(x, y) {
            result.push(x.start.max(y.start)..x.end.min(y.end));
        }
        if x.end < y.end {
            i += 1;
        } else {
            j += 1;
        }
    }
    Ok(result)
}
pub fn complement(
    ranges: &[Range<usize>],
    len: usize,
) -> Result<Vec<Range<usize>>, SelectionError> {
    let ranges = normalize_ranges(ranges, len)?;
    let mut result = Vec::new();
    let mut pos = 0;
    for r in ranges {
        if pos < r.start {
            result.push(pos..r.start);
        }
        pos = r.end;
    }
    if pos < len {
        result.push(pos..len);
    }
    Ok(result)
}
pub fn minus(
    a: &[Range<usize>],
    b: &[Range<usize>],
    len: usize,
) -> Result<Vec<Range<usize>>, SelectionError> {
    intersection(a, &complement(b, len)?, len)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionSet {
    selections: Vec<Selection>,
    ids: Vec<SelectionId>,
    primary: usize,
}
impl SelectionSet {
    pub fn new(
        data: &[u8],
        selections: Vec<Selection>,
        primary: usize,
    ) -> Result<Self, SelectionError> {
        if selections.is_empty() {
            return Err(SelectionError::EmptySet);
        }
        if primary >= selections.len() {
            return Err(SelectionError::InvalidPrimary {
                index: primary,
                count: selections.len(),
            });
        }
        let ids = (0..selections.len())
            .map(|_| SelectionId::fresh())
            .collect::<Result<_, _>>()?;
        let mut set = Self {
            selections,
            ids,
            primary,
        };
        set.normalize(data)?;
        Ok(set)
    }
    pub fn single(data: &[u8], position: usize) -> Result<Self, SelectionError> {
        Self::new(data, vec![Selection::new(data, position, position)?], 0)
    }
    pub fn selections(&self) -> &[Selection] {
        &self.selections
    }
    pub fn ids(&self) -> &[SelectionId] {
        &self.ids
    }
    pub fn primary_id(&self) -> SelectionId {
        self.ids[self.primary]
    }
    pub fn identified(&self) -> Vec<(SelectionId, Selection)> {
        self.ids
            .iter()
            .copied()
            .zip(self.selections.iter().copied())
            .collect()
    }
    pub fn index_of(&self, id: SelectionId) -> Option<usize> {
        self.ids.iter().position(|candidate| *candidate == id)
    }
    pub fn get(&self, id: SelectionId) -> Option<Selection> {
        self.index_of(id).map(|index| self.selections[index])
    }

    /// Restore live logical objects without merging them. Direct Lua geometry
    /// setters may temporarily overlap selections; native actions normalize later.
    pub fn from_identified(
        data: &[u8],
        identified: Vec<(SelectionId, Selection)>,
        primary: SelectionId,
    ) -> Result<Self, SelectionError> {
        if identified.is_empty() {
            return Err(SelectionError::EmptySet);
        }
        let mut seen = BTreeSet::new();
        for (id, selection) in &identified {
            Selection::new(data, selection.anchor, selection.cursor)?;
            if !seen.insert(*id) {
                return Err(SelectionError::DuplicateIdentity(*id));
            }
        }
        let primary = identified
            .iter()
            .position(|(id, _)| *id == primary)
            .ok_or(SelectionError::UnknownIdentity(primary))?;
        let (ids, selections) = identified.into_iter().unzip();
        Ok(Self {
            selections,
            ids,
            primary,
        })
    }

    /// Update existing objects by ordinal position, allocating IDs only for
    /// extra selections, then perform ordinary editor normalization.
    pub fn replace_selections(
        &mut self,
        data: &[u8],
        selections: Vec<Selection>,
        primary: usize,
    ) -> Result<(), SelectionError> {
        if selections.is_empty() {
            return Err(SelectionError::EmptySet);
        }
        if primary >= selections.len() {
            return Err(SelectionError::InvalidPrimary {
                index: primary,
                count: selections.len(),
            });
        }
        let ids = (0..selections.len())
            .map(|index| {
                self.ids
                    .get(index)
                    .copied()
                    .map_or_else(SelectionId::fresh, Ok)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let primary_id = ids[primary];
        let mut next =
            Self::from_identified(data, ids.into_iter().zip(selections).collect(), primary_id)?;
        next.normalize(data)?;
        *self = next;
        Ok(())
    }

    pub fn update(
        &mut self,
        data: &[u8],
        id: SelectionId,
        selection: Selection,
    ) -> Result<(), SelectionError> {
        Selection::new(data, selection.anchor, selection.cursor)?;
        let index = self
            .index_of(id)
            .ok_or(SelectionError::UnknownIdentity(id))?;
        self.selections[index] = selection;
        Ok(())
    }
    pub fn set_primary_id(&mut self, id: SelectionId) -> Result<(), SelectionError> {
        self.primary = self
            .index_of(id)
            .ok_or(SelectionError::UnknownIdentity(id))?;
        Ok(())
    }
    pub fn remove(&mut self, id: SelectionId) -> Result<bool, SelectionError> {
        let index = self
            .index_of(id)
            .ok_or(SelectionError::UnknownIdentity(id))?;
        if self.len() == 1 {
            return Ok(false);
        }
        self.selections.remove(index);
        self.ids.remove(index);
        if self.primary > index {
            self.primary -= 1;
        } else {
            self.primary = self.primary.min(self.len() - 1);
        }
        Ok(true)
    }
    /// C cursor creation inserts a point without normalizing its byte position.
    pub(crate) fn insert_point_native(
        &mut self,
        data: &[u8],
        cursor: usize,
    ) -> Result<Option<SelectionId>, SelectionError> {
        Selection::new(data, cursor, cursor)?;
        if self.selections.iter().any(|s| s.cursor == cursor) {
            return Ok(None);
        }
        let index = self
            .selections
            .iter()
            .position(|s| s.cursor > cursor)
            .unwrap_or(self.len());
        let id = SelectionId::fresh()?;
        self.selections.insert(
            index,
            Selection {
                anchor: cursor,
                cursor,
            },
        );
        self.ids.insert(index, id);
        self.primary = index;
        Ok(Some(id))
    }
    /// Create a distinct object without normalizing existing selections.
    pub fn push(
        &mut self,
        data: &[u8],
        selection: Selection,
        make_primary: bool,
    ) -> Result<SelectionId, SelectionError> {
        Selection::new(data, selection.anchor, selection.cursor)?;
        let id = SelectionId::fresh()?;
        self.ids.push(id);
        self.selections.push(selection);
        if make_primary {
            self.primary = self.len() - 1;
        }
        Ok(id)
    }
    pub fn len(&self) -> usize {
        self.selections.len()
    }
    pub fn is_empty(&self) -> bool {
        false
    }
    pub fn primary(&self) -> Selection {
        self.selections[self.primary]
    }
    pub fn primary_index(&self) -> usize {
        self.primary
    }
    pub fn set_primary(&mut self, index: usize) -> Result<(), SelectionError> {
        if index >= self.len() {
            return Err(SelectionError::InvalidPrimary {
                index,
                count: self.len(),
            });
        }
        self.primary = index;
        Ok(())
    }
    pub fn add(
        &mut self,
        data: &[u8],
        selection: Selection,
        make_primary: bool,
    ) -> Result<(), SelectionError> {
        let mut selections = self.selections.clone();
        selections.push(selection);
        let primary = if make_primary {
            selections.len() - 1
        } else {
            self.primary
        };
        self.replace_selections(data, selections, primary)?;
        Ok(())
    }
    pub fn replace_primary(
        &mut self,
        data: &[u8],
        selection: Selection,
    ) -> Result<(), SelectionError> {
        let mut selections = self.selections.clone();
        selections[self.primary] = selection;
        self.replace_selections(data, selections, self.primary)?;
        Ok(())
    }
    /// The last live cursor cannot be removed.
    pub fn remove_primary(&mut self) -> bool {
        if self.len() == 1 {
            return false;
        }
        self.selections.remove(self.primary);
        self.ids.remove(self.primary);
        self.primary = self.primary.min(self.len() - 1);
        true
    }
    pub fn keep_primary(&mut self) {
        self.ids = vec![self.primary_id()];
        self.selections = vec![self.primary()];
        self.primary = 0;
    }
    pub fn navigate(&mut self, steps: isize) {
        let count = self.len();
        let amount = steps.unsigned_abs() % count;
        self.primary = if steps >= 0 {
            (self.primary + amount) % count
        } else {
            (self.primary + count - amount) % count
        };
    }
    pub fn ranges(&self, data: &[u8]) -> Result<Vec<Range<usize>>, SelectionError> {
        self.selections.iter().map(|s| s.range(data)).collect()
    }
    /// Normalization retains the direction of the first selection in each
    /// merged component and moves primary identity into its merged component.
    /// Errors leave the previous selection set intact.
    pub fn normalize(&mut self, data: &[u8]) -> Result<(), SelectionError> {
        self.normalize_with_order(data, true)
    }
    /// Native operators retain the linked view's order even when moving a
    /// cursor past another selection. Only adjacent overlapping objects merge.
    pub(crate) fn normalize_native(&mut self, data: &[u8]) -> Result<(), SelectionError> {
        self.normalize_with_order(data, false)
    }
    fn normalize_with_order(&mut self, data: &[u8], sort: bool) -> Result<(), SelectionError> {
        let mut sorted: Vec<_> = self
            .selections
            .iter()
            .copied()
            .enumerate()
            .map(|(i, s)| Ok((s.range(data)?, s, self.ids[i], i == self.primary)))
            .collect::<Result<_, SelectionError>>()?;
        if sort {
            sorted.sort_by_key(|(r, _, _, _)| (r.start, r.end));
        }
        let mut merged: Vec<(Range<usize>, Direction, SelectionId, bool)> = Vec::new();
        for (range, s, id, primary) in sorted {
            if let Some((last, _, _, is_primary)) = merged.last_mut() {
                if overlaps(last, &range) {
                    last.start = last.start.min(range.start);
                    last.end = last.end.max(range.end);
                    *is_primary |= primary;
                    continue;
                }
            }
            merged.push((range, s.direction(), id, primary));
        }
        let mut selections = Vec::with_capacity(merged.len());
        let mut ids = Vec::with_capacity(merged.len());
        let mut primary = 0;
        for (i, (range, direction, id, is_primary)) in merged.into_iter().enumerate() {
            // view_selections_set rounds the final endpoint to the previous
            // grapheme even if that crosses a raw byte position. This matters
            // for invalid continuation bytes and codepoint-oriented motions.
            let end = if range.is_empty() {
                range.end
            } else {
                motion::char_prev(data, range.end)
            };
            selections.push(match direction {
                Direction::Forward => Selection {
                    anchor: range.start,
                    cursor: end,
                },
                Direction::Backward => Selection {
                    anchor: end,
                    cursor: range.start,
                },
            });
            ids.push(id);
            if is_primary {
                primary = i;
            }
        }
        self.selections = selections;
        self.ids = ids;
        self.primary = primary;
        Ok(())
    }
    pub fn transform(
        &mut self,
        data: &[u8],
        mut f: impl FnMut(Selection) -> Selection,
    ) -> Result<(), SelectionError> {
        let selections = self.selections.iter().copied().map(&mut f).collect();
        self.replace_selections(data, selections, self.primary)?;
        Ok(())
    }
    pub fn flip_all(&mut self) {
        for s in &mut self.selections {
            s.flip();
        }
    }
    pub fn collapse_all(&mut self, data: &[u8]) -> Result<(), SelectionError> {
        self.transform(data, |mut s| {
            s.collapse();
            s
        })
    }
    /// Replace ranges as `view_selections_set_all` does: existing positions
    /// contribute their orientation, new selections extend forward, and the
    /// first resulting selection becomes primary. Empty output retains a
    /// collapsed primary cursor.
    pub fn set_ranges(
        &mut self,
        data: &[u8],
        ranges: Vec<Range<usize>>,
    ) -> Result<(), SelectionError> {
        for r in &ranges {
            validate_range(r, data.len())?;
        }
        if ranges.is_empty() {
            let cursor = self.primary().cursor.min(data.len());
            let id = self.primary_id();
            *self = Self::from_identified(
                data,
                vec![(
                    id,
                    Selection {
                        anchor: cursor,
                        cursor,
                    },
                )],
                id,
            )?;
            return Ok(());
        }
        let selections = ranges
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                let direction = self
                    .selections
                    .get(i)
                    .map_or(Direction::Forward, |s| s.direction());
                Selection::from_range(data, r, direction)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.replace_selections(data, selections, 0)?;
        Ok(())
    }
    pub fn union_with(
        &mut self,
        data: &[u8],
        ranges: &[Range<usize>],
    ) -> Result<(), SelectionError> {
        self.set_ranges(data, union(&self.ranges(data)?, ranges, data.len())?)
    }
    pub fn intersect_with(
        &mut self,
        data: &[u8],
        ranges: &[Range<usize>],
    ) -> Result<(), SelectionError> {
        self.set_ranges(data, intersection(&self.ranges(data)?, ranges, data.len())?)
    }
    pub fn subtract(&mut self, data: &[u8], ranges: &[Range<usize>]) -> Result<(), SelectionError> {
        self.set_ranges(data, minus(&self.ranges(data)?, ranges, data.len())?)
    }
    pub fn invert(&mut self, data: &[u8]) -> Result<(), SelectionError> {
        self.set_ranges(data, complement(&self.ranges(data)?, data.len())?)
    }
    fn retain_indices(&mut self, keep: impl Fn(usize) -> bool) {
        let old_primary = self.primary;
        let fallback = self.primary();
        let fallback_id = self.primary_id();
        let mut primary = None;
        let mut result = Vec::new();
        let mut ids = Vec::new();
        for (i, s) in self.selections.iter().copied().enumerate() {
            if keep(i) {
                if i == old_primary {
                    primary = Some(result.len());
                }
                result.push(s);
                ids.push(self.ids[i]);
            }
        }
        if result.is_empty() {
            result.push(fallback);
            ids.push(fallback_id);
        }
        self.selections = result;
        self.ids = ids;
        self.primary = primary.unwrap_or(0);
    }
    pub fn keep_parity(&mut self, parity: Parity) {
        self.retain_indices(|i| match parity {
            Parity::Odd => i % 2 == 0,
            Parity::Even => i % 2 == 1,
        });
    }
    /// Columns are ordinal cursor positions within each line, rather than
    /// display columns; the requested index is zero-based and clamps globally.
    pub fn column_count(&self, data: &[u8]) -> Result<usize, SelectionError> {
        Ok(self.columns(data)?.into_iter().max().unwrap_or(0) + 1)
    }
    fn columns(&self, data: &[u8]) -> Result<Vec<usize>, SelectionError> {
        let mut columns = Vec::with_capacity(self.len());
        let mut previous = None;
        let mut column = 0;
        for s in &self.selections {
            s.range(data)?;
            let line = motion::line_begin(data, s.cursor);
            if previous == Some(line) {
                column += 1;
            } else {
                column = 0;
            }
            columns.push(column);
            previous = Some(line);
        }
        Ok(columns)
    }
    pub fn remove_column(&mut self, data: &[u8], column: usize) -> Result<(), SelectionError> {
        let columns = self.columns(data)?;
        let column = column.min(columns.iter().copied().max().unwrap_or(0));
        self.retain_indices(|i| columns[i] != column);
        Ok(())
    }
    pub fn keep_column(&mut self, data: &[u8], column: usize) -> Result<(), SelectionError> {
        let columns = self.columns(data)?;
        let column = column.min(columns.iter().copied().max().unwrap_or(0));
        self.retain_indices(|i| columns[i] == column);
        Ok(())
    }
    /// Return false if every selection was whitespace; a collapsed primary
    /// cursor remains so the editor can leave visual mode, as C does.
    pub fn trim(&mut self, data: &[u8]) -> Result<bool, SelectionError> {
        let mut result = Vec::new();
        let mut primary = None;
        for (i, s) in self.selections.iter().copied().enumerate() {
            let mut r = s.range(data)?;
            while r.start < r.end && c_space(data[r.end - 1]) {
                r.end -= 1;
            }
            while r.start < r.end && c_space(data[r.start]) {
                r.start += 1;
            }
            if !r.is_empty() {
                if i == self.primary {
                    primary = Some(result.len());
                }
                result.push((self.ids[i], Selection::from_range(data, r, s.direction())?));
            }
        }
        if result.is_empty() {
            let cursor = self.primary().cursor;
            *self = Self::from_identified(
                data,
                vec![(self.primary_id(), Selection::new(data, cursor, cursor)?)],
                self.primary_id(),
            )?;
            return Ok(false);
        }
        let primary_id = result[primary.unwrap_or(0)].0;
        *self = Self::from_identified(data, result, primary_id)?;
        self.normalize(data)?;
        Ok(true)
    }
    /// Rotate selected contents right for positive steps, left for negative.
    /// With multiple selections per line, rotate each line independently;
    /// otherwise rotate the entire selection list, matching `main.c`.
    pub fn rotate_contents(&mut self, text: &mut Text, steps: isize) -> Result<(), SelectionError> {
        let data = text.to_vec();
        let ranges = self.ranges(&data)?;
        let columns = self.column_count(&data)?;
        let mut source: Vec<usize> = (0..self.len()).collect();
        let mut start = 0;
        while start < self.len() {
            let line = motion::line_begin(&data, self.selections[start].cursor);
            let mut end = start + 1;
            while end < self.len()
                && (columns == 1 || motion::line_begin(&data, self.selections[end].cursor) == line)
            {
                end += 1;
            }
            let count = end - start;
            let off = steps.unsigned_abs() % count;
            for old in start..end {
                let target = if steps >= 0 {
                    (old - start + off) % count
                } else {
                    (old - start + count - off) % count
                };
                source[start + target] = old;
            }
            start = end;
        }
        if source.iter().enumerate().all(|(i, &source)| i == source) {
            return Ok(());
        }
        let contents: Vec<_> = source
            .iter()
            .map(|&i| data[ranges[i].clone()].to_vec())
            .collect();
        // Validate all resulting offsets and character endpoints before editing.
        let mut output = Vec::with_capacity(data.len());
        let mut new_ranges = Vec::with_capacity(self.len());
        let mut pos = 0;
        for (r, bytes) in ranges.iter().zip(&contents) {
            output.extend_from_slice(&data[pos..r.start]);
            let begin = output.len();
            output.extend_from_slice(bytes);
            new_ranges.push(begin..output.len());
            pos = r.end;
        }
        output.extend_from_slice(&data[pos..]);
        let mut updated = self.clone();
        updated.set_ranges(&output, new_ranges)?;
        let old_primary = self.primary;
        updated.primary = old_primary.min(updated.len() - 1);
        // Editing from the end keeps original offsets valid and preserves
        // stable marks on all unselected bytes. The caller supplies the undo
        // group boundary, just as for the other text edit operations.
        for (range, bytes) in ranges.into_iter().zip(contents).rev() {
            if data[range.clone()] != bytes {
                text.replace(range, &bytes)?;
            }
        }
        *self = updated;
        Ok(())
    }
}
fn c_space(b: u8) -> bool {
    b.is_ascii_whitespace() || b == 0x0b
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)] // These containers deliberately hold ranges, not their members.
mod tests {
    use super::*;
    fn set(data: &[u8], ranges: Vec<Range<usize>>) -> SelectionSet {
        let selections = ranges
            .into_iter()
            .map(|r| Selection::from_range(data, r, Direction::Forward).unwrap())
            .collect();
        SelectionSet::new(data, selections, 0).unwrap()
    }
    #[test]
    fn native_creation_keeps_raw_bytes_and_operator_normalization_keeps_order() {
        let data = b"\xffone\n\x80two\n";
        let mut set = SelectionSet::new(
            data,
            vec![Selection {
                anchor: 0,
                cursor: 0,
            }],
            0,
        )
        .unwrap();
        let id = set.insert_point_native(data, 5).unwrap().unwrap();
        assert_eq!(set.primary().cursor, 5);
        set.normalize_native(data).unwrap();
        assert_eq!(
            set.primary(),
            Selection {
                anchor: 5,
                cursor: 4
            }
        );
        assert_eq!(set.primary_id(), id);
        let data = b"abcdefghijk";
        let mut set = SelectionSet::new(
            data,
            vec![
                Selection {
                    anchor: 0,
                    cursor: 0,
                },
                Selection {
                    anchor: 2,
                    cursor: 2,
                },
                Selection {
                    anchor: 4,
                    cursor: 4,
                },
            ],
            1,
        )
        .unwrap();
        let ids = set.ids().to_vec();
        set.update(
            data,
            ids[1],
            Selection {
                anchor: 8,
                cursor: 8,
            },
        )
        .unwrap();
        set.normalize_native(data).unwrap();
        assert_eq!(
            set.selections()
                .iter()
                .map(|s| s.cursor)
                .collect::<Vec<_>>(),
            vec![0, 8, 4]
        );
        assert_eq!(set.ids(), ids);
        assert_eq!(set.primary_id(), ids[1]);
    }
    #[test]
    fn orientation_and_combining_endpoints() {
        let d = "a\u{301}中z".as_bytes();
        let mut s = Selection::from_range(d, 0..6, Direction::Backward).unwrap();
        assert_eq!(
            s,
            Selection {
                anchor: 3,
                cursor: 0
            }
        );
        assert_eq!(s.range(d).unwrap(), 0..6);
        s.flip();
        assert_eq!(s.direction(), Direction::Forward);
        assert_eq!(s.range(d).unwrap(), 0..6);
        assert!(Selection::from_range(d, 0..2, Direction::Forward).is_err());
        assert_eq!(
            Selection::from_range(d, 6..6, Direction::Forward)
                .unwrap()
                .range(d)
                .unwrap(),
            6..7
        );
        assert_eq!(
            Selection::from_range(d, 7..7, Direction::Backward)
                .unwrap()
                .range(d)
                .unwrap(),
            7..7
        );
    }
    #[test]
    fn strict_overlap_and_adjacency_match_fixtures() {
        let d = include_bytes!("../../test/vis/selections/union-adjacent.in");
        let mut odd = Vec::new();
        let mut even = Vec::new();
        let mut p = 0;
        let mut n = 0;
        while p < d.len() {
            let end = motion::line_next(d, p);
            if n % 2 == 0 {
                odd.push(p..end);
            } else {
                even.push(p..end);
            }
            p = end;
            n += 1;
        }
        assert_eq!(union(&odd, &even, d.len()).unwrap().len(), 10);
        assert!(intersection(&odd, &even, d.len()).unwrap().is_empty());
        assert_eq!(minus(&odd, &even, d.len()).unwrap(), odd);
        assert_eq!(union(&[0..4], &[2..6], 8).unwrap(), vec![0..6]);
        assert_eq!(intersection(&[0..4], &[2..6], 8).unwrap(), vec![2..4]);
        assert_eq!(minus(&[0..8], &[2..6], 8).unwrap(), vec![0..2, 6..8]);
    }
    #[test]
    fn empty_and_whole_algebra_preserves_live_cursor() {
        let d = b"abcdef";
        let mut s = set(d, vec![1..3]);
        s.intersect_with(d, &[4..6]).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s.primary().anchor, s.primary().cursor);
        assert_eq!(complement(&[0..6], 6).unwrap(), vec![]);
        assert_eq!(complement(&[], 6).unwrap(), vec![0..6]);
        assert_eq!(union(&[0..6], &[], 6).unwrap(), vec![0..6]);
        assert_eq!(intersection(&[0..6], &[], 6).unwrap(), vec![]);
        assert_eq!(
            SelectionSet::single(b"", 0).unwrap().ranges(b"").unwrap(),
            vec![0..0]
        );
    }
    #[test]
    fn normalize_preserves_orientation_and_primary_component() {
        let d = b"abcdefghij";
        let s = SelectionSet::new(
            d,
            vec![
                Selection {
                    anchor: 5,
                    cursor: 2,
                },
                Selection {
                    anchor: 0,
                    cursor: 3,
                },
                Selection {
                    anchor: 8,
                    cursor: 9,
                },
            ],
            0,
        )
        .unwrap();
        assert_eq!(s.ranges(d).unwrap(), vec![0..6, 8..10]);
        assert_eq!(s.primary_index(), 0);
        assert_eq!(s.primary().direction(), Direction::Forward);
        let mut s = s;
        s.set_primary(1).unwrap();
        s.navigate(1);
        assert_eq!(s.primary_index(), 0);
        s.navigate(-1);
        assert_eq!(s.primary_index(), 1);
    }
    #[test]
    fn add_remove_and_primary_identity() {
        let d = b"abcdef";
        let mut s = SelectionSet::single(d, 4).unwrap();
        s.add(
            d,
            Selection {
                anchor: 0,
                cursor: 1,
            },
            true,
        )
        .unwrap();
        assert_eq!(s.primary_index(), 0);
        assert_eq!(s.ranges(d).unwrap(), vec![0..2, 4..5]);
        s.replace_primary(
            d,
            Selection {
                anchor: 3,
                cursor: 0,
            },
        )
        .unwrap();
        assert_eq!(s.primary().direction(), Direction::Backward);
        assert!(s.remove_primary());
        assert_eq!(s.primary().cursor, 4);
        assert!(!s.remove_primary());
        s.keep_primary();
        assert_eq!(s.len(), 1);
    }
    #[test]
    fn invalid_changes_are_atomic() {
        let d = b"abc";
        let mut s = set(d, vec![0..2]);
        let before = s.clone();
        assert!(s.set_ranges(d, vec![2..4]).is_err());
        assert_eq!(s, before);
        assert!(
            s.transform(d, |_| Selection {
                anchor: 9,
                cursor: 0
            })
            .is_err()
        );
        assert_eq!(s, before);
        assert!(union(&[Range { start: 2, end: 1 }], &[], 3).is_err());
        assert!(SelectionSet::new(d, vec![], 0).is_err());
        assert!(s.set_primary(2).is_err());
        assert_eq!(s, before);
    }
    #[test]
    fn ordinal_columns_parity_and_trim() {
        let d = b" a b \n c d \n e \n";
        let all = vec![1..2, 3..4, 7..8, 9..10, 13..14];
        let mut s = set(d, all.clone());
        assert_eq!(s.column_count(d).unwrap(), 2);
        s.remove_column(d, 1).unwrap();
        assert_eq!(s.ranges(d).unwrap(), vec![1..2, 7..8, 13..14]);
        let mut s = set(d, all.clone());
        s.keep_column(d, 1).unwrap();
        assert_eq!(s.ranges(d).unwrap(), vec![3..4, 9..10]);
        let mut s = set(d, all);
        s.keep_parity(Parity::Even);
        assert_eq!(s.ranges(d).unwrap(), vec![3..4, 9..10]);
        let mut s = set(d, vec![0..5, 6..11]);
        assert!(s.trim(d).unwrap());
        assert_eq!(s.ranges(d).unwrap(), vec![1..4, 7..10]);
        let mut s = set(d, vec![4..6]);
        assert!(!s.trim(d).unwrap());
        assert_eq!(s.len(), 1);
    }
    #[test]
    fn rotate_per_line_matches_fixture() {
        let input = include_bytes!("../../test/vis/selections/selections-rotate.in");
        let expected = include_bytes!("../../test/vis/selections/selections-rotate.ref");
        let mut text = Text::from_bytes(input);
        let mut s = set(
            input,
            vec![0..1, 2..3, 4..5, 6..7, 8..9, 10..11, 12..13, 14..15, 16..17],
        );
        s.rotate_contents(&mut text, -1).unwrap();
        assert_eq!(text.to_vec(), expected);
        s.rotate_contents(&mut text, 1).unwrap();
        assert_eq!(text.to_vec(), input);
    }
    #[test]
    fn rotation_different_lengths_and_marks_restore() {
        let d = b"one two longer";
        let mut text = Text::from_bytes(d);
        let mut s = set(d, vec![0..3, 4..7, 8..14]);
        let marks = s.primary().save(&text).unwrap();
        text.snapshot();
        s.rotate_contents(&mut text, 1).unwrap();
        assert_eq!(text.to_vec(), b"longer one two");
        assert_eq!(s.ranges(&text.to_vec()).unwrap(), vec![0..6, 7..10, 11..14]);
        assert!(marks.restore(&text).is_none());
        text.undo();
        assert_eq!(
            marks.restore(&text),
            Some(Selection {
                anchor: 0,
                cursor: 2
            })
        );
    }
    #[test]
    fn rotation_preserves_unselected_marks_and_noop_preserves_all_marks() {
        let d = b"a gap longer";
        let mut text = Text::from_bytes(d);
        let gap = text.mark(2).unwrap();
        let selected = text.mark(0).unwrap();
        let mut s = set(d, vec![0..1, 6..12]);
        s.rotate_contents(&mut text, 0).unwrap();
        assert_eq!(text.mark_get(selected), Some(0));
        s.rotate_contents(&mut text, 1).unwrap();
        assert_eq!(text.to_vec(), b"longer gap a");
        assert_eq!(text.mark_get(gap), Some(7));
    }
    #[test]
    fn randomized_algebra_matches_byte_membership() {
        let mut seed = 7u32;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 16) as usize
        };
        for _ in 0..1000 {
            let len = 32;
            let mut a = Vec::new();
            let mut b = Vec::new();
            for ranges in [&mut a, &mut b] {
                for _ in 0..next() % 8 {
                    let x = next() % (len + 1);
                    let y = next() % (len + 1);
                    ranges.push(x.min(y)..x.max(y));
                }
            }
            let u = union(&a, &b, len).unwrap();
            let i = intersection(&a, &b, len).unwrap();
            let m = minus(&a, &b, len).unwrap();
            let c = complement(&a, len).unwrap();
            for p in 0..len {
                let contains = |ranges: &[Range<usize>]| ranges.iter().any(|r| r.contains(&p));
                let x = contains(&a);
                let y = contains(&b);
                assert_eq!(contains(&u), x || y);
                assert_eq!(contains(&i), x && y);
                assert_eq!(contains(&m), x && !y);
                assert_eq!(contains(&c), !x);
            }
        }
    }
    #[test]
    fn identities_follow_objects_through_sort_merge_trim_and_removal() {
        let data = b" ab cd ";
        let mut selections = SelectionSet::new(
            data,
            vec![
                Selection {
                    anchor: 4,
                    cursor: 5,
                },
                Selection {
                    anchor: 0,
                    cursor: 2,
                },
            ],
            0,
        )
        .unwrap();
        let ids = selections.ids().to_vec();
        assert!(selections.trim(data).unwrap());
        assert_eq!(selections.ids(), ids);
        selections
            .update(
                data,
                ids[1],
                Selection {
                    anchor: 1,
                    cursor: 1,
                },
            )
            .unwrap();
        assert_eq!(selections.ids(), ids); // direct setters preserve overlapping objects
        selections.normalize(data).unwrap();
        assert_eq!(selections.ids(), &[ids[1]]);
        let new = selections
            .push(
                data,
                Selection {
                    anchor: 6,
                    cursor: 6,
                },
                true,
            )
            .unwrap();
        assert_ne!(new, ids[1]);
        assert!(selections.remove(new).unwrap());
        assert!(selections.get(new).is_none());
    }
    #[test]
    fn identified_restore_rejects_duplicate_ids_atomically() {
        let data = b"ab";
        let mut selections = SelectionSet::single(data, 0).unwrap();
        let id = selections.primary_id();
        let before = selections.clone();
        assert!(
            selections
                .replace_selections(
                    data,
                    vec![Selection {
                        anchor: 99,
                        cursor: 99
                    }],
                    0
                )
                .is_err()
        );
        assert_eq!(selections, before);
        assert!(
            SelectionSet::from_identified(
                data,
                vec![
                    (
                        id,
                        Selection {
                            anchor: 0,
                            cursor: 0
                        }
                    ),
                    (
                        id,
                        Selection {
                            anchor: 1,
                            cursor: 1
                        }
                    )
                ],
                id
            )
            .is_err()
        );
    }
}
