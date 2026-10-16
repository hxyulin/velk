use super::Mode;
use std::{collections::BTreeMap, path::PathBuf};
use velk_plugin_sdk::{
    Host,
    commands::{Choice, ChoiceAction, Encoding, Location, PickerReply, Position},
    host::{Request, Response},
};
const MAX_RESULTS: usize = 1000;
const MAX_SEARCH_BYTES: usize = 32 * 1024 * 1024;
pub(super) trait Files {
    fn request(&self, request: Request) -> Result<Response, String>;
}
impl Files for Host {
    fn request(&self, request: Request) -> Result<Response, String> {
        self.request(request)
    }
}
pub(super) fn run(
    host: &Host,
    mode: Mode,
    query: &velk_search::Query,
    cancelled: impl Fn() -> bool,
) -> Result<PickerReply, String> {
    search(host, mode, query, &cancelled)
}
pub(super) fn bytes(host: &impl Files, path: PathBuf) -> Result<Vec<u8>, String> {
    match host.request(Request::ReadFile { path })? {
        Response::File { bytes } => Ok(bytes),
        _ => Err("invalid project file reply".into()),
    }
}
fn search(
    host: &impl Files,
    mode: Mode,
    query: &velk_search::Query,
    cancelled: &impl Fn() -> bool,
) -> Result<PickerReply, String> {
    if matches!(mode, Mode::Content) && query.text.is_empty() {
        return Ok(PickerReply::Ready {
            items: vec![],
            notice: "Enter text to search".into(),
        });
    }
    let Response::Workspace { workspace } = host.request(Request::Workspace)? else {
        return Err("invalid workspace reply".into());
    };
    let root = workspace
        .project_root
        .ok_or("Choose a project folder first")?;
    let mut overlays = BTreeMap::new();
    if matches!(mode, Mode::Content) {
        let Response::Buffers { buffers } = host.request(Request::Buffers)? else {
            return Err("invalid buffer list".into());
        };
        for buffer in buffers {
            if cancelled() {
                return Err("search cancelled".into());
            }
            if let Some(path) = buffer.path.filter(|path| path.starts_with(&root)) {
                overlays.insert(path, buffer.id);
            }
        }
    }
    let index = crate::index::Index::build(host, cancelled)?;
    if matches!(mode, Mode::Files) {
        return {
            let mut index = index;
            index.filter(&query.text, cancelled)
        };
    }
    let mut items = Vec::new();
    let mut used_bytes = 0;
    let mut skipped = index.skipped;
    let mut limited = false;
    for relative in &index.paths {
        if cancelled() {
            return Err("search cancelled".into());
        }
        let path = root.join(relative);
        let source = if let Some(id) = overlays.get(&path) {
            match host.request(Request::ReadBuffer { id: *id }) {
                Ok(Response::Buffer { buffer, text }) if buffer.path.as_ref() == Some(&path) => {
                    Ok(text.into_bytes())
                }
                _ => Err("buffer unavailable".into()),
            }
        } else {
            bytes(host, path.clone())
        };
        let Ok(source) = source else {
            skipped += 1;
            continue;
        };
        used_bytes += source.len();
        if used_bytes > MAX_SEARCH_BYTES {
            limited = true;
            break;
        }
        if source.contains(&0) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&source) else {
            continue;
        };
        for (line, column, preview) in matches(text, query, MAX_RESULTS - items.len()) {
            items.push(Choice {
                title: format!(
                    "{}:{}:{}",
                    caption(&relative.to_string_lossy()),
                    line + 1,
                    column + 1
                ),
                detail: Some(preview),
                action: ChoiceAction::Location {
                    location: Location {
                        path: path.clone(),
                        position: Position {
                            line: line as u32,
                            column: column as u32,
                            encoding: Encoding::Utf32,
                        },
                    },
                },
            });
        }
        if items.len() == MAX_RESULTS {
            limited = true;
            break;
        }
    }
    {
        items.sort_by(|a, b| match (&a.action, &b.action) {
            (ChoiceAction::Location { location: a }, ChoiceAction::Location { location: b }) => (
                &a.path,
                a.position.line,
                a.position.column,
            )
                .cmp(&(&b.path, b.position.line, b.position.column)),
            _ => a.title.cmp(&b.title),
        });
    }
    Ok(PickerReply::Ready {
        items,
        notice: format!(
            "{} entries checked; {skipped} unreadable or oversized paths skipped{}",
            index.paths.len(),
            if limited {
                "; results limited (1,000 results, 32 MiB of text)"
            } else {
                ""
            }
        ),
    })
}
fn matches(text: &str, query: &velk_search::Query, limit: usize) -> Vec<(usize, usize, String)> {
    let needle = if query.case_sensitive {
        query.text.clone()
    } else {
        query.text.to_lowercase()
    };
    if needle.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for (line, text) in text.lines().enumerate() {
        let mut folded = String::new();
        let mut starts = Vec::new();
        let mut ends = Vec::new();
        for (index, ch) in text.chars().enumerate() {
            starts.push((folded.len(), index));
            if query.case_sensitive {
                folded.push(ch);
            } else {
                folded.extend(ch.to_lowercase());
            }
            ends.push((folded.len(), index + 1));
        }
        let chars: Vec<_> = text.chars().collect();
        for (offset, _) in folded.match_indices(&needle) {
            let (Ok(start), Ok(end)) = (
                starts.binary_search_by_key(&offset, |p| p.0),
                ends.binary_search_by_key(&(offset + needle.len()), |p| p.0),
            ) else {
                continue;
            };
            let start = starts[start].1;
            let end = ends[end].1;
            if query.whole_word
                && (start > 0 && word(chars[start - 1]) || end < chars.len() && word(chars[end]))
            {
                continue;
            }
            let preview: String = chars
                .iter()
                .skip(start.saturating_sub(40))
                .take(160)
                .map(|c| if c.is_control() { ' ' } else { *c })
                .collect();
            found.push((line, start, preview));
            if found.len() >= limit {
                return found;
            }
        }
    }
    found
}
fn word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

