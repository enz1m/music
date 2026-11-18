//! Context-menu reconciliation and models.

use super::*;

impl MusicApp {
    /// Re-derive an open context menu's rows from live state.
    ///
    /// Runs after each reducer arm, so a frame never offers a row the target no
    /// longer qualifies for; inapplicable rows are omitted, not disabled.
    /// `MenuModel::reconcile` keeps the highlight on the same row id, falling to
    /// the nearest focusable row only when that id is gone.
    pub(crate) fn reconcile_open_context_menu(&mut self) {
        // Rebuild through the same resolver a fresh open uses; `None` when nothing
        // is open.
        let Some((target, podcast_key, radio_key)) = self.context_menu.as_ref().map(|open| {
            (
                open.target,
                open.podcast_key.clone(),
                open.radio_key.clone(),
            )
        }) else {
            return;
        };
        let rows = self.context_menu_model(target, radio_key.as_ref(), podcast_key.as_ref());
        if let Some(open) = self.context_menu.as_mut() {
            let _ = open.model.reconcile(rows.entries().to_vec());
        }
    }

    /// The [`MenuModel`] for `target`, resolved against live state.
    ///
    /// Library targets delegate to [`context_menu::menu_model`]; radio and podcast
    /// rows are built here from [`RadioState`](crate::radio::RadioState) and the
    /// subscriptions list.
    fn context_menu_model(
        &self,
        target: domain::ContextTarget,
        radio_key: Option<&(u64, String)>,
        podcast_key: Option<&(String, String)>,
    ) -> enzui_core::menu_model::MenuModel {
        use domain::ContextTarget as T;
        match target {
            T::RadioStation(_) => self.radio_menu_model(radio_key),
            T::PodcastEpisode(_) => self.podcast_menu_model(podcast_key),
            _ => context_menu::menu_model(target, &self.library, &self.playback),
        }
    }

    /// The current u64 id the durable `remote_uuid` resolves to, or `None` when
    /// the uuid is absent or ambiguous, so a hash collision or catalog replacement
    /// never acts on the wrong station.
    fn radio_current_id(&self, remote_uuid: &str) -> Option<u64> {
        self.radio.by_uuid(remote_uuid).map(|s| s.id)
    }

