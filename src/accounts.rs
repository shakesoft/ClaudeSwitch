//! Account snapshots: each account is a parked copy of Claude desktop's data
//! directory; switching renames the live directory away and the target in.

use std::fs;
use std::io::{self, Error, ErrorKind};
use std::path::PathBuf;
use std::process::Command;

#[derive(Clone)]
pub struct Store {
    /// Claude desktop's data directory (`%APPDATA%\Claude`).
    pub live: PathBuf,
    /// Where snapshots and the current account name live.
    pub root: PathBuf,
}

impl Store {
    pub fn default_paths() -> Store {
        let env = |k| PathBuf::from(std::env::var_os(k).unwrap_or_default());
        Store { live: env("APPDATA").join("Claude"), root: env("USERPROFILE").join(".claude-switcher") }
    }

    fn snapshot(&self, name: &str) -> PathBuf {
        self.root.join("accounts").join(name)
    }

    pub fn current(&self) -> String {
        fs::read_to_string(self.root.join("current.txt"))
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "默认".to_string())
    }

    /// The current account first, then the parked ones by name.
    pub fn list(&self) -> Vec<String> {
        let current = self.current();
        let mut parked: Vec<String> = fs::read_dir(self.root.join("accounts"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| *n != current)
            .collect();
        parked.sort();
        parked.insert(0, current);
        parked
    }

    /// Trims `name` and checks it is a usable folder name not taken by any
    /// account other than `except` (so a rename may change only the case).
    fn check_new_name<'a>(&self, name: &'a str, except: &str) -> io::Result<&'a str> {
        let name = name.trim();
        let bad = name.is_empty()
            || name == "."
            || name == ".."
            || name.chars().any(|c| r#"\/:*?"<>|"#.contains(c) || c.is_control());
        if bad {
            return Err(Error::new(ErrorKind::InvalidInput, "账号名不能为空，也不能包含 \\ / : * ? \" < > |"));
        }
        if self.list().iter().any(|n| n != except && n.eq_ignore_ascii_case(name)) {
            return Err(Error::new(ErrorKind::AlreadyExists, "已有同名账号"));
        }
        Ok(name)
    }

    pub fn add(&self, name: &str) -> io::Result<()> {
        let name = self.check_new_name(name, "")?;
        fs::create_dir_all(self.snapshot(name))
    }

    /// Renames a parked account's snapshot folder, or just relabels the
    /// current account (its data is the live directory, which keeps its path).
    pub fn rename(&self, old: &str, new: &str) -> io::Result<()> {
        if new.trim() == old {
            return Ok(());
        }
        let new = self.check_new_name(new, old)?;
        if old == self.current() {
            fs::create_dir_all(&self.root)?;
            return fs::write(self.root.join("current.txt"), new);
        }
        let snap = self.snapshot(old);
        if old.is_empty() || !snap.is_dir() {
            return Err(Error::new(ErrorKind::NotFound, format!("找不到账号「{old}」的快照")));
        }
        fs::rename(snap, self.snapshot(new))
    }

    /// Parks the live directory as the current account and moves `target`'s
    /// snapshot in. Claude must not be running: Windows refuses to rename a
    /// directory with open files, which is what keeps this from tearing data.
    pub fn switch(&self, target: &str) -> io::Result<()> {
        let current = self.current();
        if target == current {
            return Ok(());
        }
        let snap = self.snapshot(target);
        if !snap.is_dir() {
            return Err(Error::new(ErrorKind::NotFound, format!("找不到账号「{target}」的快照")));
        }
        let park = self.snapshot(&current);
        if park.exists() {
            return Err(Error::new(ErrorKind::AlreadyExists, format!("{} 已存在，为避免覆盖已中止", park.display())));
        }
        if self.live.exists() {
            fs::rename(&self.live, &park).map_err(|e| {
                Error::new(e.kind(), format!("无法移动 Claude 数据目录（Claude 可能仍在运行）：{e}"))
            })?;
        } else {
            fs::create_dir_all(&park)?;
        }
        if let Err(e) = fs::rename(&snap, &self.live) {
            // Put the live directory back so Claude still has its data.
            let _ = fs::rename(&park, &self.live);
            return Err(e);
        }
        fs::write(self.root.join("current.txt"), target)
    }
}

