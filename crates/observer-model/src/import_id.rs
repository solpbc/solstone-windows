// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Pure import-identifier validation and status-pill precedence.

/// Strict ASCII `^[0-9]{8}_[0-9]{6}$` import identifier check.
///
/// Rejects Unicode digits, path separators, query/fragment characters,
/// whitespace, and wrong lengths.
pub fn is_valid_import_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 15
        && bytes[8] == b'_'
        && bytes
            .iter()
            .enumerate()
            .all(|(idx, byte)| idx == 8 || byte.is_ascii_digit())
}

/// Origin-relative journal path `/app/import/<id>` for a validated import ID.
pub fn import_item_journal_path(id: &str) -> Option<String> {
    is_valid_import_id(id).then(|| format!("/app/import/{id}"))
}

/// Paired-and-connected status pill state when no sync hold or offline fault applies.
///
/// Precedence when `waiting == 0`:
/// `needs attention` (`Attention`, non-green `connected`)
///   > `ready to review` (`ReadyToReview(n)`, orange bullseye, non-fault)
///   > `connected` (`Connected`, green dot).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectedPillState {
    WaitingToSync,
    Attention,
    ReadyToReview(u64),
    Connected,
}

pub fn connected_pill_state(
    waiting: u64,
    imports_need_owner: bool,
    imports_ready_to_review: u64,
) -> ConnectedPillState {
    if waiting > 0 {
        return ConnectedPillState::WaitingToSync;
    }
    if imports_need_owner {
        return ConnectedPillState::Attention;
    }
    if imports_ready_to_review > 0 {
        return ConnectedPillState::ReadyToReview(imports_ready_to_review);
    }
    ConnectedPillState::Connected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_ascii_import_id_accepts_only_yyyymmdd_hhmmss() {
        assert!(is_valid_import_id("20261122_143059"));
        assert!(is_valid_import_id("00000000_000000"));
        assert_eq!(
            import_item_journal_path("20261122_143059").as_deref(),
            Some("/app/import/20261122_143059")
        );

        for invalid in [
            "",
            "20261122_14305",
            "20261122_1430590",
            "20261122-143059",
            "20261122_14305a",
            "../20261122_143059",
            "20261122_143059/discard",
            "20261122_143059?x=1",
            "20261122_143059#a",
            " 20261122_143059",
            "20261122_143059\n",
            // Arabic-Indic Unicode digits (must fail strict ASCII check)
            "\u{0662}\u{0660}\u{0662}\u{0666}\u{0661}\u{0661}\u{0662}\u{0662}_\u{0661}\u{0664}\u{0663}\u{0660}\u{0665}\u{0669}",
            // Fullwidth digits (must fail strict ASCII check)
            "\u{ff12}\u{ff10}\u{ff12}\u{ff16}\u{ff11}\u{ff11}\u{ff12}\u{ff12}_\u{ff11}\u{ff14}\u{ff13}\u{ff10}\u{ff15}\u{ff19}",
        ] {
            assert!(!is_valid_import_id(invalid), "must reject {invalid:?}");
            assert!(import_item_journal_path(invalid).is_none());
        }
    }

    #[test]
    fn status_pill_precedence_ranks_waiting_then_attention_then_review_then_connected() {
        assert_eq!(
            connected_pill_state(2, true, 3),
            ConnectedPillState::WaitingToSync
        );
        assert_eq!(
            connected_pill_state(0, true, 3),
            ConnectedPillState::Attention
        );
        assert_eq!(
            connected_pill_state(0, true, 0),
            ConnectedPillState::Attention
        );
        assert_eq!(
            connected_pill_state(0, false, 3),
            ConnectedPillState::ReadyToReview(3)
        );
        assert_eq!(
            connected_pill_state(0, false, 0),
            ConnectedPillState::Connected
        );
    }
}
