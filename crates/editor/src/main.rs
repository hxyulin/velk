mod showcase;
mod stress;

use rxui::prelude::*;
use velk_editor_widget::Editor;

fn main() -> Result<(), ApplicationError> {
    let showcase = std::env::args()
        .skip(1)
        .any(|argument| argument == "--showcase");
    let stress = std::env::args()
        .skip(1)
        .any(|argument| argument == "--stress");
    Application::new().run(move |cx| {
        if stress {
            let editor = cx.new(|_| stress::editor());
            cx.open_window(
                WindowOptions::new()
                    .title("Velk — 100,000-line stress test")
                    .size(1280., 800.),
                editor,
            )?;
            return Ok(());
        }
        if showcase {
            let gallery = cx.new(|cx| showcase::Showcase::new(cx));
            cx.open_window(
                WindowOptions::new()
                    .title("Velk — rich text playground")
                    .size(1280., 900.),
                gallery,
            )?;
            return Ok(());
        }
        let editor = cx.new(|_| Editor::default());
        cx.open_window(WindowOptions::new().title("Velk").size(1280., 800.), editor)?;
        Ok(())
    })
}
