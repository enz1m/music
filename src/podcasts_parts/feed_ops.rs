//! Feed parsing, fetching, and OPML.

use super::*;

/// Parse either an RSS 2.0 or Atom feed with namespace-aware XML events.
pub fn parse_feed(feed_url: &str, body: &[u8]) -> Result<PodcastFeed, String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Rss,
        Atom,
    }
    #[derive(Clone, Copy)]
    enum TextTarget {
        FeedTitle,
        Description,
        EpisodeTitle,
        Guid,
        // Channel and episode metadata targets; the `End` arm applies captured
        // text, and an undeclared field stays empty.
        FeedAuthor,
        FeedLink,
        FeedArtworkUrl,
        EpisodePublished,
        EpisodeDuration,
        EpisodeDescription,
    }

    let mut reader = Reader::from_reader(body);
    // Keep whitespace around entity events (`A &amp; B` is emitted as three
    // events); trim only after the complete field has been assembled.
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut kind = None;
    let mut in_channel = false;
    let mut title = String::new();
    let mut description = String::new();
    // Channel-level metadata accumulated across the whole document.
    let mut author = String::new();
    let mut link = String::new();
    let mut artwork_url: Option<String> = None;
    // Scopes `<url>` capture to the RSS channel `<image>` container, so an
    // item `<url>` is never taken as feed artwork.
    let mut in_channel_image = false;
    let mut current: Option<PodcastEpisode> = None;
    let mut episodes = Vec::new();
    let mut capture: Option<(Vec<u8>, TextTarget, String)> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(tag)) => {
                let name = tag.local_name().as_ref().to_ascii_lowercase();
                if kind.is_none() {
                    kind = match name.as_slice() {
                        b"rss" => Some(Kind::Rss),
                        b"feed" => Some(Kind::Atom),
                        _ => return Err("The response is XML, but not an RSS or Atom feed.".into()),
                    };
                }
                match (kind, name.as_slice()) {
                    (Some(Kind::Rss), b"channel") => in_channel = true,
                    (Some(Kind::Rss), b"item") | (Some(Kind::Atom), b"entry") => {
                        current = Some(PodcastEpisode::default());
                    }
                    (Some(Kind::Rss), b"enclosure") if current.is_some() => {
                        apply_media_link(&tag, &reader, current.as_mut().unwrap(), false)?;
                    }
                    (Some(Kind::Atom), b"link") if current.is_some() => {
                        apply_media_link(&tag, &reader, current.as_mut().unwrap(), true)?;
                    }
                    // `itunes:image` / `<image href>` carries the URL in an
                    // attribute: an open item claims it, else the channel. A plain
                    // RSS `<image>` (no href) opens the `<url>`-child capture.
                    (_, b"image") => {
                        if let Some(href) = read_attr(&tag, &reader, b"href")? {
                            apply_artwork(href, current.as_mut(), &mut artwork_url);
                        } else if current.is_none() {
                            in_channel_image = true;
                        }
                    }
                    (_, b"url") if in_channel_image && current.is_none() => {
                        capture = Some((name, TextTarget::FeedArtworkUrl, String::new()))
                    }
                    (_, b"title") if current.is_some() => {
                        capture = Some((name, TextTarget::EpisodeTitle, String::new()))
                    }
                    (Some(Kind::Rss), b"guid") if current.is_some() => {
                        capture = Some((name, TextTarget::Guid, String::new()))
                    }
                    (Some(Kind::Atom), b"id") if current.is_some() => {
                        capture = Some((name, TextTarget::Guid, String::new()))
                    }
                    // Episode fields precede the channel arms so an in-item field
                    // is never captured as its channel namesake.
                    (Some(Kind::Rss), b"pubdate") if current.is_some() => {
                        capture = Some((name, TextTarget::EpisodePublished, String::new()))
                    }
                    (Some(Kind::Atom), b"published" | b"updated") if current.is_some() => {
                        capture = Some((name, TextTarget::EpisodePublished, String::new()))
                    }
                    (_, b"duration") if current.is_some() => {
                        capture = Some((name, TextTarget::EpisodeDuration, String::new()))
                    }
                    (Some(Kind::Rss), b"description" | b"summary") if current.is_some() => {
                        capture = Some((name, TextTarget::EpisodeDescription, String::new()))
                    }
                    (Some(Kind::Atom), b"summary") if current.is_some() => {
                        capture = Some((name, TextTarget::EpisodeDescription, String::new()))
                    }
                    (Some(Kind::Rss), b"title") if in_channel && !in_channel_image => {
                        capture = Some((name, TextTarget::FeedTitle, String::new()))
                    }
                    (Some(Kind::Rss), b"description") if in_channel => {
                        capture = Some((name, TextTarget::Description, String::new()))
                    }
                    // Channel author and website. `current.is_none()` keeps item
                    // `itunes:author` out, and `!in_channel_image` keeps the image
                    // block's `<title>`/`<link>` from being read as channel identity.
                    (Some(Kind::Rss), b"author" | b"managingeditor") if current.is_none() => {
                        capture = Some((name, TextTarget::FeedAuthor, String::new()))
                    }
                    (Some(Kind::Rss), b"link")
                        if in_channel && current.is_none() && !in_channel_image =>
                    {
                        capture = Some((name, TextTarget::FeedLink, String::new()))
                    }
                    (Some(Kind::Atom), b"title") => {
                        capture = Some((name, TextTarget::FeedTitle, String::new()))
                    }
                    (Some(Kind::Atom), b"subtitle") => {
                        capture = Some((name, TextTarget::Description, String::new()))
                    }
                    (Some(Kind::Atom), b"author" | b"name") if current.is_none() => {
                        capture = Some((name, TextTarget::FeedAuthor, String::new()))
                    }
                    (Some(Kind::Atom), b"logo" | b"icon") if current.is_none() => {
                        capture = Some((name, TextTarget::FeedArtworkUrl, String::new()))
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(tag)) => {
                let name = tag.local_name().as_ref().to_ascii_lowercase();
                match (kind, name.as_slice()) {
                    (Some(Kind::Rss), b"enclosure") if current.is_some() => {
                        apply_media_link(&tag, &reader, current.as_mut().unwrap(), false)?
                    }
                    (Some(Kind::Atom), b"link") if current.is_some() => {
                        apply_media_link(&tag, &reader, current.as_mut().unwrap(), true)?
                    }
                    // Self-closing `itunes:image href`, the common shape.
                    (_, b"image") => {
                        if let Some(href) = read_attr(&tag, &reader, b"href")? {
                            apply_artwork(href, current.as_mut(), &mut artwork_url);
                        }
                    }
                    // An Atom channel `<link rel="alternate" href>` is the website;
                    // the first alternate wins.
                    (Some(Kind::Atom), b"link") if current.is_none() => {
                        if link.is_empty()
                            && let Some(href) = atom_alternate_link(&tag, &reader)?
                        {
                            link = bounded_meta(href);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(text)) => {
                if let Some((_, _, value)) = capture.as_mut() {
                    let decoded = text
                        .decode()
                        .map_err(|_| "Feed text uses an unsupported encoding.".to_string())?;
                    let unescaped = quick_xml::escape::unescape(&decoded)
                        .map_err(|_| "Feed contains an invalid XML entity.".to_string())?;
                    value.push_str(&unescaped);
                }
            }
            Ok(Event::CData(text)) => {
                if let Some((_, _, value)) = capture.as_mut() {
                    value.push_str(
                        &text
                            .decode()
                            .map_err(|_| "Feed text uses an unsupported encoding.".to_string())?,
                    );
                }
            }
            Ok(Event::GeneralRef(reference)) => {
                if let Some((_, _, value)) = capture.as_mut() {
                    let reference = reference
                        .decode()
                        .map_err(|_| "Feed contains an invalid XML entity.".to_string())?;
                    let encoded = format!("&{reference};");
                    value.push_str(
                        &quick_xml::escape::unescape(&encoded)
                            .map_err(|_| "Feed contains an invalid XML entity.".to_string())?,
                    );
                }
            }
            Ok(Event::End(tag)) => {
                let name = tag.local_name().as_ref().to_ascii_lowercase();
                if capture
                    .as_ref()
                    .is_some_and(|(captured, _, _)| captured.as_slice() == name.as_slice())
                {
                    let (_, target, value) = capture.take().unwrap();
                    let value = value.trim().to_string();
                    match target {
                        TextTarget::FeedTitle => title = value,
                        TextTarget::Description => description = bounded_meta(value),
                        TextTarget::EpisodeTitle => {
                            if let Some(episode) = current.as_mut() {
                                episode.title = value;
                            }
                        }
                        TextTarget::Guid => {
                            if let Some(episode) = current.as_mut() {
                                episode.guid = value;
                            }
                        }
                        // Channel author, website and artwork; first non-empty
                        // wins over later item-scoped namesakes.
                        TextTarget::FeedAuthor => {
                            if author.is_empty() && !value.is_empty() {
                                author = bounded_meta(value);
                            }
                        }
                        TextTarget::FeedLink => {
                            if link.is_empty() && !value.is_empty() {
                                link = bounded_meta(value);
                            }
                        }
                        TextTarget::FeedArtworkUrl => {
                            if artwork_url.is_none() && !value.is_empty() {
                                artwork_url = Some(bounded_meta(value));
                            }
                        }
                        // Episode published, duration and description.
                        TextTarget::EpisodePublished => {
                            if let Some(episode) = current.as_mut() {
                                episode.published = bounded_meta(value);
                            }
                        }
                        TextTarget::EpisodeDuration => {
                            if let Some(episode) = current.as_mut() {
                                episode.duration_secs = parse_itunes_duration(&value);
                            }
                        }
                        TextTarget::EpisodeDescription => {
                            if let Some(episode) = current.as_mut()
                                && episode.description.is_empty()
                                && !value.is_empty()
                            {
                                episode.description = bounded_notes(value);
                            }
                        }
                    }
                }
                match (kind, name.as_slice()) {
                    (Some(Kind::Rss), b"item") | (Some(Kind::Atom), b"entry") => {
                        if let Some(mut episode) = current.take() {
                            if episode.guid.is_empty() {
                                episode.guid = episode
                                    .audio_url
                                    .clone()
                                    .unwrap_or_else(|| episode.title.clone());
                            }
                            episodes.push(episode);
                        }
                    }
                    (Some(Kind::Rss), b"channel") => in_channel = false,
                    // Close the RSS `<image>` container so a later stray `<url>`
                    // is not read as channel artwork.
                    (_, b"image") => in_channel_image = false,
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return Err("The server returned malformed XML.".into()),
            _ => {}
        }
        buf.clear();
    }

    if kind.is_none() {
        return Err("The response did not contain an RSS or Atom feed.".into());
    }
    Ok(PodcastFeed {
        feed_url: feed_url.to_string(),
        title,
        description,
        author,
        link,
        artwork_url,
        episodes,
        status: FeedStatus::Loaded,
    })
}

/// Bound a metadata string: collapse whitespace and cap characters, ending a
/// truncated value in a single U+2026.
pub const MAX_META_CHARS: usize = 2_000;

pub(crate) fn bounded_meta(value: String) -> String {
    // Collapse whitespace runs so multi-line show notes read as one paragraph.
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_META_CHARS {
        return collapsed;
    }
    let mut bounded: String = collapsed.chars().take(MAX_META_CHARS - 1).collect();
    bounded.push('\u{2026}');
    bounded
}

/// The most of an episode's show notes kept: the details dialog shows them
/// whole, so more than a channel's one-paragraph metadata.
pub const MAX_NOTES_CHARS: usize = 12_000;

/// Show notes, bounded but with their line breaks (the markup carries the
/// paragraphs; plain-text notes keep their own).
pub(crate) fn bounded_notes(value: String) -> String {
    let value = value.trim();
    if value.chars().count() <= MAX_NOTES_CHARS {
        return value.to_owned();
    }
    let mut bounded: String = value.chars().take(MAX_NOTES_CHARS - 1).collect();
    bounded.push('\u{2026}');
    bounded
}

/// Route an `itunes:image` / `<image href>` artwork URL to the open episode,
/// else to the channel. First non-empty wins for each destination.
pub(crate) fn apply_artwork(
    href: String,
    episode: Option<&mut PodcastEpisode>,
    feed_artwork: &mut Option<String>,
) {
    let href = href.trim();
    if href.is_empty() {
        return;
    }
    let bounded = bounded_meta(href.to_string());
    match episode {
        Some(episode) => {
            if episode.artwork_url.is_none() {
                episode.artwork_url = Some(bounded);
            }
        }
        None => {
            if feed_artwork.is_none() {
                *feed_artwork = Some(bounded);
            }
        }
    }
}

/// Parse an `itunes:duration` value into whole seconds.
///
/// Accepts a seconds count (`"1234"`) or a colon-clock (`"MM:SS"` /
/// `"HH:MM:SS"`). An unparsable or empty value yields `None`, never zero.
pub fn parse_itunes_duration(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some((whole, _frac)) = raw.split_once('.') {
        // Fractional seconds (`"1234.5"`) keep the whole part.
        if !whole.contains(':') {
            return whole.parse::<u64>().ok();
        }
    }
    if !raw.contains(':') {
        return raw.parse::<u64>().ok();
    }
    // Colon-clock: at most three components (H:M:S), each a bounded integer,
    // seconds and minutes below 60.
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut total: u64 = 0;
    for (index, part) in parts.iter().enumerate() {
        // A fractional tail (e.g. `"1:02:03.5"`) is dropped, not an error.
        let field = part.trim();
        let field = field.split_once('.').map_or(field, |(whole, _)| whole);
        let value: u64 = field.parse().ok()?;
        // Trailing components are capped at 59; the leading one is unbounded
        // so a 90-minute show parses as `90:00`.
        if index != 0 && value >= 60 {
            return None;
        }
        total = total.checked_mul(60)?.checked_add(value)?;
    }
    Some(total)
}

/// Format an episode duration as `H:MM:SS` from an hour up, else `M:SS`.
/// Returns `None` for `None`, so no `0:00` chip is fabricated.
#[must_use]
pub fn format_duration(duration_secs: Option<u64>) -> Option<String> {
    let secs = duration_secs?;
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    Some(if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    })
}

pub(crate) fn apply_media_link(
    tag: &BytesStart<'_>,
    reader: &Reader<&[u8]>,
    episode: &mut PodcastEpisode,
    atom: bool,
) -> Result<(), String> {
    let mut url = None;
    let mut media_type = None;
    let mut relation = None;
    for attribute in tag.attributes() {
        let attribute =
            attribute.map_err(|_| "Feed contains an invalid XML attribute.".to_string())?;
        let name = attribute.key.local_name().as_ref().to_ascii_lowercase();
        let value = attribute
            .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, reader.decoder())
            .map_err(|_| "Feed contains an invalid XML attribute value.".to_string())?
            .into_owned();
        match name.as_slice() {
            b"url" | b"href" => url = Some(value),
            b"type" => media_type = Some(value),
            b"rel" => relation = Some(value),
            _ => {}
        }
    }
    let is_enclosure = !atom || relation.as_deref() == Some("enclosure");
    let is_audio = media_type
        .as_deref()
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("audio/"));
    if is_enclosure && is_audio {
        episode.audio_url = url;
    }
    Ok(())
}

/// Read one attribute off a tag by its namespace-stripped local name.
///
/// Returns `None` when absent and an error only when the value fails to decode.
pub(crate) fn read_attr(
    tag: &BytesStart<'_>,
    reader: &Reader<&[u8]>,
    name: &[u8],
) -> Result<Option<String>, String> {
    for attribute in tag.attributes() {
        let attribute =
            attribute.map_err(|_| "Feed contains an invalid XML attribute.".to_string())?;
        let key = attribute.key.local_name().as_ref().to_ascii_lowercase();
        if key.as_slice() == name {
            let value = attribute
                .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, reader.decoder())
                .map_err(|_| "Feed contains an invalid XML attribute value.".to_string())?
                .into_owned();
            return Ok(Some(value));
        }
    }
    Ok(None)
}

