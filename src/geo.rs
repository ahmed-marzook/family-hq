//! Places and coordinates.
//!
//! * Coordinates → nearest town (OpenStreetMap's Nominatim), for when
//!   someone shares their location in WhatsApp.
//! * Town → coordinates (Open-Meteo's geocoder), for weather forecasts.
//!
//! Both are free and need no API key. Nominatim asks for an identifying
//! User-Agent and at most one request a second, which a family bot (one
//! lookup per shared location) is far below.

use std::collections::HashMap;

use serde::Deserialize;
use tokio::sync::Mutex;

use crate::error::AppError;

/// Coordinates in decimal degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coordinates {
    pub latitude: f64,
    pub longitude: f64,
}

/// What a shared location is nearest to.
#[derive(Debug, Clone, PartialEq)]
pub struct NearestPlace {
    /// Names to try for prayer times, most specific first: e.g.
    /// ["Bletchley", "Milton Keynes", "Buckinghamshire"].
    pub names: Vec<String>,
    /// 2-letter country code, upper case ("GB").
    pub country: String,
}

pub struct Geo {
    http: reqwest::Client,
    nominatim_url: String,
    geocoding_url: String,
    /// Town → coordinates, remembered: a town doesn't move.
    coordinates: Mutex<HashMap<(String, String), Coordinates>>,
}

impl Default for Geo {
    fn default() -> Self {
        Self::new()
    }
}

impl Geo {
    pub fn new() -> Self {
        Geo {
            // Nominatim refuses requests without a real User-Agent; the
            // shared client sends one.
            http: crate::http::client(),
            nominatim_url: "https://nominatim.openstreetmap.org".to_string(),
            geocoding_url: "https://geocoding-api.open-meteo.com/v1".to_string(),
            coordinates: Mutex::new(HashMap::new()),
        }
    }

    /// The same, but asking a fake geocoder at `geocoding_url`, so tests
    /// never reach the real one.
    #[cfg(test)]
    pub(crate) fn with_geocoding_url(geocoding_url: &str) -> Self {
        Geo {
            geocoding_url: geocoding_url.to_string(),
            ..Geo::new()
        }
    }

