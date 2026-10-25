//! The per-window 32-slot selection jump cache from `vis-marks.c`.
//!
//! This is a ring, not a stack with clamped ends. Its unsigned cursor moves even
//! across unused or invalid entries. Saving compares only the current slot's
//! restored geometry; equal geometry preserves both that slot's mode and cursor.
//! Regions retain persistent text marks, so deletion invalidates them until undo.
//!
//! The host owns mode transitions and selection application. A target contains
//! ranges and a mode, never saved orientation or a primary selection: native
//! `view_selections_set_all` preserves existing per-index orientation, creates
//! additional selections forward, and makes the first selection primary. Switch
//! mode through ordinary hooks before applying ranges; leaving visual mode may
//! reentrantly save another entry. Targets therefore own their data.

use crate::motion;
use crate::text::{Mark, Text};
use std::num::NonZeroIsize;
use std::ops::Range;

pub const CAPACITY: usize = 32;

#[derive(Clone, Debug)]
struct Region {
    anchor: Mark,
    cursor: Mark,
}

impl Region {
    fn save(text: &Text, data: &[u8], range: &Range<usize>) -> Option<Self> {
        // view_regions_save rejects EOF even though an EOF cursor is valid.
        if range.start > range.end || range.start >= text.len() {
            return None;
        }
        let mut end = range.end.min(text.len());
        if range.start != end {
            end = motion::char_prev(data, end);
        }
        Some(Self {
            anchor: text.mark(range.start)?,
            cursor: text.mark(end)?,
        })
    }

    fn restore(&self, text: &Text, data: &[u8]) -> Option<Range<usize>> {
        let anchor = text.mark_get(self.anchor.clone())?;
        let cursor = text.mark_get(self.cursor.clone())?;
        Some(anchor.min(cursor)..motion::char_next(data, anchor.max(cursor)))
    }
}

#[derive(Clone, Debug)]
struct Entry<M> {
    regions: Vec<Region>,
    mode: M,
}

impl<M> Entry<M> {
    fn ranges(&self, text: &Text, data: &[u8]) -> Vec<Range<usize>> {
        let mut ranges = self
            .regions
            .iter()
            .filter_map(|region| region.restore(text, data))
            .collect::<Vec<_>>();
        normalize_native(&mut ranges);
        ranges
    }
}

/// A requested host transition. Empty ranges are significant: stored marks can
/// all be invalid, while their nonempty slot still requests a mode transition
/// and native empty-range selection application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JumpTarget<M> {
    pub mode: M,
    pub ranges: Vec<Range<usize>>,
}

#[derive(Clone, Debug)]
pub struct JumpList<M> {
    cursor: usize,
    entries: [Option<Entry<M>>; CAPACITY],
}

impl<M> Default for JumpList<M> {
    fn default() -> Self {
        Self {
            cursor: 0,
            entries: std::array::from_fn(|_| None),
        }
    }
}

impl<M: Clone> JumpList<M> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Native unsigned cursor, intentionally not reduced to the ring capacity.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Save ordered live ranges in the current slot. Returns whether the slot
    /// was written. EOF-only or invalid geometry still advances the cursor,
    /// even when it produces an empty slot.
    pub fn save(&mut self, text: &Text, current: &[Range<usize>], mode: M) -> bool {
        let data = text.to_vec();
        let index = self.cursor % CAPACITY;
        let unchanged = self.entries[index]
            .as_ref()
            .filter(|entry| !entry.regions.is_empty())
            .is_some_and(|entry| entry.ranges(text, &data) == current);
        if unchanged {
            return false;
        }
        self.entries[index] = Some(Entry {
            regions: current
                .iter()
                .filter_map(|range| Region::save(text, &data, range))
                .collect(),
            mode,
        });
        self.cursor = self.cursor.wrapping_add(1);
        true
    }

    pub fn older(&mut self, text: &Text, current: &[Range<usize>]) -> Option<JumpTarget<M>> {
        self.navigate(
            text,
            current,
            NonZeroIsize::new(-1).expect("minus one is nonzero"),
        )
    }

    pub fn newer(&mut self, text: &Text, current: &[Range<usize>]) -> Option<JumpTarget<M>> {
        self.navigate(text, current, NonZeroIsize::new(1).expect("one is nonzero"))
    }

    /// Move exactly once, including across empty slots. Negative traversal
    /// examines the new cursor; positive traversal examines the old cursor.
    /// The three native actions use -1, 0 (save), and +1 without count scaling.
    pub fn navigate(
        &mut self,
        text: &Text,
        current: &[Range<usize>],
        advance: NonZeroIsize,
    ) -> Option<JumpTarget<M>> {
        let previous = self.cursor;
        self.cursor = self.cursor.wrapping_add_signed(advance.get());
        let index = if advance.get() < 0 {
            self.cursor
        } else {
            previous
        } % CAPACITY;
        let entry = self.entries[index].as_ref()?;
        if entry.regions.is_empty() {
            return None;
        }
        let ranges = entry.ranges(text, &text.to_vec());
        (ranges != current).then(|| JumpTarget {
            mode: entry.mode.clone(),
            ranges,
        })
    }
}