/// Extract an Atom channel `<link>`'s `href` when it names the alternate
/// (website) relation, explicitly or by omitting `rel`.
pub(crate) fn atom_alternate_link(
    tag: &BytesStart<'_>,
    reader: &Reader<&[u8]>,
) -> Result<Option<String>, String> {
    let relation = read_attr(tag, reader, b"rel")?;
    let is_alternate = relation
        .as_deref()
        .map(|rel| rel.eq_ignore_ascii_case("alternate"))
        .unwrap_or(true);
    if !is_alternate {
        return Ok(None);
    }
    read_attr(tag, reader, b"href")
}

#[cfg(test)]
pub fn parse_rss_feed(feed_url: &str, body: &str) -> Result<PodcastFeed, String> {
    parse_feed(feed_url, body.as_bytes())
}

/// Run one request through an injected transport on a [`PodcastService`]
/// worker. Synchronous like the transport; the service keeps it off the UI
/// executor.
pub fn fetch_with_transport(
    request: FetchRequest,
    transport: &dyn Transport,
    policy: UrlPolicy,
) -> FetchResult {
    let url = request.url.clone();
    let generation = request.generation;
    let result = fetch_body(&request, transport, policy)
        .and_then(|body| parse_feed(&url, &body))
        .map_err(actionable_error);
    FetchResult {
        url,
        generation,
        result,
    }
}

