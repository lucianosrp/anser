//! The jump list: Ctrl+O goes back, Ctrl+I forward, through where the
//! owner has been (Vim's jump list). A spot is the page plus its cheap view
//! state (app.rs `Spot`); the app records the current spot after every
//! frame, and this keeps the list:
//!
//! * a spot equal to the current one is not a jump (consecutive duplicates
//!   never pile up)
//! * a new spot after going back drops the forward part, then appends
//! * at most [`CAP`] spots; the oldest go first
//! * `same_place` spots replace the current one instead of adding: typing a
//!   search is one jump, not one per key

/// The most spots kept.
pub const CAP: usize = 100;

#[derive(Debug)]
pub struct JumpList<T> {
    spots: Vec<T>,
    /// index of the current spot (when not empty)
    at: usize,
}

impl<T> Default for JumpList<T> {
    fn default() -> Self {
        JumpList { spots: Vec::new(), at: 0 }
    }
}

impl<T: Clone + PartialEq> JumpList<T> {
    /// Where the app is now. `same_place(current, new)`: the new spot only
    /// refines the current one (it replaces it, the list doesn't grow).
    pub fn record(&mut self, spot: T, same_place: impl Fn(&T, &T) -> bool) {
        match self.spots.get(self.at) {
            Some(cur) if *cur == spot => {}
            Some(cur) if same_place(cur, &spot) => {
                self.spots.truncate(self.at + 1);
                self.spots[self.at] = spot;
            }
            Some(_) => {
                self.spots.truncate(self.at + 1);
                self.spots.push(spot);
                if self.spots.len() > CAP {
                    self.spots.drain(..self.spots.len() - CAP);
                }
                self.at = self.spots.len() - 1;
            }
            None => {
                self.spots.push(spot);
                self.at = 0;
            }
        }
    }

    /// Ctrl+O: the previous spot, now current (None at the oldest).
    pub fn back(&mut self) -> Option<T> {
        (self.at > 0 && !self.spots.is_empty()).then(|| {
            self.at -= 1;
            self.spots[self.at].clone()
        })
    }

    /// Ctrl+I: the next spot after going back (None at the newest).
    pub fn forward(&mut self) -> Option<T> {
        (self.at + 1 < self.spots.len()).then(|| {
            self.at += 1;
            self.spots[self.at].clone()
        })
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.spots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never(_: &&str, _: &&str) -> bool {
        false
    }

    fn list(spots: &[&'static str]) -> JumpList<&'static str> {
        let mut j = JumpList::default();
        for s in spots {
            j.record(*s, never);
        }
        j
    }

    #[test]
    fn back_and_forward_walk_the_list() {
        let mut j = list(&["a", "b", "c"]);
        assert_eq!(j.forward(), None);
        assert_eq!(j.back(), Some("b"));
        assert_eq!(j.back(), Some("a"));
        assert_eq!(j.back(), None);
        assert_eq!(j.forward(), Some("b"));
        assert_eq!(j.forward(), Some("c"));
        assert_eq!(j.forward(), None);
    }

    #[test]
    fn going_back_then_somewhere_new_drops_the_forward_part() {
        let mut j = list(&["a", "b", "c"]);
        j.back();
        j.back();
        j.record("d", never);
        assert_eq!(j.len(), 2);
        assert_eq!(j.forward(), None);
        assert_eq!(j.back(), Some("a"));
    }

    #[test]
    fn restoring_a_spot_is_not_a_new_jump() {
        let mut j = list(&["a", "b"]);
        let back = j.back().unwrap();
        // the app applies it, then records where it is: the same spot
        j.record(back, never);
        assert_eq!(j.len(), 2);
        assert_eq!(j.forward(), Some("b"));
    }

    #[test]
    fn consecutive_duplicates_are_dropped() {
        let j = list(&["a", "a", "b", "b", "b", "a"]);
        assert_eq!(j.len(), 3);
    }

    #[test]
    fn capped_oldest_first() {
        let mut j: JumpList<usize> = JumpList::default();
        for i in 0..250 {
            j.record(i, |_, _| false);
        }
        assert_eq!(j.len(), CAP);
        let mut oldest = 249;
        while let Some(s) = j.back() {
            oldest = s;
        }
        assert_eq!(oldest, 250 - CAP);
    }

    #[test]
    fn same_place_replaces_the_current_spot() {
        // (page, query): typing on one page is one jump
        let same = |a: &(u8, &str), b: &(u8, &str)| a.0 == b.0;
        let mut j = JumpList::default();
        j.record((1, ""), same);
        j.record((2, ""), same);
        j.record((2, "c"), same);
        j.record((2, "co"), same);
        j.record((2, "coffee"), same);
        assert_eq!(j.len(), 2);
        assert_eq!(j.back(), Some((1, "")));
        assert_eq!(j.forward(), Some((2, "coffee")));
    }
}
