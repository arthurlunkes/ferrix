//! nginx geoip (`$geoip_*` variables) from MaxMind `.mmdb` databases.
//! Databases are opened once and kept for the life of the process; a changed
//! path in the config opens the new file.

use maxminddb::{geoip2, Reader};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, LazyLock, RwLock};

use crate::config::GeoIpConfig;

type Readers = HashMap<String, Option<Arc<Reader<Vec<u8>>>>>;
static READERS: LazyLock<RwLock<Readers>> = LazyLock::new(|| RwLock::new(HashMap::new()));

fn reader(path: &str) -> Option<Arc<Reader<Vec<u8>>>> {
    if let Some(r) = READERS.read().ok().and_then(|m| m.get(path).cloned()) {
        return r;
    }
    let r = match Reader::open_readfile(path) {
        Ok(r) => Some(Arc::new(r)),
        Err(e) => {
            tracing::error!(path, error = %e, "cannot open GeoIP database");
            None
        }
    };
    if let Ok(mut m) = READERS.write() {
        m.insert(path.to_string(), r.clone());
    }
    r
}

fn en<'a>(names: &Option<std::collections::BTreeMap<&'a str, &'a str>>) -> String {
    names
        .as_ref()
        .and_then(|n| n.get("en").copied())
        .unwrap_or("")
        .to_string()
}

/// Value of `$<name>` (e.g. `geoip_country_code`) for `ip`.
pub fn variable(cfg: &GeoIpConfig, name: &str, ip: IpAddr) -> Option<String> {
    let field = name.strip_prefix("geoip_")?;
    match field {
        "org" | "asn" => {
            let r = reader(cfg.asn_db.as_deref()?)?;
            let asn: geoip2::Asn = r.lookup(ip).ok()?;
            Some(if field == "org" {
                asn.autonomous_system_organization.unwrap_or("").to_string()
            } else {
                asn.autonomous_system_number
                    .map(|n| n.to_string())
                    .unwrap_or_default()
            })
        }
        "country_code" | "country_name" | "continent_code" => {
            let path = cfg.country_db.as_deref().or(cfg.city_db.as_deref())?;
            let r = reader(path)?;
            let c: geoip2::Country = r.lookup(ip).ok()?;
            Some(match field {
                "country_code" => c.country.and_then(|c| c.iso_code).unwrap_or("").to_string(),
                "country_name" => c.country.map(|c| en(&c.names)).unwrap_or_default(),
                _ => c.continent.and_then(|c| c.code).unwrap_or("").to_string(),
            })
        }
        _ => {
            let path = cfg.city_db.as_deref()?;
            let r = reader(path)?;
            let c: geoip2::City = r.lookup(ip).ok()?;
            Some(match field {
                "city" => c.city.map(|c| en(&c.names)).unwrap_or_default(),
                "city_country_code" => c.country.and_then(|c| c.iso_code).unwrap_or("").to_string(),
                "region" => c
                    .subdivisions
                    .as_ref()
                    .and_then(|s| s.first())
                    .and_then(|s| s.iso_code)
                    .unwrap_or("")
                    .to_string(),
                "region_name" => c
                    .subdivisions
                    .as_ref()
                    .and_then(|s| s.first())
                    .map(|s| en(&s.names))
                    .unwrap_or_default(),
                "postal_code" => c.postal.and_then(|p| p.code).unwrap_or("").to_string(),
                "latitude" => c
                    .location
                    .as_ref()
                    .and_then(|l| l.latitude)
                    .map(|v| format!("{v:.4}"))
                    .unwrap_or_default(),
                "longitude" => c
                    .location
                    .as_ref()
                    .and_then(|l| l.longitude)
                    .map(|v| format!("{v:.4}"))
                    .unwrap_or_default(),
                _ => return None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn city_lookups() {
        let db = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/geoip-test.mmdb"
        );
        let cfg = GeoIpConfig {
            city_db: Some(db.into()),
            ..Default::default()
        };
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        let v = |n: &str| variable(&cfg, n, ip).unwrap_or_default();
        assert_eq!(v("geoip_country_code"), "BR");
        assert_eq!(v("geoip_country_name"), "Brazil");
        assert_eq!(v("geoip_continent_code"), "SA");
        assert_eq!(v("geoip_city"), "Florianopolis");
        assert_eq!(v("geoip_region"), "SC");
        assert_eq!(v("geoip_region_name"), "Santa Catarina");
        assert_eq!(v("geoip_postal_code"), "88000");
        assert_eq!(v("geoip_latitude"), "-27.5900");
        assert_eq!(
            variable(&cfg, "geoip_country_code", "8.8.8.8".parse().unwrap()),
            None
        );
    }
}
