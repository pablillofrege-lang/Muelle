//! Minimal macOS-style menu bar across the top of the screen.
//!
//! Hiding the Windows taskbar also hides its clock and status area; this bar
//! brings them back: the name of the app in front, the battery, a control
//! center button (opens the Windows quick settings: network, volume,
//! brightness) and the date and time (opens the notification center).
//!
//! Cost while idle: one timer that fires once a minute, on the minute, to
//! redraw the clock. Everything else is event-driven.
//!
//! With auto-hide on, the bar reserves no space and stays off screen. Resting
//! the pointer briefly at the top edge reveals it.
//!
//! Status items, right to left as on a Mac: clock, control center, search,
//! network, volume, battery, then Task View and Widgets. Search, Task View and
//! Widgets appear only if the user has them on the Windows taskbar. Network
//! and volume changes arrive through separate notifications; nothing polls.

use anyhow::Result;
use muelle_config::{IslandStyle, Language};
use muelle_shell::{
    accessories::{Kind as AccessoryKind, Snapshot as AccessorySnapshot},
    appbar::{AppBar, Edge},
    launcher,
    status::{self, AudioWatch, Network, NetworkWatch, Privacy, TaskbarButtons, Volume},
    system::{self, Battery},
    watch::RegistryWatch,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering},
    },
    time::{Duration, Instant},
};
use windows::{
    Foundation::{TimeSpan, TypedEventHandler},
    UI::{
        Color,
        Composition::{
            CompositionBatchTypes, CompositionBrush, CompositionRoundedRectangleGeometry,
            CompositionScopedBatch, CompositionSurfaceBrush, Compositor, ContainerVisual,
            CubicBezierEasingFunction, Desktop::DesktopWindowTarget, ScalarKeyFrameAnimation,
            ShapeVisual, SpriteVisual, VisualCollection,
        },
    },
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SYSTEMTIME, WPARAM},
        Graphics::{
            Dwm::{DWMWA_USE_HOSTBACKDROPBRUSH, DwmSetWindowAttribute},
            Gdi::{
                CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject, HRGN, RGN_ERROR,
                RGN_OR, SetWindowRgn,
            },
        },
        System::{
            LibraryLoader::GetModuleHandleW, SystemInformation::GetLocalTime,
            WinRT::Composition::ICompositorDesktopInterop,
        },
        UI::Input::KeyboardAndMouse::{
            GetCapture, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
        },
        UI::WindowsAndMessaging::{
            CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, HHOOK,
            HWND_TOPMOST, IDC_ARROW, KillTimer, LoadCursorW, MSLLHOOKSTRUCT,
            PBT_POWERSETTINGCHANGE, PostMessageW, RegisterClassW, SWP_HIDEWINDOW, SWP_NOACTIVATE,
            SWP_SHOWWINDOW, SetCursor, SetTimer, SetWindowPos, SetWindowsHookExW,
            UnhookWindowsHookEx, WH_MOUSE_LL, WM_APP, WM_CAPTURECHANGED, WM_LBUTTONDOWN,
            WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST,
            WM_RBUTTONDOWN, WM_SETCURSOR, WM_SETTINGCHANGE, WM_TIMECHANGE, WM_TIMER,
            WM_XBUTTONDOWN, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
            WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
    },
    core::{BOOL, Interface, h, w},
};
use windows_numerics::{Vector2, Vector3};

use crate::{
    accessories, app,
    appearance::{self, Material},
    bluetooth, edge_watch,
    gfx::{BARE_ISLAND_RADIUS, Gfx, Ink, LabelBrush},
    mascot,
    media::{self, Control as MediaControl, Track},
    scene::Theme,
    strings,
    usage::{
        self, ActiveProviders, Monitor as UsageMonitor, Provider, ProviderUsage, Snapshot, State,
    },
};

// Logical pixels.
const HEIGHT: f32 = 28.0;
const LEFT_MARGIN: f32 = 14.0;
const RIGHT_MARGIN: f32 = 14.0;
const BARE_RIGHT_MARGIN: f32 = 10.0;
const ITEM_GAP: f32 = 16.0;
/// Extra clickable room around an item.
const HIT_SLOP: f32 = 7.0;

const MEDIA_CHANGED: u32 = WM_APP + 28;
const VOLUME_CAPTURE_LOST: u32 = WM_APP + 31;
const AUDIO_CHANGED: u32 = WM_APP + 32;
const AUDIO_TICK: u32 = WM_APP + 33;
const BLUETOOTH_CHANGED: u32 = WM_APP + 34;
const VOLUME_TIMER: usize = 4;
const PROGRESS_TIMER: usize = 5;
/// The system monitor's samples (issue #64): every 2 s while it shows, and
/// a first one soon after it appears.
const MONITOR_TIMER: usize = 6;
const MONITOR_EVERY_MS: u32 = 2_000;
const MONITOR_FIRST_MS: u32 = 500;
/// Its two lines per column, smaller than the bar's text so both fit.
const MONITOR_FONT: f32 = 10.0;
/// How far each line reaches past the middle of the bar, in logical pixels.
const MONITOR_TUCK: f32 = 1.0;
const MONITOR_COLUMN_GAP: f32 = 8.0;
/// The network lost this long is worth a word from the mascot; shorter
/// drops (Wi-Fi roaming, a router restarting) say nothing.
const NETWORK_TIMER: usize = 7;
const OFFLINE_GRACE: Duration = Duration::from_secs(8);
const ACCESSORIES_CHANGED: u32 = WM_APP + 30;

const CLOCK_TIMER: usize = 1;
const POINTER_TIMER: usize = 2;
const POINTER_CHECK_MS: u32 = 100;
const ZONED_POINTER_CHECK_MS: u32 = 32;
const HIDE_ANIMATION_TIMER: usize = 3;
const REVEAL_MS: u32 = 180;
const HIDE_MS: u32 = 130;
// Recovery only; normal completion comes from the compositor, not this timer.
const HIDE_FALLBACK_MS: u32 = HIDE_MS + 250;
const HIDE_ANIMATION_DONE: u32 = WM_APP + 29;
const ISLAND_SINGLE_WIDTH: f32 = 145.0;
const ISLAND_EXPANDED_WIDTH: f32 = 440.0;
const ISLAND_ROW_HEIGHT: f32 = 118.0;
const BUBBLE_VERTICAL_INSET: f32 = 2.5;
const BUBBLE_MUSIC_WIDTH: f32 = 164.0;
const BUBBLE_PROVIDER_WIDTH: f32 = 62.0;
const BUBBLE_USAGE_PADDING: f32 = 16.0;
const PANEL_GAP: f32 = 5.0;
const PANEL_RADIUS: f32 = 18.0;
const PANEL_SHADOW_MARGIN: f32 = 20.0;
const WM_POWERBROADCAST: u32 = 0x0218;
const WM_MOUSELEAVE: u32 = 0x02A3;
/// AppBar notifications need a message id; the bar ignores them.
const APPBAR_CALLBACK: u32 = WM_APP + 20;
/// Posted by the network watch; audio uses its own coalesced notification.
const STATUS_CHANGED: u32 = WM_APP + 21;
/// Posted by the bar to itself to update from the message loop.
const TICK: u32 = WM_APP + 22;
const USAGE_CHANGED: u32 = WM_APP + 23;
const DISMISS_ISLAND: u32 = WM_APP + 24;
/// Privacy activity or the user's taskbar preferences changed.
const REGISTRY_CHANGED: u32 = WM_APP + 25;
/// `wParam` of WM_SETTINGCHANGE when the work area changes, which every
/// AppBar reservation (the dock's, this bar's) causes.
const SPI_SETWORKAREA: usize = 0x002F;
/// A `TICK` is already posted.
static TICK_PENDING: AtomicBool = AtomicBool::new(false);
static AUDIO_TICK_PENDING: AtomicBool = AtomicBool::new(false);
static TRIGGERS_LEFT: AtomicI32 = AtomicI32::new(0);
static TRIGGERS_BOTTOM: AtomicI32 = AtomicI32::new(0);
static TRIGGERS_RIGHT: AtomicI32 = AtomicI32::new(0);
static PANEL_LEFT: AtomicI32 = AtomicI32::new(0);
static PANEL_RIGHT: AtomicI32 = AtomicI32::new(0);
static BAR_TOP: AtomicI32 = AtomicI32::new(0);
static BAR_BOTTOM: AtomicI32 = AtomicI32::new(0);
static PANEL_BOTTOM: AtomicI32 = AtomicI32::new(0);
/// The hook exists only while the island is expanded.
static ISLAND_HOOK_WINDOW: AtomicIsize = AtomicIsize::new(0);
/// Segoe Fluent Icons code points.
pub(crate) mod glyph {
    pub const ETHERNET: char = '\u{E839}';
    pub const OFFLINE: char = '\u{F384}';
    pub const VOLUME: [char; 4] = ['\u{E992}', '\u{E993}', '\u{E994}', '\u{E995}'];
    pub const MUTE: char = '\u{E74F}';
    pub const SEARCH: char = '\u{E721}';
    pub const TASK_VIEW: char = '\u{E7C4}';
    pub const WIDGETS: char = '\u{F246}';
    /// The taskbar's "show hidden icons" arrow.
    pub const HIDDEN_ICONS: char = '\u{E70E}';
    pub const MICROPHONE: char = '\u{E720}';
    pub const CAMERA: char = '\u{E722}';
    pub const MOUSE: char = '\u{E962}';
    pub const HEADSET: char = '\u{E95B}';
    pub const BLUETOOTH: char = '\u{E702}';
    /// The bell of the Windows 11 notification center ("Ringer").
    pub const NOTIFICATIONS: char = '\u{EA8F}';
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    AccessoryIndicator,
    QuickSettings,
    Notifications,
    /// The clock: the user's own target, or the notification center.
    Clock,
    /// The battery: power settings.
    Power,
    /// The Wi-Fi, Ethernet or offline icon: the list of networks.
    Network,
    Sound,
    Search,
    TaskView,
    Widgets,
    HiddenIcons,
    InputLanguage,
    Microphone,
    Camera,
    Bluetooth,
    /// The system monitor: Task Manager, which lists what uses each.
    SystemMonitor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuBarWidgets {
    pub island_style: IslandStyle,
    /// The notification bell at the right end (`Config::menu_bar_notifications`).
    pub notifications: bool,
    /// CPU, memory and network speed (`Config::menu_bar_system_monitor`).
    pub system_monitor: bool,
    /// Only the essentials (`Config::menu_bar_essentials`): no network
    /// speed and none of the Windows shell buttons (see `essential`).
    pub essentials: bool,
    /// No background (`Config::menu_bar_bare`): a clear strip over the desktop,
    /// white text and icons with a dark halo, and an opaque black island.
    pub bare: bool,
}

/// The halo under text and icons without a background: (blur, Y offset,
/// alpha of the black), in logical pixels.
const HALO: [(f32, f32, u8); 2] = [(3.0, 1.0, 230), (12.0, 0.0, 115)];

const BARE_NOTCH_EXTRA_HEIGHT: f32 = 2.0;
const BARE_NOTCH_DIVIDER_HEIGHT: f32 = 14.0;
const BARE_MONITOR_FONT_SIZE: f32 = 12.5;
const BARE_PLAYER_WIDTH: f32 = 152.0;
const BARE_PLAYER_COVER_INSET: f32 = 14.0;
const BARE_PLAYER_TITLE_GAP: f32 = 6.0;
const BARE_PLAYER_TITLE_SIZE: f32 = 12.5;
const BARE_PLAYER_TRAILING_INSET: f32 = 8.0;
const BARE_PROVIDER_WIDTH: f32 = 80.0;
const BARE_PROVIDER_RING_MIN_WIDTH: f32 = 52.0;
const BARE_PROVIDER_LOGO_SIZE: f32 = 15.0;
const BARE_PROVIDER_LABEL_SIZE: f32 = 13.0;
const BARE_PROVIDER_ITEM_GAP: f32 = 6.0;
const BARE_PROVIDER_GROUP_GAP: f32 = 14.0;
const EMPTY_ISLAND_ENTRY_WIDTH: f32 = 320.0;

/// The island without a background: opaque, near black.
const BARE_ISLAND: Color = Color {
    A: 255,
    R: 4,
    G: 7,
    B: 6,
};

/// Bare mode has no full-width fill.
fn strip_fills(gfx: &Gfx, look: &Look) -> Result<Vec<CompositionBrush>> {
    if look.widgets.bare {
        return Ok(Vec::new());
    }
    Theme::for_bar(appearance::Look {
        material: look.material,
        light: look.light,
        glass: look.glass,
    })
    .fills(gfx)
}

/// Dark shadows shaped like the item (`mask`), under it, so white text and
/// icons stay legible on any wallpaper.
fn add_halo(
    c: &Compositor,
    layers: &VisualCollection,
    mask: &CompositionBrush,
    size: Vector2,
    offset: Vector3,
    look: &Look,
) -> Result<()> {
    let scale = look.scale;
    for (blur, lift, alpha) in HALO {
        let shadow = c.CreateDropShadow()?;
        shadow.SetMask(mask)?;
        shadow.SetBlurRadius(blur * scale)?;
        shadow.SetOffset(Vector3::new(0.0, (lift * scale).round(), 0.0))?;
        shadow.SetColor(rgba(0, 0, 0, alpha))?;
        let host = c.CreateSpriteVisual()?;
        host.SetSize(size)?;
        host.SetOffset(offset)?;
        host.SetShadow(&shadow)?;
        layers.InsertAtTop(&host)?;
    }
    Ok(())
}

/// What "Only the essentials" keeps: status and the clock, not the buttons
/// that open Windows' own shell surfaces.
fn essential(action: Action) -> bool {
    !matches!(
        action,
        Action::Notifications
            | Action::Search
            | Action::TaskView
            | Action::Widgets
            | Action::HiddenIcons
    )
}

impl Default for MenuBarWidgets {
    fn default() -> Self {
        Self {
            island_style: IslandStyle::Notch,
            notifications: false,
            system_monitor: false,
            essentials: false,
            bare: false,
        }
    }
}

/// Network, volume and taskbar buttons, read only when Windows reports a change.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Status {
    bluetooth_on: bool,
    network: Network,
    volume: Option<Volume>,
    buttons: TaskbarButtons,
    privacy: Privacy,
    battery_percentage: bool,
}

/// What the bar was last drawn for.
#[derive(Clone, PartialEq)]
struct Look {
    accessories: [Option<AccessoryReading>; 2],
    /// The system monitor's lines (see `MenuBar::monitor_lines`) while on.
    monitor: Option<[String; 4]>,
    language: Language,
    screen: RECT,
    scale: f32,
    /// The theme's material; `light` and `glass` are its other two halves
    /// (see `appearance::Look`).
    material: Material,
    light: bool,
    /// See `system::transparency_enabled`.
    glass: bool,
    zoned: bool,
    app_name: String,
    clock: String,
    /// The input language indicator, when more than one is installed.
    input: Option<String>,
    battery: Option<Battery>,
    status: Status,
    widgets: MenuBarWidgets,
    usage: Snapshot,
    usage_enabled: bool,
    active_providers: ActiveProviders,
    panel: Option<Island>,
    usage_refreshing: bool,
    track: Option<Track>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AccessoryReading {
    kind: AccessoryKind,
    percent: Option<u8>,
}

// Compare only visible readings: unrelated names/devices do not redraw the bar.
fn accessory_readings(snapshot: &AccessorySnapshot) -> [Option<AccessoryReading>; 2] {
    snapshot.compact().map(|device| {
        device.map(|device| AccessoryReading {
            kind: device.kind,
            percent: device.percent,
        })
    })
}

impl Look {
    fn island_height(&self, bar_height: f32) -> f32 {
        let extra = if self.zoned && self.widgets.island_style == IslandStyle::Notch {
            (BARE_NOTCH_EXTRA_HEIGHT * self.scale).round()
        } else {
            0.0
        };
        bar_height + extra
    }

    fn panel_content_height(&self) -> f32 {
        match self.panel {
            Some(Island::Music) => 180.0,
            Some(Island::Usage) => {
                74.0 + Provider::ALL
                    .into_iter()
                    .filter(|provider| self.active_providers.has(*provider))
                    .map(|provider| usage_row_height(self.usage.get(provider)))
                    .sum::<f32>()
            }
            None => 0.0,
        }
    }

