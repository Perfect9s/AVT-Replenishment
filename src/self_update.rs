//! Updates only the executable. No business workbook or JSON record is written.
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::*, Storage::FileSystem::ReplaceFileW, System::Threading::*,
    UI::WindowsAndMessaging::*,
};
fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(Some(0)).collect()
}
fn hash(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}
pub struct Prepared {
    pub dir: tempfile::TempDir,
    pub target: PathBuf,
    pub expected: String,
}
pub fn prepare(bytes: &[u8]) -> Result<Prepared> {
    let target = std::env::current_exe()?.canonicalize()?;
    let dir = tempfile::Builder::new()
        .prefix(".avt-update-")
        .tempdir_in(target.parent().context("无法找到程序目录")?)
        .context("程序目录不可写，请将应用放在有写入权限的文件夹后更新")?;
    let exe = crate::updates::extract_program(bytes)?;
    fs::write(dir.path().join("new.exe"), exe)?;
    fs::copy(&target, dir.path().join("helper.exe"))?;
    // Retain the exact old program for rollback before changing anything.
    fs::copy(&target, dir.path().join("old.exe"))?;
    let expected = hash(&dir.path().join("new.exe"))?;
    Ok(Prepared {
        dir,
        target,
        expected,
    })
}
pub fn launch(prepared: Prepared) -> Result<()> {
    Command::new(prepared.dir.path().join("helper.exe"))
        .arg("--avt-install-update")
        .arg(&prepared.target)
        .arg(std::process::id().to_string())
        .arg(&prepared.expected)
        .spawn()
        .context("无法启动更新助手，原程序未修改")?;
    let _ = prepared.dir.keep();
    Ok(())
}
fn wait_parent(pid: u32) -> Result<()> {
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            if GetLastError() == ERROR_INVALID_PARAMETER {
                return Ok(());
            }
            bail!("无法等待原程序退出");
        }
        let result = WaitForSingleObject(handle, 60000);
        CloseHandle(handle);
        if result != WAIT_OBJECT_0 {
            bail!("原程序未退出，更新已停止");
        }
    }
    Ok(())
}
fn replace(target: &Path, staged: &Path) -> Result<()> {
    let mut last = 0;
    for _ in 0..40 {
        unsafe {
            if ReplaceFileW(
                wide(target.as_os_str()).as_ptr(),
                wide(staged.as_os_str()).as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            ) != 0
            {
                return Ok(());
            }
            last = GetLastError();
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!("替换程序失败（Windows 错误 {last}），请关闭其他程序实例后重试")
}
fn valid_dir(dir: &Path, target: &Path) -> bool {
    dir.parent() == target.parent()
        && dir
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(".avt-update-"))
}
fn install(target: &Path, pid: u32, expected: &str) -> Result<()> {
    let dir = std::env::current_exe()?
        .canonicalize()?
        .parent()
        .context("无更新目录")?
        .to_path_buf();
    if !valid_dir(&dir, target) {
        bail!("更新目录不匹配");
    }
    let new = dir.join("new.exe");
    let old = dir.join("old.exe");
    if hash(&new)? != expected || expected.len() != 64 {
        bail!("待更新程序校验失败");
    }
    wait_parent(pid)?;
    if hash(target)? != hash(&old)? {
        bail!("原程序已发生变化，请重新检查更新");
    }
    let result = replace(target, &new).and_then(|()| {
        Command::new(target)
            .arg("--avt-clean-update")
            .arg(&dir)
            .arg(std::process::id().to_string())
            .spawn()
            .context("新版无法启动")?;
        Ok(())
    });
    if let Err(e) = result {
        // Restore through a same-volume atomic replacement if target still exists.
        if target.exists() {
            replace(target, &old).context("恢复旧程序失败，旧程序保留在更新目录的 old.exe")?;
        } else {
            fs::copy(&old, target).context("恢复旧程序失败")?;
        }
        let _ = Command::new(target)
            .arg("--avt-clean-update")
            .arg(&dir)
            .arg(std::process::id().to_string())
            .spawn();
        return Err(e);
    }
    Ok(())
}
pub fn handle_args() -> bool {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|s| s == "--avt-install-update") {
        let result = (|| -> Result<()> {
            if args.len() != 5 {
                bail!("更新参数不完整");
            }
            install(
                Path::new(&args[2]),
                args[3].to_string_lossy().parse()?,
                &args[4].to_string_lossy(),
            )
        })();
        if let Err(e) = result {
            unsafe {
                MessageBoxW(
                    std::ptr::null_mut(),
                    wide(std::ffi::OsStr::new(&format!("更新未完成：{e:#}"))).as_ptr(),
                    wide(std::ffi::OsStr::new("AVT 更新")).as_ptr(),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
        return true;
    }
    if args.get(1).is_some_and(|s| s == "--avt-clean-update") && args.len() == 4 {
        if let (Ok(target), Ok(pid)) = (
            std::env::current_exe().and_then(|p| p.canonicalize()),
            args[3].to_string_lossy().parse(),
        ) {
            let dir = PathBuf::from(&args[2]);
            if valid_dir(&dir, &target) {
                std::thread::spawn(move || {
                    if wait_parent(pid).is_ok() {
                        let _ = fs::remove_dir_all(dir);
                    }
                });
            }
        }
    }
    false
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replaces_and_rolls_back_without_touching_neighbors() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("app.exe");
        let new = dir.path().join("new.exe");
        let old = dir.path().join("old.exe");
        let workbook = dir.path().join("补货.xlsx");
        fs::write(&target, b"old").unwrap();
        fs::write(&old, b"old").unwrap();
        fs::write(&new, b"new").unwrap();
        fs::write(&workbook, b"workbook").unwrap();
        replace(&target, &new).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        replace(&target, &old).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert_eq!(fs::read(&workbook).unwrap(), b"workbook");
        assert!(!valid_dir(dir.path(), &target));
    }
}
