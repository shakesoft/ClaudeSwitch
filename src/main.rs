// Release builds are GUI apps: no console window when started at logon.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod accounts;
mod autostart;
mod netcheck;

use accounts::Store;
use ferroui::base::media::{Brush, TextWrapping};
use ferroui::base::threading::{DispatcherPriority, DispatcherTimer};
use ferroui::controls::application_lifetimes::IApplicationLifetime;
use ferroui::controls::{
    NativeMenu, NativeMenuItem, ShutdownMode, TabControl, TabItem, TrayIcon, TrayIcons, WindowCloseReason,
    WindowIcon,
};
use ferroui::prelude::*;
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[repr(C)]
pub struct App {
    base: Application,
}

ferro_class!(App: Application);
ferro_impl_classes!(App: FerroObjectImpl);

impl NewApplication for App {
    fn new_application() -> Ref<Self> {
        instantiate(Self { base: Application::construct() })
    }
}

impl ApplicationImpl for App {
    fn initialize(this: &Self) {
        Self::parent_initialize(this);
        this.styles().add(FluentTheme::new().as_style());
    }

    fn on_framework_initialization_completed(this: &Self) {
        let lifetime = this.application_lifetime();
        if let Some(desktop) =
            lifetime.as_ref().and_then(|lifetime| lifetime.as_classic_desktop_style_application_lifetime())
        {
            let window = create_main_window();
            add_tray_icon(this, &window, lifetime.clone());
            // Only the tray menu quits, so a hidden window must not end the app.
            desktop.set_shutdown_mode(ShutdownMode::OnExplicitShutdown);
            // `--tray` (used at logon) starts in the tray: the lifetime shows only a main window.
            if !std::env::args().any(|a| a == "--tray") {
                desktop.set_main_window(Some(window));
            }
        }
        Self::parent_on_framework_initialization_completed(this);
    }
}

/// A tray icon that brings the window back; closing the window only hides
/// it, and "退出" in the tray menu is the way to quit.
fn add_tray_icon(app: &App, window: &Ref<Window>, lifetime: Option<Rc<dyn IApplicationLifetime>>) {
    let quitting = Rc::new(Cell::new(false));
    window.closing({
        let (window, quitting) = (window.clone(), quitting.clone());
        move |e| {
            if !quitting.get() && e.close_reason() == WindowCloseReason::WindowClosing {
                e.set_cancel(true);
                window.hide();
            }
        }
    });

    let show = {
        let window = window.clone();
        move || {
            window.show();
            window.activate();
        }
    };
    let item = |text: &str, action: Rc<dyn Fn()>| {
        let i = NativeMenuItem::new();
        i.set_header(Some(text.to_string()));
        i.click(move |_| action());
        i
    };
    let menu = NativeMenu::new();
    menu.items().add(item("显示窗口", Rc::new(show.clone())).upcast());
    menu.items().add(
        item(
            "退出",
            Rc::new(move || {
                quitting.set(true);
                if let Some(desktop) = lifetime.as_ref().and_then(|l| l.as_classic_desktop_style_application_lifetime()) {
                    desktop.shutdown(0);
                }
            }),
        )
        .upcast(),
    );

    let tray = TrayIcon::new();
    let icon = WindowIcon::from_stream(&mut &include_bytes!("../assets/tray.png")[..]).ok();
    tray.set_icon(icon.clone());
    tray.set_tool_tip_text(Some("Claude 账号切换器".to_string()));
    tray.set_menu(Some(menu));
    tray.clicked(move |_| show());
    let icons = TrayIcons::new();
    icons.add(tray);
    TrayIcon::set_icons(app, Some(icons));
    window.set_icon(icon);
}

fn label(text: &str, size: f64) -> Ref<TextBlock> {
    let t = TextBlock::new();
    t.set_text(Some(text));
    t.set_font_size(size);
    t.set_text_wrapping(TextWrapping::Wrap);
    t
}

fn muted(text: &str) -> Ref<TextBlock> {
    let t = label(text, 13.0);
    t.set_foreground(Brush::parse("#969696").ok());
    t
}

fn button(text: &str) -> Ref<Button> {
    let b = Button::new();
    b.set_content(Some(Rc::new(text.to_string())));
    b
}