    fn panel_height(&self) -> f32 {
        let natural = self.panel_content_height();
        if self.panel == Some(Island::Usage) {
            let available = (self.screen.bottom - self.screen.top) as f32 / self.scale
                - HEIGHT
                - PANEL_GAP
                - PANEL_SHADOW_MARGIN;
            natural.min(available.max(0.0))
        } else {
            natural
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Island {
    Music,
    Usage,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Span {
    left: f32,
    right: f32,
}

impl Span {
    fn width(self) -> f32 {
        self.right - self.left
    }

    fn contains(self, x: f32) -> bool {
        (self.left..self.right).contains(&x)
    }

    fn screen_pixels(self, origin: i32) -> std::ops::Range<i32> {
        origin + self.left.floor() as i32..origin + self.right.ceil() as i32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointerZone {
    Outside,
    Left,
    Center,
    Right,
}

fn strip_zone(bounds: RECT, entrance: Span, point: POINT) -> PointerZone {
    if !(bounds.left..bounds.right).contains(&point.x)
        || !(bounds.top..bounds.bottom).contains(&point.y)
    {
        PointerZone::Outside
    } else {
        let entrance = entrance.screen_pixels(bounds.left);
        if entrance.contains(&point.x) {
            PointerZone::Center
        } else if point.x < entrance.start {
            PointerZone::Left
        } else {
            PointerZone::Right
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RevealedSides {
    left: bool,
    right: bool,
}

fn revealed_at(sides: Option<RevealedSides>, zone: PointerZone) -> Option<RevealedSides> {
    match zone {
        PointerZone::Outside => None,
        PointerZone::Center => Some(sides.unwrap_or_default()),
        PointerZone::Left => sides.map(|sides| RevealedSides {
            left: true,
            ..sides
        }),
        PointerZone::Right => sides.map(|sides| RevealedSides {
            right: true,
            ..sides
        }),
    }
}

fn island_or_panel_span(
    x: f32,
    y: f32,
    bar_height: f32,
    island_height: f32,
    island: Span,
    panel: Span,
) -> Span {
    if y < bar_height || (y < island_height && x >= island.left && x < island.right) {
        island
    } else {
        panel
    }
}

/// Whether the bar is hidden, resting on the island, or actively revealed.
/// The zoned layout tracks its revealed sides separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Presence {
    Hidden,
    IslandOnly,
    Full,
}

fn presence(
    visible: bool,
    auto_hide: bool,
    revealed: bool,
    ghost: bool,
    covered: bool,
    has_island: bool,
    zoned: bool,
) -> Presence {
    if !visible {
        Presence::Hidden
    } else if !auto_hide || revealed {
        Presence::Full
    } else if ghost && has_island && (!covered || zoned) {
        // The island remains available over maximized windows in zoned mode.
        Presence::IslandOnly
    } else {
        Presence::Hidden
    }
}

/// Whether the pointer must be polled: while a popup is open, and while an
/// auto-hiding bar shows more than the island at rest. Only the island
/// resting arms nothing, so the ghost bar costs no CPU while idle.
fn needs_pointer_timer(
    on_screen: bool,
    presence: Presence,
    auto_hide: bool,
    panel_open: bool,
    hiding: bool,
    retry_clip: bool,
) -> bool {
    retry_clip
        || (on_screen
            && (panel_open || (auto_hide && (presence != Presence::IslandOnly || hiding))))
}

/// The window region that keeps only the island, in window pixels, so clicks
/// along the rest of the top edge reach the windows below.
fn ghost_region(triggers: Span, island_height: f32) -> Option<RECT> {
    (triggers.width() > 0.0).then(|| RECT {
        left: triggers.left.floor() as i32,
        top: 0,
        right: triggers.right.ceil() as i32,
        bottom: island_height.round() as i32,
    })
}

fn resting_island_region(
    presence: Presence,
    hiding: bool,
    island_hiding: bool,
    triggers: Span,
    island_height: f32,
) -> Option<WindowRegion> {
    if !hiding && (presence == Presence::IslandOnly || island_hiding) {
        ghost_region(triggers, island_height).map(WindowRegion::Island)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RegionPart {
    rect: RECT,
    radius: f32,
    square_top: bool,
}

impl RegionPart {
    fn rounded(span: Span, top: f32, height: f32, radius: f32) -> Option<Self> {
        let mut rect = ghost_region(span, height)?;
        rect.top = top.round() as i32;
        rect.bottom += rect.top;
        (rect.bottom > rect.top).then_some(Self {
            rect,
            radius: radius.min(span.width() / 2.0).min(height / 2.0),
            square_top: false,
        })
    }

    fn create_handle(self) -> HRGN {
        let RECT {
            left,
            top,
            right,
            bottom,
        } = self.rect;
        // Leave room for the compositor's antialiased edge around each shape.
        // A full shadow margin would claim transparent pixels from apps below.
        const EDGE_PADDING: i32 = 2;
        let diameter = (2.0 * self.radius).round() as i32;
        // A notch meets the screen edge with square top corners. Its upper
        // rounding sits above the window, leaving only the lower corners.
        let top = if self.square_top {
            top - diameter - EDGE_PADDING
        } else {
            top
        };
        // SAFETY: The arguments are window-relative pixel coordinates.
        unsafe {
            let diameter = diameter + 2 * EDGE_PADDING;
            // GDI drops the final row and column of a rounded region.
            CreateRoundRectRgn(
                left - EDGE_PADDING,
                top - EDGE_PADDING,
                right + EDGE_PADDING + 1,
                bottom + EDGE_PADDING + 1,
                diameter,
                diameter,
            )
        }
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq)]
struct CapsuleLayout {
    island: Option<RegionPart>,
    left: Option<RegionPart>,
    right: Option<RegionPart>,
    panel: Option<RegionPart>,
    volume: Option<RegionPart>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum WindowRegion {
    Island(RECT),
    Capsules([Option<RegionPart>; 5]),
}

impl CapsuleLayout {
    fn region(self, sides: RevealedSides) -> WindowRegion {
        WindowRegion::Capsules([
            self.island,
            self.left.filter(|_| sides.left),
            self.right.filter(|_| sides.right),
            self.panel,
            self.volume,
        ])
    }
}

impl WindowRegion {
    fn create_handle(self) -> HRGN {
        // SAFETY: CombineRgn permits reusing the destination as a source.
        // All handles belong to us until install_window_region transfers one.
        unsafe {
            match self {
                Self::Island(r) => CreateRectRgn(r.left, r.top, r.right, r.bottom),
                Self::Capsules(parts) => {
                    let region = CreateRectRgn(0, 0, 0, 0);
                    if region.is_invalid() {
                        return region;
                    }
                    for part in parts.into_iter().flatten() {
                        let handle = part.create_handle();
                        if !handle.is_invalid() {
                            let combined =
                                CombineRgn(Some(region), Some(region), Some(handle), RGN_OR);
                            let _ = DeleteObject(handle.into());
                            if combined != RGN_ERROR {
                                continue;
                            }
                        }
                        let _ = DeleteObject(region.into());
                        return HRGN::default();
                    }
                    region
                }
            }
        }
    }
}

/// Installs a new region and consumes its handle. `None` removes the clip.
fn install_window_region(hwnd: HWND, handle: Option<HRGN>) -> bool {
    // A null result from CreateRectRgn must not remove the existing clip.
    if handle.is_some_and(|region| region.is_invalid()) {
        return false;
    }
    // SAFETY: A live window in production. Region ownership transfers to
    // Windows only on success, so release our handle on failure.
    unsafe {
        if SetWindowRgn(hwnd, handle, true) != 0 {
            return true;
        }
        if let Some(region) = handle {
            let _ = DeleteObject(region.into());
        }
    }
    false
}

fn sync_window_region(
    installed: &mut Option<WindowRegion>,
    desired: Option<WindowRegion>,
    install: impl FnOnce(Option<WindowRegion>) -> bool,
) -> bool {
    if *installed != desired {
        if !install(desired) {
            return false;
        }
        *installed = desired;
    }
    true
}

#[derive(Clone, Copy, Default, PartialEq)]
struct VolumeTrack {
    center_x: f32,
    top: f32,
    bottom: f32,
}

#[derive(Clone, Copy)]
enum IslandAction {
    Toggle(Island),
    Media(MediaControl),
    Volume(VolumeTrack),
    Mute,
    Refresh,
    ScrollUsage(i8),
}

#[derive(Clone, Copy)]
struct IslandHit {
    bounds: (f32, f32, f32, f32),
    action: IslandAction,
}

/// A retained, clipped body. Wheel/arrow input only changes its transform and
/// arrow opacity; no quota queries or redraws.
struct UsageViewport {
    content: ContainerVisual,
    maximum: f32,
    scale: f32,
    bounds: (f32, f32, f32, f32),
    up: SpriteVisual,
    down: SpriteVisual,
}

impl IslandHit {
    fn at(&self, x: f32, y: f32) -> bool {
        let (left, top, right, bottom) = self.bounds;
        x >= left && x < right && y >= top && y < bottom
    }
}

struct ExitAnimation {
    batch: CompositionScopedBatch,
    completed: i64,
}

/// Only these retained visuals move during a volume gesture. Album artwork,
/// track labels and the rest of the menu bar keep their existing visual tree.
struct VolumeControls {
    fill: CompositionRoundedRectangleGeometry,
    value: SpriteVisual,
    geometry: IslandGeometry,
    height: f32,
    position_percent: Option<u8>,
    label_percent: u8,
}

/// Only the progress fill and time labels change while the popup is open.
struct ProgressControls {
    fill: CompositionRoundedRectangleGeometry,
    elapsed: SpriteVisual,
    total: SpriteVisual,
    geometry: IslandGeometry,
    width: f32,
    elapsed_seconds: Option<u64>,
    total_seconds: Option<u64>,
}

impl ProgressControls {
    fn paint(&mut self, gfx: &Gfx, timeline: media::Timeline, now: Instant) -> Result<()> {
        let position = timeline.position_at(now);
        let px = |v| self.geometry.px(v);
        let filled = self.width * (position.as_secs_f64() / timeline.duration.as_secs_f64()) as f32;
        self.fill.SetSize(Vector2::new(filled, px(4.0)))?;
        self.fill
            .SetCornerRadius(Vector2::new(px(2.0).min(filled / 2.0), px(2.0)))?;
        for (visual, cached, seconds, right) in [
            (
                &self.elapsed,
                &mut self.elapsed_seconds,
                position.as_secs(),
                false,
            ),
            (
                &self.total,
                &mut self.total_seconds,
                timeline.duration.as_secs(),
                true,
            ),
        ] {
            if *cached == Some(seconds) {
                continue;
            }
            let label = gfx.text_brush_sized(
                &media_time(seconds),
                self.geometry.scale,
                10.0,
                false,
                self.geometry.ink(),
            )?;
            visual.SetBrush(&label.brush.cast::<CompositionBrush>()?)?;
            visual.SetSize(Vector2::new(label.width, label.height))?;
            if right {
                visual.SetOffset(Vector3::new(
                    px(26.0) + self.width - label.width,
                    px(148.0),
                    0.0,
                ))?;
            }
            *cached = Some(seconds);
        }
        Ok(())
    }
}

impl VolumeControls {
    fn move_to(&mut self, percent: u8) -> Result<()> {
        if self.position_percent == Some(percent) {
            return Ok(());
        }
        let px = |v| self.geometry.px(v);
        let filled = self.height * f32::from(percent) / 100.0;
        self.fill.SetSize(Vector2::new(px(4.0), filled))?;
        self.fill
            .SetOffset(Vector2::new(0.0, self.height - filled))?;
        self.fill
            .SetCornerRadius(Vector2::new(px(2.0), px(2.0).min(filled / 2.0)))?;
        self.position_percent = Some(percent);
        Ok(())
    }

    fn paint(&mut self, gfx: &Gfx, percent: u8) -> Result<()> {
        self.move_to(percent)?;
        if self.label_percent != percent {
            let label = gfx.text_brush_sized(
                &format!("{percent}%"),
                self.geometry.scale,
                10.5,
                false,
                self.geometry.ink(),
            )?;
            self.value
                .SetBrush(&label.brush.cast::<CompositionBrush>()?)?;
            self.value
                .SetSize(Vector2::new(label.width, label.height))?;
            self.value.SetOffset(Vector3::new(
                (self.geometry.width - label.width) / 2.0,
                self.geometry.px(12.0),
                0.0,
            ))?;
            self.label_percent = percent;
        }
        Ok(())
    }
}

impl Drop for ExitAnimation {
    fn drop(&mut self) {
        let _ = self.batch.RemoveCompleted(self.completed);
    }
}

#[derive(Default)]
struct MonitorWidths {
    scale: f32,
    /// Stacked CPU/RAM and network speeds.
    stacked: [f32; 2],
    combined_cpu_ram: f32,
}

pub struct MenuBar {
    bluetooth: Option<bluetooth::Monitor>,
    accessories: Option<accessories::Monitor>,
    accessories_enabled: bool,
    accessory_retry_at: Instant,
    /// The system monitor's counters, only while it samples (issue #64).
    monitor: Option<muelle_shell::monitor::Sampler>,
    monitor_reading: muelle_shell::monitor::Reading,
    /// Widest each monitor column has been at this scale: digits changing
    /// width must not shift the rest of the bar every two seconds.
    monitor_widths: MonitorWidths,
    hwnd: HWND,
    _target: DesktopWindowTarget,
    /// Stable parent: content refreshes must not restart reveal animations.
    surface: ContainerVisual,
    /// Stable children of `surface`: the strip (fills, app name, status
    /// items) and the island above it, which can fade independently.
    strip_layer: ContainerVisual,
    island_layer: ContainerVisual,
    island_shown: Option<bool>,
    island_hide_until: Option<Instant>,
    /// Ghost bar (`Config::menu_bar_ghost`): it auto-hides, but hidden, the
    /// island stays on screen.
    ghost: bool,
    /// The window region set now, to call `SetWindowRgn` only on change.
    window_region: Option<WindowRegion>,
    capsule_layout: CapsuleLayout,
    capsule_layers: Option<[ContainerVisual; 2]>,
    revealed_sides: RevealedSides,
    island_height: f32,
    /// How many times `place` reserved the strip (tests watch mode changes).
    #[cfg(test)]
    strip_reservations: usize,
    appbar: Option<AppBar>,
    app_name: String,
    look: Option<Look>,
    /// Clickable spans, in window pixels.
    zones: Vec<(f32, f32, Action)>,
    visible: bool,
    /// Whether the bar hides now, until the pointer reaches the top edge:
    /// always with "Automatically hide", and with "Intelligent" while the
    /// window in front reaches under it (`covered`).
    auto_hide: bool,
    /// The user's choice of hiding (`Config::menu_bar_auto_hide`).
    hide_setting: bool,
    /// Intelligent hiding (`Config::menu_bar_smart_hide`, issue #61): like
    /// the dock's, the bar hides only while the window in front reaches
    /// under it. It reserves no strip of the screen then, or each switch
    /// would resize every maximized window.
    smart: bool,
    /// The window in front reaches under the bar (see `set_covered`).
    covered: bool,
    /// With auto-hide on: the pointer is at the top edge, so the bar shows.
    revealed: bool,
    hide_until: Option<Instant>,
    animation_epoch: usize,
    exit_animation: Option<ExitAnimation>,
    /// Where the full bar goes on screen.
    bounds: RECT,
    status: Status,
    widgets: MenuBarWidgets,
    /// What the clock opens (`Config::menu_bar_clock_target`). Not drawn, so
    /// it lives outside `widgets` and changing it redraws nothing.
    clock_target: Option<String>,
    usage: Option<UsageMonitor>,
    /// Where each provider sits in the closed island, as screen x.
    usage_anchors: Vec<(Provider, i32)>,
    /// A moment for the crab, waiting for the dock (see `take_cue`).
    cue: Option<(mascot::Act, i32)>,
    /// Whether the network was up at the last reading (`None` before the
    /// first), since when it is down, and whether that was told.
    online: Option<bool>,
    offline_since: Option<Instant>,
    offline_told: bool,
    media: Option<media::Watch>,
    /// "Show what is playing": off, nothing listens to media sessions.
    media_enabled: bool,
    cover: Option<(
        Option<Arc<muelle_shell::icons::IconImage>>,
        CompositionSurfaceBrush,
    )>,
    volume_drag: Option<VolumeTrack>,
    pending_volume: Option<u8>,
    volume_target: Option<u8>,
    volume_controls: Option<VolumeControls>,
    progress_controls: Option<ProgressControls>,
    progress_timer: bool,
    usage_scroll: f32,
    usage_viewport: Option<UsageViewport>,
    suppress_volume_release: bool,
    panel: Option<Island>,
    visual_height: i32,
    triggers_span: Span,
    panel_span: Span,
    island_hits: Vec<IslandHit>,
    outside_click_hook: Option<HHOOK>,
    /// Held for their callbacks; dropping them unsubscribes.
    _network: Option<NetworkWatch>,
    /// Hears when apps start or stop using the microphone or camera.
    privacy_watch: Option<RegistryWatch>,
    /// Windows' taskbar preferences, including the battery percentage.
    settings_watch: Option<RegistryWatch>,
    /// Search uses its own Windows preference key.
    search_watch: Option<RegistryWatch>,
    /// Energy saver on or off arrives as `WM_POWERBROADCAST`; glass follows.
    _saver_watch: muelle_shell::power::SaverWatch,
    audio: Option<AudioWatch>,
}

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            launcher::note_menu_bar_press();
            app::menu_bar_volume_press((l.0 & 0xFFFF) as i16 as f32, (l.0 >> 16) as i16 as f32);
        }
        WM_LBUTTONUP => {
            let x = (l.0 & 0xFFFF) as i16;
            let y = (l.0 >> 16) as i16;
            if !app::menu_bar_volume_release(Some(f32::from(y))) {
                app::menu_bar_clicked(f32::from(x), f32::from(y));
            }
        }
        WM_MOUSEMOVE => {
            let x = (l.0 & 0xFFFF) as i16;
            let y = (l.0 >> 16) as i16;
            app::menu_bar_pointer_moved(f32::from(x), f32::from(y));
        }
        WM_MOUSELEAVE => app::menu_bar_check_pointer(),
        WM_MOUSEWHEEL => {
            let delta = (w.0 >> 16) as u16 as i16;
            let point = POINT {
                x: (l.0 & 0xFFFF) as i16 as i32,
                y: (l.0 >> 16) as i16 as i32,
            };
            if app::menu_bar_scrolled(point, delta) {
                return LRESULT(0);
            }
            return unsafe { DefWindowProcW(hwnd, msg, w, l) };
        }
        WM_TIMER if w.0 == POINTER_TIMER => app::menu_bar_check_pointer(),
        WM_TIMER if w.0 == VOLUME_TIMER => app::menu_bar_volume_flush(),
        WM_TIMER if w.0 == PROGRESS_TIMER => app::menu_bar_progress_tick(),
        WM_TIMER if w.0 == MONITOR_TIMER => app::menu_bar_monitor_tick(),
        WM_CAPTURECHANGED => {
            // Avoid reentering the app borrow inside Set/ReleaseCapture.
            unsafe {
                let _ = PostMessageW(Some(hwnd), VOLUME_CAPTURE_LOST, WPARAM(0), LPARAM(0));
            }
        }
        VOLUME_CAPTURE_LOST => {
            // A delayed notification must not cancel a newer drag.
            if unsafe { GetCapture() } != hwnd {
                app::menu_bar_volume_release(None);
            }
        }
        WM_TIMER if w.0 == HIDE_ANIMATION_TIMER => app::menu_bar_finish_hide(None),
        HIDE_ANIMATION_DONE => app::menu_bar_finish_hide(Some(w.0)),
        DISMISS_ISLAND => app::menu_bar_dismiss_island(),
        REGISTRY_CHANGED => app::menu_bar_registry_changed(),
        ACCESSORIES_CHANGED => app::menu_bar_accessories_changed(),
        MEDIA_CHANGED => app::menu_bar_media_changed(),
        BLUETOOTH_CHANGED => app::menu_bar_bluetooth_changed(),
        AUDIO_CHANGED => {
            if !AUDIO_TICK_PENDING.swap(true, Ordering::Relaxed) {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), AUDIO_TICK, WPARAM(0), LPARAM(0));
                }
            }
        }
        AUDIO_TICK => {
            AUDIO_TICK_PENDING.store(false, Ordering::Relaxed);
            app::menu_bar_audio_changed();
        }
        WM_SETCURSOR => {
            // SAFETY: Keep the standard arrow throughout the bar and popups.
            if let Ok(arrow) = unsafe { LoadCursorW(None, IDC_ARROW) } {
                unsafe { SetCursor(Some(arrow)) };
                return LRESULT(1);
            }
            return unsafe { DefWindowProcW(hwnd, msg, w, l) };
        }
        WM_NCHITTEST => {
            let x = (l.0 & 0xFFFF) as i16 as i32;
            let y = (l.0 >> 16) as i16 as i32;
            let in_island_extension = y < TRIGGERS_BOTTOM.load(Ordering::Relaxed)
                && x >= TRIGGERS_LEFT.load(Ordering::Relaxed)
                && x < TRIGGERS_RIGHT.load(Ordering::Relaxed);
            if y >= BAR_BOTTOM.load(Ordering::Relaxed)
                && !in_island_extension
                && (y >= PANEL_BOTTOM.load(Ordering::Relaxed)
                    || x < PANEL_LEFT.load(Ordering::Relaxed)
                    || x >= PANEL_RIGHT.load(Ordering::Relaxed))
            {
                return LRESULT(-1);
            }
            // SAFETY: Default hit testing for the bar and the island.
            return unsafe { DefWindowProcW(hwnd, msg, w, l) };
        }
        // The minute changed, the charger was plugged or unplugged, the
        // network or volume changed, or a Windows setting (taskbar buttons).
        // Nothing the bar shows depends on the work area.
        WM_SETTINGCHANGE if w.0 == SPI_SETWORKAREA => {}
        // Some of these are sent while the dock is busy reserving space, and
        // updating then was dropped. From the message loop the dock is never
        // busy, and a burst of them makes a single update.
        // WM_TIMECHANGE: the clock or time zone was changed.
        WM_TIMER | WM_POWERBROADCAST | WM_TIMECHANGE | STATUS_CHANGED | USAGE_CHANGED
        | WM_SETTINGCHANGE => {
            if msg == WM_POWERBROADCAST && matches!(w.0, 7 | 18) {
                app::menu_bar_accessories_reset();
            }
            if msg == WM_POWERBROADCAST && w.0 == PBT_POWERSETTINGCHANGE as usize {
                // SAFETY: Windows owns the setting payload for this message.
                unsafe {
                    muelle_shell::power::power_setting_changed(l);
                }
            }
            if !TICK_PENDING.swap(true, Ordering::Relaxed) {
                // SAFETY: Posting to our own live window.
                unsafe {
                    let _ = PostMessageW(Some(hwnd), TICK, WPARAM(0), LPARAM(0));
                }
            }
        }
        TICK => {
            TICK_PENDING.store(false, Ordering::Relaxed);
            app::menu_bar_tick();
        }
        // SAFETY: Standard forwarding of unhandled messages.
        _ => return unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
    LRESULT(0)
}

unsafe extern "system" fn outside_click_proc(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0
        && matches!(
            w.0 as u32,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
        )
    {
        // SAFETY: Windows provides an MSLLHOOKSTRUCT for mouse hook actions.
        let point = unsafe { (*(l.0 as *const MSLLHOOKSTRUCT)).pt };
        let in_triggers = point.y >= BAR_TOP.load(Ordering::Relaxed)
            && point.y < TRIGGERS_BOTTOM.load(Ordering::Relaxed)
            && point.x >= TRIGGERS_LEFT.load(Ordering::Relaxed)
            && point.x < TRIGGERS_RIGHT.load(Ordering::Relaxed);
        let in_panel = point.y >= BAR_BOTTOM.load(Ordering::Relaxed)
            && point.y < PANEL_BOTTOM.load(Ordering::Relaxed)
            && point.x >= PANEL_LEFT.load(Ordering::Relaxed)
            && point.x < PANEL_RIGHT.load(Ordering::Relaxed);
        if !in_triggers && !in_panel {
            let window = ISLAND_HOOK_WINDOW.load(Ordering::Relaxed);
            if window != 0 {
                // SAFETY: The window remains live until the hook is removed in Drop.
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(window as *mut _)),
                        DISMISS_ISLAND,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
        }
    }
    // SAFETY: Never consume the click; the target application must receive it.
    unsafe { CallNextHookEx(None, code, w, l) }
}

pub(crate) fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color {
        A: a,
        R: r,
        G: g,
        B: b,
    }
}

pub(crate) fn add_visual(
    compositor: &Compositor,
    layers: &VisualCollection,
    brush: &CompositionBrush,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Result<SpriteVisual> {
    let visual = compositor.CreateSpriteVisual()?;
    visual.SetSize(Vector2 {
        X: width,
        Y: height,
    })?;
    visual.SetOffset(Vector3 {
        X: x.round(),
        Y: y.round(),
        Z: 0.0,
    })?;
    visual.SetBrush(brush)?;
    layers.InsertAtTop(&visual)?;
    Ok(visual)
}

#[expect(
    clippy::too_many_arguments,
    reason = "A rounded shape needs its rectangle, radius and two colors"
)]
pub(crate) fn add_rounded_box(
    compositor: &Compositor,
    layers: &VisualCollection,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    fill: Color,
    border: Option<Color>,
) -> Result<()> {
    retained_rounded_box(
        compositor, layers, x, y, width, height, radius, fill, border,
    )
    .map(|_| ())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Same rectangle/color inputs as add_rounded_box; retain geometry for small UI updates"
)]
fn retained_rounded_box(
    compositor: &Compositor,
    layers: &VisualCollection,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    fill: Color,
    border: Option<Color>,
) -> Result<(ShapeVisual, CompositionRoundedRectangleGeometry)> {
    let geometry = compositor.CreateRoundedRectangleGeometry()?;
    let inset = if border.is_some() { 0.5 } else { 0.0 };
    geometry.SetOffset(Vector2 { X: inset, Y: inset })?;
    geometry.SetSize(Vector2 {
        X: (width - inset * 2.0).max(0.0),
        Y: (height - inset * 2.0).max(0.0),
    })?;
    geometry.SetCornerRadius(Vector2 {
        X: radius.min(width / 2.0),
        Y: radius.min(height / 2.0),
    })?;
    let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
    shape.SetFillBrush(&compositor.CreateColorBrushWithColor(fill)?)?;
    if let Some(border) = border {
        shape.SetStrokeBrush(&compositor.CreateColorBrushWithColor(border)?)?;
        shape.SetStrokeThickness(1.0)?;
    }
    let visual = compositor.CreateShapeVisual()?;
    visual.SetSize(Vector2 {
        X: width,
        Y: height,
    })?;
    visual.SetOffset(Vector3 {
        X: x.round(),
        Y: y.round(),
        Z: 0.0,
    })?;
    visual.Shapes()?.Append(&shape)?;
    layers.InsertAtTop(&visual)?;
    Ok((visual, geometry))
}

#[expect(
    clippy::too_many_arguments,
    reason = "Typography and placement are chosen together for panel labels"
)]
fn add_panel_text(
    gfx: &Gfx,
    layers: &VisualCollection,
    value: &str,
    x: f32,
    y: f32,
    scale: f32,
    font_size: f32,
    bold: bool,
    ink: Ink,
    opacity: f32,
) -> Result<LabelBrush> {
    let label = gfx.text_brush_sized(value, scale, font_size, bold, ink)?;
    add_visual(
        &gfx.compositor,
        layers,
        &label.brush.cast()?,
        x,
        y,
        label.width,
        label.height,
    )?
    .SetOpacity(opacity)?;
    Ok(label)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Right-aligned panel labels choose typography and placement together"
)]
fn add_panel_text_right(
    gfx: &Gfx,
    layers: &VisualCollection,
    value: &str,
    right: f32,
    y: f32,
    scale: f32,
    font_size: f32,
    bold: bool,
    ink: Ink,
    opacity: f32,
) -> Result<LabelBrush> {
    let label = gfx.text_brush_sized(value, scale, font_size, bold, ink)?;
    add_visual(
        &gfx.compositor,
        layers,
        &label.brush.cast()?,
        right - label.width,
        y,
        label.width,
        label.height,
    )?
    .SetOpacity(opacity)?;
    Ok(label)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Each provider row draws its own colors, data and measured rectangle"
)]
fn draw_usage_row(
    gfx: &Gfx,
    layers: &VisualCollection,
    provider: Provider,
    usage: &ProviderUsage,
    accent: Color,
    scale: f32,
    light: bool,
    island_width: f32,
    y: f32,
) -> Result<()> {
    let c = &gfx.compositor;
    let px = |logical: f32| (logical * scale).round();
    let ink = if light { Ink::Dark } else { Ink::Light };
    let text = strings::text();
    let left = px(23.0);
    let width = island_width - px(46.0);
    let right = left + width;
    add_rounded_box(
        c,
        layers,
        left + px(12.0),
        y + px(11.0),
        px(32.0),
        px(32.0),
        px(10.0),
        rgba(accent.R, accent.G, accent.B, if light { 31 } else { 41 }),
        None,
    )?;
    if let Some(image) = usage::logo(provider) {
        let icon_size = px(20.0) as u32;
        let image_brush = gfx.icon_brush(image, icon_size)?;
        let brush: CompositionBrush =
            if !matches!(provider, Provider::Claude | Provider::Gemini) && light {
                let tinted = c.CreateMaskBrush()?;
                tinted.SetMask(&image_brush)?;
                tinted.SetSource(&c.CreateColorBrushWithColor(rgba(27, 34, 45, 255))?)?;
                tinted.cast()?
            } else {
                image_brush.cast()?
            };
        add_visual(
            c,
            layers,
            &brush,
            left + px(18.0),
            y + px(17.0),
            icon_size as f32,
            icon_size as f32,
        )?;
    }
    add_panel_text(
        gfx,
        layers,
        provider.name(),
        left + px(53.0),
        y + px(10.0),
        scale,
        14.0,
        true,
        ink,
        1.0,
    )?;
    if let Some(main) = usage.main_window() {
        add_panel_text(
            gfx,
            layers,
            &main.title(),
            left + px(53.0),
            y + px(30.0),
            scale,
            11.5,
            false,
            ink,
            0.72,
        )?;
        add_panel_text_right(
            gfx,
            layers,
            &format!("{}%", main.used_percent),
            right - px(14.0),
            y + px(6.0),
            scale,
            20.0,
            true,
            ink,
            1.0,
        )?;
        add_panel_text_right(
            gfx,
            layers,
            text.usage_used,
            right - px(15.0),
            y + px(31.0),
            scale,
            11.0,
            false,
            ink,
            0.7,
        )?;
        let track_left = left + px(14.0);
        let track_width = width - px(28.0);
        let track_y = y + px(53.0);
        add_rounded_box(
            c,
            layers,
            track_left,
            track_y,
            track_width,
            px(6.0),
            px(3.0),
            if light {
                rgba(54, 73, 95, 43)
            } else {
                rgba(224, 235, 248, 48)
            },
            None,
        )?;
        let fill = track_width * f32::from(main.used_percent) / 100.0;
        if fill > 0.0 {
            add_rounded_box(
                c,
                layers,
                track_left,
                track_y,
                fill,
                px(6.0),
                px(3.0),
                accent,
                None,
            )?;
        }
        if let Some(reset) = main.reset_text() {
            add_panel_text_right(
                gfx,
                layers,
                &format!("{} {reset}", text.usage_resets),
                right - px(14.0),
                y + px(64.0),
                scale,
                11.0,
                false,
                ink,
                0.78,
            )?;
        }
        if usage.state == State::Unavailable
            || usage.age_minutes().is_some_and(|minutes| minutes > 5)
        {
            add_panel_text(
                gfx,
                layers,
                text.usage_stale,
                left + px(14.0),
                y + px(64.0),
                scale,
                11.0,
                false,
                ink,
                0.7,
            )?;
        }
        for (index, secondary) in usage.windows.iter().skip(1).enumerate() {
            add_visual(
                c,
                layers,
                &c.CreateColorBrushWithColor(if light {
                    rgba(50, 68, 88, 24)
                } else {
                    rgba(228, 236, 247, 29)
                })?
                .cast()?,
                left + px(14.0),
                y + px(83.0 + index as f32 * 26.0),
                width - px(28.0),
                px(1.0),
            )?;
            add_panel_text(
                gfx,
                layers,
                &format!("{} · {}%", secondary.title(), secondary.used_percent),
                left + px(14.0),
                y + px(87.0 + index as f32 * 26.0),
                scale,
                11.5,
                true,
                ink,
                0.94,
            )?;
            if let Some(reset) = secondary.reset_text() {
                add_panel_text_right(
                    gfx,
                    layers,
                    &format!("{} {reset}", text.usage_resets),
                    right - px(14.0),
                    y + px(87.0 + index as f32 * 26.0),
                    scale,
                    11.5,
                    false,
                    ink,
                    0.76,
                )?;
            }
        }
    } else {
        let message = match usage.state {
            State::Loading => text.usage_loading,
            State::SignIn if provider == Provider::Claude => text.usage_claude_sign_in,
            State::SignIn => text.usage_sign_in,
            State::Ready | State::Unavailable => text.usage_unavailable,
        };
        add_panel_text(
            gfx,
            layers,
            message,
            left + px(53.0),
            y + px(43.0),
            scale,
            12.0,
            false,
            ink,
            0.72,
        )?;
    }
    Ok(())
}

fn usage_row_height(usage: &ProviderUsage) -> f32 {
    ISLAND_ROW_HEIGHT + usage.windows.len().saturating_sub(2) as f32 * 26.0
}

