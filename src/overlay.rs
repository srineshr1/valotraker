use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreateSolidBrush,
    DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetMonitorInfoW, InvalidateRect,
    MonitorFromPoint, SelectObject, SetBkMode, SetTextColor, HDC, HFONT, HGDIOBJ, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, SRCCOPY, TRANSPARENT,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, ReleaseCapture, SetCapture, UnregisterHotKey, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetClientRect, GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowRect,
    LoadCursorW, LoadIconW, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW, SetCursor,
    SetForegroundWindow, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TrackPopupMenu, TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HICON,
    IDC_ARROW, IDC_SIZEALL, IDI_APPLICATION, LWA_ALPHA, MB_ICONINFORMATION, MB_OK, MF_STRING, MSG,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE,
    SW_SHOWNOACTIVATE, TPM_BOTTOMALIGN, TPM_RIGHTALIGN, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_HOTKEY, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    HWND_TOPMOST,
};

use crate::catalog::{rank_color, rgb};
use crate::log::log;
use crate::model::{data_dir, lk, now_ms, Phase, Row, Shared, View, MSG_VIEW};

const MSG_TRAY: u32 = 0x8001;
const HK_TOGGLE: i32 = 1;
const HK_MOVE: i32 = 2;
const HK_QUIT: i32 = 3;
const MENU_TOGGLE: usize = 10;
const MENU_MOVE: usize = 11;
const MENU_QUIT: usize = 12;

struct Brushes {
    border: HGDIOBJ,
    bg: HGDIOBJ,
    header: HGDIOBJ,
    you: HGDIOBJ,
    marker: HGDIOBJ,
    win: HGDIOBJ,
    loss: HGDIOBJ,
    neutral: HGDIOBJ,
    empty: HGDIOBJ,
}

struct Ui {
    shared: Arc<Shared>,
    view: View,
    dpi: u32,
    user_hidden: bool,
    move_mode: bool,
    move_since: Option<Instant>,
    dragging: bool,
    drag_dx: i32,
    drag_dy: i32,
    waiting_at: Option<Instant>,
    peek_until: Option<Instant>,
    shown: bool,
    placed: bool,
    width: i32,
    height: i32,
    last_second: Option<u64>,
    last_tip: String,
    font_header: HFONT,
    font_body: HFONT,
    font_small: HFONT,
    brushes: Brushes,
    icon: HICON,
}

pub fn set_dpi_aware() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn acquire_single_instance() -> bool {
    let name = wide("Local\\valotraker.singleton");
    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if handle.is_null() {
        return true;
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        info_box(
            "valotraker",
            "valotraker is already running.\r\nLook for its icon in the system tray.",
        );
        return false;
    }
    true
}

pub fn info_box(title: &str, body: &str) {
    let title = wide(title);
    let body = wide(body);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

pub fn poke(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        PostMessageW(hwnd as HWND, MSG_VIEW, 0, 0);
    }
}

pub fn run(shared: Arc<Shared>) {
    unsafe { run_window(shared) }
}

