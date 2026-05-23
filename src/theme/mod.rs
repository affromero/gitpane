//! Theme system for gitpane's TUI.
//!
//! All color choices live here. Components read `&Theme` from `App` and
//! reference semantic slots, never hardcoded `Color::X` literals. The
//! `Default` impl reproduces the colors the codebase shipped before themes
//! existed, so a config with no `theme` setting renders identically.

mod loader;
mod presets;

pub(crate) use loader::{LoadThemeError, discover_all_theme_names, load_theme};
pub(crate) use presets::muted;

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Theme {
    pub repo_list: RepoListTheme,
    pub status_bar: StatusBarTheme,
    pub file_list: FileListTheme,
    pub graph: GraphTheme,
    pub overlay: OverlayTheme,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct RepoListTheme {
    pub git_op_marker: Color,
    pub dirty_marker: Color,
    pub branch: Color,
    pub ahead: Color,
    pub behind: Color,
    pub worktree_count: Color,
    pub dirty_submodule: Color,
    pub unpushed_submodule: Color,
    pub fetch_failed: Color,
    pub stash: Color,
    pub file_count: Color,
    pub repo_name: Color,
    pub worktree_subtree_icon: Color,
    pub worktree_subtree_branch: Color,
    pub border_focused: Color,
    pub border_unfocused: Color,
    pub selection_bg: Color,
}

impl Default for RepoListTheme {
    fn default() -> Self {
        Self {
            git_op_marker: Color::Cyan,
            dirty_marker: Color::Yellow,
            branch: Color::Cyan,
            ahead: Color::Green,
            behind: Color::Red,
            worktree_count: Color::Indexed(214),
            dirty_submodule: Color::LightMagenta,
            unpushed_submodule: Color::LightRed,
            fetch_failed: Color::DarkGray,
            stash: Color::Indexed(127),
            file_count: Color::Yellow,
            repo_name: Color::White,
            worktree_subtree_icon: Color::DarkGray,
            worktree_subtree_branch: Color::Indexed(214),
            border_focused: Color::Cyan,
            border_unfocused: Color::DarkGray,
            selection_bg: Color::DarkGray,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct StatusBarTheme {
    pub bar_default: Color,
    pub version: Color,
    pub legend_text: Color,
    pub dim_separator: Color,
    pub focus_label_fg: Color,
    pub focus_label_bg: Color,
    pub error_label_fg: Color,
    pub error_label_bg: Color,
    pub error_text: Color,
    pub success_label_fg: Color,
    pub success_label_bg: Color,
    pub success_text: Color,
    pub key_hint_fg: Color,
    pub key_hint_bg: Color,
}

impl Default for StatusBarTheme {
    fn default() -> Self {
        Self {
            bar_default: Color::Gray,
            version: Color::DarkGray,
            legend_text: Color::DarkGray,
            dim_separator: Color::DarkGray,
            focus_label_fg: Color::Black,
            focus_label_bg: Color::Cyan,
            error_label_fg: Color::White,
            error_label_bg: Color::Red,
            error_text: Color::Red,
            success_label_fg: Color::Black,
            success_label_bg: Color::Green,
            success_text: Color::Green,
            key_hint_fg: Color::Black,
            key_hint_bg: Color::DarkGray,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct FileListTheme {
    pub border_focused: Color,
    pub border_unfocused: Color,
    pub empty_text: Color,
    pub status_modified: Color,
    pub status_added: Color,
    pub status_deleted: Color,
    pub status_renamed: Color,
    pub status_untracked: Color,
    pub status_conflicted: Color,
    pub submodule_path: Color,
    pub regular_path: Color,
    pub selection_bg: Color,
    pub diff_border: Color,
    pub diff_added: Color,
    pub diff_removed: Color,
    pub diff_hunk: Color,
    pub diff_meta: Color,
    pub diff_context: Color,
    pub submodule_bracket: Color,
    pub submodule_unpushed: Color,
    pub submodule_unreachable: Color,
}

impl Default for FileListTheme {
    fn default() -> Self {
        Self {
            border_focused: Color::Cyan,
            border_unfocused: Color::DarkGray,
            empty_text: Color::DarkGray,
            status_modified: Color::Yellow,
            status_added: Color::Green,
            status_deleted: Color::Red,
            status_renamed: Color::Blue,
            status_untracked: Color::DarkGray,
            status_conflicted: Color::LightRed,
            submodule_path: Color::LightMagenta,
            regular_path: Color::White,
            selection_bg: Color::DarkGray,
            diff_border: Color::Cyan,
            diff_added: Color::Green,
            diff_removed: Color::Red,
            diff_hunk: Color::Cyan,
            diff_meta: Color::DarkGray,
            diff_context: Color::White,
            submodule_bracket: Color::LightMagenta,
            submodule_unpushed: Color::Green,
            submodule_unreachable: Color::LightRed,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct GraphTheme {
    pub border_focused: Color,
    pub border_unfocused: Color,
    pub loading: Color,
    pub error_text: Color,
    pub empty: Color,
    pub dimmed: Color,
    pub collapsed_message: Color,
    pub commit_id: Color,
    pub merge_message: Color,
    pub commit_message: Color,
    pub time: Color,
    pub addition: Color,
    pub deletion: Color,
    pub selection_bg: Color,
    pub commit_msg_border: Color,
    pub commit_msg_text: Color,
    pub commit_files_border: Color,
    pub commit_files_empty: Color,
    pub commit_files_status_modified: Color,
    pub commit_files_status_added: Color,
    pub commit_files_status_deleted: Color,
    pub commit_files_status_renamed: Color,
    pub commit_files_status_other: Color,
    pub commit_files_path: Color,
    pub commit_diff_border: Color,
    pub commit_diff_added: Color,
    pub commit_diff_removed: Color,
    pub commit_diff_hunk: Color,
    pub commit_diff_meta: Color,
    pub commit_diff_context: Color,
    pub search_overlay_fg: Color,
    pub search_overlay_bg: Color,
    pub lane_palette: Vec<Color>,
    pub author_palette: Vec<Color>,
    pub paren: Color,
    pub head_marker: Color,
    pub worktree_marker: Color,
    pub tag_label: Color,
    pub remote_label: Color,
    pub local_branch_label: Color,
    /// Color for `stash@{n}` labels in the graph. Matches the repo-list stash
    /// indicator so the same stash reads consistently across panels.
    pub stash_label: Color,
}

impl Default for GraphTheme {
    fn default() -> Self {
        Self {
            border_focused: Color::Cyan,
            border_unfocused: Color::DarkGray,
            loading: Color::Yellow,
            error_text: Color::Red,
            empty: Color::Gray,
            dimmed: Color::DarkGray,
            collapsed_message: Color::Rgb(130, 130, 130),
            commit_id: Color::Yellow,
            merge_message: Color::Rgb(130, 130, 130),
            commit_message: Color::White,
            time: Color::DarkGray,
            addition: Color::Green,
            deletion: Color::Red,
            selection_bg: Color::DarkGray,
            commit_msg_border: Color::Cyan,
            commit_msg_text: Color::White,
            commit_files_border: Color::Cyan,
            commit_files_empty: Color::DarkGray,
            commit_files_status_modified: Color::Yellow,
            commit_files_status_added: Color::Green,
            commit_files_status_deleted: Color::Red,
            commit_files_status_renamed: Color::Blue,
            commit_files_status_other: Color::DarkGray,
            commit_files_path: Color::White,
            commit_diff_border: Color::Cyan,
            commit_diff_added: Color::Green,
            commit_diff_removed: Color::Red,
            commit_diff_hunk: Color::Cyan,
            commit_diff_meta: Color::DarkGray,
            commit_diff_context: Color::White,
            search_overlay_fg: Color::White,
            search_overlay_bg: Color::DarkGray,
            lane_palette: vec![
                Color::Red,
                Color::Green,
                Color::Yellow,
                Color::Blue,
                Color::Magenta,
                Color::Cyan,
            ],
            author_palette: vec![
                Color::LightBlue,
                Color::LightGreen,
                Color::LightCyan,
                Color::LightMagenta,
                Color::LightRed,
                Color::LightYellow,
                Color::Rgb(255, 165, 0),
                Color::Rgb(180, 150, 255),
            ],
            paren: Color::Yellow,
            head_marker: Color::Green,
            worktree_marker: Color::Indexed(214),
            tag_label: Color::LightYellow,
            remote_label: Color::Red,
            local_branch_label: Color::Cyan,
            stash_label: Color::Indexed(127), // matches RepoListTheme::stash default
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct OverlayTheme {
    pub context_menu_push: Color,
    pub context_menu_pull: Color,
    pub context_menu_submodule: Color,
    pub context_menu_border: Color,
    pub context_menu_selection_bg: Color,
    pub confirm_accept: Color,
    pub confirm_cancel: Color,
    pub confirm_border: Color,
    pub path_input_prompt: Color,
    pub path_input_caret_bg: Color,
    pub path_input_caret_fg: Color,
    pub path_input_hint: Color,
    pub path_input_border: Color,
    pub border_drag_active: Color,
    pub border_drag_idle: Color,
    pub update_toast_arrow: Color,
    pub update_toast_version: Color,
    pub update_toast_install: Color,
    pub update_toast_border: Color,
    pub help_key: Color,
    pub help_border: Color,
    pub help_bg: Color,
}

impl Default for OverlayTheme {
    fn default() -> Self {
        Self {
            context_menu_push: Color::Green,
            context_menu_pull: Color::Yellow,
            context_menu_submodule: Color::LightMagenta,
            context_menu_border: Color::Cyan,
            context_menu_selection_bg: Color::DarkGray,
            confirm_accept: Color::Green,
            confirm_cancel: Color::Red,
            confirm_border: Color::Yellow,
            path_input_prompt: Color::Cyan,
            path_input_caret_bg: Color::White,
            path_input_caret_fg: Color::Black,
            path_input_hint: Color::DarkGray,
            path_input_border: Color::Cyan,
            border_drag_active: Color::Yellow,
            border_drag_idle: Color::DarkGray,
            update_toast_arrow: Color::Green,
            update_toast_version: Color::Yellow,
            update_toast_install: Color::DarkGray,
            update_toast_border: Color::DarkGray,
            help_key: Color::Yellow,
            help_border: Color::Yellow,
            help_bg: Color::Black,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_roundtrips_through_toml() {
        let original = Theme::default();
        let serialized = toml::to_string_pretty(&original).unwrap();
        let restored: Theme = toml::from_str(&serialized).unwrap();
        assert_eq!(
            restored.repo_list.dirty_marker,
            Color::Yellow,
            "round-trip should preserve named colors"
        );
        assert_eq!(
            restored.repo_list.stash,
            Color::Indexed(127),
            "round-trip should preserve indexed colors"
        );
        assert_eq!(
            restored.graph.collapsed_message,
            Color::Rgb(130, 130, 130),
            "round-trip should preserve rgb colors"
        );
        assert_eq!(restored.graph.lane_palette.len(), 6);
        assert_eq!(restored.graph.author_palette.len(), 8);
    }

    #[test]
    fn partial_override_applies_to_one_field_and_keeps_others_at_default() {
        let toml_input = r#"
            [repo_list]
            stash = "Magenta"
        "#;
        let theme: Theme = toml::from_str(toml_input).unwrap();
        assert_eq!(theme.repo_list.stash, Color::Magenta);
        assert_eq!(theme.repo_list.dirty_marker, Color::Yellow);
        assert_eq!(theme.repo_list.branch, Color::Cyan);
        assert_eq!(theme.graph.tag_label, Color::LightYellow);
    }

    #[test]
    fn empty_toml_yields_default_theme() {
        let theme: Theme = toml::from_str("").unwrap();
        assert_eq!(theme.repo_list.stash, Color::Indexed(127));
        assert_eq!(theme.graph.lane_palette[0], Color::Red);
    }
}
