use crate::surface::Geometry;
use rxui::{Context, Task};
use std::{cell::RefCell, rc::Rc, time::Duration};

/// Each placement owns one timer, released when its view is removed. No redraw
/// is requested while unfocused, and focused carets toggle twice per second.
pub(crate) struct Blink {
    geometry: Rc<RefCell<Geometry>>,
    task: Option<Task>,
    epoch: Option<u64>,
    pub hidden: bool,
}

impl Blink {
    pub fn new(geometry: Rc<RefCell<Geometry>>, cx: &mut Context<'_, Self>) -> Self {
        let mut blink = Self {
            geometry,
            task: None,
            epoch: None,
            hidden: false,
        };
        blink.arm(cx);
        blink
    }

    fn arm(&mut self, cx: &mut Context<'_, Self>) {
        self.task = cx
            .try_spawn(
                rxui::sleep(Duration::from_millis(500)),
                |this, result, cx| {
                    if result.is_ok() {
                        let geometry = this.geometry.borrow();
                        if geometry.focused {
                            this.hidden = if this.epoch == geometry.epoch {
                                !this.hidden
                            } else {
                                false
                            };
                            this.epoch = geometry.epoch;
                        } else {
                            this.hidden = false;
                            cx.unchanged();
                        }
                        drop(geometry);
                        this.arm(cx);
                    }
                },
            )
            .ok();
    }
}
