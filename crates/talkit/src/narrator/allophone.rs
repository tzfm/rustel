//! The sound changes the original engine makes before it speaks.
//!
//! - A T or D after a vowel or an R and before an unstressed vowel becomes
//!   the flap DX, as in `butter`. A word-final T or D flaps before an
//!   unstressed vowel that starts the next word.
//! - An L right after a vowel becomes the dark LX, unless a stressed vowel
//!   follows it in the word.

use crate::phonetic::{Item, Ph};

/// The sound at `at`, if the item there is one.
fn sound(items: &[Item], at: usize) -> Option<(Ph, u8)> {
    match items.get(at) {
        Some(Item::Sound { ph, stress }) => Some((*ph, *stress)),
        _ => None,
    }
}

pub(super) fn apply(items: &mut [Item]) {
    for i in 0..items.len() {
        let Some((ph, _)) = sound(items, i) else {
            continue;
        };
        let before = i.checked_sub(1).and_then(|p| sound(items, p));
        match ph {
            Ph::T | Ph::D => {
                let after_vowel =
                    before.is_some_and(|(p, _)| p.is_vowel() || matches!(p, Ph::R | Ph::Rx));
                // The next sound, across one word boundary.
                let next = match items.get(i + 1) {
                    Some(Item::Word) => sound(items, i + 2),
                    _ => sound(items, i + 1),
                };
                let unstressed_vowel = next.is_some_and(|(p, s)| p.is_vowel() && s == 0);
                if after_vowel && unstressed_vowel {
                    items[i] = Item::Sound {
                        ph: Ph::Dx,
                        stress: 0,
                    };
                }
            }
            Ph::L => {
                let after_vowel = before.is_some_and(|(p, _)| p.is_vowel());
                let stressed_next = sound(items, i + 1).is_some_and(|(p, s)| p.is_vowel() && s > 0);
                if after_vowel && !stressed_next {
                    items[i] = Item::Sound {
                        ph: Ph::Lx,
                        stress: 0,
                    };
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phonetic::parse;

    fn changed(text: &str) -> Vec<Item> {
        let mut items = parse(text).unwrap();
        apply(&mut items);
        items
    }

    #[test]
    fn flaps_and_dark_l_follow_the_engine() {
        assert_eq!(changed("BAH5TER"), parse("BAH5DXER").unwrap());
        assert_eq!(changed("AA5T AA"), parse("AA5DX AA").unwrap());
        assert_eq!(changed("AA5 TAA"), parse("AA5 TAA").unwrap());
        assert_eq!(changed("AA5TAA5"), parse("AA5TAA5").unwrap());
        assert_eq!(changed("SEH5LZ"), parse("SEH5LXZ").unwrap());
        assert_eq!(changed("AA5LAA5"), parse("AA5LAA5").unwrap());
        assert_eq!(changed("SLAA5"), parse("SLAA5").unwrap());
    }
}
