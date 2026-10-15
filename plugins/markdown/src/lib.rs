//! Bundled CommonMark provider. Parsing is private to this native library.
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use velk_plugin_sdk::{
    Plugin,
    markup::{Document, Style},
};

impl Plugin for CommonMark {
    const ID: &'static str = "velk.markdown";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    fn register(reg: &mut velk_plugin_sdk::Registrar<Self>) -> Result<(), String> {
        reg.provide(
            velk_plugin_sdk::hooks::RenderMarkdown,
            |plugin: &mut Self, source: &str| plugin.render(source),
        )
    }
    fn describe(&mut self) -> String {
        "CommonMark documentation renderer".into()
    }
}

#[derive(Default)]
pub struct CommonMark;
impl CommonMark {
    fn render(&self, markdown: &str) -> Document {
        let mut doc = Document::default();
        let mut style = Style::default();
        let mut stack = Vec::new();
        let mut lists: Vec<Option<u64>> = Vec::new();
        for event in Parser::new_ext(
            markdown,
            Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS,
        ) {
            match event {
                Event::Start(tag) => {
                    stack.push(style.clone());
                    match tag {
                        Tag::Table(_) => doc.separate(2),
                        Tag::Heading { level, .. } => {
                            doc.separate(2);
                            style.heading = level as u8;
                            style.bold = true;
                        }
                        Tag::Strong => style.bold = true,
                        Tag::Emphasis => style.italic = true,
                        Tag::Strikethrough => style.strikethrough = true,
                        Tag::CodeBlock(_) => {
                            doc.separate(2);
                            style.code = true;
                        }
                        Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                            style.link = Some(dest_url.into_string())
                        }
                        Tag::List(start) => {
                            doc.separate(1);
                            lists.push(start);
                        }
                        Tag::Item => {
                            doc.separate(1);
                            let prefix = match lists.last_mut() {
                                Some(Some(n)) => {
                                    let p = format!("{n}. ");
                                    *n += 1;
                                    p
                                }
                                _ => "• ".into(),
                            };
                            doc.push(&"  ".repeat(lists.len().saturating_sub(1)), &style);
                            doc.push(&prefix, &style);
                        }
                        Tag::BlockQuote(_) => {
                            doc.separate(1);
                            doc.push("│ ", &style);
                        }
                        Tag::TableRow | Tag::TableHead => doc.separate(1),
                        _ => {}
                    }
                }
                Event::End(tag) => {
                    match tag {
                        TagEnd::Paragraph if lists.is_empty() => doc.separate(2),
                        TagEnd::Heading(_) | TagEnd::CodeBlock | TagEnd::Table => doc.separate(2),
                        TagEnd::Item
                        | TagEnd::BlockQuote(_)
                        | TagEnd::TableRow
                        | TagEnd::TableHead => doc.separate(1),
                        TagEnd::List(_) => {
                            lists.pop();
                            doc.separate(1);
                        }
                        TagEnd::TableCell => doc.push("  │  ", &Style::default()),
                        _ => {}
                    }
                    style = stack.pop().unwrap_or_default();
                }
                Event::Text(text) => doc.push(&text, &style),
                Event::Code(text) => {
                    let mut code = style.clone();
                    code.code = true;
                    doc.push(&text, &code);
                }
                Event::SoftBreak => doc.push(" ", &style),
                Event::HardBreak => doc.push("\n", &style),
                Event::Rule => {
                    doc.separate(2);
                    doc.push("────────", &Style::default());
                    doc.separate(2);
                }
                Event::TaskListMarker(done) => doc.push(if done { "☑ " } else { "☐ " }, &style),
                // HTML is shown literally, never interpreted or fetched. Images
                // similarly retain their alt text without loading remote resources.
                Event::Html(text) | Event::InlineHtml(text) => doc.push(&text, &style),
                Event::FootnoteReference(text) => doc.push(&format!("[{text}]"), &style),
                Event::InlineMath(text) | Event::DisplayMath(text) => doc.push(&text, &style),
            }
        }
        while let Some(last) = doc.runs.last_mut() {
            last.text.truncate(last.text.trim_end_matches('\n').len());
            if last.text.is_empty() {
                doc.runs.pop();
            } else {
                break;
            }
        }
        doc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_nested_styles_code_and_link_metadata() {
        let doc = CommonMark.render("# Title\n\n**bold *both*** and `x < y` and ~~old~~ [docs](https://example.com)\n\n```rust\nlet x = 1;\n```\n");
        assert!(
            doc.runs
                .iter()
                .any(|r| r.text == "Title" && r.style.heading == 1)
        );
        assert!(
            doc.runs
                .iter()
                .any(|r| r.text == "both" && r.style.bold && r.style.italic)
        );
        assert!(
            doc.runs
                .iter()
                .any(|r| r.text.contains("let x = 1;") && r.style.code)
        );
        assert!(doc.runs.iter().any(|r| r.text == "docs" && r.style.link.as_deref() == Some("https://example.com")));
        assert!(!doc.text().contains("```"));
        assert!(doc.text().contains("x < y"));
    }
    #[test]
    fn lists_quotes_and_plaintext_remain_readable() {
        let text = CommonMark
            .render("3. first\n4. second\n\n> quote\n\n- [x] done")
            .text();
        assert!(text.contains("3. first\n4. second"), "{text:?}");
        assert!(text.contains("│ quote"));
        assert!(text.contains("☑ done"));
        assert_eq!(Document::plain("**literal**").text(), "**literal**");
    }
    #[test]
    fn untrusted_html_images_and_incomplete_markdown_are_inert() {
        let doc = CommonMark.render(
            "<script>alert(1)</script>\n\n![alt](https://example.com/x.png)\n\n**unfinished é",
        );
        assert!(doc.text().contains("<script>"));
        assert!(doc.text().contains("alt"));
        assert!(doc.text().contains("unfinished é"));
    }
}
