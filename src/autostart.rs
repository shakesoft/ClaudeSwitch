//! Start at logon through the per-user Run key (no admin rights needed).

use std::io::{self, Error, ErrorKind};
use std::process::Command;

const KEY: &str = r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Run";
const NAME: &str = "ClaudeSwitcher";

fn powershell(script: &str) -> io::Result<String> {
    let exe = std::env::current_exe()?;
    // Paths go in through the environment so they are never parsed as script.
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", script])
        .env("AS_KEY", KEY)
        .env("AS_NAME", NAME)
        .env("AS_VALUE", format!("\"{}\" --tray", exe.display()))
        .output()?;
    if !out.status.success() {
        return Err(Error::new(ErrorKind::Other, String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// True only when the Run entry points at this very executable.
pub fn is_enabled() -> bool {
    powershell("(Get-ItemProperty -Path $env:AS_KEY -Name $env:AS_NAME -ErrorAction SilentlyContinue).$env:AS_NAME -eq $env:AS_VALUE")
        .is_ok_and(|s| s == "True")
}

pub fn set(on: bool) -> io::Result<()> {
    powershell(if on {
        "Set-ItemProperty -Path $env:AS_KEY -Name $env:AS_NAME -Value $env:AS_VALUE"
    } else {
        "Remove-ItemProperty -Path $env:AS_KEY -Name $env:AS_NAME -ErrorAction SilentlyContinue"
    })
    .map(drop)
}
