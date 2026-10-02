//! Nampower's cast records and the ring it keeps them in (`castqueue.h`): the non-GCD queue (six
//! casts, oldest first) and the cast history (thirty, newest first).

use std::collections::VecDeque;

use benilla_app::ext::ItemUse;

/// `CastType`, as `SPELL_CAST_EVENT` reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CastType {
    #[default]
    Normal = 0,
    NonGcd = 1,
    OnSwing = 2,
    Channel = 3,
    Targeting = 4,
    TargetingNonGcd = 5,
}

impl CastType {
    pub fn non_gcd(self) -> bool {
        matches!(self, CastType::NonGcd | CastType::TargetingNonGcd)
    }
}

/// `QueueEvents`, `SPELL_QUEUE_EVENT`'s first argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueEvent {
    OnSwingQueued = 0,
    OnSwingQueuePopped = 1,
    NormalQueued = 2,
    NormalQueuePopped = 3,
    NonGcdQueued = 4,
    NonGcdQueuePopped = 5,
}

/// Where a cast stands with the server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CastResult {
    #[default]
    WaitingForCast,
    WaitingForServer,
    ServerSuccess,
    ServerFailure,
}

/// One cast, as nampower records it (`CastSpellParams`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CastParams {
    pub cast_id: u64,
    pub spell_id: u32,
    pub item: Option<ItemUse>,
    /// The unit the cast was aimed at; `None` casts at the selection.
    pub target: Option<u64>,
    /// `StartRecoveryCategory`.
    pub gcd_category: u32,
    pub cast_time_ms: u64,
    pub start_ms: u64,
    pub cast_type: CastType,
    pub retries: u32,
    pub result: CastResult,
}

/// A bounded ring of casts. `push` appends at the back and drops the front when full;
/// `push_front` prepends and drops the back.
#[derive(Clone, Debug)]
pub struct CastQueue {
    max: usize,
    items: VecDeque<CastParams>,
}

impl CastQueue {
    pub fn new(max: usize) -> Self {
        Self {
            max,
            items: VecDeque::with_capacity(max),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Append; with `replace_category`, a non-GCD cast replaces the queued one sharing its
    /// nonzero `StartRecoveryCategory` instead.
    pub fn push(&mut self, params: CastParams, replace_category: bool) {
        if replace_category && params.cast_type == CastType::NonGcd && params.gcd_category != 0 {
            if let Some(slot) = self
                .items
                .iter_mut()
                .find(|p| p.gcd_category == params.gcd_category)
            {
                *slot = params;
                return;
            }
        }
        if self.items.len() == self.max {
            self.items.pop_front();
        }
        self.items.push_back(params);
    }

    pub fn push_front(&mut self, params: CastParams) {
        if self.items.len() == self.max {
            self.items.pop_back();
        }
        self.items.push_front(params);
    }

    pub fn pop(&mut self) -> Option<CastParams> {
        self.items.pop_front()
    }

    pub fn peek(&self) -> Option<&CastParams> {
        self.items.front()
    }

    pub fn peek_mut(&mut self) -> Option<&mut CastParams> {
        self.items.front_mut()
    }

    /// The first cast of `spell_id` from the front.
    pub fn find_spell(&mut self, spell_id: u32) -> Option<&mut CastParams> {
        self.items.iter_mut().find(|p| p.spell_id == spell_id)
    }

    pub fn remove_spell(&mut self, spell_id: u32) -> bool {
        match self.items.iter().position(|p| p.spell_id == spell_id) {
            Some(i) => {
                self.items.remove(i);
                true
            }
            None => false,
        }
    }

    fn find_from_front(&mut self, spell_id: u32, result: CastResult) -> Option<&mut CastParams> {
        self.items
            .iter_mut()
            .find(|p| p.spell_id == spell_id && p.result == result)
    }

    /// In the history (newest first): the newest cast of `spell_id` still waiting.
    pub fn newest_waiting(&mut self, spell_id: u32) -> Option<&mut CastParams> {
        self.find_from_front(spell_id, CastResult::WaitingForServer)
    }

    /// In the history: the oldest cast of `spell_id` still waiting.
    pub fn oldest_waiting(&mut self, spell_id: u32) -> Option<&mut CastParams> {
        self.items
            .iter_mut()
            .rev()
            .find(|p| p.spell_id == spell_id && p.result == CastResult::WaitingForServer)
    }

    /// In the history: the newest cast of `spell_id` the server accepted.
    pub fn newest_success(&mut self, spell_id: u32) -> Option<&mut CastParams> {
        self.find_from_front(spell_id, CastResult::ServerSuccess)
    }

    pub fn iter(&self) -> impl Iterator<Item = &CastParams> {
        self.items.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cast(spell_id: u32, gcd_category: u32) -> CastParams {
        CastParams {
            spell_id,
            gcd_category,
            cast_type: CastType::NonGcd,
            ..Default::default()
        }
    }

    #[test]
    fn a_full_queue_drops_its_oldest() {
        let mut q = CastQueue::new(2);
        q.push(cast(1, 0), false);
        q.push(cast(2, 0), false);
        q.push(cast(3, 0), false);
        assert_eq!(q.pop().map(|p| p.spell_id), Some(2));
        assert_eq!(q.pop().map(|p| p.spell_id), Some(3));
        assert!(q.pop().is_none());
    }

    #[test]
    fn a_matching_category_replaces_in_place_only_when_asked() {
        let mut q = CastQueue::new(6);
        q.push(cast(1, 7), true);
        q.push(cast(2, 7), true);
        assert_eq!(q.len(), 1);
        assert_eq!(q.peek().map(|p| p.spell_id), Some(2));
        q.push(cast(3, 7), false);
        assert_eq!(q.len(), 2);
        // Category 0 never matches.
        q.push(cast(4, 0), true);
        q.push(cast(5, 0), true);
        assert_eq!(q.len(), 4);
    }

    #[test]
    fn the_history_drops_its_oldest_and_finds_by_age() {
        let mut h = CastQueue::new(3);
        for (id, at) in [(10, 1), (10, 2), (11, 3), (10, 4)] {
            h.push_front(CastParams {
                spell_id: id,
                start_ms: at,
                result: CastResult::WaitingForServer,
                ..Default::default()
            });
        }
        assert_eq!(h.len(), 3);
        assert_eq!(h.newest_waiting(10).map(|p| p.start_ms), Some(4));
        assert_eq!(h.oldest_waiting(10).map(|p| p.start_ms), Some(2));
    }
}
