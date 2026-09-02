-- Casual (private room) rounds in the history, next to the ranked ones. They are reported by the
-- client when a round reaches the frag limit, are never rated and do not count towards wins and
-- losses. The opponent may be anonymous or a player who never reports, so player_b and the
-- ratings become optional.
ALTER TABLE matches
    ADD COLUMN ranked TINYINT(1) NOT NULL DEFAULT 1 AFTER room_code,
    MODIFY player_b BIGINT UNSIGNED NULL,
    MODIFY elo_a INT NULL,
    MODIFY elo_b INT NULL,
    ADD KEY idx_matches_casual (ranked, room_code, created_at);
