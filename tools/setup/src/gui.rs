//! Oceans Setup's window: what it will do, a log of what it did, one
//! button for the next step, and the box where the user types ERASE.
//! The steps run on a worker thread; it posts its lines and its result
//! back to the window.

use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{COLOR_WINDOW, HBRUSH};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{EM_REPLACESEL, EM_SETSEL};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::{plan, release, windows};

const BUTTON: isize = 1;
/// A log line (a boxed `String` in `lparam`).
const WM_SAY: u32 = WM_APP + 1;
/// A step ended (`wparam`: the next step, or `FAILED`).
const WM_DONE: u32 = WM_APP + 2;

const CHECK: usize = 0;
const DOWNLOAD: usize = 1;
const CONFIRM: usize = 2;
const INSTALLING: usize = 3;
const FAILED: usize = 9;

/// What the steps share.
#[derive(Default)]
struct State {
    disk: Option<plan::SystemDisk>,
    release: Option<release::Release>,
}

struct Window {
    hwnd: HWND,
    log: HWND,
    button: HWND,
    erase: HWND,
    step: usize,
    state: Arc<Mutex<State>>,
}

static mut WINDOW: Option<Window> = None;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn window() -> &'static mut Window {
    // SAFETY: only the UI thread touches it, after `run` set it.
    unsafe {
        (*std::ptr::addr_of_mut!(WINDOW))
            .as_mut()
            .expect("the window")
    }
}

/// A handle that may cross to the worker (only posted to).
#[derive(Clone, Copy)]
struct Poster(usize);

impl Poster {
    fn say(self, text: &str) {
        let boxed = Box::into_raw(Box::new(text.to_string()));
        // SAFETY: the window frees the box when it takes the message.
        unsafe { PostMessageW(self.0 as HWND, WM_SAY, 0, boxed as LPARAM) };
    }

    fn done(self, next: usize) {
        // SAFETY: a plain post.
        unsafe { PostMessageW(self.0 as HWND, WM_DONE, next, 0) };
    }
}

fn append(text: &str) {
    let w = window();
    let line = wide(&format!("{text}\r\n"));
    // SAFETY: the log is an edit control of this window.
    unsafe {
        let len = GetWindowTextLengthW(w.log);
        SendMessageW(w.log, EM_SETSEL, len as WPARAM, len as LPARAM);
        SendMessageW(w.log, EM_REPLACESEL, 0, line.as_ptr() as LPARAM);
    }
}

fn set_button(text: &str, enabled: bool) {
    let w = window();
    let text = wide(text);
    // SAFETY: the button is a child of this window.
    unsafe {
        SetWindowTextW(w.button, text.as_ptr());
        EnableWindow(w.button, i32::from(enabled));
    }
}

fn erase_typed() -> bool {
    let w = window();
    let mut buffer = [0u16; 16];
    // SAFETY: at most `buffer.len()` characters are written.
    let len = unsafe { GetWindowTextW(w.erase, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..len.max(0) as usize]).trim() == "ERASE"
}

/// Runs `step` on a worker thread.
fn spawn(step: impl FnOnce(Poster, &Mutex<State>) -> usize + Send + 'static) {
    let w = window();
    let poster = Poster(w.hwnd as usize);
    let state = Arc::clone(&w.state);
    std::thread::spawn(move || {
        let next = step(poster, &state);
        poster.done(next);
    });
}

fn check(poster: Poster, state: &Mutex<State>) -> usize {
    poster.say("Checking this PC...");
    if !windows::uefi() {
        poster.say("  This PC starts in legacy BIOS mode: Oceans needs UEFI.");
        return FAILED;
    }
    if let Some(value) = windows::read_variable("SecureBoot")
        && value.first() == Some(&1)
    {
        poster.say(
            "  Secure Boot is on. Oceans is not signed by Microsoft yet: turn Secure Boot \
             off in the firmware settings (Settings > System > Recovery > Advanced startup > \
             UEFI Firmware Settings), then run Oceans Setup again.",
        );
        return FAILED;
    }
    let disk = match windows::powershell(plan::SYSTEM_DISK_SCRIPT)
        .and_then(|t| plan::parse_system_disk(&t))
    {
        Ok(disk) => disk,
        Err(problem) => {
            poster.say(&format!("  The disk could not be examined: {problem}"));
            return FAILED;
        }
    };
    let why = plan::refusals(&disk);
    if !why.is_empty() {
        for reason in why {
            poster.say(&format!("  {reason}"));
        }
        return FAILED;
    }
    poster.say(&format!(
        "  UEFI, Secure Boot off, Windows on {} ({} GB, NVMe): Oceans can be installed.",
        disk.model,
        disk.size / 1_000_000_000
    ));
    state.lock().expect("state").disk = Some(disk);
    DOWNLOAD
}

