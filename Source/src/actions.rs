//! Requests raised by widgets and handled by the application after the UI pass.

use std::path::PathBuf;

#[derive(Clone)]
pub enum Action {
    /// open a file in a tab; optionally jump to a 1-based real line
    OpenFile(PathBuf, Option<u64>),
    Toast(String),
    OpenViewer { bulk: u64, row: Option<u64> },
    ShowRefs,
    /// move byte ranges of the current document into one new *INCLUDE file (the *INCLUDE line
    /// replaces the first range)
    SplitBlock { ranges: Vec<(usize, usize)>, suggested: String, label: String },
    /// save each of these analysis blocks as its own *INCLUDE file (batch dialog)
    SplitEach { blocks: Vec<usize>, title: String },
    /// save the mesh of one part (index of its *PART block) — opens the batch dialog with options
    SplitPartMesh { part_block: usize },
    /// replace an *INCLUDE block by the content of the file
    InlineInclude { start: usize, end: usize, path: PathBuf },
    /// turn a bulk placeholder into editable text
    ExpandBulk { bulk: u64 },
}
