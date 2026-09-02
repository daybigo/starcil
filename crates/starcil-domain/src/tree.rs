//! Pure split-tree layout: leaves are panes, internal nodes are binary splits
//! with an axis and a ratio. All transformations are deterministic and never
//! touch terminals; the server applies effects after the tree agrees.

use crate::ids::PaneId;
use serde::{Deserialize, Serialize};

/// Split direction as requested at the CLI/UI surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    /// New pane to the right (columns split).
    Right,
    /// New pane below (rows split).
    Down,
}

/// Focus/resize/neighbor direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    /// Children side by side (produced by `Right` splits).
    Horizontal,
    /// Children stacked (produced by `Down` splits).
    Vertical,
}

const MIN_RATIO: f32 = 0.05;
const MAX_RATIO: f32 = 0.95;

pub fn clamp_ratio(r: f32) -> f32 {
    r.clamp(MIN_RATIO, MAX_RATIO)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Node {
    Leaf(PaneId),
    Split {
        axis: Axis,
        /// Fraction of space given to `first` (clamped to [0.05, 0.95]).
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// A computed pane rectangle in cells, relative to the tab content area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn right(&self) -> u16 {
        self.x + self.width
    }
    pub fn bottom(&self) -> u16 {
        self.y + self.height
    }
}

/// Which edges of the tab area a pane touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl Node {
    pub fn leaf(pane: PaneId) -> Self {
        Node::Leaf(pane)
    }

    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(p) => out.push(*p),
            Node::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Node::Leaf(p) => *p == pane,
            Node::Split { first, second, .. } => first.contains(pane) || second.contains(pane),
        }
    }

    /// Split `target` in `direction`, inserting `new_pane` after it.
    /// `ratio` is the fraction kept by the EXISTING pane. Returns false if the
    /// target is not in this tree.
    pub fn split(&mut self, target: PaneId, direction: SplitDirection, new_pane: PaneId, ratio: f32) -> bool {
        match self {
            Node::Leaf(p) if *p == target => {
                let axis = match direction {
                    SplitDirection::Right => Axis::Horizontal,
                    SplitDirection::Down => Axis::Vertical,
                };
                *self = Node::Split {
                    axis,
                    ratio: clamp_ratio(ratio),
                    first: Box::new(Node::Leaf(target)),
                    second: Box::new(Node::Leaf(new_pane)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                first.split(target, direction, new_pane, ratio)
                    || second.split(target, direction, new_pane, ratio)
            }
        }
    }

    /// Remove `target`, collapsing its parent split. Returns:
    /// - `Ok(true)` removed and tree still has panes,
    /// - `Ok(false)` target was the only pane (caller drops the tree/tab),
    /// - `Err(())` target not found.
    pub fn remove(&mut self, target: PaneId) -> Result<bool, ()> {
        match self {
            Node::Leaf(p) if *p == target => Ok(false),
            Node::Leaf(_) => Err(()),
            Node::Split { first, second, .. } => {
                if first.contains(target) {
                    match first.remove(target) {
                        Ok(true) => Ok(true),
                        Ok(false) => {
                            *self = (**second).clone();
                            Ok(true)
                        }
                        Err(()) => Err(()),
                    }
                } else {
                    match second.remove(target) {
                        Ok(true) => Ok(true),
                        Ok(false) => {
                            *self = (**first).clone();
                            Ok(true)
                        }
                        Err(()) => Err(()),
                    }
                }
            }
        }
    }

    /// Swap the positions of two leaves. Returns false unless both exist.
    pub fn swap(&mut self, a: PaneId, b: PaneId) -> bool {
        if a == b || !self.contains(a) || !self.contains(b) {
            return false;
        }
        // Sentinel dance keeps the borrow checker happy: a -> MAX, b -> a, MAX -> b.
        let sentinel = PaneId { workspace: u64::MAX, pane: u64::MAX };
        self.replace(a, sentinel);
        self.replace(b, a);
        self.replace(sentinel, b);
        true
    }

    fn replace(&mut self, from: PaneId, to: PaneId) -> bool {
        match self {
            Node::Leaf(p) if *p == from => {
                *p = to;
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => first.replace(from, to) || second.replace(from, to),
        }
    }

    /// Adjust the ratio of the nearest ancestor split along the axis implied
    /// by `direction`, growing the target pane's side by `amount` (0..1).
    pub fn resize(&mut self, target: PaneId, direction: Direction, amount: f32) -> bool {
        let axis = match direction {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        };
        self.resize_inner(target, axis, direction, amount).unwrap_or(false)
    }

    /// Ok(true): handled. Ok(false): target found below but no matching-axis
    /// ancestor yet (keep looking upward). Err(()): target not in this subtree.
    fn resize_inner(&mut self, target: PaneId, axis: Axis, direction: Direction, amount: f32) -> Result<bool, ()> {
        match self {
            Node::Leaf(p) if *p == target => Ok(false),
            Node::Leaf(_) => Err(()),
            Node::Split { axis: node_axis, ratio, first, second } => {
                let in_first = first.contains(target);
                let child = if in_first { first } else { second };
                if !child.contains(target) {
                    return Err(());
                }
                match child.resize_inner(target, axis, direction, amount) {
                    Ok(true) => Ok(true),
                    Ok(false) => {
                        if *node_axis == axis {
                            // Growing toward `direction` moves the shared divider.
                            let grow_first = match direction {
                                Direction::Right | Direction::Down => in_first,
                                Direction::Left | Direction::Up => !in_first,
                            };
                            let delta = if grow_first { amount } else { -amount };
                            *ratio = clamp_ratio(*ratio + delta);
                            Ok(true)
                        } else {
                            Ok(false)
                        }
                    }
                    Err(()) => Err(()),
                }
            }
        }
    }

    /// Compute pane rectangles. `gap` inserts that many cells between siblings.
    pub fn rects(&self, area: Rect, gap: u16) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        self.rects_into(area, gap, &mut out);
        out
    }

    fn rects_into(&self, area: Rect, gap: u16, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            Node::Leaf(p) => out.push((*p, area)),
            Node::Split { axis, ratio, first, second } => {
                match axis {
                    Axis::Horizontal => {
                        let usable = area.width.saturating_sub(gap);
                        let (w1, w2) = split_len(usable, *ratio);
                        first.rects_into(Rect { x: area.x, y: area.y, width: w1, height: area.height }, gap, out);
                        second.rects_into(
                            Rect { x: area.x + w1 + gap, y: area.y, width: w2, height: area.height },
                            gap,
                            out,
                        );
                    }
                    Axis::Vertical => {
                        let usable = area.height.saturating_sub(gap);
                        let (h1, h2) = split_len(usable, *ratio);
                        first.rects_into(Rect { x: area.x, y: area.y, width: area.width, height: h1 }, gap, out);
                        second.rects_into(
                            Rect { x: area.x, y: area.y + h1 + gap, width: area.width, height: h2 },
                            gap,
                            out,
                        );
                    }
                }
            }
        }
    }

    /// Geometric neighbor in `direction` from `target`: the pane whose rect is
    /// adjacent across the divider with the largest perpendicular overlap.
    pub fn neighbor(&self, target: PaneId, direction: Direction, area: Rect, gap: u16) -> Option<PaneId> {
        let rects = self.rects(area, gap);
        let (_, from) = rects.iter().find(|(p, _)| *p == target)?;
        let mut best: Option<(PaneId, u32)> = None;
        for (p, r) in rects.iter().filter(|(p, _)| *p != target) {
            let adjacent = match direction {
                Direction::Left => r.right() <= from.x,
                Direction::Right => r.x >= from.right(),
                Direction::Up => r.bottom() <= from.y,
                Direction::Down => r.y >= from.bottom(),
            };
            if !adjacent {
                continue;
            }
            // Perpendicular overlap; distance breaks ties (nearest wins).
            let (overlap, dist) = match direction {
                Direction::Left | Direction::Right => (
                    overlap_len(r.y, r.bottom(), from.y, from.bottom()),
                    if direction == Direction::Left { from.x - r.right() } else { r.x - from.right() },
                ),
                Direction::Up | Direction::Down => (
                    overlap_len(r.x, r.right(), from.x, from.right()),
                    if direction == Direction::Up { from.y - r.bottom() } else { r.y - from.bottom() },
                ),
            };
            if overlap == 0 {
                continue;
            }
            let score = (overlap as u32) * 10_000 - (dist as u32).min(9_999);
            if best.map(|(_, s)| score > s).unwrap_or(true) {
                best = Some((*p, score));
            }
        }
        best.map(|(p, _)| p)
    }

    pub fn edges(&self, target: PaneId, area: Rect, gap: u16) -> Option<Edges> {
        let rects = self.rects(area, gap);
        let (_, r) = rects.iter().find(|(p, _)| *p == target)?;
        Some(Edges {
            left: r.x == area.x,
            right: r.right() == area.right(),
            top: r.y == area.y,
            bottom: r.bottom() == area.bottom(),
        })
    }
}

