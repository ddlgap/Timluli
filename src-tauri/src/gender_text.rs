//! Transcript-based speaker-gender inference for gendered source languages (Hebrew).
//!
//! Acoustic gender (F0 in [`crate::gender_f0`], the ONNX classifier in
//! [`crate::gender_onnx`]) fundamentally cannot tell a boy from a girl — prepubescent
//! voices are near-identical (research: even humans ~60–76%). But the *language* often
//! gives it away for free: Hebrew inflects first-person present-tense verbs/adjectives
//! by gender, so a speaker saying "אני בטוחה" / "אני הולכת" is unambiguously female and
//! "אני בטוח" / "אני הולך" is male — regardless of pitch. When the transcript is Hebrew,
//! this text signal is the ONLY thing that fixes children, and it's a near-certain
//! linguistic fact, so it OUTRANKS the acoustic guess for the speaker's own gender.
//!
//! Design = maximum precision (same "a wrong tag is worse than no tag" rule as the F0
//! path): we fire ONLY on a first-person anchor ("אני", possibly prefixed by ו/ש/כש)
//! immediately followed (within a few words, skipping adverbs) by a verb/adjective that
//! is UNAMBIGUOUSLY gendered in writing. Forms whose masculine/feminine spelling
//! collapses without niqqud (רוצה, עושה, רואה, …) are deliberately excluded. Any
//! conflicting signal within the cue → `None`. No name dataset (low coverage, homograph
//! risk, maintenance burden) — that is a deliberate future extension; grammar carries
//! the signal here.

use crate::gender_f0::SegmentGender;

/// Unambiguously FEMININE first-person present forms (the masculine counterpart has a
/// different spelling, so a written match here is reliably feminine). Kept to common,
/// low-homograph words.
const FEM: &[&str] = &[
    "בטוחה", "חושבת", "יודעת", "הולכת", "אומרת", "מרגישה", "צריכה", "מבינה", "אוהבת",
    "שמחה", "עייפה", "רעבה", "מוכנה", "יכולה", "לוקחת", "נותנת", "מדברת", "חוזרת",
    "זוכרת", "מחפשת", "אוכלת", "ישנה", "קמה", "עובדת", "כותבת", "קוראת", "שומעת",
    "נשארת", "מספרת", "שואלת", "פוחדת", "חולמת", "צוחקת", "כועסת", "מצליחה", "גרה",
    "מתחילה", "גומרת", "עוזבת", "נכנסת", "יוצאת", "עומדת", "יושבת", "מנהלת", "מקבלת",
];

/// Unambiguously MASCULINE first-person present forms (feminine adds ת/ה → different
/// spelling). Avoids words that double as common nouns/other POS (שר, בא, רץ, …).
const MASC: &[&str] = &[
    "בטוח", "חושב", "יודע", "הולך", "אומר", "מרגיש", "צריך", "מבין", "אוהב", "שמח",
    "עייף", "רעב", "מוכן", "יכול", "לוקח", "נותן", "מדבר", "חוזר", "זוכר", "מחפש",
    "אוכל", "ישן", "עובד", "כותב", "שומע", "נשאר", "מספר", "שואל", "פוחד", "חולם",
    "צוחק", "כועס", "מצליח", "מתחיל", "גומר", "עוזב", "נכנס", "יוצא", "עומד", "יושב",
    "מנהל", "מקבל",
];

