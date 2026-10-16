# CommonMark

Render **Markdown documentation** in Velk, including plugin READMEs and language-server hover text.

This is the bundled default provider, not a parser embedded in the editor. Other
plugins can implement the same documentation contract. Select a replacement in
Settings under Markdown provider and run Reload Window. Disabling the selected
provider shows literal source.

## Formatting

Supports headings, **bold**, *italic*, `code`, fenced code blocks, lists, tables, task lists, links and ~~strikethrough~~.

Tables and other block structures are flattened into styled text; this contract
does not provide arbitrary document layout or interactive elements.

HTML is displayed literally. Images retain their alt text without fetching remote resources.
