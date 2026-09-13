//! Centralized session "facts" formatting.
//!
//! Several surfaces (info widgets, the overscroll status line, and the compact
//! right-side fact stack) all want to show the same handful of facts: the model,
//! reasoning effort, context usage, the working directory, the provider, and so
//! on. Historically each surface formatted these independently, which led to
//! duplication and inconsistency (raw vs pretty model ids).
//!
//! This module is the single source of truth for compact fact formatting
//! (`pretty_model`, `dir_label`, and related helpers).

use unicode_width::UnicodeWidthStr;

/// Render `claude-opus-4-8` as `Opus 4.8`, `gpt-5.5` as `GPT-5.5`, etc. Single
/// source of truth for the human-friendly model name across every compact UI
/// surface. The redundant `Claude ` family prefix is dropped: the provider
/// fact already says Claude/Anthropic, so the model reads as `Fable 5` or
/// `Sonnet 4.5` rather than `Claude Fable 5`.
pub(crate) fn pretty_model(model: &str) -> String {
    let pretty = crate::tui::app::helpers::model_names::pretty_model_display_name(model);
    match pretty.strip_prefix("Claude ") {
        Some(rest) if !rest.trim().is_empty() => rest.to_string(),
        _ => pretty,
    }
}

/// Home-relative directory label, e.g. `/home/me/jcode` -> `~/jcode`. Does not
/// shorten intermediate path segments.
pub(crate) fn dir_label(path: &str) -> String {
    dir_label_with_home(path, std::env::var_os("HOME").as_deref())
}

fn dir_label_with_home(path: &str, home: Option<&std::ffi::OsStr>) -> String {
    let trimmed = path.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    if let Some(home) = home {
        let home = home.to_string_lossy();
        if !home.is_empty() && (trimmed == home || trimmed.starts_with(&format!("{home}/"))) {
            let rest = &trimmed[home.len()..];
            return if rest.is_empty() {
                "~".to_string()
            } else {
                format!("~{rest}")
            };
        }
    }
    trimmed.to_string()
}

/// Render a directory for a known display width.  The complete home-relative
/// path wins whenever it fits.  Otherwise elide only leading path components,
/// retaining the basename and as many trailing parents as possible.
pub(crate) fn dir_label_for_width(path: &str, width: usize) -> String {
    dir_label_for_width_with_home(path, width, std::env::var_os("HOME").as_deref())
}

fn dir_label_for_width_with_home(
    path: &str,
    width: usize,
    home: Option<&std::ffi::OsStr>,
) -> String {
    let label = dir_label_with_home(path, home);
    if label.width() <= width {
        return label;
    }
    if width == 0 {
        return String::new();
    }

    let home_relative = label.starts_with("~/");
    let components: Vec<&str> = label
        .trim_start_matches("~/")
        .trim_start_matches('/')
        .split('/')
        .filter(|component| !component.is_empty())
        .collect();
    let Some(basename) = components.last().copied() else {
        return truncate_path_tail("/", width);
    };

    let prefix = if home_relative { "~/…/" } else { "…/" };
    let mut trailing = basename.to_string();
    for component in components[..components.len() - 1].iter().rev() {
        let candidate = format!("{component}/{trailing}");
        let candidate_with_prefix = format!("{prefix}{candidate}");
        if candidate_with_prefix.width() > width {
            break;
        }
        trailing = candidate;
    }
    let result = format!("{prefix}{trailing}");
    if result.width() <= width {
        result
    } else {
        // An extremely narrow footer cannot retain a complete basename. Keep
        // its trailing characters rather than hiding which directory it is.
        truncate_path_tail(basename, width)
    }
}

fn truncate_path_tail(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }

    let available = width - 1;
    let mut suffix = String::new();
    let mut used = 0;
    for character in text.chars().rev() {
        let character_width = character.to_string().width();
        if used + character_width > available {
            break;
        }
        suffix.insert(0, character);
        used += character_width;
    }
    format!("…{suffix}")
}

/// Compact home-relative directory label that elides intermediate segments,
/// e.g. `/home/me/a/b/c` -> `…/b/c` and `~/a/b/c` -> `~/…/c`. Used where space
/// is tight (status line, overscroll, compact fact stack).
pub(crate) fn dir_label_short(path: &str) -> Option<String> {
    let trimmed = path.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let display = dir_label(trimmed);
    let segs: Vec<&str> = display.split('/').filter(|s| !s.is_empty()).collect();
    let short = if display.starts_with('~') {
        if segs.len() <= 2 {
            display.clone()
        } else {
            format!("~/…/{}", segs[segs.len() - 1])
        }
    } else if segs.len() <= 2 {
        format!("/{}", segs.join("/"))
    } else {
        format!("…/{}/{}", segs[segs.len() - 2], segs[segs.len() - 1])
    };
    Some(short)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_model_drops_redundant_claude_prefix() {
        assert_eq!(pretty_model("claude-opus-4-8"), "Opus 4.8");
        assert_eq!(pretty_model("claude-sonnet-4-5"), "Sonnet 4.5");
        assert_eq!(pretty_model("claude-fable-5"), "Fable 5");
        // Non-Claude ids are untouched.
        assert_eq!(pretty_model("gpt-5.5"), "GPT-5.5");
        assert_eq!(pretty_model("gemini-2.5-pro"), "Gemini 2.5 Pro");
    }

    #[test]
    fn dir_label_is_home_relative() {
        // Avoid depending on the real HOME by checking the non-home branch and
        // the trailing-slash normalization.
        assert_eq!(dir_label("/var/log/"), "/var/log");
        assert_eq!(dir_label("/"), "/");
        assert_eq!(dir_label("   "), "/");
    }

    #[test]
    fn dir_label_short_elides_middle_segments() {
        assert_eq!(dir_label_short("/a/b"), Some("/a/b".to_string()));
        assert_eq!(dir_label_short("/a/b/c/d"), Some("…/c/d".to_string()));
        assert_eq!(dir_label_short(""), None);
    }

    #[test]
    fn dir_label_for_width_keeps_full_home_relative_path_when_it_fits() {
        let home = std::ffi::OsStr::new("/home/ada");
        assert_eq!(
            dir_label_for_width_with_home("/home/ada/src/jcode", 20, Some(home)),
            "~/src/jcode"
        );
    }

    #[test]
    fn dir_label_for_width_keeps_basename_and_trailing_parents_when_narrow() {
        assert_eq!(
            dir_label_for_width_with_home("/a/b/c/issue24-rss24-implement1", 28, None),
            "…/c/issue24-rss24-implement1"
        );
    }

    #[test]
    fn dir_label_for_width_handles_root_nonhome_unicode_and_tiny_widths() {
        assert_eq!(dir_label_for_width_with_home("/", 1, None), "/");
        assert_eq!(
            dir_label_for_width_with_home("/opt/工具/資料", 11, None),
            "…/工具/資料"
        );
        assert_eq!(
            dir_label_for_width_with_home("/very-long-name", 4, None),
            "…ame"
        );
        assert_eq!(
            dir_label_for_width_with_home("/very-long-name", 1, None),
            "…"
        );
        assert_eq!(
            dir_label_for_width_with_home("/very-long-name", 0, None),
            ""
        );
    }
}
