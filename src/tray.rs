#![cfg_attr(not(windows), allow(dead_code))]
//! Windows notification-area (tray) icon and restoring the hidden window.
//! The window is shown through Win32 directly, so it works while egui is not painting (hidden window).
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayMsg {
    Show,
    Quit,
}

/// Does this platform have a tray icon?
pub fn supported() -> bool {
    cfg!(windows)
}

#[cfg(windows)]
pub struct Tray {
    _icon: tray_icon::TrayIcon,
}

#[cfg(not(windows))]
pub struct Tray;

/// Show and focus the main window (found by its title).
#[cfg(windows)]
pub fn show_window() {
    use windows::{
        core::{w, PCWSTR},
        Win32::UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW},
    };
    unsafe {
        if let Ok(hwnd) = FindWindowW(PCWSTR::null(), w!("TomasekLibreAIFrames")) {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(not(windows))]
pub fn show_window() {}

#[cfg(windows)]
pub fn create(ctx: egui::Context, tx: Sender<TrayMsg>) -> Option<Tray> {
    use tray_icon::{
        menu::{Menu, MenuEvent, MenuItem},
        Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
    };
    let (rgba, w, h) = crate::appicon::rgba();
    let icon = Icon::from_rgba(rgba, w, h).ok()?;
    let menu = Menu::new();
    let open = MenuItem::new(crate::tr!("Otevřít TomasekLibreAIFrames", "Open TomasekLibreAIFrames"), true, None);
    let quit = MenuItem::new(crate::tr!("Ukončit", "Quit"), true, None);
    menu.append(&open).ok()?;
    menu.append(&quit).ok()?;
    let open_id = open.id().clone();
    let quit_id = quit.id().clone();
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("TomasekLibreAIFrames")
        .with_icon(icon)
        .build()
        .ok()?;
    {
        let tx = tx.clone();
        let ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| match e {
            TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } | TrayIconEvent::DoubleClick { .. } => {
                show_window();
                let _ = tx.send(TrayMsg::Show);
                ctx.request_repaint();
            }
            _ => {}
        }));
    }
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if e.id == open_id {
            show_window();
            let _ = tx.send(TrayMsg::Show);
            ctx.request_repaint();
        } else if e.id == quit_id {
            show_window();
            let _ = tx.send(TrayMsg::Quit);
            ctx.request_repaint();
        }
    }));
    Some(Tray { _icon: tray })
}

#[cfg(not(windows))]
pub fn create(_ctx: egui::Context, _tx: Sender<TrayMsg>) -> Option<Tray> {
    None
}
