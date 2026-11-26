//! A click-through clock between the desktop icons and the wallpaper.

use anyhow::{Context, Result};
use muelle_config::Language;
use muelle_shell::system::{Monitor, long_date_text};
use windows::{
    UI::Composition::{
        CompositionBrush, ContainerVisual, Desktop::DesktopWindowTarget, SpriteVisual,
    },
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SYSTEMTIME, WPARAM},
        Graphics::Gdi::ScreenToClient,
        System::{
            LibraryLoader::GetModuleHandleW,
            RemoteDesktop::{
                WTS_CURRENT_SESSION, WTS_SESSIONSTATE_UNLOCK, WTSActive, WTSFreeMemory, WTSINFOEXW,
                WTSQuerySessionInformationW, WTSSessionInfoEx,
            },
            SystemInformation::GetLocalTime,
            WinRT::Composition::ICompositorDesktopInterop,
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowExW, FindWindowW,
            GWLP_USERDATA, GetParent, GetWindowLongPtrW, HTTRANSPARENT, IsWindow, KillTimer,
            MA_NOACTIVATE, PostMessageW, RegisterClassW, SMTO_ABORTIFHUNG, SW_HIDE,
            SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SendMessageTimeoutW, SetTimer, SetWindowLongPtrW,
            SetWindowPos, ShowWindow, WM_APP, WM_DPICHANGED_AFTERPARENT, WM_MOUSEACTIVATE,
            WM_NCHITTEST, WNDCLASSW, WS_CHILD, WS_DISABLED, WS_EX_NOACTIVATE,
            WS_EX_NOREDIRECTIONBITMAP, WS_EX_TRANSPARENT,
        },
    },
    core::{Interface, PWSTR, w},
};
use windows_numerics::{Vector2, Vector3};

use crate::{
    gfx::{
        Gfx, LabelBrush,
        clock::{ClockLine, ClockSpan},
    },
    strings,
    weather::{Cache, Reading},
};

pub const TIMER: usize = 19;
pub const DPI_CHANGED: u32 = WM_APP + 14;
const CLASS: windows::core::PCWSTR = w!("MuelleDesktopClock");
const WINDOW_TOP: f32 = 80.0;
const WIDGET_TOP: f32 = 132.0;

pub struct Clock {
    notify: HWND,
    window: Option<ClockWindow>,
    unlocked: bool,
    error_logged: bool,
    trace: bool,
}

impl Clock {
    pub fn new(notify: HWND) -> Self {
        let clock = Self {
            notify,
            window: None,
            unlocked: session_unlocked(),
            error_logged: false,
            trace: std::env::var_os("MUELLE_TRACE_CLOCK").is_some(),
        };
        clock.rearm();
        clock
    }

    pub fn rearm(&self) {
        // SAFETY: This is the dock's visual window on its own thread.
        unsafe { SetTimer(Some(self.notify), TIMER, next_minute(GetLocalTime()), None) };
    }

    pub fn set_unlocked(&mut self, unlocked: bool) {
        self.unlocked = unlocked;
    }

    pub fn reset_explorer(&mut self) {
        self.window = None;
        self.error_logged = false;
    }

    pub fn time_changed(&mut self) {
        self.rearm();
        if let Some(window) = &mut self.window {
            window.date_key = None;
            window.time_key = None;
        }
    }

    pub fn sync(
        &mut self,
        gfx: &Gfx,
        monitor: Monitor,
        covered: bool,
        weather: &mut Cache,
        tick: bool,
    ) {
        if tick {
            self.rearm();
            // If the WTS notification was missed, never ask for a forecast on
            // the lock screen, and recover as soon as the session is back.
            self.unlocked = session_unlocked();
        }
        match self.update(gfx, monitor, self.unlocked && !covered, weather, tick) {
            Ok(()) => self.error_logged = false,
            Err(error) => {
                crate::recovery::graphics_error(&error);
                if !std::mem::replace(&mut self.error_logged, true) {
                    muelle_shell::log::line(&format!("reloj del escritorio: {error:#}"));
                }
            }
        }
    }

