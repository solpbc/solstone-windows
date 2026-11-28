// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Plain-text conversation signatures from the journal's import triage at 97652c1e.

use regex::Regex;
use std::{borrow::Cow, collections::HashSet, sync::LazyLock};

pub(crate) fn matches(text: &str) -> bool {
    let mut lines = Vec::new();
    for line in text.lines() {
        if lines.len() == super::structured_exports::MAX_TURNS {
            return false;
        }
        lines.push(line);
    }
    discord(&lines)
        || otter(&lines)
        || rev(&lines)
        || irc(&lines)
        || whatsapp(&lines, true)
        || whatsapp(&lines, false)
}

fn irc(lines: &[&str]) -> bool {
    static ROW: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[0-9]{2}:[0-9]{2}(?::[0-9]{2})?\s+<[ @+%~&]?(?P<speaker>[^>]{1,40})>\s+\S.+$")
            .unwrap()
    });
    let mut speakers = HashSet::new();
    let mut turns = 0;
    for line in nonblank(lines) {
        if is_export_notice(line) {
            continue;
        }
        let Some(speaker) = ROW
            .captures(line)
            .and_then(|m| m.name("speaker"))
            .and_then(|v| speaker_label(v.as_str()))
        else {
            return false;
        };
        speakers.insert(speaker);
        turns += 1;
    }
    speakers.len() >= 2 && turns >= 3
}

fn speaker_label(label: &str) -> Option<&str> {
    let label = label.trim().trim_matches('*').trim();
    let label = match label.rsplit_once(" (") {
        Some((name, annotation)) if annotation.ends_with(')') => {
            let annotation = annotation.trim_end_matches(')').trim();
            // Dictionary headings use grammatical labels in the same position
            // as the pronouns or affiliation in a speaker's display name.
            if matches!(
                annotation.to_ascii_lowercase().as_str(),
                "noun"
                    | "verb"
                    | "adjective"
                    | "adj"
                    | "adverb"
                    | "adv"
                    | "pronoun"
                    | "preposition"
                    | "conjunction"
                    | "interjection"
                    | "determiner"
                    | "article"
            ) {
                return None;
            }
            name
        }
        _ => label,
    };
    static EMOJI: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:\p{Extended_Pictographic}|\p{Regional_Indicator})(?:\p{Extended_Pictographic}|\p{Regional_Indicator}|[\x{fe0f}\x{200d}\x{1f3fb}-\x{1f3ff}])*$").unwrap()
    });
    // The Slack export includes the literal <external> namespace in the label.
    let external_id = label.strip_prefix("<external>:").is_some_and(|id| {
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    });
    (name_label(label) || label.chars().count() <= 20 && EMOJI.is_match(label) || external_id)
        .then_some(label)
}

fn name_label(label: &str) -> bool {
    let label = label.trim();
    !label.is_empty()
        && label.chars().count() <= 40
        && label.split_whitespace().count() <= 5
        && label
            .chars()
            .any(|c| c.is_alphabetic() || c.is_ascii_digit())
        && label
            .chars()
            .all(|c| c.is_alphanumeric() || " ._'’+-@".contains(c))
        && !matches!(
            label.to_ascii_lowercase().as_str(),
            "note"
                | "todo"
                | "warning"
                | "tip"
                | "update"
                | "summary"
                | "status"
                | "owner"
                | "date"
                | "title"
                | "subject"
                | "re"
                | "prep"
                | "cook"
                | "serves"
                | "yield"
                | "ingredients"
                | "step"
                | "pros"
                | "cons"
                | "example"
                | "source"
                | "tags"
                | "author"
                | "from"
                | "to"
                | "sent"
                | "cc"
                | "http"
                | "https"
                | "mailto"
                | "info"
                | "error"
                | "warn"
                | "debug"
                | "trace"
                | "agenda"
                | "attendees"
                | "decisions"
                | "action items"
                | "priority"
        )
}

