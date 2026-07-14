use anyhow::{bail, Result};

#[derive(Clone, Debug)]
pub struct Movie {
    pub title: String,
    pub year: String,
    pub lb_rating: Option<f32>,
}

pub enum ListKind {
    Popular,
    PopularThisWeek,
    PopularThisMonth,
    TopRated,
}

impl ListKind {
    pub fn url(&self) -> &'static str {
        match self {
            ListKind::Popular => "https://letterboxd.com/csi/films/films-browser-list/popular/?esiAllowFilters=true",
            ListKind::PopularThisWeek => "https://letterboxd.com/csi/films/films-browser-list/popular/this/week/?esiAllowFilters=true",
            ListKind::PopularThisMonth => "https://letterboxd.com/csi/films/films-browser-list/popular/this/month/?esiAllowFilters=true",
            ListKind::TopRated => "https://letterboxd.com/csi/films/films-browser-list/by/rating/?esiAllowFilters=true",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            ListKind::Popular => "Popular all-time",
            ListKind::PopularThisWeek => "Popular this week",
            ListKind::PopularThisMonth => "Popular this month",
            ListKind::TopRated => "Top rated",
        }
    }
}

pub fn fetch(kind: &ListKind) -> Result<Vec<Movie>> {
    let resp = minreq::get(kind.url())
        .with_header(
            "User-Agent",
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
        )
        .with_header("Accept", "text/html,application/xhtml+xml")
        .with_header("Accept-Language", "en-US,en;q=0.9")
        .with_timeout(20)
        .send()?;

    if resp.status_code >= 400 {
        bail!("Letterboxd returned HTTP {}", resp.status_code);
    }

    Ok(parse_films(resp.as_str()?))
}

fn parse_films(html: &str) -> Vec<Movie> {
    let mut movies = Vec::new();
    let mut pos = 0;
    let anchor = "class=\"posteritem\"";

    while let Some(rel) = html[pos..].find(anchor) {
        let li_pos = pos + rel;
        let window_end = (li_pos + 2000).min(html.len());
        let window = &html[li_pos..window_end];

        let lb_rating = extract_attr(window, "data-average-rating")
            .and_then(|s| s.parse::<f32>().ok());

        let name_key = "data-item-name=\"";
        if let Some(name_rel) = window.find(name_key) {
            let name_start = name_rel + name_key.len();
            if let Some(name_end_rel) = window[name_start..].find('"') {
                let raw = decode_entities(&window[name_start..name_start + name_end_rel]);
                let (title, year) = split_title_year(&raw);
                if !title.is_empty() && !movies.iter().any(|m: &Movie| m.title == title) {
                    movies.push(Movie { title, year, lb_rating });
                }
            }
        }

        pos = li_pos + anchor.len();
    }

    movies
}

fn split_title_year(raw: &str) -> (String, String) {
    if raw.ends_with(')') {
        if let Some(lp) = raw.rfind(" (") {
            let y = &raw[lp + 2..raw.len() - 1];
            if y.chars().all(|c| c.is_ascii_digit()) && y.len() == 4 {
                return (raw[..lp].to_string(), y.to_string());
            }
        }
    }
    (raw.to_string(), String::new())
}

fn extract_attr(s: &str, attr: &str) -> Option<String> {
    let key = format!("{}=\"", attr);
    let start = s.find(&key)? + key.len();
    let end = s[start..].find('"')? + start;
    let v = s[start..end].trim().to_string();
    if v.is_empty() { None } else { Some(v) }
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
}
