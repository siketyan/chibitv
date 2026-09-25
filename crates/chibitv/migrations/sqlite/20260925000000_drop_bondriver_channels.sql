-- BonDriver support is gone, and with it the channels named by the numbers a
-- BonDriver enumerates: every channel left is tuned by its parameters.
DELETE FROM channels WHERE tuning = 'bondriver';

ALTER TABLE channels DROP COLUMN tuning;
ALTER TABLE channels DROP COLUMN space;
ALTER TABLE channels DROP COLUMN channel_number;
