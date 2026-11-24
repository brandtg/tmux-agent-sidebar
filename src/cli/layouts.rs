//! Sidebar-aware builtin window layouts.
//!
//! tmux's builtin `select-layout even-horizontal` and friends treat the
//! sidebar pane like any other pane: prefix+M-2 stacks the sidebar TUI
//! itself into the layout, prefix+M-1 shrinks it to a slice of the window,
//! and the maintain-width hook then fights the mangled result on every
//! layout change. This module rebuilds the five builtin arrangements with
//! the sidebar pane pinned to its configured edge at its configured width,
//! emitting a tmux JSON layout tree (`{"V":2,"L":...}`, tmux 3.8+) for
//! `select-layout`.
//!
//! The geometry is ported from tmux's layout-set.c / layout.c so the real
//! panes land exactly where the stock command would have put them — the
//! sidebar is simply excluded from the arrangement and re-inserted as a
//! full-height leaf at the window edge. Older tmux without JSON layout
//! parsing falls back to the plain stock `select-layout`.
//!
//! Three entry points (see `cmd_layouts`):
//! - `layouts <builtin> [window]` — apply a sidebar-aware builtin
//!   (bound to prefix+M-1..M-5 by the plugin conf) and record the
//!   window's `@sidebar_layout_base`.
//! - `layouts --keep <window>` — the `window-layout-changed` hook: when a
//!   recorded window's sidebar width drifts from the target (a layout
//!   change redistributed the columns), re-apply the recorded base
//!   builtin with the sidebar re-pinned.
//! - `layouts --restore-base <window>` — rebuild the recorded base
//!   builtin over the window's remaining panes; the close path uses this
//!   instead of the pre-sidebar layout snapshot so a toggle-off restores
//!   the user's chosen arrangement at the current window size.

use std::collections::{HashMap, HashSet};

use crate::tmux;

/// Window-scoped record of the last sidebar-aware builtin the user
/// applied. Presence opts the window into the `--keep` hook and the
/// base-builtin restore on close; unset means "never used here", and
/// every legacy path behaves exactly as before.
pub(crate) const SIDEBAR_LAYOUT_BASE: &str = "@sidebar_layout_base";

/// tmux `PANE_MINIMUM` (tmux.h): the smallest layout cell size, not
/// counting border lines.
const PANE_MINIMUM: u32 = 1;

// ─── Layout tree model ──────────────────────────────────────────────

/// Split direction of a node cell. [`Dir::Row`] is tmux's
/// `LAYOUT_LEFTRIGHT` (children side by side, serialized as `"h"`);
/// [`Dir::Col`] is `LAYOUT_TOPBOTTOM` (children stacked, `"v"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Row,
    Col,
}

impl Dir {
    fn json_tag(self) -> &'static str {
        match self {
            Dir::Row => "h",
            Dir::Col => "v",
        }
    }
}

/// One cell of a layout tree: a pane leaf or a split node. Sizes only —
/// offsets are recomputed at serialization time, matching tmux's own
/// `layout_fix_offsets`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Cell {
    Leaf {
        id: String,
        w: u32,
        h: u32,
    },
    Node {
        dir: Dir,
        w: u32,
        h: u32,
        children: Vec<Cell>,
    },
}

impl Cell {
    fn w(&self) -> u32 {
        match self {
            Cell::Leaf { w, .. } | Cell::Node { w, .. } => *w,
        }
    }

    fn h(&self) -> u32 {
        match self {
            Cell::Leaf { h, .. } | Cell::Node { h, .. } => *h,
        }
    }

    fn leaf(id: &str, w: u32, h: u32) -> Cell {
        Cell::Leaf {
            id: id.to_string(),
            w,
            h,
        }
    }
}

/// Whether a child of a column/row parent lies on the window's
/// top/bottom edge per tmux's `layout_cell_is_top` /
/// `layout_cell_is_bottom` walk: every TOPBOTTOM ancestor must have it
/// (or an ancestor of it) as the first/last tiled child; LEFTRIGHT
/// ancestors impose no constraint. `is_edge_child` is "first child" for
/// the top walk, "last child" for the bottom walk.
fn edge_flag(parent_dir: Dir, is_edge_child: bool, parent_flag: bool) -> bool {
    match parent_dir {
        // A row parent places children side by side: no vertical
        // constraint at this level, the walk continues through the
        // parent.
        Dir::Row => parent_flag,
        // A column parent stacks children: only the first (top) or last
        // (bottom) child continues the edge chain.
        Dir::Col => is_edge_child && parent_flag,
    }
}

// ─── Builtin arrangement generators ─────────────────────────────────

