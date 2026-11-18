//! Layout coordinate arithmetic for the now-playing panel.
//!
//! Every horizontal measurement the now-playing panel makes goes through
//! here, so the centring and column-alignment contracts are testable as plain
//! coordinate equalities.

/// Layout of the now-playing panel expressed in the panel's own coordinate
/// system (x=0 is the panel's left edge).
///
/// Invariant: the x-centre of every element is the panel's centre x.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NowPlayingLayout {
    pub panel_width: f32,
    pub content_width: f32,
    pub album_art_size: f32,
    pub album_art_center_x: f32,
    pub title_center_x: f32,
    pub seek_bar_center_x: f32,
    pub transport_center_x: f32,
    pub play_button_center_x: f32,
}

impl NowPlayingLayout {
    /// Compute the layout from the panel's outer width and the target content
    /// width; the view applies these coordinates without further arithmetic.
    pub fn compute(panel_width: f32, content_target: f32, h_pad: f32) -> Self {
        let content_width = content_target.min((panel_width - h_pad * 2.0).max(0.0));
        let panel_center = panel_width / 2.0;
        // Every row is content_width wide and centred, and the transport row
        // is symmetric around Play, so all centres equal panel_center.
        // The panel's sizing caps the art further (see `views::player`).
        let album_art_size = content_width;
        NowPlayingLayout {
            panel_width,
            content_width,
            album_art_size,
            album_art_center_x: panel_center,
            title_center_x: panel_center,
            seek_bar_center_x: panel_center,
            transport_center_x: panel_center,
            play_button_center_x: panel_center,
        }
    }
}

impl NowPlayingLayout {}

// ---------------------------------------------------------------------------
// Track-row layout: the column-x contract for scannable lists.
// ---------------------------------------------------------------------------

/// Column geometry for a single track row.
///
/// The duration column is placed by reserved width rather than the info
/// column's natural width, so rows of equal width share duration and action
/// edges regardless of content.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackRowLayout {
    pub row_width: f32,
    pub row_padding_l: f32,
    pub row_padding_r: f32,
    pub index_col_w: f32,
    pub duration_col_w: f32,
    pub action_col_w: f32,
    pub spacing: f32,
    /// Left edge of the (variable-width) title/artist Fill column.
    pub title_col_left: f32,
    /// Right edge of the title/artist Fill column; the duration column starts
    /// `spacing` after it.
    pub title_col_right: f32,
    /// Right edge of the duration column, shared by every row of the same
    /// `row_width`.
    pub duration_right_edge: f32,
    /// Right edge of the action column, the row's inner right edge.
    pub action_right_edge: f32,
}

impl TrackRowLayout {
    /// The default column widths the app uses. Columns:
    /// `[index 32][title/artist Fill][duration 52][action 40]`, `spacing=8`,
    /// row `padding_l=12`, `padding_r=12`.
    ///
    /// These are one coordinate contract rather than design tokens, so they
    /// stay named literals together even where a value matches a spacing rung.
    pub const INDEX_W: f32 = 32.0;
    pub const DURATION_W: f32 = 52.0;
    pub const ACTION_W: f32 = 40.0;
    pub const SPACING: f32 = 8.0;
    pub const PAD_L: f32 = 12.0;
}

#[cfg(test)]
mod tests {}