// ─── Name gazetteer (cross-language) ─────────────────────────────────────────────
// Names indicate gender ~independently of language, so a name resolved to the SPEAKER
// extends the signal to non-gendered source languages (English, etc.). The ONLY safe
// per-cue attribution is SELF-INTRODUCTION ("my name is X", "I'm X", "שמי X") — vocative
// ("X, come!") and third-person mentions refer to someone else and need diarization to
// place, so they are not used here. Lists are lowercase; clearly-gendered, common,
// film-relevant names only — dual-use/homograph names (will, may, רן, דן, גל, …) and
// unisex names (alex, jordan, noa, …) are deliberately omitted (→ no signal).
const NAME_F: &[&str] = &[
    // English / international
    "mary", "patricia", "jennifer", "linda", "elizabeth", "barbara", "susan", "jessica",
    "sarah", "karen", "lisa", "nancy", "betty", "sandra", "margaret", "ashley", "kimberly",
    "emily", "donna", "michelle", "carol", "amanda", "melissa", "deborah", "stephanie",
    "rebecca", "laura", "helen", "anna", "anne", "julie", "rachel", "hannah", "emma",
    "olivia", "sophia", "isabella", "charlotte", "amelia", "ella", "chloe", "lily", "julia",
    "maria", "elena", "sofia", "clara", "alice", "diana", "jane", "kate", "katie", "lucy",
    "ruth", "naomi", "leah", "abigail", "esther", "miriam", "nicole", "victoria", "claire",
    "fiona", "joan", "joanna", "judy", "kelly", "lauren", "megan", "molly", "samantha",
    "tina", "wendy", "gloria", "monica", "rita", "paula", "angela", "christine", "catherine",
    "janet", "marie", "diane", "heather", "teresa", "natalie", "vanessa", "carmen",
    // Hebrew
    "שרה", "רחל", "לאה", "מרים", "נעמי", "אסתר", "חנה", "רבקה", "תמר", "מיכל", "יעל",
    "שירה", "מאיה", "אורית", "רונית", "גלית", "סיגל", "ענת", "אפרת", "הדס", "ליאת",
    "אביגיל", "דפנה", "מירב", "יהודית", "נורית", "שולמית", "בתיה", "אילנה", "ורד", "מיטל",
    "הילה", "קרן", "אורלי", "סמדר", "רותי", "שירן", "ליטל", "מיכל", "רוית",
];
const NAME_M: &[&str] = &[
    // English / international
    "james", "john", "robert", "michael", "william", "david", "richard", "joseph", "thomas",
    "charles", "christopher", "daniel", "matthew", "anthony", "donald", "steven", "paul",
    "andrew", "joshua", "kenneth", "kevin", "brian", "george", "edward", "ronald", "timothy",
    "jason", "jeffrey", "ryan", "jacob", "gary", "nicholas", "eric", "jonathan", "stephen",
    "larry", "justin", "scott", "brandon", "benjamin", "samuel", "gregory", "alexander",
    "patrick", "dennis", "jerry", "henry", "peter", "simon", "adam", "aaron", "nathan",
    "luke", "leo", "oscar", "harry", "oliver", "liam", "ethan", "frank", "raymond", "carl",
    "joe", "mike", "dave", "tom", "tony", "fred", "ralph", "roger", "walter", "sean",
    "martin", "victor", "philip", "arthur", "albert", "louis", "harold", "eugene",
    // Hebrew
    "דוד", "משה", "יוסף", "יעקב", "אברהם", "יצחק", "מיכאל", "דניאל", "יונתן", "איתן",
    "עומר", "איתי", "אסף", "עידו", "יואב", "אלון", "ניר", "עידן", "שמואל", "בנימין",
    "נתן", "אלי", "חיים", "מנחם", "שלמה", "אהרון", "מרדכי", "ראובן", "שמעון", "יהודה",
    "זאב", "אריה", "ברוך", "מאיר", "רועי", "אורן", "גלעד", "עמוס", "גדעון", "יורם",
    "רפאל", "נדב", "אייל", "צבי", "נחום", "מתן",
];

/// Adverbs/negation that may sit between "אני" and the gendered verb; skipped without
/// spending the look-ahead budget ("אני לא ממש יודעת...").
const SKIP: &[&str] = &[
    "לא", "כבר", "עוד", "ממש", "מאוד", "באמת", "רק", "גם", "אף", "תמיד", "אולי",
    "בכלל", "פשוט", "כך", "הרי", "כן", "עדיין", "כמעט", "בדיוק", "אפילו",
];

/// Other-subject pronouns: hitting one ends the first-person clause (so we never read
/// an addressee's/3rd-party's inflection as the speaker's). Includes accusative "את".
const SUBJECT_BREAK: &[&str] = &[
    "את", "אתה", "אתם", "אתן", "הוא", "היא", "הם", "הן", "אנחנו", "אני",
];

