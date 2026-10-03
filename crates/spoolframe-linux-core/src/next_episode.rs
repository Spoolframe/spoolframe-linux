//! What follows an episode in its show when nothing was lined up after
//! it: the next of its season, else the first of the next season that
//! has any. The player's Next Episode (P2-10) plays it when the episode
//! was started on its own, from Continue watching or a search.

use spoolframe_types::{Episode, Item};

use crate::backend::{BackendError, Session};

/// The episode after `episode` in its show; `None` after the last.
pub async fn next_episode(
    session: &dyn Session,
    episode: &Episode,
) -> Result<Option<Item>, BackendError> {
    let mut episodes = episodes_of(session, &episode.season_id)
        .await?
        .skip_while(|e| e.id() != &episode.id)
        .skip(1);
    if let Some(next) = episodes.next() {
        return Ok(Some(next));
    }
    let (_, seasons) = session.item(&episode.show_id).await?;
    let later = seasons
        .iter()
        .skip_while(|s| s.id() != &episode.season_id)
        .skip(1);
    for season in later {
        if let Some(first) = episodes_of(session, season.id()).await?.next() {
            return Ok(Some(first));
        }
    }
    Ok(None)
}

/// A season's episodes, in order; anything else among its children is
/// left out.
async fn episodes_of(
    session: &dyn Session,
    season: &spoolframe_types::MediaId,
) -> Result<impl Iterator<Item = Item>, BackendError> {
    let (_, children) = session.item(season).await?;
    Ok(children
        .into_iter()
        .filter(|c| matches!(c, Item::Episode(_))))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use spoolframe_types::LibraryKind;

    use super::*;
    use crate::backend::Client;
    use crate::fake::FakeClient;

    fn session() -> Arc<dyn Session> {
        FakeClient::new(None)
            .session(&FakeClient::profile("andrey"))
            .unwrap()
    }

    /// Every episode of the fake's `show`th show, season by season.
    async fn episodes(session: &dyn Session, show: usize) -> Vec<Episode> {
        let libraries = session.libraries().await.unwrap();
        let shows = libraries
            .iter()
            .find(|l| l.kind == LibraryKind::Shows)
            .unwrap();
        let (shows, _) = session.items(&shows.id, 0, 10).await.unwrap();
        let (_, seasons) = session.item(shows[show].id()).await.unwrap();
        let mut episodes = Vec::new();
        for season in seasons {
            for e in session.item(season.id()).await.unwrap().1 {
                if let Item::Episode(e) = e {
                    episodes.push(e);
                }
            }
        }
        episodes
    }

    async fn next_of(session: &dyn Session, episode: &Episode) -> Option<(u32, u32)> {
        match next_episode(session, episode).await.unwrap() {
            Some(Item::Episode(e)) => Some((e.season, e.episode)),
            _ => None,
        }
    }

    #[tokio::test]
    async fn the_next_of_its_season_follows_an_episode() {
        let session = session();
        let severance = episodes(session.as_ref(), 0).await;
        assert_eq!(next_of(session.as_ref(), &severance[0]).await, Some((2, 2)));
    }

    #[tokio::test]
    async fn the_next_seasons_first_follows_a_seasons_last() {
        // Andor: four episodes of season 1, then season 2.
        let session = session();
        let andor = episodes(session.as_ref(), 1).await;
        assert_eq!(next_of(session.as_ref(), &andor[3]).await, Some((2, 1)));
    }

    #[tokio::test]
    async fn nothing_follows_the_last_episode_of_the_last_season() {
        let session = session();
        let andor = episodes(session.as_ref(), 1).await;
        assert_eq!(next_of(session.as_ref(), andor.last().unwrap()).await, None);
    }
}
