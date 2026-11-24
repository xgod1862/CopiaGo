//! Resolve a notification-area anchor in virtual-screen pixels.

use windows::{
    Win32::{
        Foundation::{E_FAIL, POINT, RECT},
        Graphics::Gdi::{
            GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL,
            MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, MonitorFromRect,
            MonitorFromWindow,
        },
        UI::WindowsAndMessaging::{FindWindowExW, GetCursorPos, GetWindowRect},
    },
    core::{Error, Interface, PCWSTR, Result, w},
};
use windows_numerics::Vector3;

use super::{Placement, PopupBounds, PopupScene};
use crate::animation::{Animator, Axis, MotionPolicy, Spring, Tween};

const GAP_DIPS: f32 = 12.0;
const ENTER_DIPS: f32 = 36.0;
const ENTER_SPRING: Spring = Spring::new(70, 0.72);

#[derive(Clone, Copy)]
pub(super) struct TrayPlacement {
    monitor: HMONITOR,
    icon: Option<RECT>,
    work: RECT,
    edge: Edge,
    align_to: Option<i32>,
}

impl TrayPlacement {
    pub(super) fn resolve(icon: Option<RECT>, fallback: Option<POINT>) -> Result<Self> {
        if let Some(icon) = icon.filter(|rect| valid(*rect)) {
            // SAFETY: The rectangle uses virtual-screen coordinates and is readable.
            let monitor = unsafe { MonitorFromRect(&icon, MONITOR_DEFAULTTONULL) };
            if !monitor.is_invalid()
                && let Ok(anchor) = Self::on_monitor(monitor, Some(icon))
            {
                return Ok(anchor);
            }
        }

        let (point, flags) = fallback
            .or_else(cursor_position)
            .map_or((POINT::default(), MONITOR_DEFAULTTOPRIMARY), |point| {
                (point, MONITOR_DEFAULTTONEAREST)
            });
        // SAFETY: The point and fallback flags are passed by value.
        Self::on_monitor(unsafe { MonitorFromPoint(point, flags) }, None)
    }

    pub(super) fn refresh(self, fallback: POINT) -> Result<Self> {
        Self::on_monitor(self.monitor, self.icon).or_else(|_| Self::resolve(None, Some(fallback)))
    }

