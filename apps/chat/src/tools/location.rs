//! Where the user is: from the desktop's location service (GeoClue), from
//! the public IP address, or typed in by hand. Off unless the user turns it on.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::prefs::Prefs;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Location {
    pub place: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub source: &'static str,
}

impl Location {
    pub fn describe(&self) -> String {
        let mut s = self.place.clone();
        if let (Some(lat), Some(lon)) = (self.lat, self.lon) {
            s.push_str(&format!(" (latitude {lat:.3}, longitude {lon:.3})"));
        }
        s.push_str(&format!(" — from {}", self.source));
        s
    }
}

static CACHE: Mutex<Option<(String, Instant, Location)>> = Mutex::new(None);

pub async fn lookup(prefs: &Prefs) -> Result<Location> {
    let key = format!("{}|{}", prefs.location_mode, prefs.manual_location);
    if let Some((k, at, loc)) = CACHE.lock().ok().and_then(|c| c.clone())
        && k == key
        && at.elapsed() < Duration::from_secs(600)
    {
        return Ok(loc);
    }
    let loc = match prefs.location_mode.as_str() {
        "manual" => {
            if prefs.manual_location.trim().is_empty() {
                bail!("No location is set. The user can type one in Settings → Location.");
            }
            Location { place: prefs.manual_location.trim().into(), source: "the user's settings", ..Default::default() }
        }
        "ip" => from_ip().await?,
        "desktop" => from_geoclue().await?,
        _ => bail!("Location sharing is off. The user can turn it on in Settings → Location."),
    };
    if let Ok(mut c) = CACHE.lock() {
        *c = Some((key, Instant::now(), loc.clone()));
    }
    Ok(loc)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("nyx-chat/", env!("CARGO_PKG_VERSION"), " (desktop chat client)"))
        .build()
        .unwrap_or_default()
}

async fn from_ip() -> Result<Location> {
    let v: Value = client().get("https://ipapi.co/json/").send().await?.error_for_status().context("IP lookup")?.json().await?;
    if v["error"].as_bool() == Some(true) {
        bail!("IP lookup failed: {}", v["reason"].as_str().unwrap_or("unknown"));
    }
    let place = [v["city"].as_str(), v["region"].as_str(), v["country_name"].as_str()]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(Location { place, lat: v["latitude"].as_f64(), lon: v["longitude"].as_f64(), source: "the IP address (approximate)" })
}

async fn from_geoclue() -> Result<Location> {
    let (lat, lon) = geoclue_coordinates()
        .await
        .map_err(|e| anyhow::anyhow!("The desktop location service didn't answer ({e}). Is GeoClue installed and allowed?"))?;
    let place = reverse_geocode(lat, lon).await.unwrap_or_default();
    Ok(Location { place, lat: Some(lat), lon: Some(lon), source: "the desktop location service" })
}

pub fn geoclue_installed() -> bool {
    std::path::Path::new("/usr/share/dbus-1/system-services/org.freedesktop.GeoClue2.service").exists()
        || std::path::Path::new("/usr/lib/geoclue").exists()
}

async fn geoclue_coordinates() -> zbus::Result<(f64, f64)> {
    use zbus::proxy::CacheProperties;
    use zbus::zvariant::OwnedObjectPath;
    let conn = zbus::Connection::system().await?;
    let proxy = |path: String, iface: &'static str| {
        let conn = conn.clone();
        async move {
            zbus::proxy::Builder::<zbus::Proxy>::new(&conn)
                .destination("org.freedesktop.GeoClue2")?
                .path(path)?
                .interface(iface)?
                .cache_properties(CacheProperties::No)
                .build()
                .await
        }
    };
    let manager = proxy("/org/freedesktop/GeoClue2/Manager".into(), "org.freedesktop.GeoClue2.Manager").await?;
    let client_path: OwnedObjectPath = manager.call_method("GetClient", &()).await?.body().deserialize()?;
    let client = proxy(client_path.to_string(), "org.freedesktop.GeoClue2.Client").await?;
    client.set_property("DesktopId", "io.github.design_nexus.nyx.Chat").await.map_err(zbus::Error::from)?;
    // 4 = city level: enough for weather and local answers.
    client.set_property("RequestedAccuracyLevel", 4u32).await.map_err(zbus::Error::from)?;
    client.call_method("Start", &()).await?;
    let mut found = None;
    for _ in 0..40 {
        let loc: OwnedObjectPath = client.get_property("Location").await?;
        if loc.as_str() != "/" {
            found = Some(loc);
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let _ = client.call_method("Stop", &()).await;
    let loc = found.ok_or_else(|| zbus::Error::Failure("no location yet".into()))?;
    let location = proxy(loc.to_string(), "org.freedesktop.GeoClue2.Location").await?;
    Ok((location.get_property("Latitude").await?, location.get_property("Longitude").await?))
}

async fn reverse_geocode(lat: f64, lon: f64) -> Result<String> {
    let url = format!("https://nominatim.openstreetmap.org/reverse?format=jsonv2&zoom=10&lat={lat}&lon={lon}");
    let v: Value = client().get(url).send().await?.error_for_status()?.json().await?;
    let a = &v["address"];
    let city = ["city", "town", "village", "municipality", "county"].iter().find_map(|k| a[*k].as_str());
    Ok([city, a["state"].as_str(), a["country"].as_str()].into_iter().flatten().collect::<Vec<_>>().join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes() {
        let l = Location { place: "Oslo, Norway".into(), lat: Some(59.9139), lon: Some(10.7522), source: "test" };
        assert_eq!(l.describe(), "Oslo, Norway (latitude 59.914, longitude 10.752) — from test");
    }

    #[test]
    fn off_by_default() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        assert!(rt.block_on(lookup(&Prefs::default())).is_err());
        let p = Prefs { location_mode: "manual".into(), manual_location: "Lisbon".into(), ..Default::default() };
        assert_eq!(rt.block_on(lookup(&p)).unwrap().place, "Lisbon");
    }
}
