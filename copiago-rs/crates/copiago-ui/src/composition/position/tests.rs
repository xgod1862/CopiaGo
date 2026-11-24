use super::*;

mod live;

const CARD: PopupBounds = PopupBounds::new(392.0, 452.0, 24.0);
const DISPLAY: RECT = rect(-2560, -1440, 0, 0);

const fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT {
        left,
        top,
        right,
        bottom,
    }
}

fn card_box(placement: Placement) -> RECT {
    rect(
        placement.position.x,
        placement.position.y,
        placement.position.x + placement.width,
        placement.position.y + placement.height,
    )
}

fn anchor(work: RECT, bar: Option<RECT>, icon: Option<RECT>) -> TrayPlacement {
    TrayPlacement::on_display(
        HMONITOR::default(),
        DISPLAY,
        work,
        bar.and_then(|bar| Edge::from_bar(DISPLAY, bar).map(|edge| (edge, bar))),
        icon,
    )
}

#[test]
fn all_taskbar_edges_anchor_inside_the_correct_monitor_at_each_dpi() {
    for (edge, bar, work, icon, offset, pivot) in [
        (
            Edge::Left,
            rect(-2560, -1440, -2512, 0),
            rect(-2512, -1440, 0, 0),
            rect(-2552, -400, -2528, -376),
            Vector3::new(-18.0, 0.0, 0.0),
            Vector3::new(0.0, 226.0, 0.0),
        ),
        (
            Edge::Top,
            rect(-2560, -1440, 0, -1392),
            rect(-2560, -1392, 0, 0),
            rect(-850, -1428, -826, -1404),
            Vector3::new(0.0, -18.0, 0.0),
            Vector3::new(196.0, 0.0, 0.0),
        ),
        (
            Edge::Right,
            rect(-48, -1440, 0, 0),
            rect(-2560, -1440, -48, 0),
            rect(-42, -400, -18, -376),
            Vector3::new(18.0, 0.0, 0.0),
            Vector3::new(392.0, 226.0, 0.0),
        ),
        (
            Edge::Bottom,
            rect(-2560, -48, 0, 0),
            rect(-2560, -1440, 0, -48),
            rect(-850, -42, -826, -18),
            Vector3::new(0.0, 18.0, 0.0),
            Vector3::new(196.0, 452.0, 0.0),
        ),
    ] {
        assert_eq!(Edge::from_bar(DISPLAY, bar), Some(edge));
        assert_eq!(Edge::from_work_area(DISPLAY, work, Some(icon)), edge);
        let (axis, actual_offset, actual_pivot) = edge.entrance(CARD.width, CARD.height);
        assert_eq!(actual_offset, offset);
        assert_eq!(actual_pivot, pivot);
        assert_eq!(matches!(axis, Axis::X), offset.X != 0.0);

        let anchor = anchor(work, Some(bar), Some(icon));
        for (dpi, width, height, gap) in
            [(96, 392, 452, 12), (144, 588, 678, 18), (192, 784, 904, 24)]
        {
            let placement = anchor.place(CARD, dpi).unwrap();
            let expected = match edge {
                Edge::Left => rect(
                    work.left + gap,
                    work.bottom - gap - height,
                    work.left + gap + width,
                    work.bottom - gap,
                ),
                Edge::Top => rect(
                    work.right - gap - width,
                    work.top + gap,
                    work.right - gap,
                    work.top + gap + height,
                ),
                Edge::Right => rect(
                    work.right - gap - width,
                    work.bottom - gap - height,
                    work.right - gap,
                    work.bottom - gap,
                ),
                Edge::Bottom => rect(
                    work.right - gap - width,
                    work.bottom - gap - height,
                    work.right - gap,
                    work.bottom - gap,
                ),
            };
            assert_eq!(card_box(placement), expected);
            assert_eq!(intersection(expected, work), Some(expected));
        }
    }
}

#[test]
fn any_icon_position_uses_the_corner_of_the_actual_taskbar_edge() {
    let work = rect(-2500, -1440, 0, -48);
    let bar = rect(-2560, -48, 0, 0);
    let flyout_icon = rect(-350, -160, -326, -136);
    let stale_icon = rect(120, 140, 144, 164);
    for icon in [Some(flyout_icon), Some(stale_icon), None] {
        let anchor = anchor(work, Some(bar), icon);
        assert_eq!(anchor.edge, Edge::Bottom);
        assert_eq!(
            card_box(anchor.place(CARD, 96).unwrap()),
            rect(-404, -512, -12, -60)
        );
    }
    let icon = rect(-140, -42, -116, -18);
    let anchor = anchor(work, None, Some(icon));
    assert_eq!(anchor.edge, Edge::Bottom);
    assert_eq!(card_box(anchor.place(CARD, 96).unwrap()).right, -12);
}

#[test]
fn revealed_autohide_bar_and_tight_work_areas_are_respected() {
    let bar = rect(-2560, -1440, -2500, 0);
    let icon = rect(-2550, -500, -2526, -476);
    let tray = anchor(DISPLAY, Some(bar), Some(icon));
    assert_eq!(tray.edge, Edge::Left);
    assert_eq!(card_box(tray.place(CARD, 144).unwrap()).left, -2482);

    let work = rect(-400, -480, 0, 0);
    let tray = anchor(work, None, None);
    assert_eq!(card_box(tray.place(CARD, 96).unwrap()).left, -396);
    let larger = tray.place(CARD, 192).unwrap();
    assert_eq!(larger.position, POINT { x: -400, y: -480 });
    assert!(tray.place(CARD, 0).is_err());
}
