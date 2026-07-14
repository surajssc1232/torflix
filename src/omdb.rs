use anyhow::Result;
use serde::Deserialize;

#[derive(Clone, Debug, Default)]
pub struct Ratings {
    pub imdb: Option<String>,
    pub rt: Option<String>,
}

#[derive(Deserialize)]
struct OmdbResponse {
    #[serde(rename = "imdbRating", default)]
    imdb_rating: String,
    #[serde(rename = "Ratings", default)]
    ratings: Vec<OmdbRating>,
    #[serde(rename = "Response", default)]
    response: String,
}

#[derive(Deserialize)]
struct OmdbRating {
    #[serde(rename = "Source")]
    source: String,
    #[serde(rename = "Value")]
    value: String,
}

pub fn fetch(title: &str, year: &str, api_key: &str) -> Result<Ratings> {
    let title_enc = title.replace(' ', "+").replace('&', "%26");
    let url = if year.is_empty() {
        format!(
            "http://www.omdbapi.com/?t={}&type=movie&apikey={}",
            title_enc, api_key
        )
    } else {
        format!(
            "http://www.omdbapi.com/?t={}&y={}&type=movie&apikey={}",
            title_enc, year, api_key
        )
    };

    let resp = minreq::get(&url).with_timeout(10).send()?;
    let o: OmdbResponse = resp.json()?;

    if o.response != "True" {
        return Ok(Ratings::default());
    }

    let imdb = if o.imdb_rating.is_empty() || o.imdb_rating == "N/A" {
        None
    } else {
        Some(format!("{}/10", o.imdb_rating))
    };

    let rt = o
        .ratings
        .iter()
        .find(|r| r.source == "Rotten Tomatoes")
        .map(|r| r.value.clone());

    Ok(Ratings { imdb, rt })
}
