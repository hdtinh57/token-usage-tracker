use std::sync::mpsc::Sender;

use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

use crate::settings::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    ToggleWindow,
    ToggleCloseToTray,
    ToggleNotifications,
    OpenPricing,
    Quit,
}

pub fn command_for(id: &str) -> Option<Command> {
    match id {
        "show-hide" => Some(Command::ToggleWindow),
        "close-to-tray" => Some(Command::ToggleCloseToTray),
        "notifications" => Some(Command::ToggleNotifications),
        "open-pricing" => Some(Command::OpenPricing),
        "quit" => Some(Command::Quit),
        _ => None,
    }
}

pub struct Tray {
    _icon: TrayIcon,
    close_to_tray: CheckMenuItem,
    notifications: CheckMenuItem,
}

impl Tray {
    pub fn new(
        settings: Settings,
        sender: Sender<Command>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let menu = Menu::new();
        let show_hide = MenuItem::with_id("show-hide", "Show / hide", true, None);
        let close_to_tray = CheckMenuItem::with_id(
            "close-to-tray",
            "Close to tray",
            true,
            settings.close_to_tray,
            None,
        );
        let notifications = CheckMenuItem::with_id(
            "notifications",
            "Quota notifications",
            true,
            settings.notifications_enabled,
            None,
        );
        let open_pricing = MenuItem::with_id("open-pricing", "Open pricing.json", true, None);
        let quit = MenuItem::with_id("quit", "Quit", true, None);
        menu.append_items(&[
            &show_hide,
            &PredefinedMenuItem::separator(),
            &close_to_tray,
            &notifications,
            &open_pricing,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;

        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some(command) = command_for(event.id.as_ref()) {
                let _ = sender.send(command);
            }
        }));

        let icon = Icon::from_rgba(vec![76; 16 * 16 * 4], 16, 16)?;
        let icon = TrayIconBuilder::new()
            .with_icon(icon)
            .with_tooltip("Token Usage Tracker")
            .with_menu(Box::new(menu))
            .build()?;
        Ok(Self {
            _icon: icon,
            close_to_tray,
            notifications,
        })
    }

    pub fn sync_settings(&self, settings: Settings) {
        self.close_to_tray.set_checked(settings.close_to_tray);
        self.notifications
            .set_checked(settings.notifications_enabled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_for_maps_show_hide_quit_and_ignores_unknown_ids() {
        assert_eq!(command_for("show-hide"), Some(Command::ToggleWindow));
        assert_eq!(command_for("quit"), Some(Command::Quit));
        assert_eq!(command_for("unknown"), None);
    }
}
