// Release builds are GUI apps: no console window when started at logon.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod accounts;
mod autostart;
mod netcheck;

use accounts::Store;
use slint::{ComponentHandle, ModelRc, SharedPixelBuffer, SharedString, VecModel, Weak};
use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

slint::slint! {
    import { Button, LineEdit, CheckBox, TabWidget, VerticalBox, Palette } from "std-widgets.slint";

    export struct Line { text: string, warn: bool }

    component Muted inherits Text {
        color: #969696;
        font-size: 13px;
        wrap: word-wrap;
    }

    // "重命名" swaps the name for an edit box; "删除" asks for a second click.
    component AccountRow inherits HorizontalLayout {
        in property <string> name;
        in property <bool> current;
        callback switch(string);
        callback rename(string, string);
        callback remove(string);
        property <bool> editing;
        property <bool> armed;
        property <bool> removing;
        property <string> draft;
        spacing: 8px;
        if !editing: Text { text: name; width: 200px; font-size: 15px; vertical-alignment: center; overflow: elide; }
        if editing: LineEdit {
            width: 200px;
            text <=> draft;
            init => { self.focus(); }
            accepted => { editing = false; rename(name, draft); }
        }
        Button {
            text: current ? "使用中" : "切换到此账号";
            enabled: !current;
            clicked => { switch(name); }
        }
        Button {
            text: editing ? "保存" : "重命名";
            clicked => {
                if (!editing) {
                    draft = name;
                    editing = true;
                } else {
                    editing = false;
                    rename(name, draft);
                }
            }
        }
        if !current: Button {
            text: armed ? "确认删除？再点一次" : "删除";
            enabled: !removing;
            clicked => {
                if (!armed) {
                    armed = true;
                } else {
                    removing = true;
                    remove(name);
                }
            }
        }
        Rectangle { }
    }

    export component MainWindow inherits Window {
        title: "Claude 账号切换器";
        preferred-width: 640px;
        preferred-height: 600px;
        in property <image> app-icon;
        icon: app-icon;
        // Slint's fallback misses some CJK glyphs on Windows.
        default-font-family: "Microsoft YaHei UI";

        in property <string> current;
        in property <[string]> accounts;
        in property <string> status;
        in-out property <string> new-name;
        callback switch(string);
        callback rename(string, string);
        callback remove(string);
        callback add();
        callback quit-claude();
        callback login();

        in property <[Line]> checks;
        in-out property <string> adapter;
        in-out property <string> procs;
        in-out property <bool> guard-on;
        in property <string> guard-status: "保护状态：未启用";
        in-out property <bool> autostart;
        callback run-checks();
        callback guard-toggled();
        callback autostart-toggled();

        TabWidget {
            Tab {
                title: "账号切换";
                VerticalBox {
                    alignment: start;
                    spacing: 12px;
                    Text { text: "当前账号：" + current; font-size: 20px; }
                    Muted { text: "切换前会关闭 Claude 桌面版，并把登录数据（%APPDATA%\\Claude）存成该账号的快照。"; }
                    VerticalLayout {
                        spacing: 6px;
                        for name in accounts: AccountRow {
                            name: name;
                            current: name == current;
                            switch(n) => { root.switch(n); }
                            rename(old, new) => { root.rename(old, new); }
                            remove(n) => { root.remove(n); }
                        }
                    }
                    HorizontalLayout {
                        spacing: 8px;
                        LineEdit { width: 200px; placeholder-text: "新账号名称"; text <=> new-name; accepted => { add(); } }
                        Button { text: "添加账号"; clicked => { add(); } }
                        Rectangle { }
                    }
                    HorizontalLayout {
                        spacing: 8px;
                        Button { text: "退出 Claude 桌面版"; clicked => { quit-claude(); } }
                        Button { text: "登录"; clicked => { login(); } }
                        Rectangle { }
                    }
                    Muted { text: status; }
                }
            }
            Tab {
                title: "网络自检";
                VerticalBox {
                    alignment: start;
                    spacing: 12px;
                    Text { text: "泄漏自查"; font-size: 18px; }
                    Muted { text: "比较 HTTP、UDP（WebRTC 所用）与 IPv6 的出口地址，并列出 DNS 服务器。"; }
                    HorizontalLayout {
                        Button { text: "开始检测"; clicked => { run-checks(); } }
                        Rectangle { }
                    }
                    VerticalLayout {
                        spacing: 4px;
                        for l in checks: Text {
                            text: l.text;
                            font-size: 14px;
                            wrap: word-wrap;
                            color: l.warn ? #E8A33D : Palette.foreground;
                        }
                    }

                    Text { text: "断线保护"; font-size: 18px; }
                    HorizontalLayout {
                        spacing: 8px;
                        LineEdit { width: 220px; placeholder-text: "VPN 网卡名关键字，如 TUN"; text <=> adapter; enabled: !guard-on; }
                        LineEdit { width: 260px; placeholder-text: "要结束的进程，如 app.exe,b.exe"; text <=> procs; enabled: !guard-on; }
                        Rectangle { }
                    }
                    CheckBox {
                        text: "启用：VPN 网卡断开时立即结束上述进程";
                        checked <=> guard-on;
                        toggled => { guard-toggled(); }
                    }
                    Muted { text: guard-status; }
                    CheckBox {
                        text: "登录 Windows 后自动启动本程序（配合上面的“启用”即可开机即保护）";
                        checked <=> autostart;
                        toggled => { autostart-toggled(); }
                    }
                }
            }
        }
    }
}

