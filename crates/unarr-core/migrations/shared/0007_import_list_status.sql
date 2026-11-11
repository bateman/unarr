-- The half of the originals' ImportListStatus that is not failure state
-- (that half lives in provider_status): when the list was last fetched and
-- whether an item left it since the library was last cleaned.
CREATE TABLE import_list_status (
    provider_id                      INTEGER PRIMARY KEY,
    last_info_sync                   TEXT,
    has_removed_item_since_last_clean INTEGER NOT NULL DEFAULT 0
);