    /// The town (and the bigger places around it) nearest to a point.
    pub async fn nearest_place(&self, at: Coordinates) -> Result<NearestPlace, AppError> {
        let reply: ReverseReply = self
            .http
            .get(format!("{}/reverse", self.nominatim_url))
            .query(&[
                ("lat", at.latitude.to_string()),
                ("lon", at.longitude.to_string()),
                ("format", "jsonv2".to_string()),
                // 12 = town level: "Olney" rather than a street, or just
                // the county (which is what city level gives for villages).
                ("zoom", "12".to_string()),
                ("addressdetails", "1".to_string()),
                // English names ("Colombo", not "කොළඹ").
                ("accept-language", "en".to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        nearest_from_reply(reply)
            .ok_or_else(|| AppError::Geo("no town or country near that location".to_string()))
    }

    /// Where a town is, for prayer times and the weather. Remembered, since
    /// towns don't move.
    pub async fn coordinates_of(&self, city: &str, country: &str) -> Result<Coordinates, AppError> {
        let key = (city.to_lowercase(), country.to_uppercase());
        if let Some(found) = self.coordinates.lock().await.get(&key) {
            return Ok(*found);
        }
        let found = self.find_town(city, country).await?.coordinates;
        self.coordinates.lock().await.insert(key, found);
        Ok(found)
    }

    /// Looks a town up by name: its proper name, country code, coordinates
    /// and timezone. Used for `/settings city …`, so a typo is caught and
    /// the timezone comes for free.
    pub async fn find_town(&self, city: &str, country: &str) -> Result<Town, AppError> {
        let reply: GeocodingReply = self
            .http
            .get(format!("{}/search", self.geocoding_url))
            .query(&[
                ("name", city),
                ("count", "1"),
                // Only a 2-letter code narrows the search; a full country
                // name ("United Kingdom") isn't understood, so leave it out.
                ("countryCode", if country.len() == 2 { country } else { "" }),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        reply
            .results
            .and_then(|r| r.into_iter().next())
            .map(|r| Town {
                name: r.name,
                country: r.country_code.to_uppercase(),
                coordinates: Coordinates {
                    latitude: r.latitude,
                    longitude: r.longitude,
                },
                timezone: r.timezone.parse().ok(),
            })
            .ok_or_else(|| AppError::Geo(format!("couldn't find {city}, {country}")))
    }
}

/// Nominatim's reply: `{"address":{"town":"Olney","county":"...","country_code":"gb"}}`.
#[derive(Deserialize)]
struct ReverseReply {
    address: Option<Address>,
}

/// The parts of an address we can use, from most to least specific.
#[derive(Deserialize, Default)]
struct Address {
    town: Option<String>,
    village: Option<String>,
    city: Option<String>,
    municipality: Option<String>,
    county: Option<String>,
    state: Option<String>,
    country_code: Option<String>,
}

/// Picks the place names out of a reply, most specific first, skipping
/// missing and repeated ones. Pure, so it's tested with saved replies.
fn nearest_from_reply(reply: ReverseReply) -> Option<NearestPlace> {
    let a = reply.address?;
    let country = a.country_code?.to_uppercase();
    let mut names: Vec<String> = Vec::new();
    for name in [a.town, a.village, a.city, a.municipality, a.county, a.state]
        .into_iter()
        .flatten()
    {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    (!names.is_empty()).then_some(NearestPlace { names, country })
}

/// Open-Meteo geocoding: `{"results":[{"name":"...","latitude":..,"longitude":..}]}`.
/// No `results` at all when nothing matched.
#[derive(Deserialize)]
struct GeocodingReply {
    results: Option<Vec<GeocodingResult>>,
}

#[derive(Deserialize)]
struct GeocodingResult {
    name: String,
    country_code: String,
    latitude: f64,
    longitude: f64,
    timezone: String,
}

/// A town found by name.
#[derive(Debug, Clone, PartialEq)]
pub struct Town {
    /// As the geocoder spells it ("Crawley", not "crawley").
    pub name: String,
    /// 2-letter code, upper case.
    pub country: String,
    pub coordinates: Coordinates,
    /// `None` if the geocoder's timezone name is one we don't know.
    pub timezone: Option<chrono_tz::Tz>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reverse(json: &str) -> Option<NearestPlace> {
        nearest_from_reply(serde_json::from_str(json).unwrap())
    }

    #[test]
    fn town_comes_before_the_city_around_it() {
        // Real reply shape for a point in Bletchley, Milton Keynes.
        let place = reverse(
            r#"{"address":{"city":"Milton Keynes","town":"Bletchley",
                "county":"Buckinghamshire","state":"England","country_code":"gb"}}"#,
        )
        .unwrap();
        assert_eq!(place.names[0], "Bletchley");
        assert_eq!(place.names[1], "Milton Keynes");
        assert_eq!(place.country, "GB");
    }

    #[test]
    fn a_village_without_a_town_still_has_names() {
        let place = reverse(
            r#"{"address":{"village":"Sherington","county":"City of Milton Keynes",
                "country_code":"gb"}}"#,
        )
        .unwrap();
        assert_eq!(place.names, ["Sherington", "City of Milton Keynes"]);
    }

    #[test]
    fn repeated_names_appear_once() {
        let place = reverse(
            r#"{"address":{"city":"Colombo","county":"Colombo","state":"Western Province",
                "country_code":"lk"}}"#,
        )
        .unwrap();
        assert_eq!(place.names, ["Colombo", "Western Province"]);
        assert_eq!(place.country, "LK");
    }

    #[test]
    fn the_middle_of_the_sea_has_no_place() {
        assert_eq!(reverse(r#"{"error":"Unable to geocode"}"#), None);
        assert_eq!(reverse(r#"{"address":{"country_code":"gb"}}"#), None);
    }
}