/// Shows a status and re-renders the account list; safe to call from any thread.
fn report(ui: &Weak<MainWindow>, store: &Store, msg: String) {
    let store = store.clone();
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_status(msg.into());
        ui.set_current(store.current().into());
        let names: Vec<SharedString> = store.list().into_iter().map(Into::into).collect();
        ui.set_accounts(ModelRc::new(VecModel::from(names)));
    });
}

fn wire_accounts(ui: &MainWindow, store: &Store) {
    let weak = ui.as_weak();
    report(&weak, store, "状态：就绪".into());

    ui.on_switch({
        let (weak, store) = (weak.clone(), store.clone());
        move |name| {
            report(&weak, &store, format!("状态：正在切换到「{name}」…"));
            let (weak, store) = (weak.clone(), store.clone());
            thread::spawn(move || {
                let _ = accounts::quit_claude_desktop();
                // Claude needs a moment to release its files after being killed.
                let mut result = store.switch(&name);
                for _ in 0..5 {
                    if result.is_ok() {
                        break;
                    }
                    thread::sleep(Duration::from_secs(1));
                    result = store.switch(&name);
                }
                report(&weak, &store, match result {
                    Ok(()) => format!("状态：已切换到「{name}」，请重新打开 Claude"),
                    Err(e) => format!("状态：切换失败：{e}"),
                });
            });
        }
    });

    ui.on_rename({
        let (weak, store) = (weak.clone(), store.clone());
        move |name, new| {
            report(&weak, &store, match store.rename(&name, &new) {
                Ok(()) if new.trim() == name.as_str() => "状态：就绪".to_string(),
                Ok(()) => format!("状态：已将「{name}」重命名为「{}」", new.trim()),
                Err(e) => format!("状态：重命名失败：{e}"),
            });
        }
    });

    ui.on_remove({
        let (weak, store) = (weak.clone(), store.clone());
        move |name| {
            let (weak, store) = (weak.clone(), store.clone());
            thread::spawn(move || {
                report(&weak, &store, match store.remove(&name) {
                    Ok(()) => format!("状态：已删除「{name}」（快照已移入回收站）"),
                    Err(e) => format!("状态：{e}"),
                });
            });
        }
    });

    ui.on_add({
        let (weak, store) = (weak.clone(), store.clone());
        move || {
            let ui = weak.unwrap();
            let text = ui.get_new_name();
            report(&weak, &store, match store.add(&text) {
                Ok(()) => {
                    ui.set_new_name("".into());
                    format!("状态：已添加「{}」，切换过去后在 Claude 里登录即可", text.trim())
                }
                Err(e) => format!("状态：{e}"),
            });
        }
    });

    ui.on_quit_claude({
        let (weak, store) = (weak.clone(), store.clone());
        move || {
            let (weak, store) = (weak.clone(), store.clone());
            thread::spawn(move || {
                report(&weak, &store, match accounts::quit_claude_desktop() {
                    Ok(()) => "状态：已结束 Claude 桌面版进程".to_string(),
                    Err(e) => format!("状态：退出失败：{e}"),
                });
            });
        }
    });

    ui.on_login({
        let (weak, store) = (weak.clone(), store.clone());
        move || {
            report(&weak, &store, match accounts::relogin_claude_code() {
                Ok(()) => "状态：已在新窗口中执行 claude auth logout，随后 claude auth login".to_string(),
                Err(e) => format!("状态：无法启动登录：{e}"),
            });
        }
    });
}

fn guard_file() -> PathBuf {
    Store::default_paths().root.join("guard.txt")
}

/// Line 1: adapter keyword, line 2: process names, line 3: "on" to start guarding at launch.
fn save_guard(keyword: &str, procs: &str, on: bool) {
    let file = guard_file();
    let _ = fs::create_dir_all(file.parent().unwrap());
    let _ = fs::write(&file, format!("{keyword}\n{procs}\n{}", if on { "on" } else { "off" }));
}

