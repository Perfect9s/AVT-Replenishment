use anyhow::{bail, Context, Result};
use semver::Version;
use serde::Deserialize;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPOSITORY: &str = "Perfect9s/AVT-Replenishment";
pub const RELEASES_PAGE: &str = "https://github.com/Perfect9s/AVT-Replenishment/releases";
pub const API_PATH: &str = "/repos/Perfect9s/AVT-Replenishment/releases/latest";

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub asset_id: u64,
    pub asset_size: u64,
    pub digest: Option<String>,
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
    #[serde(default)]
    id: u64,
    #[serde(default)]
    digest: Option<String>,
    name: String,
    state: String,
    size: u64,
}

pub fn parse_release(status: u32, bytes: &[u8], current: &str) -> Result<UpdateInfo> {
    match status {
        200 => (),
        401 => bail!("更新服务拒绝了请求，请稍后重试。"),
        403 | 429 => bail!("GitHub 请求频率达到限制，请稍后重试。"),
        404 => {
            bail!("暂时无法找到最新正式版本，请稍后重试。")
        }
        300..=399 => bail!("更新地址发生重定向。本次检查已停止。"),
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
    let asset = release.assets.iter().find(|a| a.name == expected).unwrap();
    // Construct the URL from the fixed repository and a validated SemVer tag;
    // never open a URL supplied in an untrusted release response.
    Ok(UpdateInfo {
        asset_id: asset.id,
        asset_size: asset.size,
        digest: asset.digest.clone(),
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
        mem::size_of,
        ptr::{null, null_mut},
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::*, Networking::WinHttp::*, Security::Credentials::*, UI::WindowsAndMessaging::*,
    };
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

    pub fn check_latest() -> Result<UpdateInfo> {
        let (status, bytes, _) = request("api.github.com", API_PATH, false, 2 * 1024 * 1024)?;
        parse_release(status, &bytes, VERSION)
    }
    fn request(
        host: &str,
        path: &str,
        binary: bool,
        limit: usize,
    ) -> Result<(u32, Vec<u8>, Option<String>)> {
        unsafe {
            let session = Internet::new(WinHttpOpen(
                wide("AVT-Replenishment/1.2").as_ptr(),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                null(),
                null(),
                0,
            ))?;
            check(
                WinHttpSetTimeouts(session.0, 5000, 5000, 15000, 15000),
                "无法设置超时",
            )?;
            let connection = Internet::new(WinHttpConnect(session.0, wide(host).as_ptr(), 443, 0))?;
            let request = Internet::new(WinHttpOpenRequest(
                connection.0,
                wide("GET").as_ptr(),
                wide(path).as_ptr(),
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
                "无法限制重定向",
            )?;
            let accept = if binary {
                "application/octet-stream"
            } else {
                "application/vnd.github+json"
            };
            let headers = format!("Accept: {accept}\r\nX-GitHub-Api-Version: 2022-11-28\r\n");
            let h = wide(&headers);
            check(
                WinHttpSendRequest(request.0, h.as_ptr(), (h.len() - 1) as u32, null(), 0, 0, 0),
                "无法连接 GitHub，请检查网络或系统代理",
            )?;
            check(
                WinHttpReceiveResponse(request.0, null_mut()),
                "无法接收 GitHub 响应",
            )?;
            let mut status = 0u32;
            let mut length = 4u32;
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
            if status == 302 {
                let mut location = vec![0u16; 16384];
                let mut size = (location.len() * 2) as u32;
                check(
                    WinHttpQueryHeaders(
                        request.0,
                        WINHTTP_QUERY_LOCATION,
                        null(),
                        location.as_mut_ptr().cast(),
                        &mut size,
                        null_mut(),
                    ),
                    "无法读取下载地址",
                )?;
                let n = location
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(location.len());
                return Ok((
                    status,
                    Vec::new(),
                    Some(String::from_utf16_lossy(&location[..n])),
                ));
            }
            if status != 200 {
                return Ok((status, Vec::new(), None));
            }
            let started = Instant::now();
            let mut bytes = Vec::new();
            loop {
                if started.elapsed() > Duration::from_secs(180) {
                    bail!("下载超时，请重试。");
                }
                let mut chunk = [0u8; 65536];
                let mut read = 0;
                check(
                    WinHttpReadData(
                        request.0,
                        chunk.as_mut_ptr().cast(),
                        chunk.len() as u32,
                        &mut read,
                    ),
                    "下载中断，请重试",
                )?;
                if read == 0 {
                    break;
                }
                if bytes.len() + read as usize > limit {
                    bail!("下载文件超过大小限制。");
                }
                bytes.extend_from_slice(&chunk[..read as usize]);
            }
            Ok((status, bytes, None))
        }
    }
    pub fn download_update(info: &UpdateInfo) -> Result<Vec<u8>> {
        if info.asset_id == 0 || info.asset_size > 64 * 1024 * 1024 {
            bail!("更新包信息不完整或过大。");
        }
        let digest = info
            .digest
            .as_deref()
            .context("更新包缺少 SHA-256 校验值，已停止更新")?;
        let path = format!("/repos/{REPOSITORY}/releases/assets/{}", info.asset_id);
        let (mut status, mut bytes, location) =
            request("api.github.com", &path, true, 64 * 1024 * 1024)?;
        if status == 302 {
            let location = location.context("缺少下载地址")?;
            let path = super::asset_redirect_path(&location)?;
            // GitHub's signed asset URL is used without forwarding the Token.
            (status, bytes, _) = request(
                "release-assets.githubusercontent.com",
                path,
                true,
                64 * 1024 * 1024,
            )?;
        }
        if status != 200 {
            bail!("更新包下载失败（HTTP {status}）。请检查网络后重试。");
        }
        super::verify_package(&bytes, info.asset_size, digest)?;
        Ok(bytes)
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

pub fn asset_redirect_path(url: &str) -> Result<&str> {
    let path = url
        .strip_prefix("https://release-assets.githubusercontent.com/")
        .context("更新包下载地址不属于 GitHub，已停止")?;
    if path.is_empty() || url.chars().any(|c| c.is_control() || c == '\\') {
        bail!("更新包下载地址无效");
    }
    Ok(&url["https://release-assets.githubusercontent.com".len()..])
}
pub fn verify_package(bytes: &[u8], size: u64, digest: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    let expected = digest
        .strip_prefix("sha256:")
        .context("更新包校验算法不受支持")?;
    if expected.len() != 64
        || !expected.bytes().all(|b| b.is_ascii_hexdigit())
        || bytes.len() as u64 != size
        || format!("{:x}", Sha256::digest(bytes)) != expected.to_ascii_lowercase()
    {
        bail!("更新包大小或 SHA-256 校验失败，原程序未修改。");
    }
    Ok(())
}
pub fn extract_program(bytes: &[u8]) -> Result<Vec<u8>> {
    use std::io::{Cursor, Read};
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).context("更新包不是有效 ZIP")?;
    let name = "AVT-Replenishment-Windows-x64/AVT-Replenishment.exe";
    if archive.file_names().filter(|n| *n == name).count() != 1 {
        bail!("更新包必须包含唯一的应用程序。");
    }
    let file = archive.by_name(name)?;
    if file.size() > 64 * 1024 * 1024 {
        bail!("更新程序超过大小限制。");
    }
    let mut exe = Vec::new();
    file.take(64 * 1024 * 1024 + 1).read_to_end(&mut exe)?;
    if exe.len() > 64 * 1024 * 1024 || exe.get(..2) != Some(b"MZ") {
        bail!("更新程序格式无效。");
    }
    let offset = exe
        .get(60..64)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()) as usize)
        .context("更新程序缺少 PE 标头")?;
    if exe.get(offset..offset.saturating_add(6)) != Some(b"PE\0\0\x64\x86") {
        bail!("更新程序不是 Windows x64 应用。");
    }
    Ok(exe)
}
#[cfg(test)]
mod package_tests {
    use super::*;
    #[test]
    fn download_integrity_and_redirect_boundaries() {
        use sha2::{Digest, Sha256};
        let bytes = b"package";
        let digest = format!("sha256:{:x}", Sha256::digest(bytes));
        verify_package(bytes, 7, &digest).unwrap();
        assert!(verify_package(b"changed", 7, &digest).is_err());
        assert!(verify_package(bytes, 8, &digest).is_err());
        assert!(verify_package(bytes, 7, "sha256:bad").is_err());
        assert_eq!(
            asset_redirect_path("https://release-assets.githubusercontent.com/path?a=b").unwrap(),
            "/path?a=b"
        );
        for url in [
            "http://release-assets.githubusercontent.com/path",
            "https://evil.com/path",
            "https://release-assets.githubusercontent.com.evil.com/path",
            "https://release-assets.githubusercontent.com@evil.com/path",
            "https://release-assets.githubusercontent.com/\\evil",
        ] {
            assert!(asset_redirect_path(url).is_err());
        }
    }
    #[test]
    fn extracts_only_fixed_program_path() {
        use std::io::{Cursor, Write};
        let mut exe = vec![0u8; 128];
        exe[..2].copy_from_slice(b"MZ");
        exe[60..64].copy_from_slice(&64u32.to_le_bytes());
        exe[64..70].copy_from_slice(b"PE\0\0\x64\x86");
        let pack = |name: &str, data: &[u8]| {
            let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
            z.start_file(name, zip::write::FileOptions::default())
                .unwrap();
            z.write_all(data).unwrap();
            z.finish().unwrap().into_inner()
        };
        assert_eq!(
            extract_program(&pack(
                "AVT-Replenishment-Windows-x64/AVT-Replenishment.exe",
                &exe
            ))
            .unwrap(),
            exe
        );
        assert!(extract_program(&pack("../../AVT-Replenishment.exe", &exe)).is_err());
        assert!(extract_program(&pack(
            "AVT-Replenishment-Windows-x64/AVT-Replenishment.exe",
            b"not exe"
        ))
        .is_err());
    }
}
