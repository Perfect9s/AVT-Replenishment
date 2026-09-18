//! An owned, non-activating caption button alongside the system caption controls.
use std::{
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    System::LibraryLoader::*,
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
const PROPERTY: &str = "AVT.CaptionPin";
struct Pin {
    owner: HWND,
    hover: bool,
    background: u32,
}
unsafe fn button(owner: HWND) -> HWND {
    GetPropW(owner, wide(PROPERTY).as_ptr())
}
unsafe fn position(owner: HWND, pin: HWND) {
    if IsWindowVisible(owner) == 0 || IsIconic(owner) != 0 {
        ShowWindow(pin, SW_HIDE);
        return;
    }
    let mut window: RECT = zeroed();
    GetWindowRect(owner, &mut window);
    let mut bounds: RECT = zeroed();
    let dpi = GetDpiForWindow(owner);
    let width = (44 * dpi / 96) as i32;
    if DwmGetWindowAttribute(
        owner,
        DWMWA_CAPTION_BUTTON_BOUNDS as u32,
        &mut bounds as *mut _ as _,
        size_of::<RECT>() as u32,
    ) < 0
        || bounds.right <= bounds.left
        || bounds.bottom <= bounds.top
    {
        let frame = GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi)
            + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
        bounds = RECT {
            left: window.right - window.left - frame - 3 * width,
            top: frame,
            right: window.right - window.left - frame,
            bottom: frame + GetSystemMetricsForDpi(SM_CYCAPTION, dpi),
        };
    }
    SetWindowPos(
        pin,
        null_mut(),
        window.left + bounds.left - width,
        window.top + bounds.top,
        width,
        bounds.bottom - bounds.top,
        SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
    );
    EnableWindow(pin, IsWindowEnabled(owner));
    InvalidateRect(pin, null(), 0);
}
/// # Safety
/// Owner must be a live window on this thread. Call once per window.
pub unsafe fn create(owner: HWND) {
    let class = wide("AVTCaptionPin");
    let instance = GetModuleHandleW(null());
    let wc = WNDCLASSW {
        lpfnWndProc: Some(proc),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        ..zeroed()
    };
    RegisterClassW(&wc);
    let color = 0x00e0e0e0u32;
    let background = if DwmSetWindowAttribute(
        owner,
        DWMWA_CAPTION_COLOR as u32,
        &color as *const _ as _,
        4,
    ) >= 0
    {
        color
    } else {
        0x00ffffff
    };
    let pin = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        class.as_ptr(),
        wide("窗口置顶：未开启").as_ptr(),
        WS_POPUP,
        0,
        0,
        0,
        0,
        owner,
        null_mut(),
        instance,
        null(),
    );
    if pin.is_null() {
        return;
    }
    SetWindowLongPtrW(
        pin,
        GWLP_USERDATA,
        Box::into_raw(Box::new(Pin {
            owner,
            hover: false,
            background,
        })) as isize,
    );
    SetPropW(owner, wide(PROPERTY).as_ptr(), pin);
    position(owner, pin);
}
/// # Safety
/// Owner must be the live window whose message is being dispatched.
pub unsafe fn owner_event(owner: HWND, msg: u32) {
    let pin = button(owner);
    if pin.is_null() {
        return;
    }
    if msg == WM_DESTROY {
        RemovePropW(owner, wide(PROPERTY).as_ptr());
        DestroyWindow(pin);
        return;
    }
    if matches!(
        msg,
        WM_WINDOWPOSCHANGED | WM_SIZE | WM_SHOWWINDOW | WM_ACTIVATE | WM_ENABLE | WM_DPICHANGED
    ) {
        position(owner, pin);
    }
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Pin;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    match msg {
        WM_MOUSEACTIVATE => return MA_NOACTIVATE as isize,
        WM_MOUSEMOVE => {
            if !(*ptr).hover {
                (*ptr).hover = true;
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut track);
                InvalidateRect(hwnd, null(), 0);
            }
        }
        WM_MOUSELEAVE => {
            (*ptr).hover = false;
            InvalidateRect(hwnd, null(), 0);
        }
        WM_LBUTTONDOWN => {
            SetCapture(hwnd);
        }
        WM_LBUTTONUP => {
            let captured = GetCapture() == hwnd;
            ReleaseCapture();
            let mut r: RECT = zeroed();
            GetClientRect(hwnd, &mut r);
            let x = lp as i16 as i32;
            let y = (lp >> 16) as i16 as i32;
            if captured
                && x >= 0
                && y >= 0
                && x < r.right
                && y < r.bottom
                && IsWindowEnabled((*ptr).owner) != 0
            {
                let pinned = GetWindowLongW((*ptr).owner, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST == 0;
                let result = crate::updates::windows::set_topmost((*ptr).owner, pinned);
                if result.is_ok() {
                    SetWindowTextW(
                        hwnd,
                        wide(if pinned {
                            "窗口置顶：已开启"
                        } else {
                            "窗口置顶：未开启"
                        })
                        .as_ptr(),
                    );
                }
                InvalidateRect(hwnd, null(), 0);
            }
        }
        WM_PAINT => {
            let mut ps = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut r: RECT = zeroed();
            GetClientRect(hwnd, &mut r);
            let pinned = GetWindowLongW((*ptr).owner, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST != 0;
            let brush = CreateSolidBrush(if (*ptr).hover {
                0x00cacaca
            } else {
                (*ptr).background
            });
            FillRect(dc, &r, brush);
            DeleteObject(brush);
            let scale = GetDpiForWindow((*ptr).owner) as f64 / 96.0;
            let cx = r.right / 2;
            let cy = r.bottom / 2;
            let pen = CreatePen(
                PS_SOLID,
                (scale.round() as i32).max(1),
                if pinned { 0x00bb6600 } else { 0x00606060 },
            );
            let old = SelectObject(dc, pen);
            let points = [
                (-2, -5),
                (2, -5),
                (1, -1),
                (3, 2),
                (-3, 2),
                (-1, -1),
                (-2, -5),
            ];
            for (i, (x, y)) in points.iter().enumerate() {
                let px = cx + (*x as f64 * scale).round() as i32;
                let py = cy + (*y as f64 * scale).round() as i32;
                if i == 0 {
                    MoveToEx(dc, px, py, null_mut());
                } else {
                    LineTo(dc, px, py);
                }
            }
            MoveToEx(dc, cx, cy + (2.0 * scale) as i32, null_mut());
            LineTo(dc, cx, cy + (6.0 * scale) as i32);
            if pinned {
                let fill = CreateSolidBrush(0x00bb6600);
                let body = RECT {
                    left: cx - (scale as i32).max(1),
                    top: cy - (3.0 * scale) as i32,
                    right: cx + (scale as i32).max(1) + 1,
                    bottom: cy + (2.0 * scale) as i32,
                };
                FillRect(dc, &body, fill);
                DeleteObject(fill);
            }
            SelectObject(dc, old);
            DeleteObject(pen);
            EndPaint(hwnd, &ps);
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(ptr));
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn caption_pin_click_position_and_minimize() {
        unsafe {
            let owner = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                wide("Caption test").as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                100,
                100,
                900,
                600,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            assert!(!owner.is_null());
            create(owner);
            let pin = button(owner);
            assert!(!pin.is_null());
            assert_eq!(GetWindow(pin, GW_OWNER), owner);
            let mut wr: RECT = zeroed();
            let mut pr: RECT = zeroed();
            GetWindowRect(owner, &mut wr);
            GetWindowRect(pin, &mut pr);
            assert!(
                pr.left > wr.left + 400
                    && pr.right < wr.right
                    && pr.top >= wr.top
                    && pr.bottom < wr.top + 100
            );
            let coord = (((pr.right - pr.left) / 2) | (((pr.bottom - pr.top) / 2) << 16)) as isize;
            SendMessageW(pin, WM_LBUTTONDOWN, 0, coord);
            assert_eq!(GetCapture(), pin, "capture absent");
            SendMessageW(pin, WM_LBUTTONUP, 0, coord);
            assert_ne!(GetWindowLongW(owner, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST, 0);
            SendMessageW(pin, WM_LBUTTONDOWN, 0, coord);
            SendMessageW(pin, WM_LBUTTONUP, 0, coord);
            assert_eq!(GetWindowLongW(owner, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST, 0);
            SetWindowPos(owner, null_mut(), 200, 150, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
            owner_event(owner, WM_WINDOWPOSCHANGED);
            let mut moved: RECT = zeroed();
            GetWindowRect(pin, &mut moved);
            assert_eq!(moved.left - pr.left, 100);
            assert_eq!(moved.top - pr.top, 50);
            ShowWindow(owner, SW_MAXIMIZE);
            owner_event(owner, WM_SIZE);
            GetWindowRect(owner, &mut wr);
            GetWindowRect(pin, &mut pr);
            assert!(pr.left >= wr.left && pr.right <= wr.right);
            ShowWindow(owner, SW_MINIMIZE);
            owner_event(owner, WM_SIZE);
            assert_eq!(IsWindowVisible(pin), 0);
            ShowWindow(owner, SW_RESTORE);
            owner_event(owner, WM_SIZE);
            assert_ne!(IsWindowVisible(pin), 0);
            owner_event(owner, WM_DESTROY);
            DestroyWindow(owner);
            assert_eq!(IsWindow(pin), 0);
        }
    }
}
