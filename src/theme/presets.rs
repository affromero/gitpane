//! Built-in theme presets.

use ratatui::style::Color;

use super::Theme;

/// "Muted" preset: same semantic structure as the default, but the eye-bleed
/// `Light*` and pure-saturation ANSI colors are swapped for desaturated
/// 256-color indices. Designed for dark terminals where the default palette
/// feels too bright. Slots not listed here retain the default value.
pub(crate) fn muted() -> Theme {
    let mut t = Theme::default();

    // RepoList: file count / dirty marker / stash less saturated.
    t.repo_list.dirty_marker = Color::Indexed(178); // Gold3
    t.repo_list.file_count = Color::Indexed(178);
    t.repo_list.ahead = Color::Indexed(71); // DarkSeaGreen4
    t.repo_list.behind = Color::Indexed(167); // Salmon3
    t.repo_list.dirty_submodule = Color::Indexed(133); // MediumOrchid3
    t.repo_list.unpushed_submodule = Color::Indexed(167);
    t.repo_list.repo_name = Color::Indexed(252); // Grey82
    t.repo_list.branch = Color::Indexed(73); // CadetBlue
    // Worktree = warm amber, stash = dark magenta. Stays clear of the
    // cyan branch and the LightMagenta dirty-submodule slot.
    t.repo_list.worktree_count = Color::Indexed(172); // Orange3, dimmer than default's 214
    t.repo_list.worktree_subtree_branch = Color::Indexed(172);
    t.repo_list.stash = Color::Indexed(96); // Plum4
    t.repo_list.border_focused = Color::Indexed(73);

    // StatusBar: error/success backgrounds softened.
    t.status_bar.error_label_bg = Color::Indexed(124); // Red3
    t.status_bar.error_text = Color::Indexed(167);
    t.status_bar.success_label_bg = Color::Indexed(71);
    t.status_bar.success_text = Color::Indexed(71);
    t.status_bar.focus_label_bg = Color::Indexed(73);

    // FileList: status colors muted.
    t.file_list.border_focused = Color::Indexed(73);
    t.file_list.status_modified = Color::Indexed(178);
    t.file_list.status_added = Color::Indexed(71);
    t.file_list.status_deleted = Color::Indexed(167);
    t.file_list.status_renamed = Color::Indexed(67); // SteelBlue3
    t.file_list.status_conflicted = Color::Indexed(167);
    t.file_list.submodule_path = Color::Indexed(133);
    t.file_list.regular_path = Color::Indexed(252);
    t.file_list.diff_border = Color::Indexed(73);
    t.file_list.diff_added = Color::Indexed(71);
    t.file_list.diff_removed = Color::Indexed(167);
    t.file_list.diff_hunk = Color::Indexed(73);
    t.file_list.diff_context = Color::Indexed(252);
    t.file_list.submodule_bracket = Color::Indexed(133);
    t.file_list.submodule_unpushed = Color::Indexed(71);
    t.file_list.submodule_unreachable = Color::Indexed(167);

    // Graph: paragraph + commit colors muted, palettes desaturated.
    t.graph.border_focused = Color::Indexed(73);
    t.graph.loading = Color::Indexed(178);
    t.graph.error_text = Color::Indexed(167);
    t.graph.commit_id = Color::Indexed(178);
    t.graph.commit_message = Color::Indexed(252);
    t.graph.addition = Color::Indexed(71);
    t.graph.deletion = Color::Indexed(167);
    t.graph.commit_msg_border = Color::Indexed(73);
    t.graph.commit_msg_text = Color::Indexed(252);
    t.graph.commit_files_border = Color::Indexed(73);
    t.graph.commit_files_status_modified = Color::Indexed(178);
    t.graph.commit_files_status_added = Color::Indexed(71);
    t.graph.commit_files_status_deleted = Color::Indexed(167);
    t.graph.commit_files_status_renamed = Color::Indexed(67);
    t.graph.commit_files_path = Color::Indexed(252);
    t.graph.commit_diff_border = Color::Indexed(73);
    t.graph.commit_diff_added = Color::Indexed(71);
    t.graph.commit_diff_removed = Color::Indexed(167);
    t.graph.commit_diff_hunk = Color::Indexed(73);
    t.graph.commit_diff_context = Color::Indexed(252);
    t.graph.search_overlay_fg = Color::Indexed(252);
    t.graph.lane_palette = vec![
        Color::Indexed(167), // soft red
        Color::Indexed(71),  // soft green
        Color::Indexed(178), // soft gold
        Color::Indexed(67),  // soft blue
        Color::Indexed(133), // soft magenta
        Color::Indexed(73),  // soft cyan
    ];
    t.graph.author_palette = vec![
        Color::Indexed(110), // LightSkyBlue3
        Color::Indexed(108), // DarkSeaGreen
        Color::Indexed(116), // DarkSlateGray3
        Color::Indexed(139), // Grey63
        Color::Indexed(174), // LightPink3
        Color::Indexed(143), // DarkKhaki
        Color::Indexed(180), // Tan
        Color::Indexed(146), // LightSteelBlue3
    ];
    t.graph.paren = Color::Indexed(178);
    t.graph.head_marker = Color::Indexed(71);
    t.graph.worktree_marker = Color::Indexed(133);
    t.graph.tag_label = Color::Indexed(143); // DarkKhaki — much softer than LightYellow
    t.graph.remote_label = Color::Indexed(167);
    t.graph.local_branch_label = Color::Indexed(73);

    // Overlay: same softening across confirm/help/toast.
    t.overlay.context_menu_push = Color::Indexed(71);
    t.overlay.context_menu_pull = Color::Indexed(178);
    t.overlay.context_menu_submodule = Color::Indexed(133);
    t.overlay.context_menu_border = Color::Indexed(73);
    t.overlay.confirm_accept = Color::Indexed(71);
    t.overlay.confirm_cancel = Color::Indexed(167);
    t.overlay.confirm_border = Color::Indexed(178);
    t.overlay.path_input_prompt = Color::Indexed(73);
    t.overlay.path_input_caret_bg = Color::Indexed(252);
    t.overlay.path_input_border = Color::Indexed(73);
    t.overlay.border_drag_active = Color::Indexed(178);
    t.overlay.update_toast_arrow = Color::Indexed(71);
    t.overlay.update_toast_version = Color::Indexed(178);
    t.overlay.help_key = Color::Indexed(178);
    t.overlay.help_border = Color::Indexed(178);

    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn muted_preserves_unmodified_slots() {
        let m = muted();
        let d = Theme::default();
        assert_eq!(m.repo_list.fetch_failed, d.repo_list.fetch_failed);
        assert_eq!(m.status_bar.bar_default, d.status_bar.bar_default);
        assert_eq!(m.overlay.help_bg, d.overlay.help_bg);
    }

    #[test]
    fn muted_replaces_bright_slots() {
        let m = muted();
        assert_ne!(m.repo_list.dirty_marker, Color::Yellow);
        assert_ne!(m.repo_list.dirty_submodule, Color::LightMagenta);
        assert_ne!(m.repo_list.unpushed_submodule, Color::LightRed);
        assert_ne!(m.graph.tag_label, Color::LightYellow);
    }

    #[test]
    fn muted_lane_and_author_palettes_match_expected_sizes() {
        let m = muted();
        assert_eq!(m.graph.lane_palette.len(), 6);
        assert_eq!(m.graph.author_palette.len(), 8);
    }
}
