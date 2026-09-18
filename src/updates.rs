use anyhow::{bail, Context, Result};
use semver::Version;
use serde::Deserialize;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPOSITORY: &str = "Perfect9s/AVT-Replenishment";
pub const RELEASES_PAGE: &str = "https://github.com/Perfect9s/AVT-Replenishment/releases";
pub const API_PATH: &str = "/repos/Perfect9s/AVT-Replenishment/releases/latest";

#[derive(Debug)]
pub struct UpdateInfo {
    pub latest: String,
    pub newer: bool,
    pub release_url: String,
}
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    state: String,
    size: u64,
}

pub fn validate_token(token: &str) -> Result<()> {
    if token.is_empty()
        || token.len() > 1024
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        bail!("请填写有效的 GitHub Token；不要填写 GitHub 登录密码。");
    }
    Ok(())
}

pub fn parse_release(status: u32, bytes: &[u8], current: &str) -> Result<UpdateInfo> {
    match status {
        200 => (),
        401 => bail!("GitHub 授权无效或已过期，请重新设置更新授权。"),
        403 | 429 => bail!("GitHub 拒绝请求或已限流，请确认该仓库的 Contents 只读权限，稍后重试。"),
        404 => {
            bail!("无法读取私有仓库的最新版本：可能尚未发布正式版本，或 Token 未获该仓库访问权限。")
        }
        300..=399 => bail!("更新地址发生重定向。为避免将凭据发送到其他地址，本次检查已停止。"),
        _ => bail!("更新检查失败（HTTP {status}），请稍后重试。"),
    }
    if bytes.len() > 2 * 1024 * 1024 {
        bail!("更新响应过大，已停止处理。");
    }
    let release: Release = serde_json::from_slice(bytes).context("GitHub 版本信息格式无效")?;
    if release.draft || release.prerelease {
        bail!("更新源返回了非正式版本，本次不推荐更新。");
    }
    let tag = release.tag_name;
    let v = Version::parse(tag.strip_prefix('v').unwrap_or(&tag)).context("版本号格式无效")?;
    if !v.pre.is_empty() || !v.build.is_empty() {
        bail!("仅支持正式版本号。");
    }
    let current = Version::parse(current)?;
    let expected = format!("AVT-Replenishment-Windows-x64-v{v}.zip");
    if !release
        .assets
        .iter()
        .any(|a| a.name == expected && a.state == "uploaded" && a.size > 0)
    {
        bail!("最新 Release 的 Windows 安装包尚未就绪，请稍后重试。");
    }
    // Construct the URL from the fixed repository and a validated SemVer tag;
    // never open a URL supplied in an untrusted release response.
    Ok(UpdateInfo {
        latest: v.to_string(),
        newer: v > current,
        release_url: format!("{RELEASES_PAGE}/tag/{tag}"),
    })
}

#[cfg(windows)]
pub mod windows {
    use super::*;
    use std::{
        ffi::c_void,
        mem::{size_of, zeroed},
        ptr::{null, null_mut},
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::*, Networking::WinHttp::*, Security::Credentials::*, UI::WindowsAndMessaging::*,
    };
    use zeroize::Zeroizing;
    const TARGET: &str = "AVT-Replenishment/GitHub/Perfect9s/AVT-Replenishment";
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    fn check(ok: i32, context: &str) -> Result<()> {
        if ok == 0 {
            bail!("{context}（Windows 错误 {}）。", unsafe {
                GetLastError()
            });
        }
        Ok(())
    }
    struct Internet(*mut c_void);
    impl Internet {
        fn new(p: *mut c_void) -> Result<Self> {
            if p.is_null() {
                bail!("无法建立更新连接（Windows 错误 {}）。", unsafe {
                    GetLastError()
                });
            }
            Ok(Self(p))
        }
    }
    impl Drop for Internet {
        fn drop(&mut self) {
            unsafe {
                WinHttpCloseHandle(self.0);
            }
        }
    }

