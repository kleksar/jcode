use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

pub fn clear_area(frame: &mut Frame, area: Rect) {
    for x in area.left()..area.right() {
        for y in area.top()..area.bottom() {
            frame.buffer_mut()[(x, y)].reset();
        }
    }
}

pub fn left_aligned_content_inset(width: u16, centered: bool) -> u16 {
    if centered || width <= 1 { 0 } else { 1 }
}

pub fn centered_content_block_width(width: u16, max_width: usize) -> usize {
    (width as usize).min(max_width).max(1)
}

pub fn left_pad_lines_to_block_width(lines: &mut [Line<'static>], width: u16, block_width: usize) {
    let block_width = block_width.min(width as usize);
    let pad = (width as usize).saturating_sub(block_width) / 2;
    for line in lines {
        if pad > 0 {
            line.spans.insert(0, Span::raw(" ".repeat(pad)));
        }
        line.alignment = Some(Alignment::Left);
    }
}

const RIGHT_RAIL_HEADER_HEIGHT: u16 = 1;
const RIGHT_RAIL_LEFT_INSET: u16 = 2;

pub fn right_rail_border_style(focused: bool, focus_color: Color, dim_color: Color) -> Style {
    let border_color = if focused { focus_color } else { dim_color };
    Style::default().fg(border_color)
}

fn right_rail_inner(area: Rect) -> Rect {
    Block::default().borders(Borders::LEFT).inner(area)
}

fn right_rail_inset(area: Rect) -> Option<Rect> {
    let inset = RIGHT_RAIL_LEFT_INSET.min(area.width);
    let area = Rect {
        x: area.x + inset,
        y: area.y,
        width: area.width - inset,
        height: area.height,
    };
    (area.width > 0).then_some(area)
}

fn right_rail_header_area(area: Rect) -> Option<Rect> {
    let inner = right_rail_inset(right_rail_inner(area))?;
    (inner.height >= RIGHT_RAIL_HEADER_HEIGHT).then_some(Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: RIGHT_RAIL_HEADER_HEIGHT,
    })
}

fn right_rail_content_area(area: Rect) -> Option<Rect> {
    let inner = right_rail_inset(right_rail_inner(area))?;
    if inner.height <= RIGHT_RAIL_HEADER_HEIGHT {
        return None;
    }

    Some(Rect {
        x: inner.x,
        y: inner.y + RIGHT_RAIL_HEADER_HEIGHT,
        width: inner.width,
        height: inner.height - RIGHT_RAIL_HEADER_HEIGHT,
    })
}

pub fn draw_right_rail_chrome(
    frame: &mut Frame,
    area: Rect,
    title: Line<'static>,
    border_style: Style,
) -> Option<Rect> {
    let content_area = right_rail_content_area(area)?;
    let header_area = right_rail_header_area(area)?;

    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(border_style);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(title), header_area);

    Some(content_area)
}

/// Set alignment on a line only if it doesn't already have one set.
/// This allows markdown rendering to mark code blocks as left-aligned while
/// other content inherits the default alignment (e.g., centered mode).
pub fn align_if_unset(line: Line<'static>, align: Alignment) -> Line<'static> {
    if line.alignment.is_some() {
        line
    } else {
        line.alignment(align)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_rail_content_insets_two_cells_after_left_border() {
        assert_eq!(
            right_rail_header_area(Rect::new(10, 5, 12, 8)),
            Some(Rect::new(13, 5, 9, 1))
        );
        assert_eq!(
            right_rail_content_area(Rect::new(10, 5, 12, 8)),
            Some(Rect::new(13, 6, 9, 7))
        );
    }

    #[test]
    fn right_rail_content_inset_saturates_at_tiny_widths() {
        assert_eq!(right_rail_content_area(Rect::new(0, 0, 1, 3)), None);
        assert_eq!(right_rail_content_area(Rect::new(0, 0, 2, 3)), None);
        assert_eq!(right_rail_content_area(Rect::new(0, 0, 3, 3)), None);
        assert_eq!(
            right_rail_content_area(Rect::new(0, 0, 4, 3)),
            Some(Rect::new(3, 1, 1, 2))
        );
    }
}
