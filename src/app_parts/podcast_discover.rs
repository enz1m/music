//! Podcast discovery from Apple's charts, and OPML subscription files.

use super::*;

/// The Podcasts page's scroll view, shared by the list and a podcast's page.
pub(crate) fn podcasts_scroll_id() -> iced::widget::Id {
    iced::widget::Id::new("music-podcasts-scroll")
}

/// How many episodes a podcast page adds at a time.
const EPISODE_PAGE: usize = 50;

/// Podcast notifications replace one another.
const PODCAST_COALESCE_KEY: &str = "music-podcasts";

impl MusicApp {
    pub(crate) fn reduce_podcast_discover(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PodcastChartsGenre(genre) => {
                self.podcast_charts.genre = genre;
                self.podcast_charts.failed = None;
                return self.load_podcast_chart();
            }
            Message::PodcastOpen(feed_url) => {
                self.podcast_charts.open = Some(feed_url.clone());
                self.podcast_charts.episodes_shown = EPISODE_PAGE;
                // A podcast's page starts at its top.
                let to_top = iced::widget::operation::snap_to(
                    podcasts_scroll_id(),
                    iced::widget::scrollable::RelativeOffset::START,
                    iced::widget::operation::Animation::Instant,
                );
                let subscribed = self.subscriptions.iter().any(|f| f.feed_url == feed_url);
                let previewed = self.podcast_charts.preview.as_ref().is_some_and(|p| {
                    p.feed_url == feed_url && p.status != podcasts::FeedStatus::Loading
                });
                let failed = self.podcast_charts.preview.as_ref().is_some_and(|p| {
                    p.feed_url == feed_url && matches!(p.status, podcasts::FeedStatus::Failed(_))
                });
                if !subscribed && (!previewed || failed) {
                    // Seeded from what discovery knows, until the feed arrives.
                    let known = match &self.podcast_search_state {
                        PodcastSearchState::Results(results) => {
                            results.iter().find(|r| r.feed_url == feed_url).cloned()
                        }
                        _ => None,
                    }
                    .or_else(|| self.podcast_charts.find(&feed_url).cloned());
                    self.podcast_charts.preview = Some(podcasts::PodcastFeed {
                        feed_url: feed_url.clone(),
                        title: known.as_ref().map(|k| k.title.clone()).unwrap_or_default(),
                        author: known.as_ref().map(|k| k.author.clone()).unwrap_or_default(),
                        artwork_url: known.and_then(|k| k.artwork_url),
                        status: podcasts::FeedStatus::Loading,
                        ..podcasts::PodcastFeed::default()
                    });
                    self.start_podcast_fetch(feed_url);
                    self.reconcile_and_dispatch_artwork();
                }
                return to_top;
            }
            Message::PodcastClose => {
                self.podcast_charts.open = None;
                // Back where the list was.
                return iced::widget::operation::scroll_to(
                    podcasts_scroll_id(),
                    iced::widget::scrollable::AbsoluteOffset {
                        x: None,
                        y: Some(self.podcast_charts.list_scroll),
                    },
                    iced::widget::operation::Animation::Instant,
                );
            }
            Message::PodcastsScrolled(y) => {
                // Only the list's position is kept; a podcast page opens at
                // its top.
                if self.podcast_charts.open.is_none() {
                    self.podcast_charts.list_scroll = y;
                }
            }
            Message::PodcastEpisodeDetails(feed_url, guid) => {
                if self.feed_episode(&feed_url, &guid).is_none() {
                    return Task::none();
                }
                self.context_menu = None;
                self.episode_details = Some((feed_url.clone(), guid.clone()));
                return self.open_modal_focus(
                    ModalPresentationId::EpisodeDetails,
                    Some(crate::context_menu::podcast_episode_row_id(
                        &feed_url, &guid,
                    )),
                );
            }
            Message::CloseEpisodeDetails => {
                return self.dismiss_modal_focus(
                    ModalPresentationId::EpisodeDetails,
                    enzui_core::modal::DismissCause::CloseButton,
                );
            }
            Message::PodcastShowMoreEpisodes => {
                self.podcast_charts.episodes_shown = self
                    .podcast_charts
                    .episodes_shown
                    .saturating_add(EPISODE_PAGE);
            }
            Message::PodcastCountryPicker => {
                self.podcast_charts.picking_country = !self.podcast_charts.picking_country;
            }
            Message::PodcastChartsCountry(code) => {
                self.podcast_charts.picking_country = false;
                if !podcasts::charts::valid_country(&code) || code == self.podcast_charts.country {
                    return Task::none();
                }
                self.podcast_charts.country = code.clone();
                self.podcast_charts.chosen_country = Some(code);
                self.podcast_charts.lists.clear();
                self.podcast_charts.loading = None;
                self.podcast_charts.failed = None;
                self.mark_persistence_dirty();
                // Radio follows the same country.
                if self.current_view == View::Radio {
                    self.dispatch_catalog_top_list();
                    return Task::none();
                }
                return self.load_podcast_chart();
            }
            Message::PodcastChartsLoaded(genre, result) => {
                if self.podcast_charts.loading == Some(genre) {
                    self.podcast_charts.loading = None;
                }
                match result {
                    // A chart for a country no longer shown is dropped.
                    Ok((country, _)) if country != self.podcast_charts.country => {}
                    Ok((_, shows)) => {
                        self.podcast_charts.lists.insert(genre, shows);
                        if self.podcast_charts.genre == genre {
                            self.podcast_charts.failed = None;
                        }
                        // The covers come with the chart.
                        self.reconcile_and_dispatch_artwork();
                    }
                    Err(message) if self.podcast_charts.genre == genre => {
                        self.podcast_charts.failed = Some(message);
                    }
                    Err(_) => {}
                }
            }
            Message::OpmlImportFile => {
                if let Err(text) = self.opml_transfer.start_import() {
                    self.post_podcast_status(text, ToastTone::Warning);
                }
            }
            Message::OpmlExportFile => {
                if self.subscriptions.is_empty() {
                    self.post_podcast_status(
                        "There are no subscriptions to export yet.",
                        ToastTone::Info,
                    );
                    return Task::none();
                }
                let document = podcasts::export_opml(&self.subscriptions);
                if let Err(text) = self.opml_transfer.start_export(document) {
                    self.post_podcast_status(text, ToastTone::Warning);
                }
            }
            Message::OpmlTransferWake => {
                let mut tasks = Vec::new();
                match self.opml_transfer.pump_import() {
                    Some(Ok(path)) => tasks.push(Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || crate::opml_transfer::read(&path))
                                .await
                                .unwrap_or_else(|_| Err("The file couldn't be read.".to_owned()))
                        },
                        Message::OpmlFileRead,
                    )),
                    Some(Err(text)) => self.post_podcast_status(text, ToastTone::Warning),
                    None => {}
                }
                for done in self.opml_transfer.pump_export() {
                    match done {
                        Ok((grant, document)) => tasks.push(Task::perform(
                            async move {
                                tokio::task::spawn_blocking(move || {
                                    crate::opml_transfer::publish(&grant, &document)
                                })
                                .await
                                .unwrap_or_else(|_| Err("The file couldn't be saved.".to_owned()))
                            },
                            Message::OpmlFileSaved,
                        )),
                        Err(text) => self.post_podcast_status(text, ToastTone::Warning),
                    }
                }
                return Task::batch(tasks);
            }
            Message::OpmlFileRead(Ok(text)) => {
                let found = podcasts::import_opml(&text).len();
                let (added, truncated) = self.subscribe_from_opml(&text);
                let note = match (found, added) {
                    (0, _) => "That file lists no podcast feeds.".to_owned(),
                    (_, 0) => "You're already subscribed to every podcast in that file.".to_owned(),
                    (_, 1) => "Subscribed to 1 podcast.".to_owned(),
                    (_, n) if truncated => {
                        format!("Subscribed to {n} podcasts. Import the file again for the rest.")
                    }
                    (_, n) => format!("Subscribed to {n} podcasts."),
                };
                let tone = if added > 0 {
                    ToastTone::Success
                } else {
                    ToastTone::Info
                };
                self.post_podcast_status(note, tone);
            }
            Message::OpmlFileRead(Err(text)) | Message::OpmlFileSaved(Err(text)) => {
                self.post_podcast_status(text, ToastTone::Warning);
            }
            Message::OpmlFileSaved(Ok(())) => {
                self.post_podcast_status("Subscriptions exported.", ToastTone::Success);
            }
            _ => {}
        }
        Task::none()
    }

    /// Load the chart on show, unless it's loaded or loading.
    pub(crate) fn load_podcast_chart(&mut self) -> Task<Message> {
        let genre = self.podcast_charts.genre;
        if self.podcast_charts.lists.contains_key(&genre)
            || self.podcast_charts.loading == Some(genre)
        {
            return Task::none();
        }
        self.podcast_charts.loading = Some(genre);
        let country = self.podcast_charts.country.clone();
        let asked = country.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || podcasts::charts::fetch(&asked, genre))
                    .await
                    .unwrap_or_else(
                        |_| Err("Apple's podcast charts couldn't be loaded.".to_owned()),
                    )
            },
            move |result| {
                Message::PodcastChartsLoaded(genre, result.map(|shows| (country.clone(), shows)))
            },
        )
    }

    /// Subscribe to each feed in an OPML document not yet subscribed, up to
    /// the import limit. Returns how many were added and whether the limit
    /// cut the document short.
    pub(crate) fn subscribe_from_opml(&mut self, opml: &str) -> (usize, bool) {
        let mut truncated = false;
        let mut added = 0usize;
        for (title, url) in podcasts::import_opml(opml) {
            let url = url.trim().to_string();
            if url.is_empty() || self.subscriptions.iter().any(|f| f.feed_url == url) {
                continue;
            }
            if added == podcasts::MAX_IMPORT_FEEDS {
                truncated = true;
                break;
            }
            let title = if title.trim().is_empty() {
                url.clone()
            } else {
                title
            };
            self.subscriptions.push(podcasts::PodcastFeed {
                feed_url: url.clone(),
                title,
                description: String::new(),
                episodes: Vec::new(),
                status: podcasts::FeedStatus::Loading,
                ..podcasts::PodcastFeed::default()
            });
            self.start_podcast_fetch(url);
            added += 1;
        }
        if added > 0 {
            self.mark_persistence_dirty();
        }
        (added, truncated)
    }

    /// A podcast outcome or failure, as a transient notification.
    pub(crate) fn post_podcast_status(&mut self, text: impl Into<String>, tone: ToastTone) {
        self.toasts.push(
            enzui_widgets::toast_host::ToastSpec::new(text)
                .tone(tone)
                .coalesce_key(PODCAST_COALESCE_KEY),
        );
    }
}