pub(crate) fn fetch_body(
    request: &FetchRequest,
    transport: &dyn Transport,
    policy: UrlPolicy,
) -> Result<Vec<u8>, String> {
    if request.cancel.is_cancelled() {
        return Err("Feed request was cancelled.".into());
    }
    if let Some(path) = request.url.strip_prefix("file://") {
        return read_local_feed(path);
    }
    let remote = RemoteRequest::checked(&request.url, &policy).map_err(url_error)?;
    let response = transport
        .fetch(&remote, &policy, &FEED_LIMITS, &request.cancel, &mut |_| {})
        .map_err(transport_error)?;
    validate_response(response)
}

pub(crate) fn read_local_feed(path: &str) -> Result<Vec<u8>, String> {
    if path.is_empty() {
        return Err("Choose an explicit file:// path to a feed.".into());
    }
    let unreadable = || "Couldn’t read that local feed file.".to_string();
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // A FIFO or device would block the feed worker; only a regular file is a feed.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NOFOLLOW | libc::O_NONBLOCK,
    );
    let file = options.open(path).map_err(|_| unreadable())?;
    let meta = file.metadata().map_err(|_| unreadable())?;
    if !meta.is_file() {
        return Err(unreadable());
    }
    if meta.len() > MAX_FEED_BYTES {
        return Err(format!(
            "Feed is larger than the {} MiB limit.",
            MAX_FEED_BYTES / 1024 / 1024
        ));
    }
    let mut body = Vec::new();
    file.take(MAX_FEED_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|_| "Couldn’t read that local feed file.".to_string())?;
    if body.len() as u64 > MAX_FEED_BYTES {
        return Err(format!(
            "Feed is larger than the {} MiB limit.",
            MAX_FEED_BYTES / 1024 / 1024
        ));
    }
    validate_feed_bytes(None, body)
}