/// Split `usable` cells by `ratio`. Both sides get at least 1 cell when there
/// is room; deeply nested splits may degrade to zero-size panes rather than
/// panic (the UI/CLI enforce minimum sizes before splitting).
fn split_len(usable: u16, ratio: f32) -> (u16, u16) {
    if usable < 2 {
        return (usable, 0);
    }
    let first = (((usable as f32) * ratio).round() as u16).clamp(1, usable - 1);
    (first, usable - first)
}

fn overlap_len(a0: u16, a1: u16, b0: u16, b1: u16) -> u16 {
    let lo = a0.max(b0);
    let hi = a1.min(b1);
    hi.saturating_sub(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: u64) -> PaneId {
        PaneId { workspace: 1, pane: n }
    }

    fn area() -> Rect {
        Rect { x: 0, y: 0, width: 120, height: 40 }
    }

    #[test]
    fn split_and_collect() {
        let mut t = Node::leaf(p(1));
        assert!(t.split(p(1), SplitDirection::Right, p(2), 0.5));
        assert!(t.split(p(2), SplitDirection::Down, p(3), 0.5));
        assert_eq!(t.panes(), vec![p(1), p(2), p(3)]);
        assert!(!t.split(p(99), SplitDirection::Right, p(4), 0.5));
    }

    #[test]
    fn remove_collapses() {
        let mut t = Node::leaf(p(1));
        t.split(p(1), SplitDirection::Right, p(2), 0.5);
        t.split(p(2), SplitDirection::Down, p(3), 0.5);
        assert_eq!(t.remove(p(2)), Ok(true));
        assert_eq!(t.panes(), vec![p(1), p(3)]);
        assert_eq!(t.remove(p(99)), Err(()));
        assert_eq!(t.remove(p(1)), Ok(true));
        assert_eq!(t.panes(), vec![p(3)]);
        assert_eq!(t.remove(p(3)), Ok(false));
    }

    #[test]
    fn swap_leaves() {
        let mut t = Node::leaf(p(1));
        t.split(p(1), SplitDirection::Right, p(2), 0.5);
        t.split(p(2), SplitDirection::Down, p(3), 0.5);
        assert!(t.swap(p(1), p(3)));
        assert_eq!(t.panes(), vec![p(3), p(2), p(1)]);
        assert!(!t.swap(p(1), p(1)));
        assert!(!t.swap(p(1), p(99)));
    }

    #[test]
    fn rects_cover_area_without_overlap() {
        let mut t = Node::leaf(p(1));
        t.split(p(1), SplitDirection::Right, p(2), 0.6);
        t.split(p(2), SplitDirection::Down, p(3), 0.3);
        let rects = t.rects(area(), 0);
        assert_eq!(rects.len(), 3);
        let total: u32 = rects.iter().map(|(_, r)| r.width as u32 * r.height as u32).sum();
        assert_eq!(total, 120 * 40);
        // No pane is degenerate.
        for (_, r) in &rects {
            assert!(r.width >= 1 && r.height >= 1);
        }
    }

    #[test]
    fn rects_respect_gap() {
        let mut t = Node::leaf(p(1));
        t.split(p(1), SplitDirection::Right, p(2), 0.5);
        let rects = t.rects(area(), 1);
        let r1 = rects[0].1;
        let r2 = rects[1].1;
        assert_eq!(r1.right() + 1, r2.x);
    }

    #[test]
    fn neighbors() {
        let mut t = Node::leaf(p(1));
        t.split(p(1), SplitDirection::Right, p(2), 0.5);
        t.split(p(2), SplitDirection::Down, p(3), 0.5);
        let a = area();
        assert_eq!(t.neighbor(p(1), Direction::Right, a, 0), Some(p(2)));
        assert_eq!(t.neighbor(p(3), Direction::Up, a, 0), Some(p(2)));
        assert_eq!(t.neighbor(p(3), Direction::Left, a, 0), Some(p(1)));
        assert_eq!(t.neighbor(p(1), Direction::Left, a, 0), None);
    }

    #[test]
    fn edges_detects_boundaries() {
        let mut t = Node::leaf(p(1));
        t.split(p(1), SplitDirection::Right, p(2), 0.5);
        let e1 = t.edges(p(1), area(), 0).unwrap();
        assert!(e1.left && e1.top && e1.bottom && !e1.right);
        let e2 = t.edges(p(2), area(), 0).unwrap();
        assert!(e2.right && e2.top && e2.bottom && !e2.left);
    }

    #[test]
    fn resize_moves_shared_divider() {
        let mut t = Node::leaf(p(1));
        t.split(p(1), SplitDirection::Right, p(2), 0.5);
        assert!(t.resize(p(1), Direction::Right, 0.1));
        match &t {
            Node::Split { ratio, .. } => assert!((*ratio - 0.6).abs() < 1e-6),
            _ => panic!(),
        }
        assert!(t.resize(p(2), Direction::Right, 0.1));
        match &t {
            Node::Split { ratio, .. } => assert!((*ratio - 0.5).abs() < 1e-6),
            _ => panic!(),
        }
        // No vertical ancestor: resizing up/down fails on a pure horizontal tree.
        assert!(!t.resize(p(1), Direction::Down, 0.1));
    }

    #[test]
    fn random_ops_never_lose_panes() {
        // Deterministic pseudo-random op fuzz (xorshift), no external dep.
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut t = Node::leaf(p(0));
        let mut live: Vec<PaneId> = vec![p(0)];
        let mut next = 1u64;
        for _ in 0..500 {
            let r = rng();
            match r % 3 {
                0 => {
                    let target = live[(r >> 8) as usize % live.len()];
                    let dir = if r & 1 == 0 { SplitDirection::Right } else { SplitDirection::Down };
                    let new = p(next);
                    next += 1;
                    assert!(t.split(target, dir, new, ((r >> 16) % 100) as f32 / 100.0));
                    live.push(new);
                }
                1 if live.len() > 1 => {
                    let idx = (r >> 8) as usize % live.len();
                    let target = live.remove(idx);
                    assert_eq!(t.remove(target), Ok(true));
                }
                _ if live.len() > 1 => {
                    let a = live[(r >> 8) as usize % live.len()];
                    let b = live[(r >> 24) as usize % live.len()];
                    t.swap(a, b);
                }
                _ => {}
            }
            let mut got = t.panes();
            let mut want = live.clone();
            got.sort();
            want.sort();
            assert_eq!(got, want, "pane set diverged");
            let rects = t.rects(area(), 0);
            assert_eq!(rects.len(), live.len());
        }
    }
}
