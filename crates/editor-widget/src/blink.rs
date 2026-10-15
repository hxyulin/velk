use crate::surface::Geometry;
use rxui::{Context, Task};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

const INTERVAL: Duration = Duration::from_millis(500);

/// Each placement owns one timer, released when its view is removed. An old
/// timer wake only checks the deadline; it cannot shorten a new visible interval.
pub(crate) struct Blink {
    geometry: Rc<RefCell<Geometry>>,
    task: Option<Task>,
    phase: RefCell<Phase>,
}

struct Phase {
    epoch: u64,
    hidden: bool,
    deadline: Instant,
}

impl Phase {
    fn new(epoch: u64, now: Instant) -> Self {
        Self {
            epoch,
            hidden: false,
            deadline: now + INTERVAL,
        }
    }

    fn visible(&mut self, epoch: u64, now: Instant) -> bool {
        if self.epoch != epoch {
            *self = Self::new(epoch, now);
        }
        !self.hidden
    }

    /// Returns whether the visible state changed and needs a redraw.
    fn tick(&mut self, focused: bool, now: Instant) -> bool {
        if !focused {
            self.hidden = false;
            self.deadline = now + INTERVAL;
            return false;
        }
        if now < self.deadline {
            return false;
        }
        self.hidden = !self.hidden;
        self.deadline = now + INTERVAL;
        true
    }
}

impl Blink {
    pub fn new(geometry: Rc<RefCell<Geometry>>, epoch: u64, cx: &mut Context<'_, Self>) -> Self {
        let mut blink = Self {
            geometry,
            task: None,
            phase: RefCell::new(Phase::new(epoch, Instant::now())),
        };
        blink.arm(cx);
        blink
    }

    pub fn visible(&self, epoch: u64) -> bool {
        // The current view already observes this state. Resetting the phase here
        // makes both this frame and its geometry follow-up show the caret.
        self.phase.borrow_mut().visible(epoch, Instant::now())
    }

    fn arm(&mut self, cx: &mut Context<'_, Self>) {
        let delay = self
            .phase
            .borrow()
            .deadline
            .saturating_duration_since(Instant::now());
        self.task = cx
            .try_spawn(rxui::sleep(delay), |this, result, cx| {
                if result.is_ok() {
                    let changed = this
                        .phase
                        .borrow_mut()
                        .tick(this.geometry.borrow().focused, Instant::now());
                    if !changed {
                        cx.unchanged();
                    }
                    this.arm(cx);
                }
            })
            .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interaction_while_hidden_survives_followup_and_restarts_deadline() {
        let start = Instant::now();
        let mut phase = Phase::new(0, start);
        assert!(phase.tick(true, start + INTERVAL));
        assert!(phase.hidden);
        let input = start + Duration::from_millis(990);
        assert!(phase.visible(1, input));
        // Geometry publication must not restore the previous hidden state.
        assert!(phase.visible(1, input + Duration::from_millis(1)));
        // The timer from before the input was due at 1000 ms.
        assert!(!phase.tick(true, start + INTERVAL * 2));
        assert!(!phase.hidden);
        assert!(!phase.tick(true, input + INTERVAL - Duration::from_nanos(1)));
        assert!(phase.tick(true, input + INTERVAL));
        assert!(phase.hidden);
    }

    #[test]
    fn interaction_while_visible_also_restarts_deadline() {
        let start = Instant::now();
        let mut phase = Phase::new(0, start);
        let input = start + Duration::from_millis(499);
        assert!(phase.visible(1, input));
        assert!(!phase.tick(true, start + INTERVAL));
        assert!(phase.tick(true, input + INTERVAL));
        // A normal reevaluation must not restart the interval.
        assert!(!phase.visible(1, input + INTERVAL));
        assert!(phase.tick(true, input + INTERVAL * 2));
        assert!(!phase.hidden);
    }

    #[test]
    fn editor_appearance_updates_do_not_reset_caret_activity() {
        let mut editor = crate::Editor::new("hello");
        let initial = editor.caret_epoch;
        editor
            .set_appearance(crate::EditorAppearance::default())
            .unwrap();
        editor.clear_annotations();
        assert_eq!(editor.caret_epoch, initial);
        editor.insert("!");
        assert_eq!(editor.caret_epoch, initial + 1);
        editor.edited(); // Also used by navigation and pointer selection.
        assert_eq!(editor.caret_epoch, initial + 2);
    }

    #[test]
    fn unfocused_timer_does_not_request_redraw_or_catch_up() {
        let start = Instant::now();
        let mut phase = Phase::new(0, start);
        assert!(phase.tick(true, start + INTERVAL));
        let later = start + Duration::from_secs(10);
        assert!(!phase.tick(false, later));
        assert!(!phase.hidden);
        assert!(!phase.tick(true, later));
        assert!(phase.tick(true, later + INTERVAL));
    }
}