fn nonblank<'a>(lines: &'a [&'a str]) -> impl Iterator<Item = &'a str> {
    lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
}

fn is_export_notice(line: &str) -> bool {
    let irc_event = line.split_once(' ').is_some_and(|(clock, body)| {
        clock.len() == 5
            && clock.as_bytes()[2] == b':'
            && ["-!- ", "* "]
                .iter()
                .any(|prefix| body.trim_start().starts_with(prefix))
    });
    let tab_event = line.split_once('\t').is_some_and(|(stamp, body)| {
        stamp.len() >= 19
            && stamp.as_bytes()[10] == b' '
            && body.split_once('\t').is_some_and(|(nick, _)| {
                matches!(
                    nick.trim(),
                    "-->" | "<--" | "--" | "*" | "◥" | "◣" | "→" | "←"
                )
            })
    });
    irc_event
        || tab_event
        || line.starts_with("--- Log opened ")
        || line.starts_with("--- Log closed ")
        || line.starts_with("--- Day changed ")
        || line.starts_with("Transcribed by https://otter.ai")
        || line.starts_with("|")
            && line
                .trim_matches(|c: char| c == '|' || c.is_whitespace())
                .is_empty()
}

fn discord(lines: &[&str]) -> bool {
    let lines: Vec<_> = nonblank(lines).collect();
    let border = |line: &str| line.len() == 62 && line.bytes().all(|byte| byte == b'=');
    lines.len() >= 9
        && border(lines[0])
        && lines[1].starts_with("Guild: ")
        && lines[2].starts_with("Channel: ")
        && border(lines[lines.len() - 1])
        && border(lines[lines.len() - 3])
        && lines[lines.len() - 2].starts_with("Exported ")
        && lines[lines.len() - 2].ends_with(" message(s)")
        && lines[3..lines.len() - 3]
            .iter()
            .any(|line| line.starts_with('[') && line.contains("] "))
}

fn whatsapp_notice(body: &str) -> bool {
    [
        "Messages and calls are end-to-end encrypted.",
        "Berichten en oproepen worden end-to-end versleuteld.",
        "الرسائل والمكالمات مشفرة تمامًا بين الطرفين",
        "This business uses a secure service from Meta to manage this chat.",
    ]
    .iter()
    .any(|notice| body.starts_with(notice))
}

fn whatsapp_row(line: &str, bracketed: bool) -> Option<&str> {
    static BRACKETED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(r"^\[(?:\p{Nd}{1,2}[-/.]\p{Nd}{1,2}[-/.]\p{Nd}{2,4}|\p{Nd}{4}[/.]\p{Nd}{1,2}[/.]\p{Nd}{1,2})(?: BE)?[,،]?\s+",
            r"\p{Nd}{1,2}[:.]\p{Nd}{2}(?:[:.]\p{Nd}{2})?(?:\s+(?:[aApP]\.?\s?[mM]\.?|[صم]))?\]\s+(.+)$")).unwrap()
    });
    static DASH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(r"^\p{Nd}{1,4}[-/.]\p{Nd}{1,2}[-/.]\p{Nd}{1,4}[,،]?\s+(?:klo\s+)?",
            r"\p{Nd}{1,2}[:.]\p{Nd}{2}(?:[:.]\p{Nd}{2})?(?:\s+[aApP]\.?\s?[mM]\.?)?(?:\s+-\s+|:\s+)(.+)$")).unwrap()
    });
    let stamp = if bracketed { &*BRACKETED } else { &*DASH };
    stamp.captures(line)?.get(1).map(|m| m.as_str())
}

