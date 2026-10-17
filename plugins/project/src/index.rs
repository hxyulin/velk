use crate::scan::{Files, bytes, caption};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use nucleo_matcher::{
    Config, Matcher, Utf32String,
    pattern::{AtomKind, CaseMatching, Normalization, Pattern},
};
use std::path::PathBuf;
use velk_plugin_sdk::{
    commands::{Encoding, Location, Position},
    host::{FileKind, MAX_DIRECTORY_ENTRIES, Request, Response},
};
use velk_project_contracts::{Match, Reply as QueryReply};

pub(super) struct Index {
    pub root: PathBuf,
    pub paths: Vec<PathBuf>,
    names: Vec<Utf32String>,
    pub skipped: usize,
    matcher: Matcher,
}
impl Index {
    pub fn build(host: &impl Files, cancelled: impl Fn() -> bool) -> Result<Self, String> {
        let Response::Workspace { workspace } = host.request(Request::Workspace)? else {
            return Err("invalid workspace reply".into());
        };
        let root = workspace
            .project_root
            .ok_or("Choose a project folder first")?;
        let mut stack = vec![(PathBuf::new(), Vec::<Gitignore>::new())];
        let mut paths = Vec::new();
        let mut skipped = 0;
        while let Some((directory, mut ignores)) = stack.pop() {
            let mut entries = Vec::new();
            let mut after = None;
            loop {
                if cancelled() {
                    return Err("index cancelled".into());
                }
                let page = match host.request(Request::ReadDirectory {
                    path: root.join(&directory),
                    after,
                }) {
                    Ok(Response::Directory { entries }) => entries,
                    _ => {
                        skipped += 1;
                        break;
                    }
                };
                let more = page.len() == MAX_DIRECTORY_ENTRIES;
                after = page.last().map(|entry| entry.path.clone());
                entries.extend(page);
                if !more {
                    break;
                }
            }
            for name in [".gitignore", ".ignore", ".rgignore"] {
                let path = directory.join(name);
                if !entries
                    .iter()
                    .any(|entry| entry.path == path && entry.kind == FileKind::File)
                {
                    continue;
                }
                match bytes(host, root.join(path))
                    .and_then(|bytes| String::from_utf8(bytes).map_err(|e| e.to_string()))
                {
                    Ok(text) => {
                        let mut builder = GitignoreBuilder::new(root.join(&directory));
                        for line in text.lines() {
                            if builder.add_line(None, line).is_err() {
                                skipped += 1;
                            }
                        }
                        match builder.build() {
                            Ok(ignore) => ignores.push(ignore),
                            Err(_) => skipped += 1,
                        }
                    }
                    Err(_) => skipped += 1,
                }
            }
            for entry in entries {
                if cancelled() {
                    return Err("index cancelled".into());
                }
                if entry
                    .path
                    .components()
                    .any(|part| part.as_os_str() == ".git")
                    || entry.kind == FileKind::Symlink
                {
                    continue;
                }
                let directory = entry.kind == FileKind::Directory;
                let path = root.join(&entry.path);
                let mut ignored = false;
                for matcher in &ignores {
                    let matched = matcher.matched(&path, directory);
                    if matched.is_ignore() {
                        ignored = true;
                    } else if matched.is_whitelist() {
                        ignored = false;
                    }
                }
                if ignored {
                    continue;
                }
                if directory {
                    stack.push((entry.path, ignores.clone()));
                } else if entry.kind == FileKind::File {
                    paths.push(entry.path);
                }
            }
        }
        paths.sort();
        let names = paths
            .iter()
            .map(|path| Utf32String::from(path.to_string_lossy().as_ref()))
            .collect();
        Ok(Self {
            root,
            paths,
            names,
            skipped,
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
        })
    }
    pub fn filter(
        &mut self,
        query: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<QueryReply, String> {
        let pattern = Pattern::new(
            query,
            CaseMatching::Smart,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        let mut ranked = Vec::new();
        for (index, name) in self.names.iter().enumerate() {
            if cancelled() {
                return Err("search cancelled".into());
            }
            if let Some(score) = pattern.score(name.slice(..), &mut self.matcher) {
                ranked.push((std::cmp::Reverse(score), index));
            }
        }
        let total = ranked.len();
        if ranked.len() > 1000 {
            ranked.select_nth_unstable(1000);
            ranked.truncate(1000);
        }
        ranked.sort_unstable();
        let matches = ranked
            .into_iter()
            .map(|(_, index)| Match {
                relative: caption(&self.paths[index].to_string_lossy()),
                preview: None,
                location: Location {
                    path: self.root.join(&self.paths[index]),
                    position: Position {
                        line: 0,
                        column: 0,
                        encoding: Encoding::Utf32,
                    },
                },
            })
            .collect();
        Ok(QueryReply::Ready {
            matches,
            notice: format!(
                "{total} matches · {} indexed files · {} skipped paths",
                self.paths.len(),
                self.skipped
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use velk_plugin_sdk::host::{DirectoryEntry, Workspace};

    struct Project {
        paths: Vec<PathBuf>,
        reads: Cell<usize>,
    }
    impl Files for Project {
        fn request(&self, request: Request) -> Result<Response, String> {
            self.reads.set(self.reads.get() + 1);
            match request {
                Request::Workspace => Ok(Response::Workspace {
                    workspace: Workspace {
                        current_directory: "/project".into(),
                        project_root: Some("/project".into()),
                    },
                }),
                Request::ReadDirectory { after, .. } => Ok(Response::Directory {
                    entries: self
                        .paths
                        .iter()
                        .filter(|path| after.as_ref().is_none_or(|after| *path > after))
                        .take(MAX_DIRECTORY_ENTRIES)
                        .map(|path| DirectoryEntry {
                            path: path.clone(),
                            kind: FileKind::File,
                        })
                        .collect(),
                }),
                _ => Err("unexpected file read".into()),
            }
        }
    }
    fn ready(reply: QueryReply) -> Vec<Match> {
        let QueryReply::Ready { matches: items, .. } = reply else {
            panic!("expected results")
        };
        items
    }
    #[test]
    fn complete_index_is_filtered_before_results_are_limited_without_more_file_reads() {
        let project = Project {
            paths: (0..25_000)
                .map(|i| PathBuf::from(format!("file-{i:05}.rs")))
                .collect(),
            reads: Cell::new(0),
        };
        let mut index = Index::build(&project, || false).unwrap();
        assert_eq!(index.paths.len(), 25_000);
        assert_eq!(project.reads.get(), 8);
        let reads = project.reads.get();
        assert_eq!(ready(index.filter("", || false).unwrap()).len(), 1000);
        let items = ready(index.filter("file-24999", || false).unwrap());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].relative, "file-24999.rs");
        assert!(ready(index.filter("missing", || false).unwrap()).is_empty());
        assert_eq!(project.reads.get(), reads);
        assert!(index.filter("file", || true).is_err());
        assert!(Index::build(&project, || true).is_err());
    }
    #[test]
    fn matching_handles_unicode_path_ranking_and_refreshed_files() {
        let mut project = Project {
            paths: [
                "main.rs",
                "many/intermediate/directories/name.rs",
                "src/世界.rs",
            ]
            .map(PathBuf::from)
            .into(),
            reads: Cell::new(0),
        };
        let mut index = Index::build(&project, || false).unwrap();
        assert_eq!(
            ready(index.filter("main", || false).unwrap())[0].relative,
            "main.rs"
        );
        assert_eq!(
            ready(index.filter("世rs", || false).unwrap())[0].relative,
            "src/世界.rs"
        );
        project.paths.push("z-new.rs".into());
        assert!(ready(index.filter("z-new", || false).unwrap()).is_empty());
        let mut refreshed = Index::build(&project, || false).unwrap();
        assert_eq!(ready(refreshed.filter("z-new", || false).unwrap()).len(), 1);
    }
    #[test]
    #[ignore = "manual release-mode filtering measurement"]
    fn measure_large_index_filtering() {
        let project = Project {
            paths: (0..100_000)
                .map(|i| PathBuf::from(format!("crates/package-{i:06}/src/document.rs")))
                .collect(),
            reads: Cell::new(0),
        };
        let mut index = Index::build(&project, || false).unwrap();
        let mut elapsed = Vec::new();
        for _ in 0..10 {
            for query in ["", "doc", "document", "p9999", "pkg src", "missing"] {
                let start = std::time::Instant::now();
                std::hint::black_box(index.filter(query, || false).unwrap());
                elapsed.push(start.elapsed());
            }
        }
        elapsed.sort();
        eprintln!(
            "100,000 indexed paths, 60 queries: median {:?}, p95 {:?}",
            elapsed[elapsed.len() / 2],
            elapsed[elapsed.len() * 95 / 100]
        );
    }
}
