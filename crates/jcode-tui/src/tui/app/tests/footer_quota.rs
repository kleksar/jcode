#[test]
fn test_footer_quota_hides_floating_diagnostics_but_preserves_model_effort() {
    use crate::tui::TuiState;
    let _lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_provider_model = Some("gpt-6-astra".into());
    app.remote_reasoning_effort = Some("high".into());
    let shared = app.info_widget_data();
    assert_eq!(shared.model.as_deref(), Some("gpt-6-astra"));
    assert_eq!(shared.reasoning_effort.as_deref(), Some("high"));
    let floating = app.floating_info_widget_data();
    assert!(!floating.has_data_for(crate::tui::info_widget::WidgetKind::ModelInfo));
    assert!(
        floating.model.is_none()
            && floating.usage_info.is_none()
            && floating.context_info.is_none()
    );
    assert!(floating.git_info.is_none() && floating.cache_hit_info.is_none());
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    let text = render_and_snap(&app, &mut terminal);
    let footer_rows = (28..30)
        .map(|y| {
            (0..140)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        footer_rows.contains("/high"),
        "model effort must stay in footer: {footer_rows}"
    );
    assert_eq!(
        text.matches("/high").count(),
        1,
        "no duplicate floating model: {text}"
    );
    assert!(!text.contains("KV cache:"), "{text}");
}

#[test]
fn footer_renders_current_session_label_at_right_edge() {
    use crate::tui::TuiState;
    let _lock = scroll_render_test_lock();
    let app = create_test_app();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();

    render_and_snap(&app, &mut terminal);

    let footer = (0..140)
        .map(|x| terminal.backend().buffer()[(x, 29)].symbol())
        .collect::<String>();
    let name = app.session_display_name().unwrap();
    let icon = crate::id::session_icon(&name);
    let name_start = footer.find(&name).expect("session name should render");
    let name_end = name_start + name.len();
    assert!(
        footer[..name_start].trim_end().ends_with(icon),
        "footer: {footer:?}"
    );
    assert!(footer[name_end..].starts_with('│'), "footer: {footer:?}");
}

#[test]
fn test_footer_quota_accepts_verified_weekly_usage_only() {
    use crate::tui::{
        FooterQuota,
        info_widget::{UsageInfo, UsageProvider},
    };
    let mut usage = UsageInfo {
        provider: UsageProvider::OpenAI,
        available: true,
        seven_day: 0.23,
        seven_day_resets_at: Some("2030-09-06T12:00:00Z".into()),
        ..Default::default()
    };
    assert_eq!(
        FooterQuota::from_usage(&usage).unwrap().remaining_percent,
        77
    );
    usage.seven_day = 1.0;
    assert_eq!(
        FooterQuota::from_usage(&usage).unwrap().remaining_percent,
        0
    );
    usage.seven_day = 0.0;
    assert_eq!(
        FooterQuota::from_usage(&usage).unwrap().remaining_percent,
        100
    );
    for invalid in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        usage.seven_day = invalid;
        assert!(FooterQuota::from_usage(&usage).is_none());
    }
    usage.seven_day = 0.23;
    usage.provider = UsageProvider::CostBased;
    assert!(FooterQuota::from_usage(&usage).is_none());
    usage.provider = UsageProvider::Anthropic;
    assert_eq!(
        FooterQuota::from_usage(&usage).unwrap().remaining_percent,
        77
    );
    usage.available = false;
    assert!(FooterQuota::from_usage(&usage).is_none());
    usage.available = true;
    usage.seven_day_resets_at = None;
    assert!(FooterQuota::from_usage(&usage).is_none());
    usage.seven_day_resets_at = Some("invalid".into());
    assert!(FooterQuota::from_usage(&usage).is_none());
}