    pub fn check_latest(token: &str) -> Result<UpdateInfo> {
        validate_token(token)?;
        unsafe {
            let session = Internet::new(WinHttpOpen(
                wide("AVT-Replenishment/1.1").as_ptr(),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                null(),
                null(),
                0,
            ))?;
            check(
                WinHttpSetTimeouts(session.0, 5000, 5000, 10000, 10000),
                "无法设置连接超时",
            )?;
            let connection = Internet::new(WinHttpConnect(
                session.0,
                wide("api.github.com").as_ptr(),
                443,
                0,
            ))?;
            let request = Internet::new(WinHttpOpenRequest(
                connection.0,
                wide("GET").as_ptr(),
                wide(API_PATH).as_ptr(),
                null(),
                null(),
                null(),
                WINHTTP_FLAG_SECURE,
            ))?;
            let redirect = WINHTTP_OPTION_REDIRECT_POLICY_NEVER;
            check(
                WinHttpSetOption(
                    request.0,
                    WINHTTP_OPTION_REDIRECT_POLICY,
                    &redirect as *const _ as *const c_void,
                    size_of::<u32>() as u32,
                ),
                "无法限制更新重定向",
            )?;
            let headers = Zeroizing::new(format!("Accept: application/vnd.github+json\r\nAuthorization: Bearer {token}\r\nX-GitHub-Api-Version: 2022-11-28\r\n"));
            let header_wide = Zeroizing::new(wide(&headers));
            check(
                WinHttpSendRequest(
                    request.0,
                    header_wide.as_ptr(),
                    (header_wide.len() - 1) as u32,
                    null(),
                    0,
                    0,
                    0,
                ),
                "无法连接 GitHub，请检查网络或系统代理",
            )?;
            check(
                WinHttpReceiveResponse(request.0, null_mut()),
                "无法接收 GitHub 响应",
            )?;
            let mut status = 0u32;
            let mut length = size_of::<u32>() as u32;
            check(
                WinHttpQueryHeaders(
                    request.0,
                    WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                    null(),
                    &mut status as *mut _ as *mut c_void,
                    &mut length,
                    null_mut(),
                ),
                "无法读取更新状态",
            )?;
            if status != 200 {
                return parse_release(status, &[], VERSION);
            }
            let started = Instant::now();
            let mut bytes = Vec::new();
            loop {
                if started.elapsed() > Duration::from_secs(30) {
                    bail!("更新检查超时，请稍后重试。");
                }
                let mut chunk = [0u8; 8192];
                let mut read = 0;
                check(
                    WinHttpReadData(
                        request.0,
                        chunk.as_mut_ptr().cast(),
                        chunk.len() as u32,
                        &mut read,
                    ),
                    "更新响应读取失败",
                )?;
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..read as usize]);
                if bytes.len() > 2 * 1024 * 1024 {
                    bail!("更新响应过大，已停止处理。");
                }
            }
            parse_release(status, &bytes, VERSION)
        }
    }

    pub fn load_token() -> Result<Option<Zeroizing<String>>> {
        unsafe {
            let mut credential: *mut CREDENTIALW = null_mut();
            if CredReadW(wide(TARGET).as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) == 0 {
                if GetLastError() == ERROR_NOT_FOUND {
                    return Ok(None);
                }
                bail!("无法读取 Windows 凭据管理器，请重新设置更新授权。");
            }
            let result = if (*credential).CredentialBlobSize == 0 {
                None
            } else {
                let bytes = std::slice::from_raw_parts(
                    (*credential).CredentialBlob,
                    (*credential).CredentialBlobSize as usize,
                );
                Some(Zeroizing::new(String::from_utf8_lossy(bytes).into_owned()))
            };
            CredFree(credential.cast());
            if let Some(t) = &result {
                validate_token(t)?;
            }
            Ok(result)
        }
    }
    pub fn forget_token() -> Result<()> {
        unsafe {
            if CredDeleteW(wide(TARGET).as_ptr(), CRED_TYPE_GENERIC, 0) == 0
                && GetLastError() != ERROR_NOT_FOUND
            {
                bail!("无法删除已保存的更新授权，请在 Windows 凭据管理器中删除 AVT-Replenishment 项。");
            }
            Ok(())
        }
    }
    fn save_token(token: &str) -> Result<()> {
        unsafe {
            validate_token(token)?;
            let mut target = wide(TARGET);
            let mut user = wide("GitHub Token");
            let cred = CREDENTIALW {
                Type: CRED_TYPE_GENERIC,
                TargetName: target.as_mut_ptr(),
                CredentialBlobSize: token.len() as u32,
                CredentialBlob: token.as_ptr() as *mut u8,
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                UserName: user.as_mut_ptr(),
                ..zeroed()
            };
            check(CredWriteW(&cred, 0), "无法保存到 Windows 凭据管理器")
        }
    }
    /// Uses the Windows credential dialog; token is never written to a config/log file.
    /// # Safety
    /// Owner must be a valid window handle. Caller must not hold references to window state across this modal call.
    pub unsafe fn prompt_token(owner: HWND) -> Result<Option<Zeroizing<String>>> {
        let message = wide("用户名可保持 GitHub；密码栏填写 GitHub Token（不是登录密码）。\n仅授权 Perfect9s/AVT-Replenishment，Contents: Read-only。\n勾选保存则存入 Windows 凭据管理器；不勾选则仅本次会话使用。");
        let caption = wide("AVT 私有仓库更新授权");
        let ui = CREDUI_INFOW {
            cbSize: size_of::<CREDUI_INFOW>() as u32,
            hwndParent: owner,
            pszMessageText: message.as_ptr(),
            pszCaptionText: caption.as_ptr(),
            hbmBanner: null_mut(),
        };
        let mut user = vec![0u16; 256];
        let name = wide("GitHub");
        user[..name.len()].copy_from_slice(&name);
        let mut password = Zeroizing::new(vec![0u16; 1025]);
        let mut save = 0;
        let code = CredUIPromptForCredentialsW(
            &ui,
            wide(TARGET).as_ptr(),
            null(),
            0,
            user.as_mut_ptr(),
            user.len() as u32,
            password.as_mut_ptr(),
            password.len() as u32,
            &mut save,
            CREDUI_FLAGS_GENERIC_CREDENTIALS
                | CREDUI_FLAGS_ALWAYS_SHOW_UI
                | CREDUI_FLAGS_DO_NOT_PERSIST
                | CREDUI_FLAGS_SHOW_SAVE_CHECK_BOX,
        );
        if code == ERROR_CANCELLED {
            return Ok(None);
        }
        if code != 0 {
            bail!("无法打开更新授权窗口（Windows 错误 {code}）。");
        }
        let n = password
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(password.len());
        let token = Zeroizing::new(String::from_utf16_lossy(&password[..n]).trim().to_owned());
        validate_token(&token)?;
        if save != 0 {
            save_token(&token)?;
        } else {
            forget_token()?;
        }
        Ok(Some(token))
    }
    /// # Safety
    /// hwnd must refer to a live window owned by the calling thread.
    pub unsafe fn set_topmost(hwnd: HWND, pinned: bool) -> Result<()> {
        unsafe {
            check(
                SetWindowPos(
                    hwnd,
                    if pinned { HWND_TOPMOST } else { HWND_NOTOPMOST },
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                ),
                "无法更改窗口置顶状态",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn release(tag: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"tag_name":tag,"draft":false,"prerelease":false,"assets":[{"name":format!("AVT-Replenishment-Windows-x64-{tag}.zip"),"state":"uploaded","size":1200}]})).unwrap()
    }
    #[test]
    fn semantic_versions_and_fixed_links() {
        let u = parse_release(200, &release("v1.10.0"), "1.9.0").unwrap();
        assert!(u.newer);
        assert_eq!(u.release_url, format!("{RELEASES_PAGE}/tag/v1.10.0"));
        assert!(
            !parse_release(200, &release("v1.1.0"), "1.1.0")
                .unwrap()
                .newer
        );
        assert!(
            !parse_release(200, &release("v1.0.1"), "1.1.0")
                .unwrap()
                .newer
        );
    }
    #[test]
    fn rejects_auth_errors_and_unready_releases() {
        for code in [401, 403, 404, 429, 302, 500] {
            assert!(parse_release(code, &[], VERSION).is_err());
        }
        for tag in ["../../evil", "v1.2.0-rc1", "v1.2.0+meta"] {
            assert!(parse_release(200, &release(tag), VERSION).is_err());
        }
        let mut r: serde_json::Value = serde_json::from_slice(&release("v2.0.0")).unwrap();
        r["assets"] = serde_json::json!([]);
        assert!(parse_release(200, &serde_json::to_vec(&r).unwrap(), VERSION).is_err());
        r["draft"] = serde_json::json!(true);
        assert!(parse_release(200, &serde_json::to_vec(&r).unwrap(), VERSION).is_err());
    }
    #[test]
    fn tokens_cannot_inject_headers() {
        assert!(validate_token("github_pat_example123").is_ok());
        for t in ["", "abc\r\nX-Evil: 1", "not a token", "abc\0def"] {
            assert!(validate_token(t).is_err());
        }
    }
}

#[cfg(all(test, windows))]
mod native_tests {
    use super::windows::set_topmost;
    use std::ptr::null_mut;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    #[test]
    fn pin_toggles_without_moving_or_resizing() {
        unsafe {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                40,
                50,
                400,
                300,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            );
            assert!(!hwnd.is_null());
            let mut before = std::mem::zeroed();
            assert_ne!(GetWindowRect(hwnd, &mut before), 0);
            set_topmost(hwnd, true).unwrap();
            assert_ne!(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST, 0);
            set_topmost(hwnd, false).unwrap();
            assert_eq!(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST, 0);
            let mut after = std::mem::zeroed();
            assert_ne!(GetWindowRect(hwnd, &mut after), 0);
            assert_eq!(
                (before.left, before.top, before.right, before.bottom),
                (after.left, after.top, after.right, after.bottom)
            );
            DestroyWindow(hwnd);
        }
    }
}