fn wire_network(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.on_run_checks({
        let weak = weak.clone();
        move || {
            let line = |text: String| Line { warn: text.starts_with('⚠'), text: text.into() };
            weak.unwrap().set_checks(ModelRc::new(VecModel::from(vec![line("检测中…".into())])));
            let weak = weak.clone();
            thread::spawn(move || {
                let lines: Vec<Line> = netcheck::run_all().into_iter().map(line).collect();
                let _ = weak.upgrade_in_event_loop(move |ui| ui.set_checks(ModelRc::new(VecModel::from(lines))));
            });
        }
    });

    let saved = fs::read_to_string(guard_file()).unwrap_or_default();
    let mut saved = saved.lines();
    ui.set_adapter(saved.next().unwrap_or_default().into());
    ui.set_procs(saved.next().unwrap_or_default().into());
    let auto_on = saved.next() == Some("on");

    let stop: Rc<RefCell<Option<Arc<AtomicBool>>>> = Rc::default();
    ui.on_guard_toggled({
        let weak = weak.clone();
        move || {
            let ui = weak.unwrap();
            if let Some(flag) = stop.borrow_mut().take() {
                flag.store(true, Ordering::Relaxed);
            }
            if !ui.get_guard_on() {
                save_guard(&ui.get_adapter(), &ui.get_procs(), false);
                ui.set_guard_status("保护状态：未启用".into());
                return;
            }
            let keyword = ui.get_adapter().trim().to_string();
            let names: Vec<String> =
                ui.get_procs().split([',', '，', ' ']).filter(|s| !s.is_empty()).map(str::to_string).collect();
            if keyword.is_empty() || names.is_empty() {
                ui.set_guard_on(false);
                ui.set_guard_status("保护状态：请先填写网卡关键字和进程名".into());
                return;
            }
            save_guard(&keyword, &names.join(","), true);
            ui.set_guard_status(format!("保护状态：正在检查含「{keyword}」的网卡…").into());
            let flag = Arc::new(AtomicBool::new(false));
            *stop.borrow_mut() = Some(flag.clone());
            let weak = weak.clone();
            // ponytail: guards only while this app runs; a scheduled task would cover logon-to-logoff.
            thread::spawn(move || {
                let mut last = None;
                while !flag.load(Ordering::Relaxed) {
                    let state = netcheck::adapter_up(&keyword).map_err(|e| e.to_string());
                    let msg = match &state {
                        Ok(true) => (last != Some(true)).then(|| "保护状态：VPN 在线，监视中".to_string()),
                        Ok(false) => {
                            let killed = netcheck::kill(&names);
                            let killed = if killed.is_empty() { "（无运行中的进程）".to_string() } else { killed.join(", ") };
                            (last != Some(false) || !killed.starts_with('（'))
                                .then(|| format!("保护状态：VPN 已断开！已结束：{killed}"))
                        }
                        Err(e) => Some(format!("保护状态：{e}")),
                    };
                    last = state.ok();
                    // The user may have switched it off while we were checking.
                    if let Some(m) = msg.filter(|_| !flag.load(Ordering::Relaxed)) {
                        let _ = weak.upgrade_in_event_loop(move |ui| ui.set_guard_status(m.into()));
                    }
                    thread::sleep(Duration::from_secs(3));
                }
            });
        }
    });
    // Setting the property doesn't fire `toggled`, so start the guard by hand.
    if auto_on {
        ui.set_guard_on(true);
        ui.invoke_guard_toggled();
    }

    ui.set_autostart(autostart::is_enabled());
    ui.on_autostart_toggled(move || {
        let ui = weak.unwrap();
        if let Err(e) = autostart::set(ui.get_autostart()) {
            ui.set_guard_status(format!("自动启动设置失败：{e}").into());
        }
    });
}

/// A tray icon that brings the window back; closing the window only hides
/// it (Slint's default), and "退出" in the tray menu is the way to quit.
fn add_tray(ui: &MainWindow, icon: Icon) -> TrayIcon {
    let show = MenuItem::new("显示窗口", true, None);
    let quit = MenuItem::new("退出", true, None);
    let menu = Menu::new();
    menu.append_items(&[&show, &quit]).unwrap();
    let tray = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("Claude 账号切换器")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .unwrap();

    let weak = ui.as_weak();
    let show_window = move || {
        let _ = weak.upgrade_in_event_loop(|ui| {
            let _ = ui.show();
        });
    };
    let (show_id, quit_id) = (show.id().clone(), quit.id().clone());
    MenuEvent::set_event_handler(Some({
        let show_window = show_window.clone();
        move |e: MenuEvent| {
            if e.id == quit_id {
                let _ = slint::quit_event_loop();
            } else if e.id == show_id {
                show_window();
            }
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |e| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            show_window();
        }
    }));
    tray
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;
    let png = image::load_from_memory(include_bytes!("../assets/tray.png")).unwrap().into_rgba8();
    let (w, h) = png.dimensions();
    ui.set_app_icon(slint::Image::from_rgba8(SharedPixelBuffer::clone_from_slice(png.as_raw(), w, h)));
    let _tray = add_tray(&ui, Icon::from_rgba(png.into_raw(), w, h).unwrap());

    wire_accounts(&ui, &Store::default_paths());
    wire_network(&ui);
    // `--tray` (used at logon) starts in the tray.
    if !std::env::args().any(|a| a == "--tray") {
        ui.show()?;
    }
    // Hidden windows don't end the app; only the tray's "退出" does.
    slint::run_event_loop_until_quit()
}