pub(super) fn caption(text: &str) -> String {
    text.chars()
        .take(240)
        .map(|ch| if ch.is_control() { '�' } else { ch })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use velk_plugin_sdk::host::FileKind;
    struct Disk {
        root: PathBuf,
        overlay: Option<String>,
    }
    impl Files for Disk {
        fn request(&self, request: Request) -> Result<Response, String> {
            match request {
                Request::Workspace => Ok(Response::Workspace {
                    workspace: velk_plugin_sdk::host::Workspace {
                        current_directory: self.root.clone(),
                        project_root: Some(self.root.clone()),
                    },
                }),
                Request::ReadDirectory { path, after } => {
                    let mut entries = std::fs::read_dir(self.root.join(path))
                        .map_err(|e| e.to_string())?
                        .map(|entry| {
                            let entry = entry.unwrap();
                            let ty = entry.file_type().unwrap();
                            velk_plugin_sdk::host::DirectoryEntry {
                                path: entry.path().strip_prefix(&self.root).unwrap().into(),
                                kind: if ty.is_symlink() {
                                    FileKind::Symlink
                                } else if ty.is_dir() {
                                    FileKind::Directory
                                } else {
                                    FileKind::File
                                },
                            }
                        })
                        .collect::<Vec<_>>();
                    entries.sort_by(|a, b| a.path.cmp(&b.path));
                    entries.retain(|entry| after.as_ref().is_none_or(|after| entry.path > *after));
                    entries.truncate(velk_plugin_sdk::host::MAX_DIRECTORY_ENTRIES);
                    Ok(Response::Directory { entries })
                }
                Request::ReadFile { path } => std::fs::read(self.root.join(path))
                    .map(|bytes| Response::File { bytes })
                    .map_err(|e| e.to_string()),
                Request::Buffers => Ok(Response::Buffers {
                    buffers: self
                        .overlay
                        .as_ref()
                        .map(|_| velk_plugin_sdk::host::Buffer {
                            id: 1,
                            title: "main.rs".into(),
                            path: Some(self.root.join("src/main.rs")),
                            revision: 1,
                            dirty: true,
                            active: false,
                            anchor: 0,
                            head: 0,
                        })
                        .into_iter()
                        .collect(),
                }),
                Request::ReadBuffer { .. } => Ok(Response::Buffer {
                    buffer: velk_plugin_sdk::host::Buffer {
                        id: 1,
                        title: "main.rs".into(),
                        path: Some(self.root.join("src/main.rs")),
                        revision: 1,
                        dirty: true,
                        active: false,
                        anchor: 0,
                        head: 0,
                    },
                    text: self.overlay.clone().unwrap(),
                }),
                _ => Err("unexpected test request".into()),
            }
        }
    }
    #[test]
    fn search_positions_match_original_unicode_with_case_and_word_controls() {
        let query = velk_search::Query {
            text: "界".into(),
            ..Default::default()
        };
        assert_eq!(
            matches("a界界\n𐐀界", &query, 100)
                .iter()
                .map(|(l, c, _)| (*l, *c))
                .collect::<Vec<_>>(),
            vec![(0, 1), (0, 2), (1, 1)]
        );
        let query = velk_search::Query {
            text: "i".into(),
            ..Default::default()
        };
        assert_eq!(matches("İ i I", &query, 100).len(), 2);
        let query = velk_search::Query {
            text: "word".into(),
            whole_word: true,
            ..Default::default()
        };
        assert_eq!(matches("word words _word word!", &query, 100).len(), 2);
        assert_eq!(matches("word word word", &query, 2).len(), 2);
    }
    #[test]
    fn project_walk_respects_nested_ignores_and_prefers_unsaved_buffer_text() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        std::fs::create_dir(root.path().join("target")).unwrap();
        std::fs::write(root.path().join(".gitignore"), "target/\n*.tmp\n").unwrap();
        std::fs::write(root.path().join("src/.gitignore"), "generated.rs\n").unwrap();
        std::fs::write(root.path().join("src/generated.rs"), "needle").unwrap();
        std::fs::write(root.path().join("src/main.rs"), "disk text").unwrap();
        std::fs::write(root.path().join("target/output"), "needle").unwrap();
        std::fs::write(root.path().join("ignored.tmp"), "needle").unwrap();
        let disk = Disk {
            root: root.path().into(),
            overlay: Some("first\n世界 needle".into()),
        };
        let PickerReply::Ready { items, .. } = search(
            &disk,
            Mode::Files,
            &velk_search::Query {
                text: "smr".into(),
                ..Default::default()
            },
            &|| false,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "src/main.rs");
        let PickerReply::Ready { items, .. } = search(
            &disk,
            Mode::Content,
            &velk_search::Query {
                text: "needle".into(),
                ..Default::default()
            },
            &|| false,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(items.len(), 1);
        let ChoiceAction::Location { location } = &items[0].action else {
            panic!()
        };
        assert_eq!(location.position.line, 1);
        assert_eq!(location.position.column, 3);
        assert!(
            search(&disk, Mode::Files, &Default::default(), &|| true)
                .unwrap_err()
                .contains("cancelled")
        );
    }
}