impl MenuBar {
    pub fn create(
        gfx: &Gfx,
        usage_enabled: bool,
        media_enabled: bool,
        auto_hide: bool,
        widgets: MenuBarWidgets,
    ) -> Result<Self> {
        let class = w!("MuelleMenuBar");
        // SAFETY: Standard class registration and window creation on the UI
        // thread; the window is destroyed in `Drop`.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None)?;
            // Registering an existing class fails harmlessly.
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(bar_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: class,
                ..Default::default()
            });
            let hwnd = CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!("Muelle"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            // Without this, the system backdrop brush appears black.
            let enable = BOOL::from(true);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_HOSTBACKDROPBRUSH,
                (&raw const enable).cast(),
                size_of::<BOOL>() as u32,
            );
            hwnd
        };
        let target = unsafe {
            // SAFETY: `hwnd` is the live window created above.
            gfx.compositor
                .cast::<ICompositorDesktopInterop>()?
                .CreateDesktopWindowTarget(hwnd, true)?
        };
        // Our compositor owns the transition; do not combine it with DWM's
        // native show/hide effect, which can present a cached opaque frame.
        system::set_window_transitions(hwnd.0 as isize, false);
        let surface = gfx.compositor.CreateContainerVisual()?;
        target.SetRoot(&surface)?;
        let strip_layer = gfx.compositor.CreateContainerVisual()?;
        let island_layer = gfx.compositor.CreateContainerVisual()?;
        surface.Children()?.InsertAtTop(&strip_layer)?;
        surface.Children()?.InsertAtTop(&island_layer)?;
        let mut bar = Self {
            accessories: None,
            accessories_enabled: false,
            accessory_retry_at: Instant::now() + Duration::from_secs(60),
            monitor: None,
            monitor_reading: muelle_shell::monitor::Reading::default(),
            monitor_widths: MonitorWidths::default(),
            hwnd,
            _target: target,
            surface,
            strip_layer,
            island_layer,
            island_shown: None,
            island_hide_until: None,
            ghost: false,
            window_region: None,
            capsule_layout: CapsuleLayout::default(),
            capsule_layers: None,
            revealed_sides: RevealedSides::default(),
            island_height: 0.0,
            #[cfg(test)]
            strip_reservations: 0,
            appbar: AppBar::register(hwnd, APPBAR_CALLBACK),
            app_name: String::new(),
            look: None,
            zones: Vec::new(),
            visible: true,
            auto_hide,
            hide_setting: auto_hide,
            smart: false,
            covered: false,
            revealed: false,
            hide_until: None,
            animation_epoch: 0,
            exit_animation: None,
            bounds: RECT::default(),
            status: Status {
                bluetooth_on: false,
                network: Network::Offline,
                volume: None,
                buttons: TaskbarButtons::default(),
                privacy: Privacy::default(),
                battery_percentage: false,
            },
            widgets,
            clock_target: None,
            usage: usage_enabled.then(|| UsageMonitor::new(hwnd.0 as isize, USAGE_CHANGED)),
            usage_anchors: Vec::new(),
            cue: None,
            online: None,
            offline_since: None,
            offline_told: false,
            media: media_enabled
                .then(|| media::Watch::start(hwnd, MEDIA_CHANGED))
                .flatten(),
            media_enabled,
            cover: None,
            volume_drag: None,
            pending_volume: None,
            volume_target: None,
            volume_controls: None,
            progress_controls: None,
            progress_timer: false,
            usage_scroll: 0.0,
            usage_viewport: None,
            suppress_volume_release: false,
            panel: None,
            visual_height: 0,
            triggers_span: Span::default(),
            panel_span: Span::default(),
            island_hits: Vec::new(),
            outside_click_hook: None,
            bluetooth: bluetooth::Monitor::new(hwnd.0 as isize, BLUETOOTH_CHANGED).ok(),
            _network: NetworkWatch::new(hwnd.0 as isize, STATUS_CHANGED),
            privacy_watch: status::watch_privacy(hwnd.0 as isize, REGISTRY_CHANGED),
            settings_watch: status::watch_taskbar_settings(hwnd.0 as isize, REGISTRY_CHANGED),
            search_watch: status::watch_search_settings(hwnd.0 as isize, REGISTRY_CHANGED),
            _saver_watch: muelle_shell::power::SaverWatch::new(hwnd.0 as isize),
            audio: AudioWatch::new(hwnd.0 as isize, AUDIO_CHANGED),
        };
        AUDIO_TICK_PENDING.store(false, Ordering::Relaxed);
        bar.update_status();
        bar.refresh(gfx)?;
        bar.schedule_next_minute();
        Ok(bar)
    }

    pub fn set_app_name(&mut self, gfx: &Gfx, name: &str) -> Result<()> {
        if self.app_name == name {
            return Ok(());
        }
        name.clone_into(&mut self.app_name);
        self.refresh(gfx)
    }

    /// Hidden while an app is full screen.
    /// How tall the bar is at `scale`: the strip it keeps at the top of the
    /// primary display unless it hides itself. A side dock starts below it.
    pub fn strip_height(scale: f32) -> i32 {
        (HEIGHT * scale).round() as i32
    }

    pub fn set_visible(&mut self, visible: bool) -> Result<()> {
        if !visible {
            self.release_volume(None)?;
        }
        if self.visible == visible {
            return Ok(());
        }
        self.visible = visible;
        if !visible && self.auto_hide {
            self.revealed = false;
        }
        self.place()
    }

    #[cfg(test)]
    pub fn set_auto_hide(&mut self, auto_hide: bool) -> Result<()> {
        self.set_hiding(self.ghost, auto_hide, self.smart, self.covered)
    }

    /// Intelligent hiding on or off (see `smart`).
    #[cfg(test)]
    pub fn set_smart_hide(&mut self, smart: bool) -> Result<()> {
        self.set_hiding(self.ghost, self.hide_setting, smart, self.covered)
    }

    /// The three ways the bar hides, set together and applied once: one at a
    /// time, a switch between two of them passed through "always visible",
    /// reserving the strip (resizing maximized windows) and showing the bar
    /// for a moment.
    pub fn set_hiding(
        &mut self,
        ghost: bool,
        auto_hide: bool,
        smart: bool,
        covered: bool,
    ) -> Result<()> {
        if (self.ghost, self.hide_setting, self.smart) == (ghost, auto_hide, smart) {
            return Ok(());
        }
        if self.ghost != ghost {
            if self.widgets.bare {
                self.panel = None;
            }
            // Whichever layer animated so far starts over in its resting state.
            self.animation_epoch = self.animation_epoch.wrapping_add(1);
            self.exit_animation = None;
            self.island_shown = None;
            self.island_hide_until = None;
            for layer in [&self.strip_layer, &self.island_layer, &self.surface] {
                layer.StopAnimation(h!("Offset.Y"))?;
                layer.StopAnimation(h!("Opacity"))?;
                layer.SetOffset(Vector3::new(0.0, 0.0, 0.0))?;
                layer.SetOpacity(1.0)?;
            }
        }
        self.ghost = ghost;
        self.hide_setting = auto_hide;
        self.smart = smart;
        self.covered = covered;
        self.apply_hiding()
    }

    /// Ghost bar on or off (see `ghost`).
    #[cfg(test)]
    pub fn set_ghost(&mut self, ghost: bool) -> Result<()> {
        self.set_hiding(ghost, self.hide_setting, self.smart, self.covered)
    }

    fn presence_now(&self) -> Presence {
        presence(
            self.visible,
            self.auto_hide,
            self.revealed,
            self.ghost,
            self.covered,
            self.triggers_span.width() > 0.0,
            self.zoned(),
        )
    }

    fn zoned(&self) -> bool {
        self.ghost && self.widgets.bare
    }

    fn status_shown(&self) -> bool {
        self.visible
            && (!self.auto_hide || self.revealed)
            && (!self.zoned() || self.revealed_sides.right)
    }

    fn zone_entrance(&self) -> Span {
        if self.triggers_span.width() > 0.0 {
            return self.triggers_span;
        }
        // Keep the side controls reachable when neither island widget shows.
        let width = (self.bounds.right - self.bounds.left) as f32;
        let scale = (self.bounds.bottom - self.bounds.top) as f32 / HEIGHT;
        let half_entry = width.min((EMPTY_ISLAND_ENTRY_WIDTH * scale).round()) / 2.0;
        Span {
            left: width / 2.0 - half_entry,
            right: width / 2.0 + half_entry,
        }
    }

    /// What slides and fades when the bar hides: the strip alone with the
    /// ghost bar, so the island stays put; the whole bar otherwise.
    fn motion_layer(&self) -> &ContainerVisual {
        if self.ghost {
            &self.strip_layer
        } else {
            &self.surface
        }
    }

    /// A change of setting applies at once, without animation.
    fn apply_hiding(&mut self) -> Result<()> {
        self.auto_hide = self.ghost || self.hide_setting || (self.smart && self.covered);
        self.revealed = false;
        self.revealed_sides = RevealedSides::default();
        self.hide_until = None;
        // The reserved strip may change even when hiding does not.
        self.place()
    }

    /// Whether the bar keeps a strip of the screen for itself, so windows
    /// are maximized below it: only while it neither auto-hides nor hides
    /// intelligently.
    fn reserves(&self) -> bool {
        !self.hide_setting && !self.smart && !self.ghost
    }

    /// Where the bar is on screen, shown or not.
    pub fn bounds(&self) -> RECT {
        self.bounds
    }

    /// Intelligent hiding: whether the window in front reaches under the
    /// bar. It slides away as when the pointer leaves an auto-hiding bar,
    /// unless the pointer is on it, and slides back once nothing covers it;
    /// meanwhile the top edge reveals it as usual.
    pub fn set_covered(&mut self, gfx: &Gfx, covered: bool) -> Result<()> {
        if self.covered == covered {
            return Ok(());
        }
        self.covered = covered;
        if self.ghost {
            // Coverage can hide the island in the original ghost mode.
            return self.place();
        }
        if !self.smart || self.hide_setting {
            return Ok(());
        }
        if covered {
            // It is on screen now: hide it the way a leaving pointer does.
            self.auto_hide = true;
            self.revealed = true;
            let mut point = POINT::default();
            // SAFETY: Read-only cursor query.
            let on_bar = unsafe { GetCursorPos(&mut point) }.is_ok()
                && point.x >= self.bounds.left
                && point.x < self.bounds.right
                && point.y >= self.bounds.top
                && point.y < self.bounds.bottom;
            if on_bar || self.panel.is_some() || self.volume_drag.is_some() {
                // In use: it goes once the pointer leaves, as usual.
                return self.place();
            }
            return self.hover(gfx, false);
        }
        let was_hidden = self.auto_hide && !self.revealed;
        self.auto_hide = false;
        self.revealed = false;
        self.hide_until = None;
        self.animation_epoch = self.animation_epoch.wrapping_add(1);
        self.exit_animation = None;
        self.place()?;
        if was_hidden && self.visible && system::animations_enabled() {
            // `place` showed it in its final state; slide it in from there.
            self.motion_layer()
                .SetOffset(Vector3::new(0.0, -self.reveal_lift(), 0.0))?;
            self.motion_layer().SetOpacity(0.0)?;
            if let Err(error) = self.animate_visibility(gfx, true) {
                muelle_shell::log::line(&format!("animación de barra superior: {error:#}"));
                self.reset_surface()?;
            }
        }
        // Content is not refreshed while hidden.
        self.refresh(gfx)
    }

    pub fn set_usage_enabled(&mut self, gfx: &Gfx, enabled: bool) -> Result<()> {
        if self.usage.is_some() == enabled {
            return Ok(());
        }
        self.usage = enabled.then(|| UsageMonitor::new(self.hwnd.0 as isize, USAGE_CHANGED));
        self.refresh(gfx)
    }

    /// Shows or hides what is playing. Off, the media watch stops too, so
    /// nothing follows playback changes.
    pub fn set_media_enabled(&mut self, gfx: &Gfx, enabled: bool) -> Result<()> {
        if self.media_enabled == enabled {
            return Ok(());
        }
        self.media_enabled = enabled;
        self.media = enabled
            .then(|| media::Watch::start(self.hwnd, MEDIA_CHANGED))
            .flatten();
        self.refresh(gfx)
    }

    pub fn set_accessories_enabled(&mut self, gfx: &Gfx, enabled: bool) -> Result<()> {
        if self.accessories_enabled == enabled {
            return Ok(());
        }
        self.accessories_enabled = enabled;
        if enabled {
            self.restart_accessories(gfx)
        } else {
            self.accessories = None;
            self.refresh(gfx)
        }
    }

    pub fn accessories_changed(&mut self, gfx: &Gfx) -> Result<()> {
        if self.accessories_enabled && self.status_shown() {
            self.refresh(gfx)?;
        }
        Ok(())
    }

    pub fn restart_accessories(&mut self, gfx: &Gfx) -> Result<()> {
        // Resume/audio notifications and the minute tick must never start a
        // disabled reader, including after a previously enabled one failed.
        if !self.accessories_enabled {
            return Ok(());
        }
        self.accessory_retry_at = Instant::now() + Duration::from_secs(60);
        self.accessories = None;
        self.accessories = accessories::Monitor::new(
            self.hwnd.0 as isize,
            ACCESSORIES_CHANGED,
            self.status_shown(),
        )
        .inspect_err(|error| eprintln!("muelle: accesorios: {error:#}"))
        .ok();
        self.refresh(gfx)
    }

    pub fn set_widgets(&mut self, gfx: &Gfx, widgets: MenuBarWidgets) -> Result<()> {
        let was_zoned = self.look.as_ref().is_some_and(|look| look.zoned);
        let zoned = self.ghost && widgets.bare;
        if self.widgets == widgets && was_zoned == zoned {
            return Ok(());
        }
        self.widgets = widgets;
        if was_zoned != zoned {
            self.panel = None;
            self.apply_hiding()?;
        }
        self.refresh(gfx)
    }

    /// What clicking the clock opens; `None` for the notification center.
    pub fn set_clock_target(&mut self, target: Option<&str>) {
        if self.clock_target.as_deref() != target {
            self.clock_target = target.map(str::to_owned);
        }
    }

    /// The pointer reached the top edge or left the bar.
    pub fn hover(&mut self, gfx: &Gfx, inside: bool) -> Result<()> {
        if self.volume_drag.is_some() && !inside {
            return Ok(());
        }
        if !inside && self.panel.take().is_some() {
            self.refresh(gfx)?;
        }
        if !self.auto_hide || self.revealed == inside {
            return Ok(());
        }
        let was_hiding = self.hide_until.is_some();
        let animated = self.visible && system::animations_enabled();
        self.animation_epoch = self.animation_epoch.wrapping_add(1);
        self.exit_animation = None;
        self.revealed = inside;
        if inside && self.zoned() && !was_hiding {
            self.revealed_sides = RevealedSides::default();
        }
        self.hide_until = (!inside && animated)
            .then(|| Instant::now() + Duration::from_millis(u64::from(HIDE_FALLBACK_MS)));
        if !animated {
            self.reset_surface()?;
        } else if inside && !was_hiding {
            self.motion_layer()
                .SetOffset(Vector3::new(0.0, -self.reveal_lift(), 0.0))?;
            self.motion_layer().SetOpacity(0.0)?;
        }
        self.place()?;
        if animated && let Err(error) = self.animate_visibility(gfx, inside) {
            muelle_shell::log::line(&format!("animación de barra superior: {error:#}"));
            self.hide_until = None;
            self.reset_surface()?;
            self.place()?;
        }
        if inside && !self.zoned() {
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            // SAFETY: `track` is a valid structure with its size initialized.
            unsafe { TrackMouseEvent(&mut track)? };
        }
        let reason = if inside && self.ghost && self.visible {
            usage::Refresh::Reveal
        } else {
            usage::Refresh::Scheduled
        };
        self.refresh_with_usage(gfx, reason)
    }

    fn reveal_lift(&self) -> f32 {
        (self.bounds.bottom - self.bounds.top) as f32 * 0.3
    }

    fn visibility_easing(
        c: &Compositor,
        shown: bool,
    ) -> Result<(CubicBezierEasingFunction, TimeSpan)> {
        let ease = if shown {
            c.CreateCubicBezierEasingFunction(Vector2::new(0.2, 0.8), Vector2::new(0.2, 1.0))?
        } else {
            c.CreateCubicBezierEasingFunction(Vector2::new(0.4, 0.0), Vector2::new(1.0, 1.0))?
        };
        let duration = TimeSpan {
            Duration: i64::from(if shown { REVEAL_MS } else { HIDE_MS }) * 10_000,
        };
        Ok((ease, duration))
    }

    fn visibility_fade(
        c: &Compositor,
        shown: bool,
        ease: &CubicBezierEasingFunction,
        duration: TimeSpan,
    ) -> Result<ScalarKeyFrameAnimation> {
        let fade = c.CreateScalarKeyFrameAnimation()?;
        fade.SetDuration(duration)?;
        fade.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        fade.InsertKeyFrameWithEasingFunction(1.0, if shown { 1.0 } else { 0.0 }, ease)?;
        Ok(fade)
    }

    fn sync_island_fade(&mut self, presence: Presence) -> Result<()> {
        if !self.ghost {
            return Ok(());
        }
        let shown = presence != Presence::Hidden && self.triggers_span.width() > 0.0;
        let previous = self.island_shown;
        if self.visible && previous == Some(shown) {
            return Ok(());
        }
        self.island_hide_until = None;
        // An island left without content has nothing to fade, and no region
        // to clip to: fading it would leave a full-width window eating clicks.
        let has_region = self.triggers_span.width() > 0.0;
        if self.visible && previous.is_some() && has_region && system::animations_enabled() {
            let started = (|| -> Result<()> {
                let c = self.island_layer.Compositor()?;
                let (ease, duration) = Self::visibility_easing(&c, shown)?;
                let fade = Self::visibility_fade(&c, shown, &ease, duration)?;
                self.island_layer.StartAnimation(h!("Opacity"), &fade)?;
                Ok(())
            })();
            if started.is_ok() {
                self.island_shown = Some(shown);
                self.island_hide_until =
                    (!shown).then(|| Instant::now() + Duration::from_millis(u64::from(HIDE_MS)));
                return Ok(());
            }
            if let Err(error) = started {
                muelle_shell::log::line(&format!("animación de la isla: {error:#}"));
            }
        }
        self.island_layer.StopAnimation(h!("Opacity"))?;
        self.island_layer
            .SetOpacity(if shown { 1.0 } else { 0.0 })?;
        self.island_shown = self.visible.then_some(shown);
        Ok(())
    }

    fn animate_visibility(&mut self, gfx: &Gfx, shown: bool) -> Result<()> {
        let c = &gfx.compositor;
        let (ease, duration) = Self::visibility_easing(c, shown)?;
        let slide = c.CreateScalarKeyFrameAnimation()?;
        slide.SetDuration(duration)?;
        slide.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        slide.InsertKeyFrameWithEasingFunction(
            1.0,
            if shown { 0.0 } else { -self.reveal_lift() },
            &ease,
        )?;
        let fade = Self::visibility_fade(c, shown, &ease, duration)?;
        let batch = (!shown)
            .then(|| c.CreateScopedBatch(CompositionBatchTypes::Animation))
            .transpose()?;
        let started = (|| -> Result<()> {
            self.motion_layer().StartAnimation(h!("Offset.Y"), &slide)?;
            self.motion_layer().StartAnimation(h!("Opacity"), &fade)?;
            Ok(())
        })();
        if let Some(batch) = batch {
            let window = self.hwnd.0 as isize;
            let epoch = self.animation_epoch;
            let completed = batch.Completed(&TypedEventHandler::new(move |_, _| {
                // SAFETY: Only scalar state is posted back to the UI thread.
                // A cancelled transition's already queued reply is ignored.
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(window as *mut _)),
                        HIDE_ANIMATION_DONE,
                        WPARAM(epoch),
                        LPARAM(0),
                    );
                }
                Ok(())
            }));
            // Register before ending the scope. End it even on failure so
            // unrelated later animations cannot become part of this exit.
            let ended = batch.End();
            started?;
            ended?;
            let completed = completed?;
            self.exit_animation = Some(ExitAnimation { batch, completed });
        } else {
            started?;
        }
        Ok(())
    }

    fn park_surface(&self) -> Result<()> {
        let layer = self.motion_layer();
        layer.StopAnimation(h!("Offset.Y"))?;
        layer.StopAnimation(h!("Opacity"))?;
        layer.SetOffset(Vector3::new(0.0, -self.reveal_lift(), 0.0))?;
        layer.SetOpacity(0.0)?;
        Ok(())
    }

    fn reset_surface(&self) -> Result<()> {
        let layer = self.motion_layer();
        layer.StopAnimation(h!("Offset.Y"))?;
        layer.StopAnimation(h!("Opacity"))?;
        layer.SetOffset(Vector3::new(0.0, 0.0, 0.0))?;
        layer.SetOpacity(1.0)?;
        Ok(())
    }

    pub fn finish_hide(&mut self, gfx: &Gfx, completed_epoch: Option<usize>) -> Result<()> {
        let mut point = POINT::default();
        // SAFETY: Read-only cursor query before completing an exit transition.
        unsafe { GetCursorPos(&mut point)? };
        self.finish_hide_at(gfx, Instant::now(), point, completed_epoch)
    }

    fn finish_hide_at(
        &mut self,
        gfx: &Gfx,
        now: Instant,
        point: POINT,
        completed_epoch: Option<usize>,
    ) -> Result<()> {
        // A queued timer from a cancelled transition cannot hide a reopened bar.
        if completed_epoch.is_some_and(|epoch| epoch != self.animation_epoch) {
            return Ok(());
        }
        let island_hiding = self.island_hide_until.is_some();
        if self
            .island_hide_until
            .is_some_and(|deadline| now >= deadline)
        {
            self.island_hide_until = None;
        }
        let Some(deadline) = self.hide_until else {
            return if island_hiding { self.place() } else { Ok(()) };
        };
        self.pointer_left_at(gfx, point)?;
        if self.hide_until.is_none() {
            return Ok(());
        }
        if completed_epoch.is_some() || now >= deadline {
            self.hide_until = None;
            self.exit_animation = None;
        }
        self.place()
    }

    /// A leave notification can already be queued when the hidden window is
    /// shown. Check the pointer rather than hiding a newly revealed bar on it.
    pub fn check_pointer(&mut self, gfx: &Gfx) -> Result<()> {
        if self.zoned()
            && self.desired_window_region(self.presence_now(), self.hide_until.is_some())
                != self.window_region
        {
            self.place()?;
        }
        if self.panel.is_some() {
            // SAFETY: Read-only Escape state; checked only while a popup is open.
            if unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(0x1B) } < 0 {
                return self.dismiss_island(gfx);
            }
        }
        let mut point = POINT::default();
        // SAFETY: Read-only cursor query in the same DPI context as `bounds`.
        unsafe { GetCursorPos(&mut point)? };
        self.pointer_left_at(gfx, point)
    }

    fn pointer_left_at(&mut self, gfx: &Gfx, point: POINT) -> Result<()> {
        let x = (point.x - self.bounds.left) as f32;
        if let Some(span) = self.volume_drag {
            let y = (point.y - self.bounds.top) as f32;
            self.queue_volume(span, y);
            // A nonactivating background window may not receive mouse-up
            // outside its visible bounds. Reuse the existing popup pointer
            // check to finish the gesture without ever stealing app focus.
            let pressed = unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
                let primary = if GetSystemMetrics(SM_SWAPBUTTON) == 0 {
                    0x01
                } else {
                    0x02
                };
                windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(primary) < 0
            };
            if pressed {
                return self.paint_volume_preview(gfx);
            }
            self.release_volume(Some(y))?;
        }
        let within_bar = point.x >= self.bounds.left
            && point.x < self.bounds.right
            && point.y >= self.bounds.top
            && point.y < self.bounds.bottom;
        let within_island = point.y >= self.bounds.bottom
            && point.y < self.bounds.top + self.island_height as i32
            && if self.zoned() {
                self.triggers_span
                    .screen_pixels(self.bounds.left)
                    .contains(&point.x)
            } else {
                self.triggers_span.contains(x)
            };
        // Include the gap between the island and its popup so crossing it
        // does not close the popup before the pointer reaches its controls.
        let within_panel = self.panel.is_some()
            && x >= self.panel_span.left
            && x < self.panel_span.right
            && point.y >= self.bounds.bottom
            && point.y < PANEL_BOTTOM.load(Ordering::Relaxed);
        let inside = within_bar || within_island || within_panel;
        if self.zoned() {
            let zone = if within_bar {
                strip_zone(self.bounds, self.zone_entrance(), point)
            } else if inside {
                PointerZone::Center
            } else {
                PointerZone::Outside
            };
            if !self.revealed {
                if self.hide_until.is_some()
                    && zone == PointerZone::Center
                    && !muelle_shell::tracker::caption_button_at(point, self.bounds)
                {
                    return self.hover(gfx, true);
                }
                return Ok(());
            }
            let Some(sides) = revealed_at(Some(self.revealed_sides), zone) else {
                return self.hover(gfx, false);
            };
            let changed = self.revealed_sides != sides;
            self.revealed_sides = sides;
            if self.panel.is_some() && matches!(zone, PointerZone::Left | PointerZone::Right) {
                return self.dismiss_island(gfx);
            }
            if changed {
                self.place()?;
            }
            return Ok(());
        }
        if inside && self.hide_until.is_some() {
            self.hover(gfx, true)?;
        } else if !inside {
            self.hover(gfx, false)?;
        }
        Ok(())
    }

    /// Leaving the island sideways still stays inside the full-width bar window.
    pub fn pointer_moved(&mut self, gfx: &Gfx, x: f32, y: f32) -> Result<()> {
        if let Some(span) = self.volume_drag {
            self.queue_volume(span, y);
            return self.paint_volume_preview(gfx);
        }
        if self.zoned() {
            return self.pointer_left_at(
                gfx,
                POINT {
                    x: self.bounds.left + x as i32,
                    y: self.bounds.top + y as i32,
                },
            );
        }
        if self.ghost && !self.revealed && self.hide_until.is_none() {
            // The edge watcher applies the same dwell to the resting island.
            return Ok(());
        }
        let span = island_or_panel_span(
            x,
            y,
            (self.bounds.bottom - self.bounds.top) as f32,
            self.island_height,
            self.triggers_span,
            self.panel_span,
        );
        if self.panel.is_some() && (x < span.left || x >= span.right) {
            self.dismiss_island(gfx)?;
        }
        self.hover(gfx, true)
    }

    pub fn dismiss_island(&mut self, gfx: &Gfx) -> Result<()> {
        self.release_volume(None)?;
        if self.panel.take().is_some() {
            self.refresh(gfx)?;
        }
        Ok(())
    }

    /// The strip of the screen the bar covers for good, if any: none while it
    /// auto-hides or is hidden for a full-screen app.
    pub fn reserved_strip(&self) -> Option<RECT> {
        (self.visible && self.reserves()).then_some(self.bounds)
    }

    fn visible_sides(&self, presence: Presence, hiding: bool) -> RevealedSides {
        if presence == Presence::Full || hiding {
            self.revealed_sides
        } else {
            RevealedSides::default()
        }
    }

    fn desired_window_region(&self, presence: Presence, hiding: bool) -> Option<WindowRegion> {
        if self.zoned() {
            Some(
                self.capsule_layout
                    .region(self.visible_sides(presence, hiding)),
            )
        } else {
            resting_island_region(
                presence,
                hiding,
                self.island_hide_until.is_some(),
                self.triggers_span,
                self.island_height,
            )
        }
    }

    /// Positions the window and reserves (or frees) its strip of the screen.
    fn place(&mut self) -> Result<()> {
        self.sync_progress_watch();
        let bounds = self.bounds;
        let bar_height = bounds.bottom - bounds.top;
        if bar_height <= 0 {
            return Ok(());
        }
        let hidden = self.auto_hide && !self.revealed;
        if !self.visible || !self.auto_hide {
            self.hide_until = None;
            self.exit_animation = None;
        }
        let presence = self.presence_now();
        self.sync_island_fade(presence)?;
        let hiding = self.hide_until.is_some();
        let show_bar = self.visible && (presence != Presence::Hidden || hiding);
        let island_hiding = self.island_hide_until.is_some();
        let on_screen = show_bar || island_hiding;
        let full = presence == Presence::Full;
        let zoned = self.zoned();
        let sides = self.visible_sides(presence, hiding);
        if let Some([left, right]) = &self.capsule_layers {
            left.SetIsVisible(sides.left)?;
            right.SetIsVisible(sides.right)?;
        }
        let show_status = self.status_shown();
        if let Some(monitor) = &self.accessories {
            monitor.set_visible(show_status);
        }
        self.sync_monitor(show_status);
        if !on_screen {
            // Keep the hidden tree transparent. Restoring opacity here races
            // HWND hiding and flashes its last opaque frame on the next show.
            self.park_surface()?;
            if self.ghost {
                self.surface.SetOpacity(0.0)?;
            }
        } else if self.ghost {
            self.surface.StopAnimation(h!("Offset.Y"))?;
            self.surface.StopAnimation(h!("Opacity"))?;
            self.surface.SetOffset(Vector3::new(0.0, 0.0, 0.0))?;
            self.surface.SetOpacity(1.0)?;
            if !full && !hiding {
                self.park_surface()?;
            }
        } else if !self.auto_hide {
            self.reset_surface()?;
        }
        let region = self.desired_window_region(presence, hiding);
        // An auto-hiding or intelligent bar leaves the whole screen to the
        // windows.
        let strip = if self.reserves() { bar_height } else { 0 };
        #[cfg(test)]
        if strip > 0 {
            self.strip_reservations += 1;
        }
        if let Some(appbar) = &mut self.appbar {
            appbar.reserve(Edge::Top, bounds, strip);
        }
        // Hidden, the bar keeps no window on screen, not even a thin strip:
        // a strip along the top edge took the clicks meant for the close
        // button of maximized windows and for browser tabs. The edge watcher
        // watches the pointer instead.
        let hwnd = self.hwnd;
        let install = |region: Option<WindowRegion>| {
            install_window_region(hwnd, region.map(WindowRegion::create_handle))
        };
        let clip_ready = !zoned || sync_window_region(&mut self.window_region, region, install);
        let show = if on_screen && clip_ready {
            SWP_SHOWWINDOW
        } else {
            SWP_HIDEWINDOW
        };
        // SAFETY: Our own live window.
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                self.visual_height,
                SWP_NOACTIVATE | show,
            )?;
            if !zoned {
                sync_window_region(&mut self.window_region, region, install);
            }
            // Retry a failed zoned clip even while the pointer is idle over the island.
            if needs_pointer_timer(
                show_bar,
                presence,
                self.auto_hide,
                self.panel.is_some(),
                hiding,
                on_screen && !clip_ready,
            ) {
                // TrackMouseEvent may report a leave immediately after showing
                // a window. Check real geometry while revealed, including if
                // no further WM_MOUSEMOVE reaches this window.
                let interval = if zoned {
                    ZONED_POINTER_CHECK_MS
                } else {
                    POINTER_CHECK_MS
                };
                SetTimer(Some(self.hwnd), POINTER_TIMER, interval, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), POINTER_TIMER);
            }
            let _ = KillTimer(Some(self.hwnd), HIDE_ANIMATION_TIMER);
            if let Some(deadline) = self
                .hide_until
                .into_iter()
                .chain(self.island_hide_until)
                .min()
            {
                let wait = deadline.saturating_duration_since(Instant::now());
                SetTimer(
                    Some(self.hwnd),
                    HIDE_ANIMATION_TIMER,
                    (wait.as_millis() as u32).max(1),
                    None,
                );
            }
        }
        let island = (presence == Presence::IslandOnly).then(|| {
            let entrance = self.triggers_span.screen_pixels(bounds.left);
            RECT {
                left: entrance.start,
                right: entrance.end,
                bottom: bounds.top + self.island_height as i32,
                ..bounds
            }
        });
        let top = (self.visible && hidden).then(|| {
            if zoned {
                let entrance = self.zone_entrance().screen_pixels(bounds.left);
                RECT {
                    left: entrance.start,
                    right: entrance.end,
                    ..bounds
                }
            } else {
                bounds
            }
        });
        edge_watch::set_menu_bar(top, island);
        self.sync_outside_click_hook()
    }

    /// The edge watcher has verified the pointer's dwell on this bar.
    pub fn edge_rested(&mut self, gfx: &Gfx, point: POINT) -> Result<()> {
        if self.visible && self.auto_hide && !self.revealed {
            if self.zoned() {
                let entrance = RECT {
                    bottom: self
                        .bounds
                        .bottom
                        .max(self.bounds.top + self.island_height as i32),
                    ..self.bounds
                };
                let zone = strip_zone(entrance, self.zone_entrance(), point);
                if revealed_at(None, zone).is_none() {
                    edge_watch::retry_on_move(edge_watch::Target::MenuBar);
                    return Ok(());
                }
            }
            if muelle_shell::tracker::caption_button_at(point, self.bounds) {
                edge_watch::retry_on_move(edge_watch::Target::MenuBar);
            } else {
                if self.zoned()
                    && self
                        .island_hits
                        .iter()
                        .any(|hit| matches!(hit.action, IslandAction::Toggle(Island::Music)))
                {
                    self.panel = Some(Island::Music);
                }
                self.hover(gfx, true)?;
            }
        }
        Ok(())
    }

    fn sync_outside_click_hook(&mut self) -> Result<()> {
        let needed =
            self.panel_span.width() > 0.0 && self.visible && (!self.auto_hide || self.revealed);
        if needed && self.outside_click_hook.is_none() {
            // SAFETY: This process owns the hook procedure and installs the hook
            // only while the panel is visible.
            let module = unsafe { GetModuleHandleW(None)? };
            let hook = unsafe {
                SetWindowsHookExW(
                    WH_MOUSE_LL,
                    Some(outside_click_proc),
                    Some(module.into()),
                    0,
                )?
            };
            ISLAND_HOOK_WINDOW.store(self.hwnd.0 as isize, Ordering::Relaxed);
            self.outside_click_hook = Some(hook);
        } else if !needed && let Some(hook) = self.outside_click_hook {
            // SAFETY: This hook was installed by this MenuBar.
            unsafe { UnhookWindowsHookEx(hook)? };
            self.outside_click_hook = None;
            ISLAND_HOOK_WINDOW.store(0, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Privacy activity or the user's taskbar preferences changed.
    pub fn registry_changed(&mut self, gfx: &Gfx) -> Result<()> {
        if let Some(watch) = &mut self.privacy_watch {
            watch.arm();
        }
        if let Some(watch) = &mut self.settings_watch {
            watch.arm();
        }
        if let Some(watch) = &mut self.search_watch {
            watch.arm();
        }
        // Only reread registry-backed indicators. This event must not trigger
        // network/audio queries, provider scans or accessory discovery.
        self.status.buttons = status::taskbar_buttons();
        self.status.battery_percentage = status::battery_percentage_enabled();
        self.status.privacy = status::privacy();
        let Some(mut look) = self.look.clone() else {
            return self.refresh(gfx);
        };
        look.status = self.status;
        self.apply_look(gfx, look)
    }

    pub fn tick(&mut self, gfx: &Gfx) -> Result<()> {
        self.update_status();
        self.watch_network();
        let audio_changed = self
            .audio
            .as_ref()
            .is_some_and(AudioWatch::take_default_changed);
        let retry_due = Instant::now() >= self.accessory_retry_at
            && self.accessories.as_ref().is_none_or(|monitor| {
                monitor.snapshot().state == muelle_shell::accessories::State::Unavailable
            });
        if self.accessories_enabled && (audio_changed || retry_due) {
            self.restart_accessories(gfx)?;
        }
        self.schedule_next_minute();
        self.refresh(gfx)
    }

    /// Ordinary audio notifications do not requery network, registry, providers
    /// or clock. A default-device change still restarts enabled accessories.
    pub fn audio_changed(&mut self, gfx: &Gfx) -> Result<()> {
        let volume = self.audio.as_mut().and_then(AudioWatch::volume);
        if self
            .audio
            .as_ref()
            .is_some_and(AudioWatch::take_default_changed)
            && self.accessories_enabled
        {
            self.status.volume = volume;
            self.restart_accessories(gfx)?;
        }
        self.apply_volume_reading(gfx, volume)
    }

    fn apply_volume_reading(&mut self, gfx: &Gfx, volume: Option<Volume>) -> Result<()> {
        self.status.volume = volume;
        if let Some(look) = &self.look
            && same_volume_indicator(look.status.volume, volume)
        {
            if let Some(volume) = volume
                && let Some(controls) = &mut self.volume_controls
            {
                controls.paint(gfx, self.volume_target.unwrap_or(volume.percent))?;
            }
            if let Some(look) = &mut self.look {
                look.status.volume = volume;
            }
            return Ok(());
        }
        // Availability, mute and speaker-glyph changes affect layout, but do
        // not justify a new provider/process or system-status scan either.
        let Some(mut look) = self.look.clone() else {
            return self.refresh(gfx);
        };
        look.status.volume = volume;
        self.apply_look(gfx, look)
    }

    pub fn bluetooth_changed(&mut self, gfx: &Gfx) -> Result<()> {
        let on = self
            .bluetooth
            .as_ref()
            .is_some_and(bluetooth::Monitor::is_on);
        self.status.bluetooth_on = on;
        let Some(mut look) = self.look.clone() else {
            return self.refresh(gfx);
        };
        if look.status.bluetooth_on == on {
            return Ok(());
        }
        look.status.bluetooth_on = on;
        self.apply_look(gfx, look)
    }

    fn update_status(&mut self) {
        self.status = Status {
            bluetooth_on: self
                .bluetooth
                .as_ref()
                .is_some_and(bluetooth::Monitor::is_on),
            network: status::network(),
            volume: self.audio.as_mut().and_then(AudioWatch::volume),
            buttons: status::taskbar_buttons(),
            privacy: status::privacy(),
            battery_percentage: status::battery_percentage_enabled(),
        };
    }

    /// One wake-up per minute, aligned to the minute, instead of polling.
    fn schedule_next_minute(&self) {
        // SAFETY: Read-only query, then a timer on our own window (setting an
        // existing timer id replaces it).
        unsafe {
            let now: SYSTEMTIME = GetLocalTime();
            let elapsed = u32::from(now.wSecond) * 1000 + u32::from(now.wMilliseconds);
            SetTimer(
                Some(self.hwnd),
                CLOCK_TIMER,
                60_000 - elapsed.min(59_900) + 50,
                None,
            );
        }
    }

    pub fn click(&mut self, gfx: &Gfx, x: f32, y: f32) -> Result<()> {
        let resting_island = self.presence_now() == Presence::IslandOnly;
        if self.ghost && !self.revealed && !resting_island && self.hide_until.is_none() {
            // Only the fading island can still be on screen here.
            return Ok(());
        }
        if let Some(hit) = self.island_hits.iter().find(|hit| hit.at(x, y)) {
            match hit.action {
                IslandAction::Toggle(island) => {
                    if resting_island {
                        // An explicit click can open the panel immediately.
                        self.hover(gfx, true)?;
                    }
                    self.panel = toggle_panel(self.panel, island);
                    if self.panel.is_some() {
                        let mut track = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: self.hwnd,
                            dwHoverTime: 0,
                        };
                        // SAFETY: `track` describes our live window.
                        unsafe { TrackMouseEvent(&mut track)? };
                    }
                }
                IslandAction::Refresh => {
                    if let Some(usage) = &mut self.usage {
                        usage.refresh(usage::Refresh::Manual);
                    }
                }
                IslandAction::Media(control) => {
                    if let Some(media) = &self.media
                        && let Some(track) = self.look.as_ref().and_then(|look| look.track.as_ref())
                    {
                        media.send(track, control);
                    }
                    return Ok(());
                }
                IslandAction::Volume(_) => return Ok(()), // Handled on press/drag.
                IslandAction::Mute => {
                    if let Some(audio) = &mut self.audio {
                        audio.toggle_mute()?;
                    }
                    return self.audio_changed(gfx);
                }
                IslandAction::ScrollUsage(direction) => {
                    self.scroll_usage(f32::from(direction) * 72.0)?;
                    return Ok(());
                }
            }
            return self.refresh(gfx);
        }
        if resting_island || y >= (self.bounds.bottom - self.bounds.top) as f32 {
            return Ok(());
        }
        self.dismiss_island(gfx)?;
        let hit = self
            .zones
            .iter()
            .find(|(left, right, _)| x >= *left && x < *right);
        // Where a flyout opened from this item should hang: under its centre.
        let anchor = hit.map_or((0, 0), |(left, right, _)| {
            (
                self.bounds.left + ((left + right) / 2.0) as i32,
                self.bounds.bottom,
            )
        });
        match hit.map(|(_, _, action)| *action) {
            Some(Action::AccessoryIndicator) => {}
            Some(Action::QuickSettings) => launcher::open_quick_settings(),
            Some(Action::Notifications) => launcher::open_notifications(),
            Some(Action::Clock) => match &self.clock_target {
                // A target that no longer opens (an uninstalled app) falls
                // back to what the clock always opened.
                Some(target) if launcher::open(target).is_ok() => {}
                _ => launcher::open_notifications(),
            },
            Some(Action::Power) => launcher::open_power_settings(),
            Some(Action::Network) => launcher::open_network_list(),
            Some(Action::Sound) => launcher::open_sound(),
            Some(Action::Search) => launcher::open_search(),
            Some(Action::TaskView) => launcher::open_task_view(),
            Some(Action::Widgets) => launcher::open_widgets(),
            Some(Action::Bluetooth) => launcher::open_bluetooth(),
            Some(Action::SystemMonitor) => {
                let _ = launcher::open("taskmgr.exe");
            }
            Some(Action::HiddenIcons) => launcher::toggle_hidden_icons(anchor),
            Some(Action::InputLanguage) => {} // Opened outside the app borrow.
            // Windows' own pages list which apps use each device.
            Some(Action::Microphone) => {
                let _ = launcher::open("ms-settings:privacy-microphone");
            }
            Some(Action::Camera) => {
                let _ = launcher::open("ms-settings:privacy-webcam");
            }
            None => {}
        }
        Ok(())
    }

    pub fn input_switcher_anchor(&self, x: f32, y: f32) -> Option<POINT> {
        if y < 0.0 || y >= (self.bounds.bottom - self.bounds.top) as f32 {
            return None;
        }
        self.zones
            .iter()
            .find(|(left, right, action)| {
                x >= *left && x < *right && matches!(action, Action::InputLanguage)
            })
            .map(|(left, right, _)| POINT {
                x: self.bounds.left + ((left + right) * 0.5) as i32,
                y: self.bounds.bottom + 6,
            })
    }

    /// Renew the host sample without rebuilding an open island or its controls.
    pub fn refresh_backdrops(&self, gfx: &Gfx) -> Result<()> {
        gfx.refresh_backdrops(&self.surface)?;
        Ok(())
    }

    /// Redraws the bar if anything it shows has changed.
    pub fn refresh(&mut self, gfx: &Gfx) -> Result<()> {
        self.refresh_with_usage(gfx, usage::Refresh::Scheduled)
    }

    fn refresh_with_usage(&mut self, gfx: &Gfx, reason: usage::Refresh) -> Result<()> {
        let Some(monitor) = system::primary_monitor() else {
            return Ok(());
        };
        let active_providers = if self.usage.is_some() {
            usage::active_providers()
        } else {
            ActiveProviders::default()
        };
        let track = self.media.as_ref().and_then(media::Watch::snapshot);
        self.panel = available_panel(self.panel, track.is_some(), active_providers.any());
        if !active_providers.any() {
            if let Some(usage) = &mut self.usage {
                usage.mark_inactive();
            }
        } else if self.presence_now() != Presence::Hidden
            && let Some(usage) = &mut self.usage
        {
            usage.refresh(reason);
        }
        let appearance = appearance::menu_bar();
        let look = Look {
            accessories: self.accessories.as_ref().map_or([None, None], |monitor| {
                accessory_readings(&monitor.snapshot())
            }),
            monitor: self.monitor_lines(),
            language: strings::language(),
            screen: monitor.screen,
            scale: monitor.scale,
            material: appearance.material,
            // Without a background the ink is white and the island dark,
            // like its popups, whatever the Windows theme.
            light: appearance.light && !self.widgets.bare,
            glass: appearance.glass,
            zoned: self.zoned(),
            app_name: self.app_name.clone(),
            clock: system::clock_text(),
            input: system::input_language(),
            battery: system::battery(),
            status: self.status,
            widgets: self.widgets,
            usage: self
                .usage
                .as_ref()
                .map_or_else(Snapshot::default, UsageMonitor::snapshot),
            usage_enabled: self.usage.is_some() && active_providers.any(),
            active_providers,
            panel: self.panel,
            usage_refreshing: self.usage.as_ref().is_some_and(UsageMonitor::is_refreshing),
            track,
        };
        self.apply_look(gfx, look)
    }

    /// Samples only while the monitor is on and the bar shows; a hidden or
    /// auto-hidden bar costs nothing (issue #64).
    fn sync_monitor(&mut self, shown: bool) {
        let wanted = self.widgets.system_monitor && shown;
        if wanted == self.monitor.is_some() {
            return;
        }
        if wanted {
            let mut sampler = muelle_shell::monitor::Sampler::default();
            // Memory reads at once; CPU and network keep their last values
            // until the first sample, half a second later.
            self.monitor_reading.memory = sampler.sample().memory;
            self.monitor = Some(sampler);
            // SAFETY: A timer on our own window.
            unsafe {
                SetTimer(Some(self.hwnd), MONITOR_TIMER, MONITOR_FIRST_MS, None);
            }
        } else {
            self.monitor = None;
            // SAFETY: A timer on our own window.
            unsafe {
                let _ = KillTimer(Some(self.hwnd), MONITOR_TIMER);
            }
        }
    }

    /// A new sample of the system monitor; redraws if what it shows changed.
    pub fn monitor_tick(&mut self, gfx: &Gfx) -> Result<()> {
        let Some(sampler) = &mut self.monitor else {
            return Ok(());
        };
        self.monitor_reading = sampler.sample();
        // SAFETY: A timer on our own window; after the first sample, the
        // regular interval.
        unsafe {
            SetTimer(Some(self.hwnd), MONITOR_TIMER, MONITOR_EVERY_MS, None);
        }
        let Some(mut look) = self.look.clone() else {
            return self.refresh(gfx);
        };
        look.monitor = self.monitor_lines();
        self.apply_look(gfx, look)
    }

    /// CPU and memory, then upload and download, as the bar shows them.
    fn monitor_lines(&self) -> Option<[String; 4]> {
        if !self.widgets.system_monitor {
            return None;
        }
        let reading = self.monitor_reading;
        let comma = matches!(
            strings::language(),
            Language::Es | Language::Fr | Language::Pt
        );
        let share = |label: &str, value: Option<u8>| {
            value.map_or_else(|| format!("{label} —"), |value| format!("{label} {value}%"))
        };
        let speed = |arrow: char, value: Option<u64>| {
            value.map_or_else(
                || format!("{arrow} —"),
                |value| {
                    format!(
                        "{arrow} {}",
                        muelle_shell::monitor::format_rate(value, comma)
                    )
                },
            )
        };
        // Without network speed the lines stay empty, so a changing speed
        // does not redraw the bar.
        let essentials = self.widgets.essentials;
        Some([
            share("CPU", reading.cpu),
            share("RAM", reading.memory),
            if essentials {
                String::new()
            } else {
                speed('↑', reading.up)
            },
            if essentials {
                String::new()
            } else {
                speed('↓', reading.down)
            },
        ])
    }

    /// Track changes only need to update the contents of the island.
    pub fn media_changed(&mut self, gfx: &Gfx) -> Result<()> {
        let Some(mut look) = self.look.clone() else {
            return self.refresh(gfx);
        };
        let track = self.media.as_ref().and_then(media::Watch::snapshot);
        let timeline_only = look
            .track
            .as_ref()
            .zip(track.as_ref())
            .is_some_and(|(old, new)| {
                old.same_content(new) && old.timeline.is_some() == new.timeline.is_some()
            });
        look.track = track;
        if timeline_only {
            self.look = Some(look);
            return self.progress_tick(gfx);
        }
        self.panel = available_panel(self.panel, look.track.is_some(), look.usage_enabled);
        look.panel = self.panel;
        self.apply_look(gfx, look)
    }

    fn apply_look(&mut self, gfx: &Gfx, mut look: Look) -> Result<()> {
        look.zoned = self.ghost && look.widgets.bare;
        look.active_providers = look.usage.visible_providers(look.active_providers);
        look.usage_enabled &= look.active_providers.any();
        self.panel = available_panel(look.panel, look.track.is_some(), look.usage_enabled);
        look.panel = self.panel;
        if look.panel != Some(Island::Usage) {
            self.usage_scroll = 0.0;
        }
        let moved = self
            .look
            .as_ref()
            .is_some_and(|old| old.screen != look.screen || old.scale != look.scale);
        if look.panel != Some(Island::Music) || moved {
            self.release_volume(None)?;
        }
        if self.look.as_ref() == Some(&look) {
            return Ok(());
        }
        let moment = self
            .look
            .as_ref()
            .and_then(|old| usage_moment(&old.usage, &look.usage, &look.active_providers));
        self.build(gfx, &look)?;
        // A crowded bar can have no room even for a compact trigger. Do not
        // keep a ghost popup that reappears by itself when space returns.
        if look.panel.is_some() && self.panel_span.width() <= 0.0 {
            self.panel = None;
            look.panel = None;
            self.build(gfx, &look)?;
        }
        self.look = Some(look);
        self.sync_progress_watch();
        if let Some((provider, act)) = moment
            && let Some(&(_, x)) = self.usage_anchors.iter().find(|(p, _)| *p == provider)
        {
            self.cue = Some((act, x));
        }
        Ok(())
    }

    /// A moment worth the crab, found by the last update: the act and the
    /// screen x of the provider in the island. Taken once.
    /// The moment waiting for the mascot, with the point under the bar
    /// where it happens, or `None` when the bar does not show now.
    pub fn take_cue(&mut self) -> Option<(mascot::Act, Option<POINT>)> {
        let (act, x) = self.cue.take()?;
        // Resolve network anchors at delivery, when the visible sides are known.
        if self.zoned() && matches!(act, mascot::Act::Online | mascot::Act::Offline) {
            let point = self.anchor(Action::Network).or_else(|| {
                let island = self.triggers_span;
                (self.presence_now() != Presence::Hidden && island.width() > 0.0).then_some(POINT {
                    x: self.bounds.left + ((island.left + island.right) / 2.0) as i32,
                    y: self.bounds.top + self.island_height as i32,
                })
            });
            return Some((act, point));
        }
        // The moment sits under the island, on screen also while only the
        // island shows.
        let island_shown = self.presence_now() != Presence::Hidden;
        Some((
            act,
            island_shown.then_some(POINT {
                x,
                y: self.bounds.bottom,
            }),
        ))
    }

    /// The point under a status icon, while the bar shows: its centre and
    /// the bar's bottom edge, in screen pixels.
    fn anchor(&self, item: Action) -> Option<POINT> {
        if !self.status_shown() {
            return None;
        }
        let (left, right, _) = self.zones.iter().find(|(_, _, action)| *action == item)?;
        Some(POINT {
            x: self.bounds.left + ((left + right) / 2.0) as i32,
            y: self.bounds.bottom,
        })
    }

    /// Under the battery icon, for the battery's moments.
    pub fn battery_anchor(&self) -> Option<POINT> {
        self.anchor(Action::Power)
    }

    /// The network lost for a few seconds, or back after that: a moment
    /// under the network icon (see `take_cue`). Brief drops say nothing;
    /// the first reading is only the starting point.
    fn watch_network(&mut self) {
        let online = !matches!(self.status.network, Network::Offline);
        let before = self.online.replace(online);
        if online {
            if self.offline_told {
                self.network_cue(mascot::Act::Online);
            }
            self.offline_since = None;
            self.offline_told = false;
            // SAFETY: A timer on our own window.
            unsafe {
                let _ = KillTimer(Some(self.hwnd), NETWORK_TIMER);
            }
            return;
        }
        if before == Some(true) {
            self.offline_since = Some(Instant::now());
            // Fires a tick, which reads the network again.
            // SAFETY: A timer on our own window.
            unsafe {
                SetTimer(
                    Some(self.hwnd),
                    NETWORK_TIMER,
                    OFFLINE_GRACE.as_millis() as u32,
                    None,
                );
            }
        } else if !self.offline_told
            && self
                .offline_since
                .is_some_and(|since| since.elapsed() + Duration::from_millis(250) >= OFFLINE_GRACE)
        {
            self.offline_told = true;
            // SAFETY: A timer on our own window.
            unsafe {
                let _ = KillTimer(Some(self.hwnd), NETWORK_TIMER);
            }
            self.network_cue(mascot::Act::Offline);
        }
    }

    fn network_cue(&mut self, act: mascot::Act) {
        let x = self
            .zones
            .iter()
            .find(|(_, _, action)| *action == Action::Network)
            .map_or(
                self.bounds.left + (self.bounds.right - self.bounds.left) / 2,
                |(left, right, _)| self.bounds.left + ((left + right) / 2.0) as i32,
            );
        self.cue = Some((act, x));
    }

    fn build(&mut self, gfx: &Gfx, look: &Look) -> Result<()> {
        let c = &gfx.compositor;
        let scale = look.scale;
        let px = |logical: f32| (logical * scale).round();
        let screen = look.screen;
        let (width, height) = ((screen.right - screen.left) as f32, px(HEIGHT));
        let island_height = look.island_height(height);
        let panel_bottom = height + px(PANEL_GAP) + px(look.panel_height());
        let visual_height = if look.panel.is_some() {
            panel_bottom + px(PANEL_SHADOW_MARGIN)
        } else {
            island_height
        };
        let ink = if look.light { Ink::Dark } else { Ink::Light };

        let root = c.CreateContainerVisual()?;
        root.SetSize(Vector2 {
            X: width,
            Y: visual_height,
        })?;
        let layers = root.Children()?;
        self.capsule_layout = CapsuleLayout::default();
        let (left_layers, right_layers) = if look.zoned {
            let left = c.CreateContainerVisual()?;
            let right = c.CreateContainerVisual()?;
            for side in [&left, &right] {
                side.SetSize(Vector2::new(width, height))?;
                side.SetIsVisible(false)?;
                layers.InsertAtTop(side)?;
            }
            let collections = (left.Children()?, right.Children()?);
            self.capsule_layers = Some([left, right]);
            collections
        } else {
            self.capsule_layers = None;
            (layers.clone(), layers.clone())
        };
        // The island has its own root, so the ghost bar can hide the strip
        // while the island stays.
        let island_root = c.CreateContainerVisual()?;
        island_root.SetSize(Vector2 {
            X: width,
            Y: visual_height,
        })?;
        let island_layers = island_root.Children()?;
        // Without transparency the bar is solid, as macOS draws it with
        // "Reduce transparency".
        let fills = strip_fills(gfx, look)?;
        for brush in &fills {
            let fill = c.CreateSpriteVisual()?;
            fill.SetSize(Vector2 {
                X: width,
                Y: height,
            })?;
            fill.SetBrush(brush)?;
            layers.InsertAtTop(&fill)?;
        }

        let place = |layers: &windows::UI::Composition::VisualCollection,
                     item: &LabelBrush,
                     left: f32|
         -> Result<SpriteVisual> {
            let visual = c.CreateSpriteVisual()?;
            visual.SetSize(Vector2 {
                X: item.width,
                Y: item.height,
            })?;
            let offset = Vector3 {
                X: left.round(),
                Y: ((height - item.height) / 2.0).round(),
                Z: 0.0,
            };
            visual.SetOffset(offset)?;
            visual.SetBrush(&item.brush)?;
            if look.widgets.bare && !look.zoned {
                let size = Vector2::new(item.width, item.height);
                add_halo(c, layers, &item.brush.cast()?, size, offset, look)?;
            }
            layers.InsertAtTop(&visual)?;
            Ok(visual)
        };

        self.zones.clear();
        let mut left_end = px(LEFT_MARGIN);
        if !look.app_name.is_empty() {
            let name = gfx.text_brush_fitted(
                &look.app_name,
                scale,
                13.0,
                true,
                ink,
                px(220.0).min(width * 0.22),
            )?;
            place(&left_layers, &name, px(LEFT_MARGIN))?;
            left_end += name.width;
        }

        // Right side, from the edge inward.
        let slop = px(HIT_SLOP);
        let right_margin = if look.zoned {
            BARE_RIGHT_MARGIN
        } else {
            RIGHT_MARGIN
        };
        let mut right = width - px(right_margin);
        let status = look.status;
        let symbol = |layers: &[(char, f32)]| gfx.glyph_brush(layers, scale, ink);
        let mut items: Vec<(LabelBrush, Action)> = Vec::new();
        // Checked at each push, not filtered afterwards: the battery and
        // accessory labels remember indexes into `items`.
        let wanted = |action: Action| !look.widgets.essentials || essential(action);
        // The notification bell, when chosen, takes the very end of the bar.
        if look.widgets.notifications && wanted(Action::Notifications) {
            items.push((
                symbol(&[(glyph::NOTIFICATIONS, 1.0)])?,
                Action::Notifications,
            ));
        }
        items.push((
            gfx.text_brush(&look.clock, scale, look.zoned, ink)?,
            Action::Clock,
        ));
        items.push((gfx.control_center_brush(scale, ink)?, Action::QuickSettings));
        // Privacy indicators, as the Windows tray shows them: something is
        // using the microphone or camera right now.
        let privacy = status.privacy;
        for (on, glyph, action) in [
            (privacy.microphone, glyph::MICROPHONE, Action::Microphone),
            (privacy.camera, glyph::CAMERA, Action::Camera),
        ] {
            if on {
                items.push((symbol(&[(glyph, 1.0)])?, action));
            }
        }
        if status.buttons.search && wanted(Action::Search) {
            items.push((symbol(&[(glyph::SEARCH, 1.0)])?, Action::Search));
        }
        if status.bluetooth_on {
            items.push((symbol(&[(glyph::BLUETOOTH, 1.0)])?, Action::Bluetooth));
        }
        // The control center already opens all of quick settings: the network
        // icon goes straight to the list of networks (issue #55).
        match status.network {
            Network::Wifi { bars, has_internet } => {
                items.push((
                    gfx.wifi_brush(bars, has_internet, scale, ink)?,
                    Action::Network,
                ));
            }
            Network::Ethernet => {
                items.push((symbol(&[(glyph::ETHERNET, 1.0)])?, Action::Network));
            }
            Network::Offline => {
                items.push((symbol(&[(glyph::OFFLINE, 1.0)])?, Action::Network));
            }
        }
        if let Some(volume) = status.volume {
            let speaker = if volume.muted {
                glyph::MUTE
            } else {
                glyph::VOLUME[usize::from(volume.level.min(3))]
            };
            items.push((symbol(&[(speaker, 1.0)])?, Action::Sound));
        }
        let mut battery_label = None;
        if let Some(battery) = look.battery {
            if status.battery_percentage {
                battery_label = Some((
                    items.len(),
                    gfx.text_brush(&format!("{}%", battery.percent), scale, false, ink)?,
                ));
            }
            items.push((
                gfx.battery_brush(battery.percent, battery.charging, battery.saver, scale, ink)?,
                Action::Power,
            ));
        }
        let mut accessory_labels = Vec::new();
        for device in look.accessories.into_iter().flatten().rev() {
            let value = device
                .percent
                .map_or_else(|| "—".to_owned(), |percent| format!("{percent}%"));
            accessory_labels.push((items.len(), gfx.text_brush(&value, scale, false, ink)?));
            let icon = match device.kind {
                AccessoryKind::Mouse => glyph::MOUSE,
                AccessoryKind::Headset => glyph::HEADSET,
            };
            items.push((symbol(&[(icon, 1.0)])?, Action::AccessoryIndicator));
        }
        if let Some(language) = &look.input {
            items.push((
                gfx.text_brush(language, scale, false, ink)?,
                Action::InputLanguage,
            ));
        }
        // The notification area (antivirus, OneDrive, apps in the background)
        // lives only in the hidden taskbar; this arrow opens its flyout.
        if wanted(Action::HiddenIcons) {
            items.push((symbol(&[(glyph::HIDDEN_ICONS, 1.0)])?, Action::HiddenIcons));
        }
        if status.buttons.task_view && wanted(Action::TaskView) {
            items.push((symbol(&[(glyph::TASK_VIEW, 1.0)])?, Action::TaskView));
        }
        if status.buttons.widgets && wanted(Action::Widgets) {
            items.push((symbol(&[(glyph::WIDGETS, 1.0)])?, Action::Widgets));
        }
        for (index, (item, action)) in items.iter().enumerate() {
            let zone_right = right;
            if let Some((_, label)) = battery_label
                .iter()
                .chain(accessory_labels.iter())
                .find(|(item_index, _)| *item_index == index)
            {
                right -= label.width;
                place(&right_layers, label, right)?;
                right -= px(6.0);
            }
            let left = right - item.width;
            place(&right_layers, item, left)?;
            // The battery icon and its percentage form one control.
            // Accessory readings are passive status indicators, not controls.
            if *action != Action::AccessoryIndicator {
                self.zones.push((left - slop, zone_right + slop, *action));
            }
            right = left - px(ITEM_GAP);
        }

        // The system monitor (issue #64), left of the status icons.
        if let Some(lines) = &look.monitor {
            // "Only the essentials" keeps CPU over memory, without speeds.
            let columns: &[usize] = if look.widgets.essentials {
                &[0]
            } else {
                &[1, 0]
            };
            let cpu_ram = if look.zoned {
                Some(gfx.text_brush_sized(
                    &format!("{} · {}", lines[0], lines[1]),
                    scale,
                    BARE_MONITOR_FONT_SIZE,
                    false,
                    ink,
                )?)
            } else {
                None
            };
            let first_column = usize::from(cpu_ram.is_some());
            let brushes = lines[first_column * 2..columns.len() * 2]
                .iter()
                .map(|line| gfx.text_brush_sized(line, scale, MONITOR_FONT, false, ink))
                .collect::<Result<Vec<_>>>()?;
            if self.monitor_widths.scale != scale {
                self.monitor_widths = MonitorWidths {
                    scale,
                    ..MonitorWidths::default()
                };
            }
            for (column, pair) in brushes.chunks(2).enumerate() {
                let widest = pair.iter().map(|brush| brush.width).fold(0.0, f32::max);
                let index = first_column + column;
                let width = &mut self.monitor_widths.stacked[index];
                *width = width.max(widest);
            }
            if let Some(brush) = &cpu_ram {
                let width = &mut self.monitor_widths.combined_cpu_ram;
                *width = width.max(brush.width);
            }
            let zone_right = right;
            let mut left = right;
            for &column in columns {
                if column == 0
                    && let Some(brush) = &cpu_ram
                {
                    left -= self.monitor_widths.combined_cpu_ram;
                    place(&right_layers, brush, left)?;
                    continue;
                }
                left -= self.monitor_widths.stacked[column];
                let start = (column - first_column) * 2;
                for (row, brush) in brushes[start..start + 2].iter().enumerate() {
                    let top = if row == 0 {
                        height / 2.0 - brush.height + px(MONITOR_TUCK)
                    } else {
                        height / 2.0 - px(MONITOR_TUCK)
                    };
                    let visual = c.CreateSpriteVisual()?;
                    visual.SetSize(Vector2 {
                        X: brush.width,
                        Y: brush.height,
                    })?;
                    let offset = Vector3 {
                        X: left.round(),
                        Y: top.round(),
                        Z: 0.0,
                    };
                    visual.SetOffset(offset)?;
                    visual.SetBrush(&brush.brush)?;
                    if look.widgets.bare && !look.zoned {
                        let size = Vector2::new(brush.width, brush.height);
                        add_halo(c, &right_layers, &brush.brush.cast()?, size, offset, look)?;
                    }
                    right_layers.InsertAtTop(&visual)?;
                }
                if column == 1 {
                    left -= px(MONITOR_COLUMN_GAP);
                }
            }
            self.zones
                .push((left - slop, zone_right + slop, Action::SystemMonitor));
            right = left - px(ITEM_GAP);
        }

        if look.zoned {
            let radius = BARE_ISLAND_RADIUS * scale;
            if left_end > px(LEFT_MARGIN) {
                let span = Span {
                    left: (px(LEFT_MARGIN) - slop).max(0.0),
                    right: (left_end + slop).min(width),
                };
                self.capsule_layout.left = RegionPart::rounded(span, 0.0, height, radius);
            }
            let span = Span {
                left: (right + px(ITEM_GAP) - slop).max(0.0),
                right: (width - px(right_margin) + slop).min(width),
            };
            self.capsule_layout.right = RegionPart::rounded(span, 0.0, height, radius);
            for (layers, part) in [
                (&left_layers, self.capsule_layout.left),
                (&right_layers, self.capsule_layout.right),
            ] {
                if let Some(part) = part {
                    let r = part.rect;
                    let (fill, _) = retained_rounded_box(
                        c,
                        layers,
                        r.left as f32,
                        r.top as f32,
                        (r.right - r.left) as f32,
                        (r.bottom - r.top) as f32,
                        part.radius,
                        BARE_ISLAND,
                        None,
                    )?;
                    layers.Remove(&fill)?;
                    layers.InsertAtBottom(&fill)?;
                }
            }
        }

        self.island_hits.clear();
        self.volume_controls = None;
        self.progress_controls = None;
        self.usage_viewport = None;
        self.triggers_span = Span::default();
        self.panel_span = Span::default();
        self.draw_island(gfx, look, &island_layers, width, height, left_end, right)?;
        self.island_height = if self.triggers_span.width() > 0.0 {
            island_height
        } else {
            height
        };
        if let Some(drag) = self.volume_drag {
            let new_span = self.island_hits.iter().find_map(|hit| match hit.action {
                IslandAction::Volume(span) => Some(span),
                _ => None,
            });
            if new_span != Some(drag) {
                self.release_volume(None)?;
            }
        }
        self.surface.SetSize(Vector2 {
            X: width,
            Y: visual_height,
        })?;
        for (layer, content) in [
            (&self.strip_layer, &root),
            (&self.island_layer, &island_root),
        ] {
            layer.SetSize(Vector2 {
                X: width,
                Y: visual_height,
            })?;
            let children = layer.Children()?;
            children.RemoveAll()?;
            children.InsertAtTop(content)?;
        }
        self.visual_height = if self.panel_span.width() > 0.0 {
            visual_height
        } else {
            self.island_height
        } as i32;
        self.bounds = RECT {
            bottom: screen.top + height as i32,
            ..screen
        };
        TRIGGERS_LEFT.store(
            screen.left + self.triggers_span.left as i32,
            Ordering::Relaxed,
        );
        TRIGGERS_RIGHT.store(
            screen.left + self.triggers_span.right as i32,
            Ordering::Relaxed,
        );
        PANEL_LEFT.store(screen.left + self.panel_span.left as i32, Ordering::Relaxed);
        PANEL_RIGHT.store(
            screen.left + self.panel_span.right as i32,
            Ordering::Relaxed,
        );
        BAR_TOP.store(screen.top, Ordering::Relaxed);
        BAR_BOTTOM.store(screen.top + height as i32, Ordering::Relaxed);
        TRIGGERS_BOTTOM.store(screen.top + self.island_height as i32, Ordering::Relaxed);
        let clickable_height = if self.panel_span.width() > 0.0 {
            panel_bottom
        } else {
            height
        };
        PANEL_BOTTOM.store(screen.top + clickable_height as i32, Ordering::Relaxed);
        self.place()
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "The island is laid out between the measured bar items"
    )]
    fn draw_island(
        &mut self,
        gfx: &Gfx,
        look: &Look,
        layers: &VisualCollection,
        width: f32,
        bar_height: f32,
        left_end: f32,
        right_start: f32,
    ) -> Result<()> {
        let px = |logical: f32| (logical * look.scale).round();
        let style = look.widgets.island_style;
        let cover = if let Some(track) = &look.track {
            let cached =
                self.cover
                    .as_ref()
                    .is_some_and(|(source, _)| match (source, &track.artwork) {
                        (Some(old), Some(new)) => Arc::ptr_eq(old, new),
                        (None, None) => true,
                        _ => false,
                    });
            if !cached {
                self.cover = media::artwork(track)
                    .map(|image| {
                        let surface = gfx.image_surface(image)?;
                        Ok::<_, anyhow::Error>((
                            track.artwork.clone(),
                            gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?,
                        ))
                    })
                    .transpose()?;
            }
            self.cover.as_ref().map(|(_, brush)| brush.clone())
        } else {
            self.cover = None;
            None
        };
        let prepared_music = if style == IslandStyle::Bubble
            && let Some(track) = &look.track
        {
            Some(prepare_bubble_music(gfx, look, track)?)
        } else {
            None
        };
        let triggers = match style {
            IslandStyle::Notch => trigger_layout(
                Span {
                    left: left_end + px(8.0),
                    right: right_start - px(8.0),
                },
                width / 2.0,
                look.scale,
                look.track.is_some(),
                look.active_providers.count(),
                look.zoned,
            ),
            IslandStyle::Bubble => bubble_trigger_layout(
                Span {
                    left: left_end + px(8.0),
                    right: right_start,
                },
                look.scale,
                prepared_music.as_ref().map(|music| music.width),
                look.active_providers.count(),
            ),
        };
        let [(_, music), (_, usage)] = triggers;
        let (Some(first), Some(last)) = (music.or(usage), usage.or(music)) else {
            return Ok(());
        };
        self.triggers_span = Span {
            left: first.left,
            right: last.right,
        };
        let geometry = IslandGeometry::new(self.triggers_span, 0.0, look);
        let (compact, compact_height, divider_inset) = match style {
            IslandStyle::Notch => {
                let height = look.island_height(bar_height);
                (
                    island_surface(gfx, look, layers, geometry, height)?,
                    height,
                    if look.zoned {
                        (height - px(BARE_NOTCH_DIVIDER_HEIGHT)) / 2.0
                    } else {
                        px(7.0)
                    },
                )
            }
            IslandStyle::Bubble => {
                let (surface, pill_height) =
                    bubble_surface(gfx, look, layers, geometry, bar_height)?;
                (surface, pill_height, px(5.0))
            }
        };
        if look.zoned {
            let (top, radius) = match style {
                IslandStyle::Notch => (0.0, BARE_ISLAND_RADIUS * look.scale),
                IslandStyle::Bubble => (px(BUBBLE_VERTICAL_INSET), compact_height / 2.0),
            };
            self.capsule_layout.island =
                RegionPart::rounded(self.triggers_span, top, compact_height, radius).map(|part| {
                    RegionPart {
                        square_top: style == IslandStyle::Notch,
                        ..part
                    }
                });
        }
        let divider = music
            .zip(usage)
            .map(|(music, usage)| ((music.right + usage.left) / 2.0).round());
        let c = &gfx.compositor;
        if let Some(x) = divider {
            add_visual(
                c,
                &compact,
                &c.CreateColorBrushWithColor(geometry.border())?.cast()?,
                x - self.triggers_span.left,
                divider_inset,
                px(1.0).max(1.0),
                (compact_height - 2.0 * divider_inset).max(1.0),
            )?;
        }
        let providers = island_providers(look);
        for (kind, span) in triggers {
            let Some(span) = span else {
                continue;
            };
            let geometry = IslandGeometry::new(span, 0.0, look);
            let segment = c.CreateContainerVisual()?;
            segment.SetSize(Vector2::new(geometry.width, compact_height))?;
            segment.SetOffset(Vector3::new(span.left - self.triggers_span.left, 0.0, 0.0))?;
            let content = segment.Children()?;
            compact.InsertAtTop(&segment)?;
            match kind {
                Island::Music => {
                    if let Some(track) = &look.track {
                        match style {
                            IslandStyle::Notch => draw_compact_player(
                                gfx,
                                &content,
                                geometry,
                                geometry.width,
                                compact_height,
                                track,
                                cover.as_ref(),
                            )?,
                            IslandStyle::Bubble => {
                                if let Some(prepared) = &prepared_music {
                                    draw_bubble_player(
                                        gfx,
                                        &content,
                                        geometry,
                                        compact_height,
                                        track,
                                        prepared,
                                        cover.as_ref(),
                                    )?;
                                }
                            }
                        }
                    }
                }
                Island::Usage => {
                    let anchors = match style {
                        IslandStyle::Notch => {
                            draw_compact_usage(gfx, &content, geometry, compact_height, &providers)?
                        }
                        IslandStyle::Bubble => {
                            draw_bubble_usage(gfx, &content, geometry, compact_height, &providers)?
                        }
                    };
                    self.usage_anchors = anchors
                        .into_iter()
                        .map(|(provider, x)| (provider, look.screen.left + (span.left + x) as i32))
                        .collect();
                }
            }
            let (left, right) = match kind {
                Island::Music => (span.left, divider.unwrap_or(span.right)),
                Island::Usage => (divider.unwrap_or(span.left), span.right),
            };
            self.island_hits.push(IslandHit {
                bounds: (left, 0.0, right, bar_height.max(compact_height)),
                action: IslandAction::Toggle(kind),
            });
            if look.panel != Some(kind) {
                continue;
            }
            let desired = px(match kind {
                Island::Music => 360.0,
                Island::Usage => ISLAND_EXPANDED_WIDTH,
            });
            self.panel_span = match style {
                IslandStyle::Notch => panel_span(span, width, desired),
                IslandStyle::Bubble => bubble_panel_span(self.triggers_span, width, desired),
            };
            let volume_span = if kind == Island::Music && look.status.volume.is_some() {
                let (music, volume) = volume_popover_layout(self.panel_span, width, look.scale);
                self.panel_span = music;
                Some(volume)
            } else {
                None
            };
            let geometry = IslandGeometry::new(self.panel_span, bar_height + px(PANEL_GAP), look);
            let height = px(look.panel_height());
            let theme = Theme::for_popup(look.light, look.glass);
            let content = popup_surface(gfx, theme, layers, geometry, height, px(PANEL_RADIUS))?;
            if look.zoned {
                self.capsule_layout.panel =
                    RegionPart::rounded(self.panel_span, geometry.top, height, px(PANEL_RADIUS));
            }
            match kind {
                Island::Music => {
                    if let Some(track) = &look.track {
                        self.draw_player(gfx, &content, geometry, track, cover.as_ref())?;
                    }
                    if let Some(span) = volume_span {
                        let volume_geometry = IslandGeometry::new(span, geometry.top, look);
                        let volume_content = popup_surface(
                            gfx,
                            theme,
                            layers,
                            volume_geometry,
                            height,
                            span.width() / 2.0,
                        )?;
                        self.draw_volume_capsule(gfx, &volume_content, volume_geometry)?;
                        if look.zoned {
                            self.capsule_layout.volume =
                                RegionPart::rounded(span, geometry.top, height, span.width() / 2.0);
                        }
                        // Keep the small bridge between surfaces interactive,
                        // so moving towards the volume does not dismiss it.
                        self.panel_span.left = self.panel_span.left.min(span.left);
                        self.panel_span.right = self.panel_span.right.max(span.right);
                    }
                }
                Island::Usage => {
                    self.draw_usage_panel(gfx, &content, geometry, height, look, &providers)?
                }
            }
        }
        Ok(())
    }

    fn draw_usage_panel(
        &mut self,
        gfx: &Gfx,
        content: &VisualCollection,
        geometry: IslandGeometry,
        island_height: f32,
        look: &Look,
        providers: &[IslandProvider<'_>],
    ) -> Result<()> {
        let c = &gfx.compositor;
        let scale = geometry.scale;
        let px = |v| geometry.px(v);
        let island_width = geometry.width;
        let ink = geometry.ink();
        let title = strings::text();
        add_panel_text(
            gfx,
            content,
            title.usage_title,
            px(28.0),
            px(15.0),
            scale,
            13.0,
            true,
            ink,
            0.94,
        )?;
        let button_text = if look.usage_refreshing {
            title.usage_refreshing
        } else {
            title.usage_refresh
        };
        let button_label = gfx.text_brush_sized(button_text, scale, 11.5, true, ink)?;
        let button_width = button_label.width + px(20.0);
        let button_height = px(27.0);
        let button_left = island_width - px(23.0) - button_width;
        let button_top = px(8.0);
        add_rounded_box(
            c,
            content,
            button_left,
            button_top,
            button_width,
            button_height,
            px(13.5),
            geometry.control_fill(),
            Some(geometry.border()),
        )?;
        add_visual(
            c,
            content,
            &button_label.brush.cast()?,
            button_left + px(10.0),
            button_top + (button_height - button_label.height) / 2.0,
            button_label.width,
            button_label.height,
        )?;
        if !look.usage_refreshing {
            self.island_hits.push(geometry.hit(
                button_left,
                button_top,
                button_width,
                button_height,
                IslandAction::Refresh,
            ));
        }
        let body_top = px(42.0);
        let body_height = (island_height - body_top - px(26.0)).max(0.0);
        let content_height = px((look.panel_content_height() - 68.0).max(0.0));
        let maximum = ((content_height - body_height) / scale).max(0.0);
        self.usage_scroll = self.usage_scroll.clamp(0.0, maximum);
        let viewport = c.CreateContainerVisual()?;
        viewport.SetOffset(Vector3::new(0.0, body_top, 0.0))?;
        viewport.SetSize(Vector2::new(island_width, body_height))?;
        viewport.SetClip(&c.CreateInsetClip()?)?;
        let body = c.CreateContainerVisual()?;
        body.SetSize(Vector2::new(island_width, content_height))?;
        body.SetOffset(Vector3::new(0.0, -px(self.usage_scroll), 0.0))?;
        viewport.Children()?.InsertAtTop(&body)?;
        content.InsertAtTop(&viewport)?;
        let rows = body.Children()?;
        let mut row_top = 6.0;
        for (provider, usage, accent) in providers {
            let y = px(row_top);
            row_top += usage_row_height(usage);
            let inset = px(23.0);
            add_visual(
                c,
                &rows,
                &c.CreateColorBrushWithColor(geometry.border())?.cast()?,
                inset,
                y - px(6.0),
                island_width - 2.0 * inset,
                1.0,
            )?;
            draw_usage_row(
                gfx,
                &rows,
                *provider,
                usage,
                *accent,
                scale,
                look.light,
                island_width,
                y,
            )?;
        }
        let bounds = (
            geometry.left,
            geometry.top + body_top,
            geometry.left + island_width,
            geometry.top + body_top + body_height,
        );
        if maximum > 0.0 {
            let [up, down] = [
                ('\u{E70E}', body_top + px(2.0), -1),
                ('\u{E70D}', body_top + body_height - px(22.0), 1),
            ]
            .map(|(symbol, top, direction)| -> Result<SpriteVisual> {
                let label = gfx.glyph_brush(&[(symbol, 1.0)], scale * 0.7, ink)?;
                let visual = add_visual(
                    c,
                    content,
                    &label.brush.cast()?,
                    island_width - px(22.0) + (px(18.0) - label.width) / 2.0,
                    top + (px(20.0) - label.height) / 2.0,
                    label.width,
                    label.height,
                )?;
                self.island_hits.push(geometry.hit(
                    island_width - px(22.0),
                    top,
                    px(18.0),
                    px(20.0),
                    IslandAction::ScrollUsage(direction),
                ));
                Ok(visual)
            });
            self.usage_viewport = Some(UsageViewport {
                content: body,
                maximum,
                scale,
                bounds,
                up: up?,
                down: down?,
            });
            self.position_usage_body()?;
        }
        let newest_age = providers
            .iter()
            .filter_map(|provider| provider.1.age_minutes())
            .min();
        if let Some(minutes) = newest_age {
            let status = format!("{} {minutes} min", title.usage_updated);
            let label = gfx.text_brush_sized(&status, scale, 11.0, false, ink)?;
            add_visual(
                c,
                content,
                &label.brush.cast()?,
                (island_width - label.width) / 2.0,
                island_height - px(21.0),
                label.width,
                label.height,
            )?
            .SetOpacity(0.53)?;
        }
        Ok(())
    }

    fn draw_player(
        &mut self,
        gfx: &Gfx,
        layers: &VisualCollection,
        geometry: IslandGeometry,
        track: &Track,
        cover: Option<&CompositionSurfaceBrush>,
    ) -> Result<()> {
        let px = |v| geometry.px(v);
        let c = &gfx.compositor;
        let ink = geometry.ink();
        add_panel_text(
            gfx,
            layers,
            strings::text().media_title,
            px(26.0),
            px(15.0),
            geometry.scale,
            12.5,
            true,
            ink,
            0.88,
        )?;
        let chevron = gfx.glyph_brush(&[('', 0.72)], geometry.scale, ink)?;
        let x = geometry.width - px(22.0) - chevron.width;
        add_visual(
            c,
            layers,
            &chevron.brush.cast()?,
            x,
            (px(46.0) - chevron.height) / 2.0,
            chevron.width,
            chevron.height,
        )?;
        self.island_hits.push(geometry.hit(
            x - px(6.0),
            px(7.0),
            chevron.width + px(12.0),
            px(32.0),
            IslandAction::Toggle(Island::Music),
        ));
        let tile = px(34.0);
        if let Some(cover) = cover {
            add_visual(c, layers, &cover.cast()?, px(25.0), px(46.0), tile, tile)?;
        }
        let available = geometry.width - px(91.0);
        let title = gfx.text_brush_fitted(
            track_title(track),
            geometry.scale,
            13.0,
            true,
            ink,
            available,
        )?;
        let title_y = if track.artist.trim().is_empty() {
            px(63.0) - title.height / 2.0
        } else {
            px(47.0)
        };
        add_visual(
            c,
            layers,
            &title.brush.cast()?,
            px(69.0),
            title_y,
            title.width,
            title.height,
        )?;
        if !track.artist.trim().is_empty() {
            let artist =
                gfx.text_brush_fitted(&track.artist, geometry.scale, 11.0, false, ink, available)?;
            add_visual(
                c,
                layers,
                &artist.brush.cast()?,
                px(69.0),
                px(65.0),
                artist.width,
                artist.height,
            )?
            .SetOpacity(0.69)?;
        }
        let center = geometry.width / 2.0;
        for (control, glyph, enabled, offset) in [
            (MediaControl::Previous, '\u{E892}', track.previous, -44.0),
            (
                MediaControl::PlayPause,
                if track.playing {
                    '\u{E769}'
                } else {
                    '\u{E768}'
                },
                track.toggle,
                0.0,
            ),
            (MediaControl::Next, '\u{E893}', track.next, 44.0),
        ] {
            let size = px(34.0);
            let x = center + px(offset) - size / 2.0;
            let y = px(89.0);
            let main = matches!(control, MediaControl::PlayPause);
            if main {
                add_rounded_box(
                    c,
                    layers,
                    x,
                    y,
                    size,
                    size,
                    size / 2.0,
                    geometry.control_fill(),
                    Some(geometry.border()),
                )?;
            }
            let symbol = gfx.glyph_brush(&[(glyph, 1.0)], geometry.scale, ink)?;
            add_visual(
                c,
                layers,
                &symbol.brush.cast()?,
                x + (size - symbol.width) / 2.0,
                y + (size - symbol.height) / 2.0,
                symbol.width,
                symbol.height,
            )?
            .SetOpacity(if enabled { 1.0 } else { 0.30 })?;
            if enabled {
                self.island_hits
                    .push(geometry.hit(x, y, size, size, IslandAction::Media(control)));
            }
        }
        self.draw_progress(gfx, layers, geometry, track)?;
        Ok(())
    }

    fn draw_volume_capsule(
        &mut self,
        gfx: &Gfx,
        layers: &VisualCollection,
        geometry: IslandGeometry,
    ) -> Result<()> {
        let px = |v| geometry.px(v);
        let c = &gfx.compositor;
        let ink = geometry.ink();
        if let Some(volume) = self.status.volume {
            let symbol = if volume.muted {
                glyph::MUTE
            } else {
                glyph::VOLUME[volume.level.min(3) as usize]
            };
            let label = gfx.glyph_brush(&[(symbol, 1.0)], geometry.scale, ink)?;
            add_visual(
                c,
                layers,
                &label.brush.cast()?,
                (geometry.width - label.width) / 2.0,
                px(155.0) - label.height / 2.0,
                label.width,
                label.height,
            )?;
            self.island_hits.push(geometry.hit(
                (geometry.width - px(28.0)) / 2.0,
                px(141.0),
                px(28.0),
                px(28.0),
                IslandAction::Mute,
            ));
            let left = (geometry.width - px(4.0)) / 2.0;
            let top = px(34.0);
            let height = px(96.0);
            let percent = self.volume_target.unwrap_or(volume.percent);
            add_rounded_box(
                c,
                layers,
                left,
                top,
                px(4.0),
                height,
                px(2.0),
                geometry.control_fill(),
                None,
            )?;
            let (_, fill) = retained_rounded_box(
                c,
                layers,
                left,
                top,
                px(4.0),
                height,
                px(2.0),
                if geometry.light {
                    rgba(40, 48, 58, 220)
                } else {
                    rgba(240, 244, 250, 235)
                },
                None,
            )?;
            let label =
                gfx.text_brush_sized(&format!("{percent}%"), geometry.scale, 10.5, false, ink)?;
            let value = add_visual(
                c,
                layers,
                &label.brush.cast()?,
                (geometry.width - label.width) / 2.0,
                px(12.0),
                label.width,
                label.height,
            )?;
            value.SetOpacity(0.8)?;
            let mut controls = VolumeControls {
                fill,
                value,
                geometry,
                height,
                position_percent: None,
                label_percent: percent,
            };
            controls.move_to(percent)?;
            self.volume_controls = Some(controls);
            self.island_hits.push(geometry.hit(
                (geometry.width - px(28.0)) / 2.0,
                top - px(8.0),
                px(28.0),
                height + px(16.0),
                IslandAction::Volume(VolumeTrack {
                    center_x: geometry.left + geometry.width / 2.0,
                    top: geometry.top + top,
                    bottom: geometry.top + top + height,
                }),
            ));
        }
        Ok(())
    }

    fn draw_progress(
        &mut self,
        gfx: &Gfx,
        layers: &VisualCollection,
        geometry: IslandGeometry,
        track: &Track,
    ) -> Result<()> {
        let px = |v| geometry.px(v);
        let c = &gfx.compositor;
        let width = (geometry.width - px(52.0)).max(px(1.0));
        let Some(timeline) = track.timeline else {
            let text = if self.status.volume.is_some() {
                strings::text().media_no_progress
            } else {
                strings::text().media_no_output
            };
            let label =
                gfx.text_brush_fitted(text, geometry.scale, 11.0, false, geometry.ink(), width)?;
            add_visual(
                c,
                layers,
                &label.brush.cast()?,
                px(26.0),
                px(135.0),
                label.width,
                label.height,
            )?
            .SetOpacity(0.55)?;
            return Ok(());
        };
        add_rounded_box(
            c,
            layers,
            px(26.0),
            px(139.0),
            width,
            px(4.0),
            px(2.0),
            geometry.control_fill(),
            None,
        )?;
        let (_, fill) = retained_rounded_box(
            c,
            layers,
            px(26.0),
            px(139.0),
            width,
            px(4.0),
            px(2.0),
            if geometry.light {
                rgba(40, 48, 58, 220)
            } else {
                rgba(240, 244, 250, 235)
            },
            None,
        )?;
        let label = gfx.text_brush_sized("0:00", geometry.scale, 10.0, false, geometry.ink())?;
        let elapsed = add_visual(
            c,
            layers,
            &label.brush.cast()?,
            px(26.0),
            px(148.0),
            label.width,
            label.height,
        )?;
        let total = add_visual(
            c,
            layers,
            &label.brush.cast()?,
            px(26.0),
            px(148.0),
            label.width,
            label.height,
        )?;
        elapsed.SetOpacity(0.6)?;
        total.SetOpacity(0.6)?;
        let mut controls = ProgressControls {
            fill,
            elapsed,
            total,
            geometry,
            width,
            elapsed_seconds: None,
            total_seconds: None,
        };
        controls.paint(gfx, timeline, Instant::now())?;
        self.progress_controls = Some(controls);
        Ok(())
    }

    fn sync_progress_watch(&mut self) {
        let shown =
            self.panel == Some(Island::Music) && self.visible && (!self.auto_hide || self.revealed);
        if let Some(media) = &self.media {
            media.show_progress(shown);
        }
        let ticking = shown
            && self.progress_controls.is_some()
            && self
                .look
                .as_ref()
                .and_then(|look| look.track.as_ref())
                .and_then(|track| track.timeline)
                .is_some_and(|timeline| timeline.advancing(Instant::now()));
        if ticking != self.progress_timer {
            // This timer only advances cached position/time labels while the
            // player popup is visible. No API calls, artwork reads or polling.
            unsafe {
                self.progress_timer = if ticking {
                    SetTimer(Some(self.hwnd), PROGRESS_TIMER, 1000, None) != 0
                } else {
                    let _ = KillTimer(Some(self.hwnd), PROGRESS_TIMER);
                    false
                };
            }
        }
    }

    pub fn progress_tick(&mut self, gfx: &Gfx) -> Result<()> {
        if let Some(timeline) = self
            .look
            .as_ref()
            .and_then(|look| look.track.as_ref())
            .and_then(|track| track.timeline)
            && let Some(controls) = &mut self.progress_controls
        {
            controls.paint(gfx, timeline, Instant::now())?;
        }
        self.sync_progress_watch();
        Ok(())
    }

    pub fn press_volume(&mut self, gfx: &Gfx, x: f32, y: f32) -> Result<()> {
        self.suppress_volume_release = false;
        let Some(hit) = self
            .island_hits
            .iter()
            .find(|hit| hit.at(x, y) && matches!(hit.action, IslandAction::Volume(_)))
        else {
            return Ok(());
        };
        let IslandAction::Volume(span) = hit.action else {
            return Ok(());
        };
        self.volume_drag = Some(span);
        // SAFETY: Capture only for a live slider drag, never to activate the bar.
        unsafe {
            SetCapture(self.hwnd);
        }
        self.queue_volume(span, y);
        self.paint_volume_preview(gfx)
    }

    pub fn scroll_usage_at(&mut self, point: POINT, delta: i16) -> Result<bool> {
        if !self.visible || (self.auto_hide && !self.revealed) || delta == 0 {
            return Ok(false);
        }
        let Some(viewport) = &self.usage_viewport else {
            return Ok(false);
        };
        let (x, y) = (
            (point.x - self.bounds.left) as f32,
            (point.y - self.bounds.top) as f32,
        );
        let (left, top, right, bottom) = viewport.bounds;
        if x < left || x >= right || y < top || y >= bottom {
            return Ok(false);
        }
        self.scroll_usage(-f32::from(delta) / 120.0 * 48.0)
    }

    fn scroll_usage(&mut self, delta: f32) -> Result<bool> {
        let Some(viewport) = &self.usage_viewport else {
            return Ok(false);
        };
        let next = (self.usage_scroll + delta).clamp(0.0, viewport.maximum);
        if next != self.usage_scroll {
            self.usage_scroll = next;
            self.position_usage_body()?;
        }
        Ok(true)
    }

    fn position_usage_body(&mut self) -> Result<()> {
        let Some(viewport) = &self.usage_viewport else {
            return Ok(());
        };
        let offset = (self.usage_scroll * viewport.scale).round();
        viewport
            .content
            .SetOffset(Vector3::new(0.0, -offset, 0.0))?;
        viewport
            .up
            .SetOpacity(if self.usage_scroll > 0.0 { 0.8 } else { 0.22 })?;
        viewport
            .down
            .SetOpacity(if self.usage_scroll < viewport.maximum {
                0.8
            } else {
                0.22
            })?;
        Ok(())
    }

    fn paint_volume_preview(&mut self, gfx: &Gfx) -> Result<()> {
        if let Some(percent) = self.volume_target
            && let Some(controls) = &mut self.volume_controls
        {
            controls.paint(gfx, percent)?;
        }
        Ok(())
    }

    fn queue_volume(&mut self, span: VolumeTrack, y: f32) {
        let percent = volume_percent(span, y);
        self.volume_target = Some(percent);
        if let Some(controls) = &mut self.volume_controls {
            // Optimistic GPU-only feedback is independent of endpoint latency.
            // Audio writes still proceed if device loss interrupts this paint;
            // the existing graphics heartbeat owns recovery.
            let _ = controls.move_to(percent);
        }
        if self.pending_volume == Some(percent)
            || (self.pending_volume.is_none()
                && self
                    .status
                    .volume
                    .is_some_and(|v| v.percent == percent && !v.muted))
        {
            return;
        }
        let arm = self.pending_volume.is_none();
        self.pending_volume = Some(percent);
        if arm {
            // At most 30 endpoint writes/second during dragging; no idle timer.
            unsafe {
                SetTimer(Some(self.hwnd), VOLUME_TIMER, 33, None);
            }
        }
    }

    pub fn flush_volume(&mut self) -> Result<()> {
        // SAFETY: Our own short-lived drag timer.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), VOLUME_TIMER);
        }
        if let Some(percent) = self.pending_volume.take()
            && let Some(audio) = &mut self.audio
        {
            audio.set_percent(percent)?;
        }
        Ok(())
    }

    pub fn release_volume(&mut self, y: Option<f32>) -> Result<bool> {
        let Some(span) = self.volume_drag.take() else {
            return Ok(y.is_some() && std::mem::take(&mut self.suppress_volume_release));
        };
        self.suppress_volume_release = true;
        if let Some(y) = y {
            self.queue_volume(span, y);
        }
        // Clear state first: ReleaseCapture posts a capture-lost notification.
        unsafe {
            if GetCapture() == self.hwnd {
                let _ = ReleaseCapture();
            }
        }
        if let Err(error) = self.flush_volume() {
            muelle_shell::log::line(&format!("ajustar volumen: {error:#}"));
        }
        self.volume_target = None;
        Ok(true)
    }
}

