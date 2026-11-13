//! Pointer state of the slider being dragged, and the debounce rules, without any GPUI types so they
//! can be tested.
//!
//! The thumb follows the pointer at once; what goes to the hardware is delayed until the pointer has
//! been still for a moment (so a fast drag is one write, not dozens of EC transactions), and the final
//! position is always sent on release. After the release the thumb stays where it was let go until
//! the last job has landed, so it does not jump back to the old value in between.

use std::time::Duration;

/// How long the pointer must be still before the position goes to the hardware.
pub const DEBOUNCE: Duration = Duration::from_millis(120);

pub type Pos = (&'static str, usize);

#[derive(Default)]
pub struct SliderDrag {
    /// `(slider id, position the thumb shows)`; kept after release until the last job lands.
    pub thumb: Option<Pos>,
    /// Is the pointer down on a slider?
    pub down: bool,
    /// What the hardware was last told during this drag.
    sent: Option<Pos>,
    generation: u64,
    jobs: usize,
}

impl SliderDrag {
    /// Pointer down on `id`, whose hardware value is `hw_value`, at position `at`. Returns the
    /// generation a debounce timer must carry.
    pub fn press(&mut self, id: &'static str, hw_value: usize, at: usize) -> u64 {
        self.down = true;
        self.sent = Some((id, hw_value));
        self.moved(id, at)
    }

    /// The thumb moved to `at`. Returns the generation a debounce timer must carry; every move makes
    /// the earlier timers stale.
    pub fn moved(&mut self, id: &'static str, at: usize) -> u64 {
        self.thumb = Some((id, at));
        self.generation += 1;
        self.generation
    }

    /// A debounce timer carrying `generation` fired: the position to send, unless the pointer moved
    /// again since or the hardware already has it.
    pub fn due(&mut self, generation: u64) -> Option<Pos> {
        if generation == self.generation {
            self.take_unsent()
        } else {
            None
        }
    }

    /// Pointer up: the final position to send, unless the hardware already has it.
    pub fn release(&mut self) -> Option<Pos> {
        if !self.down {
            return None;
        }
        self.down = false;
        self.generation += 1; // a pending timer is stale now
        self.take_unsent()
    }

    pub fn job_started(&mut self) {
        self.jobs += 1;
    }

    pub fn job_done(&mut self) {
        self.jobs = self.jobs.saturating_sub(1);
        self.settle();
    }

    /// Let go of the thumb once the pointer is up and no job is left. Returns whether it did.
    pub fn settle(&mut self) -> bool {
        let idle = !self.down && self.jobs == 0;
        if idle {
            self.thumb = None;
            self.sent = None;
        }
        idle
    }

    fn take_unsent(&mut self) -> Option<Pos> {
        let t = self.thumb?;
        if self.sent == Some(t) {
            None
        } else {
            self.sent = Some(t);
            Some(t)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fast_drag_is_one_write_after_the_pointer_rests() {
        let mut d = SliderDrag::default();
        let g1 = d.press("s", 3, 4);
        let g2 = d.moved("s", 5);
        let g3 = d.moved("s", 8);
        // the timers of the earlier positions are stale, only the last one sends
        assert_eq!(d.due(g1), None);
        assert_eq!(d.due(g2), None);
        assert_eq!(d.due(g3), Some(("s", 8)));
        // and it is not sent twice
        assert_eq!(d.due(g3), None);
        assert_eq!(d.thumb, Some(("s", 8)));
    }

    #[test]
    fn release_sends_the_final_position_once() {
        let mut d = SliderDrag::default();
        d.press("s", 3, 4);
        let g = d.moved("s", 9);
        assert_eq!(d.release(), Some(("s", 9)));
        // the timer that was pending when the pointer went up is stale
        assert_eq!(d.due(g), None);
        // a second release (the mouse-up after a mouse-move release) is nothing
        assert_eq!(d.release(), None);
    }

    #[test]
    fn releasing_where_the_hardware_already_is_sends_nothing() {
        let mut d = SliderDrag::default();
        d.press("s", 3, 3); // click on the thumb itself
        assert_eq!(d.release(), None);
        assert!(
            d.settle(),
            "no job: the thumb goes back to the hardware at once"
        );
        assert_eq!(d.thumb, None);

        // dragged away and back to the starting value before anything was sent
        d.press("s", 3, 5);
        d.moved("s", 3);
        assert_eq!(d.release(), None);
    }

    #[test]
    fn the_timer_may_fire_mid_drag_and_release_then_skips_the_duplicate() {
        let mut d = SliderDrag::default();
        d.press("s", 0, 2);
        let g = d.moved("s", 6);
        assert_eq!(d.due(g), Some(("s", 6))); // the pointer rested
        d.job_started();
        assert_eq!(d.release(), None); // released at the same spot
        assert!(!d.settle(), "a job is still running");
    }

    #[test]
    fn the_thumb_is_held_until_the_pointer_is_up_and_every_job_landed() {
        let mut d = SliderDrag::default();
        d.press("s", 0, 2);
        let g = d.moved("s", 6);
        assert_eq!(d.due(g), Some(("s", 6)));
        d.job_started(); // mid-drag job
        let last = d.moved("s", 9);
        assert_eq!(d.release(), Some(("s", 9)));
        d.job_started(); // the final job
        let _ = last;

        // the mid-drag job lands first: the thumb must stay where it was let go
        d.job_done();
        assert_eq!(d.thumb, Some(("s", 9)));
        // an unrelated job landing early cannot release it either (it is not counted here at all)
        d.job_done();
        assert_eq!(d.thumb, None, "the last slider job released the thumb");
    }

    #[test]
    fn a_job_landing_while_the_pointer_is_down_does_not_release_the_thumb() {
        let mut d = SliderDrag::default();
        d.press("s", 0, 2);
        let g = d.moved("s", 6);
        d.due(g);
        d.job_started();
        d.job_done();
        assert_eq!(d.thumb, Some(("s", 6)));
        assert!(d.down);
    }
}