    fn update(
        &mut self,
        gfx: &Gfx,
        monitor: Monitor,
        visible: bool,
        weather: &mut Cache,
        tick: bool,
    ) -> Result<()> {
        if !visible {
            if let Some(window) = &mut self.window {
                window.show(false);
            }
            return Ok(());
        }
        if self
            .window
            .as_ref()
            .is_none_or(|window| !window.is_attached())
        {
            self.window = Some(ClockWindow::create(gfx, self.notify)?);
        }
        if let Some(window) = &mut self.window {
            let area = viewport(monitor);
            window.place(area)?;
            if tick {
                weather.reveal();
            }
            // SAFETY: Read-only clock query.
            let now = unsafe { GetLocalTime() };
            if window.paint(gfx, now, strings::language(), weather.forecast())? && self.trace {
                muelle_shell::log::line("reloj del escritorio: hora redibujada");
            }
            window.show(true);
        }
        Ok(())
    }
}

impl Drop for Clock {
    fn drop(&mut self) {
        // SAFETY: The timer belongs to the dock on this thread.
        let _ = unsafe { KillTimer(Some(self.notify), TIMER) };
    }
}

fn next_minute(now: SYSTEMTIME) -> u32 {
    let elapsed = u32::from(now.wSecond) * 1000 + u32::from(now.wMilliseconds);
    60_000 - elapsed.min(59_999) + 50
}

