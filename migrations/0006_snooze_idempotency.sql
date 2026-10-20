-- Persist the Telegram message that represents one delivered occurrence.
-- A receipt is consumed once, atomically with creation of its snoozed alert.
CREATE TABLE alert_deliveries (
    chat_id                    BIGINT NOT NULL,
    message_id                 BIGINT NOT NULL,
    source_alert_id            BIGINT NOT NULL REFERENCES alerts(id),
    source_claim_generation    BIGINT NOT NULL,
    snoozed_alert_id           BIGINT REFERENCES alerts(id),
    delivered_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chat_id, message_id),
    UNIQUE (source_alert_id, source_claim_generation)
);

ALTER TABLE alerts
    ADD COLUMN snooze_source_alert_id BIGINT REFERENCES alerts(id),
    ADD COLUMN snooze_source_generation BIGINT,
    ADD CONSTRAINT alerts_snooze_source_complete CHECK (
        (snooze_source_alert_id IS NULL) = (snooze_source_generation IS NULL)
    );

CREATE UNIQUE INDEX alerts_snooze_occurrence_idx
    ON alerts (snooze_source_alert_id, snooze_source_generation)
    WHERE snooze_source_alert_id IS NOT NULL;
