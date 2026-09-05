#![cfg(target_os = "windows")]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use windows::{
    core::{w, HSTRING, PCWSTR},
    Win32::{
        Foundation::{GENERIC_ALL, HWND, LPARAM, LRESULT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            StationsAndDesktops::{CreateDesktopW, OpenDesktopW, SetThreadDesktop},
        },
        UI::Accessibility::NotifyWinEvent,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
            GetWindowThreadProcessId, IsWindow, PeekMessageW, RegisterClassW, SetWindowTextW,
            TranslateMessage, CHILDID_SELF, CREATESTRUCTW, CW_USEDEFAULT, EVENT_OBJECT_NAMECHANGE,
            HMENU, MSG, OBJID_CLIENT, PM_REMOVE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_NCCREATE,
            WNDCLASSW, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
        },
    },
};
use xa11y_core::{Locator, Provider};
use xa11y_windows::WindowsProvider;

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let _ = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

struct PrivateDesktopFixture {
    window: isize,
    commands: TcpStream,
    child: Child,
}

impl PrivateDesktopFixture {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture control socket");
        listener
            .set_nonblocking(true)
            .expect("make fixture listener nonblocking");
        let address = listener.local_addr().expect("fixture control address");
        let mut child = Command::new(std::env::current_exe().expect("test executable path"))
            .args([
                "--ignored",
                "--exact",
                "private_desktop_fixture_process",
                "--nocapture",
            ])
            .env("XA11Y_PRIVATE_DESKTOP_FIXTURE", address.to_string())
            .stdin(Stdio::null())
            .spawn()
            .expect("start private-desktop fixture process");

        let deadline = Instant::now() + Duration::from_secs(10);
        let commands = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if let Some(status) = child.try_wait().expect("inspect fixture process") {
                        panic!("fixture process exited before publishing its HWND: {status}");
                    }
                    assert!(Instant::now() < deadline, "fixture process did not connect");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept fixture connection: {error}"),
            }
        };
        commands
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("set fixture handshake timeout");
        let mut line = String::new();
        BufReader::new(commands.try_clone().expect("clone fixture stream"))
            .read_line(&mut line)
            .expect("read fixture HWND");
        let window = line.trim().parse().expect("parse fixture HWND");
        commands
            .set_read_timeout(None)
            .expect("clear fixture handshake timeout");

        Self {
            window,
            commands,
            child,
        }
    }

    fn rename_button(&mut self) {
        self.commands
            .write_all(b"r")
            .expect("send fixture rename command");
    }
}

impl Drop for PrivateDesktopFixture {
    fn drop(&mut self) {
        let _ = self.commands.write_all(b"s");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return;
                }
            }
        }
    }
}

fn run_private_desktop_fixture(address: &str) {
    unsafe {
        let name = HSTRING::from(format!("xa11y-native-window-{}", std::process::id()));
        let desktop = CreateDesktopW(
            PCWSTR(name.as_ptr()),
            PCWSTR::null(),
            None,
            Default::default(),
            GENERIC_ALL.0,
            None,
        )
        .expect("create private desktop");
        SetThreadDesktop(desktop).expect("attach private desktop");

        let module = GetModuleHandleW(None).expect("get test module");
        let class = w!("Xa11yPrivateDesktopFixture");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module.into(),
            lpszClassName: class,
            ..Default::default()
        });
        let window = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("Private desktop accessibility fixture"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            500,
            300,
            None,
            None,
            Some(module.into()),
            None,
        )
        .expect("create fixture window");
        let button = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("Toggle topology"),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
            20,
            20,
            180,
            40,
            Some(window),
            Some(HMENU(std::ptr::dangling_mut())),
            Some(module.into()),
            None,
        )
        .expect("create native button");

        let mut control = TcpStream::connect(address).expect("connect fixture control socket");
        writeln!(control, "{}", window.0 as isize).expect("publish fixture HWND");
        control
            .set_nonblocking(true)
            .expect("make fixture controls nonblocking");

        let mut message = MSG::default();
        let mut command = [0u8; 1];
        let mut running = true;
        while running {
            match control.read(&mut command) {
                Ok(1) if command[0] == b'r' => {
                    SetWindowTextW(button, w!("Topology changed")).expect("rename native button");
                    NotifyWinEvent(
                        EVENT_OBJECT_NAMECHANGE,
                        button,
                        OBJID_CLIENT.0,
                        CHILDID_SELF as i32,
                    );
                }
                Ok(1) if command[0] == b's' => running = false,
                Ok(0) => running = false,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => running = false,
            }
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = DestroyWindow(window);
        // This process exists only to own the fixture desktop. The OS detaches
        // its test thread and closes the desktop handle at process exit.
        let _ = desktop;
    }
}

#[test]
#[ignore = "fixture process launched by the private-desktop integration test"]
fn private_desktop_fixture_process() {
    let Ok(address) = std::env::var("XA11Y_PRIVATE_DESKTOP_FIXTURE") else {
        return;
    };
    run_private_desktop_fixture(&address);
}

#[test]
#[ignore = "requires an interactive Windows session with UI Automation"]
fn hwnd_root_actions_and_subscription_work_on_a_private_desktop() {
    let mut fixture = PrivateDesktopFixture::spawn();
    let desktop_name = HSTRING::from(format!("xa11y-native-window-{}", fixture.child.id()));
    let desktop = unsafe {
        OpenDesktopW(
            PCWSTR(desktop_name.as_ptr()),
            Default::default(),
            false,
            GENERIC_ALL.0,
        )
    }
    .expect("open fixture desktop");
    unsafe { SetThreadDesktop(desktop) }.expect("attach automation thread to target desktop");
    let native_window = HWND(fixture.window as *mut _);
    assert!(
        unsafe { IsWindow(Some(native_window)) }.as_bool(),
        "fixture HWND is not valid"
    );
    let target_thread = unsafe { GetWindowThreadProcessId(native_window, None) };
    assert_ne!(target_thread, 0, "resolve fixture window thread");
    let provider = std::sync::Arc::new(WindowsProvider::new().expect("create UIA provider"));
    let root = provider
        .element_from_native_window(fixture.window)
        .expect("resolve private-desktop HWND");
    assert_eq!(root.pid, Some(fixture.child.id()));

    let provider_dyn: std::sync::Arc<dyn Provider> = provider.clone();
    let button = Locator::new(
        provider_dyn,
        Some(root.clone()),
        r#"button[name="Toggle topology"]"#,
    );
    let _events = provider
        .subscribe(&root)
        .expect("subscribe from exact HWND root");
    std::thread::sleep(Duration::from_millis(250));
    button.press().expect("invoke native button through UIA");
    button.focus().expect("focus native button through UIA");
    fixture.rename_button();
    std::thread::sleep(Duration::from_millis(100));
    Locator::new(
        provider.clone(),
        Some(root),
        r#"button[name="Topology changed"]"#,
    )
    .element()
    .expect("observe the updated private-desktop accessibility tree");
}
