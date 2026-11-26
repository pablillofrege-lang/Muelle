//! One-shot forecasts for the zoned menu bar.

use std::{
    env,
    io::Read,
    os::windows::process::CommandExt,
    process::Command,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use muelle_config::Weather;
use reqwest::{blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize};

pub const WORKER_FLAG: &str = "--weather-worker";
const VALID_FOR: Duration = Duration::from_secs(30 * 60);
const MAX_RESPONSE_BYTES: usize = 16 * 1024;

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

fn expired(last_attempt: Option<Instant>, now: Instant) -> bool {
    last_attempt.is_none_or(|last| now.saturating_duration_since(last) >= VALID_FOR)
}

#[derive(Default)]
pub struct Cache {
    settings: Weather,
    last_attempt: Option<Instant>,
    reading: Option<Reading>,
    pending: Option<Receiver<Option<Reading>>>,
}

impl Cache {
    pub fn configure(&mut self, settings: Weather) -> bool {
        if self.settings == settings {
            return false;
        }
        self.settings = settings;
        self.reading = None;
        self.pending = None;
        true
    }

    pub fn reveal(&mut self) {
        let now = Instant::now();
        let Weather {
            enabled,
            latitude,
            longitude,
        } = self.settings;
        if !enabled
            || !(-90.0..=90.0).contains(&latitude)
            || !(-180.0..=180.0).contains(&longitude)
            || !expired(self.last_attempt, now)
        {
            return;
        }
        self.last_attempt = Some(now);
        self.reading = None;
        let (send, receive) = mpsc::channel();
        if thread::Builder::new()
            .name("muelle-weather".into())
            .spawn(move || {
                let reading = fetch_in_child(latitude, longitude);
                let _ = send.send(reading);
            })
            .is_ok()
        {
            self.pending = Some(receive);
        }
    }

    /// Checked by the existing pointer timer, which runs during reveal.
    pub fn poll(&mut self) -> bool {
        let Some(pending) = &self.pending else {
            return false;
        };
        let reading = match pending.try_recv() {
            Ok(reading) => reading,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => None,
        };
        self.pending = None;
        self.reading = reading;
        reading.is_some()
    }

    pub fn reading(&self) -> Option<Reading> {
        self.reading
    }
}

fn fetch_in_child(latitude: f64, longitude: f64) -> Option<Reading> {
    // Like the usage worker, release the HTTP runtime and TLS heap on exit.
    let output = Command::new(env::current_exe().ok()?)
        .args([WORKER_FLAG, &latitude.to_string(), &longitude.to_string()])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
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
        .build()?;
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}\
         &current=temperature_2m,weather_code&daily=temperature_2m_max,temperature_2m_min\
         &temperature_unit=celsius&timezone=auto&forecast_days=1"
    );
    let mut body = Vec::new();
    client
        .get(url)
        .send()?
        .error_for_status()?
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
    fn weather_attempt_expires_after_thirty_minutes() {
        let started = Instant::now();
        assert!(expired(None, started));
        assert!(!expired(Some(started), started + Duration::from_secs(1799)));
        assert!(expired(Some(started), started + Duration::from_secs(1800)));
    }
}
