mod menubar;

use std::thread;
use std::time::Duration;

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::menu::{MenuEvent, MenuId};

use pigtop::ntstat::NtstatClient;
use pigtop::rates::{ProcessBandwidth, RateTracker};

use menubar::MenuBar;

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

    let poll_proxy = proxy.clone();
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
        loop {
            match client.poll_all() {
                Ok(samples) => {
                    let ranked = tracker.update(&samples);
                    if poll_proxy.send_event(UserEvent::Tick(ranked)).is_err() {
                        return;
                    }
                }
                Err(e) => eprintln!("pigtop: poll_all failed: {e}"),
            }
            thread::sleep(Duration::from_secs(1));
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
                    menu_bar.update(&ranked);
                }
            }
            Event::UserEvent(UserEvent::Quit) => {
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
