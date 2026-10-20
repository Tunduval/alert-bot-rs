-- Separate a revocable worker claim from an external send that has started.
ALTER TABLE alerts DROP CONSTRAINT alerts_state_check;
ALTER TABLE alerts
    ADD CONSTRAINT alerts_state_check
    CHECK (state IN ('pending', 'claimed', 'sending', 'sent', 'failed', 'cancelled'));

-- Sending reminders remain active for quotas and listings.
DROP INDEX alerts_chat_idx;
CREATE INDEX alerts_chat_idx ON alerts (chat_id)
    WHERE state IN ('pending', 'claimed', 'sending');

DROP INDEX alerts_user_active_idx;
CREATE INDEX alerts_user_active_idx ON alerts (user_id)
    WHERE state IN ('pending', 'claimed', 'sending');
