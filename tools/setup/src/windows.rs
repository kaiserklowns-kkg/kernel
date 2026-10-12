//! What only Windows does for Oceans Setup (ADR-0128): checking this PC,
//! making the staging partition, the UEFI boot entries, the restart.
//! Disk work goes through PowerShell's Storage module (what Windows' own
//! Disk Management uses); firmware variables through the Win32 API.

use std::os::windows::process::CommandExt;
use std::process::Command;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LUID};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, GetTokenInformation, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION, TOKEN_PRIVILEGES, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::SystemInformation::{
    FIRMWARE_TYPE, FirmwareTypeUefi, GetFirmwareType,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::System::WindowsProgramming::{
    GetFirmwareEnvironmentVariableW, SetFirmwareEnvironmentVariableExW,
};

use crate::efi;

/// No console window for PowerShell.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Non-volatile, boot service and runtime access.
const VARIABLE_ATTRIBUTES: u32 = 7;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Whether this process runs as administrator.
pub fn elevated() -> bool {
    // SAFETY: plain Win32 calls on this process's token, closed after.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

pub fn uefi() -> bool {
    let mut kind: FIRMWARE_TYPE = 0;
    // SAFETY: writes one value.
    unsafe { GetFirmwareType(&mut kind) != 0 && kind == FirmwareTypeUefi }
}

/// Lets this process read and write firmware variables.
pub fn firmware_access() -> Result<(), String> {
    // SAFETY: plain Win32 calls on this process's token, closed after.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        ) == 0
        {
            return Err("cannot open this program's token".into());
        }
        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        let name = wide("SeSystemEnvironmentPrivilege");
        let mut ok = LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) != 0;
        if ok {
            let privileges = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [windows_sys::Win32::Security::LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            ok = AdjustTokenPrivileges(
                token,
                0,
                &privileges,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ) != 0;
        }
        CloseHandle(token);
        if ok {
            Ok(())
        } else {
            Err("Windows did not allow access to the firmware's settings".into())
        }
    }
}

/// A UEFI global variable, if it exists.
pub fn read_variable(name: &str) -> Option<Vec<u8>> {
    let mut buffer = vec![0u8; 4096];
    let name = wide(name);
    let guid = wide(efi::GLOBAL);
    // SAFETY: the buffer is `buffer.len()` bytes; Windows writes at most that.
    let len = unsafe {
        GetFirmwareEnvironmentVariableW(
            name.as_ptr(),
            guid.as_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
        )
    };
    (len != 0).then(|| {
        buffer.truncate(len as usize);
        buffer
    })
}

pub fn write_variable(name: &str, value: &[u8]) -> Result<(), String> {
    let wide_name = wide(name);
    let guid = wide(efi::GLOBAL);
    // SAFETY: `value` is read for `value.len()` bytes.
    let ok = unsafe {
        SetFirmwareEnvironmentVariableExW(
            wide_name.as_ptr(),
            guid.as_ptr(),
            value.as_ptr().cast(),
            value.len() as u32,
            VARIABLE_ATTRIBUTES,
        )
    };
    if ok != 0 {
        Ok(())
    } else {
        Err(format!("the firmware refused to store {name}"))
    }
}

/// Runs a PowerShell script; its output, or why not.
pub fn powershell(script: &str) -> Result<String, String> {
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("cannot run PowerShell: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Downloads `url` (PowerShell's web client: Windows' TLS and proxy).
pub fn download(url: &str) -> Result<Vec<u8>, String> {
    let path = std::env::temp_dir().join(format!("oceans-setup-{}.part", std::process::id()));
    let script = format!(
        "$ProgressPreference = 'SilentlyContinue'; $ErrorActionPreference = 'Stop'; \
         Invoke-WebRequest -UseBasicParsing -Uri '{url}' -OutFile '{}'",
        path.display()
    );
    powershell(&script).map_err(|e| format!("{url}: {e}"))?;
    let bytes = std::fs::read(&path).map_err(|e| format!("{url}: {e}"))?;
    let _ = std::fs::remove_file(&path);
    Ok(bytes)
}

/// Restarts Windows in a few seconds.
pub fn restart() -> Result<(), String> {
    Command::new("shutdown.exe")
        .args(["/r", "/t", "5", "/c", "Installing Oceans"])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("cannot restart: {e}"))
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err("Windows did not restart".into())
            }
        })
}

/// Restarts this program as administrator (Windows asks the user).
pub fn relaunch_elevated() -> bool {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let file = wide(&exe.display().to_string());
    let verb = wide("runas");
    // SAFETY: null-terminated strings that live through the call.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    result as usize > 32
}