pub(crate) fn validate_response(response: TransportResponse) -> Result<Vec<u8>, String> {
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "Feed server returned HTTP {}. Check the URL and retry.",
            response.status
        ));
    }
    let content_type = response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.as_str());
    validate_feed_bytes(content_type, response.body)
}

pub(crate) fn validate_feed_bytes(
    content_type: Option<&str>,
    body: Vec<u8>,
) -> Result<Vec<u8>, String> {
    if body.len() as u64 > MAX_FEED_BYTES {
        return Err(format!(
            "Feed is larger than the {} MiB limit.",
            MAX_FEED_BYTES / 1024 / 1024
        ));
    }
    if let Some(content_type) = content_type {
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let xml_like = matches!(
            mime.as_str(),
            "application/rss+xml"
                | "application/atom+xml"
                | "application/xml"
                | "text/xml"
                | "text/plain"
        ) || mime.ends_with("+xml");
        if !xml_like {
            return Err(format!("Server returned {mime}, not an RSS or Atom feed."));
        }
    }
    if body.contains(&0) {
        return Err("Server returned binary data, not an RSS or Atom feed.".into());
    }
    let prefix = String::from_utf8_lossy(&body[..body.len().min(1024)]).to_ascii_lowercase();
    if !prefix.contains("<rss") && !prefix.contains("<feed") {
        return Err("Server response is not an RSS or Atom feed.".into());
    }
    Ok(body)
}