fn volume_percent(span: VolumeTrack, y: f32) -> u8 {
    let height = span.bottom - span.top;
    if height <= 0.0 || !y.is_finite() {
        return 0;
    }
    (((span.bottom - y) / height).clamp(0.0, 1.0) * 100.0).round() as u8
}

/// Prefer the player's right edge, then its left. On narrow displays reserve
/// space for the capsule instead of covering the progress or playback buttons.
fn volume_popover_layout(panel: Span, width: f32, scale: f32) -> (Span, Span) {
    let px = |value: f32| (value * scale).round();
    let margin = px(12.0).min(width / 10.0);
    let available = (width - margin * 2.0).max(0.0);
    let capsule = px(44.0).min(available / 3.0);
    let gap = px(8.0).min(available / 10.0);
    let player_width = panel.width().min((available - capsule - gap).max(0.0));
    let mut left = panel
        .left
        .clamp(margin, (width - margin - player_width).max(margin));
    let volume_left = if left + player_width + gap + capsule <= width - margin {
        left + player_width + gap
    } else if left - gap - capsule >= margin {
        left - gap - capsule
    } else {
        left = (width - margin - player_width - gap - capsule).max(margin);
        left + player_width + gap
    };
    (
        Span {
            left,
            right: left + player_width,
        },
        Span {
            left: volume_left,
            right: volume_left + capsule,
        },
    )
}