    fn on_monitor(monitor: HMONITOR, icon: Option<RECT>) -> Result<Self> {
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: info is a writable MONITORINFO with its cbSize initialized.
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            return Err(Error::from_win32());
        }
        if !valid(info.rcMonitor) {
            return Err(Error::new(E_FAIL, "Invalid display rectangle"));
        }
        Ok(Self::on_display(
            monitor,
            info.rcMonitor,
            intersection(info.rcMonitor, info.rcWork).unwrap_or(info.rcMonitor),
            taskbar_on(monitor, info.rcMonitor),
            icon,
        ))
    }

    fn on_display(
        monitor: HMONITOR,
        screen: RECT,
        work: RECT,
        bar: Option<(Edge, RECT)>,
        icon: Option<RECT>,
    ) -> Self {
        let edge = bar.map_or_else(
            || Edge::from_work_area(screen, work, icon),
            |(edge, _)| edge,
        );
        // A revealed auto-hide taskbar can extend into the reported work area.
        let work = bar
            .and_then(|(_, bar)| edge.exclude_bar(work, bar))
            .unwrap_or(work);
        let strip = edge.strip(screen, work);
        // A hidden icon may have a rectangle inside the overflow flyout. It
        // still identifies the display but cannot align a card along the bar.
        let align_to = icon
            .filter(|rect| intersection(*rect, strip).is_some())
            .map(|rect| match edge {
                Edge::Top | Edge::Bottom => rect.right,
                Edge::Left | Edge::Right => rect.bottom,
            });
        Self {
            monitor,
            icon,
            work,
            edge,
            align_to,
        }
    }

    pub(super) fn dpi_sample(self) -> POINT {
        POINT {
            x: self.work.left.midpoint(self.work.right),
            y: self.work.top.midpoint(self.work.bottom),
        }
    }

    pub(super) fn place(self, bounds: PopupBounds, dpi: u32) -> Result<Placement> {
        let (width, height) = bounds.pixel_size(dpi)?;
        let gap = (GAP_DIPS * dpi as f32 / super::DEFAULT_DPI as f32).round() as i64;
        let (left, right) = origin_limits(self.work.left, self.work.right, width, gap);
        let (top, bottom) = origin_limits(self.work.top, self.work.bottom, height, gap);
        let align = |extent, min, max| {
            self.align_to
                .map_or(max, |end| i64::from(end) - i64::from(extent))
                .clamp(min, max)
        };
        let (x, y) = match self.edge {
            Edge::Left => (left, align(height, top, bottom)),
            Edge::Top => (align(width, left, right), top),
            Edge::Right => (right, align(height, top, bottom)),
            Edge::Bottom => (align(width, left, right), bottom),
        };
        Placement::new(
            POINT {
                x: x as i32,
                y: y as i32,
            },
            bounds,
            dpi,
        )
    }

    pub(super) fn animate_in(self, scene: &PopupScene, policy: MotionPolicy) -> Result<()> {
        if !policy.allows_motion() {
            return Ok(());
        }
        let animator = Animator::new(scene.compositor(), policy)?;
        let visual = scene.motion_root();
        let size = visual.Size()?;
        let (axis, offset, pivot) = self.edge.entrance(size.X, size.Y);
        visual.SetCenterPoint(pivot)?;
        visual.SetOffset(offset)?;
        visual.SetScale(Vector3::new(0.94, 0.94, 1.0))?;
        visual.SetOpacity(0.0)?;
        animator.spring(
            &visual.cast()?,
            "Offset",
            &[(axis, 0.0)],
            ENTER_SPRING,
            || visual.SetOffset(Vector3::default()),
        )?;
        animator.spring_scale(visual, 1.0, ENTER_SPRING)?;
        animator.opacity(visual, 1.0, Tween::new(220))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Edge {
    Left,
    Top,
    Right,
    Bottom,
}

impl Edge {
    fn from_bar(screen: RECT, bar: RECT) -> Option<Self> {
        if !valid(bar) || intersection(screen, bar).is_none() {
            return None;
        }
        let width = span(bar.left, bar.right);
        let height = span(bar.top, bar.bottom);
        if width >= height {
            Some(
                if bar.top.midpoint(bar.bottom) < screen.top.midpoint(screen.bottom) {
                    Self::Top
                } else {
                    Self::Bottom
                },
            )
        } else {
            Some(
                if bar.left.midpoint(bar.right) < screen.left.midpoint(screen.right) {
                    Self::Left
                } else {
                    Self::Right
                },
            )
        }
    }

    fn from_work_area(screen: RECT, work: RECT, icon: Option<RECT>) -> Self {
        // If there are multiple reserved edges, prefer the one containing
        // the icon. Fall back to the widest reservation, then the bottom.
        let mut edge = Self::Bottom;
        let mut rank = (false, 0);
        for candidate in [Self::Bottom, Self::Top, Self::Left, Self::Right] {
            let strip = candidate.strip(screen, work);
            let size = match candidate {
                Self::Top | Self::Bottom => span(strip.top, strip.bottom),
                Self::Left | Self::Right => span(strip.left, strip.right),
            };
            let contains_icon = icon.is_some_and(|icon| intersection(strip, icon).is_some());
            if (contains_icon, size) > rank {
                edge = candidate;
                rank = (contains_icon, size);
            }
        }
        edge
    }

    fn strip(self, screen: RECT, work: RECT) -> RECT {
        match self {
            Self::Left => RECT {
                right: work.left,
                ..screen
            },
            Self::Top => RECT {
                bottom: work.top,
                ..screen
            },
            Self::Right => RECT {
                left: work.right,
                ..screen
            },
            Self::Bottom => RECT {
                top: work.bottom,
                ..screen
            },
        }
    }

    fn exclude_bar(self, mut work: RECT, bar: RECT) -> Option<RECT> {
        match self {
            Self::Left => work.left = work.left.max(bar.right),
            Self::Top => work.top = work.top.max(bar.bottom),
            Self::Right => work.right = work.right.min(bar.left),
            Self::Bottom => work.bottom = work.bottom.min(bar.top),
        }
        valid(work).then_some(work)
    }

    fn entrance(self, width: f32, height: f32) -> (Axis, Vector3, Vector3) {
        match self {
            Self::Left => (
                Axis::X,
                Vector3::new(-ENTER_DIPS, 0.0, 0.0),
                Vector3::new(0.0, height / 2.0, 0.0),
            ),
            Self::Top => (
                Axis::Y,
                Vector3::new(0.0, -ENTER_DIPS, 0.0),
                Vector3::new(width / 2.0, 0.0, 0.0),
            ),
            Self::Right => (
                Axis::X,
                Vector3::new(ENTER_DIPS, 0.0, 0.0),
                Vector3::new(width, height / 2.0, 0.0),
            ),
            Self::Bottom => (
                Axis::Y,
                Vector3::new(0.0, ENTER_DIPS, 0.0),
                Vector3::new(width / 2.0, height, 0.0),
            ),
        }
    }
}

fn taskbar_on(monitor: HMONITOR, screen: RECT) -> Option<(Edge, RECT)> {
    // Explorer exposes a separate window for each taskbar. Fall back to the
    // monitor's work-area insets when these window classes are unavailable.
    for class in [w!("Shell_TrayWnd"), w!("Shell_SecondaryTrayWnd")] {
        let mut previous = None;
        // SAFETY: Search desktop windows by a static class with no title filter.
        while let Ok(window) = unsafe { FindWindowExW(None, previous, class, PCWSTR::null()) } {
            previous = Some(window);
            let mut rect = RECT::default();
            // SAFETY: The APIs only inspect this HWND and write to a valid RECT.
            let on_monitor = unsafe {
                MonitorFromWindow(window, MONITOR_DEFAULTTONULL) == monitor
                    && GetWindowRect(window, &mut rect).is_ok()
            };
            if on_monitor && let Some(edge) = Edge::from_bar(screen, rect) {
                return Some((edge, rect));
            }
        }
    }
    None
}

fn cursor_position() -> Option<POINT> {
    let mut point = POINT::default();
    // SAFETY: The point is a writable output parameter.
    unsafe { GetCursorPos(&mut point) }.ok().map(|()| point)
}

fn origin_limits(start: i32, end: i32, extent: i32, gap: i64) -> (i64, i64) {
    // Use the full working area when the card is too big for both margins.
    let spare = (span(start, end) - i64::from(extent)).max(0);
    let gap = gap.min(spare / 2);
    (i64::from(start) + gap, i64::from(start) + spare - gap)
}

fn span(start: i32, end: i32) -> i64 {
    i64::from(end) - i64::from(start)
}

fn intersection(a: RECT, b: RECT) -> Option<RECT> {
    let rect = RECT {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right),
        bottom: a.bottom.min(b.bottom),
    };
    valid(rect).then_some(rect)
}

fn valid(rect: RECT) -> bool {
    rect.left < rect.right && rect.top < rect.bottom
}

#[cfg(test)]
mod tests;