pub(crate) fn url_error(error: UrlVerdict) -> String {
    match error {
        UrlVerdict::CleartextRefused => "Use an https:// feed URL; plain HTTP is blocked.".into(),
        UrlVerdict::CredentialsInUrl => "Remove the username or password from the feed URL.".into(),
        UrlVerdict::SchemeRefused { .. } => {
            "Use an https:// feed URL (or an explicit file:// path for a local feed).".into()
        }
        UrlVerdict::MissingHost | UrlVerdict::Malformed => {
            "Enter a complete https:// feed URL.".into()
        }
        UrlVerdict::TooManyRedirects { limit } => {
            format!("Feed redirected more than {limit} times. Check its URL.")
        }
        _ => "The feed URL was refused by the safety policy.".into(),
    }
}

pub(crate) fn transport_error(error: TransportError) -> String {
    match error {
        TransportError::Url(error) => url_error(error),
        TransportError::AddressBlocked(_) => {
            "Feed host resolves to a private, local, or metadata address, which is blocked.".into()
        }
        TransportError::Dns => {
            "Couldn’t resolve the feed host. Check the address and connection.".into()
        }
        TransportError::Cancelled(_) => "Feed request was cancelled.".into(),
        TransportError::ByteLimit { .. } => format!(
            "Feed is larger than the {} MiB limit.",
            MAX_FEED_BYTES / 1024 / 1024
        ),
        TransportError::Timeout => "Feed request timed out. Check the connection and retry.".into(),
        TransportError::Redirect => "Feed returned an invalid redirect. Check its URL.".into(),
        TransportError::HttpStatus(status) => {
            format!("Feed server returned HTTP {status}. Check the URL and retry.")
        }
        TransportError::ContentEncoding => "Feed uses an unsupported content encoding.".into(),
        TransportError::Backend(code) => format!(
            "Couldn’t download the feed (network error {code}). Check the connection and retry."
        ),
        _ => "Couldn’t download the feed. Check the connection and retry.".into(),
    }
}

