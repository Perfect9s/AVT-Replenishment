//! Native Win32 UI. Worker threads exchange owned data only; HWNDs stay on the UI thread.
use avt_replenishment::core::{self, Outcome, Plan};
use avt_replenishment::updates::{self, windows as updater, UpdateInfo};
use std::{
    collections::BTreeMap,
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr::{null, null_mut},
    sync::mpsc::{self, Receiver},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::*,
    UI::{
        Controls::Dialogs::*, Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*,
        WindowsAndMessaging::*,
    },
};
use zeroize::Zeroizing;

const PATH: i32 = 101;
const PICK: i32 = 102;
const SCAN: i32 = 103;
const LIST: i32 = 104;
const NOTES: i32 = 105;
const BACKUP: i32 = 106;
const ACK: i32 = 107;
const FILL: i32 = 108;
const SUMMARY: i32 = 109;
const STATUS: i32 = 110;
const PIN: i32 = 111;
const UPDATE: i32 = 112;
const AUTHORIZE: i32 = 113;
const FORGET: i32 = 114;
const RELEASE: i32 = 115;
enum Job {
    Inspect(Result<Plan, String>),
    Fill(Result<Outcome, String>),
    Update(Result<UpdateInfo, String>),
    Download(Result<avt_replenishment::self_update::Prepared, String>),
}
struct State {
    hwnd: HWND,
    controls: BTreeMap<i32, HWND>,
    font: HFONT,
    title_font: HFONT,
    scale: f64,
    plan: Option<Plan>,
    worker: Option<Receiver<Job>>,
    done: bool,
    token: Option<Zeroizing<String>>,
    available: Option<UpdateInfo>,
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn display(p: &std::path::Path) -> String {
    p.display().to_string()
}
unsafe fn text(hwnd: HWND, s: &str) {
    SetWindowTextW(hwnd, wide(s).as_ptr());
}
unsafe fn get_text(hwnd: HWND) -> String {
    let n = GetWindowTextLengthW(hwnd);
    let mut b = vec![0u16; n as usize + 1];
    GetWindowTextW(hwnd, b.as_mut_ptr(), b.len() as i32);
    String::from_utf16_lossy(&b[..n as usize])
}
impl State {
    fn c(&self, id: i32) -> HWND {
        *self.controls.get(&id).unwrap_or(&null_mut())
    }
    unsafe fn control(&mut self, id: i32, class: &str, label: &str, style: u32) {
        let h = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(label).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            0,
            0,
            self.hwnd,
            id as usize as HMENU,
            GetModuleHandleW(null()),
            null(),
        );
        self.controls.insert(id, h);
        SendMessageW(h, WM_SETFONT, self.font as usize, 1);
    }
    unsafe fn set_busy(&self, busy: bool) {
        for id in [
            PATH, PICK, SCAN, BACKUP, ACK, UPDATE, AUTHORIZE, FORGET, RELEASE,
        ] {
            EnableWindow(self.c(id), (!busy) as i32);
        }
        EnableWindow(
            self.c(RELEASE),
            (!busy && self.available.as_ref().is_some_and(|i| i.newer)) as i32,
        );
        self.ready();
    }
    unsafe fn ready(&self) {
        let ack = SendMessageW(self.c(ACK), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
        let ready = self.worker.is_none()
            && !self.done
            && self
                .plan
                .as_ref()
                .is_some_and(|p| p.warnings.is_empty() || ack);
        EnableWindow(self.c(FILL), ready as i32);
    }
    unsafe fn start(&mut self, path: PathBuf) {
        if self.worker.is_some() {
            return;
        }
        self.plan = None;
        self.done = false;
        text(self.c(PATH), &display(&path));
        text(self.c(SUMMARY), "正在识别站点、日期和源文件…");
        text(self.c(NOTES), "");
        text(self.c(STATUS), "识别期间不会修改任何文件。");
        SendMessageW(self.c(LIST), LVM_DELETEALLITEMS, 0, 0);
        SendMessageW(self.c(ACK), BM_SETCHECK, BST_UNCHECKED as usize, 0);
        ShowWindow(self.c(ACK), SW_HIDE);
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        self.set_busy(true);
        std::thread::spawn(move || {
            let _ = tx.send(Job::Inspect(
                core::inspect(&path).map_err(|e| format!("{e:#}")),
            ));
        });
    }
    unsafe fn fill(&mut self) {
        if self.worker.is_some() || self.done {
            return;
        }
        let Some(p) = self.plan.clone() else {
            return;
        };
        if get_text(self.c(PATH)).trim().trim_matches('"') != display(&p.target) {
            self.start(PathBuf::from(
                get_text(self.c(PATH)).trim().trim_matches('"'),
            ));
            return;
        }
        if !p.warnings.is_empty()
            && SendMessageW(self.c(ACK), BM_GETCHECK, 0, 0) != BST_CHECKED as isize
        {
            return;
        }
        let backup = SendMessageW(self.c(BACKUP), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        self.set_busy(true);
        text(
            self.c(STATUS),
            "正在填充和校验，完成后覆盖原表。请勿打开或移动文件…",
        );
        std::thread::spawn(move || {
            let _ = tx.send(Job::Fill(
                core::execute(&p, backup).map_err(|e| format!("{e:#}")),
            ));
        });
    }
    unsafe fn check_update(&mut self) {
        if self.worker.is_some() {
            return;
        }
        let Some(token) = self.token.clone() else {
            text(
                self.c(STATUS),
                "请先设置更新授权，私有仓库需要只读 GitHub Token。",
            );
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        self.set_busy(true);
        self.available = None;
        text(self.c(STATUS), "正在检查 GitHub 正式版本…");
        std::thread::spawn(move || {
            let _ = tx.send(Job::Update(
                updater::check_latest(&token).map_err(|e| format!("{e:#}")),
            ));
        });
    }
    unsafe fn download_update(&mut self) {
        if self.worker.is_some() {
            return;
        }
        let (Some(token), Some(info)) = (self.token.clone(), self.available.clone()) else {
            return;
        };
        if !info.newer {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        self.set_busy(true);
        text(
            self.c(STATUS),
            "正在下载并校验新版，请稍候。成功后自动重启；补货表不会修改。",
        );
        std::thread::spawn(move || {
            let result = updater::download_update(&token, &info)
                .and_then(|bytes| avt_replenishment::self_update::prepare(&bytes));
            let _ = tx.send(Job::Download(result.map_err(|e| format!("{e:#}"))));
        });
    }
    unsafe fn show_plan(&mut self, p: Plan) {
        text(self.c(PATH), &display(&p.target));
        text(
            self.c(SUMMARY),
            &format!(
                "{} 站    |    日期 {}    |    {} 个源文件    |    将更新 {} 张工作表",
                p.station,
                p.date,
                p.sources.len(),
                p.sheets.len()
            ),
        );
        for (i, s) in p.sources.iter().enumerate() {
            let fields = [
                s.sheet.clone(),
                s.country.clone(),
                s.path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                s.rows.to_string(),
                s.converted.to_string(),
                s.modified.clone(),
            ];
            for (j, field) in fields.iter().enumerate() {
                let mut b = wide(field);
                let item = LVITEMW {
                    mask: LVIF_TEXT,
                    iItem: i as i32,
                    iSubItem: j as i32,
                    pszText: b.as_mut_ptr(),
                    ..zeroed()
                };
                SendMessageW(
                    self.c(LIST),
                    if j == 0 {
                        LVM_INSERTITEMW
                    } else {
                        LVM_SETITEMTEXTW
                    },
                    if j == 0 { 0 } else { i },
                    &item as *const _ as isize,
                );
            }
        }
        let mut notes = format!("站点目录：{}\r\n", display(&p.root));
        if p.warnings.is_empty() {
            notes.push_str("匹配完成。请核对源文件后点击“确认填充并覆盖原表”。\r\n");
        } else {
            notes.push_str("需要核对：\r\n");
            for w in &p.warnings {
                notes.push_str(&format!("• {w}\r\n"));
            }
        }
        notes.push_str("\r\n源文件完整路径：\r\n");
        for s in &p.sources {
            notes.push_str(&format!(
                "{} / {}：{} [{}]\r\n",
                s.sheet,
                s.country,
                display(&s.path),
                s.encoding
            ));
        }
        text(self.c(NOTES), &notes);
        ShowWindow(
            self.c(ACK),
            if p.warnings.is_empty() {
                SW_HIDE
            } else {
                SW_SHOW
            },
        );
        text(
            self.c(STATUS),
            "已识别，尚未填充。普通数字按固定列转换；货币、百分比内容保持原样。",
        );
        self.plan = Some(p);
        self.ready();
    }
    unsafe fn poll(&mut self) {
        let job = match &self.worker {
            Some(rx) => match rx.try_recv() {
                Ok(j) => Some(j),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Job::Inspect(Err("处理线程意外退出，请重新识别。".into())))
                }
                Err(_) => None,
            },
            None => None,
        };
        if let Some(job) = job {
            self.worker = None;
            match job {
                Job::Inspect(Ok(p)) => self.show_plan(p),
                Job::Fill(Ok(o)) => {
                    self.done = true;
                    text(
                        self.c(STATUS),
                        &format!(
                            "填充完成：{} 张工作表，{} 个普通数字已转换。",
                            o.sheets, o.converted
                        ),
                    );
                    let mut msg = format!("{}\r\n\r\n原表：{}\r\n", o.message, display(&o.target));
                    if let Some(p) = o.backup {
                        msg.push_str(&format!("备份：{}\r\n", display(&p)));
                    } else {
                        msg.push_str("本次未创建备份。\r\n");
                    }
                    text(self.c(NOTES), &msg);
                }
                Job::Update(Ok(info)) => {
                    let message = if info.newer {
                        format!(
                            "发现新版本 v{}，当前 v{}。点击“下载并更新”，完成后自动重启。",
                            info.latest,
                            updates::VERSION
                        )
                    } else {
                        format!(
                            "当前 v{}，最新正式版本 v{}，无需更新。",
                            updates::VERSION,
                            info.latest
                        )
                    };
                    self.available = Some(info);
                    text(self.c(STATUS), &message);
                }
                Job::Download(Ok(prepared)) => {
                    match avt_replenishment::self_update::launch(prepared) {
                        Ok(()) => {
                            PostMessageW(self.hwnd, WM_CLOSE, 0, 0);
                        }
                        Err(e) => {
                            text(self.c(STATUS), &format!("无法启动更新：{e:#}"));
                        }
                    }
                }
                Job::Download(Err(e)) => {
                    text(self.c(STATUS), &format!("更新未完成：{e}"));
                }
                Job::Update(Err(e)) => {
                    text(self.c(STATUS), &format!("更新检查失败：{e}"));
                }
                Job::Inspect(Err(e)) | Job::Fill(Err(e)) => {
                    text(self.c(STATUS), "处理未完成，请查看下方说明。");
                    text(self.c(NOTES), &e);
                }
            }
            self.set_busy(false);
        }
    }
    unsafe fn layout(&self) {
        let mut rect = zeroed();
        GetClientRect(self.hwnd, &mut rect);
        let w = (rect.right as f64 / self.scale) as i32;
        let h = (rect.bottom as f64 / self.scale) as i32;
        let table_h = (h - 430).max(130);
        for (id, x, y, ww, hh) in [
            (1, 24, 18, 260, 34),
            (PIN, w - 596, 20, 108, 30),
            (UPDATE, w - 480, 18, 108, 34),
            (AUTHORIZE, w - 364, 18, 94, 34),
            (FORGET, w - 262, 18, 94, 34),
            (RELEASE, w - 160, 18, 136, 34),
            (2, 24, 59, w - 48, 25),
            (PATH, 24, 99, w - 240, 32),
            (PICK, w - 206, 98, 90, 34),
            (SCAN, w - 106, 98, 82, 34),
            (SUMMARY, 24, 151, w - 48, 26),
            (LIST, 24, 186, w - 48, table_h),
            (NOTES, 24, 198 + table_h, w - 48, 110),
            (ACK, 24, h - 83, w - 48, 24),
            (BACKUP, 24, h - 49, 240, 28),
            (FILL, w - 280, h - 53, 256, 36),
            (STATUS, 24, h - 117, w - 48, 25),
        ] {
            MoveWindow(
                self.c(id),
                (x as f64 * self.scale) as i32,
                (y as f64 * self.scale) as i32,
                (ww as f64 * self.scale) as i32,
                (hh as f64 * self.scale) as i32,
                1,
            );
        }
        let widths = [
            112,
            52,
            (w - 48 - 112 - 52 - 70 - 80 - 180 - 8).max(220),
            70,
            80,
            180,
        ];
        for (i, width) in widths.iter().enumerate() {
            SendMessageW(
                self.c(LIST),
                LVM_SETCOLUMNWIDTH,
                i,
                (*width as f64 * self.scale) as isize,
            );
        }
    }
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_CREATE {
        let scale = GetDpiForWindow(hwnd) as f64 / 96.0;
        let font = CreateFontW(
            (-16.0 * scale) as i32,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            0,
            0,
            CLEARTYPE_QUALITY as u32,
            0,
            wide("Microsoft YaHei UI").as_ptr(),
        );
        let title_font = CreateFontW(
            (-25.0 * scale) as i32,
            0,
            0,
            0,
            600,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            0,
            0,
            CLEARTYPE_QUALITY as u32,
            0,
            wide("Microsoft YaHei UI").as_ptr(),
        );
        let mut s = Box::new(State {
            hwnd,
            controls: BTreeMap::new(),
            font,
            title_font,
            scale,
            plan: None,
            worker: None,
            done: false,
            token: updater::load_token().unwrap_or(None),
            available: None,
        });
        s.control(1, "STATIC", "AVT  补货计划填充", 0);
        SendMessageW(s.c(1), WM_SETFONT, title_font as usize, 1);
        s.control(
            PIN,
            "BUTTON",
            "图钉置顶",
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        );
        s.control(UPDATE, "BUTTON", "检查更新", WS_TABSTOP);
        s.control(AUTHORIZE, "BUTTON", "更新授权", WS_TABSTOP);
        s.control(FORGET, "BUTTON", "清除授权", WS_TABSTOP);
        s.control(RELEASE, "BUTTON", "下载并更新", WS_TABSTOP);
        EnableWindow(s.c(RELEASE), 0);
        s.control(
            2,
            "STATIC",
            "拖入补货计划 → 核对识别结果 → 选择备份并确认填充     US / CA / EU / UK / JP",
            0,
        );
        s.control(
            PATH,
            "EDIT",
            "",
            WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
        );
        SendMessageW(
            s.c(PATH),
            EM_SETCUEBANNER,
            0,
            wide("将 .xlsx 补货计划拖入窗口，或粘贴完整路径").as_ptr() as isize,
        );
        s.control(PICK, "BUTTON", "选择文件", WS_TABSTOP);
        s.control(SCAN, "BUTTON", "识别", WS_TABSTOP);
        s.control(SUMMARY, "STATIC", "等待拖入补货计划…", 0);
        s.control(
            LIST,
            "SysListView32",
            "",
            WS_BORDER | WS_TABSTOP | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
        );
        SendMessageW(
            s.c(LIST),
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (LVS_EX_FULLROWSELECT | LVS_EX_GRIDLINES | LVS_EX_DOUBLEBUFFER) as isize,
        );
        for (i, name) in ["工作表", "国家", "源文件", "数据行", "数字转换", "修改时间"]
            .iter()
            .enumerate()
        {
            let mut b = wide(name);
            let col = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH,
                cx: 100,
                pszText: b.as_mut_ptr(),
                ..zeroed()
            };
            SendMessageW(s.c(LIST), LVM_INSERTCOLUMNW, i, &col as *const _ as isize);
        }
        s.control(NOTES,"EDIT","源文件从补货表所在目录自动查找。\r\n业务报告按文件日期匹配；库存和限制发货数量选择修改时间最新的文件。\r\n保留 US + MX 和 EU 多国拼接逻辑。",WS_BORDER|WS_VSCROLL|WS_TABSTOP|ES_MULTILINE as u32|ES_AUTOVSCROLL as u32|ES_READONLY as u32);
        s.control(
            STATUS,
            "STATIC",
            "只转换业务报告固定数量列，货币和百分比保持原样。",
            0,
        );
        s.control(
            ACK,
            "BUTTON",
            "我已核对提示，按上述匹配结果填充",
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        );
        ShowWindow(s.c(ACK), SW_HIDE);
        s.control(
            BACKUP,
            "BUTTON",
            "填充前备份原表（带时间戳）",
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        );
        SendMessageW(s.c(BACKUP), BM_SETCHECK, BST_CHECKED as usize, 0);
        s.control(
            FILL,
            "BUTTON",
            "确认填充并覆盖原表",
            WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
        );
        EnableWindow(s.c(FILL), 0);
        s.layout();
        DragAcceptFiles(hwnd, 1);
        SetTimer(hwnd, 1, 100, None);
        // State becomes reachable only after controls are fully created.
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(s) as isize);
        PostMessageW(hwnd, WM_APP + 1, 0, 0);
        return 0;
    }
    if ![
        WM_SIZE,
        WM_TIMER,
        WM_COMMAND,
        WM_DROPFILES,
        WM_CLOSE,
        WM_DESTROY,
        WM_NCDESTROY,
        WM_GETMINMAXINFO,
        WM_APP + 1,
    ]
    .contains(&msg)
    {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    if msg == WM_COMMAND && (wp >> 16) != BN_CLICKED as usize {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    // Native credential dialogs pump messages. Do not hold a mutable State
    // reference while the modal dialog is active.
    if msg == WM_COMMAND {
        let command = (wp & 0xffff) as i32;
        if (command == AUTHORIZE || (command == UPDATE && (*ptr).token.is_none()))
            && (*ptr).worker.is_none()
        {
            let result = updater::prompt_token(hwnd);
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
            if ptr.is_null() {
                return 0;
            }
            let s = &mut *ptr;
            match result {
                Ok(Some(token)) => {
                    s.token = Some(token);
                    text(s.c(STATUS), "更新授权已设置，点击“检查更新”连接私有仓库。");
                    if command == UPDATE {
                        s.check_update();
                    }
                }
                Ok(None) => (),
                Err(e) => {
                    text(s.c(STATUS), &format!("更新授权未保存：{e:#}"));
                }
            }
            return 0;
        }
    }
    let s = &mut *ptr;
    match msg {
        WM_SIZE => s.layout(),
        WM_TIMER => s.poll(),
        WM_COMMAND => match (wp & 0xffff) as i32 {
            PIN => {
                let pinned = SendMessageW(s.c(PIN), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
                if let Err(e) = updater::set_topmost(hwnd, pinned) {
                    SendMessageW(
                        s.c(PIN),
                        BM_SETCHECK,
                        if pinned { BST_UNCHECKED } else { BST_CHECKED } as usize,
                        0,
                    );
                    text(s.c(STATUS), &e.to_string());
                }
            }
            UPDATE => s.check_update(),
            FORGET => {
                s.token = None;
                s.available = None;
                EnableWindow(s.c(RELEASE), 0);
                match updater::forget_token() {
                    Ok(()) => {
                        text(
                            s.c(STATUS),
                            "已清除本次会话及 Windows 凭据管理器中的更新授权。",
                        );
                    }
                    Err(e) => {
                        text(s.c(STATUS), &e.to_string());
                    }
                }
            }
            RELEASE => s.download_update(),
            PICK => {
                let mut path = vec![0u16; 32768];
                let filter = wide("补货计划 (*.xlsx)\0*.xlsx\0\0");
                let mut dialog: OPENFILENAMEW = zeroed();
                dialog.lStructSize = size_of::<OPENFILENAMEW>() as u32;
                dialog.hwndOwner = hwnd;
                dialog.lpstrFilter = filter.as_ptr();
                dialog.lpstrFile = path.as_mut_ptr();
                dialog.nMaxFile = path.len() as u32;
                dialog.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
                if GetOpenFileNameW(&mut dialog) != 0 {
                    let n = path.iter().position(|&c| c == 0).unwrap_or(path.len());
                    s.start(PathBuf::from(String::from_utf16_lossy(&path[..n])));
                }
            }
            SCAN => s.start(PathBuf::from(get_text(s.c(PATH)).trim().trim_matches('"'))),
            FILL => s.fill(),
            ACK => s.ready(),
            _ => {}
        },
        WM_DROPFILES => {
            let drop = wp as HDROP;
            let count = DragQueryFileW(drop, u32::MAX, null_mut(), 0);
            if count == 1 && s.worker.is_none() {
                let len = DragQueryFileW(drop, 0, null_mut(), 0);
                let mut buf = vec![0u16; len as usize + 1];
                DragQueryFileW(drop, 0, buf.as_mut_ptr(), buf.len() as u32);
                let p = PathBuf::from(String::from_utf16_lossy(&buf[..len as usize]));
                DragFinish(drop);
                s.start(p);
            } else {
                DragFinish(drop);
                text(s.c(STATUS), "请等待当前任务结束后，每次拖入一份补货计划。");
            }
        }
        WM_CLOSE => {
            if s.worker.is_some() {
                text(s.c(STATUS), "正在处理，请等待完成后关闭窗口。");
            } else {
                DestroyWindow(hwnd);
            }
        }
        WM_DESTROY => {
            KillTimer(hwnd, 1);
            PostQuitMessage(0);
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DeleteObject(s.font);
            DeleteObject(s.title_font);
            drop(Box::from_raw(ptr));
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        WM_GETMINMAXINFO => {
            let info = &mut *(lp as *mut MINMAXINFO);
            info.ptMinTrackSize.x = (960.0 * s.scale) as i32;
            info.ptMinTrackSize.y = (640.0 * s.scale) as i32;
        }
        _ => {
            if msg == WM_APP + 1 {
                if let Some(p) = std::env::args_os().nth(1) {
                    s.start(PathBuf::from(p));
                }
            }
        }
    }
    0
}
pub fn run() {
    unsafe {
        SetProcessDPIAware();
        InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES,
        });
        let instance = GetModuleHandleW(null());
        let class = wide("AVTReplenishmentWindow");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
            ..zeroed()
        };
        RegisterClassW(&wc);
        let scale = GetDpiForSystem() as f64 / 96.0;
        let hwnd = CreateWindowExW(
            WS_EX_ACCEPTFILES,
            class.as_ptr(),
            wide(&format!("AVT 补货计划填充  v{}", updates::VERSION)).as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            (1080.0 * scale) as i32,
            (800.0 * scale) as i32,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            MessageBoxW(
                null_mut(),
                wide("无法创建应用窗口。").as_ptr(),
                wide("AVT").as_ptr(),
                MB_ICONERROR,
            );
            return;
        }
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
