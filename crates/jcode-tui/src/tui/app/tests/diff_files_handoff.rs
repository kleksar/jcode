#[test]
fn diff_files_handoff_routes_select_and_render_the_same_file() {
    use super::files_inspector::FileInspectorMode::{Read, Source};
    use super::worktree_pane::WorktreePaneTab::{Diff, Files};
    let _lock = scroll_render_test_lock();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    for route in 0..3 {
        for trigger in 0..4 {
            for (path, mode, rendered) in [
                ("README.md", Read, "changed"),
                ("script.py", Source, "def new()"),
                ("docs/deep/NEW.MD", Read, "untracked"),
            ] {
                let (repo, mut app, mut terminal) = inspector_fixture();
                std::fs::create_dir_all(repo.path().join("docs/deep")).unwrap();
                std::fs::write(
                    repo.path().join("docs/deep/NEW.MD"),
                    "# New\n\n**untracked**\n",
                )
                .unwrap();
                crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
                crate::tui::ui::prime_project_tree_for_tests(repo.path());
                app.set_worktree_pane_tab(Diff);
                render_and_snap(&app, &mut terminal);
                let layout = crate::tui::ui::worktree_pane_layout().unwrap();
                let index = layout.paths.iter().position(|item| item == path).unwrap();
                app.handle_worktree_pane_mouse(worktree_test_mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    layout.list_area.x + 1,
                    layout.list_area.y + index as u16,
                ));
                assert_eq!(app.current_worktree_selected_file(), Some(path));
                app.diff_pane_scroll = 2;
                app.diff_pane_scroll_x = 3;
                app.diff_pane_auto_scroll = false;
                app.worktree_pane.list_scroll = 1;
                let pages = app.side_panel.pages.len();
                match trigger {
                    0 => route_worktree_arrow(&mut app, route, KeyCode::Right, &rt),
                    1 => route_worktree_arrow(&mut app, route, KeyCode::Tab, &rt),
                    2 => {
                        app.handle_diff_pane_focus_key(KeyCode::BackTab, KeyModifiers::SHIFT);
                    }
                    _ => {
                        app.handle_worktree_pane_mouse(worktree_test_mouse(
                            MouseEventKind::Down(MouseButton::Left),
                            layout.files_tab_area.x + 1,
                            layout.files_tab_area.y,
                        ));
                    }
                }
                assert_eq!(
                    app.worktree_pane.tab, Files,
                    "route={route} trigger={trigger}"
                );
                assert_eq!(app.worktree_pane.tree_selected_path.as_deref(), Some(path));
                assert_eq!(app.files_inspector.mode(), Some(mode));
                let text = render_and_snap(&app, &mut terminal);
                assert!(text.contains(rendered), "{path}: {text}");
                if mode == Read {
                    assert!(!text.contains(&format!("**{rendered}**")), "{text}");
                }
                let files = crate::tui::ui::worktree_pane_layout().unwrap();
                assert!(files.preview_area.is_some());
                assert!(files.tree_rows.iter().any(|row| row.path == path));
                assert_eq!(app.side_panel.pages.len(), pages);
                app.files_inspector.set_scroll_offset(7);
                route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
                assert_eq!(app.worktree_pane.tab, Diff);
                assert_eq!(app.current_worktree_selected_file(), Some(path));
                assert_eq!(app.worktree_pane.list_scroll, 1);
                assert_eq!(
                    (
                        app.diff_pane_scroll,
                        app.diff_pane_scroll_x,
                        app.diff_pane_auto_scroll
                    ),
                    (2, 3, false)
                );
                render_and_snap(&app, &mut terminal);
                route_worktree_arrow(&mut app, route, KeyCode::Right, &rt);
                assert_eq!(app.files_inspector.mode(), Some(mode));
                assert_eq!(app.files_inspector.scroll_offset(), 7);
                assert_eq!(app.side_panel.pages.len(), pages);
            }
        }
    }
}

