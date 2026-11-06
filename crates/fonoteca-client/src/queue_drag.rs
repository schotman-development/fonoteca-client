//! Where a row of the sheet's queue list lands when it is dragged, or moved
//! with the keys. Slots count the gaps between rows: slot 0 is above the
//! first row, slot `n` below the last of `n` rows. Row 0 holds the song or
//! work playing, so nothing goes above it.

/// The slot nearest to `at`, in rows from the list's top, that the `from`th
/// row can move to: not inside a work that is open, where `parts` says a row
/// is one of its movements, and not where it already is. None when that
/// would move nothing.
pub fn drop_slot(parts: &[bool], from: usize, at: f32) -> Option<usize> {
    let nearest = (1..=parts.len())
        .filter(|&slot| !is_part(parts, slot))
        .min_by(|&one, &other| distance(one, at).total_cmp(&distance(other, at)))?;
    (nearest != from && nearest != from + 1).then_some(nearest)
}

/// The slot one unit up or down from the `from`th row: above the unit before
/// it, or below the unit after it.
pub fn step_slot(parts: &[bool], from: usize, down: bool) -> Option<usize> {
    if down {
        let after = from + 1;
        let mut slot = after + 1;
        (after < parts.len()).then(|| {
            while is_part(parts, slot) {
                slot += 1;
            }
            slot
        })
    } else {
        let mut slot = from.checked_sub(1)?;
        while slot > 0 && is_part(parts, slot) {
            slot -= 1;
        }
        (slot > 0).then_some(slot)
    }
}

/// How many rows the song playing takes at the list's top: its own, and
/// the movements under it when it is a work that is open.
pub fn playing_rows(parts: &[bool]) -> usize {
    match parts.split_first() {
        Some((_, rest)) => 1 + rest.iter().take_while(|&&part| part).count(),
        None => 0,
    }
}

/// The row to move the `from`th row onto to put it in `slot`: it comes to
/// stand after a row from before it, else before it.
pub fn onto_row(from: i32, slot: i32) -> i32 {
    if slot > from { slot - 1 } else { slot }
}

fn is_part(parts: &[bool], row: usize) -> bool {
    parts.get(row).copied().unwrap_or(false)
}

fn distance(slot: usize, at: f32) -> f32 {
    (slot as f32 - at).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    // A song playing, a song, an open work with three movements, a song.
    const LIST: [bool; 7] = [false, false, false, true, true, true, false];

    #[test]
    fn a_row_dropped_nearest_a_gap_lands_in_it() {
        assert_eq!(drop_slot(&LIST, 6, 2.9), Some(2));
        assert_eq!(drop_slot(&LIST, 1, 6.6), Some(7));
        assert_eq!(drop_slot(&LIST, 6, 1.2), Some(1));
        assert_eq!(
            drop_slot(&LIST, 6, -4.0),
            Some(1),
            "nothing goes above the one playing"
        );
        assert_eq!(drop_slot(&LIST, 1, 40.0), Some(7), "or below the last");
    }

    #[test]
    fn a_row_dropped_where_it_is_moves_nothing() {
        assert_eq!(drop_slot(&LIST, 1, 1.0), None);
        assert_eq!(drop_slot(&LIST, 1, 1.5), None);
        assert_eq!(drop_slot(&LIST, 1, 2.0), None);
        assert_eq!(drop_slot(&[false], 0, 0.5), None);
        assert_eq!(drop_slot(&[], 0, 0.5), None);
    }

    #[test]
    fn a_row_never_lands_inside_an_open_work() {
        // The gaps among the movements go to the ones beside the work.
        assert_eq!(drop_slot(&LIST, 6, 3.0), Some(2));
        assert_eq!(drop_slot(&LIST, 1, 4.4), Some(6));
        assert_eq!(drop_slot(&LIST, 6, 3.6), Some(2));
        assert_eq!(
            drop_slot(&LIST, 6, 5.0),
            None,
            "the gap below the work is its own"
        );
    }

    #[test]
    fn the_keys_step_over_a_whole_unit() {
        assert_eq!(step_slot(&LIST, 1, true), Some(6));
        assert_eq!(step_slot(&[false; 3], 1, true), Some(3));
        assert_eq!(step_slot(&LIST, 6, true), None);
        assert_eq!(step_slot(&LIST, 6, false), Some(2));
        assert_eq!(step_slot(&LIST, 2, false), Some(1));
        assert_eq!(
            step_slot(&LIST, 1, false),
            None,
            "not above the one playing"
        );
        assert_eq!(step_slot(&[false, false], 1, true), None);
    }

    #[test]
    fn the_song_playing_takes_its_row_and_its_open_movements() {
        assert_eq!(playing_rows(&LIST), 1);
        assert_eq!(playing_rows(&[false, true, true, false, true]), 3);
        assert_eq!(playing_rows(&[false]), 1);
        assert_eq!(playing_rows(&[]), 0);
    }

    #[test]
    fn a_slot_below_the_row_is_a_move_onto_the_row_above_it() {
        assert_eq!(onto_row(1, 6), 5);
        assert_eq!(onto_row(6, 2), 2);
        assert_eq!(onto_row(2, 1), 1);
    }
}