fn session_unlocked() -> bool {
    let mut buffer = PWSTR::null();
    let mut bytes = 0;
    // SAFETY: WTS supplies and owns the buffer until WTSFreeMemory. A
    // successful level 1 result of the expected size has the matching layout.
    unsafe {
        if WTSQuerySessionInformationW(
            None,
            WTS_CURRENT_SESSION,
            WTSSessionInfoEx,
            &mut buffer,
            &mut bytes,
        )
        .is_err()
            || buffer.is_null()
        {
            return false;
        }
        let unlocked = if bytes as usize >= size_of::<WTSINFOEXW>() {
            let data = &*buffer.0.cast::<WTSINFOEXW>();
            data.Level == 1
                && data.Data.WTSInfoExLevel1.SessionState == WTSActive
                && data.Data.WTSInfoExLevel1.SessionFlags == WTS_SESSIONSTATE_UNLOCK as i32
        } else {
            false
        };
        WTSFreeMemory(buffer.0.cast());
        unlocked
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Viewport {
    rect: RECT,
    scale: f32,
}

fn viewport(monitor: Monitor) -> Viewport {
    let RECT {
        left, top, right, ..
    } = monitor.screen;
    let screen_width = (right - left).max(1);
    // At very large DPI on a narrow display, keep the full time legible.
    let scale = monitor.scale.min(screen_width as f32 / 640.0);
    let width = ((820.0 * scale).round() as i32).min(screen_width);
    let left = left + (screen_width - width) / 2;
    let top = top + (WINDOW_TOP * scale).round() as i32;
    Viewport {
        rect: RECT {
            left,
            top,
            right: left + width,
            bottom: top + (338.0 * scale).ceil() as i32,
        },
        scale,
    }
}

struct Window(HWND);

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: This process owns the child. Explorer may already have destroyed it.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

struct WeatherKey {
    reading: Reading,
    city: Option<String>,
    language: Language,
}

struct ClockWindow {
    hwnd: Window,
    progman: HWND,
    icons: HWND,
    _target: DesktopWindowTarget,
    date: SpriteVisual,
    time: SpriteVisual,
    forecast: SpriteVisual,
    viewport: Option<Viewport>,
    date_key: Option<(u16, u16, u16, Language)>,
    time_key: Option<(u16, u16)>,
    weather_key: Option<WeatherKey>,
    visible: bool,
}

impl ClockWindow {
    fn create(gfx: &Gfx, notify: HWND) -> Result<Self> {
        // SAFETY: Create a window owned by this thread. Progman owns Z-order
        // and the wallpaper. We never modify the desktop icons window.
        let (hwnd, progman, icons, target) = unsafe {
            let progman = FindWindowW(w!("Progman"), None).context("Progman no está disponible")?;
            if FindWindowExW(Some(progman), None, w!("WorkerW"), None).is_err() {
                let _ = SendMessageTimeoutW(
                    progman,
                    0x052c,
                    WPARAM(0xd),
                    LPARAM(1),
                    SMTO_ABORTIFHUNG,
                    250,
                    None,
                );
            }
            let icons = FindWindowExW(Some(progman), None, w!("SHELLDLL_DefView"), None)
                .context("SHELLDLL_DefView no es hija de Progman")?;
            let instance = GetModuleHandleW(None)?;
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: CLASS,
                ..Default::default()
            });
            let hwnd = Window(CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                CLASS,
                w!("Muelle"),
                WS_CHILD | WS_DISABLED,
                0,
                0,
                0,
                0,
                Some(progman),
                None,
                Some(instance.into()),
                None,
            )?);
            SetWindowLongPtrW(hwnd.0, GWLP_USERDATA, notify.0 as isize);
            let target = gfx
                .compositor
                .cast::<ICompositorDesktopInterop>()?
                .CreateDesktopWindowTarget(hwnd.0, true)?;
            (hwnd, progman, icons, target)
        };

        let root: ContainerVisual = gfx.compositor.CreateContainerVisual()?;
        let date = gfx.compositor.CreateSpriteVisual()?;
        let time = gfx.compositor.CreateSpriteVisual()?;
        let forecast = gfx.compositor.CreateSpriteVisual()?;
        let layers = root.Children()?;
        layers.InsertAtTop(&time)?;
        layers.InsertAtTop(&date)?;
        layers.InsertAtTop(&forecast)?;
        target.SetRoot(&root)?;
        Ok(Self {
            hwnd,
            progman,
            icons,
            _target: target,
            date,
            time,
            forecast,
            viewport: None,
            date_key: None,
            time_key: None,
            weather_key: None,
            visible: false,
        })
    }

    fn is_attached(&self) -> bool {
        // SAFETY: Read-only queries on the shell windows and the live child.
        unsafe {
            IsWindow(Some(self.hwnd.0)).as_bool()
                && GetParent(self.hwnd.0).ok() == Some(self.progman)
                && GetParent(self.icons).ok() == Some(self.progman)
        }
    }

    fn place(&mut self, area: Viewport) -> Result<()> {
        if self.viewport == Some(area) {
            return Ok(());
        }
        let rect = area.rect;
        let mut point = POINT {
            x: rect.left,
            y: rect.top,
        };
        // SAFETY: This child is positioned in its Progman parent's coordinates.
        unsafe {
            ScreenToClient(self.progman, &mut point)
                .ok()
                .context("coordenadas de Progman")?;
            SetWindowPos(
                self.hwnd.0,
                Some(self.icons),
                point.x,
                point.y,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOACTIVATE,
            )?;
        }
        self.viewport = Some(area);
        self.date_key = None;
        self.time_key = None;
        self.weather_key = None;
        self.forecast.SetIsVisible(false)?;
        Ok(())
    }

    fn show(&mut self, show: bool) {
        if self.visible != show {
            // SAFETY: The child belongs to this UI thread and cannot take focus.
            let _ =
                unsafe { ShowWindow(self.hwnd.0, if show { SW_SHOWNOACTIVATE } else { SW_HIDE }) };
            self.visible = show;
        }
    }

    fn paint(
        &mut self,
        gfx: &Gfx,
        now: SYSTEMTIME,
        language: Language,
        forecast: Option<(Reading, Option<&str>)>,
    ) -> Result<bool> {
        let Some(area) = self.viewport else {
            return Ok(false);
        };
        let scale = area.scale;
        let width = (area.rect.right - area.rect.left) as f32;
        let date_y = WIDGET_TOP - WINDOW_TOP;
        let time_y = date_y + ClockLine::Date.height();
        let weather_y = time_y + ClockLine::Time.height();
        let date_key = (now.wYear, now.wMonth, now.wDay, language);
        if self.date_key != Some(date_key) {
            let date = long_date_text(&now, language == Language::Es);
            let line = ClockLine::Date;
            let label = gfx.clock_line(line, &[(&date, ClockSpan::Normal)], scale, width)?;
            place_line(&self.date, &label, width, (date_y - line.margin()) * scale)?;
            self.date_key = Some(date_key);
        }
        let time_key = (now.wHour, now.wMinute);
        let time_changed = self.time_key != Some(time_key);
        if time_changed {
            let time = format!("{:02}:{:02}", now.wHour, now.wMinute);
            let line = ClockLine::Time;
            let label = gfx.clock_line(line, &[(&time, ClockSpan::Strong)], scale, width)?;
            place_line(&self.time, &label, width, (time_y - line.margin()) * scale)?;
            self.time_key = Some(time_key);
        }
        match (forecast, self.weather_key.as_ref()) {
            (None, None) => {}
            (None, Some(_)) => {
                self.forecast.SetIsVisible(false)?;
                self.forecast.SetBrush(None::<&CompositionBrush>)?;
                self.weather_key = None;
            }
            (Some((reading, city)), old) => {
                if old.is_none_or(|key| {
                    (key.reading, key.city.as_deref(), key.language) != (reading, city, language)
                }) {
                    let text = strings::weather(language);
                    let mut glyph = [0; 4];
                    let temperature = format!("{}°", reading.temperature);
                    let range = reading
                        .high_low
                        .map(|(high, low)| format!("{} {high}° / {} {low}°", text.high, text.low));
                    let mut spans = Vec::with_capacity(5);
                    if let Some(symbol) = reading.glyph() {
                        spans.push((&*symbol.encode_utf8(&mut glyph), ClockSpan::Symbol));
                    }
                    spans.push((&temperature, ClockSpan::Strong));
                    if let Some(condition) = reading.condition() {
                        spans.push((text.conditions[condition as usize], ClockSpan::Normal));
                    }
                    if let Some(range) = range.as_deref() {
                        spans.push((range, ClockSpan::Muted));
                    }
                    if let Some(city) = city {
                        spans.push((city, ClockSpan::Muted));
                    }
                    let line = ClockLine::Weather;
                    let label = gfx.clock_line(line, &spans, scale, width)?;
                    place_line(
                        &self.forecast,
                        &label,
                        width,
                        (weather_y - line.margin()) * scale,
                    )?;
                    self.forecast.SetIsVisible(true)?;
                    self.weather_key = Some(WeatherKey {
                        reading,
                        city: city.map(str::to_owned),
                        language,
                    });
                }
            }
        }
        Ok(time_changed)
    }
}

