-- Notes are stored and shown verbatim: the local-LLM summarization step's reply is the note, and the
-- Settings prompt template dictates its content and Markdown format. Collapse the old structured
-- summary + action_items columns into a single `content` column. Existing rows are backfilled by
-- concatenating their summary with the action items rendered as a Markdown bullet list, so already
-- generated notes are preserved.
ALTER TABLE meeting_notes ADD COLUMN content TEXT NOT NULL DEFAULT '';

UPDATE meeting_notes
SET content = TRIM(
    summary ||
    COALESCE(
        (SELECT char(10) || char(10) || group_concat('- ' || value, char(10))
         FROM json_each(action_items)),
        ''
    ),
    -- Trim newlines/tabs/spaces from both ends (bare TRIM strips spaces only, so an empty summary
    -- would otherwise leave the leading blank lines before the action-item list).
    char(10) || char(13) || char(9) || ' '
);

ALTER TABLE meeting_notes DROP COLUMN summary;
ALTER TABLE meeting_notes DROP COLUMN action_items;