fn row() -> Ref<StackPanel> {
    let p = StackPanel::new();
    p.set_orientation(Orientation::Horizontal);
    p.set_spacing(8.0);
    p
}

fn column() -> Ref<StackPanel> {
    let p = StackPanel::new();
    p.set_spacing(12.0);
    p.set_margin(Thickness::uniform(16.0));
    p
}

fn tab(header: &str, content: Ref<StackPanel>) -> Option<ferroui::base::BoxedValue> {
    let item = TabItem::new();
    item.set_header(Some(Rc::new(header.to_string())));
    item.set_content(Some(Control::boxed(content)));
    Some(Control::boxed(item))
}

/// Results that worker threads hand back to the UI thread.
enum Msg {
    Accounts(String),
    Checks(Vec<String>),
    Guard(String),
}

/// Rebuilds the account rows; each switch button hands the work to a thread.
fn render_accounts(list: &StackPanel, title: &TextBlock, store: &Store, tx: &Sender<Msg>) {
    let current = store.current();
    title.set_text(Some(format!("当前账号：{current}").as_str()));
    list.children().clear();
    for name in store.list() {
        let r = row();
        let n = label(&name, 15.0);
        n.set_width(200.0);
        n.set_vertical_alignment(VerticalAlignment::Center);
        r.children().add(n.clone());
        let edit = TextBox::new();
        edit.set_width(200.0);
        edit.set_is_visible(false);
        r.children().add(edit.clone());
        let rename = rename_button(&name, n, edit, store, tx);
        let is_current = name == current;
        let b = button(if is_current { "使用中" } else { "切换到此账号" });
        b.set_is_enabled(!is_current);
        let delete = (!is_current).then(|| delete_button(&name, store, tx));
        let (store, tx) = (store.clone(), tx.clone());
        b.click(move |_, _| {
            let _ = tx.send(Msg::Accounts(format!("状态：正在切换到「{name}」…")));
            let (store, tx, name) = (store.clone(), tx.clone(), name.clone());
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
                let _ = tx.send(Msg::Accounts(match result {
                    Ok(()) => format!("状态：已切换到「{name}」，请重新打开 Claude"),
                    Err(e) => format!("状态：切换失败：{e}"),
                }));
            });
        });
        r.children().add(b);
        r.children().add(rename);
        if let Some(d) = delete {
            r.children().add(d);
        }
        list.children().add(r);
    }
}

/// "重命名" swaps the name label for an edit box; the second click ("保存") applies it.
fn rename_button(name: &str, label: Ref<TextBlock>, edit: Ref<TextBox>, store: &Store, tx: &Sender<Msg>) -> Ref<Button> {
    let b = button("重命名");
    let (name, store, tx) = (name.to_string(), store.clone(), tx.clone());
    b.click({
        let b = b.clone();
        move |_, _| {
            if !edit.is_visible() {
                edit.set_text(Some(name.as_str()));
                label.set_is_visible(false);
                edit.set_is_visible(true);
                edit.focus();
                b.set_content(Some(Rc::new("保存".to_string())));
                return;
            }
            let new = edit.text().unwrap_or_default();
            // Either way the list re-renders, which closes the edit box.
            let _ = tx.send(Msg::Accounts(match store.rename(&name, &new) {
                Ok(()) if new.trim() == name => "状态：就绪".to_string(),
                Ok(()) => format!("状态：已将「{name}」重命名为「{}」", new.trim()),
                Err(e) => format!("状态：重命名失败：{e}"),
            }));
        }
    });
    b
}

/// "删除" asks for a second click before moving the snapshot to the Recycle Bin.
fn delete_button(name: &str, store: &Store, tx: &Sender<Msg>) -> Ref<Button> {
    let b = button("删除");
    let armed = Cell::new(false);
    let (name, store, tx) = (name.to_string(), store.clone(), tx.clone());
    b.click({
        let b = b.clone();
        move |_, _| {
            if !armed.replace(true) {
                b.set_content(Some(Rc::new("确认删除？再点一次".to_string())));
                return;
            }
            b.set_is_enabled(false);
            let (name, store, tx) = (name.clone(), store.clone(), tx.clone());
            thread::spawn(move || {
                let _ = tx.send(Msg::Accounts(match store.remove(&name) {
                    Ok(()) => format!("状态：已删除「{name}」（快照已移入回收站）"),
                    Err(e) => format!("状态：{e}"),
                }));
            });
        }
    });
    b
}