fn media_time(seconds: u64) -> String {
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

fn same_volume_indicator(old: Option<Volume>, new: Option<Volume>) -> bool {
    old.map(|v| (v.level, v.muted)) == new.map(|v| (v.level, v.muted))
}

#[derive(Clone, Copy)]
struct IslandGeometry {
    left: f32,
    top: f32,
    width: f32,
    scale: f32,
    light: bool,
    bare: bool,
}

impl IslandGeometry {
    fn new(span: Span, top: f32, look: &Look) -> Self {
        Self {
            left: span.left,
            top,
            width: span.width(),
            scale: look.scale,
            light: look.light,
            bare: look.zoned,
        }
    }

    fn hit(self, x: f32, y: f32, width: f32, height: f32, action: IslandAction) -> IslandHit {
        let left = self.left + x;
        let top = self.top + y;
        IslandHit {
            bounds: (left, top, left + width, top + height),
            action,
        }
    }

    fn px(self, logical: f32) -> f32 {
        (logical * self.scale).round()
    }
    fn ink(self) -> Ink {
        if self.light { Ink::Dark } else { Ink::Light }
    }
    fn control_fill(self) -> Color {
        if self.light {
            rgba(0, 0, 0, 18)
        } else {
            rgba(255, 255, 255, 27)
        }
    }
    fn border(self) -> Color {
        if self.light {
            rgba(35, 57, 83, 31)
        } else {
            rgba(223, 237, 255, 37)
        }
    }
}

fn toggle_panel(open: Option<Island>, clicked: Island) -> Option<Island> {
    (open != Some(clicked)).then_some(clicked)
}

fn available_panel(open: Option<Island>, music: bool, usage: bool) -> Option<Island> {
    open.filter(|island| match island {
        Island::Music => music,
        Island::Usage => usage,
    })
}

fn trigger_layout(
    bounds: Span,
    center: f32,
    scale: f32,
    music: bool,
    providers: usize,
    bare: bool,
) -> [(Island, Option<Span>); 2] {
    let px = |v: f32| (v * scale).round();
    let mut result = [(Island::Music, None), (Island::Usage, None)];
    let gap = if music && providers > 0 { px(8.0) } else { 0.0 };
    let music_width = if music {
        px(if bare { BARE_PLAYER_WIDTH } else { 214.0 })
    } else {
        0.0
    };
    let usage_width = match providers {
        0 => 0.0,
        count if bare => px(BARE_PROVIDER_WIDTH * count as f32),
        1 => px(ISLAND_SINGLE_WIDTH),
        count => px(134.0 * count as f32),
    };
    let total = music_width + usage_width;
    let available = (bounds.width() - gap).min(total);
    if total == 0.0 || available < px(if gap > 0.0 { 85.0 } else { 42.0 }) {
        return result;
    }
    let music_width = if gap > 0.0 {
        (available * music_width / total)
            .round()
            .clamp(px(42.0), available - px(42.0))
    } else if music {
        available
    } else {
        0.0
    };
    let left = (center - (available + gap) / 2.0)
        .clamp(
            bounds.left,
            (bounds.right - available - gap).max(bounds.left),
        )
        .round();
    if music {
        result[0].1 = Some(Span {
            left,
            right: left + music_width,
        });
    }
    if providers > 0 {
        result[1].1 = Some(Span {
            left: left + music_width + gap,
            right: left + available + gap,
        });
    }
    result
}

fn bubble_trigger_layout(
    bounds: Span,
    scale: f32,
    music_width: Option<f32>,
    providers: usize,
) -> [(Island, Option<Span>); 2] {
    let px = |v: f32| (v * scale).round();
    let mut result = [(Island::Music, None), (Island::Usage, None)];
    let music = music_width.is_some();
    let gap = if music && providers > 0 { px(8.0) } else { 0.0 };
    let music_width = music_width
        .map(|width| width.clamp(px(36.0), px(BUBBLE_MUSIC_WIDTH)))
        .unwrap_or(0.0);
    let usage_width = if providers > 0 {
        px(BUBBLE_USAGE_PADDING + BUBBLE_PROVIDER_WIDTH * providers as f32)
    } else {
        0.0
    };
    let total = music_width + usage_width;
    let available = (bounds.width() - gap).min(total);
    if total == 0.0 || available < px(if gap > 0.0 { 72.0 } else { 36.0 }) {
        return result;
    }
    let music_width = if gap > 0.0 {
        (available * music_width / total)
            .round()
            .clamp(px(36.0), available - px(36.0))
    } else if music {
        available
    } else {
        0.0
    };
    let left = (bounds.right - available - gap).max(bounds.left).round();
    if music {
        result[0].1 = Some(Span {
            left,
            right: left + music_width,
        });
    }
    if providers > 0 {
        result[1].1 = Some(Span {
            left: left + music_width + gap,
            right: left + available + gap,
        });
    }
    result
}

fn panel_span(trigger: Span, screen_width: f32, desired: f32) -> Span {
    let width = desired.max(trigger.width()).min(screen_width);
    let left = ((trigger.left + trigger.right - width) / 2.0)
        .clamp(0.0, screen_width - width)
        .round();
    Span {
        left,
        right: left + width,
    }
}

fn bubble_panel_span(anchor: Span, screen_width: f32, desired: f32) -> Span {
    let width = desired.max(anchor.width()).min(screen_width);
    let left = (anchor.right - width)
        .clamp(0.0, screen_width - width)
        .round();
    Span {
        left,
        right: left + width,
    }
}

type IslandProvider<'a> = (Provider, &'a ProviderUsage, Color);

