//! A system notification when a batch of downloads ends while the window is
//! not in front: the sound alone is easy to miss, and quiet hours mute it.
//!
//! Linux: `org.freedesktop.Notifications` over D-Bus, through the zbus the
//! tray already brings. Windows: a toast. A program not installed from the
//! Store must first register an app id under `HKCU\Software\Classes\
//! AppUserModelId`, or Windows drops its toasts without a word.
//!
//! Failing to notify (no notification service, say) is only logged.

pub struct Note {
    pub title: String,
    pub body: String,
}

/// Show it from a background task; never blocks the caller.
pub fn show(rt: &tokio::runtime::Handle, note: Note) {
    #[cfg(target_os = "linux")]
    rt.spawn(async move {
        if let Err(e) = linux::show(&note).await {
            log::info!("no desktop notification: {e}");
        }
    });
    #[cfg(windows)]
    rt.spawn_blocking(move || {
        if let Err(e) = win::show(&note) {
            log::info!("no desktop notification: {e}");
        }
    });
    #[cfg(not(any(target_os = "linux", windows)))]
    let _ = (rt, note);
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use zbus::zvariant::Value;

    const ICON: u32 = 64;

    pub async fn show(note: &super::Note) -> zbus::Result<()> {
        send(&zbus::Connection::session().await?, note).await
    }

    async fn send(conn: &zbus::Connection, note: &super::Note) -> zbus::Result<()> {
        let mut hints: HashMap<&str, Value> = HashMap::new();
        hints.insert("desktop-entry", Value::from("reelgene"));
        // (iiibiiay): width, height, row stride, alpha, bits per sample,
        // channels, pixels. The icon is drawn in code, so there is no file to
        // name instead.
        let side = ICON as i32;
        let image = (
            side,
            side,
            side * 4,
            true,
            8i32,
            4i32,
            crate::icon::rgba(ICON),
        );
        hints.insert("image-data", Value::from(image));
        conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                "reelgene",
                0u32,
                "",
                note.title.as_str(),
                note.body.as_str(),
                Vec::<&str>::new(),
                hints,
                -1i32,
            ),
        )
        .await?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use futures::StreamExt;

        type Notify = (
            String,
            u32,
            String,
            String,
            String,
            Vec<String>,
            HashMap<String, zbus::zvariant::OwnedValue>,
            i32,
        );

        /// Against a real, private message bus with a stand-in notification
        /// service: what arrives is what a desktop's would receive.
        #[tokio::test]
        async fn the_notification_reaches_the_service_as_the_specification_says() {
            let Ok(mut daemon) = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address"])
                .stdout(std::process::Stdio::piped())
                .spawn()
            else {
                eprintln!("no dbus-daemon here; skipped");
                return;
            };
            let mut address = String::new();
            std::io::BufRead::read_line(
                &mut std::io::BufReader::new(daemon.stdout.take().unwrap()),
                &mut address,
            )
            .unwrap();
            let connect = || async {
                zbus::connection::Builder::address(address.trim())
                    .unwrap()
                    .build()
                    .await
                    .unwrap()
            };

            let service = connect().await;
            service
                .request_name("org.freedesktop.Notifications")
                .await
                .unwrap();
            let mut calls = zbus::MessageStream::from(&service);
            let answer = tokio::spawn(async move {
                while let Some(Ok(msg)) = calls.next().await {
                    let header = msg.header();
                    if header.member().is_some_and(|m| m.as_str() == "Notify") {
                        let call: Notify = msg.body().deserialize().unwrap();
                        service.reply(&header, &(1u32,)).await.unwrap();
                        return call;
                    }
                }
                panic!("no Notify call arrived");
            });

            let note = super::super::Note {
                title: "Downloads finished".into(),
                body: "3 done".into(),
            };
            send(&connect().await, &note).await.unwrap();
            let (app, replaces, _, title, body, actions, hints, timeout) = answer.await.unwrap();
            let _ = daemon.kill();
            assert_eq!((app.as_str(), replaces), ("reelgene", 0));
            assert_eq!(
                (title.as_str(), body.as_str()),
                ("Downloads finished", "3 done")
            );
            assert!(actions.is_empty());
            assert_eq!(timeout, -1, "the desktop decides how long it shows");
            assert!(hints.contains_key("image-data") && hints.contains_key("desktop-entry"));
        }

        #[test]
        fn the_icon_has_the_type_the_specification_asks_for() {
            let side = ICON as i32;
            let image = Value::from((side, side, side * 4, true, 8i32, 4i32, vec![0u8; 4]));
            assert_eq!(image.value_signature().to_string(), "(iiibiiay)");
        }
    }
}

#[cfg(windows)]
mod win {
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::core::HSTRING;

    const APP_ID: &str = "reelgene";

    pub fn show(note: &super::Note) -> windows::core::Result<()> {
        // Every time: cheap, and it survives the key being cleaned away.
        let key = windows_registry::CURRENT_USER
            .create(format!(r"Software\Classes\AppUserModelId\{APP_ID}"))?;
        key.set_string("DisplayName", "reelgene")?;

        let xml = XmlDocument::new()?;
        xml.LoadXml(&HSTRING::from(format!(
            "<toast><visual><binding template=\"ToastGeneric\">\
             <text>{}</text><text>{}</text></binding></visual></toast>",
            escape(&note.title),
            escape(&note.body)
        )))?;
        let toast = ToastNotification::CreateToastNotification(&xml)?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?.Show(&toast)
    }

    fn escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
}
