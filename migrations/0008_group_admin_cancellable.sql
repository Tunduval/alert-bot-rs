-- Preserve whether a private group alert came from the normal alert command.
-- Old one-shot alerts are ambiguous because snoozes created before migration
-- 0006 did not record their source. Recurring alerts cannot be snoozes, so
-- they are the only existing rows that can be backfilled safely.
ALTER TABLE alerts
    ADD COLUMN group_admin_cancellable BOOLEAN NOT NULL DEFAULT FALSE;

UPDATE alerts
SET group_admin_cancellable = TRUE
WHERE scope = 'private'
  AND chat_type IN ('group', 'supergroup')
  AND rrule IS NOT NULL
  AND snooze_source_alert_id IS NULL;