    /// Resolve a radio menu's stored `(id, remote_uuid)` to the station it may
    /// still act on: `Some((current_id, uuid))` only when the uuid resolves
    /// uniquely to the same id the menu opened on.
    fn radio_menu_station<'k>(
        &self,
        radio_key: Option<&'k (u64, String)>,
    ) -> Option<(u64, &'k str)> {
        let (opened_id, uuid) = radio_key?;
        let current = self.radio_current_id(uuid)?;
        (current == *opened_id).then_some((current, uuid.as_str()))
    }

    /// The two-row radio menu (Play, Add/Remove favourite); empty, and so
    /// unopenable, when the stored uuid no longer resolves to its exact id.
    fn radio_menu_model(
        &self,
        radio_key: Option<&(u64, String)>,
    ) -> enzui_core::menu_model::MenuModel {
        use enzui_core::menu_model::{MenuEntryModel, MenuModel};
        let Some((id, _uuid)) = self.radio_menu_station(radio_key) else {
            return MenuModel::new(Vec::new());
        };
        let Some(station) = self.radio.station(id) else {
            return MenuModel::new(Vec::new());
        };
        let play = MenuEntryModel::command(
            domain::ContextAction::Play.key(),
            format!(
                "Play \u{201C}{}\u{201D}",
                domain::truncate_label(&station.name, 32)
            ),
        )
        .icon(domain::ContextAction::Play.icon());
        // The label names the current state, so it reads as an action.
        let fav_label = if self.radio.is_favorite(id) {
            "Remove favourite"
        } else {
            "Add favourite"
        };
        let favourite = MenuEntryModel::command(
            domain::ContextAction::ToggleFavorite.key(),
            fav_label.to_string(),
        )
        .icon(domain::ContextAction::ToggleFavorite.icon());
        MenuModel::new(vec![play, favourite])
    }

    /// The one-row podcast episode menu (Play); empty when the `(feed_url, guid)`
    /// key no longer resolves to a playable episode.
    fn podcast_menu_model(
        &self,
        podcast_key: Option<&(String, String)>,
    ) -> enzui_core::menu_model::MenuModel {
        use enzui_core::menu_model::{MenuEntryModel, MenuModel};
        let Some((feed_url, guid)) = podcast_key else {
            return MenuModel::new(Vec::new());
        };
        let Some(episode) = self.episode(feed_url, guid) else {
            return MenuModel::new(Vec::new());
        };
        let play = MenuEntryModel::command(
            domain::ContextAction::Play.key(),
            format!(
                "Play \u{201C}{}\u{201D}",
                domain::truncate_label(&episode.title, 32)
            ),
        )
        .icon(domain::ContextAction::Play.icon());
        let details =
            MenuEntryModel::command(domain::ContextAction::TrackInfo.key(), "Episode details")
                .icon("info");
        MenuModel::new(vec![play, details])
    }

    /// The playable episode a durable `(feed_url, guid)` key resolves to, or
    /// `None` once a feed refresh has dropped it.
    pub(crate) fn episode(&self, feed_url: &str, guid: &str) -> Option<&podcasts::PodcastEpisode> {
        self.feed_episode(feed_url, guid)
            .map(|(_, episode)| episode)
    }

    /// A playable episode and its podcast, from the subscriptions or the
    /// previewed podcast.
    pub(crate) fn feed_episode(
        &self,
        feed_url: &str,
        guid: &str,
    ) -> Option<(&podcasts::PodcastFeed, &podcasts::PodcastEpisode)> {
        self.subscriptions
            .iter()
            .chain(self.podcast_charts.unsubscribed_feeds())
            .find(|feed| feed.feed_url == feed_url)
            .and_then(|feed| {
                feed.playable_episodes()
                    .find(|e| e.guid == guid)
                    .map(|episode| (feed, episode))
            })
    }

    /// The accessible name the open menu publishes, resolved against live
    /// state. Delegates to [`context_menu::menu_label`] for the
    /// library-resolvable targets and names radio/podcast subjects here.
    pub(crate) fn context_menu_label(
        &self,
        target: domain::ContextTarget,
        radio_key: Option<&(u64, String)>,
        podcast_key: Option<&(String, String)>,
    ) -> Option<String> {
        use domain::ContextTarget as T;
        match target {
            T::RadioStation(_) => self
                .radio_menu_station(radio_key)
                .and_then(|(id, _)| self.radio.station(id))
                .map(|s| s.name.clone()),
            T::PodcastEpisode(_) => podcast_key
                .and_then(|(feed_url, guid)| self.episode(feed_url, guid))
                .map(|e| e.title.clone()),
            _ => context_menu::menu_label(target, &self.library, &self.playback),
        }
    }

    /// The widget focus returns to when the open menu closes, resolved against
    /// live state. `None` once the subject is gone, so a stale menu restores
    /// nothing rather than a wrong row.
    pub(crate) fn context_menu_invoker(
        &self,
        target: domain::ContextTarget,
        radio_key: Option<&(u64, String)>,
        podcast_key: Option<&(String, String)>,
    ) -> Option<iced::advanced::widget::Id> {
        use domain::ContextTarget as T;
        match target {
            // Keyed by the durable uuid, so the id survives a tune that changes
            // the row's label; `None` once the uuid stops resolving to its id.
            T::RadioStation(_) => self
                .radio_menu_station(radio_key)
                .map(|(_, uuid)| context_menu::radio_row_id(uuid)),
            // Keyed by the durable (feed_url, guid), so the id survives the
            // "Now playing" caption appearing on the current episode.
            T::PodcastEpisode(_) => podcast_key.and_then(|(feed_url, guid)| {
                self.episode(feed_url, guid)
                    .map(|_| context_menu::podcast_episode_row_id(feed_url, guid))
            }),
            _ => context_menu::menu_invoker_id(target, &self.library, &self.playback),
        }
    }

    /// The [`domain::ContextAction`] an activated id names, re-resolved against
    /// live state; `None` for a stale radio or podcast target.
    pub(crate) fn context_menu_row_for_id(
        &self,
        target: domain::ContextTarget,
        radio_key: Option<&(u64, String)>,
        podcast_key: Option<&(String, String)>,
        id: &enzui_core::menu_model::EntryId,
    ) -> Option<domain::ContextAction> {
        use domain::ContextTarget as T;
        match target {
            T::RadioStation(_) => {
                // Exact-uuid gate: a collision or catalog replacement yields
                // `None` here, so no `ContextAction` is dispatched.
                self.radio_menu_station(radio_key)?;
                [
                    domain::ContextAction::Play,
                    domain::ContextAction::ToggleFavorite,
                ]
                .into_iter()
                .find(|a| a.key() == id.as_str())
            }
            T::PodcastEpisode(_) => {
                let (feed_url, guid) = podcast_key?;
                self.episode(feed_url, guid)?;
                [
                    domain::ContextAction::Play,
                    domain::ContextAction::TrackInfo,
                ]
                .into_iter()
                .find(|a| a.key() == id.as_str())
            }
            _ => context_menu::row_for_id(target, &self.library, &self.playback, id),
        }
    }

    /// Build a fresh open menu for `target` at `at`, refusing an empty row set.
    /// `podcast_key` carries a podcast episode's durable identity.
    pub(crate) fn build_context_menu(
        &self,
        target: domain::ContextTarget,
        at: iced::Point,
        podcast_key: Option<(String, String)>,
        invoker: Option<iced::advanced::widget::Id>,
    ) -> Option<context_menu::OpenContextMenu> {
        // Snapshot the u64 id with its uuid at open time so a later reload cannot
        // retarget the menu.
        let radio_key: Option<(u64, String)> = match target {
            domain::ContextTarget::RadioStation(id) => {
                self.radio.station(id).map(|s| (id, s.remote_uuid.clone()))
            }
            _ => None,
        };
        let model = self.context_menu_model(target, radio_key.as_ref(), podcast_key.as_ref());
        let invoker = invoker.or_else(|| {
            self.context_menu_invoker(target, radio_key.as_ref(), podcast_key.as_ref())
        });
        context_menu::OpenContextMenu::new_with_restore(target, at, model, invoker)
            .map(|open| open.with_podcast_key(podcast_key).with_radio_key(radio_key))
    }
}
