//! The headphone presets of OPRA (github.com/opra-project/OPRA), bundled in
//! the app as a gzipped extract and mapped onto the EQ's bands. Its data is
//! CC BY-SA 4.0, see `data/OPRA-LICENSE.md`; `scripts/opra_extract.py` makes
//! the extract from OPRA's database.

use std::io::Read;

use flate2::read::GzDecoder;
use serde::Deserialize;

use crate::prefs::{EqBand, Equalizer};

const BUNDLE: &[u8] = include_bytes!("../data/opra.json.gz");
/// The Q of a low pass, whose OPRA entry gives a slope of 12 dB per octave
/// instead: the second order filter's usual.
const LOW_PASS_Q: f64 = 0.71;

/// Where a product sits in the catalog: its vendor's place, then its own.
pub type ProductId = (usize, usize);

#[derive(Debug, Deserialize)]
pub struct Vendor {
    pub name: String,
    pub products: Vec<Product>,
}

#[derive(Debug, Deserialize)]
pub struct Product {
    pub name: String,
    pub subtype: String,
    pub presets: Vec<Preset>,
}

/// An EQ made by `author` for a product, in OPRA's terms.
#[derive(Debug, Deserialize)]
pub struct Preset {
    pub author: String,
    pub details: String,
    pub link: Option<String>,
    pub gain: f64,
    /// Each band's type, frequency, gain and Q; a low pass has no gain or Q.
    pub bands: Vec<(String, f64, Option<f64>, Option<f64>)>,
}

#[derive(Debug, Deserialize)]
struct Extract {
    vendors: Vec<Vendor>,
}

/// Every vendor and product, with what a search reads of them.
#[derive(Debug)]
pub struct Catalog {
    vendors: Vec<Vendor>,
    searched: Vec<(ProductId, String)>,
}

/// The catalog that is bundled. Unzipping and parsing take a moment, so
/// this is for a thread that isn't the interface's.
pub fn bundled() -> Result<Catalog, String> {
    parse(GzDecoder::new(BUNDLE))
}

fn parse(json: impl Read) -> Result<Catalog, String> {
    let extract: Extract = serde_json::from_reader(json).map_err(|problem| format!("{problem}"))?;
    let searched = extract
        .vendors
        .iter()
        .enumerate()
        .flat_map(|(vendor, found)| {
            found
                .products
                .iter()
                .enumerate()
                .map(move |(product, item)| {
                    let name = format!("{} {}", found.name, item.name);
                    ((vendor, product), name.to_lowercase())
                })
        })
        .collect();
    Ok(Catalog {
        vendors: extract.vendors,
        searched,
    })
}

impl Catalog {
    pub fn vendor_count(&self) -> usize {
        self.vendors.len()
    }

    pub fn product_count(&self) -> usize {
        self.searched.len()
    }

    pub fn vendor(&self, (vendor, _): ProductId) -> &Vendor {
        &self.vendors[vendor]
    }

    pub fn product(&self, (vendor, product): ProductId) -> &Product {
        &self.vendors[vendor].products[product]
    }

    /// The products whose vendor and name hold every word of `query`, in
    /// the catalog's order; none for an empty query.
    pub fn search(&self, query: &str) -> Vec<ProductId> {
        let query = query.to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        if words.is_empty() {
            return Vec::new();
        }
        self.searched
            .iter()
            .filter(|(_, name)| words.iter().all(|word| name.contains(word)))
            .map(|(id, _)| *id)
            .collect()
    }
}

impl Preset {
    /// The preset as the EQ holds it, or why the EQ can't.
    pub fn equalizer(&self) -> Result<Equalizer, String> {
        let bands = self
            .bands
            .iter()
            .map(|(kind, frequency, gain, q)| band(kind, *frequency, *gain, *q))
            .collect::<Result<_, _>>()?;
        Ok(Equalizer {
            preamp: self.gain,
            bands,
        })
    }
}

