use std::{env, fs, io::Read, path::PathBuf, time::Duration};

use anyhow::Result;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use windows::{
    Devices::Geolocation::Geolocator,
    Foundation::TimeSpan,
    Win32::{
        Foundation::E_ACCESSDENIED,
        System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Coordinates {
    pub latitude: f64,
    pub longitude: f64,
}

impl Coordinates {
    pub fn rounded(latitude: f64, longitude: f64) -> Option<Self> {
        if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
            return None;
        }
        Some(Self {
            latitude: (latitude * 100.0).round() / 100.0,
            longitude: (longitude * 100.0).round() / 100.0,
        })
    }

    fn within_city_range(self, named: Self) -> bool {
        let dlat = (self.latitude - named.latitude).to_radians();
        let dlon = (self.longitude - named.longitude).to_radians();
        let a = (dlat / 2.0).sin().powi(2)
            + self.latitude.to_radians().cos()
                * named.latitude.to_radians().cos()
                * (dlon / 2.0).sin().powi(2);
        2.0 * 6371.0 * a.clamp(0.0, 1.0).sqrt().asin() <= 5.0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Named {
    position: Coordinates,
    city: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Stored {
    last: Coordinates,
    named: Option<Named>,
}

impl Stored {
    fn path() -> Option<PathBuf> {
        Some(PathBuf::from(env::var_os("LOCALAPPDATA")?).join("Muelle/weather.json"))
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut stored: Self = serde_json::from_slice(bytes).ok()?;
        stored.last = Coordinates::rounded(stored.last.latitude, stored.last.longitude)?;
        stored.named = stored.named.and_then(|named| {
            Some(Named {
                position: Coordinates::rounded(named.position.latitude, named.position.longitude)?,
                city: city_label(&named.city)?,
            })
        });
        Some(stored)
    }

    pub fn load() -> Option<Self> {
        let mut bytes = Vec::new();
        fs::File::open(Self::path()?)
            .ok()?
            .take(4096)
            .read_to_end(&mut bytes)
            .ok()?;
        Self::decode(&bytes)
    }

    pub fn save(&self) -> Result<()> {
        if let Some(path) = Self::path()
            && let Some(parent) = path.parent()
        {
            let temporary = path.with_extension("tmp");
            fs::create_dir_all(parent)?;
            fs::write(&temporary, serde_json::to_vec(self)?)?;
            fs::rename(temporary, path)?;
        }
        Ok(())
    }

    fn name_at(&self, position: Coordinates) -> Option<&str> {
        self.named
            .as_ref()
            .filter(|named| position.within_city_range(named.position))
            .map(|named| named.city.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Unavailable {
    Denied,
    NoPosition,
}

impl Unavailable {
    pub const fn text(self) -> &'static str {
        match self {
            Self::Denied => "ubicación de Windows denegada",
            Self::NoPosition => "ubicación de Windows sin posición",
        }
    }
}

// The caller is the one-shot worker. It uses the permission Windows has
// already granted to desktop apps, without opening a permission prompt.
fn windows_position() -> Result<Coordinates, Unavailable> {
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            // SAFETY: Balances initialization on this same worker thread.
            unsafe { RoUninitialize() };
        }
    }
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(|_| Unavailable::NoPosition)?;
    let _apartment = Apartment;
    let classify = |error: windows::core::Error| {
        if error.code() == E_ACCESSDENIED {
            Unavailable::Denied
        } else {
            Unavailable::NoPosition
        }
    };
    let locator = Geolocator::new().map_err(classify)?;
    let maximum_age = TimeSpan {
        Duration: 60 * 10_000_000,
    };
    let timeout = TimeSpan {
        Duration: 4 * 10_000_000,
    };
    let result = locator
        .GetGeopositionAsyncWithAgeAndTimeout(maximum_age, timeout)
        .and_then(|request| request.join())
        .and_then(|position| position.Coordinate()?.Point()?.Position());
    let position = result.map_err(classify)?;
    Coordinates::rounded(position.Latitude, position.Longitude).ok_or(Unavailable::NoPosition)
}

pub(super) fn city_label(city: &str) -> Option<String> {
    let label = city
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect::<String>();
    (!label.is_empty()).then_some(label)
}

fn reverse(client: &Client, at: Coordinates) -> Option<String> {
    #[derive(Deserialize)]
    struct Response {
        address: Address,
    }
    #[derive(Deserialize)]
    struct Address {
        city: Option<String>,
        town: Option<String>,
        village: Option<String>,
        municipality: Option<String>,
        hamlet: Option<String>,
    }
    let url = format!(
        "https://nominatim.openstreetmap.org/reverse?lat={:.2}&lon={:.2}&format=jsonv2&zoom=10&accept-language=es",
        at.latitude, at.longitude
    );
    let response = client
        .get(url)
        .timeout(Duration::from_secs(3))
        .header(reqwest::header::USER_AGENT, "Muelle-fork")
        .send()
        .ok()?
        .error_for_status()
        .ok()?;
    let address = serde_json::from_reader::<_, Response>(response.take(16 * 1024))
        .ok()?
        .address;
    [
        address.city,
        address.town,
        address.village,
        address.municipality,
        address.hamlet,
    ]
    .into_iter()
    .flatten()
    .find_map(|name| city_label(&name))
}

pub(super) struct Selection {
    pub position: Option<Coordinates>,
    pub city: Option<String>,
    pub stored: Option<Stored>,
    pub fallback: Option<Unavailable>,
}

pub(super) fn select(
    client: &Client,
    settings: &muelle_config::Weather,
    previous: Option<Stored>,
) -> Selection {
    let automatic = settings.auto.then(windows_position);
    select_from(settings, previous, automatic, |position| {
        reverse(client, position)
    })
}

fn select_from(
    settings: &muelle_config::Weather,
    previous: Option<Stored>,
    automatic: Option<Result<Coordinates, Unavailable>>,
    name: impl FnOnce(Coordinates) -> Option<String>,
) -> Selection {
    let position = Coordinates::rounded(settings.latitude, settings.longitude);
    let mut selection = Selection {
        position,
        city: city_label(&settings.city),
        stored: None,
        fallback: None,
    };
    match automatic {
        None => {}
        Some(Err(reason)) => {
            selection.fallback = Some(reason);
            if let Some(previous) = previous {
                selection.position = Some(previous.last);
                selection.city = previous.name_at(previous.last).map(str::to_owned);
            }
        }
        Some(Ok(current)) => {
            let mut stored = previous.unwrap_or(Stored {
                last: current,
                named: None,
            });
            if stored.name_at(current).is_none()
                && let Some(city) = name(current)
            {
                stored.named = Some(Named {
                    position: current,
                    city,
                });
            }
            stored.last = current;
            selection.position = Some(current);
            selection.city = stored.name_at(current).map(str::to_owned);
            selection.stored = Some(stored);
        }
    }
    selection
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_rounds_coordinates_before_geocoding_and_forecasting() {
        let requested = Coordinates::rounded(37.7796, -3.7849);
        assert_eq!(requested, Coordinates::rounded(37.78, -3.78));
        assert!(Coordinates::rounded(90.01, 0.0).is_none());
    }

    #[test]
    fn weather_reuses_the_city_until_moving_over_five_kilometers() -> Result<()> {
        let settings = muelle_config::Weather::default();
        let center = Coordinates::rounded(37.78, -3.78).unwrap_or_else(|| unreachable!());
        let near = Coordinates {
            latitude: 37.82,
            ..center
        };
        let far = Coordinates {
            latitude: 37.83,
            ..center
        };
        let first = select_from(&settings, None, Some(Ok(center)), |_| Some("Jaén".into()));
        let saved = Stored::decode(&serde_json::to_vec(&first.stored)?);
        let next = select_from(&settings, saved, Some(Ok(near)), |_| {
            panic!("city is cached")
        });
        assert_eq!(next.city.as_deref(), Some("Jaén"));
        let moved = select_from(&settings, next.stored, Some(Ok(far)), |_| {
            Some("Otra".into())
        });
        assert_eq!(moved.city.as_deref(), Some("Otra"));
        Ok(())
    }

    #[test]
    fn weather_denial_uses_last_position_then_manual_location() {
        let settings = muelle_config::Weather::default();
        let denial = Some(Err(Unavailable::Denied));
        let fallback = select_from(&settings, None, denial, |_| panic!("no geocoding"));
        assert_eq!(fallback.city.as_deref(), Some("Jaén"));
        assert_eq!(fallback.position, Coordinates::rounded(37.78, -3.78));
        let last = Coordinates {
            latitude: 36.72,
            longitude: -4.42,
        };
        let stored = Stored {
            last,
            named: Some(Named {
                position: last,
                city: "Málaga".into(),
            }),
        };
        let fallback = select_from(&settings, Some(stored), denial, |_| panic!("no geocoding"));
        assert_eq!(fallback.position, Some(last));
        assert_eq!(fallback.city.as_deref(), Some("Málaga"));
        assert_eq!(fallback.fallback, Some(Unavailable::Denied));
    }
}
