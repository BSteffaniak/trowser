//! Interactive application layout shared by painting and input handling.

use bmux_tui::geometry::Rect;

/// Columns reserved at the right edge of the document for the scrollbar.
pub const SCROLLBAR_WIDTH: u16 = 1;

/// Width at which the browser document is projected for a terminal width.
///
/// The scrollbar column is always reserved so the document does not reflow
/// when it becomes long enough to scroll.
#[must_use]
pub const fn document_width(terminal_width: u16) -> u16 {
    let width = terminal_width.saturating_sub(SCROLLBAR_WIDTH);
    if width == 0 { 1 } else { width }
}

/// Regions of one interactive application frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserLayout {
    /// One-row title header.
    pub header: Rect,
    /// Tab bar row, when tabs exist and the terminal is tall enough.
    pub tabs: Option<Rect>,
    /// Browser document viewport, excluding the scrollbar column.
    pub body: Rect,
    /// Scrollbar column beside the document viewport.
    pub scrollbar: Rect,
    /// Key hint row, when help is shown and the terminal is tall enough.
    pub hints: Option<Rect>,
    /// One-row status or prompt line.
    pub status: Rect,
}

impl BrowserLayout {
    /// Lay out `area` for the given optional rows.
    #[must_use]
    pub const fn new(area: Rect, has_tabs: bool, show_hints: bool) -> Self {
        let header = Rect::new(area.x, area.y, area.width, 1);
        let has_tabs = has_tabs && area.height >= 4;
        let tabs = if has_tabs {
            Some(Rect::new(area.x, area.y.saturating_add(1), area.width, 1))
        } else {
            None
        };
        let status_y = area.bottom().saturating_sub(1);
        let status = Rect::new(area.x, status_y, area.width, 1);
        let body_y = area.y.saturating_add(if has_tabs { 2 } else { 1 });
        let show_hints = show_hints && status_y >= body_y.saturating_add(2);
        let (body_bottom, hints) = if show_hints {
            let hints_y = status_y.saturating_sub(1);
            (hints_y, Some(Rect::new(area.x, hints_y, area.width, 1)))
        } else {
            (status_y, None)
        };
        let body_height = body_bottom.saturating_sub(body_y);
        let body_width = if area.width > SCROLLBAR_WIDTH {
            area.width - SCROLLBAR_WIDTH
        } else {
            area.width
        };
        let body = Rect::new(area.x, body_y, body_width, body_height);
        let scrollbar = Rect::new(
            area.x.saturating_add(body_width),
            body_y,
            area.width.saturating_sub(body_width),
            body_height,
        );
        Self {
            header,
            tabs,
            body,
            scrollbar,
            hints,
            status,
        }
    }
}

#[cfg(test)]
mod tests {
    use bmux_tui::geometry::Rect;

    use super::{BrowserLayout, document_width};

    #[test]
    fn reserves_rows_for_tabs_and_hints_and_a_scrollbar_column() {
        let layout = BrowserLayout::new(Rect::new(0, 0, 40, 10), true, true);

        assert_eq!(layout.tabs, Some(Rect::new(0, 1, 40, 1)));
        assert_eq!(layout.body, Rect::new(0, 2, 39, 6));
        assert_eq!(layout.scrollbar, Rect::new(39, 2, 1, 6));
        assert_eq!(layout.hints, Some(Rect::new(0, 8, 40, 1)));
        assert_eq!(layout.status, Rect::new(0, 9, 40, 1));
        assert_eq!(document_width(40), 39);
    }

    #[test]
    fn drops_optional_rows_on_short_terminals() {
        let layout = BrowserLayout::new(Rect::new(0, 0, 10, 3), true, true);

        assert_eq!(layout.tabs, None);
        assert_eq!(layout.hints, None);
        assert_eq!(layout.body.height, 1);
    }
}
