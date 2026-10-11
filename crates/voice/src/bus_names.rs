//! Bus names: `bus("drums")` beside `bus(1)`.

use std::sync::Mutex;

use rustel_audio::MAX_BUSES;

/// Maps bus names to the bus slots of one host.
///
/// A number selects its own slot. A name takes the highest slot that no
/// number and no other name holds. With no free slot, the name used longest
/// ago gives up its slot, so a name that no score plays any more makes room.
/// Two sends share a slot only when more names and numbers play together
/// than there are slots.
#[derive(Debug, Default)]
pub struct BusNames {
    slots: Mutex<Slots>,
}

#[derive(Debug, Default)]
struct Slots {
    owners: [Owner; MAX_BUSES],
    /// Counts name lookups. A name keeps the count of its last lookup.
    lookups: u64,
}

#[derive(Debug, Default)]
enum Owner {
    #[default]
    Free,
    Number,
    Name {
        name: Box<str>,
        used: u64,
    },
}

impl BusNames {
    /// Keep `slot` for the bus number of the same value. A name in that slot
    /// takes a new slot at its next lookup.
    pub fn number(&self, slot: u8) {
        let mut slots = self.slots.lock().expect("bus names");
        if let Some(owner) = slots.owners.get_mut(usize::from(slot)) {
            *owner = Owner::Number;
        }
    }

    /// The slot of `name`, or `None` when numbers hold every slot.
    pub fn name(&self, name: &str) -> Option<u8> {
        let mut slots = self.slots.lock().expect("bus names");
        slots.lookups += 1;
        let now = slots.lookups;
        let held = slots.owners.iter().position(
            |owner| matches!(owner, Owner::Name { name: held, .. } if held.as_ref() == name),
        );
        let slot = held
            .or_else(|| {
                slots
                    .owners
                    .iter()
                    .rposition(|owner| matches!(owner, Owner::Free))
            })
            .or_else(|| {
                slots
                    .owners
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, owner)| match owner {
                        Owner::Name { used, .. } => Some((*used, slot)),
                        _ => None,
                    })
                    .min()
                    .map(|(_, slot)| slot)
            })?;
        slots.owners[slot] = Owner::Name {
            name: name.into(),
            used: now,
        };
        Some(slot as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_keeps_its_slot_and_stays_off_numbered_slots() {
        let names = BusNames::default();
        names.number(15);
        assert_eq!(names.name("move_0"), Some(14));
        assert_eq!(names.name("drift_0"), Some(13));
        assert_eq!(names.name("move_0"), Some(14));
        // A number takes its slot from a name.
        names.number(14);
        assert_eq!(names.name("move_0"), Some(12));
    }

    #[test]
    fn a_full_table_reuses_the_slot_of_the_name_used_longest_ago() {
        let names = BusNames::default();
        for index in 0..MAX_BUSES {
            names.name(&format!("old_{index}"));
        }
        // `old_0` holds slot 15. Every other name is newer after this lookup.
        for index in 1..MAX_BUSES {
            names.name(&format!("old_{index}"));
        }
        assert_eq!(names.name("new"), Some(15));

        let numbers = BusNames::default();
        for slot in 0..MAX_BUSES {
            numbers.number(slot as u8);
        }
        assert_eq!(numbers.name("new"), None);
    }
}
