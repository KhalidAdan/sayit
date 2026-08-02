//! GNOME/KDE StatusNotifierItem tray implemented directly over D-Bus. This
//! avoids requiring libayatana-appindicator on the immutable host.

use super::LinuxBackend;
use ksni::blocking::TrayMethods;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;

use crate::settings;

pub(super) struct SayitTray {
    app: AppHandle,
    pub(super) status: String,
    microphone: Option<String>,
    microphones: Vec<Option<String>>,
    keep_awake: bool,
    autostart: bool,
}

impl ksni::Tray for SayitTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "dev.khalid.sayit".into()
    }

    fn title(&self) -> String {
        format!("sayit — {}", self.status)
    }

    fn icon_name(&self) -> String {
        "audio-input-microphone-symbolic".into()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "sayit".into(),
            description: self.status.clone(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;

        let selected = self
            .microphones
            .iter()
            .position(|m| m == &self.microphone)
            .unwrap_or(0);
        let mic_options = self
            .microphones
            .iter()
            .map(|mic| RadioItem {
                label: mic.clone().unwrap_or_else(|| "System default".into()),
                ..Default::default()
            })
            .collect();

        vec![
            StandardItem {
                label: self.status.clone(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            SubMenu {
                label: "Microphone".into(),
                submenu: vec![RadioGroup {
                    selected,
                    options: mic_options,
                    select: Box::new(|this: &mut Self, index| {
                        let choice = this.microphones.get(index).cloned().flatten();
                        this.microphone = choice.clone();
                        *this.app.state::<crate::MicChoice>().0.lock().unwrap() = choice.clone();
                        let mut saved = settings::load(&this.app);
                        saved.microphone = choice;
                        settings::save(&this.app, &saved);
                    }),
                }
                .into()],
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Dictionary…".into(),
                activate: Box::new(|this: &mut Self| crate::dictionary::show(&this.app)),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Keep engine awake".into(),
                checked: self.keep_awake,
                activate: Box::new(|this: &mut Self| {
                    this.keep_awake = !this.keep_awake;
                    let _ = this.app.emit("keep_awake", this.keep_awake);
                    let mut saved = settings::load(&this.app);
                    saved.keep_awake = this.keep_awake;
                    settings::save(&this.app, &saved);
                }),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Start at login".into(),
                checked: self.autostart,
                activate: Box::new(|this: &mut Self| {
                    let launcher = this.app.autolaunch();
                    let result = if this.autostart {
                        launcher.disable()
                    } else {
                        launcher.enable()
                    };
                    if let Err(e) = result {
                        eprintln!("[sayit] autostart toggle failed: {e}");
                    }
                    this.autostart = launcher.is_enabled().unwrap_or(false);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Diagnostics…".into(),
                activate: Box::new(|this: &mut Self| crate::setup::show(&this.app)),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "made by the Dream Team — GitHub ↗".into(),
                activate: Box::new(|_| {
                    let _ = std::process::Command::new("gio")
                        .args(["open", "https://github.com/KhalidAdan/sayit"])
                        .spawn();
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit sayit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|this: &mut Self| this.app.exit(0)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub(super) fn build(
    backend: &LinuxBackend,
    app: &AppHandle,
    saved: &settings::Settings,
) -> Result<(), String> {
    let mut microphones = vec![None];
    microphones.extend(crate::capture::list_inputs().into_iter().map(Some));
    let tray = SayitTray {
        app: app.clone(),
        status: "warming up…".into(),
        microphone: saved.microphone.clone(),
        microphones,
        keep_awake: saved.keep_awake,
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
    };
    let handle = tray.spawn().map_err(|e| e.to_string())?;
    backend.tray.lock().unwrap().replace(handle);
    Ok(())
}