/// Preserve vis_mark_normalize's observable loop behavior, rather than using
/// the editor's general range union. Its inner and outer increments skip the
/// item after a non-overlap; a union only updates the local previous range.
/// In particular, three identical saved ranges restore as two, not one.
fn normalize_native(ranges: &mut Vec<Range<usize>>) {
    let mut index = 0;
    while index < ranges.len() {
        if ranges[index].is_empty() {
            ranges.swap_remove(index);
        }
        index += 1;
    }
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut previous: Option<Range<usize>> = None;
    let mut index = 0;
    while index < ranges.len() {
        let range = ranges[index].clone();
        if let Some(last) = previous
            .as_mut()
            .filter(|last| last.start < range.end && range.start < last.end)
        {
            last.start = last.start.min(range.start);
            last.end = last.end.max(range.end);
            ranges.remove(index);
        } else {
            previous = Some(range);
            index += 1;
        }
        index += 1;
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)] // Selection snapshots contain ranges, not integer sequences.
mod tests {
    use super::*;

    fn text() -> Text {
        Text::from_bytes(b"abcdefghijklmnopqrstuvwxyz0123456789")
    }

    #[test]
    fn empty_boundaries_move_unsigned_cursor_and_wrap() {
        let text = text();
        let mut list = JumpList::<u8>::new();
        assert!(list.older(&text, &[7..8]).is_none());
        assert_eq!(list.cursor(), usize::MAX);
        assert!(list.newer(&text, &[7..8]).is_none());
        assert_eq!(list.cursor(), 0);
        for _ in 0..CAPACITY {
            assert!(list.older(&text, &[7..8]).is_none());
        }
        assert!(list.save(&text, &[7..8], 0));
        assert_eq!(list.older(&text, &[9..10]).unwrap().ranges, vec![7..8]);
    }

    #[test]
    fn adjacent_duplicates_are_saved_but_equal_target_geometry_does_not_replace_mode() {
        let text = text();
        let mut list = JumpList::new();
        assert!(list.save(&text, &[2..3], 0));
        assert!(list.save(&text, &[2..3], 0));
        assert_eq!(list.cursor(), 2);
        assert!(list.older(&text, &[2..3]).is_none());
        assert!(list.older(&text, &[2..3]).is_none());
        assert_eq!(list.cursor(), 0);
        assert!(!list.save(&text, &[2..3], 2));
        assert_eq!(list.cursor(), 0);
        assert_eq!(
            list.newer(&text, &[8..9]).unwrap(),
            JumpTarget {
                mode: 0,
                ranges: vec![2..3]
            }
        );
    }

    #[test]
    fn first_previous_and_next_can_visit_equal_slots_without_moving_selection() {
        let text = text();
        let mut list = JumpList::new();
        list.save(&text, &[0..1], 0);
        list.save(&text, &[5..6], 0);
        assert!(list.older(&text, &[5..6]).is_none());
        assert_eq!(list.older(&text, &[5..6]).unwrap().ranges, vec![0..1]);
        assert!(list.newer(&text, &[0..1]).is_none());
        assert_eq!(list.newer(&text, &[0..1]).unwrap().ranges, vec![5..6]);
        assert!(list.newer(&text, &[5..6]).is_none());
        assert_eq!(list.cursor(), 3);
    }

