mod showcase;

use rxui::prelude::*;
use velk_editor_widget::Editor;

fn main() -> Result<(), ApplicationError> {
    let showcase = std::env::args()
        .skip(1)
        .any(|argument| argument == "--showcase");
    Application::new().run(move |cx| {
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