fn island_providers(look: &Look) -> Vec<IslandProvider<'_>> {
    Provider::ALL
        .into_iter()
        .filter(|provider| look.active_providers.has(*provider))
        .map(|provider| {
            let accent = match provider {
                Provider::Claude => rgba(217, 119, 87, 255),
                Provider::Codex => rgba(135, 190, 245, 255),
                Provider::OpenCode => rgba(132, 196, 166, 255),
                Provider::Gemini => rgba(111, 146, 255, 255),
                Provider::Zai => rgba(79, 110, 247, 255),
            };
            (provider, look.usage.get(provider), accent)
        })
        .collect()
}

fn popup_surface(
    gfx: &Gfx,
    theme: Theme,
    layers: &VisualCollection,
    geometry: IslandGeometry,
    height: f32,
    radius: f32,
) -> Result<VisualCollection> {
    let c = &gfx.compositor;
    let size = Vector2::new(geometry.width, height);
    let panel = theme.panel(gfx, size, radius, geometry.scale)?;
    let popup = c.CreateContainerVisual()?;
    popup.SetSize(size)?;
    popup.SetOffset(Vector3::new(geometry.left.round(), geometry.top, 0.0))?;
    let children = popup.Children()?;
    let mask = c.CreateNineGridBrush()?;
    mask.SetSource(&gfx.rounded_mask(radius)?)?;
    mask.SetInsets(radius.ceil() + 1.0)?;
    children.InsertAtTop(&island_shadow(c, &mask.cast()?, size, geometry)?)?;
    children.InsertAtTop(&panel)?;
    layers.InsertAtTop(&popup)?;
    Ok(children)
}

fn island_shadow(
    c: &Compositor,
    mask: &CompositionBrush,
    size: Vector2,
    geometry: IslandGeometry,
) -> Result<SpriteVisual> {
    let shadow = c.CreateDropShadow()?;
    shadow.SetMask(mask)?;
    shadow.SetBlurRadius(geometry.px(12.0))?;
    shadow.SetOffset(Vector3::new(0.0, geometry.px(5.0), 0.0))?;
    shadow.SetColor(rgba(0, 0, 0, 92))?;
    let host = c.CreateSpriteVisual()?;
    host.SetSize(size)?;
    host.SetShadow(&shadow)?;
    Ok(host)
}

fn island_surface(
    gfx: &Gfx,
    look: &Look,
    layers: &VisualCollection,
    geometry: IslandGeometry,
    island_height: f32,
) -> Result<VisualCollection> {
    let c = &gfx.compositor;
    let px = |v| geometry.px(v);
    let island_left = geometry.left;
    let island_width = geometry.width;
    let scale = geometry.scale;
    let size = Vector2::new(island_width, island_height);
    let island = c.CreateContainerVisual()?;
    island.SetSize(size)?;
    island.SetOffset(Vector3::new(island_left.round(), 0.0, 0.0))?;
    let content = island.Children()?;
    let silhouette = gfx.island_mask(island_width, island_height, scale, look.zoned)?;
    content.InsertAtTop(&island_shadow(c, &silhouette.cast()?, size, geometry)?)?;

    let tint = match (look.widgets.bare, look.light, look.glass) {
        (true, _, _) => BARE_ISLAND,
        (false, true, true) => rgba(247, 249, 252, 243),
        (false, false, true) => rgba(7, 9, 13, 242),
        (false, true, false) => rgba(247, 249, 252, 255),
        (false, false, false) => rgba(10, 12, 17, 255),
    };
    // Without a background the island is opaque, not glass.
    if look.glass && !look.widgets.bare {
        // The backdrop cannot be used as a mask-brush source. Inset its
        // rounded clip so it stays entirely inside the curved silhouette.
        let inset = (12.0 * scale)
            .min(island_width / 5.0)
            .min(island_height / 2.0);
        let glass_width = island_width - inset * 2.0;
        let glass = c.CreateContainerVisual()?;
        glass.SetSize(Vector2 {
            X: glass_width,
            Y: island_height,
        })?;
        glass.SetOffset(Vector3 {
            X: inset,
            Y: 0.0,
            Z: 0.0,
        })?;
        let outline = c.CreateRoundedRectangleGeometry()?;
        let lift = px(22.0);
        outline.SetSize(Vector2 {
            X: glass_width,
            Y: island_height + lift,
        })?;
        outline.SetOffset(Vector2 { X: 0.0, Y: -lift })?;
        outline.SetCornerRadius(Vector2 { X: lift, Y: lift })?;
        glass.SetClip(&c.CreateGeometricClipWithGeometry(&outline)?)?;
        add_visual(
            c,
            &glass.Children()?,
            &c.CreateHostBackdropBrush()?.cast()?,
            0.0,
            0.0,
            glass_width,
            island_height,
        )?;
        content.InsertAtTop(&glass)?;
    }
    let masked = c.CreateMaskBrush()?;
    masked.SetSource(&c.CreateColorBrushWithColor(tint)?)?;
    masked.SetMask(&silhouette)?;
    add_visual(
        c,
        &content,
        &masked.cast()?,
        0.0,
        0.0,
        island_width,
        island_height,
    )?;

    layers.InsertAtTop(&island)?;
    Ok(content)
}

fn bubble_surface(
    gfx: &Gfx,
    look: &Look,
    layers: &VisualCollection,
    geometry: IslandGeometry,
    bar_height: f32,
) -> Result<(VisualCollection, f32)> {
    let c = &gfx.compositor;
    let inset_y = geometry.px(BUBBLE_VERTICAL_INSET);
    let pill_height = (bar_height - 2.0 * inset_y).max(1.0);
    let radius = pill_height / 2.0;
    let size = Vector2::new(geometry.width, pill_height);
    let bubble = c.CreateContainerVisual()?;
    bubble.SetSize(size)?;
    bubble.SetOffset(Vector3::new(geometry.left.round(), inset_y, 0.0))?;
    let content = bubble.Children()?;

    let tint = match (look.zoned, look.light, look.glass) {
        (true, _, _) => BARE_ISLAND,
        (false, true, true) => rgba(247, 249, 252, 243),
        (false, false, true) => rgba(7, 9, 13, 242),
        (false, true, false) => rgba(247, 249, 252, 255),
        (false, false, false) => rgba(10, 12, 17, 255),
    };
    if look.glass && !look.zoned {
        let glass = c.CreateContainerVisual()?;
        glass.SetSize(size)?;
        let outline = c.CreateRoundedRectangleGeometry()?;
        outline.SetSize(size)?;
        outline.SetCornerRadius(Vector2 {
            X: radius,
            Y: radius,
        })?;
        glass.SetClip(&c.CreateGeometricClipWithGeometry(&outline)?)?;
        add_visual(
            c,
            &glass.Children()?,
            &c.CreateHostBackdropBrush()?.cast()?,
            0.0,
            0.0,
            geometry.width,
            pill_height,
        )?;
        content.InsertAtTop(&glass)?;
    }
    add_rounded_box(
        c,
        &content,
        0.0,
        0.0,
        geometry.width,
        pill_height,
        radius,
        tint,
        None,
    )?;

    layers.InsertAtTop(&bubble)?;
    Ok((content, pill_height))
}

/// Draws the providers in the closed island and returns where each one's
/// figure sits (its centre, from the segment's left edge), for the crab.
/// The moment an AI plan's figure crosses a line worth showing, comparing
/// the previous update with this one: a limit reset to 0 % (after being in
/// use), past 90 %, or at 100 %. Nothing on the first figures after starting,
/// which have nothing to compare with.
fn usage_moment(
    old: &usage::Snapshot,
    new: &usage::Snapshot,
    active: &ActiveProviders,
) -> Option<(Provider, mascot::Act)> {
    Provider::ALL
        .into_iter()
        .filter(|p| active.has(*p))
        .find_map(|provider| {
            let previous = old.get(provider);
            let current = new.get(provider);
            if previous.state != State::Ready || current.state != State::Ready {
                return None;
            }
            let before_window = previous.main_window()?;
            let now_window = current.main_window()?;
            if before_window.minutes != now_window.minutes
                || before_window.label != now_window.label
            {
                return None;
            }
            if let (Some(before), Some(now)) = (previous.fetched_at, current.fetched_at)
                && now <= before
            {
                return None;
            }
            let before = before_window.used_percent;
            let now = now_window.used_percent;
            let act = if now >= 100 && before < 100 {
                mascot::Act::Exhausted
            } else if now >= 90 && before < 90 {
                mascot::Act::Nervous
            } else if now <= 1 && before >= 20 {
                mascot::Act::Fresh
            } else {
                return None;
            };
            Some((provider, act))
        })
}

