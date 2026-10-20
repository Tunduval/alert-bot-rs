use teloxide::types::UserId as TelegramUserId;

/// Convert Telegram's unsigned user id into the signed representation used by
/// PostgreSQL. Reject values outside Teloxide's documented Telegram range
/// instead of accepting forged ids or wrapping them into negative chat ids and
/// advisory-lock keys.
pub fn user_id(raw: u64) -> Option<i64> {
    if !(TelegramUserId::MIN.0..=TelegramUserId::MAX.0).contains(&raw) {
        return None;
    }
    i64::try_from(raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_ids_must_fit_the_database_representation() {
        assert_eq!(user_id(TelegramUserId::MIN.0), Some(0));
        assert_eq!(
            user_id(TelegramUserId::MAX.0),
            i64::try_from(TelegramUserId::MAX.0).ok()
        );
        assert_eq!(user_id(TelegramUserId::MAX.0 + 1), None);
        assert_eq!(user_id(i64::MAX as u64), None);
        assert_eq!(user_id(i64::MAX as u64 + 1), None);
        assert_eq!(user_id(u64::MAX), None);
    }
}
