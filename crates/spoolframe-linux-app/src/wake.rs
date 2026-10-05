//! When the machine wakes from sleep or its network changes, connections
//! that looked open may be dead without having closed (P1-15): the app
//! asks the client to check them then ([`Client::resume`]), so the next
//! request goes on a working one instead of waiting out its time limit.
//!
//! Waking is logind's PrepareForSleep(false) on the system bus; a network
//! change is GIO's network monitor. Without a system bus (a container),
//! only the network is watched.
//!
//! [`Client::resume`]: spoolframe_linux_core::backend::Client::resume

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;

/// Keeps the watches for as long as it lives.
pub struct Wake {
    _sleep: Rc<RefCell<Option<gio::SignalSubscription>>>,
    _network: Option<(gio::NetworkMonitor, glib::SignalHandlerId)>,
}

/// Calls `resume` after each wake and network change.
pub fn watch(resume: Rc<dyn Fn()>) -> Wake {
    let sleep = Rc::new(RefCell::new(None));
    {
        let (sleep, resume) = (sleep.clone(), resume.clone());
        glib::spawn_future_local(async move {
            let bus = match gio::bus_get_future(gio::BusType::System).await {
                Ok(bus) => bus,
                Err(e) => {
                    return tracing::debug!(error = %e, "no system bus; not watching for sleep");
                }
            };
            let subscription = bus.subscribe_to_signal(
                Some("org.freedesktop.login1"),
                Some("org.freedesktop.login1.Manager"),
                Some("PrepareForSleep"),
                Some("/org/freedesktop/login1"),
                None,
                gio::DBusSignalFlags::NONE,
                move |signal| {
                    // True going to sleep, false on waking.
                    if signal.parameters.get::<(bool,)>() == Some((false,)) {
                        tracing::debug!("woke from sleep; checking connections");
                        resume();
                    }
                },
            );
            sleep.replace(Some(subscription));
        });
    }
    let monitor = gio::NetworkMonitor::default();
    let handler = monitor.connect_network_changed(move |_, _| resume());
    Wake {
        _sleep: sleep,
        _network: Some((monitor, handler)),
    }
}