fn download(poster: Poster, state: &Mutex<State>) -> usize {
    poster.say("Downloading Oceans...");
    let Some((key, publisher)) = release::release_key() else {
        poster.say("  This Oceans Setup has no release key.");
        return FAILED;
    };
    let fetched = release::check(
        key,
        publisher,
        |name| windows::download(&format!("{}{name}", release::RELEASES)),
        |line| poster.say(&format!("  {line}")),
    );
    match fetched {
        Ok(checked) => {
            let model = state
                .lock()
                .expect("state")
                .disk
                .as_ref()
                .map_or(String::new(), |d| d.model.clone());
            poster.say("");
            poster.say(&format!(
                "Installing Oceans erases EVERYTHING on {model}: Windows, your files and \
                 your apps. Copy what you want to keep elsewhere first."
            ));
            poster.say("To go on, type ERASE in the box below, then press Install.");
            state.lock().expect("state").release = Some(checked);
            CONFIRM
        }
        Err(problem) => {
            poster.say(&format!("  {problem}"));
            FAILED
        }
    }
}

fn install(poster: Poster, state: &Mutex<State>) -> usize {
    let state = state.lock().expect("state");
    let (Some(disk), Some(checked)) = (&state.disk, &state.release) else {
        return FAILED;
    };
    let say = |line: &str| poster.say(line);
    match crate::install_now(disk, checked, &say) {
        Ok(()) => INSTALLING,
        Err(problem) => {
            poster.say(&format!("Not installed: {problem}"));
            poster.say("Windows was not changed unless said above.");
            FAILED
        }
    }
}

fn clicked() {
    let w = window();
    match w.step {
        CHECK => {
            set_button("Checking...", false);
            spawn(check);
        }
        DOWNLOAD => {
            set_button("Downloading...", false);
            spawn(download);
        }
        CONFIRM => {
            if !erase_typed() {
                append("Type ERASE (in capitals) to install.");
                return;
            }
            set_button("Installing...", false);
            spawn(install);
        }
        _ => {}
    }
}

fn finished(next: usize) {
    let w = window();
    w.step = next;
    match next {
        DOWNLOAD => set_button("Download Oceans", true),
        CONFIRM => {
            set_button("Install", true);
            // SAFETY: a child of this window.
            unsafe { ShowWindow(w.erase, SW_SHOW) };
        }
        INSTALLING => set_button("Restarting...", false),
        _ => set_button("Close", false),
    }
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND if (wparam & 0xffff) as isize == BUTTON => {
            clicked();
            0
        }
        WM_SAY => {
            // SAFETY: posted by `Poster::say`, taken once.
            let text = unsafe { Box::from_raw(lparam as *mut String) };
            append(&text);
            0
        }
        WM_DONE => {
            finished(wparam);
            0
        }
        WM_DESTROY => {
            // SAFETY: plain Win32.
            unsafe { PostQuitMessage(0) };
            0
        }
        // SAFETY: the default for everything else.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

pub fn run() {
    if !windows::elevated() {
        // Disk and firmware work needs administrator rights; Windows asks.
        windows::relaunch_elevated();
        return;
    }
    // SAFETY: the usual window set-up and message loop, all on this thread.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let class = wide("OceansSetup");
        let title = wide("Oceans Setup");
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(procedure),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            680,
            520,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        let child = |class: &str, text: &str, style: u32, x, y, w, h, id: isize| {
            let class = wide(class);
            let text = wide(text);
            CreateWindowExW(
                0,
                class.as_ptr(),
                text.as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                x,
                y,
                w,
                h,
                hwnd,
                id as _,
                instance,
                std::ptr::null(),
            )
        };
        child(
            "STATIC",
            "Oceans Setup installs Oceans on this PC in place of Windows. It checks the PC \
             first, then downloads Oceans and checks its signature. Nothing changes until you \
             confirm.",
            0,
            16,
            12,
            640,
            48,
            0,
        );
        let log = child(
            "EDIT",
            "",
            WS_BORDER | WS_VSCROLL | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
            16,
            68,
            640,
            330,
            0,
        );
        let erase = child("EDIT", "", WS_BORDER, 16, 412, 200, 26, 0);
        ShowWindow(erase, SW_HIDE);
        let button = child(
            "BUTTON",
            "Check this PC",
            BS_PUSHBUTTON as u32,
            496,
            412,
            160,
            30,
            BUTTON,
        );
        *std::ptr::addr_of_mut!(WINDOW) = Some(Window {
            hwnd,
            log,
            button,
            erase,
            step: CHECK,
            state: Arc::new(Mutex::new(State::default())),
        });
        let mut message = std::mem::zeroed::<MSG>();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}
