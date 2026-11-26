//! Which providers have just reached the red line of their limit (95 %) and
//! deserve the island's announcement. Pure state: nothing here draws or arms
//! a timer.

use crate::usage::{ActiveProviders, Level, Provider, Snapshot, State};

/// Remembers which providers are in red, to announce each one once per climb.
#[derive(Default)]
pub struct AlertTracker {
    critical: [bool; Provider::COUNT],
}

impl AlertTracker {
    /// Reads a new snapshot and returns the providers to announce now, in
    /// the order they crossed (those crossing in the same reading, in the
    /// island's order).
    ///
    /// A provider is announced when it passes to critical from anything
    /// else, including on the first reading after starting; it is not
    /// announced again while it stays critical, and drops out of red (so it
    /// can be announced again) once it falls under 95 %. A reading that is
    /// not `Ready` (loading, signed out, unavailable) neither announces nor
    /// re-arms: it says nothing about the limit.
    pub fn observe(&mut self, snapshot: &Snapshot, active: ActiveProviders) -> Vec<Provider> {
        let mut announce = Vec::new();
        for provider in Provider::ALL {
            let usage = snapshot.get(provider);
            if !active.has(provider) || usage.state != State::Ready {
                continue;
            }
            let critical = usage.alert_level() == Level::Critical;
            let was = std::mem::replace(&mut self.critical[provider as usize], critical);
            if critical && !was {
                announce.push(provider);
            }
        }
        announce
    }

    /// Whether the provider is in red as of its last `Ready` reading.
    #[cfg(test)]
    pub fn critical(&self, provider: Provider) -> bool {
        self.critical[provider as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{ProviderUsage, Window};

    fn reading(state: State, percent: u8) -> ProviderUsage {
        ProviderUsage {
            state,
            windows: vec![Window {
                label: None,
                minutes: 300,
                used_percent: percent,
                resets_at: None,
            }],
            fetched_at: None,
        }
    }

    fn snapshot(claude: Option<ProviderUsage>, codex: Option<ProviderUsage>) -> Snapshot {
        let mut snapshot = Snapshot::default();
        if let Some(usage) = claude {
            snapshot.providers[Provider::Claude as usize] = usage;
        }
        if let Some(usage) = codex {
            snapshot.providers[Provider::Codex as usize] = usage;
        }
        snapshot
    }

    fn both() -> ActiveProviders {
        let mut active = ActiveProviders::default();
        active.0[Provider::Claude as usize] = true;
        active.0[Provider::Codex as usize] = true;
        active
    }

    #[test]
    fn the_first_reading_in_red_is_announced_once() {
        let mut tracker = AlertTracker::default();
        let red = snapshot(Some(reading(State::Ready, 97)), None);
        assert_eq!(tracker.observe(&red, both()), vec![Provider::Claude]);
        assert!(tracker.critical(Provider::Claude));
        let redder = snapshot(Some(reading(State::Ready, 98)), None);
        assert_eq!(tracker.observe(&redder, both()), Vec::<Provider>::new());
    }

    #[test]
    fn falling_under_95_re_arms_the_announcement() {
        let mut tracker = AlertTracker::default();
        let at = |percent| snapshot(Some(reading(State::Ready, percent)), None);
        assert!(tracker.observe(&at(90), both()).is_empty());
        assert_eq!(tracker.observe(&at(96), both()), vec![Provider::Claude]);
        assert!(tracker.observe(&at(94), both()).is_empty());
        assert!(!tracker.critical(Provider::Claude));
        assert_eq!(tracker.observe(&at(95), both()), vec![Provider::Claude]);
    }

    #[test]
    fn two_providers_crossing_together_come_in_the_islands_order() {
        let mut tracker = AlertTracker::default();
        let both_red = snapshot(
            Some(reading(State::Ready, 96)),
            Some(reading(State::Ready, 100)),
        );
        assert_eq!(
            tracker.observe(&both_red, both()),
            vec![Provider::Claude, Provider::Codex]
        );
    }

    #[test]
    fn a_reading_that_is_not_ready_neither_announces_nor_re_arms() {
        let mut tracker = AlertTracker::default();
        let red = snapshot(Some(reading(State::Ready, 97)), None);
        assert_eq!(tracker.observe(&red, both()).len(), 1);
        // The network drops: the stale 97 % must not announce again.
        let lost = snapshot(Some(reading(State::Unavailable, 97)), None);
        assert!(tracker.observe(&lost, both()).is_empty());
        assert!(tracker.observe(&red, both()).is_empty());
        // A signed-out provider in red never announces.
        let mut other = AlertTracker::default();
        let signed_out = snapshot(Some(reading(State::SignIn, 99)), None);
        assert!(other.observe(&signed_out, both()).is_empty());
        assert!(!other.critical(Provider::Claude));
    }

    #[test]
    fn a_provider_that_is_not_running_is_not_announced() {
        let mut tracker = AlertTracker::default();
        let red = snapshot(Some(reading(State::Ready, 99)), None);
        assert!(tracker.observe(&red, ActiveProviders::default()).is_empty());
        assert_eq!(tracker.observe(&red, both()), vec![Provider::Claude]);
    }
}