/// Trim non-Hebrew-letter characters (punctuation, Latin, digits) off both ends.
fn clean(tok: &str) -> &str {
    tok.trim_matches(|c: char| !('\u{05D0}'..='\u{05EA}').contains(&c))
}

/// Trim non-letter chars off both ends and lowercase (ASCII; Hebrew is caseless),
/// keeping internal apostrophes so contractions like "i'm" survive. Used for the
/// cross-language name gazetteer (works for Latin and Hebrew tokens alike).
fn clean_name(tok: &str) -> String {
    tok.trim_matches(|c: char| !c.is_alphabetic()).to_lowercase()
}

fn name_gender(name: &str) -> Option<SegmentGender> {
    if NAME_F.contains(&name) {
        Some(SegmentGender::Female)
    } else if NAME_M.contains(&name) {
        Some(SegmentGender::Male)
    } else {
        None
    }
}

/// Strip a single leading conjunction/relativizer (ו / ש / כש) for matching.
fn destem(w: &str) -> &str {
    for p in ["כש", "ש", "ו"] {
        if let Some(r) = w.strip_prefix(p) {
            if !r.is_empty() {
                return r;
            }
        }
    }
    w
}

fn is_ani(w: &str) -> bool {
    w == "אני" || destem(w) == "אני"
}

fn is_subject_break(w: &str) -> bool {
    SUBJECT_BREAK.contains(&w) || SUBJECT_BREAK.contains(&destem(w))
}

/// Gender of a single candidate word, if it is an unambiguous first-person form.
fn word_gender(w: &str) -> Option<SegmentGender> {
    let d = destem(w);
    if FEM.contains(&w) || FEM.contains(&d) {
        Some(SegmentGender::Female)
    } else if MASC.contains(&w) || MASC.contains(&d) {
        Some(SegmentGender::Male)
    } else {
        None
    }
}

/// Pass 1 — Hebrew first-person gender morphology ("אני בטוחה" → F, "אני הולך" → M).
fn grammar_pass(text: &str, saw_f: &mut bool, saw_m: &mut bool) {
    let toks: Vec<&str> = text
        .split_whitespace()
        .map(clean)
        .filter(|t| !t.is_empty())
        .collect();
    for i in 0..toks.len() {
        if !is_ani(toks[i]) {
            continue;
        }
        // Look ahead from this "אני" for the first gendered verb/adjective, skipping
        // adverbs, stopping at a new subject pronoun or after a few content words.
        let mut budget = 4;
        let mut j = i + 1;
        while j < toks.len() && budget > 0 {
            let w = toks[j];
            if is_subject_break(w) {
                break;
            }
            if SKIP.contains(&w) {
                j += 1;
                continue;
            }
            match word_gender(w) {
                Some(SegmentGender::Female) => {
                    *saw_f = true;
                    break;
                }
                Some(SegmentGender::Male) => {
                    *saw_m = true;
                    break;
                }
                _ => {}
            }
            budget -= 1;
            j += 1;
        }
    }
}

/// Pass 2 — cross-language SELF-INTRODUCTION → speaker name → gender. Fires only on an
/// explicit self-intro anchor ("my name is X" / "i'm X" / "call me X" / "שמי X" /
/// "קוראים לי X" / "אני X"); the introduced token is looked up in the name gazetteer.
fn name_pass(text: &str, saw_f: &mut bool, saw_m: &mut bool) {
    let tl: Vec<String> = text
        .split_whitespace()
        .map(clean_name)
        .filter(|t| !t.is_empty())
        .collect();
    let at = |k: usize| tl.get(k).map(String::as_str);
    for i in 0..tl.len() {
        let t = tl[i].as_str();
        let name_idx = if t == "i'm" || t == "im" || t == "שמי" || t == "אני" {
            Some(i + 1)
        } else if t == "i" && at(i + 1) == Some("am") {
            Some(i + 2)
        } else if t == "call" && at(i + 1) == Some("me") {
            Some(i + 2)
        } else if t == "קוראים" && at(i + 1) == Some("לי") {
            Some(i + 2)
        } else if t == "my" && at(i + 1) == Some("name") && at(i + 2) == Some("is") {
            Some(i + 3)
        } else {
            None
        };
        if let Some(name) = name_idx.and_then(at) {
            match name_gender(name) {
                Some(SegmentGender::Female) => *saw_f = true,
                Some(SegmentGender::Male) => *saw_m = true,
                _ => {}
            }
        }
    }
}

