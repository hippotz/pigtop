mod menubar;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::menu::{MenuEvent, MenuId};
use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};

use pigtop::ntstat::{NtstatClient, DEFAULT_RECV_TIMEOUT};
use pigtop::rates::{ProcessBandwidth, RateTracker};

use menubar::MenuBar;

/// Poll cadence outside of boost mode — fine-grained enough for normal viewing without
/// unnecessary kernel-socket chatter while nobody's looking.
const NORMAL_POLL_SLEEP: Duration = Duration::from_millis(100);

/// Poll cadence while boosted: short-lived connections (e.g. a many-parallel-socket speed test)
/// can live and die entirely inside one NORMAL_POLL_SLEEP gap and never get measured — boost
/// mode trades kernel-socket chatter for a much smaller blind spot while the user is watching.
const BOOST_RECV_TIMEOUT: Duration = Duration::from_millis(15);
const BOOST_POLL_SLEEP: Duration = Duration::from_millis(10);

/// How often the displayed number actually changes, independent of poll/boost cadence — so
/// boost mode catches more short-lived traffic without making the menu bar flicker.
const UI_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

enum UserEvent {
    Tick(Vec<ProcessBandwidth>),
    Quit,
}

fn main() {
    let mut builder = EventLoopBuilder::<UserEvent>::with_user_event();
    let mut event_loop = builder.build();
    // No dock icon / app switcher entry — this app lives only in the menu bar.
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    let proxy = event_loop.create_proxy();

    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == MenuId::new(menubar::QUIT_MENU_ID) {
            let _ = menu_proxy.send_event(UserEvent::Quit);
        }
    }));

    // Toggled by clicking the tray icon (see TrayIconEvent handler below); read by the polling
    // thread to pick its cadence, and by the main-thread closure to show it's active.
    let boost = Arc::new(AtomicBool::new(false));

    let boost_click = Arc::clone(&boost);
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        // tray-icon's macOS backend opens the attached menu via `performClick` from inside its
        // own mouseDown handler, which kicks off AppKit's modal menu-tracking loop right there —
        // the real mouse-up that follows is almost certainly consumed by that tracking session
        // rather than ever reaching this handler as a `Click { button_state: Up }` event. `Down`
        // is the one event guaranteed to fire exactly once per physical click (it's sent
        // unconditionally, before the menu-opening logic even runs), so toggle on that instead.
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Down,
            ..
        } = event
        {
            boost_click.fetch_xor(true, Ordering::Relaxed);
        }
    }));

    let poll_proxy = proxy.clone();
    let boost_poll = Arc::clone(&boost);
    thread::spawn(move || {
        let mut client = match NtstatClient::connect() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("pigtop: failed to connect to ntstat: {e}");
                return;
            }
        };
        if let Err(e) = client.subscribe_tcp_udp() {
            eprintln!("pigtop: failed to subscribe to network sources: {e}");
            return;
        }

        let mut tracker = RateTracker::new();
        let mut boosted = false;
        let mut last_ui_update = Instant::now();
        loop {
            let want_boost = boost_poll.load(Ordering::Relaxed);
            if want_boost != boosted {
                let timeout = if want_boost { BOOST_RECV_TIMEOUT } else { DEFAULT_RECV_TIMEOUT };
                if let Err(e) = client.set_recv_timeout(timeout) {
                    eprintln!("pigtop: failed to change poll cadence: {e}");
                } else {
                    boosted = want_boost;
                }
            }

            match client.poll_all() {
                Ok(samples) => tracker.record(&samples),
                Err(e) => eprintln!("pigtop: poll_all failed: {e}"),
            }

            if last_ui_update.elapsed() >= UI_REFRESH_INTERVAL {
                last_ui_update = Instant::now();
                let ranked = tracker.snapshot();
                if poll_proxy.send_event(UserEvent::Tick(ranked)).is_err() {
                    return;
                }
            }

            thread::sleep(if boosted { BOOST_POLL_SLEEP } else { NORMAL_POLL_SLEEP });
        }
    });

    // Only ever touched from this closure, which tao guarantees runs on the main thread.
    let mut menu_bar: Option<MenuBar> = None;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::NewEvents(StartCause::Init) => {
                menu_bar = Some(MenuBar::build().expect("create menu bar status item"));
            }
            Event::UserEvent(UserEvent::Tick(ranked)) => {
                if let Some(menu_bar) = &menu_bar {
                    menu_bar.update(&ranked, boost.load(Ordering::Relaxed));
                }
            }
            Event::UserEvent(UserEvent::Quit) => {
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