    #[test]
    fn full_ring_wraps_instead_of_clamping_to_oldest() {
        let text = text();
        let mut list = JumpList::new();
        for p in 0..=CAPACITY {
            list.save(&text, &[p..p + 1], 0);
        }
        assert!(list.older(&text, &[32..33]).is_none());
        let mut current = vec![32..33];
        for p in (1..CAPACITY).rev() {
            current = list.older(&text, &current).unwrap().ranges;
            assert_eq!(current, vec![p..p + 1]);
        }
        assert_eq!(list.older(&text, &current).unwrap().ranges, vec![32..33]);
    }

    #[test]
    fn eof_saves_consume_an_empty_slot() {
        let text = text();
        let mut list = JumpList::new();
        list.save(&text, &[1..2], 0);
        list.save(&text, &[text.len()..text.len()], 2);
        list.save(&text, &[2..3], 0);
        assert_eq!(list.cursor(), 3);
        assert!(list.older(&text, &[2..3]).is_none());
        assert!(list.older(&text, &[2..3]).is_none());
        assert_eq!(list.older(&text, &[2..3]).unwrap().ranges, vec![1..2]);
    }

    #[test]
    fn marks_shift_with_edits_and_deleted_entries_revive_after_undo() {
        let mut text = text();
        let mut list = JumpList::new();
        list.save(&text, &[3..4], 2);
        text.snapshot();
        text.insert(0, b"++").unwrap();
        text.snapshot();
        assert_eq!(list.older(&text, &[8..9]).unwrap().ranges, vec![5..6]);
        text.undo().unwrap();
        assert_eq!(list.newer(&text, &[8..9]).unwrap().ranges, vec![3..4]);
        text.delete(3..4).unwrap();
        text.snapshot();
        assert_eq!(
            list.older(&text, &[8..9]).unwrap(),
            JumpTarget {
                mode: 2,
                ranges: vec![]
            }
        );
        text.undo().unwrap();
        assert_eq!(list.newer(&text, &[8..9]).unwrap().ranges, vec![3..4]);
    }

    #[test]
    fn foreign_document_marks_are_invalid_without_destroying_the_entry() {
        let text = text();
        let other = Text::from_bytes(b"same positions are another document");
        let mut list = JumpList::new();
        list.save(&text, &[3..4], 2);
        assert!(list.older(&other, &[8..9]).unwrap().ranges.is_empty());
        assert_eq!(list.newer(&text, &[8..9]).unwrap().ranges, vec![3..4]);
    }

    #[test]
    fn native_duplicate_normalization_keeps_two_of_three_identical_regions() {
        let text = text();
        let mut list = JumpList::new();
        list.save(&text, &[0..1, 0..1, 0..1], 2);
        assert_eq!(
            list.older(&text, &[0..1, 0..1, 0..1]).unwrap().ranges,
            vec![0..1, 0..1]
        );
    }

    #[test]
    fn returned_geometry_is_sorted_but_live_geometry_comparison_is_ordered() {
        let text = text();
        let mut list = JumpList::new();
        list.save(&text, &[4..5, 0..1], 2);
        assert_eq!(
            list.older(&text, &[4..5, 0..1]).unwrap().ranges,
            vec![0..1, 4..5]
        );
    }

    #[test]
    fn target_owns_mode_and_ranges_before_reentrant_transition_save() {
        let text = text();
        let mut list = JumpList::new();
        list.save(&text, &[2..3], 0);
        list.older(&text, &[2..3]);
        let target = list.newer(&text, &[2..9]).unwrap();
        // The host's visual-leave hook saves before applying this old target.
        list.save(&text, &[2..9], 2);
        assert_eq!(
            target,
            JumpTarget {
                mode: 0,
                ranges: vec![2..3]
            }
        );
        assert_eq!(
            list.older(&text, &[2..3]).unwrap(),
            JumpTarget {
                mode: 2,
                ranges: vec![2..9]
            }
        );
    }

    #[test]
    fn region_save_clamps_finish_and_expands_empty_ranges_before_eof() {
        let text = Text::from_bytes("aλz".as_bytes());
        let mut list = JumpList::new();
        list.save(&text, &[1..100], 2);
        assert_eq!(list.older(&text, &[0..1]).unwrap().ranges, vec![1..4]);
        list.save(&text, &[1..1], 2);
        assert_eq!(list.older(&text, &[0..1]).unwrap().ranges, vec![1..3]);
    }
}