/// The window options the builtins read, pre-resolved from tmux.
#[derive(Debug, Clone, Default)]
struct LayoutOptions {
    /// `main-pane-height` raw value (`24`, `50%`, ...).
    main_pane_height: String,
    /// `main-pane-width` raw value.
    main_pane_width: String,
    /// `other-pane-height` raw value (empty/0 = derive from main).
    other_pane_height: String,
    /// `other-pane-width` raw value.
    other_pane_width: String,
    /// `tiled-layout-max-columns` (0 = unlimited).
    tiled_max_columns: u32,
    /// `pane-border-status`: top/bottom leaves get spread compensation.
    border_status: BorderStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum BorderStatus {
    #[default]
    Off,
    Top,
    Bottom,
}

/// Parse a tmux size option: plain cells, or `N%` of `base` clamped to
/// `base` (tmux's `args_string_percentage` with min 0 / max base).
/// `None` for garbage — callers substitute their own default, matching
/// tmux's `cause != NULL` fallbacks.
fn parse_size_or_pct(raw: &str, base: u32) -> Option<u32> {
    let raw = raw.trim();
    if let Some(pct) = raw.strip_suffix('%') {
        let pct: u32 = pct.trim().parse().ok()?;
        return Some((base.saturating_mul(pct) / 100).min(base));
    }
    raw.parse::<u32>().ok().map(|cells| cells.min(base))
}

/// Resolve `main-pane-height` against the available `sy`, with tmux's
/// default of 24 rows on garbage.
fn resolve_main_height(raw: &str, sy: u32) -> u32 {
    parse_size_or_pct(raw, sy).unwrap_or(24)
}

/// Resolve `main-pane-width` against the available `sx`, with tmux's
/// default of 80 columns on garbage.
fn resolve_main_width(raw: &str, sx: u32) -> u32 {
    parse_size_or_pct(raw, sx).unwrap_or(80)
}

/// Generate the base arrangement for `ids` (pane ids in window listing
/// order) inside a `win`-sized area, as tmux's own builtin would. The
/// tree root always spans exactly `win` (except in the degenerate
/// too-many-panes case tmux itself handles by overgrowing the layout —
/// callers reject that via `validate_tree` and fall back to the stock
/// command).
///
/// One pane gets a single full-size leaf (the layout is trivially the
/// window); zero panes returns `None`.
fn builtin_tree(name: &str, ids: &[String], win: (u32, u32), opts: &LayoutOptions) -> Option<Cell> {
    match name {
        "even-horizontal" => even_tree(Dir::Row, ids, win, opts),
        "even-vertical" => even_tree(Dir::Col, ids, win, opts),
        "main-horizontal" => main_tree(false, ids, win, opts),
        "main-vertical" => main_tree(true, ids, win, opts),
        "tiled" => tiled_tree(ids, win, opts),
        _ => None,
    }
}

fn even_tree(dir: Dir, ids: &[String], win: (u32, u32), opts: &LayoutOptions) -> Option<Cell> {
    if ids.is_empty() {
        return None;
    }
    if ids.len() == 1 {
        return Some(Cell::leaf(&ids[0], win.0, win.1));
    }
    // tmux layout_set_even: every child starts at the full window size,
    // then the root is spread along the split direction.
    let children: Vec<Cell> = ids.iter().map(|id| Cell::leaf(id, win.0, win.1)).collect();
    let mut root = Cell::Node {
        dir,
        w: win.0,
        h: win.1,
        children,
    };
    spread_node(&mut root, true, true, opts.border_status)?;
    Some(root)
}

/// `main-vertical` when `vertical` is true, `main-horizontal` otherwise.
/// Ported from `layout_set_main_v` / `layout_set_main_h`: the first pane
/// (window listing order) is the main pane; the rest share the remaining
/// strip.
fn main_tree(
    vertical: bool,
    ids: &[String],
    win: (u32, u32),
    opts: &LayoutOptions,
) -> Option<Cell> {
    if ids.is_empty() {
        return None;
    }
    if ids.len() == 1 {
        return Some(Cell::leaf(&ids[0], win.0, win.1));
    }
    let others = ids.len() - 1;

    if vertical {
        // Available width minus one column for the border between main
        // and other.
        let sx = win.0.checked_sub(1)?;
        let mainw = clamp_main(resolve_main_width(&opts.main_pane_width, sx), sx);
        let otherw = resolve_other(mainw, sx, &opts.other_pane_width);
        // Minimum height for the other strip, growing to the window.
        let sy = (others as u32 * (PANE_MINIMUM + 1))
            .saturating_sub(1)
            .max(win.1);

        let mut root = Cell::Node {
            dir: Dir::Row,
            w: mainw + otherw + 1,
            h: sy,
            children: vec![Cell::leaf(&ids[0], mainw, sy)],
        };
        if others == 1 {
            root.push_child(Cell::leaf(&ids[1], otherw, sy));
        } else {
            let mut other = Cell::Node {
                dir: Dir::Col,
                w: otherw,
                h: sy,
                children: ids[1..]
                    .iter()
                    .map(|id| Cell::leaf(id, otherw, PANE_MINIMUM))
                    .collect(),
            };
            // The other strip's edge flags: its parent chain runs through
            // the row root, which imposes no vertical constraint, so both
            // edges pass (tmux's walk: LEFTRIGHT ancestors are free).
            spread_node(&mut other, true, true, opts.border_status)?;
            root.push_child(other);
        }
        Some(root)
    } else {
        let sy = win.1.checked_sub(1)?;
        let mainh = clamp_main(resolve_main_height(&opts.main_pane_height, sy), sy);
        let otherh = resolve_other(mainh, sy, &opts.other_pane_height);
        let sx = (others as u32 * (PANE_MINIMUM + 1))
            .saturating_sub(1)
            .max(win.0);

        let mut root = Cell::Node {
            dir: Dir::Col,
            w: sx,
            h: mainh + otherh + 1,
            children: vec![Cell::leaf(&ids[0], sx, mainh)],
        };
        if others == 1 {
            root.push_child(Cell::leaf(&ids[1], sx, otherh));
        } else {
            let mut other = Cell::Node {
                dir: Dir::Row,
                w: sx,
                h: otherh,
                children: ids[1..]
                    .iter()
                    .map(|id| Cell::leaf(id, PANE_MINIMUM, otherh))
                    .collect(),
            };
            // Row-direction spread: tmux's border compensation lives only
            // in the stacked branch, so edge flags are irrelevant here;
            // pass the walk values anyway (top: other is not the root's
            // first child → false; bottom: it is the last → true).
            spread_node(&mut other, false, true, opts.border_status)?;
            root.push_child(other);
        }
        Some(root)
    }
}

/// `mainh + PANE_MINIMUM >= sy` clamp from `layout_set_main_*`: when the
/// configured main size would leave no room for the other pane, pin the
/// other pane at `PANE_MINIMUM` and give main the rest.
fn clamp_main(mainh: u32, sy: u32) -> u32 {
    if mainh + PANE_MINIMUM >= sy {
        if sy <= PANE_MINIMUM + PANE_MINIMUM {
            PANE_MINIMUM
        } else {
            sy - PANE_MINIMUM
        }
    } else {
        mainh
    }
}

/// `other-pane-*` resolution from `layout_set_main_*`: unset or zero
/// derives the other size from main; an oversize other (or one that
/// would shrink main below its resolved size) is clamped back to
/// `base - main`.
fn resolve_other(main: u32, base: u32, raw: &str) -> u32 {
    match parse_size_or_pct(raw, base) {
        None => base - main,
        Some(0) => base - main,
        Some(other) => {
            if other > base || base - other < main {
                base - main
            } else {
                base - other
            }
        }
    }
}

/// `layout_set_tiled`: a near-square grid, row-major over the window's
/// pane order, leftover columns/rows given to the last cell of each row
/// and to the last row.
fn tiled_tree(ids: &[String], win: (u32, u32), opts: &LayoutOptions) -> Option<Cell> {
    let n = ids.len();
    if n == 0 {
        return None;
    }
    if n == 1 {
        return Some(Cell::leaf(&ids[0], win.0, win.1));
    }
    let n = n as u32;

    // How many rows and columns are wanted (tmux's loop; 0 max columns
    // means unlimited).
    let mut rows: u32 = 1;
    let mut columns: u32 = 1;
    while rows * columns < n {
        rows += 1;
        if rows * columns < n && (opts.tiled_max_columns == 0 || columns < opts.tiled_max_columns) {
            columns += 1;
        }
    }

    let width = ((win.0 - (columns - 1)) / columns).max(PANE_MINIMUM);
    let height = ((win.1 - (rows - 1)) / rows).max(PANE_MINIMUM);

    let sx = ((width + 1) * columns - 1).max(win.0);
    let sy = ((height + 1) * rows - 1).max(win.1);

    let mut root = Cell::Node {
        dir: Dir::Col,
        w: sx,
        h: sy,
        children: Vec::new(),
    };

    let mut next = 0usize;
    for row in 0..rows {
        let remaining = n - row * columns;
        if remaining == 0 {
            break;
        }
        let count = remaining.min(columns) as usize;
        // A single-cell row (or a single-column layout) becomes a direct
        // full-width leaf, exactly as tmux does.
        if remaining == 1 || columns == 1 {
            root.push_child(Cell::leaf(&ids[next], win.0, height));
            next += 1;
            continue;
        }
        let mut row_node = Cell::Node {
            dir: Dir::Row,
            w: win.0,
            h: height,
            children: Vec::new(),
        };
        for _ in 0..count {
            row_node.push_child(Cell::leaf(&ids[next], width, height));
            next += 1;
        }
        // Leftover columns go to the row's last cell.
        let used = count as u32 * (width + 1) - 1;
        if win.0 > used {
            row_node.widen_last(win.0 - used);
        }
        root.push_child(row_node);
    }

    // Leftover rows go to the last row as a whole (node or lone leaf).
    let used = rows * height + rows - 1;
    if win.1 > used {
        root.grow_last_col(win.1 - used);
    }

    Some(root)
}

impl Cell {
    fn push_child(&mut self, child: Cell) {
        if let Cell::Node { children, .. } = self {
            children.push(child);
        }
    }

