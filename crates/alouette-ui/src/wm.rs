//! The window manager: one set of windows, three layouts.
//!
//! * **Desktop**: windows float with their own position and size, stacked in z-order.
//! * **Tiled**: a main pane on the left and the rest stacked on the right.
//! * **Tabs**: one window fills the screen; a tab bar lists the others.
//!
//! Windows keep a stable creation order for Tiled and Tabs, and a separate stacking
//! order for Desktop, so switching layouts never shuffles anything.

use alouette_core::config::LayoutKind;
use ratatui::layout::Rect;

use crate::window::Window;

/// Identifies a window for its whole life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WinId(pub u64);

pub struct Managed {
    pub id: WinId,
    pub win: Box<dyn Window>,
    /// Position and size in the Desktop layout.
    pub float: Rect,
    /// The size before zooming, when zoomed in the Desktop layout.
    pub restore: Option<Rect>,
}

/// Directions for focus movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Height of the tab bar in the Tabs layout.
pub const TAB_BAR_HEIGHT: u16 = 1;
/// Smallest window allowed in the Desktop layout.
pub const MIN_W: u16 = 16;
pub const MIN_H: u16 = 5;

pub struct Wm {
    pub layout: LayoutKind,
    windows: Vec<Managed>,
    /// Desktop stacking order, bottom to top. The top window has focus.
    z: Vec<WinId>,
    next_id: u64,
    cascade: u16,
    /// Share of the width the main pane takes in the Tiled layout.
    pub master_ratio: f32,
    /// In the Tiled layout, show only the focused window.
    pub tiled_zoom: bool,
}

