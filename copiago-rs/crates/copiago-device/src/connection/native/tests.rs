use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use copiago_core::i18n::Language;

use super::*;

struct TestTunnel<'a> {
    has_service: bool,
    closed: &'a Cell<usize>,
}

impl PasteboardTunnel for TestTunnel<'_> {
    fn has_pasteboard(&self) -> bool {
        self.has_service
    }

    async fn close(self) {
        self.closed.set(self.closed.get() + 1);
    }
}

#[tokio::test]
async fn missing_pasteboard_installs_then_checks_one_fresh_tunnel() {
    for recovered in [true, false] {
        let states = RefCell::new(VecDeque::from([false, recovered]));
        let closed = Cell::new(0);
        let prepared = Cell::new(0);
        let events = RefCell::new(Vec::new());
        let result = open_pasteboard(
            async || {
                let has_service = states
                    .borrow_mut()
                    .pop_front()
                    .expect("at most one reconnect");
                Ok(TestTunnel {
                    has_service,
                    closed: &closed,
                })
            },
            async |_: &mut TestTunnel<'_>| {
                prepared.set(prepared.get() + 1);
                Ok(())
            },
            true,
            true,
            &|event| {
                events.borrow_mut().push(event);
                ControlFlow::Continue(())
            },
        )
        .await;
        assert_eq!(prepared.get(), 1);
        assert_eq!(closed.get(), if recovered { 1 } else { 2 });
        assert!(states.borrow().is_empty());
        let events = events.borrow();
        let [
            DeviceEvent::DeveloperServicesMissing { wireless: true },
            DeviceEvent::Status(ConnState::Connecting, reconnect),
        ] = events.as_slice()
        else {
            panic!("expected missing-service and reconnect events")
        };
        assert_eq!(
            reconnect.render(Language::En),
            "Reconnecting and checking the clipboard service…"
        );
        if recovered {
            assert!(result.unwrap().unwrap().has_service);
        } else {
            assert!(matches!(result, Err(error) if error == Failure::setup(DdiStage::Install)));
        }
    }
}

#[tokio::test]
async fn read_only_missing_service_does_not_start_setup() {
    let opened = Cell::new(0);
    let closed = Cell::new(0);
    let result = open_pasteboard(
        async || {
            opened.set(opened.get() + 1);
            Ok(TestTunnel {
                has_service: false,
                closed: &closed,
            })
        },
        async |_: &mut TestTunnel<'_>| panic!("read-only setup called"),
        false,
        false,
        &|_| ControlFlow::Continue(()),
    )
    .await;
    assert!(matches!(result, Err(error) if error == Failure::setup(DdiStage::Install)));
    assert_eq!((opened.get(), closed.get()), (1, 1));
}

#[tokio::test]
async fn disabled_developer_mode_and_sign_timeout_bypass_reopen() {
    for (error, expected, state) in [
        (
            SetupError::DeveloperModeRequired,
            Failure::DeveloperModeRequired,
            ConnState::Developer,
        ),
        (
            SetupError::Failed {
                stage: DdiStage::Sign,
                timed_out: true,
            },
            Failure::Setup {
                stage: DdiStage::Sign,
                timed_out: true,
            },
            ConnState::Hold,
        ),
    ] {
        let opened = Cell::new(0);
        let closed = Cell::new(0);
        let result = open_pasteboard(
            async || {
                opened.set(opened.get() + 1);
                Ok(TestTunnel {
                    has_service: false,
                    closed: &closed,
                })
            },
            async |_: &mut TestTunnel<'_>| Err(error),
            true,
            true,
            &|_| ControlFlow::Continue(()),
        )
        .await;
        assert!(matches!(result, Err(failure) if failure == expected));
        assert_eq!(expected.wait(false).state, state);
        assert_eq!((opened.get(), closed.get()), (1, 1));
    }
}
