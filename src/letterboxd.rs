//! Letterboxd ratings.
//!
//! Letterboxd has no public API. `digital-drop-movies.netlify.app` gets the
//! numbers by fetching the film's own page and reading the `ratingValue` and
//! `ratingCount` out of the JSON-LD block it embeds — its `/api/film` endpoint
//! is a serverless wrapper around exactly that, needed there only because a
//! browser can't fetch letterboxd.com itself. torflix is not a browser, so it
//! asks Letterboxd directly: `letterboxd.com/imdb/<id>/` redirects to the film
//! page, and Cinemeta already gives us IMDb ids.
//!
//! The page is ~310 KB, but gzipped it's about 43 KB — the size of a poster —
//! so it's asked for compressed, and answers are cached on disk for a week.
//! The lighter `/csi/film/<slug>/rating-histogram/`, `/esi/…` and poster-ajax
//! endpoints are all behind a Cloudflare challenge and can't be used.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// How long a cached rating is trusted before it's fetched again.
const TTL: u64 = 7 * 24 * 60 * 60;

#[derive(Clone, Debug, PartialEq)]
pub struct Rating {
    /// Out of 5, as Letterboxd scores films.
    pub score: f64,
    pub votes: u64,
    /// Film slug, e.g. `interstellar`.
    pub slug: String,
}

impl Rating {
    /// "4.2★ Letterboxd" — the form the details header shows.
    pub fn label(&self) -> String {
        format!("{:.1}★ Letterboxd", self.score)
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Cached {
    /// Absent when Letterboxd has the film but nobody has rated it, or it has
    /// no page at all — remembered so it isn't asked for again every time.
    score: Option<f64>,
    #[serde(default)]
    votes: u64,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    at: u64,
}

fn cache_path(imdb_id: &str) -> std::path::PathBuf {
    crate::config::cache_dir()
        .join("letterboxd")
        .join(format!("{imdb_id}.json"))
}

/// The rating for an IMDb id, from cache when it's fresh. `None` means
/// Letterboxd has no rating for it.
pub fn fetch(imdb_id: &str) -> Result<Option<Rating>> {
    if !imdb_id.starts_with("tt") || imdb_id.len() < 3 {
        return Err(anyhow!("not an IMDb id: {imdb_id}"));
    }
    let path = cache_path(imdb_id);
    let cached: Cached = crate::config::load_json(&path);
    if cached.at > 0 && crate::history::now().saturating_sub(cached.at) < TTL {
        return Ok(cached.score.map(|score| Rating {
            score,
            votes: cached.votes,
            slug: cached.slug,
        }));
    }

    let resp = minreq::get(format!("https://letterboxd.com/imdb/{imdb_id}/"))
        .with_header("Accept-Encoding", "gzip")
        .with_timeout(20)
        .send()?;
    if resp.status_code == 404 {
        crate::config::write_json(&path, &Cached { at: crate::history::now(), ..Default::default() });
        return Ok(None);
    }
    if resp.status_code >= 400 {
        return Err(anyhow!("letterboxd returned HTTP {}", resp.status_code));
    }
    let body = gunzip(resp.as_bytes());
    let rating = parse(&body, &resp.url);
    crate::config::write_json(
        &path,
        &Cached {
            score: rating.as_ref().map(|r| r.score),
            votes: rating.as_ref().map(|r| r.votes).unwrap_or(0),
            slug: rating.as_ref().map(|r| r.slug.clone()).unwrap_or_default(),
            at: crate::history::now(),
        },
    );
    Ok(rating)
}

/// Undo the gzip Letterboxd was asked for. Anything that isn't gzipped is
/// already the page itself.
fn gunzip(body: &[u8]) -> String {
    if body.starts_with(&[0x1f, 0x8b]) {
        let mut out = String::new();
        if std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(body), &mut out).is_ok() {
            return out;
        }
    }
    String::from_utf8_lossy(body).into_owned()
}

/// Pull the rating out of the film page's JSON-LD, with the slug from the URL
/// the redirect landed on.
fn parse(body: &str, url: &str) -> Option<Rating> {
    let score: f64 = number_after(body, "\"ratingValue\":")?.parse().ok()?;
    let votes = number_after(body, "\"ratingCount\":")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    Some(Rating { score, votes, slug: slug_from_url(url) })
}

/// The digits (and dot) that follow `key`, skipping any spaces after the colon.
fn number_after<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let rest = body.split_once(key)?.1.trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(rest.len());
    let n = &rest[..end];
    (!n.is_empty()).then_some(n)
}

/// `https://letterboxd.com/film/interstellar/` → `interstellar`.
fn slug_from_url(url: &str) -> String {
    url.split("/film/")
        .nth(1)
        .unwrap_or("")
        .trim_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<html><head><script type="application/ld+json">
      {"@type":"Movie","name":"Interstellar","aggregateRating":{"@type":"AggregateRating",
       "ratingCount":1850420,"bestRating":5,"worstRating":1,"ratingValue": 4.37}}
      </script></head><body>…</body></html>"#;

    #[test]
    fn reads_the_json_ld_rating() {
        let r = parse(PAGE, "https://letterboxd.com/film/interstellar/").expect("rating");
        assert_eq!(r.score, 4.37);
        assert_eq!(r.votes, 1_850_420);
        assert_eq!(r.slug, "interstellar");
        assert_eq!(r.label(), "4.4★ Letterboxd");
    }

    #[test]
    fn a_page_without_a_rating_is_not_one() {
        assert!(parse("<html>no ratings yet</html>", "https://letterboxd.com/film/x/").is_none());
        // A film nobody has rated still has a count of 0 and no value.
        assert!(parse(r#"{"ratingCount":0}"#, "https://letterboxd.com/film/x/").is_none());
    }

    #[test]
    fn gzipped_and_plain_pages_both_read() {
        assert!(gunzip(PAGE.as_bytes()).contains("ratingValue"));
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut enc, PAGE.as_bytes()).unwrap();
        assert_eq!(gunzip(&enc.finish().unwrap()), PAGE);
    }

    #[test]
    fn slugs_come_off_the_redirect_url() {
        assert_eq!(slug_from_url("https://letterboxd.com/film/the-thing/"), "the-thing");
        assert_eq!(slug_from_url("https://letterboxd.com/film/dune-2021/crew/"), "dune-2021");
        assert_eq!(slug_from_url("https://letterboxd.com/"), "");
    }

    #[test]
    #[ignore = "hits letterboxd.com"]
    fn live_rating_for_interstellar() {
        let dir = std::env::temp_dir().join(format!("torflix-lb-{}", std::process::id()));
        std::env::set_var("XDG_CACHE_HOME", &dir);
        let r = fetch("tt0816692").expect("fetch").expect("rated");
        assert_eq!(r.slug, "interstellar");
        assert!(r.score > 3.0 && r.score <= 5.0, "{}", r.score);
        assert!(r.votes > 100_000, "{}", r.votes);
        // Second call must come from the cache, not the network.
        assert_eq!(fetch("tt0816692").unwrap().unwrap(), r);
        std::fs::remove_dir_all(&dir).ok();
    }
}
