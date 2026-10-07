//! Keeping a list's selection in view, with room to see what comes next.
//!
//! A list that scrolls only when its selection reaches the edge keeps the
//! selection pinned to that edge while walking, and whatever comes next is
//! out of sight until it is already selected. Every list in the studio keeps
//! a margin instead - vim's `scrolloff` - so the rows ahead of the selection
//! are on screen before the selection gets to them.
//!
//! The margin belongs to the keyboard and the wheel. A click selects the row
//! under the pointer and leaves the list where it is: a list that scrolled
//! under a click would move the row away from the pointer that chose it, and
//! a second click would land on another one. Callers that know the selection
//! came from a pointer pass a margin of zero, which is minimal scrolling.

/// The most rows a list keeps between its selection and an edge.
pub const MAX_MARGIN: usize = 2;

/// The margin a list `height` rows tall keeps: about a third of what it
/// shows, never more than [`MAX_MARGIN`]. Five rows keep one and two rows
/// keep none: a margin taking most of a short list would leave the selection
/// nowhere to move without moving the whole list.
pub fn margin(height: usize) -> usize {
    (height / 3).min(MAX_MARGIN)
}

/// The first row to show, of `total` rows through a window `height` rows
/// tall, so that `selected` sits at least `margin` rows inside either edge.
///
/// The window moves from `first` no further than that takes, so walking
/// through the middle of a list leaves it still. It never scrolls past
/// either end: at the top and the bottom of the list the selection may sit
/// on the edge itself, since there is nothing beyond it to show.
pub fn follow(first: usize, selected: usize, height: usize, total: usize, margin: usize) -> usize {
    follow_choices(first, selected, height, total, margin, |_| true)
}

/// [`follow`], with the margin counted in choices rather than rows.
///
/// A list whose choices are separated by rows nobody can select - a group's
/// title and the blank line above it, a menu's separator, a heading - would
/// spend a margin of rows on those and still hide the next choice. Here the
/// margin reaches `margin` choices past the selection each way, however many
/// rows lie between, or the end of the list when fewer are left: at the end
/// of a group of settings, the next group's title and its first two rows
/// come into sight together. It never takes so many rows that the selection
/// could leave the window.
pub fn follow_choices(
    first: usize,
    selected: usize,
    height: usize,
    total: usize,
    margin: usize,
    is_choice: impl Fn(usize) -> bool,
) -> usize {
    let height = height.max(1);
    // Never more margin either way than leaves one row between the two.
    let cap = (height - 1) / 2;
    let below = reach(selected, total, margin, cap, true, &is_choice);
    let above = reach(selected, total, margin, cap, false, &is_choice);
    let mut first = first;
    if selected < first + above {
        first = selected.saturating_sub(above);
    }
    if selected + below >= first + height {
        first = selected + below + 1 - height;
    }
    first.min(total.saturating_sub(height))
}

