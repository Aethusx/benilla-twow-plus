//! The template caches behind `Cache::QueryLoad`: creature, gameobject and quest records as the
//! server answers them, the loads the API asked for, and their `*_DATA_LOAD_RESULT(id, success)`.
//!
//! Every answer is taken, benilla's own queries' included, as the engine's cache holds them all.
//! A load already cached succeeds at once; an asked one succeeds when its answer lands and fails
//! after [`WAIT`] (the DLL's 1200 world ticks). Deviation: an answer naming no such record fails
//! the load then, where the DLL waits out its timeout.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use benilla_app::ext::ExtQuery;
use benilla_protocol::messages::QuestTemplate;

/// How long an asked load waits for its answer.
const WAIT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Default)]
pub struct Creature {
    pub name: String,
    pub subname: String,
    pub creature_type: u32,
    pub family: u32,
    pub rank: u32,
    pub display_id: u32,
}

#[derive(Clone, Debug, Default)]
pub struct GameObject {
    pub name: String,
    pub type_id: u32,
    pub display_id: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Creature,
    GameObject,
    Quest,
}

impl Kind {
    fn event(self) -> &'static str {
        match self {
            Kind::Creature => "CREATURE_DATA_LOAD_RESULT",
            Kind::GameObject => "GAMEOBJECT_DATA_LOAD_RESULT",
            Kind::Quest => "QUEST_DATA_LOAD_RESULT",
        }
    }

    fn ask(self, id: u32) -> ExtQuery {
        match self {
            Kind::Creature => ExtQuery::Creature(id),
            Kind::GameObject => ExtQuery::GameObject(id),
            Kind::Quest => ExtQuery::Quest(id),
        }
    }
}

#[derive(Default)]
pub struct Templates {
    pub creatures: HashMap<u32, Creature>,
    pub gameobjects: HashMap<u32, GameObject>,
    pub quests: HashMap<u32, Box<QuestTemplate>>,
    pending: HashMap<(Kind, u32), Instant>,
    asks: Vec<ExtQuery>,
    results: Vec<(&'static str, u32, bool)>,
}

impl Templates {
    pub fn cached(&self, kind: Kind, id: u32) -> bool {
        match kind {
            Kind::Creature => self.creatures.contains_key(&id),
            Kind::GameObject => self.gameobjects.contains_key(&id),
            Kind::Quest => self.quests.contains_key(&id),
        }
    }

    /// `RequestLoad`: a cached record succeeds now; otherwise the query goes out once and the
    /// load waits.
    pub fn request(&mut self, kind: Kind, id: u32, now: Instant) {
        if self.cached(kind, id) {
            self.results.push((kind.event(), id, true));
            return;
        }
        if self.pending.insert((kind, id), now).is_none() {
            self.asks.push(kind.ask(id));
        }
    }

    fn answered(&mut self, kind: Kind, id: u32, found: bool) {
        if self.pending.remove(&(kind, id)).is_some() {
            self.results.push((kind.event(), id, found));
        }
    }

    /// `SMSG_CREATURE_QUERY_RESPONSE`; `None` is no such creature.
    pub fn on_creature(&mut self, entry: u32, rec: Option<Creature>) {
        let found = rec.is_some();
        if let Some(rec) = rec {
            self.creatures.insert(entry, rec);
        }
        self.answered(Kind::Creature, entry, found);
    }

    /// `SMSG_GAMEOBJECT_QUERY_RESPONSE`; an unknown entry answers with an empty name.
    pub fn on_gameobject(&mut self, entry: u32, rec: GameObject) {
        let found = !rec.name.is_empty();
        if found {
            self.gameobjects.insert(entry, rec);
        }
        self.answered(Kind::GameObject, entry, found);
    }

    pub fn on_quest(&mut self, quest: Box<QuestTemplate>) {
        let id = quest.quest_id;
        self.quests.insert(id, quest);
        self.answered(Kind::Quest, id, true);
    }

    /// The queries to send and the results to fire this frame, the timed-out loads failed.
    pub fn tick(&mut self, now: Instant) -> (Vec<ExtQuery>, Vec<(&'static str, u32, bool)>) {
        let expired: Vec<(Kind, u32)> = self
            .pending
            .iter()
            .filter(|(_, at)| now.duration_since(**at) >= WAIT)
            .map(|(k, _)| *k)
            .collect();
        for (kind, id) in expired {
            self.pending.remove(&(kind, id));
            self.results.push((kind.event(), id, false));
        }
        (
            std::mem::take(&mut self.asks),
            std::mem::take(&mut self.results),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_load_asks_once_and_answers_once() {
        let mut t = Templates::default();
        let t0 = Instant::now();
        t.request(Kind::Creature, 69, t0);
        t.request(Kind::Creature, 69, t0);
        let (asks, results) = t.tick(t0);
        assert_eq!(asks, [ExtQuery::Creature(69)]);
        assert!(results.is_empty());
        t.on_creature(69, Some(Creature::default()));
        assert_eq!(t.tick(t0).1, [("CREATURE_DATA_LOAD_RESULT", 69, true)]);
        // Cached now: a second load succeeds at once, with no query.
        t.request(Kind::Creature, 69, t0);
        assert_eq!(
            t.tick(t0),
            (vec![], vec![("CREATURE_DATA_LOAD_RESULT", 69, true)])
        );
        // Unanswered: fails after the wait.
        t.request(Kind::GameObject, 5, t0);
        t.tick(t0);
        assert_eq!(
            t.tick(t0 + WAIT).1,
            [("GAMEOBJECT_DATA_LOAD_RESULT", 5, false)]
        );
    }
}
