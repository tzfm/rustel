//! The phonetic notation the synthesizer speaks.
//!
//! Text becomes this notation first, then the notation becomes sound. It is
//! the notation of the original engine: ARPAbet-style codes in capitals, a
//! stress digit from 1 to 9 after a stressed vowel, a space between words and
//! punctuation for the pauses.
//!
//! ```text
//! /HEH5LOW DHEH4R, /HAW AAR YUW5?
//! ```

/// One speech sound of the notation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ph {
    // Vowels.
    Iy,
    Ih,
    Eh,
    Ae,
    Aa,
    Ah,
    Ao,
    Ow,
    Uh,
    Uw,
    Er,
    /// The reduced vowel of `about`.
    Ax,
    /// The reduced vowel of `roses`.
    Ix,
    Ey,
    Ay,
    Oy,
    Aw,
    /// The Spanish `o`.
    Oh,
    // Syllabic consonants: a reduced vowel and the consonant in one sound.
    Ul,
    Um,
    Un,
    Il,
    Im,
    In,
    // Consonants.
    B,
    /// The Spanish `ch` written alone.
    C,
    Ch,
    D,
    Dh,
    /// The flap of `butter`.
    Dx,
    F,
    G,
    /// Aspiration before a vowel that takes no `/H`, as in `heard`, and the
    /// Spanish `j`.
    Hh,
    /// Aspiration, as in `hat`.
    H,
    /// The German `ch` of `Bach`.
    Kh,
    J,
    K,
    L,
    /// The dark `l` after a vowel.
    Lx,
    M,
    N,
    /// The `ng` of `sing`.
    Nx,
    P,
    /// Glottal stop.
    Q,
    /// Silent closure.
    Qx,
    R,
    /// The `r` after a vowel.
    Rx,
    S,
    Sh,
    T,
    Th,
    V,
    W,
    /// The `wh` of `when`.
    Wh,
    Y,
    Z,
    Zh,
}

impl Ph {
    /// The code as the notation writes it.
    pub fn code(self) -> &'static str {
        use Ph::*;
        match self {
            Iy => "IY",
            Ih => "IH",
            Eh => "EH",
            Ae => "AE",
            Aa => "AA",
            Ah => "AH",
            Ao => "AO",
            Ow => "OW",
            Uh => "UH",
            Uw => "UW",
            Er => "ER",
            Ax => "AX",
            Ix => "IX",
            Ey => "EY",
            Ay => "AY",
            Oy => "OY",
            Aw => "AW",
            Oh => "OH",
            Ul => "UL",
            Um => "UM",
            Un => "UN",
            Il => "IL",
            Im => "IM",
            In => "IN",
            B => "B",
            C => "C",
            Ch => "CH",
            D => "D",
            Dh => "DH",
            Dx => "DX",
            F => "F",
            G => "G",
            Hh => "HH",
            H => "/H",
            Kh => "/C",
            J => "J",
            K => "K",
            L => "L",
            Lx => "LX",
            M => "M",
            N => "N",
            Nx => "NX",
            P => "P",
            Q => "Q",
            Qx => "QX",
            R => "R",
            Rx => "RX",
            S => "S",
            Sh => "SH",
            T => "T",
            Th => "TH",
            V => "V",
            W => "W",
            Wh => "WH",
            Y => "Y",
            Z => "Z",
            Zh => "ZH",
        }
    }

    const ALL: [Ph; 57] = {
        use Ph::*;
        [
            Iy, Ih, Eh, Ae, Aa, Ah, Ao, Ow, Uh, Uw, Er, Ax, Ix, Ey, Ay, Oy, Aw, Oh, Ul, Um, Un, Il,
            Im, In, B, C, Ch, D, Dh, Dx, F, G, Hh, H, Kh, J, K, L, Lx, M, N, Nx, P, Q, Qx, R, Rx,
            S, Sh, T, Th, V, W, Wh, Y, Z, Zh,
        ]
    };

    fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|ph| ph.code() == code)
    }

    /// True for the sounds that carry a syllable and so can take stress.
    pub fn is_vowel(self) -> bool {
        use Ph::*;
        matches!(
            self,
            Iy | Ih
                | Eh
                | Ae
                | Aa
                | Ah
                | Ao
                | Ow
                | Uh
                | Uw
                | Er
                | Ax
                | Ix
                | Ey
                | Ay
                | Oy
                | Aw
                | Oh
                | Ul
                | Um
                | Un
                | Il
                | Im
                | In
        )
    }
}

