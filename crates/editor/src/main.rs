use rxui::prelude::*;
use velk_editor_widget::Editor;

fn main() -> Result<(), ApplicationError> {
    Application::new().run(|cx| {
        let editor = cx.new(|_| Editor::default());
        cx.open_window(WindowOptions::new().title("Velk").size(1280., 800.), editor)?;
        Ok(())
    })
}