fn whatsapp(lines: &[&str], bracketed: bool) -> bool {
    let mut speakers = HashSet::new();
    let mut messages = 0;
    let mut body_lines = 0;
    let mut marked = false;
    for line in nonblank(lines) {
        let line = if line.contains(BIDI_MARKS) {
            Cow::Owned(line.replace(BIDI_MARKS, ""))
        } else {
            Cow::Borrowed(line)
        };
        let Some(row) = whatsapp_row(&line, bracketed) else {
            if messages == 0 {
                return false;
            }
            body_lines += 1;
            continue;
        };
        let (label, body) = row.split_once([':', '：']).unwrap_or(("", row));
        let body = body.trim();
        if whatsapp_notice(body) {
            marked = true;
        } else if let Some(speaker) = speaker_label(label) {
            speakers.insert(speaker.to_lowercase());
            messages += 1;
        } else if [
            " created group ",
            " added ",
            " changed the group ",
            " joined using ",
            "You were added",
            "You created group ",
        ]
        .iter()
        .any(|notice| row.contains(notice))
        {
            // Exporter events are not turns or continuation prose.
        } else if messages == 0 {
            return false;
        } else {
            body_lines += 1;
        }
    }
    messages >= 2
        && if marked {
            body_lines <= messages * 4
        } else {
            speakers.len() >= 2 && body_lines <= messages
        }
}

fn otter(lines: &[&str]) -> bool {
    static CLOCK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:(?P<speaker>[^:]{1,60}?)\s{2,})?\d{1,3}:[0-5]\d(?::[0-5]\d)?$").unwrap()
    });
    let lines: Vec<_> = nonblank(lines).collect();
    let Some((&"Transcribed by https://otter.ai", body)) = lines.split_last() else {
        return false;
    };
    let rows = body.iter().copied();
    stacked_blocks(rows, &CLOCK)
}

fn rev(lines: &[&str]) -> bool {
    static CLOCK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^Speaker [0-9]+ \(\s*\d{1,3}:[0-5]\d(?::[0-5]\d)?\s*\):$").unwrap()
    });
    stacked_blocks(nonblank(lines), &CLOCK)
}

fn stacked_blocks<'a>(lines: impl Iterator<Item = &'a str>, clock: &Regex) -> bool {
    let mut turns = 0;
    let mut has_body = false;
    for line in lines {
        if let Some(found) = clock.captures(line)
            && found
                .name("speaker")
                .is_none_or(|label| speaker_label(label.as_str()).is_some())
        {
            if turns > 0 && !has_body {
                return false;
            }
            turns += 1;
            has_body = false;
        } else if turns == 0 {
            return false;
        } else {
            has_body = true;
        }
    }
    turns >= 2 && has_body
}

const BIDI_MARKS: [char; 11] = [
    '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}',
    '\u{2067}', '\u{2068}', '\u{2069}',
];

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn recognizes_conversation_exports_and_leaves_ordinary_notes_alone() {
        let bar = "=".repeat(62);
        let discord = format!(
            "{bar}\nGuild: Potters\nChannel: firing\n{bar}\n\n[4/7/2025 10:02 AM] lio\nReady.\n\n[4/7/2025 10:03 AM] val\nComing.\n\n{bar}\nExported 2 message(s)\n{bar}\n"
        );
        let irc = "--- Log opened Tue Sep 3 00:00:00 2024\n00:00 -!- lio has joined\n00:01 <@lio> Ready.\n00:01 < val> Over here.\n00:02 <@lio> Coming.\n";
        let otter = "Unknown Speaker  0:04\nThe wind is picking up.\nUnknown Speaker  2:13\nThe smaller boats have tied up.\n";
        let footer = "Transcribed by https://otter.ai";
        for text in [
            discord.as_str(),
            irc,
            &format!("{otter}{footer}"),
            "\u{200e}[17/08/2025, 09:01:01] Pia: Where is the shelf?\n[17/08/2025, 09:01:03] Ivo: Around the corner.\nBy the door.",
            "8/17/25, 9:01 AM - Messages and calls are end-to-end encrypted.\n8/17/25, 9:01 AM - Pia: Where is it?\n8/17/25, 9:02\u{202f}AM - Ivo: At the shop.",
        ] {
            assert!(matches(text), "expected transcript: {text}");
        }
        for text in [
            otter,
            &format!("I meant to find this later.\n{footer}"),
            "Guild: pottery\nChannel: firing",
            "A diary entry.\n00:01 <lio> Ready.",
            "date: 8/17/25\nmessage: arriving at 9:01 AM",
        ] {
            assert!(!matches(text), "expected note: {text}");
        }
    }
}
