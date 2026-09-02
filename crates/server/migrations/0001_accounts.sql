-- Accounts, e-mail codes and ranked matches.
CREATE TABLE accounts (
    id            BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    email         VARCHAR(255)    NOT NULL,
    username      VARCHAR(24)     NOT NULL,
    password_hash VARCHAR(255)    NOT NULL,
    elo           INT             NOT NULL DEFAULT 1500,
    wins          INT             NOT NULL DEFAULT 0,
    losses        INT             NOT NULL DEFAULT 0,
    -- Bumped on password change so older login tokens stop working.
    token_version INT             NOT NULL DEFAULT 0,
    verified_at   DATETIME        NULL,
    created_at    DATETIME        NOT NULL DEFAULT (UTC_TIMESTAMP()),
    UNIQUE KEY uq_accounts_email (email),
    UNIQUE KEY uq_accounts_username (username)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Short codes mailed for e-mail verification and password resets (stored hashed).
CREATE TABLE email_codes (
    id         BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    account_id BIGINT UNSIGNED NOT NULL,
    purpose    ENUM('verify', 'reset') NOT NULL,
    code_hash  CHAR(64)        NOT NULL,
    attempts   INT             NOT NULL DEFAULT 0,
    expires_at DATETIME        NOT NULL,
    created_at DATETIME        NOT NULL DEFAULT (UTC_TIMESTAMP()),
    KEY idx_email_codes_account (account_id, purpose),
    CONSTRAINT fk_email_codes_account FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Ranked (matchmade) games. Names and ratings are copied at match time so history survives renames.
CREATE TABLE matches (
    id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    room_code   CHAR(6)         NOT NULL,
    player_a    BIGINT UNSIGNED NOT NULL,
    player_b    BIGINT UNSIGNED NOT NULL,
    name_a      VARCHAR(24)     NOT NULL,
    name_b      VARCHAR(24)     NOT NULL,
    elo_a       INT             NOT NULL,
    elo_b       INT             NOT NULL,
    score_a     INT             NULL,
    score_b     INT             NULL,
    -- 0 = player_a won, 1 = player_b won.
    winner      TINYINT         NULL,
    delta_a     INT             NULL,
    delta_b     INT             NULL,
    status      ENUM('playing', 'finished', 'void') NOT NULL DEFAULT 'playing',
    -- Each player's result report as "score_a,score_b,winner"; the match is settled when they agree.
    report_a    VARCHAR(24)     NULL,
    report_b    VARCHAR(24)     NULL,
    report_a_at DATETIME        NULL,
    report_b_at DATETIME        NULL,
    created_at  DATETIME        NOT NULL DEFAULT (UTC_TIMESTAMP()),
    finished_at DATETIME        NULL,
    KEY idx_matches_a (player_a, id),
    KEY idx_matches_b (player_b, id),
    KEY idx_matches_status (status, created_at),
    CONSTRAINT fk_matches_a FOREIGN KEY (player_a) REFERENCES accounts (id),
    CONSTRAINT fk_matches_b FOREIGN KEY (player_b) REFERENCES accounts (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
