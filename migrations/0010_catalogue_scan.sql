-- What the code scan of a catalogue version found (issue 189): a JSON array of
-- {"file", "line", "reason"} for the admin who reviews it (issue 192). NULL until the scan has
-- run; an empty array when it found nothing. A flag never fails a version.
--
-- Additive only. Rolling back to a binary without this migration:
-- ALTER TABLE catalogue_versions DROP COLUMN scan;
-- DELETE FROM _sqlx_migrations WHERE version = 10;
ALTER TABLE catalogue_versions ADD COLUMN scan text;

-- A restart's lookup: the versions whose checks it must run again.
CREATE INDEX catalogue_versions_checking_idx ON catalogue_versions (seq) WHERE state = 'checking';
