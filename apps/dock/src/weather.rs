//! One-shot forecasts shared by the desktop clock and the zoned menu bar.

use std::{
    env,
    io::Read,
    os::windows::{io::AsRawHandle, process::CommandExt},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use muelle_config::Weather;
use reqwest::{blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use windows::Win32::{
    Foundation::{HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM},
    System::Threading::WaitForSingleObject,
    UI::WindowsAndMessaging::{PostMessageW, WM_APP},
};

pub const WORKER_FLAG: &str = "--weather-worker";
pub const READY: u32 = WM_APP + 13;
const VALID_FOR: Duration = Duration::from_secs(30 * 60);
/// After a failed query, the next reveal may ask again this soon.
const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);
/// A reading older than this is no longer shown when a query fails.
const KEEP_FOR: Duration = Duration::from_secs(2 * 60 * 60);
const MAX_RESPONSE_BYTES: usize = 16 * 1024;
const MAX_ERROR_BYTES: usize = 1024;
/// The worker's own HTTP timeout is 6 s; this bounds a hung process.
const WORKER_DEADLINE: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Condition {
    Clear,
    Cloudy,
    Fog,
    Rain,
    Snow,
    Thunderstorm,
}

impl Condition {
    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 | 1 => Self::Clear,
            2 | 3 => Self::Cloudy,
            45 | 48 => Self::Fog,
            51 | 53 | 55..=57 | 61 | 63 | 65..=67 | 80..=82 => Self::Rain,
            71 | 73 | 75 | 77 | 85 | 86 => Self::Snow,
            95..=97 | 99 => Self::Thunderstorm,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reading {
    condition: Option<Condition>,
    pub temperature: i16,
    pub high_low: Option<(i16, i16)>,
}

impl Reading {
    pub fn glyph(self) -> Option<char> {
        Some(match self.condition? {
            Condition::Clear => '\u{2600}',
            Condition::Cloudy => '\u{2601}',
            Condition::Fog => '\u{224b}',
            Condition::Rain => '\u{2602}',
            Condition::Snow => '\u{2744}',
            Condition::Thunderstorm => '\u{26a1}',
        })
    }
}

fn parse_forecast(body: &[u8]) -> Option<Reading> {
    #[derive(Deserialize)]
    struct Forecast {
        current: Current,
        daily: Option<Daily>,
    }

    #[derive(Deserialize)]
    struct Current {
        temperature_2m: f64,
        weather_code: u8,
    }

    #[derive(Deserialize)]
    struct Daily {
        temperature_2m_max: Vec<Option<f64>>,
        temperature_2m_min: Vec<Option<f64>>,
    }

    fn degrees(value: f64) -> Option<i16> {
        (-150.0..=150.0)
            .contains(&value)
            .then_some(value.round() as i16)
    }

    let forecast: Forecast = serde_json::from_slice(body).ok()?;
    let high_low = forecast.daily.and_then(|day| {
        let high = degrees((*day.temperature_2m_max.first()?)?)?;
        let low = degrees((*day.temperature_2m_min.first()?)?)?;
        (high >= low).then_some((high, low))
    });
    Some(Reading {
        temperature: degrees(forecast.current.temperature_2m)?,
        condition: Condition::from_code(forecast.current.weather_code),
        high_low,
    })
}

/// How long to wait after an attempt before the next visible request.
fn due(last_attempt: Option<Instant>, failed: bool, now: Instant) -> bool {
    let wait = if failed { RETRY_AFTER } else { VALID_FOR };
    last_attempt.is_none_or(|last| now.saturating_duration_since(last) >= wait)
}

#[derive(Default)]
pub struct Cache {
    notify: isize,
    settings: Weather,
    last_attempt: Option<Instant>,
    /// The last attempt brought nothing: the next reveal retries sooner.
    failed: bool,
    /// The last good reading and when it arrived.
    reading: Option<(Reading, Instant)>,
    pending: Option<Receiver<Option<Reading>>>,
    discard_pending: bool,
}

impl Cache {
    pub fn new(notify: HWND) -> Self {
        Self {
            notify: notify.0 as isize,
            ..Self::default()
        }
    }

    pub fn configure(&mut self, settings: &Weather) -> bool {
        if &self.settings == settings {
            return false;
        }
        self.settings = settings.clone();
        self.last_attempt = None;
        self.failed = false;
        self.reading = None;
        // Allow the child to exit before launching another one. Its result
        // belongs to the old settings even if they are later selected again.
        self.discard_pending = self.pending.is_some();
        true
    }

    pub fn reveal(&mut self) {
        let notify = self.notify;
        self.reveal_at(Instant::now(), |latitude, longitude| {
            let (send, receive) = mpsc::channel();
            thread::Builder::new()
                .name("muelle-weather".into())
                .spawn(move || {
                    let result = fetch_in_child(latitude, longitude);
                    if send.send(result).is_ok() {
                        // SAFETY: No borrowed payload crosses the thread boundary.
                        let _ = unsafe {
                            PostMessageW(Some(HWND(notify as _)), READY, WPARAM(0), LPARAM(0))
                        };
                    }
                })
                .ok()
                .map(|_| receive)
        });
    }

    fn reveal_at(
        &mut self,
        now: Instant,
        start: impl FnOnce(f64, f64) -> Option<Receiver<Option<Reading>>>,
    ) {
        let settings = &self.settings;
        if !settings.enabled
            || !settings.has_valid_coordinates()
            || self.pending.is_some()
            || !due(self.last_attempt, self.failed, now)
        {
            return;
        }
        // The previous reading stays on screen until the new one arrives.
        self.last_attempt = Some(now);
        self.pending = start(settings.latitude, settings.longitude);
        self.failed = self.pending.is_none();
    }

    /// Consumes the worker result on the dock's UI thread.
    pub fn poll(&mut self) -> bool {
        self.poll_at(Instant::now())
    }

    fn poll_at(&mut self, now: Instant) -> bool {
        let Some(pending) = &self.pending else {
            return false;
        };
        let fetched = match pending.try_recv() {
            Ok(fetched) => fetched,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => None,
        };
        self.pending = None;
        if std::mem::take(&mut self.discard_pending) {
            return false;
        }
        self.failed = fetched.is_none();
        match fetched {
            Some(reading) => {
                self.reading = Some((reading, now));
                true
            }
            // A failed query keeps a recent reading; an old one goes.
            None => {
                let stale = self.reading.is_some() && self.reading_at(now).is_none();
                if stale {
                    self.reading = None;
                }
                stale
            }
        }
    }

    pub fn reading(&self) -> Option<Reading> {
        self.reading_at(Instant::now())
    }

    fn reading_at(&self, now: Instant) -> Option<Reading> {
        self.reading
            .filter(|(_, at)| now.saturating_duration_since(*at) < KEEP_FOR)
            .map(|(reading, _)| reading)
    }
}

fn fetch_in_child(latitude: f64, longitude: f64) -> Option<Reading> {
    run_child(latitude, longitude)
        .inspect_err(|error| muelle_shell::log::line(&format!("tiempo: {error:#}")))
        .ok()
}

fn run_child(latitude: f64, longitude: f64) -> Result<Reading> {
    // Like the usage worker, release the HTTP runtime and TLS heap on exit.
    let mut child = Command::new(env::current_exe()?)
        .args([WORKER_FLAG, &latitude.to_string(), &longitude.to_string()])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("no se pudo iniciar la consulta")?;
    // Both outputs are a few hundred bytes, well within the pipe buffers,
    // so the child never blocks on them before it exits.
    // SAFETY: The handle belongs to the live child until `wait` below.
    let finished = unsafe {
        WaitForSingleObject(
            HANDLE(child.as_raw_handle()),
            WORKER_DEADLINE.as_millis() as u32,
        )
    } == WAIT_OBJECT_0;
    if !finished {
        let _ = child.kill();
        let _ = child.wait();
        bail!(
            "la consulta no respondió en {} s y se cerró",
            WORKER_DEADLINE.as_secs()
        );
    }
    let status = child.wait()?;
    let stdout = read_capped(child.stdout.take(), MAX_RESPONSE_BYTES);
    if !status.success() {
        let stderr = read_capped(child.stderr.take(), MAX_ERROR_BYTES);
        bail!("la consulta falló ({status}): {}", error_summary(&stderr));
    }
    serde_json::from_slice(&stdout).context("respuesta de la consulta no válida")
}

fn read_capped(pipe: Option<impl Read>, limit: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(pipe) = pipe {
        let _ = pipe.take(limit as u64).read_to_end(&mut bytes);
    }
    bytes
}

/// The worker's error on one line. It never holds the coordinates: the
/// worker strips the request URL from its errors.
fn error_summary(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim().trim_start_matches("Error:").trim();
    if text.is_empty() {
        return "sin detalles".into();
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn worker_main(latitude: &str, longitude: &str) -> Result<()> {
    let latitude: f64 = latitude.parse()?;
    let longitude: f64 = longitude.parse()?;
    ensure!((-90.0..=90.0).contains(&latitude), "latitud fuera de rango");
    ensure!(
        (-180.0..=180.0).contains(&longitude),
        "longitud fuera de rango"
    );
    let client = Client::builder()
        .timeout(Duration::from_secs(6))
        .redirect(Policy::none())
        .build()
        .map_err(reqwest::Error::without_url)?;
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}\
         &current=temperature_2m,weather_code&daily=temperature_2m_max,temperature_2m_min\
         &temperature_unit=celsius&timezone=auto&forecast_days=1"
    );
    let mut body = Vec::new();
    // The dock logs this error: keep the URL, with the coordinates, out of it.
    client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(reqwest::Error::without_url)?
        .take((MAX_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut body)?;
    ensure!(
        body.len() <= MAX_RESPONSE_BYTES,
        "pronóstico demasiado grande"
    );
    let reading = parse_forecast(&body).context("pronóstico no válido")?;
    serde_json::to_writer(std::io::stdout().lock(), &reading)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wmo_codes_choose_the_weather_icon() {
        use Condition::*;
        for (code, expected) in [
            (0, Some(Clear)),
            (1, Some(Clear)),
            (2, Some(Cloudy)),
            (48, Some(Fog)),
            (56, Some(Rain)),
            (82, Some(Rain)),
            (77, Some(Snow)),
            (86, Some(Snow)),
            (97, Some(Thunderstorm)),
            (99, Some(Thunderstorm)),
            (255, None),
        ] {
            assert_eq!(Condition::from_code(code), expected, "WMO {code}");
        }
    }

    #[test]
    fn parses_current_weather_and_todays_high_and_low() {
        let body = br#"{
            "current": {"time":"2026-11-26T16:00","temperature_2m":12.6,"weather_code":3},
            "daily": {"temperature_2m_max":[15.8,18.0],"temperature_2m_min":[4.1,5.0]}
        }"#;
        assert_eq!(
            parse_forecast(body),
            Some(Reading {
                condition: Some(Condition::Cloudy),
                temperature: 13,
                high_low: Some((16, 4)),
            })
        );
    }

    #[test]
    fn keeps_current_weather_when_the_daily_values_are_missing() {
        let body = br#"{
            "current":{"temperature_2m":-2.6,"weather_code":71},
            "daily":{"temperature_2m_max":[null],"temperature_2m_min":[]}
        }"#;
        assert_eq!(
            parse_forecast(body).and_then(|reading| reading.high_low),
            None
        );
        assert_eq!(
            parse_forecast(body).map(|reading| reading.temperature),
            Some(-3)
        );
        assert_eq!(parse_forecast(br#"{"error":true}"#), None);
    }

    #[test]
    fn weather_attempt_waits_thirty_minutes_or_five_after_a_failure() {
        let started = Instant::now();
        assert!(due(None, false, started));
        assert!(!due(
            Some(started),
            false,
            started + Duration::from_secs(1799)
        ));
        assert!(due(
            Some(started),
            false,
            started + Duration::from_secs(1800)
        ));
        assert!(!due(
            Some(started),
            true,
            started + Duration::from_secs(299)
        ));
        assert!(due(Some(started), true, started + Duration::from_secs(300)));
    }

    const SUNNY: Reading = Reading {
        condition: Some(Condition::Clear),
        temperature: 21,
        high_low: None,
    };

    /// A cache with coordinates other than the default ones.
    fn cache() -> Cache {
        let mut cache = Cache::default();
        cache.configure(&Weather {
            latitude: 40.0,
            ..Weather::default()
        });
        cache
    }

    /// The fake queries a cache started, still waiting for an answer.
    #[derive(Default)]
    struct Queries {
        started: u32,
        waiting: Vec<mpsc::Sender<Option<Reading>>>,
    }

    impl Queries {
        /// Reveals at `now`; true when that started a query.
        fn reveal(&mut self, cache: &mut Cache, now: Instant) -> bool {
            let before = self.started;
            cache.reveal_at(now, |_, _| {
                self.started += 1;
                let (send, receive) = mpsc::channel();
                self.waiting.push(send);
                Some(receive)
            });
            self.started > before
        }

        /// Answers the latest query, as the worker thread would.
        fn answer(&mut self, reading: Option<Reading>) -> bool {
            self.waiting
                .pop()
                .is_some_and(|send| send.send(reading).is_ok())
        }

        /// Reveals and answers at once.
        fn fetch(&mut self, cache: &mut Cache, now: Instant, reading: Option<Reading>) -> bool {
            self.reveal(cache, now) && self.answer(reading)
        }
    }

    #[test]
    fn reveal_asks_once_per_half_hour_and_keeps_the_reading_meanwhile() {
        let t0 = Instant::now();
        let mut cache = cache();
        let mut queries = Queries::default();
        assert!(queries.reveal(&mut cache, t0), "the first reveal asks");
        assert!(!queries.reveal(&mut cache, t0), "one query at a time");
        assert!(!cache.poll_at(t0), "nothing yet");
        assert!(queries.answer(Some(SUNNY)));
        let t1 = t0 + Duration::from_secs(2);
        assert!(cache.poll_at(t1));
        assert_eq!(cache.reading_at(t1), Some(SUNNY));

        assert!(!queries.reveal(&mut cache, t0 + Duration::from_secs(29 * 60)));
        let later = t0 + Duration::from_secs(30 * 60);
        assert!(queries.reveal(&mut cache, later), "expired");
        // No blank capsule while the new query runs.
        assert!(!cache.poll_at(later));
        assert_eq!(cache.reading_at(later), Some(SUNNY));
        assert_eq!(queries.started, 2);
    }

    #[test]
    fn a_failed_query_keeps_a_recent_reading_and_retries_after_five_minutes() {
        let t0 = Instant::now();
        let mut cache = cache();
        let mut queries = Queries::default();
        assert!(queries.fetch(&mut cache, t0, Some(SUNNY)));
        assert!(cache.poll_at(t0));

        let t1 = t0 + Duration::from_secs(30 * 60);
        assert!(queries.fetch(&mut cache, t1, None));
        assert!(!cache.poll_at(t1), "the capsule keeps what it shows");
        assert_eq!(cache.reading_at(t1), Some(SUNNY));
        assert!(!queries.reveal(&mut cache, t1 + Duration::from_secs(299)));
        let t2 = t1 + Duration::from_secs(300);
        assert!(queries.reveal(&mut cache, t2), "retried sooner");
        // A worker thread that dies without answering fails as well.
        queries.waiting.clear();
        assert!(!cache.poll_at(t2));
        assert_eq!(cache.reading_at(t2), Some(SUNNY));

        // Past two hours a failure no longer shows the old reading.
        let t3 = t0 + Duration::from_secs(2 * 60 * 60);
        assert!(queries.fetch(&mut cache, t3, None));
        assert!(cache.poll_at(t3), "the stale reading goes");
        assert_eq!(cache.reading_at(t3), None);
        assert_eq!(queries.started, 4);

        // A success returns to the half-hour rhythm.
        let t4 = t3 + Duration::from_secs(300);
        assert!(queries.fetch(&mut cache, t4, Some(SUNNY)));
        assert!(cache.poll_at(t4));
        assert!(!queries.reveal(&mut cache, t4 + Duration::from_secs(300)));
    }

    #[test]
    fn configure_forgets_the_reading_and_asks_on_the_next_reveal() {
        let t0 = Instant::now();
        let mut cache = cache();
        let mut queries = Queries::default();
        assert!(queries.fetch(&mut cache, t0, Some(SUNNY)));
        assert!(cache.poll_at(t0));
        let settings = cache.settings.clone();
        assert!(!cache.configure(&settings), "unchanged");
        assert_eq!(cache.reading_at(t0), Some(SUNNY));

        assert!(cache.configure(&Weather {
            longitude: -3.0,
            ..settings
        }));
        assert_eq!(cache.reading_at(t0), None);
        let moved = t0 + Duration::from_secs(60);
        assert!(queries.reveal(&mut cache, moved), "no half-hour wait");

        // Off, nothing is asked; turned back on, the next reveal asks.
        let off = Weather {
            enabled: false,
            ..cache.settings.clone()
        };
        assert!(cache.configure(&off));
        assert!(!queries.reveal(&mut cache, moved));
        assert!(cache.configure(&Weather {
            enabled: true,
            ..off
        }));
        assert!(!queries.reveal(&mut cache, moved));
        assert!(queries.answer(Some(SUNNY)));
        assert!(!cache.poll_at(moved));
        assert!(queries.reveal(&mut cache, moved));
        assert_eq!(queries.started, 3);
    }

    #[test]
    fn changing_settings_does_not_overlap_workers_or_publish_stale_weather() {
        let t0 = Instant::now();
        let mut cache = cache();
        let mut queries = Queries::default();
        assert!(queries.reveal(&mut cache, t0));
        assert!(cache.configure(&Weather {
            longitude: -3.0,
            ..Weather::default()
        }));
        assert!(!queries.reveal(&mut cache, t0));
        assert!(queries.answer(Some(SUNNY)));
        assert!(!cache.poll_at(t0));
        assert_eq!(cache.reading_at(t0), None);
        assert!(queries.reveal(&mut cache, t0));
    }

    #[test]
    fn coordinates_out_of_range_never_start_a_query() {
        let mut cache = Cache::default();
        cache.configure(&Weather {
            latitude: 91.0,
            ..Weather::default()
        });
        let mut queries = Queries::default();
        assert!(!queries.reveal(&mut cache, Instant::now()));
        assert_eq!(queries.started, 0);
    }

    #[test]
    fn the_logged_worker_error_fits_on_one_line() {
        let stderr = b"Error: error sending request\n\nCaused by:\n    0: dns error\n";
        assert_eq!(
            error_summary(stderr),
            "error sending request Caused by: 0: dns error"
        );
        assert_eq!(error_summary(b"  \r\n"), "sin detalles");
    }
}