    /// `layout_resize_adjust(..., LAYOUT_LEFTRIGHT, delta)` on the last
    /// child: widen it (and only it) by `delta`.
    fn widen_last(&mut self, delta: u32) {
        if let Cell::Node { children, .. } = self {
            if let Some(last) = children.last_mut() {
                match last {
                    Cell::Leaf { w, .. } => *w += delta,
                    Cell::Node { w, .. } => *w += delta,
                }
            }
        }
    }

    /// `layout_resize_adjust(..., LAYOUT_TOPBOTTOM, delta)` on the last
    /// child of the root: the row's own height grows, and because the
    /// row runs in the other direction, each of its leaves grows too.
    fn grow_last_col(&mut self, delta: u32) {
        if let Cell::Node { children, .. } = self {
            if let Some(last) = children.last_mut() {
                match last {
                    Cell::Leaf { h, .. } => *h += delta,
                    Cell::Node { h, children, .. } => {
                        *h += delta;
                        for child in children {
                            if let Cell::Leaf { h: leaf_h, .. } = child {
                                *leaf_h += delta;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Port of `layout_spread_cell` for the trees this module generates (all
/// children tiled, no floating panes). `node_top` / `node_bottom` are the
/// node's own window-edge flags from tmux's `layout_cell_is_top` /
/// `is_bottom` walk; the root passes `true, true`. Returns `None` when
/// the spread is impossible (mirroring tmux's bail-outs) so callers can
/// fall back to the stock command.
fn spread_node(
    node: &mut Cell,
    node_top: bool,
    node_bottom: bool,
    status: BorderStatus,
) -> Option<()> {
    let (size, n) = match node {
        Cell::Node {
            dir,
            w,
            h,
            children,
        } => {
            let n = children.len();
            if n <= 1 {
                return None;
            }
            let raw = match dir {
                Dir::Row => *w,
                Dir::Col => {
                    // With a pane status line on this node's window edge,
                    // tmux spreads one row less; the edge leaves get it
                    // back via the border bonus.
                    let mut size = *h;
                    if status == BorderStatus::Top && node_top {
                        size = size.saturating_sub(1);
                    }
                    if status == BorderStatus::Bottom && node_bottom {
                        size = size.saturating_sub(1);
                    }
                    size
                }
            };
            (raw, n)
        }
        Cell::Leaf { .. } => return None,
    };

    let n_u32 = u32::try_from(n).ok()?;
    if size < n_u32 - 1 {
        return None;
    }
    let each = (size - (n_u32 - 1)) / n_u32;
    if each == 0 {
        return None;
    }
    let remainder = size - n_u32 * (each + 1) + 1;

    if let Cell::Node { dir, children, .. } = node {
        let node_dir = *dir;
        for (i, child) in children.iter_mut().enumerate() {
            let mut this = each;
            // Border bonus, Col direction only (see spread_sizes).
            if node_dir == Dir::Col {
                let is_first = i == 0;
                let is_last = i + 1 == n;
                let child_top = edge_flag(node_dir, is_first, node_top);
                let child_bottom = edge_flag(node_dir, is_last, node_bottom);
                if (status == BorderStatus::Top && child_top)
                    || (status == BorderStatus::Bottom && child_bottom)
                {
                    this += 1;
                }
            }
            if (i as u32) < remainder {
                this += 1;
            }
            match (node_dir, child) {
                (Dir::Row, Cell::Leaf { w, .. }) | (Dir::Row, Cell::Node { w, .. }) => *w = this,
                (Dir::Col, Cell::Leaf { h, .. }) | (Dir::Col, Cell::Node { h, .. }) => *h = this,
            }
        }
    }
    Some(())
}

// ─── Sidebar insertion ──────────────────────────────────────────────

/// Wrap the base tree with a full-height sidebar leaf at the window
/// edge, widening the root to the full window width. The base tree must
/// already be built at `(window_width - target - 1, window_height)` so
/// the sum tiles the window exactly.
fn with_sidebar(
    base: Cell,
    sidebar_id: &str,
    target: u32,
    win: (u32, u32),
    position: crate::cli::toggle::SidebarPosition,
) -> Cell {
    let side = Cell::leaf(sidebar_id, target, win.1);
    let base_w = base.w();
    let (children, w) = match position {
        crate::cli::toggle::SidebarPosition::Left => (vec![side, base], target + base_w + 1),
        crate::cli::toggle::SidebarPosition::Right => (vec![base, side], base_w + target + 1),
    };
    Cell::Node {
        dir: Dir::Row,
        w,
        h: win.1,
        children,
    }
}

/// Stamp each leaf with the `i` (pane index) tmux's parser maps back to
/// the pane in window listing order: cells are sorted by `i` and filled
/// with panes in listing order, so a leaf's `i` must be the listing
/// position of the pane meant to occupy it. Sidebar leaves keep their
/// own listing position (the sidebar is spatially at the edge but its
/// pane may sit anywhere in the creation order); the remaining leaves
/// consume the remaining positions in tree order.
fn stamp_indices(
    root: &mut Cell,
    pane_order: &[String],
    sidebar_ids: &HashSet<String>,
) -> Option<HashMap<String, u32>> {
    let position: HashMap<&str, u32> = pane_order
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i as u32))
        .collect();
    let mut next_regular = 0usize;
    let mut indices = HashMap::new();
    let complete = walk_leaves_mut(root, &mut |leaf| {
        let Cell::Leaf { id, .. } = leaf else {
            return false;
        };
        let idx = if sidebar_ids.contains(id) {
            match position.get(id.as_str()) {
                Some(idx) => *idx,
                None => return false,
            }
        } else {
            // Skip over sidebar positions in the listing order.
            while next_regular < pane_order.len() && sidebar_ids.contains(&pane_order[next_regular])
            {
                next_regular += 1;
            }
            match pane_order.get(next_regular) {
                // The stamped index is the pane's listing position.
                Some(_) => {
                    let idx = next_regular as u32;
                    next_regular += 1;
                    idx
                }
                None => return false,
            }
        };
        if indices.insert(id.clone(), idx).is_some() {
            return false;
        }
        true
    });
    if !complete {
        return None;
    }
    Some(indices)
}

/// Visit every leaf mutable, depth-first in tree order. The callback
/// returning `false` aborts the walk (reported as `false` overall).
fn walk_leaves_mut(cell: &mut Cell, f: &mut dyn FnMut(&mut Cell) -> bool) -> bool {
    match cell {
        Cell::Leaf { .. } => f(cell),
        Cell::Node { children, .. } => children.iter_mut().all(|child| walk_leaves_mut(child, f)),
    }
}

// ─── Validation and serialization ───────────────────────────────────

/// Mirror tmux's `layout_check` plus its JSON parse preconditions: node
/// children tile the parent exactly, every node has at least two
/// children, and leaves are at least `PANE_MINIMUM` in both dimensions.
fn validate_tree(cell: &Cell) -> Result<(), &'static str> {
    match cell {
        Cell::Leaf { w, h, .. } => {
            if *w < PANE_MINIMUM || *h < PANE_MINIMUM {
                return Err("cell below PANE_MINIMUM");
            }
            Ok(())
        }
        Cell::Node {
            dir,
            w,
            h,
            children,
        } => {
            if children.len() < 2 {
                return Err("node needs more than one child");
            }
            match dir {
                Dir::Row => {
                    let mut sum = 0u32;
                    for child in children {
                        if child.h() != *h {
                            return Err("row child height mismatch");
                        }
                        sum = sum.checked_add(child.w() + 1).ok_or("size overflow")?;
                    }
                    if sum - 1 != *w {
                        return Err("row children do not tile the parent");
                    }
                }
                Dir::Col => {
                    let mut sum = 0u32;
                    for child in children {
                        if child.w() != *w {
                            return Err("column child width mismatch");
                        }
                        sum = sum.checked_add(child.h() + 1).ok_or("size overflow")?;
                    }
                    if sum - 1 != *h {
                        return Err("column children do not tile the parent");
                    }
                }
            }
            for child in children {
                validate_tree(child)?;
            }
            Ok(())
        }
    }
}

/// Serialize a validated tree as tmux's JSON v2 layout, with offsets
/// computed the way `layout_fix_offsets` does and `i` values from
/// `indices`. The `a` (active) and `l` (last-visited) fields are omitted:
/// tmux keeps the current active pane when no cell claims it.
fn serialize_tree(root: &Cell, indices: &HashMap<String, u32>) -> String {
    let mut out = String::from("{\"V\":2,\"L\":");
    write_cell(&mut out, root, 0, 0, indices);
    out.push('}');
    out
}

fn write_cell(out: &mut String, cell: &Cell, x: u32, y: u32, indices: &HashMap<String, u32>) {
    match cell {
        Cell::Leaf { id, w, h } => {
            let index = indices.get(id).copied().unwrap_or(0);
            out.push_str(&format!(
                "{{\"t\":\"p\",\"w\":{w},\"h\":{h},\"x\":{x},\"y\":{y},\"i\":{index}}}"
            ));
        }
        Cell::Node {
            dir,
            w,
            h,
            children,
        } => {
            out.push_str(&format!(
                "{{\"t\":\"{}\",\"w\":{w},\"h\":{h},\"x\":{x},\"y\":{y},\"c\":[",
                dir.json_tag()
            ));
            let mut offset = 0u32;
            for (i, child) in children.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let (cx, cy) = match dir {
                    Dir::Row => (offset, y),
                    Dir::Col => (x, offset),
                };
                write_cell(out, child, cx, cy, indices);
                offset += match dir {
                    Dir::Row => child.w() + 1,
                    Dir::Col => child.h() + 1,
                };
            }
            out.push_str("]}");
        }
    }
}

// ─── tmux probing ───────────────────────────────────────────────────

/// One pane of the probed window, from a single `list-panes` call.
#[derive(Debug, Clone)]
struct PaneRow {
    id: String,
    role: String,
    floating: bool,
    width: u32,
}

/// Everything the layout paths need about a window, batched into three
/// tmux round trips (list-panes, display-message, show-window-options).
struct WindowProbe {
    width: u32,
    height: u32,
    panes: Vec<PaneRow>,
    base: String,
    position: crate::cli::toggle::SidebarPosition,
    width_setting: String,
    width_max: String,
    opts: LayoutOptions,
}

fn probe_window(window_id: &str) -> Option<WindowProbe> {
    let panes_out = tmux::run_tmux(&[
        "list-panes",
        "-t",
        window_id,
        "-F",
        "#{pane_id}|#{@pane_role}|#{pane_floating}|#{pane_width}",
    ])?;

    let meta = tmux::display_message(
        window_id,
        "#{window_width}|#{window_height}|#{pane_border_status}|#{@sidebar_position}|#{@sidebar_width}|#{@sidebar_width_max}|#{@sidebar_layout_base}",
    );
    let mut meta = meta.splitn(7, '|');
    let width: u32 = meta.next()?.trim().parse().ok()?;
    let height: u32 = meta.next()?.trim().parse().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let border_status = match meta.next().unwrap_or("").trim() {
        "top" => BorderStatus::Top,
        "bottom" => BorderStatus::Bottom,
        _ => BorderStatus::Off,
    };
    let position = crate::cli::toggle::SidebarPosition::from_setting(meta.next().unwrap_or(""));
    let width_setting = meta.next().unwrap_or("").to_string();
    let width_max = meta.next().unwrap_or("").to_string();
    let base = meta.next().unwrap_or("").to_string();

    // Window options: one global dump, parsed for the five settings the
    // builtins consult. Window-scoped overrides are not visible through
    // `show-window-options -gv`; the globals are what tmux's own
    // defaults live in and what virtually every config sets.
    let mut opts = LayoutOptions {
        border_status,
        ..LayoutOptions::default()
    };
    if let Some(globals) = tmux::run_tmux(&["show-window-options", "-gv"]) {
        for line in globals.lines() {
            let Some((name, value)) = line.split_once(' ') else {
                continue;
            };
            match name {
                "main-pane-height" => opts.main_pane_height = value.trim().to_string(),
                "main-pane-width" => opts.main_pane_width = value.trim().to_string(),
                "other-pane-height" => opts.other_pane_height = value.trim().to_string(),
                "other-pane-width" => opts.other_pane_width = value.trim().to_string(),
                "tiled-layout-max-columns" => {
                    opts.tiled_max_columns = value.trim().parse().unwrap_or(0);
                }
                _ => {}
            }
        }
    }

    let mut panes = Vec::new();
    for line in panes_out.lines() {
        let mut parts = line.splitn(4, '|');
        let Some(id) = parts.next() else {
            continue;
        };
        if id.is_empty() {
            continue;
        }
        panes.push(PaneRow {
            id: id.to_string(),
            role: parts.next().unwrap_or("").to_string(),
            floating: parts.next().unwrap_or("") == "1",
            width: parts.next().unwrap_or("").trim().parse().unwrap_or(0),
        });
    }
    if panes.is_empty() {
        return None;
    }

    Some(WindowProbe {
        width,
        height,
        panes,
        base,
        position,
        width_setting,
        width_max,
        opts,
    })
}

/// Whether this tmux parses JSON layout trees (the `"V":2` format,
/// introduced in tmux 3.8). Older versions fall back to the stock
/// command.
fn json_layouts_supported() -> bool {
    let version = tmux::run_tmux(&["-V"]).unwrap_or_default();
    let digits = version
        .trim()
        .trim_start_matches(|c: char| !c.is_ascii_digit());
    let mut parts = digits.split('.');
    let Some(major) = parts.next().and_then(|p| p.parse::<u32>().ok()) else {
        return false;
    };
    let minor: Option<u32> = parts.next().and_then(|p| {
        p.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok()
    });
    match minor {
        Some(minor) => major > 3 || (major == 3 && minor >= 8),
        None => major > 3,
    }
}

// ─── Command entry points ───────────────────────────────────────────

/// `layouts` — dispatch for the apply, keep, and restore-base paths.
pub(crate) fn cmd_layouts(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("--keep") => cmd_keep(args.get(1).map(String::as_str)),
        Some("--restore-base") => {
            if restore_base_builtin(args.get(1).map(String::as_str).unwrap_or("")) {
                0
            } else {
                1
            }
        }
        Some(name) => cmd_apply(name, args.get(1).map(String::as_str)),
        None => 0,
    }
}

/// Apply a sidebar-aware builtin to `window_id` (or the calling
/// context's window from the keybinding). On tmux without JSON layout
/// support, or whenever the smart tree cannot be built, falls back to
/// the plain stock `select-layout` so the key never does less than
/// tmux's own binding.
fn cmd_apply(name: &str, window_id: Option<&str>) -> i32 {
    let Some(window_id) = window_id.filter(|w| !w.is_empty()) else {
        return 0;
    };
    if !json_layouts_supported() {
        passthrough_select_layout(window_id, name);
        return 0;
    }
    let Some(probe) = probe_window(window_id) else {
        return 0;
    };
    if probe.panes.len() < 2 {
        // Nothing to arrange (tmux's builtins no-op here too).
        return 0;
    }
    if probe.panes.iter().any(|pane| pane.floating) {
        crate::debug::log("layouts: floating panes are not supported yet; skipping");
        return 0;
    }

    let sidebar_ids: Vec<String> = probe
        .panes
        .iter()
        .filter(|pane| pane.role == "sidebar")
        .map(|pane| pane.id.clone())
        .collect();
    if sidebar_ids.len() > 1 {
        // Duplicate sidebar panes (a lost dedupe race) cannot be given
        // distinct cells without rearranging them; leave the window to
        // the stock command until the next toggle collapses them.
        passthrough_select_layout(window_id, name);
        return 0;
    }
    let base_ids: Vec<String> = probe
        .panes
        .iter()
        .filter(|pane| pane.role != "sidebar")
        .map(|pane| pane.id.clone())
        .collect();
    if base_ids.is_empty() {
        return 0;
    }

    match build_window_layout(
        &probe,
        name,
        &base_ids,
        &sidebar_ids,
        sidebar_ids.first().map(String::as_str),
    ) {
        Some(json) => {
            if tmux::run_tmux(&["select-layout", "-t", window_id, &json]).is_some() {
                let _ = tmux::set_window_option(window_id, SIDEBAR_LAYOUT_BASE, name);
            }
            0
        }
        None => {
            // Degenerate geometry (window too small for the pane count,
            // unknown builtin name): the stock command is the honest
            // fallback.
            passthrough_select_layout(window_id, name);
            0
        }
    }
}

/// The `window-layout-changed` hook: re-pin a recorded window's sidebar
/// when a layout change redistributed its width. A sidebar already at
/// the target width (the common firing — our own writes, pane churn
/// elsewhere in the window) is a cheap no-op.
fn cmd_keep(window_id: Option<&str>) -> i32 {
    let Some(window_id) = window_id.filter(|w| !w.is_empty()) else {
        return 0;
    };
    let Some(probe) = probe_window(window_id) else {
        return 0;
    };
    if probe.base.trim().is_empty() {
        // The window never had a sidebar-aware builtin applied: stay out
        // of the user's geometry entirely.
        return 0;
    }
    let Some(sidebar) = probe.panes.iter().find(|pane| pane.role == "sidebar") else {
        return 0;
    };
    let Some(target) = resolve_sidebar_target(&probe) else {
        return 0;
    };
    if sidebar.width == target {
        return 0;
    }

    let sidebar_ids: Vec<String> = probe
        .panes
        .iter()
        .filter(|pane| pane.role == "sidebar")
        .map(|pane| pane.id.clone())
        .collect();
    let base_ids: Vec<String> = probe
        .panes
        .iter()
        .filter(|pane| pane.role != "sidebar")
        .map(|pane| pane.id.clone())
        .collect();
    if base_ids.is_empty() {
        return 0;
    }
    if let Some(json) = build_window_layout(
        &probe,
        probe.base.trim(),
        &base_ids,
        &sidebar_ids,
        Some(&sidebar.id),
    ) {
        let _ = tmux::run_tmux(&["select-layout", "-t", window_id, &json]);
    }
    0
}

/// Rebuild the recorded base builtin over the window's remaining panes.
/// Used by the close path instead of the pre-sidebar layout snapshot:
/// the tree is generated at the window's current size, so it is always
/// size-correct, and it restores the user's chosen arrangement rather
/// than whatever happened to be on screen when the sidebar opened.
/// Returns `false` (leaving the caller to its legacy fallback) when no
/// base is recorded or the rebuild fails.
pub(crate) fn restore_base_builtin(window_id: &str) -> bool {
    if window_id.is_empty() {
        return false;
    }
    let Some(probe) = probe_window(window_id) else {
        return false;
    };
    let base = probe.base.trim();
    if base.is_empty() {
        return false;
    }
    let ids: Vec<String> = probe
        .panes
        .iter()
        .filter(|pane| pane.role != "sidebar")
        .map(|pane| pane.id.clone())
        .collect();
    if ids.is_empty() {
        return false;
    }
    let Some(mut tree) = builtin_tree(base, &ids, (probe.width, probe.height), &probe.opts) else {
        return false;
    };
    if validate_tree(&tree).is_err() {
        return false;
    }
    let empty = HashSet::new();
    let Some(indices) = stamp_indices(&mut tree, &ids, &empty) else {
        return false;
    };
    let json = serialize_tree(&tree, &indices);
    tmux::run_tmux(&["select-layout", "-t", window_id, &json]).is_some()
}

/// Build the full-window JSON for a builtin plus its sidebar slice:
/// base tree at the reduced width, sidebar leaf wrapped at the edge,
/// indices stamped, tree validated. `None` means fall back.
fn build_window_layout(
    probe: &WindowProbe,
    name: &str,
    base_ids: &[String],
    sidebar_ids: &[String],
    sidebar_pane: Option<&str>,
) -> Option<String> {
    let sidebar_ids_set: HashSet<String> = sidebar_ids.iter().cloned().collect();
    let (base_width, sidebar_target) = match sidebar_pane {
        Some(_) => {
            let target = resolve_sidebar_target(probe)?;
            let base_width = probe.width.checked_sub(target + 1)?;
            if base_width < PANE_MINIMUM {
                return None;
            }
            (base_width, Some(target))
        }
        None => (probe.width, None),
    };

    let mut tree = builtin_tree(name, base_ids, (base_width, probe.height), &probe.opts)?;
    if let (Some(sidebar_pane), Some(target)) = (sidebar_pane, sidebar_target) {
        tree = with_sidebar(
            tree,
            sidebar_pane,
            target,
            (probe.width, probe.height),
            probe.position,
        );
    }
    validate_tree(&tree).ok()?;
    let pane_order: Vec<String> = probe.panes.iter().map(|pane| pane.id.clone()).collect();
    let indices = stamp_indices(&mut tree, &pane_order, &sidebar_ids_set)?;
    Some(serialize_tree(&tree, &indices))
}

/// Resolve the sidebar's target width in cells for the probed window
/// (percentage expansion + `@sidebar_width_max` cap, shared with the
/// toggle path). `None` when the resolution is unusable.
fn resolve_sidebar_target(probe: &WindowProbe) -> Option<u32> {
    let cells = crate::cli::toggle::sidebar_width_cells_with(
        "",
        &probe.width_setting,
        &probe.width_max,
        probe.width,
    );
    cells.trim().parse().ok()
}

/// tmux < 3.8 (no JSON layout parsing) and degenerate geometries fall
/// back to the plain stock command, so the key never does less than
/// tmux's own `select-layout <name>`.
fn passthrough_select_layout(window_id: &str, name: &str) {
    let _ = tmux::run_tmux(&["select-layout", "-t", window_id, name]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("%{i}")).collect()
    }

    fn leaves_of(cell: &Cell) -> Vec<(String, u32, u32)> {
        match cell {
            Cell::Leaf { id, w, h } => vec![(id.clone(), *w, *h)],
            Cell::Node { children, .. } => children.iter().flat_map(leaves_of).collect(),
        }
    }

    fn no_opts() -> LayoutOptions {
        LayoutOptions {
            main_pane_height: "24".into(),
            main_pane_width: "80".into(),
            ..LayoutOptions::default()
        }
    }

    // ─── spread (via even_tree, the spread_node caller) ────────────

    #[test]
    fn spread_gives_remainder_to_the_first_children() {
        // 160 columns, 2 panes: each (160-1)/2 = 79, remainder 1.
        let tree = even_tree(Dir::Row, &ids(2), (160, 40), &no_opts()).unwrap();
        assert_eq!(
            leaves_of(&tree),
            vec![("%0".into(), 80, 40), ("%1".into(), 79, 40)]
        );
        // 40 rows, 3 panes: each (40-2)/3 = 12, remainder 2.
        let tree = even_tree(Dir::Col, &ids(3), (160, 40), &no_opts()).unwrap();
        assert_eq!(
            leaves_of(&tree),
            vec![
                ("%0".into(), 160, 13),
                ("%1".into(), 160, 13),
                ("%2".into(), 160, 12),
            ]
        );
    }

    #[test]
    fn spread_adds_the_border_bonus_to_edge_children_only() {
        // pane-border-status=top: the spread runs one row short (the
        // status line) and the first stacked child gets both the border
        // bonus and the remainder: 12+1+1.
        let opts = LayoutOptions {
            border_status: BorderStatus::Top,
            ..no_opts()
        };
        let tree = even_tree(Dir::Col, &ids(3), (160, 40), &opts).unwrap();
        assert_eq!(
            leaves_of(&tree),
            vec![
                ("%0".into(), 160, 14),
                ("%1".into(), 160, 12),
                ("%2".into(), 160, 12),
            ]
        );
        // bottom: the last child gets the border bonus; the remainder
        // went to the first: 12+1(rem), 12, 12+1(border).
        let opts = LayoutOptions {
            border_status: BorderStatus::Bottom,
            ..no_opts()
        };
        let tree = even_tree(Dir::Col, &ids(3), (160, 40), &opts).unwrap();
        assert_eq!(
            leaves_of(&tree),
            vec![
                ("%0".into(), 160, 13),
                ("%1".into(), 160, 12),
                ("%2".into(), 160, 13),
            ]
        );
    }

    #[test]
    fn spread_bails_when_children_cannot_fit() {
        // Too many panes for the window: tmux's spread bails (each == 0)
        // and the builtin no-ops; the caller falls back to the stock
        // command.
        assert!(even_tree(Dir::Row, &ids(3), (4, 40), &no_opts()).is_none());
        assert!(even_tree(Dir::Col, &ids(3), (160, 4), &no_opts()).is_none());
    }

    // ─── even trees ─────────────────────────────────────────────────

    #[test]
    fn even_vertical_stacks_panes_at_full_width() {
        let tree = even_tree(Dir::Col, &ids(3), (160, 40), &no_opts()).unwrap();
        assert_eq!(tree.w(), 160);
        assert_eq!(tree.h(), 40);
        // spread(40, 3) = 13, 13, 12
        assert_eq!(
            leaves_of(&tree),
            vec![
                ("%0".into(), 160, 13),
                ("%1".into(), 160, 13),
                ("%2".into(), 160, 12),
            ]
        );
    }

    #[test]
    fn even_horizontal_places_panes_side_by_side() {
        let tree = even_tree(Dir::Row, &ids(2), (160, 40), &no_opts()).unwrap();
        assert_eq!(
            leaves_of(&tree),
            vec![("%0".into(), 80, 40), ("%1".into(), 79, 40)]
        );
    }

    // ─── main trees ─────────────────────────────────────────────────

    #[test]
    fn main_vertical_gives_the_first_pane_the_main_width() {
        // 160-wide window minus the border: sx = 159, main 80, other 79.
        let tree = main_tree(true, &ids(3), (160, 40), &no_opts()).unwrap();
        assert_eq!(tree.w(), 160);
        assert_eq!(tree.h(), 40);
        assert_eq!(
            leaves_of(&tree),
            vec![
                ("%0".into(), 80, 40),
                ("%1".into(), 79, 20),
                ("%2".into(), 79, 19),
            ]
        );
    }

    #[test]
    fn main_vertical_clamps_when_main_pane_width_would_starve_the_rest() {
        // main-pane-width 158 in a 160-wide window: sx = 159, mainw + 1
        // >= sx → mainw = 158, otherw = 1.
        let opts = LayoutOptions {
            main_pane_width: "158".into(),
            ..no_opts()
        };
        let tree = main_tree(true, &ids(2), (160, 40), &opts).unwrap();
        assert_eq!(
            leaves_of(&tree),
            vec![("%0".into(), 158, 40), ("%1".into(), 1, 40)]
        );
    }

    #[test]
    fn main_horizontal_stacks_the_main_pane_on_top() {
        // 40 rows minus the border: sy = 39, main-pane-height 24, other 15.
        // Two others spread across 160 columns: 80, 79.
        let tree = main_tree(false, &ids(3), (160, 40), &no_opts()).unwrap();
        assert_eq!(
            leaves_of(&tree),
            vec![
                ("%0".into(), 160, 24),
                ("%1".into(), 80, 15),
                ("%2".into(), 79, 15),
            ]
        );
    }

    #[test]
    fn main_horizontal_spreads_two_other_panes_across_the_bottom() {
        let tree = main_tree(false, &ids(3), (160, 40), &no_opts()).unwrap();
        let leaves = leaves_of(&tree);
        assert_eq!(leaves[1], ("%1".into(), 80, 15));
        assert_eq!(leaves[2], ("%2".into(), 79, 15));
    }

    // ─── tiled ──────────────────────────────────────────────────────

    #[test]
    fn tiled_builds_a_grid_with_the_last_cell_taking_the_leftovers() {
        // 5 panes in 100x30: rows=3, columns=2; width 49, height 9; the
        // leftover column goes to each row's last pane and the leftover
        // rows to the last (single-pane, full-width) row.
        let tree = tiled_tree(&ids(5), (100, 30), &no_opts()).unwrap();
        assert_eq!(
            leaves_of(&tree),
            vec![
                ("%0".into(), 49, 9),
                ("%1".into(), 50, 9),
                ("%2".into(), 49, 9),
                ("%3".into(), 50, 9),
                ("%4".into(), 100, 10),
            ]
        );
    }

    #[test]
    fn tiled_single_column_or_trailing_single_cell_rows_go_full_width() {
        // 3 panes in 80x30: tmux's loop lands on rows=2, columns=2 —
        // width (80-1)/2 = 39, height (30-1)/2 = 14; the leftover column
        // goes to each row's last pane and the leftover rows to the last
        // (single-pane, full-width) row.
        let tree = tiled_tree(&ids(3), (80, 30), &no_opts()).unwrap();
        assert_eq!(
            leaves_of(&tree),
            vec![
                ("%0".into(), 39, 14),
                ("%1".into(), 40, 14),
                ("%2".into(), 80, 15),
            ]
        );
        // With columns forced to 1 by tiled-layout-max-columns: rows=3,
        // height (30-2)/3 = 9, leftover rows to the last leaf.
        let opts = LayoutOptions {
            tiled_max_columns: 1,
            ..no_opts()
        };
        let tree = tiled_tree(&ids(3), (80, 30), &opts).unwrap();
        assert_eq!(
            leaves_of(&tree),
            vec![
                ("%0".into(), 80, 9),
                ("%1".into(), 80, 9),
                ("%2".into(), 80, 10),
            ]
        );
    }

    // ─── sidebar wrap + index stamping ──────────────────────────────

    #[test]
    fn with_sidebar_wraps_the_base_tree_at_the_left_edge() {
        let base = Cell::leaf("%0", 129, 40);
        let tree = with_sidebar(
            base,
            "%1",
            30,
            (160, 40),
            crate::cli::toggle::SidebarPosition::Left,
        );
        assert_eq!(tree.w(), 160);
        assert_eq!(
            leaves_of(&tree),
            vec![("%1".into(), 30, 40), ("%0".into(), 129, 40)]
        );
    }

    #[test]
    fn with_sidebar_wraps_at_the_right_edge_for_right_position() {
        let base = Cell::leaf("%0", 129, 40);
        let tree = with_sidebar(
            base,
            "%1",
            30,
            (160, 40),
            crate::cli::toggle::SidebarPosition::Right,
        );
        assert_eq!(
            leaves_of(&tree),
            vec![("%0".into(), 129, 40), ("%1".into(), 30, 40)]
        );
    }

    #[test]
    fn stamp_indices_maps_leaves_to_their_panes_listing_position() {
        // Listing order: %0 (regular), %1 (sidebar), %2 (regular). The
        // sidebar leaf sits spatially first but must carry %1's listing
        // index; the regular leaves take the remaining positions in tree
        // order.
        let pane_order = vec!["%0".to_string(), "%1".to_string(), "%2".to_string()];
        let mut sidebar = HashSet::new();
        sidebar.insert("%1".to_string());
        let mut tree = Cell::Node {
            dir: Dir::Row,
            w: 160,
            h: 40,
            children: vec![
                Cell::leaf("%1", 30, 40),
                Cell::leaf("%0", 80, 40),
                Cell::leaf("%2", 79, 40),
            ],
        };
        let indices = stamp_indices(&mut tree, &pane_order, &sidebar).unwrap();
        assert_eq!(indices["%1"], 1);
        assert_eq!(indices["%0"], 0);
        assert_eq!(indices["%2"], 2);
    }

    #[test]
    fn stamp_indices_fails_on_a_sidebar_pane_missing_from_the_listing() {
        // A sidebar leaf whose pane is not in the listing order has no
        // position to carry: the stamp must fail rather than guess.
        let pane_order = vec!["%0".to_string()];
        let mut sidebar = HashSet::new();
        sidebar.insert("%9".to_string());
        let mut tree = Cell::leaf("%9", 10, 10);
        assert!(stamp_indices(&mut tree, &pane_order, &sidebar).is_none());
    }

    // ─── validation ─────────────────────────────────────────────────

    #[test]
    fn validate_accepts_a_tiled_tree() {
        let tree = even_tree(Dir::Row, &ids(3), (100, 30), &no_opts()).unwrap();
        assert!(validate_tree(&tree).is_ok());
    }

    #[test]
    fn validate_rejects_children_that_do_not_tile_the_parent() {
        let tree = Cell::Node {
            dir: Dir::Row,
            w: 100,
            h: 30,
            children: vec![Cell::leaf("%0", 40, 30), Cell::leaf("%1", 40, 30)],
        };
        assert!(validate_tree(&tree).is_err());
    }

    #[test]
    fn validate_rejects_single_child_nodes() {
        let tree = Cell::Node {
            dir: Dir::Row,
            w: 100,
            h: 30,
            children: vec![Cell::leaf("%0", 100, 30)],
        };
        assert!(validate_tree(&tree).is_err());
    }

    #[test]
    fn validate_rejects_below_minimum_cells() {
        assert!(validate_tree(&Cell::leaf("%0", 0, 30)).is_err());
        assert!(validate_tree(&Cell::leaf("%0", 10, 0)).is_err());
    }

    // ─── serialization ──────────────────────────────────────────────

    #[test]
    fn serialize_emits_valid_json_tmux_parses() {
        let mut tree = Cell::Node {
            dir: Dir::Row,
            w: 160,
            h: 40,
            children: vec![
                Cell::leaf("%1", 30, 40),
                Cell::Node {
                    dir: Dir::Col,
                    w: 129,
                    h: 40,
                    children: vec![Cell::leaf("%0", 129, 20), Cell::leaf("%2", 129, 19)],
                },
            ],
        };
        let pane_order = vec!["%0".to_string(), "%1".to_string(), "%2".to_string()];
        let mut sidebar = HashSet::new();
        sidebar.insert("%1".to_string());
        let indices = stamp_indices(&mut tree, &pane_order, &sidebar).unwrap();
        let json = serialize_tree(&tree, &indices);
        // Must be valid JSON with the expected shape.
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["V"], 2);
        assert_eq!(value["L"]["t"], "h");
        assert_eq!(value["L"]["w"], 160);
        let children = value["L"]["c"].as_array().unwrap();
        assert_eq!(children.len(), 2);
        // Sidebar leaf first (left edge), carrying %1's listing index.
        assert_eq!(children[0]["t"], "p");
        assert_eq!(children[0]["w"], 30);
        assert_eq!(children[0]["x"], 0);
        assert_eq!(children[0]["i"], 1);
        // Stacked node offset after the sidebar + border.
        assert_eq!(children[1]["x"], 31);
        assert_eq!(children[1]["t"], "v");
        let stacked = children[1]["c"].as_array().unwrap();
        assert_eq!(stacked[0]["y"], 0);
        // Second stacked leaf sits one border row below the first.
        assert_eq!(stacked[1]["y"], 21);
    }

    #[test]
    fn serialize_omits_active_and_last_fields() {
        let indices: HashMap<String, u32> = [("%0".to_string(), 0u32)].into_iter().collect();
        let json = serialize_tree(&Cell::leaf("%0", 80, 24), &indices);
        assert!(!json.contains("\"a\""));
        assert!(!json.contains("\"l\""));
    }

    // ─── size option parsing ────────────────────────────────────────

    #[test]
    fn parse_size_or_pct_handles_cells_percent_and_garbage() {
        assert_eq!(parse_size_or_pct("24", 100), Some(24));
        assert_eq!(parse_size_or_pct("50%", 200), Some(100));
        assert_eq!(parse_size_or_pct(" 50% ", 200), Some(100));
        assert_eq!(parse_size_or_pct("150%", 100), Some(100), "clamped to base");
        assert_eq!(parse_size_or_pct("abc", 100), None);
        assert_eq!(parse_size_or_pct("", 100), None);
    }

    #[test]
    fn resolve_main_defaults_match_tmux() {
        assert_eq!(resolve_main_height("garbage", 39), 24);
        assert_eq!(resolve_main_width("garbage", 159), 80);
        assert_eq!(resolve_main_height("50%", 40), 20);
    }

    #[test]
    fn clamp_main_pins_the_other_pane_at_the_minimum() {
        // main 39 in a 39-available window: main + 1 >= 39 → main 38,
        // other pinned at 1 by the caller via resolve_other.
        assert_eq!(clamp_main(38, 39), 38);
        assert_eq!(clamp_main(39, 39), 38);
        assert_eq!(clamp_main(24, 39), 24);
        // Tiny window: everything at the minimum.
        assert_eq!(clamp_main(1, 2), 1);
    }

    #[test]
    fn resolve_other_derives_or_clamps() {
        // Unset → derive.
        assert_eq!(resolve_other(24, 39, ""), 15);
        // Explicit zero → derive.
        assert_eq!(resolve_other(24, 39, "0"), 15);
        // Oversize other → clamped back to base - main.
        assert_eq!(resolve_other(24, 39, "38"), 15);
        // Other that would shrink main below its resolved size → derive.
        assert_eq!(resolve_other(24, 39, "30"), 15);
        // Valid explicit other: main takes the rest.
        assert_eq!(resolve_other(24, 39, "10"), 29);
    }

    // ─── version gate ───────────────────────────────────────────────

    #[test]
    fn version_gate_requires_json_layout_support() {
        // The gate is `tmux -V` parsing; exercise the pure part through
        // the same arithmetic the helper uses.
        fn supports(version: &str) -> bool {
            let digits = version
                .trim()
                .trim_start_matches(|c: char| !c.is_ascii_digit());
            let mut parts = digits.split('.');
            let Some(major) = parts.next().and_then(|p| p.parse::<u32>().ok()) else {
                return false;
            };
            let minor: Option<u32> = parts.next().and_then(|p| {
                p.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .ok()
            });
            match minor {
                Some(minor) => major > 3 || (major == 3 && minor >= 8),
                None => major > 3,
            }
        }
        assert!(supports("tmux 3.8"));
        assert!(supports("tmux 3.8.1"));
        assert!(supports("tmux 4.0"));
        assert!(supports("tmux next-3.8"));
        assert!(!supports("tmux 3.7"));
        assert!(!supports("tmux 3.2"));
        assert!(!supports("tmux"));
        assert!(!supports(""));
    }

    // ─── end-to-end tree checks ─────────────────────────────────────

    #[test]
    fn full_window_layout_places_the_sidebar_at_the_edge_with_base_panes_stacked() {
        // Window 160x40, sidebar 30 at left, two base panes even-vertical:
        // sidebar (0,0,30,40); base at 129 wide stacked 20/19.
        let probe_opts = no_opts();
        let base = builtin_tree(
            "even-vertical",
            &["%0".to_string(), "%2".to_string()],
            (129, 40),
            &probe_opts,
        )
        .unwrap();
        let tree = with_sidebar(
            base,
            "%1",
            30,
            (160, 40),
            crate::cli::toggle::SidebarPosition::Left,
        );
        assert!(validate_tree(&tree).is_ok());
        let leaves = leaves_of(&tree);
        assert_eq!(leaves[0], ("%1".into(), 30, 40));
        assert_eq!(leaves[1], ("%0".into(), 129, 20));
        assert_eq!(leaves[2], ("%2".into(), 129, 19));
    }

    #[test]
    fn main_vertical_base_reduces_main_width_for_the_sidebar_slice() {
        // Base width 129 (window 160, sidebar 30): sx = 128, main 80,
        // other 48 — the main pane loses columns to the sidebar. With a
        // single other pane the strip is one full-height leaf; the
        // stacked case is covered by main_vertical_gives_the_first_pane.
        let base = builtin_tree(
            "main-vertical",
            &["%0".to_string(), "%2".to_string()],
            (129, 40),
            &no_opts(),
        )
        .unwrap();
        assert_eq!(
            leaves_of(&base),
            vec![("%0".into(), 80, 40), ("%2".into(), 48, 40)]
        );
        // Three base panes: the two others stack inside the 48-wide
        // strip, spread(40, 2) = 20, 19.
        let base = builtin_tree(
            "main-vertical",
            &["%0".to_string(), "%2".to_string(), "%3".to_string()],
            (129, 40),
            &no_opts(),
        )
        .unwrap();
        assert_eq!(
            leaves_of(&base),
            vec![
                ("%0".into(), 80, 40),
                ("%2".into(), 48, 20),
                ("%3".into(), 48, 19),
            ]
        );
    }
}