/// The accounts tab, plus the callback that shows a status and re-renders it.
fn accounts_page(store: &Store, tx: &Sender<Msg>) -> (Ref<StackPanel>, impl Fn(&str)) {
    let page = column();
    let title = label("", 20.0);
    page.children().add(title.clone());
    page.children().add(muted("切换前会关闭 Claude 桌面版，并把登录数据（%APPDATA%\\Claude）存成该账号的快照。"));

    let list = StackPanel::new();
    list.set_spacing(6.0);
    page.children().add(list.clone());

    let add = row();
    let name = TextBox::new();
    name.set_placeholder_text(Some("新账号名称"));
    name.set_width(200.0);
    add.children().add(name.clone());
    let add_btn = button("添加账号");
    add_btn.click({
        let (store, tx) = (store.clone(), tx.clone());
        move |_, _| {
            let text = name.text().unwrap_or_default();
            let _ = tx.send(Msg::Accounts(match store.add(&text) {
                Ok(()) => {
                    name.set_text(Some(""));
                    format!("状态：已添加「{}」，切换过去后在 Claude 里登录即可", text.trim())
                }
                Err(e) => format!("状态：{e}"),
            }));
        }
    });
    add.children().add(add_btn);
    page.children().add(add);

    let quit = button("退出 Claude 桌面版");
    quit.click({
        let tx = tx.clone();
        move |_, _| {
            let tx = tx.clone();
            thread::spawn(move || {
                let _ = tx.send(Msg::Accounts(match accounts::quit_claude_desktop() {
                    Ok(()) => "状态：已结束 Claude 桌面版进程".to_string(),
                    Err(e) => format!("状态：退出失败：{e}"),
                }));
            });
        }
    });
    let login = button("登录");
    login.click({
        let tx = tx.clone();
        move |_, _| {
            let _ = tx.send(Msg::Accounts(match accounts::relogin_claude_code() {
                Ok(()) => "状态：已在新窗口中执行 claude auth logout，随后 claude auth login".to_string(),
                Err(e) => format!("状态：无法启动登录：{e}"),
            }));
        }
    });
    let actions = row();
    actions.children().add(quit);
    actions.children().add(login);
    page.children().add(actions);
    let status = muted("");
    page.children().add(status.clone());

    let (store, tx) = (store.clone(), tx.clone());
    let refresh = move |msg: &str| {
        status.set_text(Some(msg));
        render_accounts(&list, &title, &store, &tx);
    };
    refresh("状态：就绪");
    (page, refresh)
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

/// The network tab, plus the panels the channel writes results into.
fn network_page(tx: &Sender<Msg>) -> (Ref<StackPanel>, Ref<StackPanel>, Ref<TextBlock>) {
    let page = column();
    page.children().add(label("泄漏自查", 18.0));
    let results = StackPanel::new();
    results.set_spacing(4.0);
    results.children().add(muted("比较 HTTP、UDP（WebRTC 所用）与 IPv6 的出口地址，并列出 DNS 服务器。"));
    let start = button("开始检测");
    start.click({
        let (tx, results) = (tx.clone(), results.clone());
        move |_, _| {
            results.children().clear();
            results.children().add(muted("检测中…"));
            let tx = tx.clone();
            thread::spawn(move || {
                let _ = tx.send(Msg::Checks(netcheck::run_all()));
            });
        }
    });
    page.children().add(start);
    page.children().add(results.clone());

    page.children().add(label("断线保护", 18.0));
    let cfg = row();
    let adapter = TextBox::new();
    adapter.set_placeholder_text(Some("VPN 网卡名关键字，如 TUN"));
    adapter.set_width(220.0);
    let procs = TextBox::new();
    procs.set_placeholder_text(Some("要结束的进程，如 app.exe,b.exe"));
    procs.set_width(260.0);
    let saved = fs::read_to_string(guard_file()).unwrap_or_default();
    let mut saved = saved.lines();
    adapter.set_text(saved.next());
    procs.set_text(saved.next());
    let auto_on = saved.next() == Some("on");
    cfg.children().add(adapter.clone());
    cfg.children().add(procs.clone());
    page.children().add(cfg);

    let guard_status = muted("保护状态：未启用");
    let enable = CheckBox::new();
    enable.set_content(Some(Rc::new("启用：VPN 网卡断开时立即结束上述进程".to_string())));
    let stop: Rc<RefCell<Option<Arc<AtomicBool>>>> = Rc::default();
    enable.is_checked_changed({
        let (tx, enable, guard_status) = (tx.clone(), enable.clone(), guard_status.clone());
        move |_, _| {
            if let Some(flag) = stop.borrow_mut().take() {
                flag.store(true, Ordering::Relaxed);
            }
            let on = enable.is_checked() == Some(true);
            adapter.set_is_enabled(!on);
            procs.set_is_enabled(!on);
            if !on {
                save_guard(&adapter.text().unwrap_or_default(), &procs.text().unwrap_or_default(), false);
                guard_status.set_text(Some("保护状态：未启用"));
                return;
            }
            let keyword = adapter.text().unwrap_or_default().trim().to_string();
            let names: Vec<String> = procs
                .text()
                .unwrap_or_default()
                .split([',', '，', ' '])
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if keyword.is_empty() || names.is_empty() {
                // Unchecking re-enters this handler, which resets the status; set ours after.
                enable.set_is_checked(Some(false));
                guard_status.set_text(Some("保护状态：请先填写网卡关键字和进程名"));
                return;
            }
            save_guard(&keyword, &names.join(","), true);
            guard_status.set_text(Some(format!("保护状态：正在检查含「{keyword}」的网卡…").as_str()));
            let flag = Arc::new(AtomicBool::new(false));
            *stop.borrow_mut() = Some(flag.clone());
            let tx = tx.clone();
            // ponytail: guards only while this window is open; a scheduled task would cover logon-to-logoff.
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
                        let _ = tx.send(Msg::Guard(m));
                    }
                    thread::sleep(Duration::from_secs(3));
                }
            });
        }
    });
    // Fires the handler above, which validates and starts the guard.
    if auto_on {
        enable.set_is_checked(Some(true));
    }
    page.children().add(enable);
    page.children().add(guard_status.clone());

    let logon = CheckBox::new();
    logon.set_content(Some(Rc::new("登录 Windows 后自动启动本程序（配合上面的“启用”即可开机即保护）".to_string())));
    // Set before the handler is attached so the initial state is not written back.
    logon.set_is_checked(Some(autostart::is_enabled()));
    logon.is_checked_changed({
        let (logon, guard_status) = (logon.clone(), guard_status.clone());
        move |_, _| {
            let on = logon.is_checked() == Some(true);
            if let Err(e) = autostart::set(on) {
                guard_status.set_text(Some(format!("自动启动设置失败：{e}").as_str()));
            }
        }
    });
    page.children().add(logon);
    (page, results, guard_status)
}

