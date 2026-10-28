-- Where a chat's city actually is, so prayer times can be fetched by
-- coordinates. Asking AlAdhan by city name relies on its own geocoder,
-- which is rate-limited and answers 503 for towns it hasn't seen before
-- (e.g. Crawley). Coordinates need no lookup.
--
-- Set together with `city` (a shared location stores the exact point; a
-- typed city stores the town's centre). NULL for chats that set their city
-- before this existed, and for chats using the default: those are looked up
-- (Open-Meteo geocoding) when needed.

ALTER TABLE chat_settings ADD COLUMN latitude REAL
    CHECK (latitude IS NULL OR latitude BETWEEN -90 AND 90);
ALTER TABLE chat_settings ADD COLUMN longitude REAL
    CHECK (longitude IS NULL OR longitude BETWEEN -180 AND 180);