#[test]
fn diff_files_handoff_deleted_file_stays_in_diff_without_false_selection() {
    use super::worktree_pane::WorktreePaneTab::{Diff, Files};
    let _lock = scroll_render_test_lock();
    let (repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "script.py");
    std::fs::remove_file(repo.path().join("README.md")).unwrap();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    app.set_worktree_pane_tab(Diff);
    app.worktree_pane.selected_file = Some("README.md".into());
    app.diff_pane_scroll = 5;
    render_and_snap(&app, &mut terminal);
    app.handle_diff_pane_focus_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.worktree_pane.tab, Diff);
    assert_eq!(
        app.worktree_pane.tree_selected_path.as_deref(),
        Some("script.py")
    );
    assert_eq!(app.current_worktree_selected_file(), Some("README.md"));
    assert_eq!(app.diff_pane_scroll, 5);
    // Explicit file/link opening is not a handoff from the deleted Diff selection.
    app.set_worktree_pane_tab(Files);
    assert_eq!(app.worktree_pane.tab, Files);
}

#[test]
fn diff_files_handoff_preserves_existing_inspector_mode_and_diff_enter() {
    use super::files_inspector::FileInspectorMode::Source;
    use super::worktree_pane::{DiffFocus, WorktreePaneTab::Diff};
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "README.md");
    app.files_inspector.select_mode(Source);
    app.files_inspector.set_scroll_offset(11);
    app.set_worktree_pane_tab(Diff);
    app.worktree_pane.selected_file = Some("README.md".into());
    render_and_snap(&app, &mut terminal);
    app.handle_diff_pane_focus_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.worktree_pane.tab, Diff);
    assert_eq!(app.worktree_pane.diff_focus, DiffFocus::Content);
    app.handle_diff_pane_focus_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.files_inspector.mode(), Some(Source));
    assert_eq!(app.files_inspector.scroll_offset(), 11);
}

#[test]
fn diff_files_handoff_does_not_expire_diff_filter_while_reading_files() {
    use super::worktree_pane::WorktreePaneTab::Diff;
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = inspector_fixture();
    app.set_worktree_pane_tab(Diff);
    app.worktree_pane.selected_file = Some("README.md".into());
    app.worktree_pane.last_activity = Some(Instant::now() - Duration::from_secs(120));
    render_and_snap(&app, &mut terminal);
    app.handle_diff_pane_focus_key(KeyCode::Tab, KeyModifiers::NONE);
    app.worktree_pane.last_activity = Some(Instant::now() - Duration::from_secs(120));
    assert!(!app.update_worktree_file_filter(Instant::now()));
    assert_eq!(app.current_worktree_selected_file(), Some("README.md"));
}

#[test]
fn diff_files_handoff_roundtrip_preserves_all_files_filter() {
    use super::worktree_pane::WorktreePaneTab::{Diff, Files};
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "script.py");
    app.set_worktree_pane_tab(Diff);
    app.worktree_pane.selected_file = None;
    render_and_snap(&app, &mut terminal);
    app.handle_key(KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert_eq!(app.worktree_pane.tab, Files);
    render_and_snap(&app, &mut terminal);
    app.handle_key(KeyCode::Left, KeyModifiers::NONE).unwrap();
    assert_eq!(app.worktree_pane.tab, Diff);
    assert_eq!(app.current_worktree_selected_file(), None);
}

#[test]
#[cfg(unix)]
fn diff_files_handoff_rejects_outside_symlink_without_replacing_inspector() {
    use super::worktree_pane::WorktreePaneTab::Diff;
    let _lock = scroll_render_test_lock();
    let (repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "script.py");
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("outside.md");
    std::fs::write(&target, "# Outside").unwrap();
    std::os::unix::fs::symlink(&target, repo.path().join("escape.md")).unwrap();
    app.set_worktree_pane_tab(Diff);
    app.worktree_pane.selected_file = Some("escape.md".into());
    app.navigate_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Files);
    assert_eq!(app.worktree_pane.tab, Diff);
    assert_eq!(
        app.worktree_pane.tree_selected_path.as_deref(),
        Some("script.py")
    );
}