/// How many rows from `selected` to the `margin`th choice past it, one way:
/// to the end of the list when fewer choices are left, and no more than
/// `cap` whatever lies beyond.
fn reach(
    selected: usize,
    total: usize,
    margin: usize,
    cap: usize,
    forwards: bool,
    is_choice: &impl Fn(usize) -> bool,
) -> usize {
    let mut choices = 0;
    let mut distance = 0;
    while choices < margin && distance < cap {
        let row = if forwards {
            match selected.checked_add(distance + 1) {
                Some(row) if row < total => row,
                _ => break,
            }
        } else {
            match selected.checked_sub(distance + 1) {
                Some(row) => row,
                None => break,
            }
        };
        distance += 1;
        if is_choice(row) {
            choices += 1;
        }
    }
    distance
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_margin_grows_with_the_window_up_to_two_rows() {
        let margins: Vec<usize> = (0..=10).map(margin).collect();
        assert_eq!(margins, [0, 0, 0, 1, 1, 1, 2, 2, 2, 2, 2]);
    }

    /// Walking down a long list: the window stays still until the selection
    /// is two rows from the bottom, then moves a row a step, keeping two rows
    /// of what comes next in sight.
    #[test]
    fn walking_down_scrolls_before_the_bottom_edge() {
        let (height, total) = (10, 100);
        let mut first = 0;
        let mut seen = Vec::new();
        for selected in 0..12 {
            first = follow(first, selected, height, total, margin(height));
            seen.push(first);
        }
        assert_eq!(seen, [0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4]);
        // The selection is never closer than two rows to the bottom edge.
        assert_eq!(first + height - 1 - 11, 2);
    }

    /// Walking back up: nothing moves until the selection is two rows from
    /// the top, then the window follows it up.
    #[test]
    fn walking_up_scrolls_before_the_top_edge() {
        let (height, total) = (10, 100);
        let mut first = 40;
        let mut seen = Vec::new();
        for selected in (38..=45).rev() {
            first = follow(first, selected, height, total, margin(height));
            seen.push((selected, first));
        }
        assert_eq!(
            seen,
            [
                (45, 40),
                (44, 40),
                (43, 40),
                (42, 40),
                (41, 39),
                (40, 38),
                (39, 37),
                (38, 36)
            ]
        );
    }

    /// At the ends of the list there is nothing to keep in view, so the
    /// selection may reach the edge, and the window never shows past the end.
    #[test]
    fn the_ends_of_the_list_let_the_selection_reach_the_edge() {
        assert_eq!(follow(0, 0, 10, 100, 2), 0, "the first row at the top");
        assert_eq!(follow(0, 1, 10, 100, 2), 0);
        assert_eq!(follow(0, 99, 10, 100, 2), 90, "the last row at the bottom");
        assert_eq!(follow(90, 98, 10, 100, 2), 90);
        assert_eq!(follow(0, 5, 10, 8, 2), 0, "a list shorter than the window");
        assert_eq!(follow(50, 3, 10, 20, 2), 1, "a jump to the top");
    }

    /// A margin of zero is minimal scrolling: what a pointer's selection
    /// uses, so a clicked row is never moved out from under it.
    #[test]
    fn no_margin_moves_only_when_the_selection_leaves_the_window() {
        assert_eq!(
            follow(0, 9, 10, 100, 0),
            0,
            "the bottom row is still inside"
        );
        assert_eq!(follow(0, 10, 10, 100, 0), 1);
        assert_eq!(follow(5, 5, 10, 100, 0), 5, "the top row is still inside");
        assert_eq!(follow(5, 4, 10, 100, 0), 4);
    }

    /// Rows nobody can select do not use up the margin: past a run of them
    /// the next two choices come into sight together, where a margin of rows
    /// would have shown only the run.
    #[test]
    fn the_margin_counts_choices_past_rows_that_are_not() {
        // Choices on every row but 7 and 8: a group's end, its blank line
        // and the next group's title, say.
        let is_choice = |row: usize| row != 7 && row != 8;
        let (height, total) = (9, 20);
        assert_eq!(
            follow(0, 6, height, total, 2),
            0,
            "rows: only the run is in sight"
        );
        let first = follow_choices(0, 6, height, total, 2, is_choice);
        assert_eq!(first, 2, "choices: rows 9 and 10 are in sight below row 6");
        assert!(first + height > 10);
        // And the same upward, from below the run.
        let first = follow_choices(9, 9, height, total, 2, is_choice);
        assert_eq!(first, 5, "rows 6 and 5 are in sight above row 9");
        // It is still a fixed point, so drawing and hit-testing agree.
        assert_eq!(follow_choices(first, 9, height, total, 2, is_choice), first);
        // A window too short for the whole run keeps the selection in it.
        let first = follow_choices(0, 6, 5, total, 2, is_choice);
        assert!(6 >= first && 6 < first + 5);
    }

    /// A margin too big for the window shrinks to what leaves the selection
    /// a row to stand on, and a window of nothing is treated as one row.
    #[test]
    fn a_small_window_shrinks_the_margin() {
        assert_eq!(
            follow(0, 1, 3, 10, 2),
            0,
            "three rows keep one row each side"
        );
        assert_eq!(follow(0, 2, 3, 10, 2), 1);
        assert_eq!(follow(0, 4, 0, 10, 2), 4, "a zero height is one row");
    }
}