impl Wm {
    pub fn new(layout: LayoutKind) -> Self {
        Self { layout, windows: vec![], z: vec![], next_id: 1, cascade: 0, master_ratio: 0.55, tiled_zoom: false }
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// Add a window, give it focus, and place it on the desktop with a cascade offset.
    pub fn add(&mut self, win: Box<dyn Window>, desktop: Rect) -> WinId {
        let id = WinId(self.next_id);
        self.next_id += 1;
        let w = (desktop.width * 3 / 4).clamp(MIN_W.min(desktop.width), 100.min(desktop.width));
        let h = (desktop.height * 3 / 4).clamp(MIN_H.min(desktop.height), 36.min(desktop.height));
        let max_x = desktop.width.saturating_sub(w);
        let max_y = desktop.height.saturating_sub(h);
        let step = self.cascade;
        self.cascade = (self.cascade + 1) % 8;
        let float = Rect {
            x: desktop.x + (2 + step * 2).min(max_x),
            y: desktop.y + (1 + step).min(max_y),
            width: w,
            height: h,
        };
        self.windows.push(Managed { id, win, float, restore: None });
        self.z.push(id);
        id
    }

    /// Remove a window and return it. Focus moves to the next window down the stack.
    pub fn remove(&mut self, id: WinId) -> Option<Box<dyn Window>> {
        let pos = self.windows.iter().position(|m| m.id == id)?;
        self.z.retain(|z| *z != id);
        Some(self.windows.remove(pos).win)
    }

    pub fn focused(&self) -> Option<WinId> {
        self.z.last().copied()
    }

    pub fn focus(&mut self, id: WinId) {
        if self.z.contains(&id) {
            self.z.retain(|z| *z != id);
            self.z.push(id);
        }
    }

    pub fn get(&self, id: WinId) -> Option<&Managed> {
        self.windows.iter().find(|m| m.id == id)
    }

    pub fn get_mut(&mut self, id: WinId) -> Option<&mut Managed> {
        self.windows.iter_mut().find(|m| m.id == id)
    }

    pub fn focused_mut(&mut self) -> Option<&mut Managed> {
        let id = self.focused()?;
        self.get_mut(id)
    }

    /// Windows in creation order, which is also their number (Alt-1, Alt-2 ...).
    pub fn ordered(&self) -> impl Iterator<Item = &Managed> {
        self.windows.iter()
    }

    pub fn ordered_mut(&mut self) -> impl Iterator<Item = &mut Managed> {
        self.windows.iter_mut()
    }

    /// The window's 1-based number in creation order.
    pub fn number(&self, id: WinId) -> Option<usize> {
        self.windows.iter().position(|m| m.id == id).map(|i| i + 1)
    }

    pub fn nth(&self, n: usize) -> Option<WinId> {
        self.windows.get(n).map(|m| m.id)
    }

    /// Find an open window showing this URL.
    pub fn find_url(&self, url: &str) -> Option<WinId> {
        self.windows.iter().find(|m| m.win.url().as_deref() == Some(url)).map(|m| m.id)
    }

    /// Cycle focus through windows in creation order.
    pub fn cycle(&mut self, forward: bool) {
        let n = self.windows.len();
        if n < 2 {
            return;
        }
        let Some(cur) = self.focused().and_then(|id| self.windows.iter().position(|m| m.id == id)) else { return };
        let next = if forward { (cur + 1) % n } else { (cur + n - 1) % n };
        let id = self.windows[next].id;
        self.focus(id);
    }

    /// Where each visible window goes for the current layout, drawn in this order.
    pub fn arrange(&self, area: Rect) -> Vec<(WinId, Rect)> {
        match self.layout {
            LayoutKind::Desktop => {
                self.z.iter().filter_map(|id| self.get(*id).map(|m| (*id, clamp_rect(m.float, area)))).collect()
            }
            LayoutKind::Tabs => {
                let body =
                    Rect { y: area.y + TAB_BAR_HEIGHT, height: area.height.saturating_sub(TAB_BAR_HEIGHT), ..area };
                self.focused().map(|id| vec![(id, body)]).unwrap_or_default()
            }
            LayoutKind::Tiled => {
                if self.tiled_zoom {
                    return self.focused().map(|id| vec![(id, area)]).unwrap_or_default();
                }
                tile(area, self.windows.len(), self.master_ratio)
                    .into_iter()
                    .zip(self.windows.iter())
                    .map(|(r, m)| (m.id, r))
                    .collect()
            }
        }
    }

    /// The topmost window under a screen position.
    pub fn hit(&self, area: Rect, x: u16, y: u16) -> Option<(WinId, Rect)> {
        self.arrange(area).into_iter().rev().find(|(_, r)| contains(*r, x, y))
    }

    /// Move focus to the nearest window in a direction, by window centers.
    pub fn focus_dir(&mut self, area: Rect, dir: Dir) {
        let rects = self.arrange(area);
        let Some(cur) = self.focused() else { return };
        let Some((_, cr)) = rects.iter().find(|(id, _)| *id == cur).copied() else { return };
        let (cx, cy) = center(cr);
        let best = rects
            .iter()
            .filter(|(id, _)| *id != cur)
            .filter_map(|(id, r)| {
                let (x, y) = center(*r);
                let (dx, dy) = (x - cx, y - cy);
                let ok = match dir {
                    Dir::Left => dx < 0.0,
                    Dir::Right => dx > 0.0,
                    Dir::Up => dy < 0.0,
                    Dir::Down => dy > 0.0,
                };
                // Prefer windows straight in the chosen direction.
                let (along, across) = match dir {
                    Dir::Left | Dir::Right => (dx.abs(), dy.abs()),
                    Dir::Up | Dir::Down => (dy.abs(), dx.abs()),
                };
                ok.then_some((*id, along + across * 2.0))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((id, _)) = best {
            self.focus(id);
        }
    }

    /// Zoom or restore the focused window.
    pub fn toggle_zoom(&mut self, area: Rect) {
        match self.layout {
            LayoutKind::Desktop => {
                if let Some(m) = self.focused_mut() {
                    match m.restore.take() {
                        Some(r) => m.float = r,
                        None => {
                            m.restore = Some(m.float);
                            m.float = area;
                        }
                    }
                }
            }
            LayoutKind::Tiled => self.tiled_zoom = !self.tiled_zoom,
            LayoutKind::Tabs => {}
        }
    }

    /// Re-place every window in a fresh cascade.
    pub fn cascade_all(&mut self, area: Rect) {
        self.cascade = 0;
        let order: Vec<WinId> = self.z.clone();
        for (i, id) in order.into_iter().enumerate() {
            let step = (i as u16) % 8;
            let w = (area.width * 3 / 4).max(MIN_W).min(area.width);
            let h = (area.height * 3 / 4).max(MIN_H).min(area.height);
            if let Some(m) = self.get_mut(id) {
                m.restore = None;
                m.float = Rect {
                    x: area.x + (2 + step * 2).min(area.width.saturating_sub(w)),
                    y: area.y + (1 + step).min(area.height.saturating_sub(h)),
                    width: w,
                    height: h,
                };
            }
        }
    }

    /// Move the focused desktop window by a delta, keeping it on screen.
    pub fn nudge(&mut self, area: Rect, dx: i32, dy: i32) {
        if let Some(m) = self.focused_mut() {
            m.restore = None;
            let x = (m.float.x as i32 + dx)
                .clamp(area.x as i32, (area.right() as i32 - m.float.width as i32).max(area.x as i32));
            let y = (m.float.y as i32 + dy).clamp(area.y as i32, (area.bottom() as i32 - 1).max(area.y as i32));
            m.float.x = x as u16;
            m.float.y = y as u16;
        }
    }

    /// Resize the focused desktop window by a delta, within limits.
    pub fn grow(&mut self, area: Rect, dw: i32, dh: i32) {
        if let Some(m) = self.focused_mut() {
            m.restore = None;
            let max_w = area.right().saturating_sub(m.float.x).max(MIN_W);
            let max_h = area.bottom().saturating_sub(m.float.y).max(MIN_H);
            m.float.width = (m.float.width as i32 + dw).clamp(MIN_W as i32, max_w as i32) as u16;
            m.float.height = (m.float.height as i32 + dh).clamp(MIN_H as i32, max_h as i32) as u16;
        }
    }

    /// Set the main pane share for the Tiled layout.
    pub fn adjust_master(&mut self, delta: f32) {
        self.master_ratio = (self.master_ratio + delta).clamp(0.2, 0.8);
    }
}

/// Split `area` into a main pane and a stack.
pub fn tile(area: Rect, n: usize, ratio: f32) -> Vec<Rect> {
    match n {
        0 => vec![],
        1 => vec![area],
        _ => {
            let main_w = ((area.width as f32) * ratio).round() as u16;
            let main = Rect { width: main_w, ..area };
            let rest_x = area.x + main_w;
            let rest_w = area.width.saturating_sub(main_w);
            let count = (n - 1) as u16;
            let mut out = vec![main];
            let mut y = area.y;
            for i in 0..count {
                // Spread leftover rows over the first panes.
                let h = area.height / count + u16::from(i < area.height % count);
                out.push(Rect { x: rest_x, y, width: rest_w, height: h });
                y += h;
            }
            out
        }
    }
}

/// Keep a floating window at least partly on screen and no bigger than the screen.
pub fn clamp_rect(r: Rect, area: Rect) -> Rect {
    let width = r.width.min(area.width);
    let height = r.height.min(area.height);
    let x = r.x.clamp(area.x, area.right().saturating_sub(width.min(8)).max(area.x));
    let y = r.y.clamp(area.y, area.bottom().saturating_sub(1).max(area.y));
    let width = width.min(area.right().saturating_sub(x));
    let height = height.min(area.bottom().saturating_sub(y));
    Rect { x, y, width, height }
}

pub fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
}

fn center(r: Rect) -> (f32, f32) {
    (r.x as f32 + r.width as f32 / 2.0, r.y as f32 + r.height as f32 / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::{DrawCx, Window};
    use ratatui::Frame;

    struct W(&'static str);
    impl Window for W {
        fn title(&self) -> String {
            self.0.into()
        }
        fn module(&self) -> &'static str {
            "test"
        }
        fn url(&self) -> Option<String> {
            Some(format!("test:{}", self.0))
        }
        fn draw(&mut self, _: &mut Frame, _: Rect, _: &DrawCx) {}
    }

    const AREA: Rect = Rect { x: 0, y: 1, width: 80, height: 22 };

    #[test]
    fn tile_covers_the_area_without_gaps() {
        let rects = tile(AREA, 4, 0.5);
        assert_eq!(rects.len(), 4);
        assert_eq!(rects[0], Rect { x: 0, y: 1, width: 40, height: 22 });
        let stack_h: u16 = rects[1..].iter().map(|r| r.height).sum();
        assert_eq!(stack_h, 22);
        assert!(rects[1..].iter().all(|r| r.x == 40 && r.width == 40));
    }

    #[test]
    fn focus_follows_add_remove_and_cycle() {
        let mut wm = Wm::new(LayoutKind::Desktop);
        let a = wm.add(Box::new(W("a")), AREA);
        let b = wm.add(Box::new(W("b")), AREA);
        let c = wm.add(Box::new(W("c")), AREA);
        assert_eq!(wm.focused(), Some(c));
        wm.cycle(true);
        assert_eq!(wm.focused(), Some(a));
        wm.cycle(false);
        assert_eq!(wm.focused(), Some(c));
        wm.remove(c);
        assert_eq!(wm.focused(), Some(a), "focus falls to the next window down the stack");
        assert_eq!(wm.find_url("test:b"), Some(b));
        assert_eq!(wm.number(b), Some(2));
    }

    #[test]
    fn layouts_arrange_the_same_windows() {
        let mut wm = Wm::new(LayoutKind::Desktop);
        wm.add(Box::new(W("a")), AREA);
        let b = wm.add(Box::new(W("b")), AREA);
        assert_eq!(wm.arrange(AREA).len(), 2);
        wm.layout = LayoutKind::Tiled;
        assert_eq!(wm.arrange(AREA).len(), 2);
        wm.layout = LayoutKind::Tabs;
        let tabs = wm.arrange(AREA);
        assert_eq!(tabs, vec![(b, Rect { x: 0, y: 2, width: 80, height: 21 })]);
    }

    #[test]
    fn directional_focus_in_tiled() {
        let mut wm = Wm::new(LayoutKind::Tiled);
        let a = wm.add(Box::new(W("a")), AREA);
        let b = wm.add(Box::new(W("b")), AREA);
        let c = wm.add(Box::new(W("c")), AREA);
        wm.focus(a);
        wm.focus_dir(AREA, Dir::Right);
        assert_eq!(wm.focused(), Some(b));
        wm.focus_dir(AREA, Dir::Down);
        assert_eq!(wm.focused(), Some(c));
        wm.focus_dir(AREA, Dir::Left);
        assert_eq!(wm.focused(), Some(a));
    }

    #[test]
    fn desktop_zoom_and_nudge_stay_on_screen() {
        let mut wm = Wm::new(LayoutKind::Desktop);
        wm.add(Box::new(W("a")), AREA);
        wm.toggle_zoom(AREA);
        assert_eq!(wm.arrange(AREA)[0].1, AREA);
        wm.toggle_zoom(AREA);
        wm.nudge(AREA, -500, -500);
        let r = wm.arrange(AREA)[0].1;
        assert_eq!((r.x, r.y), (0, 1));
        wm.grow(AREA, -500, -500);
        let r = wm.arrange(AREA)[0].1;
        assert_eq!((r.width, r.height), (MIN_W, MIN_H));
    }
}