/// Infer the SPEAKER's gender from a cue's text, cross-referencing the Hebrew grammar
/// signal and the cross-language self-introduction name signal. `None` when there is no
/// clear signal OR the two signals conflict (do no harm). Safe on any language: text
/// that matches neither the Hebrew lexicon nor the name gazetteer yields `None`.
pub fn infer_speaker_gender(text: &str) -> Option<SegmentGender> {
    let (mut saw_f, mut saw_m) = (false, false);
    grammar_pass(text, &mut saw_f, &mut saw_m);
    name_pass(text, &mut saw_f, &mut saw_m);
    match (saw_f, saw_m) {
        (true, false) => Some(SegmentGender::Female),
        (false, true) => Some(SegmentGender::Male),
        _ => None, // none, or conflicting → do no harm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(t: &str) -> Option<SegmentGender> {
        infer_speaker_gender(t)
    }

    #[test]
    fn first_person_feminine_is_female() {
        assert_eq!(g("אני בטוחה שזה נכון"), Some(SegmentGender::Female));
        assert_eq!(g("אני הולכת הביתה עכשיו"), Some(SegmentGender::Female));
        assert_eq!(g("אני לא ממש יודעת מה לעשות"), Some(SegmentGender::Female));
        assert_eq!(g("ואני שמחה מאוד"), Some(SegmentGender::Female)); // ו-prefix on אני
    }

    #[test]
    fn first_person_masculine_is_male() {
        assert_eq!(g("אני בטוח שזה נכון"), Some(SegmentGender::Male));
        assert_eq!(g("אני יודע"), Some(SegmentGender::Male));
        assert_eq!(g("אני אוהב את שרה"), Some(SegmentGender::Male)); // verb before accusative את
    }

    #[test]
    fn addressee_and_third_person_do_not_leak() {
        // "את הולכת" = YOU(f) go — addressee, anchored on את not אני → no speaker signal.
        assert_eq!(g("את הולכת מחר?"), None);
        assert_eq!(g("היא יודעת הכל"), None);
        // Speaker masculine, addressee feminine in the same cue: must read the SPEAKER.
        assert_eq!(g("אני חושב שאת צודקת"), Some(SegmentGender::Male));
    }

    #[test]
    fn ambiguous_and_conflicting_yield_none() {
        // רוצה / עושה collapse m/f without niqqud → not in the lexicon → None.
        assert_eq!(g("אני רוצה ללכת"), None);
        assert_eq!(g("אני עושה את זה"), None);
        // Conflicting first-person signals in one cue → None (do no harm).
        assert_eq!(g("אני יודע אבל אני חושבת אחרת"), None);
        // No first-person anchor at all.
        assert_eq!(g("שלום מה שלומך היום"), None);
        // English text with no name never matches.
        assert_eq!(g("I am going home now"), None);
    }

    #[test]
    fn self_introduction_name_gives_speaker_gender_cross_language() {
        assert_eq!(g("My name is Michael"), Some(SegmentGender::Male));
        assert_eq!(g("Hi, I'm Sarah!"), Some(SegmentGender::Female));
        assert_eq!(g("call me Dave"), Some(SegmentGender::Male));
        assert_eq!(g("שמי שרה"), Some(SegmentGender::Female));
        assert_eq!(g("קוראים לי דוד"), Some(SegmentGender::Male));
        assert_eq!(g("אני מיכאל ונעים מאוד"), Some(SegmentGender::Male));
        // Anchor present but the next token is not a name → no signal.
        assert_eq!(g("I'm happy to be here"), None);
        assert_eq!(g("my name is not important"), None);
        // Unisex name → no signal (omitted from the gazetteer).
        assert_eq!(g("I'm Alex"), None);
        // Grammar (M) vs self-intro name (F) conflict → None (do no harm).
        assert_eq!(g("אני הולך, קוראים לי שרה"), None);
    }
}