fn create_main_window() -> Ref<Window> {
    let store = Store::default_paths();
    let (tx, rx) = channel();
    let (accounts, refresh) = accounts_page(&store, &tx);
    let (network, results, guard_status) = network_page(&tx);

    // Worker threads report through the channel; the UI thread drains it.
    let _timer = DispatcherTimer::run(
        move || {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    Msg::Accounts(s) => refresh(&s),
                    Msg::Checks(lines) => {
                        results.children().clear();
                        for l in lines {
                            let t = label(&l, 14.0);
                            if l.starts_with('⚠') {
                                t.set_foreground(Brush::parse("#E8A33D").ok());
                            }
                            results.children().add(t);
                        }
                    }
                    Msg::Guard(s) => guard_status.set_text(Some(s.as_str())),
                }
            }
            true
        },
        Duration::from_millis(200),
        DispatcherPriority::NORMAL,
    );

    let tabs = TabControl::new();
    tabs.items().add(tab("账号切换", accounts));
    tabs.items().add(tab("网络自检", network));

    let window = Window::new();
    window.set_title(Some("Claude 账号切换器".to_string()));
    window.set_width(640.0);
    window.set_height(600.0);
    window.set_content(Some(Control::boxed(tabs)));
    window
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exit_code = AppBuilder::configure::<App>().use_platform_detect().start_with_classic_desktop_lifetime(&args);
    std::process::ExitCode::from(exit_code as u8)
}
