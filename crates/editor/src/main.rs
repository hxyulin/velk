use rxui::prelude::*;

struct Editor;

impl View for Editor {
    fn view(&self, _cx: &mut ViewContext<'_, Self>) -> impl IntoElement {
        column().fill_width().fill_height()
    }
}

fn main() -> Result<(), ApplicationError> {
    Application::new().run(|cx| {
        let editor = cx.new(|_| Editor);
        cx.open_window(WindowOptions::new().title("Velk").size(1280., 800.), editor)?;
        Ok(())
    })
}