impl Store {
    /// Moves a parked account's snapshot to the Recycle Bin, so a mistaken
    /// delete can still be undone. The current account cannot be deleted:
    /// its data is the live directory Claude is using.
    pub fn remove(&self, name: &str) -> io::Result<()> {
        if name == self.current() {
            return Err(Error::new(ErrorKind::InvalidInput, "不能删除当前正在使用的账号，请先切换到其他账号"));
        }
        let snap = self.snapshot(name);
        if name.is_empty() || !snap.is_dir() {
            return Err(Error::new(ErrorKind::NotFound, format!("找不到账号「{name}」的快照")));
        }
        // The path goes in through the environment so it is never parsed as script.
        let out = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "Add-Type -AssemblyName Microsoft.VisualBasic; \
                 [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory($env:CS_PATH, 'OnlyErrorDialogs', 'SendToRecycleBin')",
            ])
            .env("CS_PATH", &snap)
            .output()?;
        if snap.exists() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(Error::new(ErrorKind::Other, format!("删除失败：{}", err.lines().next().unwrap_or("未知错误"))));
        }
        Ok(())
    }
}

/// Force-quits Claude desktop. Matches by install path, because Claude Code
/// (the CLI) is also called `claude.exe` and must be left alone.
pub fn quit_claude_desktop() -> io::Result<()> {
    let script = r"Get-Process claude -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -match 'AnthropicClaude|\\WindowsApps\\Claude|\\Programs\\Claude' } |
        Stop-Process -Force";
    Command::new("powershell.exe").args(["-NoProfile", "-Command", script]).status()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_round_trip_keeps_each_accounts_data() {
        let dir = std::env::temp_dir().join(format!("claude-switcher-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = Store { live: dir.join("live"), root: dir.join("root") };
        fs::create_dir_all(&store.live).unwrap();
        fs::write(store.live.join("token"), "A").unwrap();

        store.add("B").unwrap();
        assert!(store.add("b").is_err(), "duplicate names are rejected");
        assert!(store.add("..").is_err(), "path tricks are rejected");
        assert_eq!(store.list(), ["默认", "B"]);

        store.switch("B").unwrap();
        assert_eq!(store.current(), "B");
        assert!(!store.live.join("token").exists(), "B starts empty");
        fs::write(store.live.join("token"), "B").unwrap();

        store.switch("默认").unwrap();
        assert_eq!(fs::read_to_string(store.live.join("token")).unwrap(), "A");
        store.switch("B").unwrap();
        assert_eq!(fs::read_to_string(store.live.join("token")).unwrap(), "B");
        assert!(store.switch("nope").is_err());
        assert!(store.remove("B").is_err(), "the account in use cannot be deleted");
        assert!(store.remove("nope").is_err());
        assert!(store.remove("").is_err(), "must never resolve to the accounts folder itself");

        // Rename the parked account, then the current one; data must follow.
        store.rename("默认", "个人").unwrap();
        assert_eq!(store.list(), ["B", "个人"]);
        assert!(store.rename("个人", "b").is_err(), "cannot take another account's name");
        assert!(store.rename("个人", "a/b").is_err());
        store.rename("B", "工作").unwrap();
        assert_eq!(store.current(), "工作");
        store.rename("工作", "工作").unwrap();
        store.switch("个人").unwrap();
        assert_eq!(fs::read_to_string(store.live.join("token")).unwrap(), "A");
        store.switch("工作").unwrap();
        assert_eq!(fs::read_to_string(store.live.join("token")).unwrap(), "B");

        fs::remove_dir_all(&dir).unwrap();
    }
}