fn band(kind: &str, frequency: f64, gain: Option<f64>, q: Option<f64>) -> Result<EqBand, String> {
    let (filter, gain, q) = match kind {
        "peak_dip" => (0, gain.unwrap_or(0.0), q),
        "low_shelf" => (1, gain.unwrap_or(0.0), q),
        "high_shelf" => (2, gain.unwrap_or(0.0), q),
        "low_pass" => (3, 0.0, Some(q.unwrap_or(LOW_PASS_Q))),
        _ => return Err(format!("The EQ has no “{kind}” filter.")),
    };
    let q = q.ok_or_else(|| format!("Its “{kind}” filter has no Q."))?;
    Ok(EqBand {
        filter,
        frequency,
        gain,
        q,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: &str = r#"{"vendors":[
        {"name":"Sennheiser","products":[
            {"name":"HD 650","subtype":"over_the_ear","presets":[
                {"author":"oratory1990","details":"Harman Target","link":"https://example.org/hd650.pdf","gain":-6.5,
                 "bands":[["low_shelf",105,2.5,0.7],["peak_dip",2500,-3.1,1.4],["high_shelf",10000,-4,0.71]]}]},
            {"name":"HD 600","subtype":"over_the_ear","presets":[
                {"author":"AutoEQ","details":"Measured by Rtings","gain":-3,
                 "bands":[["low_pass",8000,null,null]]}]}]},
        {"name":"Moondrop","products":[
            {"name":"Blessing 2","subtype":"in_ear","presets":[
                {"author":"AutoEQ","details":"x","gain":0,"bands":[["notch",1000,1,1]]}]}]}]}"#;

    fn small() -> Catalog {
        parse(SMALL.as_bytes()).unwrap()
    }

    #[test]
    fn a_preset_becomes_the_eq_with_its_preamp() {
        let catalog = small();
        let preset = &catalog.product((0, 0)).presets[0];
        assert_eq!(
            preset.link.as_deref(),
            Some("https://example.org/hd650.pdf")
        );
        let eq = preset.equalizer().unwrap();
        assert_eq!(eq.preamp, -6.5);
        let bands: Vec<(i32, f64, f64, f64)> = eq
            .bands
            .iter()
            .map(|band| (band.filter, band.frequency, band.gain, band.q))
            .collect();
        assert_eq!(
            bands,
            [
                (1, 105.0, 2.5, 0.7),
                (0, 2_500.0, -3.1, 1.4),
                (2, 10_000.0, -4.0, 0.71)
            ]
        );
    }

    #[test]
    fn a_low_pass_gets_no_gain_and_the_usual_q() {
        let catalog = small();
        let eq = catalog.product((0, 1)).presets[0].equalizer().unwrap();
        assert_eq!(
            eq.bands,
            [EqBand {
                filter: 3,
                frequency: 8_000.0,
                gain: 0.0,
                q: LOW_PASS_Q
            }]
        );
    }

    #[test]
    fn a_preset_the_eq_cannot_hold_says_why() {
        let catalog = small();
        let problem = catalog.product((1, 0)).presets[0].equalizer().unwrap_err();
        assert!(problem.contains("notch"), "{problem}");
        let no_q = band("peak_dip", 100.0, Some(1.0), None).unwrap_err();
        assert!(no_q.contains("no Q"), "{no_q}");
    }

    #[test]
    fn a_search_needs_every_word_in_the_vendor_and_the_product() {
        let catalog = small();
        assert_eq!(catalog.search("hd"), [(0, 0), (0, 1)]);
        assert_eq!(catalog.search("  SENN  650 "), [(0, 0)]);
        assert_eq!(catalog.search("650 sennheiser"), [(0, 0)]);
        assert_eq!(catalog.search("blessing"), [(1, 0)]);
        assert_eq!(catalog.search("moondrop hd"), []);
        assert_eq!(catalog.search("   "), [], "an empty search finds nothing");
        assert_eq!(catalog.search("é"), []);
    }

    #[test]
    fn bad_json_is_an_error() {
        assert!(parse("{".as_bytes()).is_err());
    }

    #[test]
    fn the_bundle_holds_the_catalog_and_every_preset_maps() {
        let catalog = bundled().unwrap();
        assert_eq!(catalog.vendor_count(), 714);
        assert_eq!(catalog.product_count(), 6_229);
        let hd650 = catalog.search("sennheiser hd 650");
        assert!(!hd650.is_empty());
        let unmapped = catalog
            .vendors
            .iter()
            .flat_map(|vendor| &vendor.products)
            .flat_map(|product| &product.presets)
            .filter(|preset| preset.equalizer().is_err())
            .count();
        assert_eq!(unmapped, 0);
        assert!(catalog.vendor(hd650[0]).name.starts_with("Sennheiser"));
    }
}