fn place_line(visual: &SpriteVisual, label: &LabelBrush, width: f32, y: f32) -> Result<()> {
    visual.SetBrush(&label.brush)?;
    visual.SetSize(Vector2 {
        X: label.width,
        Y: label.height,
    })?;
    visual.SetOffset(Vector3 {
        X: ((width - label.width) * 0.5).round(),
        Y: y.round(),
        Z: 0.0,
    })?;
    Ok(())
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DPICHANGED_AFTERPARENT => {
            // SAFETY: Only a handle value was stored at window creation.
            let notify = HWND(unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as _);
            if !notify.is_invalid() {
                let _ = unsafe { PostMessageW(Some(notify), DPI_CHANGED, WPARAM(0), LPARAM(0)) };
            }
            LRESULT(0)
        }
        // SAFETY: Unhandled messages are forwarded exactly as delivered by Win32.
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_clock_timer_aligns_to_the_next_minute() {
        let now = SYSTEMTIME {
            wSecond: 59,
            wMilliseconds: 990,
            ..Default::default()
        };
        assert_eq!(next_minute(now), 60);
        assert_eq!(next_minute(SYSTEMTIME::default()), 60_050);
    }

    #[test]
    fn desktop_clock_centers_the_viewport_and_scales_from_the_primary_origin() {
        let monitor = Monitor {
            screen: RECT {
                left: -1920,
                top: 240,
                right: 0,
                bottom: 1320,
            },
            work_left: -1920,
            work_top: 240,
            work_right: 0,
            work_bottom: 1320,
            scale: 1.5,
        };
        let Viewport { rect, scale } = viewport(monitor);
        assert_eq!((rect.left + rect.right) / 2, -960);
        assert_eq!(rect.top, 360);
        assert_eq!(scale, 1.5);
    }

    #[test]
    #[ignore = "requires an unlocked Windows desktop with the dock stopped"]
    fn native_desktop_clock_attaches_and_paints_once_per_minute() -> Result<()> {
        use muelle_shell::system::primary_monitor;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };

        assert!(session_unlocked(), "the desktop must be unlocked");
        // SAFETY: The dispatcher and graphics state live on this test thread.
        let _dispatcher = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let monitor = primary_monitor().context("pantalla principal")?;
        let mut window = ClockWindow::create(&gfx, HWND::default())?;
        window.place(viewport(monitor))?;
        assert!(window.is_attached());
        let mut now = SYSTEMTIME {
            wYear: 2026,
            wMonth: 11,
            wDay: 25,
            wHour: 19,
            wMinute: 53,
            ..Default::default()
        };
        assert!(window.paint(&gfx, now, Language::Es, None)?);
        now.wSecond = 59;
        assert!(!window.paint(&gfx, now, Language::Es, None)?);
        now.wMinute = 54;
        assert!(window.paint(&gfx, now, Language::Es, None)?);
        Ok(())
    }
}
