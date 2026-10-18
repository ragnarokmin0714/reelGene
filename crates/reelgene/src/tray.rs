//! The system tray: show the window, start or stop, quit.
//!
//! Linux uses StatusNotifierItem over D-Bus (`ksni`, no GTK). It can fail —
//! GNOME shows tray icons only with the AppIndicator extension — and then
//! `spawn` returns `None`, which also means closing the window must quit
//! rather than hide into a tray nobody can see. Windows uses `tray-icon`,
//! which has to be created on the thread that runs the window's event loop.

use crate::gui::text::Text;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCmd {
    Show,
    Start,
    Stop,
    Quit,
}

pub type Notify = Arc<dyn Fn(TrayCmd) + Send + Sync>;

#[cfg(target_os = "linux")]
pub use linux::Tray;
#[cfg(not(any(target_os = "linux", windows)))]
pub use other::Tray;
#[cfg(windows)]
pub use windows::Tray;

/// Elsewhere there is no tray; closing the window quits.
#[cfg(not(any(target_os = "linux", windows)))]
mod other {
    use super::*;

    pub struct Tray;

    impl Tray {
        pub fn spawn(
            _: &tokio::runtime::Handle,
            _: &'static Text,
            _: bool,
            _: Notify,
        ) -> Option<Self> {
            None
        }

        pub fn set_running(&self, _: bool) {}
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use ksni::TrayMethods;
    use ksni::menu::{MenuItem, StandardItem};

    struct Item {
        text: &'static Text,
        running: bool,
        notify: Notify,
    }

    impl ksni::Tray for Item {
        fn id(&self) -> String {
            "reelgene".into()
        }

        fn title(&self) -> String {
            "reelgene".into()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            [22, 32, 48]
                .into_iter()
                .map(|s| ksni::Icon {
                    width: s as i32,
                    height: s as i32,
                    data: crate::icon::argb(s),
                })
                .collect()
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            (self.notify)(TrayCmd::Show);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            let item = |label: &str, cmd: fn(&Item) -> TrayCmd| {
                StandardItem {
                    label: label.to_owned(),
                    activate: Box::new(move |t: &mut Item| (t.notify)(cmd(t))),
                    ..Default::default()
                }
                .into()
            };
            let toggle = if self.running {
                self.text.stop
            } else {
                self.text.start
            };
            vec![
                item(self.text.show_window, |_| TrayCmd::Show),
                item(toggle, |t| {
                    if t.running {
                        TrayCmd::Stop
                    } else {
                        TrayCmd::Start
                    }
                }),
                MenuItem::Separator,
                item(self.text.quit, |_| TrayCmd::Quit),
            ]
        }
    }

    pub struct Tray {
        handle: ksni::Handle<Item>,
        rt: tokio::runtime::Handle,
    }

    impl Tray {
        pub fn spawn(
            rt: &tokio::runtime::Handle,
            text: &'static Text,
            running: bool,
            notify: Notify,
        ) -> Option<Self> {
            let item = Item {
                text,
                running,
                notify,
            };
            let handle = rt.block_on(item.spawn()).ok()?;
            Some(Self {
                handle,
                rt: rt.clone(),
            })
        }

        pub fn set_running(&self, running: bool) {
            let handle = self.handle.clone();
            self.rt.spawn(async move {
                handle.update(|t| t.running = running).await;
            });
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    pub struct Tray {
        _icon: TrayIcon,
        toggle: MenuItem,
        running: Arc<AtomicBool>,
        text: &'static Text,
    }

    impl Tray {
        /// Call on the window's thread (the eframe creation callback).
        pub fn spawn(
            _rt: &tokio::runtime::Handle,
            text: &'static Text,
            running: bool,
            notify: Notify,
        ) -> Option<Self> {
            let show = MenuItem::new(text.show_window, true, None);
            let toggle = MenuItem::new(if running { text.stop } else { text.start }, true, None);
            let quit = MenuItem::new(text.quit, true, None);
            let menu = Menu::new();
            menu.append_items(&[&show, &toggle, &PredefinedMenuItem::separator(), &quit])
                .ok()?;
            let icon = tray_icon::Icon::from_rgba(crate::icon::rgba(32), 32, 32).ok()?;
            let tray = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("reelgene")
                .with_icon(icon)
                .build()
                .ok()?;

            let flag = Arc::new(AtomicBool::new(running));
            let (show_id, toggle_id, quit_id) =
                (show.id().clone(), toggle.id().clone(), quit.id().clone());
            let (on_menu, state) = (notify.clone(), flag.clone());
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                if e.id == show_id {
                    on_menu(TrayCmd::Show);
                } else if e.id == toggle_id {
                    on_menu(if state.load(Ordering::Relaxed) {
                        TrayCmd::Stop
                    } else {
                        TrayCmd::Start
                    });
                } else if e.id == quit_id {
                    on_menu(TrayCmd::Quit);
                }
            }));
            TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = e
                {
                    notify(TrayCmd::Show);
                }
            }));
            Some(Self {
                _icon: tray,
                toggle,
                running: flag,
                text,
            })
        }

        pub fn set_running(&self, running: bool) {
            self.running.store(running, Ordering::Relaxed);
            self.toggle.set_text(if running {
                self.text.stop
            } else {
                self.text.start
            });
        }
    }
}