/// One item of a phonetic string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    /// A sound, with its stress digit, 0 when unstressed.
    Sound { ph: Ph, stress: u8 },
    /// The space between two words.
    Word,
    /// Punctuation: `.`, `?`, `,`, `-`, `(`, `)`, `*` or `:`.
    Mark(char),
}

/// Why a phonetic string was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhoneticError {
    /// Byte offset of the first character that is not part of the notation.
    pub at: usize,
}

impl std::fmt::Display for PhoneticError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not a phonetic code at character {}", self.at + 1)
    }
}

/// Read a phonetic string. A two-letter code wins over a single letter, so
/// `SH` is one sound and `ST` two. Runs of spaces are one word boundary.
pub fn parse(text: &str) -> Result<Vec<Item>, PhoneticError> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b' ' | b'\t' | b'\n' | b'\r' => {
                if !matches!(out.last(), None | Some(Item::Word)) {
                    out.push(Item::Word);
                }
                i += 1;
            }
            b'.' | b'?' | b',' | b'-' | b'(' | b')' | b'*' | b':' => {
                out.push(Item::Mark(c as char));
                i += 1;
            }
            // A digit after a consonant stresses the last vowel of its word,
            // as in `FOHR5`, which the original engine reads as `FOH5R`.
            b'1'..=b'9' => {
                let vowel = out
                    .iter_mut()
                    .rev()
                    .take_while(|item| matches!(item, Item::Sound { .. }))
                    .find_map(|item| match item {
                        Item::Sound { ph, stress } if ph.is_vowel() => Some(stress),
                        _ => None,
                    });
                match vowel {
                    Some(stress) => *stress = c - b'0',
                    None => return Err(PhoneticError { at: i }),
                }
                i += 1;
            }
            _ => {
                let two = text.get(i..i + 2).and_then(Ph::from_code);
                let one = text.get(i..i + 1).and_then(Ph::from_code);
                let (ph, len) = match (two, one) {
                    (Some(ph), _) => (ph, 2),
                    (None, Some(ph)) => (ph, 1),
                    (None, None) => return Err(PhoneticError { at: i }),
                };
                out.push(Item::Sound { ph, stress: 0 });
                i += len;
            }
        }
    }
    if out.last() == Some(&Item::Word) {
        out.pop();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_codes_stress_words_and_marks() {
        let items = parse(" /HEH5LOW DHEH4R, YUW5?").unwrap();
        let sound = |ph, stress| Item::Sound { ph, stress };
        assert_eq!(
            items,
            [
                sound(Ph::H, 0),
                sound(Ph::Eh, 5),
                sound(Ph::L, 0),
                sound(Ph::Ow, 0),
                Item::Word,
                sound(Ph::Dh, 0),
                sound(Ph::Eh, 4),
                sound(Ph::R, 0),
                Item::Mark(','),
                Item::Word,
                sound(Ph::Y, 0),
                sound(Ph::Uw, 5),
                Item::Mark('?'),
            ]
        );
        assert_eq!(parse("ST").unwrap().len(), 2);
        assert_eq!(parse("FOHR5").unwrap(), parse("FOH5R").unwrap());
        assert_eq!(parse("S7").unwrap_err(), PhoneticError { at: 1 });
        assert_eq!(parse("AA S7").unwrap_err(), PhoneticError { at: 4 });
        assert_eq!(parse("AA5 E").unwrap_err(), PhoneticError { at: 4 });
    }
}