unsafe fn run_window(shared: Arc<Shared>) {
    let instance = GetModuleHandleW(std::ptr::null());
    let class_name = wide("ValotrakerOverlay");
    let mut klass: WNDCLASSW = std::mem::zeroed();
    klass.style = CS_HREDRAW | CS_VREDRAW;
    klass.lpfnWndProc = Some(wndproc);
    klass.hInstance = instance;
    klass.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
    klass.lpszClassName = class_name.as_ptr();
    if RegisterClassW(&klass) == 0 {
        log("could not register the overlay window");
        return;
    }

    let ui = Ui::new(shared);
    let state = Box::into_raw(Box::new(ui));
    let (x, y, w, h) = (*state).initial_bounds();
    let hwnd = CreateWindowExW(
        exstyle(false),
        class_name.as_ptr(),
        wide("valotraker").as_ptr(),
        WS_POPUP,
        x,
        y,
        w,
        h,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        instance,
        state as *mut _,
    );
    if hwnd.is_null() {
        log("could not create the overlay window");
        drop(Box::from_raw(state));
        return;
    }
    (*state).width = w;
    (*state).height = h;
    SetTimer(hwnd, 1, 200, None);
    let mut msg: MSG = std::mem::zeroed();
    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    drop(Box::from_raw(state));
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let create = &*(lparam as *const windows_sys::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
        let state = create.lpCreateParams as *mut Ui;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        if !state.is_null() {
            (&(*state).shared).hwnd.store(hwnd as isize, Ordering::SeqCst);
        }
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ui;
    if state.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    match (*state).handle(hwnd, msg, wparam, lparam) {
        Some(result) => result,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

impl Ui {
    fn new(shared: Arc<Shared>) -> Self {
        let view = lk(&shared.view).clone();
        let brushes = unsafe { make_brushes() };
        Self {
            shared,
            view,
            dpi: 96,
            user_hidden: false,
            move_mode: false,
            move_since: None,
            dragging: false,
            drag_dx: 0,
            drag_dy: 0,
            waiting_at: None,
            peek_until: None,
            shown: false,
            placed: false,
            width: 0,
            height: 0,
            last_second: None,
            last_tip: String::new(),
            font_header: std::ptr::null_mut(),
            font_body: std::ptr::null_mut(),
            font_small: std::ptr::null_mut(),
            brushes,
            icon: std::ptr::null_mut(),
        }
    }

    fn s(&self, px: i32) -> i32 {
        (px * self.dpi as i32) / 96
    }

    fn initial_bounds(&self) -> (i32, i32, i32, i32) {
        let (w, h) = desired_size(&self.view, 96);
        if let Some((x, y)) = load_config() {
            let (x, y) = clamp_pos(x, y, w, h);
            return (x, y, w, h);
        }
        let (x, y) = default_pos(w, h);
        (x, y, w, h)
    }

    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_CREATE => {
                self.dpi = unsafe {
                    let dpi = GetDpiForWindow(hwnd);
                    if dpi == 0 { 96 } else { dpi }
                };
                self.rebuild_fonts();
                self.icon = unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) };
                self.add_tray(hwnd);
                self.register_keys(hwnd);
                unsafe { self.sync_style(hwnd) };
                self.refresh(hwnd, true);
                Some(0)
            }
            MSG_VIEW | WM_TIMER => {
                self.on_tick(hwnd);
                Some(0)
            }
            WM_PAINT => {
                unsafe { self.paint(hwnd) };
                Some(0)
            }
            WM_ERASEBKGND => Some(1),
            WM_SETCURSOR => {
                if self.move_mode {
                    unsafe { SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_SIZEALL)) };
                    Some(1)
                } else {
                    None
                }
            }
            WM_LBUTTONDOWN => {
                if self.move_mode {
                    self.begin_drag(hwnd);
                }
                Some(0)
            }
            WM_MOUSEMOVE => {
                if self.dragging {
                    self.drag(hwnd);
                }
                Some(0)
            }
            WM_LBUTTONUP => {
                if self.dragging {
                    self.end_drag(hwnd);
                }
                Some(0)
            }
            WM_HOTKEY => {
                match wparam as i32 {
                    HK_TOGGLE => self.toggle(hwnd),
                    HK_MOVE => self.toggle_move(hwnd),
                    HK_QUIT => unsafe {
                        PostMessageW(hwnd, WM_CLOSE, 0, 0);
                    },
                    _ => {}
                }
                Some(0)
            }
            MSG_TRAY => {
                let event = lparam as u32;
                if event == WM_LBUTTONUP {
                    self.toggle(hwnd);
                } else if event == WM_RBUTTONUP {
                    unsafe { self.show_menu(hwnd) };
                }
                Some(0)
            }
            WM_COMMAND => {
                match wparam & 0xffff {
                    MENU_TOGGLE => self.toggle(hwnd),
                    MENU_MOVE => self.toggle_move(hwnd),
                    MENU_QUIT => unsafe {
                        PostMessageW(hwnd, WM_CLOSE, 0, 0);
                    },
                    _ => {}
                }
                Some(0)
            }
            WM_DPICHANGED => {
                let dpi = (wparam & 0xffff) as u32;
                if dpi > 0 {
                    self.dpi = dpi;
                    self.rebuild_fonts();
                    self.refresh(hwnd, true);
                }
                Some(0)
            }
            WM_CLOSE => {
                unsafe { DestroyWindow(hwnd) };
                Some(0)
            }
            WM_DESTROY => {
                self.remove_tray(hwnd);
                unsafe {
                    UnregisterHotKey(hwnd, HK_TOGGLE);
                    UnregisterHotKey(hwnd, HK_MOVE);
                    UnregisterHotKey(hwnd, HK_QUIT);
                    self.free_gdi();
                    PostQuitMessage(0);
                }
                Some(0)
            }
            _ => None,
        }
    }

    fn on_tick(&mut self, hwnd: HWND) {
        let mut mode_changed = false;
        if self.move_mode && !self.dragging {
            if self
                .move_since
                .map(|at| at.elapsed() > Duration::from_secs(20))
                .unwrap_or(false)
            {
                self.move_mode = false;
                unsafe { self.sync_style(hwnd) };
                mode_changed = true;
            }
        }
        let latest = lk(&self.shared.view).clone();
        let view_changed = latest != self.view;
        if view_changed {
            self.view = latest;
        }
        let show = self.should_show();
        let second = remaining(self.view.timer_ends_ms);
        let second_changed = self.shown && second != self.last_second;
        if view_changed || show != self.shown || mode_changed {
            self.refresh(hwnd, true);
        } else if second_changed {
            self.last_second = second;
            unsafe {
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
        }
        if self.shown && !self.dragging {
            unsafe {
                SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }

    fn should_show(&mut self) -> bool {
        if self.move_mode {
            return true;
        }
        if self.user_hidden {
            return false;
        }
        match self.view.phase {
            Phase::Waiting => {
                if self.waiting_at.is_none() {
                    self.waiting_at = Some(Instant::now());
                }
                self.waiting_at.unwrap().elapsed() < Duration::from_secs(12)
            }
            Phase::Idle | Phase::Menus => self
                .peek_until
                .map(|until| Instant::now() < until)
                .unwrap_or(false),
            Phase::Pregame | Phase::Ingame => true,
        }
    }

    fn toggle(&mut self, hwnd: HWND) {
        self.user_hidden = !self.user_hidden;
        if !self.user_hidden {
            self.waiting_at = Some(Instant::now());
            self.peek_until = Some(Instant::now() + Duration::from_secs(5));
        }
        self.refresh(hwnd, true);
    }

    fn toggle_move(&mut self, hwnd: HWND) {
        self.move_mode = !self.move_mode;
        if self.move_mode {
            self.user_hidden = false;
            self.move_since = Some(Instant::now());
            self.peek_until = Some(Instant::now() + Duration::from_secs(20));
        }
        unsafe { self.sync_style(hwnd) };
        self.refresh(hwnd, true);
    }

    fn refresh(&mut self, hwnd: HWND, repaint: bool) {
        let show = self.should_show();
        let (w, h) = desired_size(&self.view, self.dpi);
        unsafe {
            if show {
                SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    w,
                    h,
                    SWP_NOMOVE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
                if !self.placed {
                    self.placed = true;
                    if let Some((x, y)) = window_origin(hwnd) {
                        save_config(x, y);
                    }
                }
                if !self.shown {
                    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
                self.shown = true;
                self.width = w;
                self.height = h;
                if repaint {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            } else if self.shown {
                ShowWindow(hwnd, SW_HIDE);
                self.shown = false;
            }
        }
        self.last_second = remaining(self.view.timer_ends_ms);
        self.update_tip(hwnd);
    }

    unsafe fn sync_style(&self, hwnd: HWND) {
        let style = exstyle(self.move_mode) as isize;
        SetWindowLongPtrW(hwnd, -20, style); // GWL_EXSTYLE
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        let alpha = if self.move_mode { 255 } else { 228 };
        SetLayeredWindowAttributes(hwnd, 0, alpha, LWA_ALPHA);
    }

    fn begin_drag(&mut self, hwnd: HWND) {
        let mut cursor = POINT { x: 0, y: 0 };
        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        unsafe {
            GetCursorPos(&mut cursor);
            GetWindowRect(hwnd, &mut rect);
            SetCapture(hwnd);
        }
        self.dragging = true;
        self.drag_dx = cursor.x - rect.left;
        self.drag_dy = cursor.y - rect.top;
    }

    fn drag(&self, hwnd: HWND) {
        let mut cursor = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut cursor);
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                cursor.x - self.drag_dx,
                cursor.y - self.drag_dy,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    fn end_drag(&mut self, hwnd: HWND) {
        self.dragging = false;
        self.move_since = Some(Instant::now());
        unsafe { ReleaseCapture() };
        if let Some((x, y)) = window_origin(hwnd) {
            save_config(x, y);
        }
    }

    fn add_tray(&self, hwnd: HWND) {
        unsafe {
            let mut data: NOTIFYICONDATAW = std::mem::zeroed();
            data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            data.hWnd = hwnd;
            data.uID = 1;
            data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            data.uCallbackMessage = MSG_TRAY;
            data.hIcon = self.icon;
            write_tip(&mut data.szTip, "valotraker");
            Shell_NotifyIconW(NIM_ADD, &data);
        }
    }

    fn update_tip(&mut self, hwnd: HWND) {
        let tip = tray_text(self.view.phase).to_string();
        if tip == self.last_tip {
            return;
        }
        self.last_tip = tip.clone();
        unsafe {
            let mut data: NOTIFYICONDATAW = std::mem::zeroed();
            data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            data.hWnd = hwnd;
            data.uID = 1;
            data.uFlags = NIF_TIP;
            write_tip(&mut data.szTip, &tip);
            Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    fn remove_tray(&self, hwnd: HWND) {
        unsafe {
            let mut data: NOTIFYICONDATAW = std::mem::zeroed();
            data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            data.hWnd = hwnd;
            data.uID = 1;
            Shell_NotifyIconW(NIM_DELETE, &data);
        }
    }

    unsafe fn show_menu(&self, hwnd: HWND) {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        let show = wide("Show / hide");
        let mov = wide("Move");
        let quit = wide("Quit");
        AppendMenuW(menu, MF_STRING, MENU_TOGGLE, show.as_ptr());
        AppendMenuW(menu, MF_STRING, MENU_MOVE, mov.as_ptr());
        AppendMenuW(menu, MF_STRING, MENU_QUIT, quit.as_ptr());
        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        SetForegroundWindow(hwnd);
        TrackPopupMenu(
            menu,
            TPM_RIGHTALIGN | TPM_BOTTOMALIGN,
            cursor.x,
            cursor.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);
        PostMessageW(hwnd, 0, 0, 0);
    }

    fn register_keys(&self, hwnd: HWND) {
        unsafe {
            let mods = MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT;
            if RegisterHotKey(hwnd, HK_TOGGLE, mods, 0x77) == 0 {
                log("Ctrl+Shift+F8 is already in use");
            }
            if RegisterHotKey(hwnd, HK_MOVE, mods, 0x76) == 0 {
                log("Ctrl+Shift+F7 is already in use");
            }
            if RegisterHotKey(hwnd, HK_QUIT, mods, 0x51) == 0 {
                log("Ctrl+Shift+Q is already in use");
            }
        }
    }

    fn rebuild_fonts(&mut self) {
        unsafe {
            if !self.font_header.is_null() {
                DeleteObject(self.font_header as HGDIOBJ);
            }
            if !self.font_body.is_null() {
                DeleteObject(self.font_body as HGDIOBJ);
            }
            if !self.font_small.is_null() {
                DeleteObject(self.font_small as HGDIOBJ);
            }
            self.font_header = make_font(self.s(15), 600);
            self.font_body = make_font(self.s(13), 400);
            self.font_small = make_font(self.s(11), 400);
        }
    }

    unsafe fn free_gdi(&mut self) {
        if !self.font_header.is_null() {
            DeleteObject(self.font_header as HGDIOBJ);
            self.font_header = std::ptr::null_mut();
        }
        if !self.font_body.is_null() {
            DeleteObject(self.font_body as HGDIOBJ);
            self.font_body = std::ptr::null_mut();
        }
        if !self.font_small.is_null() {
            DeleteObject(self.font_small as HGDIOBJ);
            self.font_small = std::ptr::null_mut();
        }
        free_brush(self.brushes.border);
        free_brush(self.brushes.bg);
        free_brush(self.brushes.header);
        free_brush(self.brushes.you);
        free_brush(self.brushes.marker);
        free_brush(self.brushes.win);
        free_brush(self.brushes.loss);
        free_brush(self.brushes.neutral);
        free_brush(self.brushes.empty);
    }

    unsafe fn paint(&self, hwnd: HWND) {
        let mut paint: windows_sys::Win32::Graphics::Gdi::PAINTSTRUCT = std::mem::zeroed();
        let hdc = BeginPaint(hwnd, &mut paint);
        if hdc.is_null() {
            return;
        }
        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetClientRect(hwnd, &mut rect);
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, w.max(1), h.max(1));
        if mem.is_null() || bmp.is_null() {
            self.paint_into(hdc, w, h);
            if !bmp.is_null() {
                DeleteObject(bmp as HGDIOBJ);
            }
            if !mem.is_null() {
                DeleteDC(mem);
            }
            EndPaint(hwnd, &paint);
            return;
        }
        let old = SelectObject(mem, bmp as HGDIOBJ);
        self.paint_into(mem, w, h);
        BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old);
        DeleteObject(bmp as HGDIOBJ);
        DeleteDC(mem);
        EndPaint(hwnd, &paint);
    }

    unsafe fn paint_into(&self, hdc: HDC, w: i32, h: i32) {
        fill(hdc, 0, 0, w, h, self.brushes.border);
        fill(hdc, 1, 1, w - 2, h - 2, self.brushes.bg);
        SetBkMode(hdc, TRANSPARENT as i32);
        match self.view.phase {
            Phase::Pregame | Phase::Ingame => self.paint_match(hdc, w),
            _ => self.paint_status(hdc, w),
        }
    }

    unsafe fn paint_status(&self, hdc: HDC, w: i32) {
        let pad = self.s(12);
        let line = self.s(18);
        let mut y = self.s(10);
        self.text(hdc, "valotraker", pad, y, w - pad * 2, line, self.font_header, rgb(232, 228, 220), 0);
        y += line;
        let detail = if self.view.note.is_empty() {
            status_line(self.view.phase).to_string()
        } else {
            self.view.note.clone()
        };
        self.text(hdc, &detail, pad, y, w - pad * 2, line, self.font_body, rgb(196, 190, 178), 0);
        y += line + self.s(2);
        self.text(hdc, "Ctrl+Shift+F8   hides", pad, y, w - pad * 2, line, self.font_small, rgb(140, 136, 128), 0);
        y += self.s(16);
        self.text(hdc, "Ctrl+Shift+F7   moves", pad, y, w - pad * 2, line, self.font_small, rgb(140, 136, 128), 0);
    }

    unsafe fn paint_match(&self, hdc: HDC, w: i32) {
        let pad = self.s(10);
        let header_h = self.s(32);
        fill(hdc, 1, 1, w - 2, header_h, self.brushes.header);
        let title = if self.view.map.is_empty() { "Match" } else { self.view.map.as_str() };
        self.text(hdc, title, pad, self.s(7), self.s(150), self.s(20), self.font_header, rgb(236, 232, 224), 0);
        let meta = meta_text(&self.view, self.move_mode);
        self.text(hdc, &meta, w - pad - self.s(140), self.s(8), self.s(140), self.s(18), self.font_small, rgb(168, 164, 154), 2);
        let mut y = header_h + self.s(2);
        self.paint_legend(hdc, w, y);
        y += self.s(14);
        if !self.view.note.is_empty() {
            self.text(hdc, &self.view.note, pad, y, w - pad * 2, self.s(16), self.font_small, rgb(196, 140, 120), 0);
            y += self.s(16);
        }
        y += self.s(2);
        if self.view.single_list {
            self.paint_team(hdc, w, y, "PLAYERS", &self.view.allies, rgb(180, 176, 168));
        } else {
            y = self.paint_team(hdc, w, y, "YOUR TEAM", &self.view.allies, rgb(168, 196, 186));
            y += self.s(6);
            self.paint_team(hdc, w, y, "ENEMIES", &self.view.enemies, rgb(206, 164, 154));
        }
    }

    unsafe fn paint_team(&self, hdc: HDC, w: i32, mut y: i32, label: &str, rows: &[Row], color: COLORREF) -> i32 {
        let pad = self.s(10);
        self.text(hdc, label, pad, y, w - pad * 2, self.s(16), self.font_small, color, 0);
        y += self.s(16);
        if rows.is_empty() {
            self.text(hdc, "—", pad, y, w - pad * 2, self.s(22), self.font_body, rgb(120, 116, 108), 0);
            return y + self.s(24);
        }
        let extra = rows.len().saturating_sub(8);
        let shown = if extra == 0 { rows.len() } else { 7 };
        for row in rows.iter().take(shown) {
            self.paint_row(hdc, w, y, row);
            y += self.s(24);
        }
        if extra > 0 {
            let more = format!("+{} more", rows.len() - shown);
            self.text(hdc, &more, pad, y, w - pad * 2, self.s(22), self.font_small, rgb(140, 136, 128), 0);
            y += self.s(24);
        }
        y
    }

    unsafe fn paint_legend(&self, hdc: HDC, w: i32, y: i32) {
        let cols = self.columns(w);
        let h = self.s(14);
        let color = rgb(120, 116, 108);
        self.text(hdc, "K/D", cols.kd_x, y, cols.kd_w, h, self.font_small, color, 2);
        self.text(hdc, "HS%", cols.hs_x, y, cols.hs_w, h, self.font_small, color, 2);
        self.text(hdc, "RANK", cols.rank_x, y, cols.rank_w, h, self.font_small, color, 2);
        self.text(hdc, "RR", cols.rr_x, y, cols.rr_w, h, self.font_small, color, 2);
        self.text(hdc, "W/L", cols.form_x, y, cols.form_w, h, self.font_small, color, 2);
    }

    unsafe fn paint_row(&self, hdc: HDC, w: i32, y: i32, row: &Row) {
        let cols = self.columns(w);
        let row_h = self.s(24);
        if row.you {
            fill(hdc, 1, y, w - 2, row_h, self.brushes.you);
            fill(hdc, 1, y + self.s(4), self.s(2), row_h - self.s(8), self.brushes.marker);
        }
        let text_color = if row.you { rgb(244, 240, 232) } else { rgb(224, 220, 212) };
        let muted = rgb(148, 144, 136);
        self.text(hdc, &row.agent, cols.pad, y, cols.agent_w, row_h, self.font_body, muted, 0x8000);
        self.text(hdc, &row.name, cols.name_x, y, cols.name_w, row_h, self.font_body, text_color, 0x8000);
        let kd_color = if row.kd == "…" || row.kd == "—" { muted } else { rgb(196, 192, 184) };
        self.text(hdc, &row.kd, cols.kd_x, y, cols.kd_w, row_h, self.font_body, kd_color, 2);
        let hs_color = if row.hs == "…" || row.hs == "—" { muted } else { rgb(196, 192, 184) };
        self.text(hdc, &row.hs, cols.hs_x, y, cols.hs_w, row_h, self.font_body, hs_color, 2);
        let rank_col = if row.rank == "…" || row.rank == "UR" || row.rank == "PLC" {
            muted
        } else {
            rank_color(row.tier)
        };
        self.text(hdc, &row.rank, cols.rank_x, y, cols.rank_w, row_h, self.font_body, rank_col, 2);
        self.text(hdc, &row.rr, cols.rr_x, y, cols.rr_w, row_h, self.font_body, muted, 2);
        self.paint_pips(hdc, cols.form_x, y, row_h, cols.form_w, &row.form);
    }

    fn columns(&self, w: i32) -> Columns {
        let pad = self.s(10);
        let form_w = self.s(34);
        let rr_w = self.s(30);
        let rank_w = self.s(40);
        let hs_w = self.s(36);
        let kd_w = self.s(36);
        let right = w - pad;
        let form_x = right - form_w;
        let rr_x = form_x - rr_w;
        let rank_x = rr_x - rank_w;
        let hs_x = rank_x - hs_w;
        let kd_x = hs_x - kd_w;
        let name_x = pad + self.s(76);
        let name_w = (kd_x - name_x - self.s(4)).max(self.s(36));
        Columns {
            pad,
            agent_w: self.s(72),
            name_x,
            name_w,
            kd_x,
            kd_w,
            hs_x,
            hs_w,
            rank_x,
            rank_w,
            rr_x,
            rr_w,
            form_x,
            form_w,
        }
    }

    unsafe fn paint_pips(&self, hdc: HDC, x: i32, y: i32, row_h: i32, width: i32, form: &[i8; 5]) {
        let pip = self.s(4).max(3);
        let gap = self.s(2).max(1);
        let total = pip * 5 + gap * 4;
        let mut px = x + (width - total).max(0) / 2;
        let py = y + (row_h - pip) / 2;
        for value in form {
            let brush = match value {
                1 => self.brushes.win,
                -1 => self.brushes.loss,
                2 => self.brushes.neutral,
                _ => self.brushes.empty,
            };
            fill(hdc, px, py, pip, pip, brush);
            px += pip + gap;
        }
    }

    unsafe fn text(&self, hdc: HDC, value: &str, x: i32, y: i32, w: i32, h: i32, font: HFONT, color: COLORREF, align: u32) {
        if font.is_null() || w <= 0 || h <= 0 {
            return;
        }
        SetTextColor(hdc, color);
        SelectObject(hdc, font as HGDIOBJ);
        let mut rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
        let flags = 0x20 | 0x4 | 0x800 | align; // SINGLELINE | VCENTER | NOPREFIX | align
        let wide = wide(value);
        DrawTextW(hdc, wide.as_ptr(), -1, &mut rect, flags);
    }
}

fn desired_size(view: &View, dpi: u32) -> (i32, i32) {
    let s = |px: i32| (px * dpi as i32) / 96;
    match view.phase {
        Phase::Pregame | Phase::Ingame => {
            let ally_rows = row_slots(view.allies.len()) as i32;
            let enemy_rows = if view.single_list { 0 } else { row_slots(view.enemies.len()) as i32 };
            let sections = if view.single_list { 1 } else { 2 };
            let note = if view.note.is_empty() { 0 } else { s(16) };
            let gap = if view.single_list { 0 } else { s(6) };
            let h = s(4) + s(32) + s(14) + note + s(2) + sections * s(16) + (ally_rows + enemy_rows) * s(24) + gap + s(10);
            (s(400), h.max(s(80)))
        }
        _ => (s(280), s(92)),
    }
}

fn row_slots(len: usize) -> usize {
    if len == 0 {
        1
    } else {
        len.min(8)
    }
}

fn remaining(ends: Option<u64>) -> Option<u64> {
    let ends = ends?;
    Some(ends.saturating_sub(now_ms()) / 1000)
}

fn fmt_timer(sec: u64) -> String {
    format!("{}:{:02}", sec / 60, sec % 60)
}

fn meta_text(view: &View, move_mode: bool) -> String {
    if move_mode {
        return "DRAG".into();
    }
    let queue = view.queue.clone();
    match view.phase {
        Phase::Pregame => match remaining(view.timer_ends_ms) {
            Some(sec) if queue.is_empty() => fmt_timer(sec),
            Some(sec) => format!("{queue}  {}", fmt_timer(sec)),
            None => queue,
        },
        Phase::Ingame if queue.is_empty() => "LIVE".into(),
        Phase::Ingame => format!("{queue}  LIVE"),
        _ => queue,
    }
}

fn status_line(phase: Phase) -> &'static str {
    match phase {
        Phase::Waiting => "Waiting for Valorant",
        Phase::Idle => "Valorant isn't running",
        Phase::Menus => "In menus",
        Phase::Pregame => "Agent select",
        Phase::Ingame => "In a match",
    }
}

fn tray_text(phase: Phase) -> &'static str {
    match phase {
        Phase::Waiting => "valotraker — waiting for Valorant",
        Phase::Idle => "valotraker — Valorant isn't running",
        Phase::Menus => "valotraker — in menus",
        Phase::Pregame => "valotraker — agent select",
        Phase::Ingame => "valotraker — in a match",
    }
}

fn exstyle(move_mode: bool) -> u32 {
    let mut style = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;
    if !move_mode {
        style |= WS_EX_TRANSPARENT;
    }
    style
}

unsafe fn make_brushes() -> Brushes {
    Brushes {
        border: CreateSolidBrush(rgb(42, 44, 50)) as HGDIOBJ,
        bg: CreateSolidBrush(rgb(14, 15, 18)) as HGDIOBJ,
        header: CreateSolidBrush(rgb(24, 26, 30)) as HGDIOBJ,
        you: CreateSolidBrush(rgb(28, 36, 34)) as HGDIOBJ,
        marker: CreateSolidBrush(rgb(220, 214, 200)) as HGDIOBJ,
        win: CreateSolidBrush(rgb(118, 186, 142)) as HGDIOBJ,
        loss: CreateSolidBrush(rgb(196, 104, 96)) as HGDIOBJ,
        neutral: CreateSolidBrush(rgb(140, 136, 120)) as HGDIOBJ,
        empty: CreateSolidBrush(rgb(46, 48, 54)) as HGDIOBJ,
    }
}

unsafe fn free_brush(brush: HGDIOBJ) {
    if !brush.is_null() {
        DeleteObject(brush);
    }
}

unsafe fn fill(hdc: HDC, x: i32, y: i32, w: i32, h: i32, brush: HGDIOBJ) {
    if w <= 0 || h <= 0 || brush.is_null() {
        return;
    }
    let rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
    FillRect(hdc, &rect, brush as _);
}

unsafe fn make_font(height: i32, weight: i32) -> HFONT {
    let face = wide("Segoe UI");
    CreateFontW(if height == 0 { -13 } else { -height.abs() }, 0, 0, 0, weight, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr())
}

struct Columns {
    pad: i32,
    agent_w: i32,
    name_x: i32,
    name_w: i32,
    kd_x: i32,
    kd_w: i32,
    hs_x: i32,
    hs_w: i32,
    rank_x: i32,
    rank_w: i32,
    rr_x: i32,
    rr_w: i32,
    form_x: i32,
    form_w: i32,
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn write_tip(dest: &mut [u16], text: &str) {
    dest.fill(0);
    for (index, unit) in text.encode_utf16().take(dest.len().saturating_sub(1)).enumerate() {
        dest[index] = unit;
    }
}

fn default_pos(w: i32, h: i32) -> (i32, i32) {
    unsafe {
        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            let work = info.rcWork;
            let x = work.right - w - 20;
            let y = work.top + 80;
            return (x.max(work.left), y.min((work.bottom - h).max(work.top)));
        }
    }
    (80, 80)
}

fn clamp_pos(x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
    unsafe {
        let point = POINT { x: x + w / 2, y: y + 8 };
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            let work = info.rcWork;
            let max_x = (work.right - w).max(work.left);
            let max_y = (work.bottom - h).max(work.top);
            return (x.clamp(work.left, max_x), y.clamp(work.top, max_y));
        }
    }
    (x, y)
}

fn window_origin(hwnd: HWND) -> Option<(i32, i32)> {
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
        None
    } else {
        Some((rect.left, rect.top))
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Config {
    x: i32,
    y: i32,
}

fn load_config() -> Option<(i32, i32)> {
    let text = std::fs::read_to_string(data_dir().join("config.json")).ok()?;
    let config: Config = serde_json::from_str(&text).ok()?;
    Some((config.x, config.y))
}

fn save_config(x: i32, y: i32) {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(text) = serde_json::to_string(&Config { x, y }) {
        let _ = std::fs::write(dir.join("config.json"), text);
    }
}