pub(crate) fn actionable_error(message: String) -> String {
    if message.chars().count() <= MAX_ERROR_CHARS {
        return message;
    }
    let mut bounded: String = message.chars().take(MAX_ERROR_CHARS - 1).collect();
    bounded.push('…');
    bounded
}

/// Read `name="value"` (or `name='value'`) out of an OPML tag.
pub(crate) fn attr_value(tag: &str, name: &str) -> Option<String> {
    let key = format!("{name}=");
    let idx = tag.find(&key)?;
    let rest = &tag[idx + key.len()..];
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &rest[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

// ---------------------------------------------------------------------------
// OPML round-trip
// ---------------------------------------------------------------------------

/// Serialize subscribed feeds as an OPML 2.0 document with one
/// `<outline type="rss" xmlUrl="…"/>` per feed.
pub fn export_opml(feeds: &[PodcastFeed]) -> String {
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str("<opml version=\"2.0\">\n");
    s.push_str("  <head><title>Podcast subscriptions</title></head>\n");
    s.push_str("  <body>\n");
    for f in feeds {
        s.push_str(&format!(
            "    <outline type=\"rss\" text=\"{}\" xmlUrl=\"{}\"/>\n",
            xml_escape(&f.title),
            xml_escape(&f.feed_url),
        ));
    }
    s.push_str("  </body>\n");
    s.push_str("</opml>\n");
    s
}

/// Parse OPML 2.0 back into `(title, feed_url)` pairs. Missing text falls
/// back to the URL.
pub fn import_opml(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cursor = body;
    while let Some(start) = cursor.find("<outline") {
        let after = &cursor[start..];
        // This tag ends at its first `>`; searching for `/>` would skip a plain
        // opening tag and span two outlines.
        let end = match after.find('>') {
            Some(i) => i + 1,
            None => break,
        };
        let tag = &after[..end];
        // Only `type="rss"` outlines are subscriptions.
        let is_rss = tag.contains("type=\"rss\"") || tag.contains("type='rss'");
        // A shared OPML document must not make the app read local files.
        if is_rss
            && let Some(url) = attr_value(tag, "xmlUrl")
            && !url
                .trim_start()
                .get(..5)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
        {
            let title = attr_value(tag, "text").unwrap_or_else(|| url.clone());
            out.push((title, url));
        }
        cursor = &after[end..];
    }
    out
}

pub(crate) fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}
