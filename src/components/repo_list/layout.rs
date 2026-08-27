//! Row-layout math for the repo list's three status zones.
//!
//! Each repo row is `[gutter+name] [branch] [attention…]      [metadata]`:
//! names pack against the left edge, the branch column sits one gap after
//! the widest name (only non-default branches render), the *attention cell*
//! packs each row's actionable git state (toggles, ahead/behind, submodule
//! markers, fetch warning) tight against one shared rail, and passive
//! metadata (liveness dot, file count) forms a right-aligned band whose
//! anchor is capped by the table itself so it never drifts on wide panels.
//! No zone reserves space for indicators a row doesn't have — a clean repo
//! renders as just its name.
//!
//! Both the renderer ([`super::RepoList::render_repo_item`]) and the click
//! hit-test ([`super::render::indicator_columns`]) place the attention cell
//! from [`RowLayout::attention_x`] and [`attention_cells`] — computing them
//! in one place is what keeps the drawn glyphs and the clickable cells in
//! agreement.

use std::path::PathBuf;

use super::RepoEntry;
use crate::git::status::RepoStatus;

/// Which indicator an attention-cell segment renders; the renderer maps this
/// to a theme color and the hit-test picks out the clickable toggles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AttentionCell {
    Stash,
    Worktree,
    Ahead,
    Behind,
    DirtySub,
    UnpushedSub,
    FetchWarn,
}

/// The attention cell for one repo: its actionable git state as packed
/// `(text, kind)` segments in fixed order — toggles first (they are click
/// targets), then sync counts, then warnings. Segments are joined by single
/// spaces; absent state produces no segment at all.
pub(super) fn attention_cells(
    status: &RepoStatus,
    stash_expanded: bool,
    worktree_expanded: bool,
) -> Vec<(String, AttentionCell)> {
    let mut cells = Vec::new();
    if !status.stashes.is_empty() {
        let icon = if stash_expanded {
            "\u{25bc}"
        } else {
            "\u{25b6}"
        };
        cells.push((
            format!("{icon}${}", status.stash_count()),
            AttentionCell::Stash,
        ));
    }
    if !status.worktree_info.is_empty() {
        let icon = if worktree_expanded {
            "\u{25bc}"
        } else {
            "\u{25b6}"
        };
        cells.push((
            format!("{icon}{}", status.worktree_info.len()),
            AttentionCell::Worktree,
        ));
    }
    if status.ahead > 0 {
        cells.push((format!("\u{2191}{}", status.ahead), AttentionCell::Ahead));
    }
    if status.behind > 0 {
        cells.push((format!("\u{2193}{}", status.behind), AttentionCell::Behind));
    }
    if status.has_dirty_submodules {
        cells.push(("\u{25c8}".to_string(), AttentionCell::DirtySub));
    }
    if status.has_unpushed_submodules {
        cells.push(("\u{21e1}".to_string(), AttentionCell::UnpushedSub));
    }
    if status.fetch_failed {
        cells.push(("\u{26a0}".to_string(), AttentionCell::FetchWarn));
    }
    cells
}

/// Width of packed segments joined by single spaces.
pub(super) fn packed_width(cells: &[(String, AttentionCell)]) -> u16 {
    let glyphs: u16 = cells
        .iter()
        .map(|(text, _)| text.chars().count() as u16)
        .sum();
    glyphs + cells.len().saturating_sub(1) as u16
}

/// A repo sitting on its default branch is the quiet state: the branch still
/// renders (hiding it read as missing data) but in the dimmed
/// `branch_default` color, so only deviations carry color.
// ponytail: literal default-branch names; `git::status::default_branch_name`
// already resolves origin/HEAD — plumb it through `RepoStatus` if a workspace
// ever defaults elsewhere.
pub(super) fn is_default_branch(branch: &str) -> bool {
    branch == "main" || branch == "master"
}

/// Per-frame column widths shared by every repo row. Only the name and
/// branch columns align; everything after the branch packs tight per row.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RowLayout {
    /// Name column: the widest display name, capped so the branch column and
    /// the busiest tail still fit the panel. Longer names are ellipsized.
    pub(super) name_col: u16,
    /// Branch column: widest branch, capped to a third of the panel so one
    /// outlier branch can't squeeze every name. 0 only when no repo has
    /// status yet.
    pub(super) branch_col: u16,
}

impl RowLayout {
    /// First column of the branch zone: marker (2) + name column + one gap.
    pub(super) fn branch_x(&self) -> u16 {
        2 + self.name_col + 1
    }

    /// First column of the packed status tail: one gap after the branch
    /// column (directly after the name while no repo has status).
    pub(super) fn attention_x(&self) -> u16 {
        self.branch_x()
            + if self.branch_col > 0 {
                self.branch_col + 1
            } else {
                0
            }
    }
}

/// Compute the shared column widths for the current frame.
pub(super) fn row_layout(
    repos: &[RepoEntry],
    live_panes: &[(String, PathBuf)],
    inner_width: u16,
) -> RowLayout {
    let mut l = RowLayout::default();
    let widest_name = repos
        .iter()
        .map(|r| {
            let (nested, label) = super::row_label(repos, r);
            let dressing = if nested {
                (super::NESTED_CONNECTOR.chars().count() + super::NESTED_BADGE.chars().count())
                    as u16
            } else {
                0
            };
            dressing + label.chars().count() as u16
        })
        .max()
        .unwrap_or(0);
    // Widest packed tail (attention cells + liveness dot + file count) —
    // only used to budget the name column below.
    let mut tail_max: u16 = 0;
    for entry in repos {
        let live = crate::session::liveness::is_live(&entry.path, live_panes);
        let mut tail = u16::from(live);
        if let Some(status) = entry.status.as_ref() {
            l.branch_col = l.branch_col.max(status.branch.chars().count() as u16);
            let attention = packed_width(&attention_cells(status, false, false));
            if attention > 0 {
                tail += attention + u16::from(tail > 0);
            }
            if !status.files.is_empty() {
                let count = 2 + status.files.len().to_string().len() as u16;
                tail += count + u16::from(tail > 0);
            }
        }
        tail_max = tail_max.max(tail);
    }
    l.branch_col = l.branch_col.min(inner_width / 3);
    // Cap the name column so the busiest row still fits; the gap budget
    // mirrors branch_x/attention_x above.
    let reserved = if l.branch_col > 0 {
        l.branch_col + 1
    } else {
        0
    } + tail_max;
    l.name_col = widest_name
        .min(inner_width.saturating_sub(2 + 1 + reserved))
        .max(1);
    l
}