fn draw_compact_usage(
    gfx: &Gfx,
    content: &VisualCollection,
    geometry: IslandGeometry,
    bar_height: f32,
    providers: &[IslandProvider<'_>],
) -> Result<Vec<(Provider, f32)>> {
    let c = &gfx.compositor;
    let px = |v| geometry.px(v);
    let ink = geometry.ink();
    let scale = geometry.scale;
    let bare = geometry.bare;
    let inset = if bare { 0.0 } else { px(13.0) };
    let half = (geometry.width - 2.0 * inset) / providers.len().max(1) as f32;
    if half < px(20.0) {
        let icon = gfx.glyph_brush(&[('\u{E9D9}', 1.0)], scale, ink)?;
        add_visual(
            c,
            content,
            &icon.brush.cast()?,
            (geometry.width - icon.width) / 2.0,
            (bar_height - icon.height) / 2.0,
            icon.width,
            icon.height,
        )?;
        let centre = geometry.width / 2.0;
        return Ok(providers
            .iter()
            .map(|(provider, ..)| (*provider, centre))
            .collect());
    }
    let icon_size = px(if bare { BARE_PROVIDER_LOGO_SIZE } else { 18.0 }) as u32;
    let with_ring = half
        >= px(if bare {
            BARE_PROVIDER_RING_MIN_WIDTH
        } else {
            84.0
        });
    let ring_size = Gfx::usage_ring_size(scale, bare);
    let max_gap = px(BARE_PROVIDER_ITEM_GAP);
    let show_labels = if bare { with_ring } else { half > px(106.0) };
    let mut labels = [const { None }; Provider::COUNT];
    if show_labels {
        for (slot, (_, usage, _)) in labels.iter_mut().zip(providers) {
            let has_reading = usage.main_window().is_some();
            let font = if bare {
                BARE_PROVIDER_LABEL_SIZE
            } else if has_reading {
                14.0
            } else {
                12.0
            };
            let label = gfx.text_brush_sized(&usage.compact_text(), scale, font, true, ink)?;
            let reserve = if bare {
                icon_size as f32 + ring_size
            } else {
                px(if has_reading { 74.0 } else { 50.0 })
            };
            let remaining = half - reserve - label.width;
            *slot = (remaining >= 0.0).then(|| {
                // Give the measured percentage the space it needs before adding gaps.
                let gap = (remaining / 2.0).min(max_gap);
                (label, gap)
            });
        }
    }
    let bare_group_width = |label: &Option<(LabelBrush, f32)>| {
        let gap = label.as_ref().map_or(max_gap, |(_, gap)| *gap);
        icon_size as f32
            + label.as_ref().map_or(0.0, |(label, _)| gap + label.width)
            + if with_ring { gap + ring_size } else { 0.0 }
    };
    // Keep the design's spacing when it fits and give wide percentages priority.
    let groups_width: f32 = if bare {
        labels[..providers.len()].iter().map(bare_group_width).sum()
    } else {
        0.0
    };
    let breaks = providers.len().saturating_sub(1) as f32;
    let provider_gap =
        ((geometry.width - groups_width) / breaks.max(1.0)).clamp(0.0, px(BARE_PROVIDER_GROUP_GAP));
    let mut bare_left = (geometry.width - groups_width - provider_gap * breaks) / 2.0;
    let mut anchors = Vec::with_capacity(providers.len());
    for (index, ((provider, usage, accent), label)) in providers.iter().zip(labels).enumerate() {
        let slot = inset + index as f32 * half;
        let group_width = bare_group_width(&label);
        let start = if bare {
            let left = bare_left;
            bare_left += group_width + provider_gap;
            left
        } else {
            slot + if !with_ring {
                (half - icon_size as f32) / 2.0
            } else {
                px(21.0)
            }
        };
        let anchor = if bare {
            start + group_width / 2.0
        } else {
            inset + (index as f32 + 0.5) * half
        };
        anchors.push((*provider, anchor));
        if let Some(image) = usage::logo(*provider) {
            let image_brush = gfx.icon_brush(image, icon_size)?;
            let brush: CompositionBrush =
                if !matches!(provider, Provider::Claude | Provider::Gemini) && geometry.light {
                    let tinted = c.CreateMaskBrush()?;
                    tinted.SetMask(&image_brush)?;
                    tinted.SetSource(&c.CreateColorBrushWithColor(rgba(27, 34, 45, 255))?)?;
                    tinted.cast()?
                } else {
                    image_brush.cast()?
                };
            add_visual(
                c,
                content,
                &brush,
                start,
                (bar_height - icon_size as f32) / 2.0,
                icon_size as f32,
                icon_size as f32,
            )?;
        }
        if let Some((label, gap)) = label {
            let label_offset = if bare {
                icon_size as f32 + gap
            } else {
                px(25.0)
            };
            add_visual(
                c,
                content,
                &label.brush.cast()?,
                start + label_offset,
                (bar_height - label.height) / 2.0,
                label.width,
                label.height,
            )?;
        }
        if with_ring {
            let ring = gfx.usage_ring(
                usage.main_window().map(|window| window.used_percent),
                scale,
                *accent,
                bare,
            )?;
            let x = if bare {
                start + group_width - ring.width
            } else {
                inset + (index + 1) as f32 * half - px(16.0) - ring.width
            };
            add_visual(
                c,
                content,
                &ring.brush.cast()?,
                x,
                (bar_height - ring.height) / 2.0,
                ring.width,
                ring.height,
            )?;
        }
    }
    Ok(anchors)
}

fn draw_bubble_usage(
    gfx: &Gfx,
    content: &VisualCollection,
    geometry: IslandGeometry,
    pill_height: f32,
    providers: &[IslandProvider<'_>],
) -> Result<Vec<(Provider, f32)>> {
    let c = &gfx.compositor;
    let px = |v| geometry.px(v);
    let ink = geometry.ink();
    let scale = geometry.scale;
    let inset = px(8.0);
    let slot = (geometry.width - 2.0 * inset) / providers.len().max(1) as f32;
    if slot < px(20.0) {
        let icon = gfx.glyph_brush(&[('\u{E9D9}', 1.0)], scale, ink)?;
        add_visual(
            c,
            content,
            &icon.brush.cast()?,
            (geometry.width - icon.width) / 2.0,
            (pill_height - icon.height) / 2.0,
            icon.width,
            icon.height,
        )?;
        let centre = geometry.width / 2.0;
        return Ok(providers
            .iter()
            .map(|(provider, ..)| (*provider, centre))
            .collect());
    }
    let icon_size = px(15.0) as u32;
    let gap = px(4.0);
    let mut anchors = Vec::with_capacity(providers.len());
    for (index, (provider, usage, _accent)) in providers.iter().enumerate() {
        let slot_left = inset + index as f32 * slot;
        anchors.push((*provider, slot_left + slot / 2.0));
        let percent = usage.compact_text();
        let label = if slot >= px(44.0) {
            let label = gfx.text_brush_sized(&percent, scale, 11.5, true, ink)?;
            (label.width + icon_size as f32 + gap <= slot).then_some(label)
        } else {
            None
        };
        let group_width = icon_size as f32 + label.as_ref().map_or(0.0, |text| gap + text.width);
        let start = slot_left + ((slot - group_width) / 2.0).max(0.0);
        if let Some(image) = usage::logo(*provider) {
            let image_brush = gfx.icon_brush(image, icon_size)?;
            let brush: CompositionBrush =
                if !matches!(provider, Provider::Claude | Provider::Gemini) && geometry.light {
                    let tinted = c.CreateMaskBrush()?;
                    tinted.SetMask(&image_brush)?;
                    tinted.SetSource(&c.CreateColorBrushWithColor(rgba(27, 34, 45, 255))?)?;
                    tinted.cast()?
                } else {
                    image_brush.cast()?
                };
            add_visual(
                c,
                content,
                &brush,
                start,
                (pill_height - icon_size as f32) / 2.0,
                icon_size as f32,
                icon_size as f32,
            )?;
        }
        if let Some(label) = label {
            add_visual(
                c,
                content,
                &label.brush.cast()?,
                start + icon_size as f32 + gap,
                (pill_height - label.height) / 2.0,
                label.width,
                label.height,
            )?;
        }
    }
    Ok(anchors)
}

fn track_title(track: &Track) -> &str {
    if track.title.trim().is_empty() {
        strings::text().media_title
    } else {
        &track.title
    }
}

fn draw_compact_player(
    gfx: &Gfx,
    layers: &VisualCollection,
    geometry: IslandGeometry,
    width: f32,
    height: f32,
    track: &Track,
    cover: Option<&CompositionSurfaceBrush>,
) -> Result<()> {
    let c = &gfx.compositor;
    let px = |v| geometry.px(v);
    let ink = geometry.ink();
    let side = px(18.0).min(height).min(width);
    let note_left = px(if geometry.bare {
        BARE_PLAYER_COVER_INSET
    } else {
        18.0
    });
    let x = if geometry.bare {
        note_left + px(BARE_PLAYER_TITLE_GAP)
    } else {
        px(23.0)
    } + side;
    let show_title = width > x + px(18.0);
    let note_x = if show_title {
        note_left
    } else {
        (width - side) / 2.0
    };
    if let Some(cover) = cover {
        add_visual(
            c,
            layers,
            &cover.cast()?,
            note_x,
            (height - side) / 2.0,
            side,
            side,
        )?
        .SetOpacity(if track.playing { 1.0 } else { 0.7 })?;
    }
    if show_title {
        let label = gfx.text_brush_fitted(
            track_title(track),
            geometry.scale,
            if geometry.bare {
                BARE_PLAYER_TITLE_SIZE
            } else {
                11.5
            },
            true,
            ink,
            width
                - x
                - px(if geometry.bare {
                    BARE_PLAYER_TRAILING_INSET
                } else {
                    13.0
                }),
        )?;
        add_visual(
            c,
            layers,
            &label.brush.cast()?,
            x,
            (height - label.height) / 2.0,
            label.width,
            label.height,
        )?;
    }
    Ok(())
}

struct BubbleMusic {
    artwork_size: f32,
    label: LabelBrush,
    width: f32,
}

fn prepare_bubble_music(gfx: &Gfx, look: &Look, track: &Track) -> Result<BubbleMusic> {
    let px = |v: f32| (v * look.scale).round();
    let ink = if look.light { Ink::Dark } else { Ink::Light };
    let artwork_size = px(18.0);
    let left_pad = px(10.0);
    let gap = px(6.0);
    let right_pad = px(8.0);
    let max_text =
        (px(BUBBLE_MUSIC_WIDTH) - left_pad - artwork_size - gap - right_pad).max(px(18.0));
    let label = gfx.text_brush_fitted(track_title(track), look.scale, 11.5, true, ink, max_text)?;
    let width = (left_pad + artwork_size + gap + label.width + right_pad)
        .min(px(BUBBLE_MUSIC_WIDTH))
        .ceil();
    Ok(BubbleMusic {
        artwork_size,
        label,
        width,
    })
}

fn draw_bubble_player(
    gfx: &Gfx,
    layers: &VisualCollection,
    geometry: IslandGeometry,
    height: f32,
    track: &Track,
    prepared: &BubbleMusic,
    cover: Option<&CompositionSurfaceBrush>,
) -> Result<()> {
    let width = geometry.width;
    let c = &gfx.compositor;
    let px = |v| geometry.px(v);
    let ink = geometry.ink();
    let side = prepared.artwork_size.min(height).min(width);
    let left_pad = px(10.0);
    let gap = px(6.0);
    let right_pad = px(8.0);
    let text_x = left_pad + side + gap;
    let max_label_width = width - text_x - right_pad;
    let show_title = max_label_width >= px(18.0);
    let note_x = if show_title {
        left_pad
    } else {
        (width - side) / 2.0
    };
    if let Some(cover) = cover {
        add_visual(
            c,
            layers,
            &cover.cast()?,
            note_x,
            (height - side) / 2.0,
            side,
            side,
        )?
        .SetOpacity(if track.playing { 1.0 } else { 0.7 })?;
    }
    if show_title {
        let fitted;
        let label = if prepared.label.width <= max_label_width {
            &prepared.label
        } else {
            fitted = gfx.text_brush_fitted(
                track_title(track),
                geometry.scale,
                11.5,
                true,
                ink,
                max_label_width,
            )?;
            &fitted
        };
        add_visual(
            c,
            layers,
            &label.brush.cast()?,
            text_x,
            (height - label.height) / 2.0,
            label.width,
            label.height,
        )?;
    }
    Ok(())
}

impl Drop for MenuBar {
    fn drop(&mut self) {
        self.search_watch = None;
        self.settings_watch = None;
        self.privacy_watch = None;
        self.bluetooth = None;
        self.accessories = None;
        self.exit_animation = None;
        self.media = None;
        ISLAND_HOOK_WINDOW.store(0, Ordering::Relaxed);
        edge_watch::set_menu_bar(None, None);
        if let Some(hook) = self.outside_click_hook.take() {
            // SAFETY: Do not leave a callback pointing at a destroyed window.
            unsafe {
                let _ = UnhookWindowsHookEx(hook);
            }
        }
        // Give the reserved strip back before the window goes away.
        self.appbar = None;
        // SAFETY: Our own window, destroyed once.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod island_tests {
    use super::*;

    #[test]
    #[ignore = "Requires Windows desktop composition"]
    fn native_ghost_mode_changes_reserve_nothing_and_keep_island_cues() -> Result<()> {
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        // SAFETY: The dispatcher and bar belong to this test thread.
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, true, true, MenuBarWidgets::default())?;
        bar.audio = None; // This fixture can never change the user's audio.
        bar.media = Some(media::Watch::test_snapshot(Track::test_fixture()));
        bar.refresh(&gfx)?;
        bar.set_hiding(true, false, false, false)?;
        assert_eq!(bar.presence_now(), Presence::IslandOnly);
        // A moment of the AI plan reaches the crab while only the island shows.
        bar.cue = Some((mascot::Act::Fresh, 960));
        let (_, point) = bar.take_cue().ok_or_else(|| anyhow::anyhow!("cue"))?;
        assert!(point.is_some(), "the island is on screen");
        // Ghost to Automatic or Intelligent never reserves the strip on the way.
        for (auto, smart) in [(true, false), (false, true)] {
            bar.set_hiding(true, false, false, false)?;
            bar.strip_reservations = 0;
            bar.set_hiding(false, auto, smart, smart)?;
            assert_eq!(bar.strip_reservations, 0, "auto={auto} smart={smart}");
            assert!(bar.reserved_strip().is_none());
            assert_eq!(bar.presence_now(), Presence::Hidden);
            // A window under the intelligent strip need not be maximized.
            bar.set_hiding(true, false, false, false)?;
            assert_eq!(bar.presence_now(), Presence::IslandOnly);
        }
        bar.set_hiding(false, false, false, false)?;
        assert!(bar.strip_reservations > 0);
        assert!(bar.reserved_strip().is_some());
        Ok(())
    }

    #[test]
    #[ignore = "Hidden compositor fixture; synthetic preferences, nothing is opened"]
    fn native_bare_bar_draws_no_strip_and_halos_every_item() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        let mut look = bar.look.clone().context("initial look")?;
        look.screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        look.app_name = "Prueba".to_owned();
        look.track = Some(Track::test_fixture());
        for glass in [true, false] {
            for bare in [false, true] {
                look.glass = glass;
                look.widgets.bare = bare;
                assert_eq!(strip_fills(&gfx, &look)?.is_empty(), bare);
                bar.apply_look(&gfx, look.clone())?;
                let root: ContainerVisual = bar.content_root()?.cast()?;
                let mut halos = 0;
                for child in root.Children()? {
                    if let Ok(sprite) = child.cast::<SpriteVisual>()
                        && sprite.Shadow().is_ok()
                    {
                        halos += 1;
                    }
                }
                if bare {
                    assert!(
                        halos > 0 && halos % HALO.len() == 0,
                        "glass={glass}: {halos}"
                    );
                } else {
                    assert_eq!(halos, 0, "glass={glass}");
                }
            }
        }
        // White ink and a dark island whatever the Windows theme.
        let widgets = MenuBarWidgets {
            bare: true,
            ..MenuBarWidgets::default()
        };
        bar.set_widgets(&gfx, widgets)?;
        assert!(!bar.look.as_ref().context("look")?.light);
        Ok(())
    }

    #[test]
    fn essentials_keep_status_but_drop_shell_buttons() {
        for action in [
            Action::Notifications,
            Action::Search,
            Action::TaskView,
            Action::Widgets,
            Action::HiddenIcons,
        ] {
            assert!(!essential(action), "{action:?}");
        }
        for action in [
            Action::Clock,
            Action::QuickSettings,
            Action::Network,
            Action::Sound,
            Action::Bluetooth,
            Action::Power,
            Action::AccessoryIndicator,
            Action::SystemMonitor,
            Action::Microphone,
            Action::Camera,
            Action::InputLanguage,
        ] {
            assert!(essential(action), "{action:?}");
        }
    }

    #[test]
    #[ignore = "Hidden compositor fixture; synthetic preferences, nothing is opened"]
    fn native_essentials_drop_buttons_and_network_speed() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        let mut look = bar.look.clone().context("initial look")?;
        look.screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        look.widgets.notifications = true;
        look.status.buttons.search = true;
        look.status.buttons.task_view = true;
        look.status.buttons.widgets = true;
        look.monitor = Some([
            "CPU 12%".to_owned(),
            "RAM 48%".to_owned(),
            "↑ 12 KB/s".to_owned(),
            "↓ 35,5 MB/s".to_owned(),
        ]);
        let dropped = [
            Action::Notifications,
            Action::Search,
            Action::TaskView,
            Action::Widgets,
            Action::HiddenIcons,
        ];
        let monitor_width = |bar: &MenuBar| {
            bar.zones
                .iter()
                .find(|(_, _, a)| *a == Action::SystemMonitor)
                .map(|(left, right, _)| right - left)
        };
        look.widgets.essentials = false;
        bar.apply_look(&gfx, look.clone())?;
        let actions: Vec<Action> = bar.zones.iter().map(|(_, _, a)| *a).collect();
        for action in dropped {
            assert!(actions.contains(&action), "{action:?}");
        }
        let full = monitor_width(&bar).context("monitor zone")?;
        look.widgets.essentials = true;
        bar.apply_look(&gfx, look.clone())?;
        let actions: Vec<Action> = bar.zones.iter().map(|(_, _, a)| *a).collect();
        for action in dropped {
            assert!(!actions.contains(&action), "{action:?}");
        }
        for action in [Action::Clock, Action::QuickSettings, Action::SystemMonitor] {
            assert!(actions.contains(&action), "{action:?}");
        }
        let essential_width = monitor_width(&bar).context("monitor zone")?;
        assert!(essential_width < full, "{essential_width} < {full}");
        Ok(())
    }

    impl MenuBar {
        /// The root the last build drew the strip into.
        fn content_root(&self) -> Result<windows::UI::Composition::Visual> {
            Ok(self.strip_layer.Children()?.First()?.Current()?)
        }
    }

    #[test]
    fn presence_follows_the_ghost_table() {
        use Presence::*;
        // (visible, auto_hide, revealed, ghost, covered, has_island) -> esperado
        for (args, expected) in [
            ((false, true, true, true, false, true), Hidden),
            ((true, false, false, false, false, true), Full),
            ((true, true, true, true, true, true), Full),
            ((true, true, false, true, false, true), IslandOnly),
            ((true, true, false, true, true, true), Hidden),
            ((true, true, false, true, false, false), Hidden),
            ((true, true, false, false, false, true), Hidden),
        ] {
            let (v, a, r, g, c, i) = args;
            assert_eq!(presence(v, a, r, g, c, i, false), expected, "{args:?}");
        }
    }

    #[test]
    fn zoned_island_remains_visible_when_the_strip_is_covered() {
        assert_eq!(
            presence(true, true, false, true, true, true, true),
            Presence::IslandOnly
        );
    }

    #[test]
    fn needs_pointer_timer_is_off_while_only_the_island_rests() {
        use Presence::*;
        assert!(!needs_pointer_timer(
            true, IslandOnly, true, false, false, false
        ));
        assert!(needs_pointer_timer(
            true, IslandOnly, true, true, false, false
        ));
        assert!(needs_pointer_timer(
            true, IslandOnly, true, false, true, false
        ));
        assert!(needs_pointer_timer(true, Full, true, false, false, false));
        assert!(!needs_pointer_timer(true, Full, false, false, false, false));
        assert!(!needs_pointer_timer(
            false, Hidden, true, false, false, false
        ));
    }

    #[test]
    fn ghost_region_covers_only_the_island() {
        let island = Span {
            left: 740.4,
            right: 1179.6,
        };
        for height in [28, 30] {
            assert_eq!(
                ghost_region(island, height as f32),
                Some(RECT {
                    left: 740,
                    top: 0,
                    right: 1180,
                    bottom: height,
                })
            );
        }
        assert_eq!(ghost_region(Span::default(), 28.0), None);
    }

    #[test]
    fn revealed_unzoned_bar_has_no_region() {
        let island = Span {
            left: 740.4,
            right: 1179.6,
        };
        assert_eq!(
            resting_island_region(Presence::Full, false, false, island, 30.0),
            None
        );
    }

    #[test]
    fn a_failed_window_region_is_retried_until_installed() {
        let mut installed = Some(WindowRegion::Island(RECT::default()));
        let previous = installed;
        let desired = Some(CapsuleLayout::default().region(RevealedSides::default()));
        let failed = !sync_window_region(&mut installed, desired, |_| false);
        assert_eq!(installed, previous);
        assert!(needs_pointer_timer(
            true,
            Presence::IslandOnly,
            true,
            false,
            false,
            failed
        ));
        assert!(sync_window_region(&mut installed, desired, |_| true));
        assert_eq!(installed, desired);
        assert!(sync_window_region(&mut installed, desired, |_| {
            panic!("the installed region must be reused")
        }));
    }

    #[test]
    fn dwell_accepts_the_first_pixel_of_a_fractional_entrance() {
        let bounds = RECT {
            left: -1920,
            top: -30,
            right: 0,
            bottom: -2,
        };
        let entrance = Span {
            left: 800.4,
            right: 1120.6,
        };
        let pixels = entrance.screen_pixels(bounds.left);
        for (x, expected) in [
            (pixels.start - 1, PointerZone::Left),
            (pixels.start, PointerZone::Center),
            (pixels.end - 1, PointerZone::Center),
            (pixels.end, PointerZone::Right),
        ] {
            assert_eq!(strip_zone(bounds, entrance, POINT { x, y: -30 }), expected);
        }
    }

    #[test]
    fn sides_require_center_entry_and_stay_revealed_until_exit() {
        let bounds = RECT {
            left: -1920,
            top: -30,
            right: 0,
            bottom: -2,
        };
        let entrance = Span {
            left: 800.0,
            right: 1120.0,
        };
        let (left, center, right, below) = (
            POINT { x: -1121, y: -30 },
            POINT { x: -1120, y: -30 },
            POINT { x: -800, y: -30 },
            POINT { x: -900, y: -2 },
        );
        let both = Some(RevealedSides {
            left: true,
            right: true,
        });
        let center_only = Some(RevealedSides::default());
        let mut sides = None;
        for (point, expected) in [
            (left, None),
            (right, None),
            (center, center_only),
            (
                right,
                Some(RevealedSides {
                    left: false,
                    right: true,
                }),
            ),
            (
                center,
                Some(RevealedSides {
                    left: false,
                    right: true,
                }),
            ),
            (left, both),
            (right, both),
            (below, None),
            (left, None),
            (center, center_only),
            (
                left,
                Some(RevealedSides {
                    left: true,
                    right: false,
                }),
            ),
        ] {
            sides = revealed_at(sides, strip_zone(bounds, entrance, point));
            assert_eq!(sides, expected, "pointer at {point:?}");
        }
    }

    #[test]
    fn ghost_clip_keeps_only_revealed_capsules_and_the_open_controls() {
        let rounded =
            |left, right, top, height| RegionPart::rounded(Span { left, right }, top, height, 15.0);
        let island = rounded(800.0, 1120.0, 0.0, 30.0);
        let left = rounded(7.0, 140.0, 0.0, 28.0);
        let right = rounded(1550.0, 1917.0, 0.0, 28.0);
        let panel = rounded(740.0, 1100.0, 33.0, 180.0);
        let volume = rounded(1108.0, 1140.0, 33.0, 180.0);
        let layout = CapsuleLayout {
            island,
            left,
            right,
            ..CapsuleLayout::default()
        };
        for (sides, expected) in [
            (RevealedSides::default(), [island, None, None, None, None]),
            (
                RevealedSides {
                    left: true,
                    right: false,
                },
                [island, left, None, None, None],
            ),
            (
                RevealedSides {
                    left: false,
                    right: true,
                },
                [island, None, right, None, None],
            ),
            (
                RevealedSides {
                    left: true,
                    right: true,
                },
                [island, left, right, None, None],
            ),
        ] {
            assert_eq!(layout.region(sides), WindowRegion::Capsules(expected));
        }
        let open = CapsuleLayout {
            panel,
            volume,
            ..layout
        };
        assert_eq!(
            open.region(RevealedSides::default()),
            WindowRegion::Capsules([island, None, None, panel, volume])
        );
        assert_eq!(
            panel.map(|part| part.rect),
            Some(RECT {
                left: 740,
                top: 33,
                right: 1100,
                bottom: 213
            })
        );
        assert_eq!(
            CapsuleLayout::default().region(RevealedSides::default()),
            WindowRegion::Capsules([None; 5])
        );
    }

    #[test]
    fn capsule_region_leaves_gaps_and_rounded_corners_click_through() {
        use windows::Win32::Graphics::Gdi::PtInRegion;

        let island = RegionPart::rounded(
            Span {
                left: 800.0,
                right: 1120.0,
            },
            0.0,
            30.0,
            15.0,
        )
        .map(|part| RegionPart {
            square_top: true,
            ..part
        });
        let right = RegionPart::rounded(
            Span {
                left: 1550.0,
                right: 1917.0,
            },
            0.0,
            28.0,
            15.0,
        );
        let panel = RegionPart::rounded(
            Span {
                left: 740.0,
                right: 1100.0,
            },
            35.0,
            180.0,
            18.0,
        );
        let region = CapsuleLayout {
            island,
            right,
            panel,
            ..CapsuleLayout::default()
        }
        .region(RevealedSides {
            left: false,
            right: true,
        })
        .create_handle();
        assert!(!region.is_invalid());
        // SAFETY: The region is owned by this test and deleted after querying it.
        let covered = unsafe {
            let covered = [
                (800, 0),
                (800, 29),
                (1300, 14),
                (1550, 0),
                (1700, 14),
                (1548, 14),
                (1547, 14),
                (1918, 14),
                (1919, 14),
                (900, 216),
                (900, 217),
            ]
            .map(|(x, y)| PtInRegion(region, x, y).as_bool());
            let _ = DeleteObject(region.into());
            covered
        };
        assert_eq!(
            covered,
            [
                true, false, false, false, true, true, false, true, false, true, false
            ]
        );
    }

    #[test]
    fn pointer_crosses_from_the_island_to_the_wider_panel() {
        let island = Span {
            left: 800.0,
            right: 1120.0,
        };
        let panel = Span {
            left: 760.0,
            right: 1160.0,
        };
        for (x, y, height, expected) in [
            (780.0, 27.0, 30.0, island),
            (799.0, 28.0, 30.0, panel),
            (800.0, 28.0, 30.0, island),
            (1119.0, 29.0, 30.0, island),
            (1120.0, 29.0, 30.0, panel),
            (960.0, 30.0, 30.0, panel),
            (960.0, 28.0, 28.0, panel),
        ] {
            assert_eq!(
                island_or_panel_span(x, y, 28.0, height, island, panel),
                expected,
                "point ({x}, {y}), island height {height}"
            );
        }
    }

    #[test]
    #[ignore = "Requires Windows desktop composition"]
    fn native_ghost_bar_rests_on_the_island_and_reveals_the_strip() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::{
            Graphics::Gdi::{GDI_REGION_TYPE, GetWindowRgnBox, RGN_ERROR},
            System::WinRT::{
                CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
                DispatcherQueueOptions,
            },
            UI::WindowsAndMessaging::IsWindowVisible,
        };
        // SAFETY: The dispatcher and bar belong to this test thread.
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let region_of = |bar: &MenuBar| -> (GDI_REGION_TYPE, RECT) {
            let mut r = RECT::default();
            // SAFETY: Read-only query of our own window.
            let kind = unsafe { GetWindowRgnBox(bar.hwnd, &mut r) };
            (kind, r)
        };
        let visible = |bar: &MenuBar| unsafe { IsWindowVisible(bar.hwnd) }.as_bool();
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, true, true, MenuBarWidgets::default())?;
        bar.audio = None; // This fixture can never change the user's audio.
        // Redraws read the track from here, so the island survives them.
        bar.media = Some(media::Watch::test_snapshot(Track::test_fixture()));
        let mut look = bar.look.clone().context("initial look")?;
        look.track = Some(Track::test_fixture());
        look.screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        bar.apply_look(&gfx, look.clone())?;
        let height = |bar: &MenuBar| (bar.bounds.bottom - bar.bounds.top) as f32;

        bar.set_ghost(true)?;
        assert!(bar.auto_hide);
        assert!(bar.reserved_strip().is_none());
        assert_eq!(bar.presence_now(), Presence::IslandOnly);
        assert!(visible(&bar));
        assert_eq!(bar.strip_layer.Opacity()?, 0.0);
        assert_eq!(bar.surface.Opacity()?, 1.0);
        let (kind, r) = region_of(&bar);
        assert_ne!(kind, RGN_ERROR);
        assert_eq!(Some(r), ghost_region(bar.triggers_span, height(&bar)));
        // CreateRectRgn returns a null handle on failure. It must not be
        // forwarded to SetWindowRgn, where it would remove this clip.
        assert!(!install_window_region(bar.hwnd, Some(HRGN::default())));
        assert_eq!(region_of(&bar), (kind, r));

        bar.edge_rested(&gfx, POINT { x: 10, y: 0 })?;
        assert_eq!(bar.presence_now(), Presence::Full);
        assert_eq!(region_of(&bar).0, RGN_ERROR, "revealed, no region");

        let below = POINT {
            x: 10,
            y: bar.bounds.bottom + 50,
        };
        bar.pointer_left_at(&gfx, below)?;
        if bar.hide_until.is_some() {
            bar.finish_hide_at(&gfx, Instant::now() + Duration::from_secs(1), below, None)?;
        }
        assert_eq!(bar.presence_now(), Presence::IslandOnly);
        let (kind, r) = region_of(&bar);
        assert_ne!(kind, RGN_ERROR);
        assert_eq!(Some(r), ghost_region(bar.triggers_span, height(&bar)));

        bar.set_covered(&gfx, true)?;
        assert_eq!(bar.presence_now(), Presence::Hidden);
        if system::animations_enabled() {
            let deadline = bar.island_hide_until.context("island fade-out")?;
            assert!(visible(&bar), "keep the window alive during the fade");
            assert_eq!(bar.strip_layer.Opacity()?, 0.0);
            bar.finish_hide_at(&gfx, deadline, below, None)?;
        }
        assert!(!visible(&bar));
        bar.set_covered(&gfx, false)?;
        assert_eq!(bar.presence_now(), Presence::IslandOnly);
        assert!(visible(&bar));
        assert_eq!(bar.island_shown, Some(true));

        // An island emptied in place has no region to clip to: it must hide
        // at once, never fade as a full-width window that eats clicks.
        let kept = bar.triggers_span;
        bar.triggers_span = Span::default();
        bar.place()?;
        assert!(bar.island_hide_until.is_none(), "no fade without a region");
        assert!(!visible(&bar));
        bar.triggers_span = kept;
        bar.place()?;
        assert!(visible(&bar));

        let mut scaled = bar.look.clone().context("look")?;
        scaled.scale = 1.5;
        bar.apply_look(&gfx, scaled)?;
        let (kind, r) = region_of(&bar);
        assert_ne!(kind, RGN_ERROR);
        assert_eq!(Some(r), ghost_region(bar.triggers_span, height(&bar)));

        bar.set_auto_hide(false)?;
        bar.set_smart_hide(false)?;
        bar.set_ghost(false)?;
        assert!(!bar.auto_hide);
        assert!(bar.reserved_strip().is_some());
        Ok(())
    }

    #[test]
    #[ignore = "Hidden compositor fixture; synthetic preferences, no Windows settings writes"]
    fn native_status_items_follow_windows_preferences_without_overrides() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        let mut look = bar.look.clone().context("initial look")?;
        look.screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        look.battery = Some(Battery {
            percent: 75,
            charging: false,
            saver: false,
        });
        look.accessories = [None, None];
        look.status.bluetooth_on = false;
        let mut battery_width = [0.0; 2];
        for mask in 0..16 {
            look.status.buttons = TaskbarButtons {
                search: mask & 1 != 0,
                task_view: mask & 2 != 0,
                widgets: mask & 4 != 0,
            };
            look.status.battery_percentage = mask & 8 != 0;
            bar.apply_look(&gfx, look.clone())?;
            for (action, on) in [
                (Action::Search, look.status.buttons.search),
                (Action::TaskView, look.status.buttons.task_view),
                (Action::Widgets, look.status.buttons.widgets),
            ] {
                assert_eq!(
                    bar.zones.iter().filter(|(_, _, a)| *a == action).count(),
                    usize::from(on)
                );
            }
            // The battery is the one Power zone. Its percentage must add
            // label width, without introducing another clickable control.
            let mut power = bar.zones.iter().filter(|(_, _, a)| *a == Action::Power);
            let battery = power.next().context("battery zone")?;
            assert!(power.next().is_none());
            battery_width[usize::from(look.status.battery_percentage)] = battery.1 - battery.0;
        }
        assert!(battery_width[1] > battery_width[0]);
        // No phantom battery or percentage on a desktop without a battery.
        look.battery = None;
        bar.apply_look(&gfx, look)?;
        let count = |action: Action| bar.zones.iter().filter(|(_, _, a)| *a == action).count();
        // Only the control center opens all of quick settings; the network
        // icon has its own list (issue #55).
        assert_eq!(count(Action::QuickSettings), 1);
        assert_eq!(count(Action::Network), 1);
        assert_eq!(count(Action::Power), 0);

        // The native registry handler reads the real Windows values without
        // replacing the media/provider snapshot or resetting the clock timer.
        bar.registry_changed(&gfx)?;
        let current = bar.look.as_ref().context("updated look")?;
        assert!(current.status.buttons == status::taskbar_buttons());
        assert_eq!(
            current.status.battery_percentage,
            status::battery_percentage_enabled()
        );
        Ok(())
    }

    #[test]
    #[ignore = "Hidden compositor fixture; synthetic preferences, nothing is opened"]
    fn native_notification_bell_takes_the_right_end_only_when_chosen() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        let mut look = bar.look.clone().context("initial look")?;
        look.screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        for notifications in [false, true, false] {
            look.widgets.notifications = notifications;
            bar.apply_look(&gfx, look.clone())?;
            // Zones are pushed from the right edge inward.
            let order: Vec<Action> = bar.zones.iter().map(|(_, _, a)| *a).collect();
            let expected: &[Action] = if notifications {
                &[Action::Notifications, Action::Clock, Action::QuickSettings]
            } else {
                &[Action::Clock, Action::QuickSettings]
            };
            assert_eq!(order.get(..expected.len()), Some(expected));
            assert_eq!(
                order
                    .iter()
                    .filter(|a| **a == Action::Notifications)
                    .count(),
                usize::from(notifications)
            );
        }
        // The clock target is not drawn: choosing one keeps the same look.
        let drawn = bar.look.clone();
        bar.set_clock_target(Some("ms-clock:"));
        assert_eq!(bar.clock_target.as_deref(), Some("ms-clock:"));
        assert!(bar.look == drawn);
        bar.set_clock_target(None);
        assert_eq!(bar.clock_target, None);
        Ok(())
    }

    #[test]
    #[ignore = "Hidden compositor fixture; synthetic network states"]
    fn native_network_moments_wait_out_brief_drops_and_tell_once() -> Result<()> {
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, false, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        bar.cue = None;
        bar.online = None;
        let act = |bar: &mut MenuBar| bar.cue.take().map(|(act, _)| act);
        // The first reading is only the starting point.
        bar.status.network = Network::Ethernet;
        bar.watch_network();
        assert_eq!(act(&mut bar), None);
        // A brief drop says nothing.
        bar.status.network = Network::Offline;
        bar.watch_network();
        assert_eq!(act(&mut bar), None);
        bar.status.network = Network::Ethernet;
        bar.watch_network();
        assert_eq!(act(&mut bar), None);
        // Down for the grace period: told once.
        bar.status.network = Network::Offline;
        bar.watch_network();
        bar.offline_since = Some(Instant::now() - OFFLINE_GRACE);
        bar.watch_network();
        assert_eq!(act(&mut bar), Some(mascot::Act::Offline));
        bar.watch_network();
        assert_eq!(act(&mut bar), None);
        // Back: relief, also once.
        bar.status.network = Network::Ethernet;
        bar.watch_network();
        assert_eq!(act(&mut bar), Some(mascot::Act::Online));
        bar.watch_network();
        assert_eq!(act(&mut bar), None);
        // A hidden bar hands the moment over without a point (the dock's).
        bar.cue = Some((mascot::Act::Online, 10));
        assert!(bar.take_cue().is_some_and(|(_, point)| point.is_none()));
        Ok(())
    }

    #[test]
    #[ignore = "Hidden compositor fixture; synthetic coverage changes"]
    fn native_intelligent_bar_hides_only_while_covered_and_reserves_nothing() -> Result<()> {
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, false, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        bar.set_visible(true)?;
        assert!(bar.reserved_strip().is_some());
        bar.set_smart_hide(true)?;
        // Intelligent: shown, but maximized windows get the whole screen.
        assert!(!bar.auto_hide && bar.reserved_strip().is_none());
        bar.set_covered(&gfx, true)?;
        assert!(bar.auto_hide);
        bar.set_covered(&gfx, false)?;
        assert!(!bar.auto_hide && !bar.revealed && bar.hide_until.is_none());
        // Automatically hide wins: coverage changes nothing.
        bar.set_auto_hide(true)?;
        bar.set_covered(&gfx, true)?;
        assert!(bar.auto_hide);
        bar.set_covered(&gfx, false)?;
        assert!(bar.auto_hide);
        // Back to intelligent while covered: hidden at once.
        bar.set_covered(&gfx, true)?;
        bar.set_auto_hide(false)?;
        assert!(bar.auto_hide);
        // Always visible: reserves its strip again and shows.
        bar.set_smart_hide(false)?;
        assert!(!bar.auto_hide && bar.reserved_strip().is_some());
        bar.set_visible(false)?;
        Ok(())
    }

    #[test]
    #[ignore = "Hidden compositor fixture; synthetic monitor readings"]
    fn native_system_monitor_keeps_its_place_and_samples_only_while_shown() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let widgets = MenuBarWidgets {
            system_monitor: true,
            ..MenuBarWidgets::default()
        };
        let mut bar = MenuBar::create(&gfx, false, false, false, widgets)?;
        bar.set_visible(false)?;
        // Hidden, nothing is sampled.
        assert!(bar.monitor.is_none());
        let mut look = bar.look.clone().context("initial look")?;
        look.screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let lines = |cpu: &str, down: &str| {
            [
                format!("CPU {cpu}"),
                "RAM 48%".to_owned(),
                "↑ 12 KB/s".to_owned(),
                format!("↓ {down}"),
            ]
        };
        let zone = |bar: &MenuBar| {
            bar.zones
                .iter()
                .find(|(_, _, action)| *action == Action::SystemMonitor)
                .map(|(left, right, _)| (*left, *right))
        };
        look.monitor = None;
        bar.apply_look(&gfx, look.clone())?;
        assert_eq!(zone(&bar), None);
        look.monitor = Some(lines("5%", "0 KB/s"));
        bar.apply_look(&gfx, look.clone())?;
        let narrow = zone(&bar).context("monitor zone")?;
        // The last zone pushed is the leftmost: the monitor sits left of
        // every status icon.
        assert_eq!(
            bar.zones.last().map(|(_, _, action)| *action),
            Some(Action::SystemMonitor)
        );
        look.monitor = Some(lines("100%", "35,5 MB/s"));
        bar.apply_look(&gfx, look.clone())?;
        let wide = zone(&bar).context("monitor zone")?;
        assert!(wide.0 < narrow.0 && wide.1 == narrow.1);
        // Narrower digits again: the columns keep their widest size, so the
        // rest of the bar does not shift back and forth.
        look.monitor = Some(lines("5%", "0 KB/s"));
        bar.apply_look(&gfx, look)?;
        assert_eq!(zone(&bar), Some(wide));
        Ok(())
    }

    #[test]
    #[ignore = "Hidden compositor fixture; synthetic Bluetooth states, no hardware changes"]
    fn native_bluetooth_indicator_appears_only_for_an_enabled_system_radio() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        bar.bluetooth = None;
        let mut look = bar.look.clone().context("initial look")?;
        look.screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        for on in [false, true, false] {
            look.status.bluetooth_on = on;
            bar.status.bluetooth_on = on;
            bar.apply_look(&gfx, look.clone())?;
            assert_eq!(
                bar.zones
                    .iter()
                    .filter(|(_, _, action)| *action == Action::Bluetooth)
                    .count(),
                usize::from(on)
            );
        }
        // The native event handler updates the cached Look instead of running
        // provider/process scans or re-reading unrelated Windows preferences.
        look.status.bluetooth_on = true;
        bar.apply_look(&gfx, look)?;
        bar.bluetooth_changed(&gfx)?;
        assert!(
            !bar.zones
                .iter()
                .any(|(_, _, action)| *action == Action::Bluetooth)
        );
        Ok(())
    }

    #[test]
    #[ignore = "Native popup layout/input regression; hidden window, no audio writes"]
    fn native_popup_overflow_and_layout_changes_keep_input_in_sync() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        // SAFETY: All composition objects stay on this test's STA thread.
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        bar.audio = None;
        let mut look = bar.look.clone().context("initial look")?;
        look.screen = RECT {
            left: -1920,
            top: -768,
            right: 0,
            bottom: 0,
        };
        look.track = None;
        look.panel = Some(Island::Usage);
        look.usage_enabled = true;
        look.active_providers.0.fill(true);
        for provider in Provider::ALL {
            look.usage.providers[provider as usize] = ProviderUsage {
                state: State::Ready,
                windows: [300, 10_080]
                    .into_iter()
                    .map(|minutes| usage::Window {
                        label: None,
                        minutes,
                        used_percent: 30,
                        resets_at: None,
                    })
                    .collect(),
                fetched_at: Some(std::time::SystemTime::now()),
            };
        }
        let opencode = &mut look.usage.providers[Provider::OpenCode as usize];
        opencode.windows.push(usage::Window {
            label: None,
            minutes: 43_200,
            used_percent: 10,
            resets_at: None,
        });
        for style in [IslandStyle::Notch, IslandStyle::Bubble] {
            look.widgets.island_style = style;
            for scale in [1.0, 1.5, 2.0] {
                look.scale = scale;
                for light in [false, true] {
                    look.light = light;
                    bar.usage_scroll = 0.0;
                    bar.build(&gfx, &look)?;
                    assert!(bar.visual_height <= 769, "popup must fit a 768 px display");
                    if scale == 1.0 {
                        assert!(bar.usage_viewport.is_none());
                        continue;
                    }
                    let viewport = bar
                        .usage_viewport
                        .as_ref()
                        .context("overflow needs a viewport")?;
                    let bounds = viewport.bounds;
                    let maximum = viewport.maximum;
                    assert!(maximum > 0.0);
                    let body = viewport.content.clone();
                    let down = viewport.down.clone();
                    let root = bar.content_root()?;
                    for _ in 0..20 {
                        assert!(bar.scroll_usage(48.0)?);
                    }
                    assert_eq!(bar.content_root()?, root);
                    assert_eq!(bar.usage_scroll, maximum);
                    assert_eq!(body.Offset()?.Y, -(maximum * scale).round());
                    assert_eq!(down.Opacity()?, 0.22);
                    let up = bar
                        .island_hits
                        .iter()
                        .find(|hit| matches!(hit.action, IslandAction::ScrollUsage(-1)))
                        .context("scroll up button")?;
                    let (left, top, right, bottom) = up.bounds;
                    assert!(
                        left >= bounds.0
                            && top >= bounds.1
                            && right <= bounds.2
                            && bottom <= bounds.3
                    );
                    bar.click(&gfx, (left + right) / 2.0, (top + bottom) / 2.0)?;
                    assert!(bar.usage_scroll < maximum);
                    assert_eq!(bar.content_root()?, root);
                    assert!(bar.scroll_usage(-10_000.0)?);
                    assert_eq!(bar.usage_scroll, 0.0);
                }
            }
        }
        // A busy refresh is not another active input target.
        look.usage_refreshing = true;
        bar.build(&gfx, &look)?;
        assert!(
            !bar.island_hits
                .iter()
                .any(|hit| matches!(hit.action, IslandAction::Refresh))
        );
        look.usage_refreshing = false;
        bar.scroll_usage(10_000.0)?;
        look.active_providers.0.fill(false);
        look.active_providers.0[Provider::Codex as usize] = true;
        bar.build(&gfx, &look)?;
        assert_eq!(bar.usage_scroll, 0.0); // A refresh that removes rows clamps the offset.
        assert!(bar.usage_viewport.is_none());

        look.panel = Some(Island::Music);
        look.track = Some(Track::test_fixture());
        bar.panel = look.panel;
        bar.status.volume = Some(Volume {
            level: 2,
            muted: false,
            percent: 50,
        });
        look.status = bar.status;
        bar.apply_look(&gfx, look.clone())?;
        bar.apply_volume_reading(
            &gfx,
            Some(Volume {
                level: 0,
                muted: true,
                percent: 0,
            }),
        )?;
        // Mute/glyph changes render the current snapshot; a full refresh would
        // replace this monitor and scan live processes/system status again.
        assert!(
            bar.look
                .as_ref()
                .is_some_and(|current| current.screen == look.screen
                    && current.status.volume.is_some_and(|volume| volume.muted))
        );
        let span = bar
            .island_hits
            .iter()
            .find_map(|hit| match hit.action {
                IslandAction::Volume(span) => Some(span),
                _ => None,
            })
            .context("slider")?;
        bar.volume_drag = Some(span); // Never capture the user's mouse in a test.
        bar.queue_volume(span, span.top);
        look.scale = 1.5;
        bar.apply_look(&gfx, look.clone())?;
        assert!(
            bar.volume_drag.is_none()
                && bar.pending_volume.is_none()
                && bar.volume_target.is_none()
        );
        assert!(bar.release_volume(Some(span.bottom))?); // Late up must not activate another button.
        let span = bar
            .island_hits
            .iter()
            .find_map(|hit| match hit.action {
                IslandAction::Volume(span) => Some(span),
                _ => None,
            })
            .context("new slider")?;
        bar.volume_drag = Some(span);
        bar.queue_volume(span, span.bottom);
        // Move the capsule by changing its anchor style, without changing the
        // monitor/DPI. A provider count can now leave the capsule in place.
        look.widgets.island_style = IslandStyle::Notch;
        bar.apply_look(&gfx, look.clone())?;
        assert!(bar.volume_drag.is_none() && bar.pending_volume.is_none());

        // A trigger that cannot fit must not leave a latent open popup.
        look.screen.right = look.screen.left + 160;
        bar.apply_look(&gfx, look.clone())?;
        assert!(bar.panel.is_none() && bar.look.as_ref().is_some_and(|look| look.panel.is_none()));
        let mut wide = bar.look.clone().context("crowded look")?;
        wide.screen.right = wide.screen.left + 1920;
        bar.apply_look(&gfx, wide)?;
        assert!(bar.panel_span.width() <= 0.0 && bar.usage_viewport.is_none());
        Ok(())
    }

    #[test]
    fn vertical_volume_coordinates_increase_upwards_and_clamp_at_both_ends() {
        let span = VolumeTrack {
            center_x: 200.0,
            top: 100.0,
            bottom: 300.0,
        };
        for (y, expected) in [
            (-100.0, 100),
            (100.0, 100),
            (200.0, 50),
            (299.0, 1),
            (300.0, 0),
            (500.0, 0),
            (f32::NAN, 0),
            (f32::INFINITY, 0),
        ] {
            assert_eq!(volume_percent(span, y), expected);
        }
        assert_eq!(volume_percent(VolumeTrack::default(), 1.0), 0);
    }

    #[test]
    fn volume_capsule_stays_beside_the_player_and_inside_each_screen_edge() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for (left, right, width) in [
                (12.0, 372.0, 800.0),
                (428.0, 788.0, 800.0),
                (220.0, 580.0, 800.0),
                (12.0, 380.0, 392.0),
            ] {
                let (music, capsule) = volume_popover_layout(
                    Span {
                        left: left * scale,
                        right: right * scale,
                    },
                    width * scale,
                    scale,
                );
                assert!(music.left >= 0.0 && capsule.left >= 0.0);
                assert!(music.right <= width * scale && capsule.right <= width * scale);
                assert!(
                    music.right < capsule.left || capsule.right < music.left,
                    "never cover playback controls"
                );
                assert!(music.width() > capsule.width());
            }
        }
        let panel = Span {
            left: 428.0,
            right: 788.0,
        };
        let (music, capsule) = volume_popover_layout(panel, 800.0, 1.0);
        assert!(music == panel && capsule.right < music.left);
    }

    #[test]
    fn playback_timestamps_include_hours_without_affecting_short_tracks() {
        for (seconds, expected) in [
            (0, "0:00"),
            (65, "1:05"),
            (3599, "59:59"),
            (3600, "1:00:00"),
            (3661, "1:01:01"),
        ] {
            assert_eq!(media_time(seconds), expected);
        }
    }

    #[test]
    fn percentage_changes_keep_the_same_indicator_but_mute_and_device_changes_do_not() {
        let volume = Volume {
            level: 2,
            muted: false,
            percent: 40,
        };
        assert!(same_volume_indicator(
            Some(volume),
            Some(Volume {
                percent: 60,
                ..volume
            })
        ));
        assert!(!same_volume_indicator(
            Some(volume),
            Some(Volume {
                muted: true,
                ..volume
            })
        ));
        assert!(!same_volume_indicator(
            Some(volume),
            Some(Volume { level: 3, ..volume })
        ));
        assert!(!same_volume_indicator(Some(volume), None));
        assert!(!same_volume_indicator(None, Some(volume)));
    }

    #[test]
    #[ignore = "Native compositor coverage; hidden test window, no audio writes"]
    fn native_music_artwork_and_volume_fit_both_styles_and_release_drag_state() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        bar.audio = None; // This fixture can never change the user's audio.
        bar.status.volume = Some(Volume {
            level: 2,
            muted: false,
            percent: 42,
        });
        let mut look = bar.look.clone().context("initial look")?;
        look.status = bar.status;
        look.panel = Some(Island::Music);
        look.track = Some(Track::test_fixture());
        for style in [IslandStyle::Notch, IslandStyle::Bubble] {
            look.widgets.island_style = style;
            for scale in [1.0, 1.25, 1.5, 2.0] {
                look.scale = scale;
                for (light, glass) in [(true, true), (false, true), (true, false), (false, false)] {
                    look.light = light;
                    look.glass = glass;
                    bar.build(&gfx, &look)?;
                    assert!(bar.cover.is_some());
                    assert!(bar.progress_controls.is_some());
                    let capsule = bar
                        .volume_controls
                        .as_ref()
                        .context("volume capsule should be visible with the player")?
                        .geometry;
                    let progress = bar.progress_controls.as_ref().context("progress")?;
                    assert_eq!(
                        progress.width,
                        progress.geometry.width - progress.geometry.px(52.0),
                        "the progress track keeps only a balanced right inset"
                    );
                    assert!(
                        progress.geometry.left + progress.geometry.width <= capsule.left
                            || progress.geometry.left >= capsule.left + capsule.width,
                        "the volume capsule stays beside the player"
                    );
                    let brush = bar.cover.as_ref().context("missing cover")?.1.clone();
                    bar.build(&gfx, &look)?;
                    assert_eq!(bar.cover.as_ref().context("cached cover")?.1, brush);
                    let volume = bar
                        .island_hits
                        .iter()
                        .find(|hit| matches!(hit.action, IslandAction::Volume(_)))
                        .context("missing volume hit")?;
                    let (left, top, right, bottom) = volume.bounds;
                    assert!(left >= bar.panel_span.left && right <= bar.panel_span.right);
                    assert!(
                        top >= HEIGHT * scale
                            && bottom <= (HEIGHT + PANEL_GAP + look.panel_height()) * scale + 1.0
                    );
                    let geometry = bar
                        .volume_controls
                        .as_ref()
                        .context("volume controls")?
                        .geometry;
                    // Click the visible speaker's centre, not merely somewhere
                    // inside an independently specified hit rectangle.
                    let speaker = bar
                        .island_hits
                        .iter()
                        .find(|hit| {
                            hit.at(
                                geometry.left + geometry.width / 2.0,
                                geometry.top + geometry.px(155.0),
                            )
                        })
                        .context("speaker has no hit target")?;
                    assert!(matches!(speaker.action, IslandAction::Mute));
                    let slider = bar
                        .island_hits
                        .iter()
                        .find(|hit| hit.at((left + right) / 2.0, geometry.top + geometry.px(82.0)))
                        .context("slider has no hit target")?;
                    assert!(matches!(slider.action, IslandAction::Volume(_)));
                }
            }
        }
        // Commit the synthetic player to input/cache state after the layout
        // matrix. build() alone does not update the open panel or cached Look.
        bar.apply_look(&gfx, look.clone())?;
        let image = Arc::new(muelle_shell::icons::IconImage {
            width: 8,
            height: 8,
            bgra: [20, 40, 80, 255].repeat(64),
        });
        look.track.as_mut().context("track")?.artwork = Some(image.clone());
        bar.build(&gfx, &look)?;
        assert!(
            bar.cover
                .as_ref()
                .and_then(|(source, _)| source.as_ref())
                .is_some_and(|source| Arc::ptr_eq(source, &image))
        );
        // Compare the old full redraw path with retained updates without ever
        // writing the real audio endpoint. Tree identity is the regression
        // assertion; timings are informational, not a flaky speed threshold.
        let started = Instant::now();
        for step in 0..64 {
            bar.status.volume = Some(Volume {
                level: 2,
                muted: false,
                percent: 40 + step % 20,
            });
            look.status = bar.status;
            bar.build(&gfx, &look)?;
        }
        let full_redraw = started.elapsed();
        bar.look = Some(look.clone());
        let root = bar.content_root()?;
        let mut update = look.track.clone().context("track for progress update")?;
        update.timeline = Track::test_fixture().timeline;
        bar.media = Some(media::Watch::test_snapshot(update));
        bar.media_changed(&gfx)?;
        assert_eq!(
            bar.content_root()?,
            root,
            "position events retain the visual tree"
        );
        bar.visible = true; // The native window stays hidden throughout the fixture.
        bar.revealed = true;
        bar.sync_progress_watch();
        assert!(bar.progress_timer);
        bar.visible = false;
        bar.sync_progress_watch();
        assert!(
            !bar.progress_timer,
            "hidden popups must have no progress timer"
        );
        let started = Instant::now();
        for step in 0..64 {
            bar.apply_volume_reading(
                &gfx,
                Some(Volume {
                    level: 2,
                    muted: false,
                    percent: 40 + step % 20,
                }),
            )?;
            assert_eq!(bar.content_root()?, root);
            assert_eq!(
                bar.volume_controls
                    .as_ref()
                    .context("retained volume")?
                    .label_percent,
                40 + step % 20
            );
        }
        eprintln!(
            "64 volume updates: full redraw {:?}, retained {:?} (no audio writes)",
            full_redraw,
            started.elapsed()
        );
        let span = VolumeTrack {
            center_x: 200.0,
            top: 100.0,
            bottom: 300.0,
        };
        bar.volume_drag = Some(span); // No SetCapture in a test.
        bar.queue_volume(span, 260.0);
        bar.pointer_moved(&gfx, 999.0, 140.0)?;
        let player = bar.content_root()?;
        bar.refresh_backdrops(&gfx)?;
        assert_eq!(bar.content_root()?, player);
        assert!(bar.volume_drag.is_some());
        assert_eq!(bar.pending_volume, Some(80));
        assert_eq!(
            bar.volume_controls
                .as_ref()
                .context("immediate percentage")?
                .label_percent,
            80
        );
        assert_eq!(
            bar.volume_controls
                .as_ref()
                .context("immediate fill")?
                .position_percent,
            Some(80)
        );
        let controls = bar.volume_controls.as_ref().context("vertical fill")?;
        assert!((controls.fill.Size()?.Y - controls.height * 0.8).abs() < 0.01);
        assert!(
            (controls.fill.Offset()?.Y + controls.fill.Size()?.Y - controls.height).abs() < 0.01,
            "fill stays attached to the bottom"
        );
        let label = controls.value.Size()?;
        assert!(
            (controls.value.Offset()?.X + label.X / 2.0 - controls.geometry.width / 2.0).abs()
                < 0.01
        );
        // An endpoint acknowledgement must not drag the fill backwards while
        // a newer pointer target is still waiting for its coalesced write.
        bar.apply_volume_reading(
            &gfx,
            Some(Volume {
                level: 2,
                muted: false,
                percent: 50,
            }),
        )?;
        assert_eq!(
            bar.volume_controls
                .as_ref()
                .context("optimistic fill")?
                .position_percent,
            Some(80)
        );
        assert_eq!(bar.content_root()?, root);
        assert!(bar.release_volume(Some(300.0))?);
        assert!(bar.pending_volume.is_none() && bar.volume_drag.is_none());
        assert!(!bar.release_volume(None)?);
        assert!(bar.release_volume(Some(200.0))?); // Late up after capture loss/timer.
        assert!(!bar.release_volume(Some(200.0))?);
        bar.status.volume = None;
        look.status = bar.status;
        // Wallpaper/DWM changes renew only the host sample. Cached content and
        // the open music popup remain intact when the theme itself is unchanged.
        bar.look = Some(look.clone());
        let previous_material = bar.content_root()?;
        bar.refresh_backdrops(&gfx)?;
        assert!(bar.look.as_ref() == Some(&look));
        bar.apply_look(&gfx, look.clone())?;
        assert_eq!(bar.content_root()?, previous_material);
        bar.build(&gfx, &look)?;
        assert!(
            !bar.island_hits
                .iter()
                .any(|hit| matches!(hit.action, IslandAction::Volume(_) | IslandAction::Mute))
        );
        look.track.as_mut().context("track")?.timeline = None;
        bar.apply_look(&gfx, look)?;
        assert!(bar.progress_controls.is_none() && !bar.progress_timer);
        Ok(())
    }

    #[test]
    #[ignore = "Requires Windows composition; hidden window and no provider requests"]
    fn opencode_only_measured_go_appears_in_both_island_styles() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        // SAFETY: The dispatcher and hidden composition objects belong to this thread.
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        let mut look = bar.look.clone().context("initial look")?;
        look.track = None;
        look.active_providers = ActiveProviders::default();
        look.active_providers.0[Provider::OpenCode as usize] = true;
        // Yesterday's saved quota is not evidence that Codex is running now.
        look.usage.providers[Provider::Codex as usize] = ProviderUsage {
            state: State::Ready,
            windows: vec![usage::Window {
                label: None,
                minutes: 300,
                used_percent: 42,
                resets_at: None,
            }],
            fetched_at: Some(std::time::SystemTime::now() - Duration::from_secs(24 * 60 * 60)),
        };
        let quota: Vec<_> = [300, 10_080, 43_200]
            .into_iter()
            .map(|minutes| usage::Window {
                label: None,
                minutes,
                used_percent: 15,
                resets_at: Some(chrono::Utc::now() + chrono::Duration::hours(2)),
            })
            .collect();
        for style in [IslandStyle::Notch, IslandStyle::Bubble] {
            look.widgets.island_style = style;
            for scale in [1.0, 1.25, 1.5, 2.0] {
                look.scale = scale;
                for (light, glass) in [(true, true), (false, true), (true, false), (false, false)] {
                    look.light = light;
                    look.glass = glass;
                    look.panel = Some(Island::Usage);
                    look.usage_enabled = true;
                    look.usage.providers[Provider::OpenCode as usize] = ProviderUsage::default();
                    bar.apply_look(&gfx, look.clone())?;
                    assert!(bar.island_hits.is_empty() && bar.panel.is_none());
                    assert_eq!(bar.triggers_span.width(), 0.0);
                    look.usage.providers[Provider::OpenCode as usize] = ProviderUsage {
                        state: State::Ready,
                        windows: quota.clone(),
                        fetched_at: Some(std::time::SystemTime::now()),
                    };
                    bar.apply_look(&gfx, look.clone())?;
                    let current = bar.look.as_ref().context("rendered Go")?;
                    let providers = island_providers(current);
                    assert_eq!(providers.len(), 1);
                    assert!(providers.iter().all(|entry| entry.0 == Provider::OpenCode));
                    assert_eq!(
                        current.panel_content_height(),
                        74.0 + ISLAND_ROW_HEIGHT + 26.0
                    );
                    assert!(bar.panel_span.width() > 0.0);
                    assert!(
                        bar.island_hits
                            .iter()
                            .any(|hit| matches!(hit.action, IslandAction::Toggle(Island::Usage)))
                    );
                    assert!(
                        bar.island_hits
                            .iter()
                            .any(|hit| matches!(hit.action, IslandAction::Refresh))
                    );
                }
            }
        }
        // Removing Go credentials/quota closes its open popup and removes the trigger.
        look.usage.providers[Provider::OpenCode as usize] = ProviderUsage {
            state: State::SignIn,
            ..ProviderUsage::default()
        };
        bar.apply_look(&gfx, look)?;
        assert!(bar.panel.is_none() && bar.panel_span.width() == 0.0);
        assert!(bar.island_hits.is_empty() && bar.usage_viewport.is_none());
        Ok(())
    }

    #[test]
    #[ignore = "Requires Windows desktop composition; keeps the bar hidden"]
    fn accessory_opt_out_blocks_startup_resume_and_retry() -> Result<()> {
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, true, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        assert!(!bar.accessories_enabled && bar.accessories.is_none());
        bar.restart_accessories(&gfx)?;
        bar.tick(&gfx)?;
        assert!(bar.accessories.is_none());
        bar.set_accessories_enabled(&gfx, true)?;
        assert!(bar.accessories.is_some());
        bar.set_accessories_enabled(&gfx, false)?;
        bar.accessory_retry_at = Instant::now() - Duration::from_secs(1);
        bar.restart_accessories(&gfx)?;
        bar.tick(&gfx)?;
        assert!(!bar.accessories_enabled && bar.accessories.is_none());
        assert!(
            bar.look
                .as_ref()
                .is_some_and(|look| look.accessories == [None, None])
        );
        Ok(())
    }

    #[test]
    fn accessory_status_ignores_hidden_metadata_and_hides_incomplete_readings() {
        use muelle_shell::accessories::{Device, State};
        let mut snapshot = AccessorySnapshot {
            state: State::Ready,
            ..Default::default()
        };
        assert_eq!(accessory_readings(&snapshot), [None, None]);
        snapshot.devices = vec![
            Device {
                id: "mouse".into(),
                name: "Mouse".into(),
                kind: AccessoryKind::Mouse,
                percent: Some(0),
                conflicting: false,
                active_audio: false,
            },
            Device {
                id: "headset".into(),
                name: "Headset".into(),
                kind: AccessoryKind::Headset,
                percent: None,
                conflicting: false,
                active_audio: true,
            },
        ];
        let before = accessory_readings(&snapshot);
        assert!(before[0].is_some_and(|reading| reading.percent == Some(0)));
        assert!(before[1].is_some_and(|reading| reading.percent.is_none()));
        snapshot.devices[0].name = "New name".into();
        assert!(accessory_readings(&snapshot) == before);
        for state in [State::Loading, State::Unavailable] {
            snapshot.state = state;
            assert_eq!(accessory_readings(&snapshot), [None, None]);
        }
    }

    #[test]
    #[ignore = "Requires Windows desktop composition; keeps the test bar hidden"]
    fn accessory_indicators_render_without_controls_or_popups() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, true, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        bar.accessories = None;
        let mut look = bar.look.clone().context("missing initial menu bar")?;
        look.track = None;
        for (scale, material) in [1.0, 1.25, 1.5, 2.0].into_iter().zip(Material::ALL) {
            for light in [true, false] {
                for glass in [true, false] {
                    look.scale = scale;
                    look.material = material;
                    look.light = light;
                    look.glass = glass;
                    look.accessories = [None, None];
                    bar.build(&gfx, &look)?;
                    let root: ContainerVisual = bar.content_root()?.cast()?;
                    let empty_visuals = root.Children()?.Count()?;
                    let controls = bar.zones.len();
                    look.accessories = [
                        Some(AccessoryReading {
                            kind: AccessoryKind::Mouse,
                            percent: Some(76),
                        }),
                        Some(AccessoryReading {
                            kind: AccessoryKind::Headset,
                            percent: Some(58),
                        }),
                    ];
                    bar.build(&gfx, &look)?;
                    let root: ContainerVisual = bar.content_root()?.cast()?;
                    assert_eq!(root.Children()?.Count()?, empty_visuals + 4);
                    assert_eq!(bar.zones.len(), controls);
                    assert!(bar.island_hits.is_empty());
                    assert!(bar.outside_click_hook.is_none());
                    assert_eq!(bar.panel, None);
                }
            }
        }
        Ok(())
    }

    #[test]
    #[ignore = "Requires Windows desktop composition"]
    fn changing_ui_language_redraws_unchanged_menu_bar_content() -> Result<()> {
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        struct RestoreLanguage(Language);
        impl Drop for RestoreLanguage {
            fn drop(&mut self) {
                strings::set_language(self.0);
            }
        }
        let _restore = RestoreLanguage(strings::language());
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        strings::set_language(Language::Es);
        let mut bar = MenuBar::create(&gfx, false, true, true, MenuBarWidgets::default())?;
        assert!(
            bar.look
                .as_ref()
                .is_some_and(|look| look.language == Language::Es)
        );
        for language in [
            Language::En,
            Language::Fr,
            Language::Zh,
            Language::Hi,
            Language::Ar,
            Language::Bn,
            Language::Pt,
            Language::Es,
        ] {
            let previous = bar.content_root()?;
            strings::set_language(language);
            bar.refresh(&gfx)?;
            assert_ne!(
                previous,
                bar.content_root()?,
                "translation-only changes must rebuild the content"
            );
            assert!(
                bar.look
                    .as_ref()
                    .is_some_and(|look| look.language == language)
            );
            // Exercise Windows shaping/font fallback and compact surface bounds,
            // including mixed Arabic + Latin text and Hindi/Bengali conjuncts.
            for text in [
                strings::text().usage_title,
                strings::text().media_title,
                strings::text().usage_claude_sign_in,
            ] {
                let label = gfx.text_brush_fitted(text, 1.0, 13.0, false, Ink::Dark, 160.0)?;
                assert!(label.width > 0.0 && label.width <= 160.0);
                assert!(label.height > 0.0 && label.height < 100.0);
            }
        }
        Ok(())
    }

    #[test]
    #[ignore = "Requires Windows desktop composition"]
    fn auto_hidden_bar_can_reveal_and_hide_repeatedly() -> Result<()> {
        use windows::Win32::{
            System::WinRT::{
                CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
                DispatcherQueueOptions,
            },
            UI::WindowsAndMessaging::IsWindowVisible,
        };
        // SAFETY: The dispatcher and bar belong to this test thread.
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, true, true, MenuBarWidgets::default())?;
        assert!(
            !unsafe { IsWindowVisible(bar.hwnd) }.as_bool(),
            "auto-hide must apply before the first frame"
        );
        bar.set_auto_hide(true)?;
        for _ in 0..3 {
            assert!(!unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
            assert_eq!(
                bar.surface.Opacity()?,
                0.0,
                "hidden content must stay transparent"
            );
            let point = POINT {
                x: (bar.bounds.left + bar.bounds.right) / 2,
                y: bar.bounds.top,
            };
            bar.edge_rested(&gfx, point)?;
            assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
            // A queued leave from before the reveal must not hide it while
            // the pointer is still at the edge, including its first pixel.
            bar.pointer_left_at(&gfx, point)?;
            assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
            let surface = bar.surface.clone();
            bar.set_app_name(&gfx, "Muelle regression test")?;
            assert_eq!(
                bar.surface, surface,
                "redraws must preserve the animation parent"
            );
            let outside = POINT {
                y: bar.bounds.bottom,
                ..point
            };
            bar.pointer_left_at(&gfx, outside)?;
            if bar.hide_until.is_some() {
                // Returning during the exit cancels it. Its already queued
                // completion must not close the bar after that reversal.
                bar.pointer_left_at(&gfx, point)?;
                let old_epoch = bar.animation_epoch;
                bar.finish_hide_at(
                    &gfx,
                    Instant::now() + Duration::from_secs(1),
                    outside,
                    Some(old_epoch.wrapping_sub(1)),
                )?;
                assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
                bar.pointer_left_at(&gfx, outside)?;
                // An old completion must also be ignored during a newer exit.
                bar.finish_hide_at(
                    &gfx,
                    Instant::now() + Duration::from_secs(1),
                    outside,
                    Some(old_epoch),
                )?;
                assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
                let epoch = bar.animation_epoch;
                bar.finish_hide_at(&gfx, Instant::now(), outside, None)?;
                assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
                bar.finish_hide_at(&gfx, Instant::now(), outside, Some(epoch))?;
            }
        }
        assert!(!unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
        bar.set_auto_hide(false)?;
        assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
        bar.set_auto_hide(true)?;
        assert!(!unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
        bar.edge_rested(
            &gfx,
            POINT {
                x: bar.bounds.right / 2,
                y: bar.bounds.top,
            },
        )?;
        bar.set_visible(false)?;
        assert!(!unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
        assert!(bar.hide_until.is_none());
        bar.set_visible(true)?;
        assert!(!unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
        Ok(())
    }

    #[test]
    #[ignore = "Requires Windows desktop composition; runs visible transitions"]
    fn compositor_completion_hides_only_the_current_exit() -> Result<()> {
        use windows::Win32::{
            System::WinRT::{
                CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
                DispatcherQueueOptions,
            },
            UI::WindowsAndMessaging::{
                DispatchMessageW, IsWindowVisible, MSG, PM_REMOVE, PeekMessageW,
            },
        };
        // SAFETY: The dispatcher, queue, window and compositor are test-owned.
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, true, true, MenuBarWidgets::default())?;
        bar.set_app_name(&gfx, "Muelle animation test")?;
        bar.set_auto_hide(true)?;
        let inside = POINT {
            x: bar.bounds.left + 100,
            y: bar.bounds.top,
        };
        let outside = POINT {
            y: bar.bounds.bottom + 50,
            ..inside
        };
        let pump = |bar: &mut MenuBar, duration: Duration, point: POINT| -> Result<usize> {
            let until = Instant::now() + duration;
            let mut completions = 0;
            let mut msg = MSG::default();
            while Instant::now() < until {
                // Pump the dispatcher so real compositor completion events run.
                // Pointer state is deterministic; no mouse input is injected.
                while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                    if msg.hwnd == bar.hwnd && msg.message == HIDE_ANIMATION_DONE {
                        completions += 1;
                        bar.finish_hide_at(&gfx, Instant::now(), point, Some(msg.wParam.0))?;
                    } else if msg.hwnd == bar.hwnd
                        && msg.message == WM_TIMER
                        && msg.wParam.0 == HIDE_ANIMATION_TIMER
                    {
                        bar.finish_hide_at(&gfx, Instant::now(), point, None)?;
                    } else {
                        unsafe { DispatchMessageW(&msg) };
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(completions)
        };
        pump(&mut bar, Duration::from_millis(300), outside)?;
        for _ in 0..3 {
            bar.hover(&gfx, true)?;
            pump(&mut bar, Duration::from_millis(500), inside)?;
            assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
            bar.hover(&gfx, false)?;
            if system::animations_enabled() {
                assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
            }
            let completions = pump(&mut bar, Duration::from_millis(500), outside)?;
            if system::animations_enabled() {
                assert_eq!(completions, 1, "exit must complete through the compositor");
            }
            assert!(!unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
            assert_eq!(bar.surface.Opacity()?, 0.0);
        }
        Ok(())
    }

    #[test]
    fn clicking_usage_replaces_music_and_clicking_again_closes_it() {
        let open = toggle_panel(Some(Island::Music), Island::Usage);
        assert_eq!(open, Some(Island::Usage));
        assert_eq!(toggle_panel(open, Island::Usage), None);
    }

    #[test]
    fn usage_panel_covers_the_last_provider_even_when_the_trigger_is_wide() {
        let screen = 1920.0;
        let triggers = trigger_layout(
            Span {
                left: 210.0,
                right: 1540.0,
            },
            screen / 2.0,
            1.25,
            true,
            5,
            false,
        );
        let Some(usage) = triggers[1].1 else {
            panic!("Usage trigger should fit");
        };
        let panel = panel_span(usage, screen, 550.0);
        assert!(panel.left <= usage.left && panel.right >= usage.right);
    }

    #[test]
    fn narrow_bar_still_gives_each_trigger_its_own_clickable_span() {
        let bounds = Span {
            left: 200.0,
            right: 390.0,
        };
        let [(_, Some(music)), (_, Some(usage))] =
            trigger_layout(bounds, 300.0, 1.5, true, 3, false)
        else {
            panic!("Both triggers should fit");
        };
        assert!(
            music.left >= bounds.left && music.right < usage.left && usage.right <= bounds.right
        );
    }

    #[test]
    fn bare_notch_is_320_pixels_wide_with_music_and_two_providers() {
        let bounds = Span {
            left: 0.0,
            right: 1920.0,
        };
        for (providers, bare_width, original_width) in [(1, 80.0, 145.0), (2, 160.0, 268.0)] {
            for (bare, music_width, usage_width) in
                [(true, 152.0, bare_width), (false, 214.0, original_width)]
            {
                let [(_, Some(music)), (_, Some(usage))] =
                    trigger_layout(bounds, 960.0, 1.0, true, providers, bare)
                else {
                    panic!("Both triggers should fit");
                };
                assert_eq!(music.width(), music_width);
                assert_eq!(usage.width(), usage_width);
                assert_eq!(usage.left - music.right, 8.0);
            }
        }
    }

    #[test]
    fn bubble_layout_aligns_to_right_status_edge_and_stays_compact() {
        let bounds = Span {
            left: 210.0,
            right: 1420.0,
        };
        let [(_, Some(short_music)), (_, Some(short_usage))] =
            bubble_trigger_layout(bounds, 1.0, Some(92.0), 2)
        else {
            panic!("Both bubble segments should fit");
        };
        let [(_, Some(long_music)), (_, Some(long_usage))] =
            bubble_trigger_layout(bounds, 1.0, Some(240.0), 2)
        else {
            panic!("Both bubble segments should fit");
        };
        assert_eq!(short_usage.right, bounds.right);
        assert_eq!(long_usage.right, bounds.right);
        assert!(short_music.width() < long_music.width());
        assert_eq!(long_music.width(), BUBBLE_MUSIC_WIDTH);
        assert!(short_music.left >= bounds.left && short_music.right < short_usage.left);

        let short_bubble = Span {
            left: short_music.left,
            right: short_usage.right,
        };
        let long_bubble = Span {
            left: long_music.left,
            right: long_usage.right,
        };
        let short_panel = bubble_panel_span(short_bubble, 1920.0, 360.0);
        let long_panel = bubble_panel_span(long_bubble, 1920.0, 360.0);
        assert_eq!(short_panel.left, long_panel.left);
        assert_eq!(short_panel.right, long_panel.right);
    }

    #[test]
    fn losing_a_session_closes_its_panel_without_opening_the_other() {
        assert_eq!(available_panel(Some(Island::Music), false, true), None);
        assert_eq!(available_panel(Some(Island::Usage), true, false), None);
    }

    #[test]
    #[ignore = "Requires Windows desktop composition"]
    fn popup_surface_builds_in_both_themes_with_and_without_transparency() -> Result<()> {
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };

        // SAFETY: The controller initializes the test thread's own queue.
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        for (light, glass) in [(true, true), (false, true), (true, false), (false, false)] {
            let root = gfx.compositor.CreateContainerVisual()?;
            let theme = Theme::for_popup(light, glass);
            let geometry = IslandGeometry {
                left: 90.0,
                top: HEIGHT + PANEL_GAP,
                width: 360.0,
                scale: 1.0,
                light,
                bare: false,
            };
            popup_surface(
                &gfx,
                theme,
                &root.Children()?,
                geometry,
                136.0,
                geometry.px(PANEL_RADIUS),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod mascot_tests {
    use super::*;
    use crate::usage::{ProviderUsage, Snapshot, State, Window};

    fn at(percent: Option<u8>) -> Snapshot {
        let mut snapshot = Snapshot::default();
        if let Some(percent) = percent {
            snapshot.providers[Provider::Claude as usize] = ProviderUsage {
                state: State::Ready,
                windows: vec![Window {
                    label: None,
                    minutes: 300,
                    used_percent: percent,
                    resets_at: None,
                }],
                fetched_at: None,
            };
        }
        snapshot
    }

    fn moment(before: Option<u8>, now: Option<u8>) -> Option<mascot::Act> {
        let mut active = ActiveProviders::default();
        active.0[Provider::Claude as usize] = true;
        usage_moment(&at(before), &at(now), &active).map(|(_, act)| act)
    }

    #[test]
    fn usage_moment_should_fire_only_when_a_line_is_crossed() {
        assert_eq!(moment(Some(60), Some(0)), Some(mascot::Act::Fresh));
        assert_eq!(moment(Some(85), Some(91)), Some(mascot::Act::Nervous));
        assert_eq!(moment(Some(95), Some(100)), Some(mascot::Act::Exhausted));
        // Straight to 100 is "exhausted", not "nervous".
        assert_eq!(moment(Some(50), Some(100)), Some(mascot::Act::Exhausted));
        // Staying above a line, a small reset, or the first figure: nothing.
        assert_eq!(moment(Some(92), Some(95)), None);
        assert_eq!(moment(Some(10), Some(0)), None);
        assert_eq!(moment(None, Some(0)), None);
        assert_eq!(moment(None, Some(100)), None);
    }

    #[test]
    fn usage_moment_ignores_failed_reads_and_different_quota_windows() {
        let before = at(Some(95));
        let mut after = at(Some(0));
        let mut active = ActiveProviders::default();
        active.0[Provider::Claude as usize] = true;
        after.providers[Provider::Claude as usize].windows[0].minutes = 10080;
        assert!(usage_moment(&before, &after, &active).is_none());
        after.providers[Provider::Claude as usize].windows[0].minutes = 300;
        after.providers[Provider::Claude as usize].state = State::Unavailable;
        assert!(usage_moment(&before, &after, &active).is_none());
    }

    #[test]
    fn usage_moment_ignores_out_of_order_measurements() {
        let mut before = at(Some(95));
        let mut after = at(Some(0));
        let mut active = ActiveProviders::default();
        active.0[Provider::Claude as usize] = true;
        let now = std::time::SystemTime::now();
        before.providers[Provider::Claude as usize].fetched_at = Some(now);
        after.providers[Provider::Claude as usize].fetched_at =
            now.checked_sub(std::time::Duration::from_secs(60));
        assert!(usage_moment(&before, &after, &active).is_none());
    }
}
