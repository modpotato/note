//! Fits the status row's text into the cells a window has. The row shows a message on the
//! left and a readout on the right, each a run of monospace cells. Zoomed in, the cells run
//! out, and the two must not run into each other.

/// Cells kept clear between the message and the readout.
const GAP: usize = 2;
/// The fewest cells a message gets before a shorter readout is tried. A shorter message than
/// this is kept whole.
const MIN_MESSAGE: usize = 8;
const ELLIPSIS: char = '\u{2026}';

/// The readout to draw, and how many cells the message may fill beside it.
#[derive(Debug, PartialEq, Eq)]
pub struct Fit {
    pub readout: usize,
    pub room: usize,
}

/// Picks the readout to show in a row `cells` wide, beside a message `message_len` characters
/// long. `readout_lens` gives the length of each readout, from most to least detailed.
///
/// The first readout that leaves the message `min(message_len, MIN_MESSAGE)` cells is used, so
/// details go before a message is cut short. If none does, the last one is used.
pub fn fit(message_len: usize, readout_lens: &[usize], cells: usize) -> Fit {
    let need = message_len.min(MIN_MESSAGE);
    let readout = readout_lens
        .iter()
        .position(|&len| room(len, cells).is_some_and(|k| k >= need))
        .unwrap_or(readout_lens.len().saturating_sub(1));
    let len = readout_lens.get(readout).copied().unwrap_or(0);
    Fit {
        readout,
        room: room(len, cells).unwrap_or(0),
    }
}

/// Cuts `message` in place to `room` cells. It is unchanged when it fits; otherwise it keeps
/// its first `room - 1` characters and an ellipsis.
pub fn cut(message: &mut String, room: usize) {
    if message.chars().count() <= room {
        return;
    }
    let end = message
        .char_indices()
        .nth(room.saturating_sub(1))
        .map_or(message.len(), |(i, _)| i);
    message.truncate(end);
    if room > 0 {
        message.push(ELLIPSIS);
    }
}

/// Cells the message may fill beside a readout `len` cells long. The message starts one cell
/// in and stops `GAP` cells short of the readout. None when the readout does not fit, with
/// one cell of margin on the right.
fn room(len: usize, cells: usize) -> Option<usize> {
    let start = cells.checked_sub(len + 1)?;
    Some(start.saturating_sub(GAP + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "Ln 1, Col 1  |  Plain text  |  UTF-8  |  CRLF  |  172%";
    const SHORT: &str = "Ln 1, Col 1  |  172%";
    const ZOOM: &str = "172%";
    const READOUTS: [&str; 3] = [FULL, SHORT, ZOOM];

    /// What is drawn for `message` in a row `cells` wide: the message as cut, and its readout.
    fn shown(message: &str, cells: usize) -> (String, &'static str) {
        let lens = READOUTS.map(|s| s.chars().count());
        let f = fit(message.chars().count(), &lens, cells);
        let mut text = message.to_string();
        cut(&mut text, f.room);
        (text, READOUTS[f.readout])
    }

    /// The readout is inside the row, and the message stops `GAP` cells short of it.
    fn assert_clear(message: &str, readout: &str, cells: usize) {
        let r = readout.chars().count();
        assert!(r < cells, "{readout:?} does not fit in {cells} cells");
        let start = cells - r - 1;
        let m = message.chars().count();
        assert!(
            m == 0 || 1 + m + GAP <= start,
            "{message:?} runs into {readout:?} in {cells} cells"
        );
    }

    #[test]
    fn wide_window_keeps_message_and_full_readout() {
        assert_eq!(shown("Untitled", 200), ("Untitled".to_string(), FULL));
    }

    #[test]
    fn exact_fit_keeps_full_readout() {
        // The message takes cells 1 to 8, the gap is cells 9 and 10, and the 54-cell readout
        // starts at cell 11.
        let (text, readout) = shown("Untitled", 66);
        assert_eq!((text.as_str(), readout), ("Untitled", FULL));
        assert_clear(&text, readout, 66);
    }

    #[test]
    fn one_cell_less_drops_detail_not_the_message() {
        let (text, readout) = shown("Untitled", 65);
        assert_eq!((text.as_str(), readout), ("Untitled", SHORT));
        assert_clear(&text, readout, 65);
    }

    #[test]
    fn long_name_is_cut_with_an_ellipsis() {
        let (text, readout) = shown("a-very-long-name.txt [modified]", 66);
        assert_eq!((text.as_str(), readout), ("a-very-\u{2026}", FULL));
        assert_clear(&text, readout, 66);
    }

    #[test]
    fn short_message_is_not_cut_to_keep_a_detail() {
        // At 62 cells the full readout leaves 4 cells, fewer than "a.txt" needs.
        let (text, readout) = shown("a.txt", 62);
        assert_eq!((text.as_str(), readout), ("a.txt", SHORT));
        assert_clear(&text, readout, 62);
    }

    #[test]
    fn narrow_window_keeps_zoom_and_message() {
        // The position no longer fits beside the message; the zoom alone leaves 12 cells.
        let (text, readout) = shown("Untitled", 20);
        assert_eq!((text.as_str(), readout), ("Untitled", ZOOM));
        assert_clear(&text, readout, 20);
    }

    #[test]
    fn message_is_dropped_before_the_zoom_is_clipped() {
        // At 8 cells the zoom starts at cell 3, which leaves no room for the message.
        let (text, readout) = shown("Untitled", 8);
        assert_eq!((text.as_str(), readout), ("", ZOOM));
        assert_clear(&text, readout, 8);
    }

    #[test]
    fn readout_wider_than_the_window_is_the_last_resort() {
        assert_eq!(shown("Untitled", 3), (String::new(), ZOOM));
    }

    #[test]
    fn empty_message_needs_only_the_readout_to_fit() {
        assert_eq!(shown("", 54), (String::new(), SHORT));
        assert_eq!(shown("", 55), (String::new(), FULL));
    }

    #[test]
    fn cut_counts_characters_and_keeps_the_ellipsis_in_the_cell_count() {
        let cut_to = |s: &str, room: usize| {
            let mut text = s.to_string();
            cut(&mut text, room);
            text
        };
        assert_eq!(cut_to("héllo wörld", 11), "héllo wörld");
        assert_eq!(cut_to("héllo wörld", 5), "héll\u{2026}");
        assert_eq!(cut_to("héllo", 1), "\u{2026}");
        assert_eq!(cut_to("héllo", 0), "");
    }

    #[test]
    fn never_overlaps_or_clips_at_any_width() {
        let lens = READOUTS.map(|s| s.chars().count());
        let messages = [
            "",
            "x",
            "Untitled",
            "a-very-long-name.txt [modified]  [Mark Set]",
        ];
        for cells in 0..=300 {
            for message in messages {
                let (text, readout) = shown(message, cells);
                if readout.chars().count() < cells {
                    assert_clear(&text, readout, cells);
                }
                // Every readout more detailed than the one shown must leave the message less
                // than its minimum, or it would have been shown instead.
                let need = message.chars().count().min(MIN_MESSAGE);
                let idx = READOUTS
                    .iter()
                    .position(|&r| r == readout)
                    .expect("a readout is returned");
                for &len in &lens[..idx] {
                    assert!(
                        !room(len, cells).is_some_and(|k| k >= need),
                        "a readout of {len} cells would have fit {message:?} at {cells} cells"
                    );
                }
            }
        }
    }
}
